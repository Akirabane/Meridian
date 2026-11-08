//! The extension's requests to the proxy, its **orders** (docs/EXTENSIONS.md, section 5): the half of the
//! connection that the SDK did not have. The proxy opens the connection and asks the extension for decisions; once
//! the hello is done the extension may also ask the proxy, on the same socket, with the methods of
//! [`orders`](crate::orders).
//!
//! [`Proxy`] is the handle. It is cloned freely and used from any thread, **including the thread of a handler**: a
//! handler that has changed a player's rights calls [`Proxy::invalidate_player`] and waits for the answer, while the
//! thread that reads the connection goes on handing the answers to those that wait. Get it before the extension
//! listens, with [`Extension::proxy`](super::Extension::proxy), and give it to the handlers that need it.
//!
//! Each request has an id of its own, a number that counts up for as long as the extension runs (it does not start
//! again at a new connection). The ids of the two directions are independent, as docs/EXTENSIONS.md, 2.1, says: the
//! proxy's request number 1 and the extension's request number 1 are two requests, and an answer is told from a
//! request by what it holds. A request waits for its answer for [`Extension::order_timeout`](super::Extension::order_timeout)
//! (5 seconds by default), and at most [`MAX_WAITING`] wait at once; one more is refused at once, not queued. An
//! answer that comes after its request gave up, or that no request asked for, is dropped.
//!
//! When the connection ends, every request that waits ends with [`OrderError::Disconnected`] at once, and the next
//! are [`OrderError::NotConnected`] until the proxy connects again. **Nothing is retried here**: an order that
//! failed may or may not have been done, and what to do about it is for the extension to say (see
//! [`OrderError::retry_later`]).

use crate::orders::{self, Ack};
use crate::wire::{self, RpcError, code};
use meridian_websocket::Writer;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

/// The most requests to the proxy that wait for their answers at once.
pub const MAX_WAITING: usize = 64;

/// How long a request waits for its answer, unless [`Extension::order_timeout`](super::Extension::order_timeout)
/// says otherwise.
pub(crate) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

type Reply = Result<Value, RpcError>;

/// Why an order did not do what was asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OrderError {
    /// No proxy is connected: it has not connected yet, or the connection ended. Nothing was sent.
    NotConnected,
    /// [`MAX_WAITING`] orders wait for their answers already. Nothing was sent.
    TooManyWaiting,
    /// The order was not made, because it is not valid (a unique id that is not one). Nothing was sent.
    Invalid(String),
    /// The request could not be written. The connection is closed.
    Send(String),
    /// The proxy did not answer in time. **The order may still be done**: the proxy may have been slow.
    Timeout,
    /// The connection ended while the order waited. **The order may or may not have been done.**
    Disconnected,
    /// The proxy answered an error: it did not run the order. `-32601` is a capability that was not granted (or an
    /// order this proxy does not have), `-32602` parameters it did not take, `-32001` the budget of orders spent
    /// and `-32002` as many orders running as the proxy allows.
    Refused(RpcError),
    /// The proxy answered, and not with what the order defines.
    BadAnswer(String),
}

impl OrderError {
    /// Whether asking again a moment later can work: the proxy's budget or its places were spent
    /// ([`wire::code::OVER_BUDGET`], [`wire::code::TOO_MANY_RUNNING`]), or too many of the extension's own
    /// requests wait. **An order that is refused did not run**, so an extension that must have its order done, such as
    /// one that takes someone's rights back with [`Proxy::invalidate_player`], asks again until it is answered.
    pub fn retry_later(&self) -> bool {
        match self {
            OrderError::TooManyWaiting => true,
            OrderError::Refused(error) => {
                error.code == code::OVER_BUDGET || error.code == code::TOO_MANY_RUNNING
            }
            _ => false,
        }
    }

    /// Whether the proxy answered that it has no such order for this extension: the capability the order needs was
    /// not granted in the proxy's configuration (or not asked for in the hello).
    pub fn is_not_granted(&self) -> bool {
        matches!(self, OrderError::Refused(error) if error.code == code::METHOD_NOT_FOUND)
    }
}

impl fmt::Display for OrderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OrderError::NotConnected => f.write_str("no proxy is connected"),
            OrderError::TooManyWaiting => f.write_str("too many orders wait for their answers"),
            OrderError::Invalid(why) => write!(f, "the order is not valid: {why}"),
            OrderError::Send(why) => write!(f, "cannot send the order: {why}"),
            OrderError::Timeout => f.write_str("the proxy did not answer in time"),
            OrderError::Disconnected => f.write_str("the connection ended before the answer"),
            OrderError::Refused(error) => write!(
                f,
                "the proxy refused the order: {} ({})",
                error.message, error.code
            ),
            OrderError::BadAnswer(why) => {
                write!(f, "the answer is not what the order defines: {why}")
            }
        }
    }
}

impl std::error::Error for OrderError {}

#[derive(Default)]
struct State {
    /// The writing half of the connection that is up.
    writer: Option<Writer>,
    /// The id of the latest request: the next is one more.
    last_id: u64,
    /// The requests that wait for their answers, by id.
    waiting: HashMap<u64, mpsc::Sender<Reply>>,
}

