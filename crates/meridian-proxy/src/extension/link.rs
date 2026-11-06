//! The proxy's end of one extension's connection: its settings, its state, and the call that a decision
//! point will make (docs/EXTENSIONS.md, 4.1). The connection itself, and the thread that keeps it up, are
//! in `connection.rs`.
//!
//! A call is made on the thread that is already handling a player. It takes a place among the
//! `max-in-flight`, asks the breaker, sends one request on the shared writer and waits for the reader
//! thread to hand it the answer by its id, until its deadline. Nothing here waits for a free place, for
//! the breaker or for a connection: a call that cannot go ends at once, and its caller applies the
//! on-error policy.

use super::breaker::{Breaker, Change, Permit};
use super::stats::{Outcome, Stats};
use meridian_extension::wire::{self, CommandSpec, Id, RpcError};
use meridian_websocket::Writer;
use serde_json::Value;
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, mpsc};
use std::time::{Duration, Instant};

/// How long the steps of a connection take, and how long the proxy waits between two tries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Timings {
    /// To open the TCP connection.
    pub connect: Duration,
    /// For the upgrade and the whole of the hello, from the first byte sent.
    pub hello: Duration,
    /// A write may not block longer than this.
    pub write: Duration,
    /// How often a connection that hears nothing looks at the stop flag.
    pub look: Duration,
    /// The wait after the first failure; it doubles up to `longest_wait`.
    pub first_wait: Duration,
    /// The longest wait, and the one after a refusal (a wrong token, no version in common).
    pub longest_wait: Duration,
}

impl Default for Timings {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(2),
            hello: Duration::from_secs(5),
            write: Duration::from_secs(5),
            look: Duration::from_millis(200),
            first_wait: Duration::from_secs(1),
            longest_wait: Duration::from_secs(30),
        }
    }
}

/// One `[[extension]]` (docs/EXTENSIONS.md, 4.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// In logs and metrics.
    pub name: String,
    /// Where it listens, as `host:port`. Every address it resolves to must be on the loopback.
    pub address: String,
    /// The file that holds the token, read again at each connection.
    pub token_file: PathBuf,
    /// The capabilities the operator grants it.
    pub grant: Vec<String>,
    /// The most requests that may wait for its answers at once.
    pub max_in_flight: usize,
    /// Decisions in a row without a usable answer that open the breaker.
    pub breaker_failures: u32,
    /// How long the breaker stays open.
    pub breaker_open: Duration,
    pub timings: Timings,
}

impl Settings {
    /// The defaults of the specification: 64 in flight, 5 failures, 30 seconds. The proxy builds its
    /// settings from the configuration, whose defaults are these; the tests build them here.
    #[cfg(test)]
    pub fn new(
        name: impl Into<String>,
        address: impl Into<String>,
        token_file: impl Into<PathBuf>,
    ) -> Self {
        Self {
            name: name.into(),
            address: address.into(),
            token_file: token_file.into(),
            grant: Vec::new(),
            max_in_flight: 64,
            breaker_failures: 5,
            breaker_open: Duration::from_secs(30),
            timings: Timings::default(),
        }
    }
}

impl From<&meridian_config::Extension> for Settings {
    /// What the connection needs of an `[[extension]]`. The decision points read their own tables
    /// (`decide-login` and the others) from the configuration in force, so that changing a deadline
    /// does not close the connection.
    fn from(e: &meridian_config::Extension) -> Self {
        Self {
            name: e.name.clone(),
            address: e.address.clone(),
            token_file: e.token_file.clone(),
            grant: e.grant.clone(),
            max_in_flight: e.max_in_flight as usize,
            breaker_failures: e.breaker_failures,
            breaker_open: Duration::from_secs(e.breaker_seconds),
            timings: Timings::default(),
        }
    }
}

/// What the hello told about the extension. The extension's own words (`name`, `version`, the
/// capabilities it asks for) are cleaned before they get here: they are only for logs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Info {
    pub protocol: u32,
    pub name: String,
    pub version: String,
    /// Asked for and granted.
    pub granted: Vec<String>,
    /// Asked for and not granted.
    pub refused: Vec<String>,
    /// The commands it declares (they count only with the `commands` capability).
    pub commands: Vec<CommandSpec>,
}

