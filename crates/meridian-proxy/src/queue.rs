//! The wait for a place (`[queue]`, docs/OPERATING.md section 5.8). A player who finds
//! the proxy full, or every lobby at its hard cap, waits in a line, first come first
//! served, instead of being refused. The wait is bounded in time and in length, and it
//! costs nothing but the thread the player's connection already has: that thread sleeps
//! on a condition variable and is woken when a place is given back, when the limit
//! changes or when the proxy stops. A tick of one second lets it see that the player
//! left, since nothing tells it otherwise.
//!
//! Who waits has passed the throttle, the ban list and the whitelist, and has logged in:
//! all of that comes before the line.
use crate::console::{Commands, Reply, usage};
use crate::log::info;
use crate::metrics::{LoginFailure, Metrics};
use crate::route::Full;
use crate::session::{Sessions, hyphenated, refuse};
use crate::state::Proxy;
use horizon_protocol::connection::Connection;
use meridian_config::QueueSettings;
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// How often a waiting thread looks up from its sleep to see whether its player is
/// still there and the proxy still runs.
const TICK: Duration = Duration::from_secs(1);

/// The most packets a player may send while they wait. A client sends two or three
/// when it enters the configuration phase (its settings, its brand); a player who
/// sends more is not waiting quietly, and leaves the line.
const MAX_EARLY_PACKETS: usize = 64;

/// The most bytes of those packets kept. A client's settings, brand and known packs
/// are a few hundred bytes; each packet may be 8 MiB once inflated, so the number of
/// packets alone would let one waiting player hold half a gigabyte.
const MAX_EARLY_BYTES: usize = 1 << 20;

/// The most times one player is put back at the head of the line because every
/// lobby was still at its hard cap when their turn came.
const MAX_RETRIES: u32 = 100;

/// A place in the line.
struct Entry {
    id: u64,
    name: String,
    since: Instant,
}

#[derive(Default)]
struct State {
    line: VecDeque<Entry>,
    next: u64,
    /// Counts the wake-ups, so that a waiter can tell that something happened.
    events: u64,
}

/// The line of players who wait for a place.
#[derive(Default)]
pub struct LoginQueue {
    state: Mutex<State>,
    changed: Condvar,
    /// `max-players` of the configuration in force, once a reload has set it; until
    /// then the one of the login's own configuration is used.
    limit: AtomicU32,
    limited: AtomicBool,
}

/// Why a player did not get a place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// There is no queue, or it is too long.
    Full,
    /// They waited as long as they could.
    TimedOut,
    /// They left while they waited.
    Gone,
    /// The proxy is stopping.
    Stopping,
}

/// What the thread of a waiting player sees of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Watch {
    Waiting,
    Gone,
    Stopping,
}

/// A player's own part of the line: whether they are in it, and since when. It leaves
/// the line when dropped.
pub struct Line<'q> {
    queue: &'q LoginQueue,
    metrics: &'q Metrics,
    settings: QueueSettings,
    name: String,
    id: Option<u64>,
    deadline: Option<Instant>,
    retries: u32,
    /// What the player sent while they waited, to be passed to the backend they end on.
    early: Vec<(i32, Vec<u8>)>,
}

impl<'q> Line<'q> {
    pub fn new(
        queue: &'q LoginQueue,
        metrics: &'q Metrics,
        settings: &QueueSettings,
        name: &str,
    ) -> Self {
        Line {
            queue,
            metrics,
            settings: settings.clone(),
            name: name.to_owned(),
            id: None,
            deadline: None,
            retries: 0,
            early: Vec::new(),
        }
    }

    /// Whether the queue is on for this player.
    pub fn enabled(&self) -> bool {
        self.settings.enabled
    }

    /// Whether the player has waited, or waits now.
    pub fn was_queued(&self) -> bool {
        self.id.is_some()
    }

    /// Takes a place when it is the player's turn, waiting for it in the line when it is
    /// not. `try_take` takes the place if there is one (it must not block), and `watch`
    /// says whether the player is still there. With the queue off this is `try_take`
    /// once.
    ///
    /// A player who waited stays at the head of the line after this returns, until
    /// [`Line::served`]: someone who arrives meanwhile goes behind them, and if their
    /// lobbies turn out to be full they keep their turn ([`Line::await_room`]).
    pub fn take<T>(
        &mut self,
        mut try_take: impl FnMut() -> Option<T>,
        who: &mut Who<'_>,
    ) -> Result<T, Refused> {
        let queue = self.queue;
        let mut state = queue.lock();
        if self.id.is_none() {
            if !self.settings.enabled {
                drop(state);
                return try_take().ok_or(Refused::Full);
            }
            // A newcomer finds a place free only when nobody is ahead of them.
            if state.line.is_empty() {
                if let Some(taken) = try_take() {
                    return Ok(taken);
                }
            }
            self.enter(&mut state)?;
        }
        loop {
            let me = self.id.expect("the player is in the line");
            if state.line.front().is_some_and(|e| e.id == me) {
                if let Some(taken) = try_take() {
                    return Ok(taken);
                }
            }
            state = self.sleep(state, who)?;
        }
    }

