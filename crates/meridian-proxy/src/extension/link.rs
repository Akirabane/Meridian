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
use super::orders::{Actions, Budget, Facts, Services};
use super::stats::{Outcome, Stats};
use meridian_extension::wire::{self, CommandSpec, Id, RpcError};
use meridian_websocket::Writer;
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
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
    /// A message of the extension must arrive in all within this, from its first byte to its last: the
    /// stream's read timeout counts each read, and an extension that sends a byte at a time would hold
    /// the reader as long as it liked.
    pub message: Duration,
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
            message: Duration::from_secs(5),
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
    /// The orders it may give each second, once its burst is used (docs/EXTENSIONS.md, section 5).
    pub order_rate: u32,
    /// The orders it may give at once, from a rested budget.
    pub order_burst: u32,
    /// The orders that may run at once.
    pub orders_running: usize,
    /// The players it may kick in a minute; 0 is no limit.
    pub kicks_per_minute: u32,
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
            order_rate: 20,
            order_burst: 50,
            orders_running: 8,
            kicks_per_minute: 0,
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
            order_rate: e.orders.rate_per_second,
            order_burst: e.orders.burst,
            orders_running: e.orders.max_running as usize,
            kicks_per_minute: e.orders.max_kicks_per_minute,
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
    /// The writing half of the connection that is up. A call closes it when it finds the extension
    /// does not read; the requests themselves go through `outbox`.
    pub writer: Option<Writer>,
    /// The requests to send, which a thread of the connection writes one at a time. A call puts its
    /// request here and does not wait for the socket: an extension that has stopped reading blocks
    /// that one thread, and the calls wait for their answers at their own deadlines instead of in
    /// turn for the write.
    pub outbox: Option<mpsc::SyncSender<String>>,
    next_id: u64,
    waiting: HashMap<u64, mpsc::Sender<Reply>>,
    pub info: Option<Info>,
    /// The commands of its own the extension declared in its latest hello, **kept when the connection
    /// ends**: a command that an extension that is down declared is still the extension's, and a player who
    /// types it reads that it did not answer (docs/EXTENSIONS.md, 4.3). Only a hello changes it.
    pub declared: Vec<CommandSpec>,
}

/// One of the extension's own commands is running; the place is free again when this goes.
pub(crate) struct RunPlace(Arc<Link>);