/// Why a call did not give an answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallError {
    /// There is no connection, or it broke while the call waited.
    Disconnected,
    /// `max-in-flight` requests are waiting already.
    Busy,
    /// The breaker is open.
    BreakerOpen,
    /// The deadline passed.
    Timeout,
    /// The answer is an error, or not what the method defines. The text says why in a few words and
    /// never quotes the answer.
    Invalid(String),
}

impl fmt::Display for CallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CallError::Disconnected => f.write_str("not connected"),
            CallError::Busy => f.write_str("too many requests waiting"),
            CallError::BreakerOpen => f.write_str("the breaker is open"),
            CallError::Timeout => f.write_str("no answer in time"),
            CallError::Invalid(why) => write!(f, "invalid answer: {why}"),
        }
    }
}

type Reply = Result<Value, RpcError>;

#[derive(Default)]
pub(super) struct State {
    /// The writing half of the connection that is up.
    pub writer: Option<Writer>,
    next_id: u64,
    waiting: HashMap<u64, mpsc::Sender<Reply>>,
    pub info: Option<Info>,
}

/// One extension, as the proxy sees it.
pub struct Link {
    pub(super) settings: Settings,
    pub(super) stats: Arc<Stats>,
    breaker: Mutex<Breaker>,
    pub(super) state: Mutex<State>,
    changed: Condvar,
    stop: AtomicBool,
    /// The last wait the connection took between two tries, in milliseconds (to look at, and for tests).
    pub(super) last_wait_ms: AtomicU64,
}

impl Link {
    pub(super) fn new(settings: Settings) -> Arc<Self> {
        let breaker = Breaker::new(settings.breaker_failures, settings.breaker_open);
        Arc::new(Self {
            settings,
            stats: Arc::new(Stats::default()),
            breaker: Mutex::new(breaker),
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            stop: AtomicBool::new(false),
            last_wait_ms: AtomicU64::new(0),
        })
    }

    pub fn name(&self) -> &str {
        &self.settings.name
    }

    pub fn stats(&self) -> &Arc<Stats> {
        &self.stats
    }

    /// Whether a checked connection is up.
    pub fn is_connected(&self) -> bool {
        self.lock().writer.is_some()
    }

    /// What the extension declared in the hello of the connection that is up.
    pub fn info(&self) -> Option<Info> {
        self.lock().info.clone()
    }

    /// The wait the connection took before its latest try.
    #[cfg(test)]
    pub fn last_wait(&self) -> Duration {
        Duration::from_millis(self.last_wait_ms.load(Ordering::Relaxed))
    }