/// What the connection thread and the handles share.
pub(crate) struct Client {
    state: Mutex<State>,
    /// How long a request waits, in milliseconds.
    timeout_ms: AtomicU64,
}

impl Client {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(State::default()),
            timeout_ms: AtomicU64::new(
                u64::try_from(DEFAULT_TIMEOUT.as_millis()).unwrap_or(u64::MAX),
            ),
        }
    }

    pub(crate) fn set_timeout(&self, timeout: Duration) {
        self.timeout_ms.store(
            u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A proxy is connected, and `writer` writes to it: orders may go.
    pub(crate) fn connected(&self, writer: Writer) {
        self.state().writer = Some(writer);
    }

    /// The connection ended: the requests that wait are told at once (their senders go with the map), and the next
    /// ones find no connection.
    pub(crate) fn disconnected(&self) {
        let waiting = {
            let mut state = self.state();
            state.writer = None;
            std::mem::take(&mut state.waiting)
        };
        drop(waiting);
    }

    /// The proxy answered request `id`. The answer of a request that gave up, or of one nobody sent, is dropped.
    pub(crate) fn answered(&self, id: Option<&wire::Id>, reply: Reply) {
        let Some(wire::Id::Number(id)) = id else {
            return;
        };
        let sender = self.state().waiting.remove(id);
        if let Some(sender) = sender {
            let _ = sender.send(reply);
        }
    }

    /// Sends `method` with `params` and waits for the answer's `result`, or says why there is none.
    fn order(&self, method: &str, params: Option<&impl Serialize>) -> Result<Value, OrderError> {
        let (id, writer, replies) = {
            let mut state = self.state();
            let Some(writer) = state.writer.clone() else {
                return Err(OrderError::NotConnected);
            };
            if state.waiting.len() >= MAX_WAITING {
                return Err(OrderError::TooManyWaiting);
            }
            state.last_id += 1;
            let id = state.last_id;
            let (sender, replies) = mpsc::channel();
            state.waiting.insert(id, sender);
            (id, writer, replies)
        };
        let forget = |id: u64| {
            self.state().waiting.remove(&id);
        };
        if let Err(e) = writer.send_text(&wire::request(id, method, params)) {
            forget(id);
            // A frame may be half written, or the request was too long to be a message: only the first closes.
            if e.kind() != std::io::ErrorKind::InvalidInput
                && e.kind() != std::io::ErrorKind::InvalidData
            {
                let _ = writer.shutdown();
            }
            return Err(OrderError::Send(e.to_string()));
        }
        let timeout = Duration::from_millis(self.timeout_ms.load(Ordering::Relaxed));
        match replies.recv_timeout(timeout) {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(error)) => Err(OrderError::Refused(error)),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                forget(id);
                Err(OrderError::Timeout)
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(OrderError::Disconnected),
        }
    }
}

/// The extension's way to give orders to the proxy: a handle, cloned freely, used from any thread (see the module).
/// It is made by [`Extension::proxy`](super::Extension::proxy) or [`Running::proxy`](super::Running::proxy), and works
/// for the proxy that is connected at the moment.
#[derive(Clone)]
pub struct Proxy(pub(crate) Arc<Client>);

impl fmt::Debug for Proxy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Proxy")
            .field("connected", &self.is_connected())
            .finish()
    }
}

impl Proxy {
    /// Whether a proxy is connected now.
    pub fn is_connected(&self) -> bool {
        self.0.state().writer.is_some()
    }

    /// Makes the proxy forget the permissions it kept of this extension's answers for the player with this unique
    /// id (docs/EXTENSIONS.md, section 5; capability `decide.command`, which the extension must have asked for,
    /// with [`Extension::on_command`](super::Extension::on_command), and the operator granted). Answers `Ok(())` when
    /// the proxy has done it.
    ///
    /// **It does nothing if it is refused**, and a refusal for a spent budget or a full set of places
    /// ([`OrderError::retry_later`]) is a moment's: an extension that has just taken someone's rights back must ask
    /// again until it is answered, because the right it took back is kept for the `cache_seconds` it said otherwise.
    /// A timeout or an end of the connection leaves it unknown whether the proxy did it: asking again is harmless.
    pub fn invalidate_player(&self, uuid: &str) -> Result<(), OrderError> {
        // The same spelling for both sides, and a refusal before anything is sent for a text that is not an id.
        let uuid = orders::canonical_uuid(uuid)
            .ok_or_else(|| OrderError::Invalid("\"player\" is not a unique id".into()))?;
        self.invalidate(&orders::InvalidateParams::player(uuid))
    }

    /// As [`Proxy::invalidate_player`], for every player.
    pub fn invalidate_all(&self) -> Result<(), OrderError> {
        self.invalidate(&orders::InvalidateParams::all())
    }

    fn invalidate(&self, params: &orders::InvalidateParams) -> Result<(), OrderError> {
        let result = self.0.order(orders::PERMISSIONS_INVALIDATE, Some(params))?;
        match serde_json::from_value::<Ack>(result) {
            Ok(Ack { ok: true }) => Ok(()),
            _ => Err(OrderError::BadAnswer("expected {\"ok\": true}".into())),
        }
    }
}