impl Drop for RunPlace {
    fn drop(&mut self) {
        self.0.runs.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A send of players is running for the extension: held by the thread that moves them, and let go when it ends, **however it
/// ends** (a panic included). The extension has one at a time.
pub struct SendLot(Arc<AtomicBool>);

impl Drop for SendLot {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
impl SendLot {
    /// A lot that no link holds, for the tests of what a plan does with it.
    pub(crate) fn free() -> Self {
        Self(Arc::new(AtomicBool::new(true)))
    }

    /// A lot, and the flag that says whether it is still held: true until the lot is dropped.
    pub(crate) fn watched() -> (Self, Arc<AtomicBool>) {
        let flag = Arc::new(AtomicBool::new(true));
        (Self(Arc::clone(&flag)), flag)
    }
}

/// A place among the kicks of the last minute (see [`Link::reserve_kick`]).
#[derive(Debug)]
pub(super) struct KickSlot(Option<Instant>);

/// One order of the extension is running; the place is free again when this goes.
pub(super) struct OrderPlace(Arc<Link>);

impl Drop for OrderPlace {
    fn drop(&mut self) {
        self.0.orders_running.fetch_sub(1, Ordering::SeqCst);
    }
}

/// One thread of an order of the extension is alive, running the order or waiting to write its answer; it goes
/// when the thread does.
pub(super) struct OrderThread(Arc<Link>);

impl Drop for OrderThread {
    fn drop(&mut self) {
        self.0.order_threads.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Takes one of `most` places counted in `running`, if there is one.
fn acquire(running: &AtomicUsize, most: usize) -> bool {
    let mut now = running.load(Ordering::SeqCst);
    loop {
        if now >= most {
            return false;
        }
        match running.compare_exchange_weak(now, now + 1, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return true,
            Err(seen) => now = seen,
        }
    }
}

/// The most requests to forget one player's permissions that a session can still be told of. Past it the oldest
/// are lost, and a session that did not see them forgets everything it kept: a bound on memory that errs on the
/// side of forgetting.
const MAX_FORGOTTEN: usize = 256;

/// What the extension asked the proxy to forget of the permissions it kept (`meridian:permissions/invalidate`).
#[derive(Default)]
struct Forgotten {
    /// How many times it asked for all.
    all: u64,
    /// How many times it asked for one player: the number of the latest.
    seq: u64,
    /// The number of the latest request that fell off `recent`.
    lost: u64,
    /// The latest requests for one player, by number, oldest first.
    recent: VecDeque<(u64, String)>,
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
    /// How many times the extension has said hello: what it answered before a new hello is not what it
    /// would answer after it (a restarted extension may have new rights to give), so what is kept of its
    /// answers is dropped when this changes.
    epoch: AtomicU64,
    /// How many of the extension's own commands are running (`run_place`).
    runs: AtomicUsize,
    /// How many of its orders are running (`order_place`).
    orders_running: AtomicUsize,
    /// Whether a send of players is running: held by the thread that moves them.
    sending: Arc<AtomicBool>,
    /// How many threads of its orders are alive, whether they run or wait to write (`order_thread`).
    order_threads: AtomicUsize,
    /// What is left of its budget of orders.
    budget: Mutex<Budget>,
    /// When it kicked the players of the last minute, and those that orders now running are about to kick.
    kicks: Mutex<Vec<Instant>>,
    forgotten: Mutex<Forgotten>,
    /// What the orders need of the rest of the proxy.
    services: Arc<Services>,
}

impl Link {
    pub(super) fn new(settings: Settings, services: Arc<Services>) -> Arc<Self> {
        let breaker = Breaker::new(settings.breaker_failures, settings.breaker_open);
        let budget = Budget::new(settings.order_rate, settings.order_burst, Instant::now());
        Arc::new(Self {
            settings,
            stats: Arc::new(Stats::default()),
            breaker: Mutex::new(breaker),
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            stop: AtomicBool::new(false),
            last_wait_ms: AtomicU64::new(0),
            epoch: AtomicU64::new(0),
            runs: AtomicUsize::new(0),
            orders_running: AtomicUsize::new(0),
            sending: Arc::new(AtomicBool::new(false)),
            order_threads: AtomicUsize::new(0),
            budget: Mutex::new(budget),
            kicks: Mutex::new(Vec::new()),
            forgotten: Mutex::new(Forgotten::default()),
            services,
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

    /// The commands it declared in its latest hello, whether it is connected now or not (see the field).
    pub fn declared(&self) -> Vec<CommandSpec> {
        self.lock().declared.clone()
    }

    /// A place for one of the extension's own commands to run, or `None` when half of `max-in-flight` are
    /// running already. The commands share the extension's places with its decisions; without this a number
    /// of players equal to `max-in-flight` typing a slow command would leave a login or a permission no
    /// place, and it would take its on-error policy. The place is given back when it is dropped.
    pub(crate) fn run_place(self: &Arc<Self>) -> Option<RunPlace> {
        let most = (self.settings.max_in_flight / 2).max(1);
        acquire(&self.runs, most).then(|| RunPlace(Arc::clone(self)))
    }

    /// A place for one order of the extension to run, or `None` when `orders_running` of them run already.
    pub(super) fn order_place(self: &Arc<Self>) -> Option<OrderPlace> {
        acquire(&self.orders_running, self.settings.orders_running)
            .then(|| OrderPlace(Arc::clone(self)))
    }

    /// The facts the orders that read tell, if the proxy has given them.
    pub(super) fn facts(&self) -> Option<Arc<dyn Facts>> {
        self.services.facts()
    }

    /// The place for a send of players, or `None` while one is running: an extension sends one lot at a time, so that a program
    /// cannot start threads for ever, and each lot is moved in the order it was asked.
    pub(super) fn send_lot(&self) -> Option<SendLot> {
        self.sending
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
            .then(|| SendLot(Arc::clone(&self.sending)))
    }

    /// A place among the kicks of the last minute, taken **before** the player is looked for so that two orders at once cannot
    /// both pass the limit; `None` when `max-kicks-per-minute` were kicked (or are being) in the last minute. Without a limit (0)
    /// it is always given and nothing is counted. [`Link::give_back_kick`] puts it back when nobody was kicked.
    pub(super) fn reserve_kick(&self, now: Instant) -> Option<KickSlot> {
        let most = self.settings.kicks_per_minute as usize;
        if most == 0 {
            return Some(KickSlot(None));
        }
        let mut kicks = self.kicks.lock().unwrap_or_else(|e| e.into_inner());
        kicks.retain(|at| now.saturating_duration_since(*at) < Duration::from_secs(60));
        if kicks.len() >= most {
            return None;
        }
        kicks.push(now);
        Some(KickSlot(Some(now)))
    }

    /// Gives back the place of a kick that kicked nobody (the player was not online, was protected, or the name was ambiguous):
    /// only a kick that ended a session counts against the limit.
    pub(super) fn give_back_kick(&self, slot: KickSlot) {
        if let Some(at) = slot.0 {
            let mut kicks = self.kicks.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(i) = kicks.iter().position(|k| *k == at) {
                kicks.swap_remove(i);
            }
        }
    }

    /// What the orders that act act on, if the proxy has given it.
    pub(super) fn actions(&self) -> Option<Arc<dyn Actions>> {
        self.services.actions()
    }

    /// How many threads of orders are alive, running or waiting to write (for the tests).
    #[cfg(test)]
    pub(super) fn order_threads_alive(&self) -> usize {
        self.order_threads.load(Ordering::SeqCst)
    }

    /// A thread for one order, or `None` when `2 x max-running` of them are alive. An order gives its place back
    /// when it has run, **before** it writes its answer, and a write that blocks (an extension that does not
    /// read) holds its thread for the write timeout: so the places bound the orders that run, and this bounds the
    /// threads that wait to write, to as many again.
    pub(super) fn order_thread(self: &Arc<Self>) -> Option<OrderThread> {
        acquire(
            &self.order_threads,
            self.settings.orders_running.saturating_mul(2),
        )
        .then(|| OrderThread(Arc::clone(self)))
    }

    /// Spends `cost` of the extension's budget of orders, or says there is not enough left.
    pub(super) fn take_order(&self, cost: u32) -> bool {
        self.budget
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take(cost, Instant::now())
    }

    /// Whether the budget of orders has a token to spare, taking none: the test of an order that is charged by what it
    /// reaches, before it computes what that is.
    pub(super) fn has_order_budget(&self) -> bool {
        self.budget
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .has_one(Instant::now())
    }

    /// Charges the whole of an order's cost at once, or none of it: `true` when the budget paid `cost` (at least one, at most
    /// the burst). A refused order is not free: it still costs one token, if there is one, so that trying again and again
    /// to find a cost that fits is not free either.
    pub(super) fn charge_order(&self, cost: u32) -> bool {
        self.budget
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take_all_or_one(cost, Instant::now())
    }

    /// Whether the connection that is up was granted `capability` (asked for in its hello and granted).
    pub(super) fn is_granted(&self, capability: &str) -> bool {
        self.lock()
            .info
            .as_ref()
            .is_some_and(|info| info.granted.iter().any(|c| c == capability))
    }

    fn forgotten(&self) -> MutexGuard<'_, Forgotten> {
        self.forgotten.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The extension asked the proxy to forget the permissions it kept of its answers, for every player.
    pub(crate) fn forget_all(&self) {
        self.forgotten().all += 1;
    }

    /// The same, for the player with this unique id (canonical spelling).
    pub(crate) fn forget_player(&self, uuid: String) {
        let mut forgotten = self.forgotten();
        forgotten.seq += 1;
        let seq = forgotten.seq;
        forgotten.recent.push_back((seq, uuid));
        while forgotten.recent.len() > MAX_FORGOTTEN {
            if let Some((lost, _)) = forgotten.recent.pop_front() {
                forgotten.lost = lost;
            }
        }
    }

    /// How many times the extension has asked to forget everyone's permissions: a session that kept answers
    /// under another number drops them.
    pub fn forgotten_all(&self) -> u64 {
        self.forgotten().all
    }

    /// The number of the latest request to forget one player, and whether a request newer than `since` was for
    /// this player (`uuid`, any case) or may have been, because it fell off the list that is kept.
    pub fn forgotten_player(&self, since: u64, uuid: &str) -> (u64, bool) {
        let forgotten = self.forgotten();
        let hit = forgotten.lost > since
            || forgotten
                .recent
                .iter()
                .any(|(seq, who)| *seq > since && who.eq_ignore_ascii_case(uuid));
        (forgotten.seq, hit)
    }

    /// How many hellos the extension has been through (see the field).
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Relaxed)
    }

    /// The wait the connection took before its latest try.
    #[cfg(test)]
    pub fn last_wait(&self) -> Duration {
        Duration::from_millis(self.last_wait_ms.load(Ordering::Relaxed))
    }

    pub(super) fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(super) fn breaker(&self) -> MutexGuard<'_, Breaker> {
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
    pub(super) fn connected(&self, writer: Writer, outbox: mpsc::SyncSender<String>, info: Info) {
        {
            let mut state = self.lock();
            state.writer = Some(writer);
            state.outbox = Some(outbox);
            // Only an extension that asked for `commands` and is granted it has commands.
            state.declared = if info.granted.iter().any(|c| c == meridian_config::COMMANDS) {
                info.commands.clone()
            } else {
                Vec::new()
            };
            state.info = Some(info);
        }
        self.epoch.fetch_add(1, Ordering::Relaxed);
        self.stats.set_connected(true);
        let change = self.breaker().reset();
        self.breaker_changed(change);
    }

    /// The connection ended: calls that wait are told, and the breaker opens until the next hello.
    pub(super) fn disconnected(&self) {
        let waiting = {
            let mut state = self.lock();
            state.writer = None;
            // The thread that writes ends when the last sender is gone.
            state.outbox = None;
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
        self.call_counting(method, params, timeout, parse, true)
    }

    /// [`Link::call`], where `counts` says whether the call counts for the breaker. A command of the
    /// extension's own does not, **for it or against it**: one command that is slow must not make every login
    /// and every permission take its on-error policy, and one that answers must not hide that the logins do
    /// not (an extension whose logins time out and whose commands answer would never open its breaker if an
    /// answer reset the count of failures in a row). The call is only refused while the breaker is open, and
    /// it is never the trial of one: the next decision is.
    pub fn call_counting<T>(
        &self,
        method: &'static str,
        params: Option<&Value>,
        timeout: Duration,
        parse: impl FnOnce(&Value) -> Result<T, String>,
        counts: bool,
    ) -> Result<T, CallError> {
        // A place, and an id.
        let (id, writer, outbox, replies) = {
            let mut state = self.lock();
            let (Some(writer), Some(outbox)) = (state.writer.clone(), state.outbox.clone()) else {
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
            (id, writer, outbox, replies)
        };
        let forget = |id: u64| {
            self.lock().waiting.remove(&id);
        };
        // The breaker.
        let trial = if counts {
            match self.breaker().permit(Instant::now()) {
                Permit::Go => false,
                Permit::Trial => true,
                Permit::Refuse => {
                    forget(id);
                    self.end(method, Outcome::Breaker, None);
                    return Err(CallError::BreakerOpen);
                }
            }
        } else if self.breaker().is_open() {
            // Looked at, not asked: a call that counts for nothing is never the trial.
            forget(id);
            self.end(method, Outcome::Breaker, None);
            return Err(CallError::BreakerOpen);
        } else {
            false
        };
        // The request, and the wait for its answer.
        let started = Instant::now();
        let request = wire::request(id, method, params);
        // Handed to the thread that writes, without waiting for the socket. The outbox holds as many
        // requests as there are places; it is full only when that thread has been stuck on a write for
        // as long as the places took to fill, which is an extension that does not read. Then this is a
        // failure of the extension, the trial's included (a trial that ended with no verdict would
        // leave the breaker waiting for it), and the connection is closed so that it is made again.
        if outbox.try_send(request).is_err() {
            forget(id);
            self.end(method, Outcome::Disconnected, None);
            if counts {
                let change = self.breaker().failure(Instant::now(), trial);
                self.breaker_changed(change);
            }
            let _ = writer.shutdown();
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
        if counts {
            let change = match outcome {
                Outcome::Answered => self.breaker().success(trial),
                _ => self.breaker().failure(Instant::now(), trial),
            };
            self.breaker_changed(change);
        }
        result
    }
}