    pub(super) fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn breaker(&self) -> MutexGuard<'_, Breaker> {
        self.breaker.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(super) fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// Ends the connection and the thread that keeps it up. Calls that wait end as `Disconnected`.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        let writer = self.lock().writer.clone();
        if let Some(writer) = writer {
            let _ = writer.close(meridian_websocket::close::NORMAL, "");
            let _ = writer.shutdown();
        }
        self.changed.notify_all();
    }

    /// Waits `time`, or less if the link is stopped.
    pub(super) fn pause(&self, time: Duration) {
        let until = Instant::now() + time;
        let mut state = self.lock();
        while !self.stopped() {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            state = self
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// A connection was checked: calls may go, and the breaker is closed.
    pub(super) fn connected(&self, writer: Writer, info: Info) {
        {
            let mut state = self.lock();
            state.writer = Some(writer);
            state.info = Some(info);
        }
        self.stats.set_connected(true);
        let change = self.breaker().reset();
        self.breaker_changed(change);
    }

    /// The connection ended: calls that wait are told, and the breaker opens until the next hello.
    pub(super) fn disconnected(&self) {
        let waiting = {
            let mut state = self.lock();
            state.writer = None;
            state.info = None;
            // Dropping the senders wakes every call that waits.
            std::mem::take(&mut state.waiting)
        };
        drop(waiting);
        self.stats.set_connected(false);
        let change = self.breaker().trip();
        self.breaker_changed(change);
    }

    /// An answer came from the extension for request `id`. Nobody waits for it any more if the
    /// deadline passed: it is dropped and counted.
    pub(super) fn answered(&self, id: Option<&Id>, reply: Reply) {
        let sender = match id {
            Some(Id::Number(n)) => self.lock().waiting.remove(n),
            _ => None,
        };
        match sender {
            Some(sender) => {
                if sender.send(reply).is_err() {
                    self.stats.late_answer();
                }
            }
            None => self.stats.late_answer(),
        }
    }

    fn breaker_changed(&self, change: Option<Change>) {
        match change {
            Some(Change::Opened) => {
                self.stats.set_breaker_open(true);
                if self.is_connected() {
                    crate::log::warning!(
                        "extension \"{}\": not answering, decisions use their on-error policy for {} s",
                        self.settings.name,
                        self.settings.breaker_open.as_secs()
                    );
                }
            }
            Some(Change::Closed) => {
                self.stats.set_breaker_open(false);
                crate::log::info!("extension \"{}\": answering again", self.settings.name);
            }
            None => {}
        }
    }

    fn end(&self, method: &'static str, outcome: Outcome, took: Option<Duration>) {
        self.stats.request(method, outcome, took);
    }

    /// Asks the extension `method` with `params`, and gives `parse` the answer's `result` to turn into
    /// what the method defines. The deadline counts from the send to the answer's arrival.
    ///
    /// `parse` is where an answer that is not what the method defines is found out: its error is the
    /// reason, in a few words, and is what is logged, never the answer. A usable answer, an invalid
    /// one and a deadline that passed count toward the breaker; `Busy`, `BreakerOpen` and
    /// `Disconnected` do not.
    pub fn call<T>(
        &self,
        method: &'static str,
        params: Option<&Value>,
        timeout: Duration,
        parse: impl FnOnce(&Value) -> Result<T, String>,
    ) -> Result<T, CallError> {
        // A place, and an id.
        let (id, writer, replies) = {
            let mut state = self.lock();
            let Some(writer) = state.writer.clone() else {
                drop(state);
                self.end(method, Outcome::Disconnected, None);
                return Err(CallError::Disconnected);
            };
            if state.waiting.len() >= self.settings.max_in_flight {
                drop(state);
                self.end(method, Outcome::Busy, None);
                return Err(CallError::Busy);
            }
            state.next_id += 1;
            let id = state.next_id;
            let (sender, replies) = mpsc::channel();
            state.waiting.insert(id, sender);
            (id, writer, replies)
        };
        let forget = |id: u64| {
            self.lock().waiting.remove(&id);
        };
        // The breaker.
        let trial = match self.breaker().permit(Instant::now()) {
            Permit::Go => false,
            Permit::Trial => true,
            Permit::Refuse => {
                forget(id);
                self.end(method, Outcome::Breaker, None);
                return Err(CallError::BreakerOpen);
            }
        };
        // The request, and the wait for its answer.
        let started = Instant::now();
        let request = wire::request(id, method, params);
        if writer.send_text(&request).is_err() {
            forget(id);
            self.end(method, Outcome::Disconnected, None);
            return Err(CallError::Disconnected);
        }
        let outcome = match replies.recv_timeout(timeout) {
            Ok(Ok(result)) => parse(&result).map_err(|why| (Outcome::Invalid, why)),
            Ok(Err(error)) => Err((
                Outcome::Invalid,
                format!("the extension answered the error {}", error.code),
            )),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                forget(id);
                Err((Outcome::Timeout, String::new()))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                self.end(method, Outcome::Disconnected, None);
                return Err(CallError::Disconnected);
            }
        };
        let took = started.elapsed();
        let (outcome, result) = match outcome {
            Ok(value) => (Outcome::Answered, Ok(value)),
            Err((Outcome::Timeout, _)) => (Outcome::Timeout, Err(CallError::Timeout)),
            Err((outcome, why)) => {
                crate::log::warning!(
                    "extension \"{}\": invalid answer to {method}: {why}",
                    self.settings.name
                );
                (outcome, Err(CallError::Invalid(why)))
            }
        };
        self.end(method, outcome, Some(took));
        let change = match outcome {
            Outcome::Answered => self.breaker().success(trial),
            _ => self.breaker().failure(Instant::now(), trial),
        };
        self.breaker_changed(change);
        result
    }
}
