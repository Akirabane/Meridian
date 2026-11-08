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

use crate::orders::{
    self, Ack, Drained, Found, Kicked, PlayerInfo, PlayersPage, QueueState, ServerInfo,
    ServersList, Started, Told, Undrained,
};
use crate::wire::{self, RpcError, code};
use meridian_websocket::Writer;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

/// The most pages [`Proxy::players`] asks for before it gives up: with 500 players a page, that is 100,000 players, far
/// more than a proxy holds, so a proxy that never ends its pages is told from one that has many.
const MAX_PAGES: usize = 200;

/// The most requests to the proxy that wait for their answers at once.
pub const MAX_WAITING: usize = 64;

/// The most characters of what the proxy said that an [`OrderError`] shows when it is displayed.
const MAX_SHOWN: usize = 200;

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
    /// The proxy answered an error. `-32601` is a capability that was not granted (or an order this proxy does not
    /// have), `-32602` parameters it did not take, `-32001` the budget of orders spent and `-32002` as many orders
    /// running as the proxy allows: **in these four the order did not run**. `-32603` is an order that failed
    /// while it ran (the proxy's handler panicked, or its thread could not start): **it may have been done**
    /// ([`OrderError::may_have_run`]).
    Refused(RpcError),
    /// The proxy answered, and not with what the order defines.
    BadAnswer(String),
}

impl OrderError {
    /// Whether asking again a moment later can work **and the order did not run**: the proxy's budget or its places
    /// were spent ([`wire::code::OVER_BUDGET`], [`wire::code::TOO_MANY_RUNNING`]), or too many of the extension's
    /// own requests wait. Asking again is always safe, whatever the order is. (An order that failed while it ran,
    /// `-32603`, or whose outcome is not known, is [`OrderError::may_have_run`]: asking again is safe only for an
    /// order that does the same when it is done twice, as the orders of this SDK do.)
    pub fn retry_later(&self) -> bool {
        match self {
            OrderError::TooManyWaiting => true,
            OrderError::Refused(error) => {
                error.code == code::OVER_BUDGET || error.code == code::TOO_MANY_RUNNING
            }
            _ => false,
        }
    }