    /// Every lobby was at its hard cap when the player's turn came: they keep their
    /// place at the head, give the room back, and wait until something changes before
    /// trying again. Enters the line at its end if the player was not in it.
    pub fn await_room(&mut self, who: &mut Who<'_>) -> Result<(), Refused> {
        if !self.settings.enabled {
            return Err(Refused::Full);
        }
        let mut state = self.queue.lock();
        if self.id.is_none() {
            self.enter(&mut state)?;
        }
        self.retries += 1;
        if self.retries > MAX_RETRIES {
            drop(state);
            self.leave(Left::TimedOut);
            return Err(Refused::TimedOut);
        }
        // One sleep, ended by a signal or by the tick: either way the caller tries
        // again, since a lobby may have room again without any place having moved
        // (an undrain, a server back).
        drop(self.sleep(state, who)?);
        Ok(())
    }

    /// Puts the player in the line, if there is room in it.
    fn enter(&mut self, state: &mut State) -> Result<(), Refused> {
        if state.line.len() >= self.settings.max_length as usize {
            self.metrics.queue_refused();
            return Err(Refused::Full);
        }
        let id = state.next;
        state.next += 1;
        let now = Instant::now();
        state.line.push_back(Entry {
            id,
            name: self.name.clone(),
            since: now,
        });
        self.id = Some(id);
        self.deadline = Some(now + Duration::from_secs(self.settings.wait_seconds));
        self.metrics.queue_entered();
        Ok(())
    }

    /// Sleeps until something is signalled or a tick passes, then looks at the player.
    /// Gives the state back, locked, or the reason the player leaves the line.
    fn sleep(
        &mut self,
        state: MutexGuard<'q, State>,
        who: &mut Who<'_>,
    ) -> Result<MutexGuard<'q, State>, Refused> {
        let deadline = self.deadline.expect("the player is in the line");
        let now = Instant::now();
        if now >= deadline {
            drop(state);
            self.leave(Left::TimedOut);
            return Err(Refused::TimedOut);
        }
        let slice = TICK.min(deadline - now);
        let (state, _) = self
            .queue
            .changed
            .wait_timeout(state, slice)
            .unwrap_or_else(|e| e.into_inner());
        // Reading the player takes the lock away for a moment, which is the only way
        // not to hold everyone else while a socket is read.
        drop(state);
        match who.look(&mut self.early) {
            Watch::Waiting => Ok(self.queue.lock()),
            Watch::Gone => {
                self.leave(Left::Gone);
                Err(Refused::Gone)
            }
            Watch::Stopping => {
                self.leave(Left::Gone);
                Err(Refused::Stopping)
            }
        }
    }

    /// The player got their place and their backend: they leave the line.
    pub fn served(&mut self) {
        self.leave(Left::Served);
    }

    fn leave(&mut self, cause: Left) {
        let Some(id) = self.id.take() else {
            return;
        };
        let mut state = self.queue.lock();
        let since = match state.line.iter().position(|e| e.id == id) {
            Some(at) => state.line.remove(at).map(|e| e.since),
            None => None,
        };
        state.events += 1;
        drop(state);
        self.queue.changed.notify_all();
        self.metrics
            .queue_left(cause, since.map(|s| s.elapsed()).unwrap_or_default());
    }

    /// Passes to `backend` what the player sent while they waited.
    pub fn forward_early(&mut self, backend: &mut Connection) {
        let mut body = Vec::new();
        for (id, payload) in self.early.drain(..) {
            body.clear();
            horizon_protocol::buf::write_varint(&mut body, id);
            body.extend_from_slice(&payload);
            if backend.send(&body).is_err() {
                return;
            }
        }
    }
}

impl Drop for Line<'_> {
    fn drop(&mut self) {
        // Whatever ended the login, the player is not in the line any more.
        self.leave(Left::Gone);
    }
}

/// How a player's time in the line ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Left {
    Served,
    TimedOut,
    Gone,
}