    /// Whether the order **may have been done** although it ended in an error: the proxy did not answer in time, the
    /// connection ended or broke while it waited, the proxy answered `-32603` (it failed while it ran) or answered
    /// something that is not what the order defines. Asking again is harmless for an order that has the same effect
    /// when it is done twice, such as [`Proxy::invalidate_player`] and [`Proxy::invalidate_all`]; for another it is for
    /// the extension to look first.
    pub fn may_have_run(&self) -> bool {
        match self {
            OrderError::Timeout
            | OrderError::Disconnected
            | OrderError::Send(_)
            | OrderError::BadAnswer(_) => true,
            OrderError::Refused(error) => error.code == code::INTERNAL_ERROR,
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
            // The proxy's own words, cleaned: a line break or a control character in them must not reach a log.
            OrderError::Refused(error) => write!(
                f,
                "the proxy refused the order: {} ({})",
                crate::text::clean(&error.message, MAX_SHOWN),
                error.code
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

    /// How many requests wait for their answers (for the tests).
    #[cfg(test)]
    pub(crate) fn waiting(&self) -> usize {
        self.state().waiting.len()
    }

    /// [`Client::order`], with the answer read as the type the order defines. What is wrong with an answer is told in
    /// fixed words: they are never the proxy's own, which a reader of a log should not have to trust.
    fn read<T: DeserializeOwned>(
        &self,
        method: &str,
        params: Option<&impl Serialize>,
    ) -> Result<T, OrderError> {
        let result = self.order(method, params)?;
        serde_json::from_value(result)
            .map_err(|_| OrderError::BadAnswer("not the shape the order defines".into()))
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
        // The entry goes when this goes, however the order ends: an answer (which took it already), a timeout, a
        // failed write, or a panic of a thread that waited.
        let _entry = Entry { client: self, id };
        if let Err(e) = writer.send_text(&wire::request(id, method, params)) {
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
            Err(mpsc::RecvTimeoutError::Timeout) => Err(OrderError::Timeout),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(OrderError::Disconnected),
        }
    }
}

/// The place of a request in the table of those that wait: it is given back when this is dropped.
struct Entry<'a> {
    client: &'a Client,
    id: u64,
}

impl Drop for Entry<'_> {
    fn drop(&mut self) {
        self.client.state().waiting.remove(&self.id);
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
    /// A timeout, an end of the connection or a `-32603` ([`OrderError::may_have_run`]) leaves it unknown whether the
    /// proxy did it: asking again is harmless, since doing it twice is doing it once. With no proxy connected
    /// ([`OrderError::NotConnected`]) it has not been asked: ask again when the proxy has connected. Bound the
    /// number of times, as the example of the guide does.
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

    /// One page of the players online, **in the order of their unique ids** (`orders.read`, which the extension asks
    /// for with [`Extension::wants`](super::Extension::wants) and the operator grants; an `address` is in it only if
    /// `addresses` is too). `limit` is how many the page holds, 1 to [`orders::MAX_PAGE`] (100 when `None`), and `cursor`
    /// the [`PlayersPage::next`] of the page before. A page tells how many players there are in all.
    ///
    /// Players who join or leave between two pages skip and repeat nobody: the cursor is an id, not a place. [`Proxy::players`]
    /// asks for all of them.
    pub fn players_page(
        &self,
        limit: Option<usize>,
        cursor: Option<&str>,
    ) -> Result<PlayersPage, OrderError> {
        let limit = match limit {
            None => None,
            Some(n) if (1..=orders::MAX_PAGE).contains(&n) => u32::try_from(n).ok(),
            Some(_) => {
                return Err(OrderError::Invalid(format!(
                    "\"limit\" must be from 1 to {}",
                    orders::MAX_PAGE
                )));
            }
        };
        let cursor = match cursor {
            None => None,
            Some(cursor) => Some(
                orders::canonical_uuid(cursor)
                    .ok_or_else(|| OrderError::Invalid("\"cursor\" is not a unique id".into()))?,
            ),
        };
        self.0.read(
            orders::PLAYERS_LIST,
            Some(&orders::ListParams { limit, cursor }),
        )
    }

    /// Every player online, a page at a time ([`Proxy::players_page`]), in the order of their unique ids. Each page is an
    /// order against the proxy's budget, so a large network is a few orders (500 players a page), not one for each
    /// player. A proxy whose pages do not go forward, or do not end, is [`OrderError::BadAnswer`].
    pub fn players(&self) -> Result<Vec<PlayerInfo>, OrderError> {
        let mut all = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let page = self.players_page(Some(orders::MAX_PAGE), cursor.as_deref())?;
            all.extend(page.players);
            let Some(next) = page.next else {
                return Ok(all);
            };
            let next = orders::canonical_uuid(&next)
                .filter(|next| {
                    cursor
                        .as_deref()
                        .is_none_or(|before| next.as_str() > before)
                })
                .ok_or_else(|| OrderError::BadAnswer("the pages do not go forward".into()))?;
            cursor = Some(next);
        }
        Err(OrderError::BadAnswer("the pages do not end".into()))
    }

    /// The player called `who` (whatever the case) or with that unique id (with or without hyphens), or `None` if nobody
    /// online is. A text that is neither is [`OrderError::Invalid`], and nothing is sent. Needs `orders.read`.
    pub fn find_player(&self, who: &str) -> Result<Option<PlayerInfo>, OrderError> {
        let params = orders::FindParams {
            player: who.to_owned(),
        };
        orders::parse_find(serde_json::to_value(&params).ok().as_ref())
            .map_err(OrderError::Invalid)?;
        let found: Found = self.0.read(orders::PLAYERS_FIND, Some(&params))?;
        Ok(found.player)
    }

    /// The servers, in the order of the proxy's configuration, with their players and, for those the balancer places
    /// players on, their role, caps and state. No server address is told. Needs `orders.read`.
    pub fn servers(&self) -> Result<Vec<ServerInfo>, OrderError> {
        let list: ServersList = self.0.read(orders::SERVERS_LIST, None::<&Value>)?;
        Ok(list.servers)
    }

    /// Whether the proxy's queue is on and who waits in it (the first 500). Needs `orders.read`.
    pub fn queue(&self) -> Result<QueueState, OrderError> {
        self.0.read(orders::QUEUE_LIST, None::<&Value>)
    }

    /// Shows `text` in the chat of the player called `player` (whatever the case) or with that unique id, and tells how many
    /// were **shown** it: 1, or 0 if nobody online is or their session reads no chat yet (not an error). Needs `orders.message`, which the extension asks for with
    /// [`Extension::wants`](super::Extension::wants) and the operator grants.
    ///
    /// The line is shown **as written, as plain text**, with no formatting and no click event: it must be 1 to
    /// [`orders::MAX_TEXT`] characters with none that a line may not hold (a control, formatting or invisible character), or
    /// it is [`OrderError::Invalid`] and nothing is sent ([`orders::clean_text`] makes a line valid). **This order is not
    /// the same when it is done twice**: after a timeout or an end of the connection ([`OrderError::may_have_run`]) the
    /// player may have been told, and asking again may tell them twice.
    pub fn tell(&self, player: &str, text: &str) -> Result<usize, OrderError> {
        let params = orders::TellParams {
            player: player.to_owned(),
            text: text.to_owned(),
        };
        orders::parse_tell(serde_json::to_value(&params).ok().as_ref())
            .map_err(OrderError::Invalid)?;
        let told: Told = self.0.read(orders::PLAYERS_TELL, Some(&params))?;
        Ok(told.told)
    }

    /// Shows `text` to every player online, or to those on `server`, as the proxy's own `/alert` does, and tells how many
    /// were **shown** it (a player who is still joining reads no chat and is not counted). A server the proxy does not have is a refusal (`-32602`). Needs `orders.message`.
    ///
    /// **It costs the proxy's budget of orders the number of players it reaches, and at most its burst** (the proxy
    /// refuses it, before anyone is told, with `-32001` ([`OrderError::retry_later`]) if the budget cannot pay). The line is
    /// checked as in [`Proxy::tell`], and this order too is not the same when it is done twice.
    pub fn broadcast(&self, server: Option<&str>, text: &str) -> Result<usize, OrderError> {
        let params = orders::BroadcastParams {
            server: server.map(str::to_owned),
            text: text.to_owned(),
        };
        orders::parse_broadcast(serde_json::to_value(&params).ok().as_ref())
            .map_err(OrderError::Invalid)?;
        let told: Told = self.0.read(orders::BROADCAST, Some(&params))?;
        Ok(told.told)
    }

    /// Sends the player called `player` (whatever the case) or with that unique id to the server `to`, and tells how many
    /// players the proxy **started** to move: 1, or 0 if nobody online is that or they are on `to` already (not an error).
    /// Needs `orders.move`, which the extension asks for with [`Extension::wants`](super::Extension::wants) and the operator
    /// grants. See [`Proxy::send_all`] for what the answer means.
    pub fn send_player(&self, player: &str, to: &str) -> Result<usize, OrderError> {
        self.send(orders::SendParams {
            to: to.to_owned(),
            player: Some(player.to_owned()),
            ..orders::SendParams::default()
        })
    }

    /// Sends every player online to the server `to` (those who are on it already are not moved), and tells how many the proxy
    /// **started** to move.
    ///
    /// **The answer comes when the moves start, not when they end**: they go on one after the other, as the proxy's own `send`
    /// does, and how each one ended is in the proxy's log. A player who is moving is not moved twice at once. A server the
    /// proxy does not have is a refusal (`-32602`). **It costs the proxy's budget of orders the number of players it moves, at
    /// most its burst**, charged all at once: one the budget cannot pay is refused (`-32001`, [`OrderError::retry_later`]) and
    /// moves nobody. A repeated order moves only those who are not on `to` yet, but after a timeout or an end of the
    /// connection ([`OrderError::may_have_run`]) look where the players are before asking again.
    pub fn send_all(&self, to: &str) -> Result<usize, OrderError> {
        self.send(orders::SendParams {
            to: to.to_owned(),
            all: Some(true),
            ..orders::SendParams::default()
        })
    }

    /// Sends the players of the server `from` to the server `to`. As [`Proxy::send_all`], for those players; a `from` the
    /// proxy does not have is a refusal.
    pub fn send_server(&self, from: &str, to: &str) -> Result<usize, OrderError> {
        self.send(orders::SendParams {
            to: to.to_owned(),
            from: Some(from.to_owned()),
            ..orders::SendParams::default()
        })
    }

    /// Kicks the player called `player` (whatever the case) or with that unique id: their session ends and they read `reason`
    /// (or, when it is `None`, [`orders::DEFAULT_KICK_REASON`]) as plain text. Tells how many were kicked: 1, or 0 if nobody
    /// online is that (not an error). Needs `orders.moderate`, which the extension asks for with
    /// [`Extension::wants`](super::Extension::wants) and the operator grants: **it lets the extension expel any player, an
    /// operator's included**, and is logged by the proxy with the player and the reason.
    ///
    /// The reason is valid as the line of [`Proxy::tell`] is (1 to [`orders::MAX_TEXT`] characters with no control,
    /// formatting or invisible character), or the call is [`OrderError::Invalid`] and nothing is sent. Asking again after a
    /// timeout or an end of the connection ([`OrderError::may_have_run`]) is harmless: a player who was kicked is not online
    /// to be kicked again (it answers 0), though if they came back in the meantime they are kicked again.
    pub fn kick(&self, player: &str, reason: Option<&str>) -> Result<usize, OrderError> {
        let params = orders::KickParams {
            player: player.to_owned(),
            reason: reason.map(str::to_owned),
        };
        orders::parse_kick(serde_json::to_value(&params).ok().as_ref())
            .map_err(OrderError::Invalid)?;
        let kicked: Kicked = self.0.read(orders::PLAYERS_KICK, Some(&params))?;
        Ok(kicked.kicked)
    }

    /// Stops offering the server `server` to new players, as the proxy's console `drain` does: the proxy tells whether it was
    /// drained **now** (`false`: it was drained already) and, with `move_players`, how many players it **started** to send to other
    /// servers of its role (one at a time, on a thread of its own: the answer comes when the moves start). Needs `orders.operate`,
    /// which the extension asks for with [`Extension::wants`](super::Extension::wants) and the operator grants: **it lets the
    /// extension take servers out of rotation and reload the proxy's configuration**.
    ///
    /// A server with no role is a refusal (`-32602`: there is nothing to drain). A drain that moves players costs the proxy's budget the
    /// number of players it sends, at most its burst, charged all at once, and takes the extension's place for a send (one lot at a
    /// time): a drain that cannot be paid drains nothing. Draining twice is harmless.
    pub fn drain(&self, server: &str, move_players: bool) -> Result<Drained, OrderError> {
        let params = orders::DrainParams {
            server: server.to_owned(),
            move_players: move_players.then_some(true),
        };
        orders::parse_drain(serde_json::to_value(&params).ok().as_ref())
            .map_err(OrderError::Invalid)?;
        self.0.read(orders::SERVERS_DRAIN, Some(&params))
    }

    /// Offers the drained server `server` to new players again: `true` when it was drained and is offered now, `false` when it was
    /// not drained (not an error). Needs `orders.operate`.
    pub fn undrain(&self, server: &str) -> Result<bool, OrderError> {
        let params = orders::UndrainParams {
            server: server.to_owned(),
        };
        orders::parse_undrain(serde_json::to_value(&params).ok().as_ref())
            .map_err(OrderError::Invalid)?;
        let undrained: Undrained = self.0.read(orders::SERVERS_UNDRAIN, Some(&params))?;
        Ok(undrained.undrained)
    }

    /// Asks the proxy to read its configuration file again and put it in force, as its console's `reload` does. **`Ok(())` means
    /// that the proxy took the order, not that the reload worked**: the proxy answers first and applies after (a reload can change
    /// this extension's own entry and close the connection the answer would go on), and what it did is in its log. One reload at a
    /// time for an extension: while one runs another is refused (`-32002`, [`OrderError::retry_later`]). Needs `orders.operate`.
    pub fn reload(&self) -> Result<(), OrderError> {
        let ack: Ack = self.0.read(orders::RELOAD, None::<&Value>)?;
        if ack.ok {
            Ok(())
        } else {
            Err(OrderError::BadAnswer("expected {\"ok\": true}".into()))
        }
    }

    fn send(&self, params: orders::SendParams) -> Result<usize, OrderError> {
        orders::parse_send(serde_json::to_value(&params).ok().as_ref())
            .map_err(OrderError::Invalid)?;
        let started: Started = self.0.read(orders::PLAYERS_SEND, Some(&params))?;
        Ok(started.started)
    }

    fn invalidate(&self, params: &orders::InvalidateParams) -> Result<(), OrderError> {
        let result = self.0.order(orders::PERMISSIONS_INVALIDATE, Some(params))?;
        match serde_json::from_value::<Ack>(result) {
            Ok(Ack { ok: true }) => Ok(()),
            _ => Err(OrderError::BadAnswer("expected {\"ok\": true}".into())),
        }
    }
}