/// The player who waits, as the thread that waits for them sees them: their connection,
/// and whether the proxy is stopping.
pub struct Who<'a> {
    pub connection: &'a mut Connection,
    pub stopping: &'a dyn Fn() -> bool,
}

impl Who<'_> {
    fn look(&mut self, early: &mut Vec<(i32, Vec<u8>)>) -> Watch {
        if (self.stopping)() {
            return Watch::Stopping;
        }
        watch(self.connection, early)
    }
}

/// The bytes of the packets kept.
fn kept(early: &[(i32, Vec<u8>)]) -> usize {
    early.iter().map(|(_, payload)| payload.len()).sum()
}

/// Reads the player's connection for a moment: whatever they sent is kept, and a
/// connection that is closed or broken is a player who left. One who sends more than
/// [`MAX_EARLY_PACKETS`] packets or [`MAX_EARLY_BYTES`] bytes left too.
pub fn watch(connection: &mut Connection, early: &mut Vec<(i32, Vec<u8>)>) -> Watch {
    if connection
        .reader
        .set_read_timeout(Some(Duration::from_millis(1)))
        .is_err()
    {
        return Watch::Gone;
    }
    loop {
        match connection.recv() {
            Ok(packet)
                if early.len() < MAX_EARLY_PACKETS
                    && kept(early) + packet.1.len() <= MAX_EARLY_BYTES =>
            {
                early.push(packet)
            }
            Ok(_) => return Watch::Gone,
            Err(e) if e.is_timed_out() => return Watch::Waiting,
            Err(_) => return Watch::Gone,
        }
    }
}

impl LoginQueue {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Something a waiter cares about happened: a place was given back, the limit
    /// changed, the proxy is stopping.
    pub fn wake(&self) {
        // Taking the lock first means a waiter that has just looked and not yet gone to
        // sleep cannot miss it.
        self.lock().events += 1;
        self.changed.notify_all();
    }

    /// The player limit of the configuration in force, which a reload sets; `own` is the
    /// one of the login's configuration, used until a reload sets it.
    pub fn max_players(&self, own: u32) -> u32 {
        if self.limited.load(Ordering::Relaxed) {
            self.limit.load(Ordering::Relaxed)
        } else {
            own
        }
    }

    /// A reload put a configuration in force.
    pub fn set_max_players(&self, max_players: u32) {
        self.limit.store(max_players, Ordering::Relaxed);
        self.limited.store(true, Ordering::Relaxed);
        self.wake();
    }

    /// How many players wait.
    pub fn len(&self) -> usize {
        self.lock().line.len()
    }

    /// Who waits, the longest first, and for how long.
    pub fn waiting(&self) -> Vec<(String, Duration)> {
        self.lock()
            .line
            .iter()
            .map(|e| (e.name.clone(), e.since.elapsed()))
            .collect()
    }
}

/// Ends the login of a player who did not get a place. A player who left is simply
/// dropped; the others are told why, in the words a full proxy has always used.
pub(crate) fn turn_away(
    why: Refused,
    connection: Connection,
    sessions: &Sessions,
    peer: SocketAddr,
    name: &str,
    uuid: [u8; 16],
    reason: &str,
) {
    match why {
        Refused::Gone => info!("{peer}: {name} left the queue"),
        Refused::Stopping => refuse(connection, "Server closed"),
        Refused::Full | Refused::TimedOut => {
            let waited = if why == Refused::TimedOut {
                " after waiting in the queue"
            } else {
                ""
            };
            info!(
                "{peer}: {name} ({}) refused{waited}: {reason}",
                hyphenated(uuid)
            );
            sessions.metrics().login_failed(LoginFailure::Full);
            refuse(connection, Full::MESSAGE);
        }
    }
}

/// The console's `queue`.
pub fn command(proxy: &Proxy, commands: &Commands, args: &[&str]) -> Reply {
    if !args.is_empty() {
        return Err(usage("queue", commands));
    }
    let settings = proxy.config().queue.clone();
    let waiting = proxy.sessions.queue.waiting();
    let mut lines = vec![if settings.enabled {
        format!(
            "queue on: {} waiting (at most {}, each for {} s at most)",
            waiting.len(),
            settings.max_length,
            settings.wait_seconds
        )
    } else if waiting.is_empty() {
        return Ok(vec!["queue off".to_owned()]);
    } else {
        format!("queue off now: {} still waiting", waiting.len())
    }];
    for (i, (name, waited)) in waiting.iter().enumerate() {
        lines.push(format!("  {}. {name} for {} s", i + 1, waited.as_secs()));
    }
    Ok(lines)
}

#[cfg(test)]
mod tests;
