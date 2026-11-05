//! `to-servers`: a ban, a pardon or a whitelist change made at the proxy's console is sent to
//! the servers through their sidecars (docs/OPERATING.md, section 5.10).
//!
//! The change is made on the proxy's lists first, and that is what refuses the player; sending
//! it never holds the console or a login back. Each backend that has a sidecar has a lane of
//! its own, a queue of the changes it has still to be told, in the order they were made, so
//! that a ban and the pardon after it cannot cross. A thread tries the head of each lane that
//! is ready; a server that cannot be reached is tried again after a wait that grows, six times
//! at most, and then the change is given up (said once). A server that answers with an error
//! is not tried again: it will not heal.
//!
//! A change that comes from a server is never sent on: only the console makes these.
use super::events::SOURCE;
use crate::log::{info, warning};
use meridian_config::{Config, Sidecar, Target};
use meridian_sidecar::client::SidecarClient;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The most changes a lane holds. Past it the oldest is dropped, said: a server that stays
/// away for long does not make the proxy keep what no one will read.
pub const LANE_LEN: usize = 256;

/// The waits between the tries at one change; there are as many tries as waits, and one
/// more at the start.
pub const RETRIES: [Duration; 6] = [
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(15),
    Duration::from_secs(30),
    Duration::from_secs(60),
    Duration::from_secs(120),
];

/// How long a connection to a sidecar waits for an answer: longer than the sidecar waits for
/// the server's own (`meridian_sidecar::lists::CALL_TIMEOUT`).
const BUDGET: Duration = Duration::from_secs(15);

/// How often the lanes are looked at.
const TICK: Duration = Duration::from_millis(100);

/// One change of the proxy's lists, to tell the servers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Ban { who: Target, reason: Option<String> },
    Pardon { who: Target },
    Allow { who: Target },
    Disallow { who: Target },
}

/// `{"name": ...}` or `{"id": ...}`, a player as the protocol names them.
fn player(who: &Target) -> Result<Value, String> {
    match who {
        Target::Name(name) => Ok(json!({ "name": name })),
        Target::Uuid(_) => Ok(json!({ "id": who.to_string() })),
        Target::Net(_) => Err("an address is not a player".into()),
    }
}

/// The address of an entry, when it is one and not a range: the servers ban single
/// addresses.
fn address(who: &Target) -> Result<String, String> {
    match who {
        Target::Net(net) => net.address().map(|a| a.to_string()).ok_or_else(|| {
            format!("{who} is a range: the servers ban single addresses, so it is not sent")
        }),
        _ => Err("not an address".into()),
    }
}

impl Change {
    /// The call that carries the change to a server (a method of the management protocol
    /// and its parameters), or why it cannot be one.
    pub fn call(&self) -> Result<(&'static str, Value), String> {
        let is_address = |who: &Target| matches!(who, Target::Net(_));
        Ok(match self {
            Change::Ban { who, reason } if is_address(who) => {
                let mut entry = json!({ "ip": address(who)?, "source": SOURCE });
                if let Some(reason) = reason {
                    entry["reason"] = json!(reason);
                }
                ("ip_bans/add", json!({ "add": [entry] }))
            }
            Change::Ban { who, reason } => {
                let mut entry = json!({ "player": player(who)?, "source": SOURCE });
                if let Some(reason) = reason {
                    entry["reason"] = json!(reason);
                }
                ("bans/add", json!({ "add": [entry] }))
            }
            Change::Pardon { who } if is_address(who) => {
                ("ip_bans/remove", json!({ "ip": [address(who)?] }))
            }
            Change::Pardon { who } => ("bans/remove", json!({ "remove": [player(who)?] })),
            Change::Allow { who } => ("allowlist/add", json!({ "add": [player(who)?] })),
            Change::Disallow { who } => ("allowlist/remove", json!({ "remove": [player(who)?] })),
        })
    }

    /// What the change is, for a line.
    pub fn describe(&self) -> String {
        match self {
            Change::Ban { who, .. } => format!("the ban of {who}"),
            Change::Pardon { who } => format!("the pardon of {who}"),
            Change::Allow { who } => format!("{who} added to the whitelist"),
            Change::Disallow { who } => format!("{who} removed from the whitelist"),
        }
    }
}

/// Why a call did not go through.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    /// Whether the server never said no (it could not be reached): worth trying again.
    pub lost: bool,
    pub why: String,
}

/// A way to make a call on a server: the connection to its sidecar, in tests a stand-in.
pub trait Manager: Send {
    fn call(&mut self, method: &str, params: Value) -> Result<(), Failure>;
}

/// How a sidecar is reached.
pub type Open = dyn Fn(&Sidecar) -> Result<Box<dyn Manager>, Failure> + Send + Sync;

struct Connection {
    client: SidecarClient,
}

impl Manager for Connection {
    fn call(&mut self, method: &str, params: Value) -> Result<(), Failure> {
        self.client
            .manage(method, params)
            .map(|_| ())
            .map_err(|e| Failure {
                lost: e.is_lost(),
                why: e.to_string(),
            })
    }
}

/// Connects to the sidecar over TCP with the secret of its file.
pub fn open_sidecar(sidecar: &Sidecar) -> Result<Box<dyn Manager>, Failure> {
    let lost = |why: String| Failure { lost: true, why };
    let secret = sidecar
        .read_secret()
        .map_err(|why| Failure { lost: false, why })?;
    let client = SidecarClient::connect(&sidecar.address, &secret, BUDGET)
        .map_err(|e| lost(format!("cannot reach it at {}: {e}", sidecar.address)))?;
    Ok(Box::new(Connection { client }))
}

/// What became of an attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Sent,
    /// The server could not be reached.
    Lost(String),
    /// The server said no, or there is no way to reach it any more.
    Refused(String),
}

/// What the lane does next after an attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fate {
    Done,
    /// Tried again after the wait; the number of the attempt that failed.
    Retry(Duration, usize),
    /// Given up after this many attempts.
    GaveUp(usize),
}

#[derive(Default)]
struct Lane {
    jobs: VecDeque<Change>,
    /// Whether an attempt at the head is under way.
    busy: bool,
    /// The failures in a row at the head.
    failures: usize,
    /// The head is not tried before this.
    retry_at: Option<Instant>,
}

/// The changes waiting to be told to each backend.
#[derive(Default)]
pub struct Outbox {
    lanes: Mutex<HashMap<String, Lane>>,
}

impl Outbox {
    fn lanes(&self) -> std::sync::MutexGuard<'_, HashMap<String, Lane>> {
        self.lanes.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Queues `change` for each of `backends`. Fails, with the reason, for a change that no
    /// server can take (a range of addresses); nothing is queued then.
    pub fn send(&self, backends: &[String], change: &Change) -> Result<(), String> {
        change.call()?;
        let mut lanes = self.lanes();
        for backend in backends {
            let lane = lanes.entry(backend.clone()).or_default();
            lane.jobs.push_back(change.clone());
            if lane.jobs.len() > LANE_LEN {
                // The head may be under way: it is the one that stays.
                let dropped = if lane.busy {
                    lane.jobs.remove(1)
                } else {
                    lane.jobs.pop_front()
                };
                if let Some(dropped) = dropped {
                    warning!(
                        "{} for backend \"{backend}\" is dropped: {LANE_LEN} changes wait for it",
                        dropped.describe()
                    );
                }
            }
        }
        Ok(())
    }

    /// The changes to try now: the head of each lane that has no attempt under way and is not
    /// waiting. Each is marked as under way until [`Outbox::finish`].
    pub fn take_ready(&self, now: Instant) -> Vec<(String, Change)> {
        let mut ready = Vec::new();
        for (backend, lane) in self.lanes().iter_mut() {
            if lane.busy || lane.retry_at.is_some_and(|at| now < at) {
                continue;
            }
            if let Some(change) = lane.jobs.front() {
                lane.busy = true;
                ready.push((backend.clone(), change.clone()));
            }
        }
        ready.sort_by(|a, b| a.0.cmp(&b.0));
        ready
    }

    /// Records the end of the attempt at the head of `backend`'s lane.
    pub fn finish(&self, backend: &str, outcome: &Outcome, now: Instant) -> Fate {
        let mut lanes = self.lanes();
        let Some(lane) = lanes.get_mut(backend) else {
            return Fate::Done;
        };
        lane.busy = false;
        match outcome {
            Outcome::Sent | Outcome::Refused(_) => {
                lane.jobs.pop_front();
                lane.failures = 0;
                lane.retry_at = None;
                Fate::Done
            }
            Outcome::Lost(_) => {
                lane.failures += 1;
                let failed = lane.failures;
                match RETRIES.get(failed - 1) {
                    Some(wait) => {
                        lane.retry_at = Some(now + *wait);
                        Fate::Retry(*wait, failed)
                    }
                    None => {
                        lane.jobs.pop_front();
                        lane.failures = 0;
                        lane.retry_at = None;
                        Fate::GaveUp(failed)
                    }
                }
            }
        }
    }

    /// The changes still to be told to `backend`.
    #[cfg(test)]
    pub fn waiting(&self, backend: &str) -> usize {
        self.lanes().get(backend).map_or(0, |lane| lane.jobs.len())
    }

    /// Forgets the lanes of backends that are no longer asked.
    pub fn retain(&self, mut keep: impl FnMut(&str) -> bool) {
        self.lanes().retain(|b, _| keep(b));
    }
}

/// One attempt: the call, on a connection of its own, and what became of it.
fn attempt(config: &Config, open: &Open, backend: &str, change: &Change) -> Outcome {
    let Some(sidecar) = config.sidecars.iter().find(|s| s.backend == backend) else {
        return Outcome::Refused("it has no sidecar any more".into());
    };
    let (method, params) = match change.call() {
        Ok(call) => call,
        Err(why) => return Outcome::Refused(why),
    };
    let result = open(sidecar).and_then(|mut manager| manager.call(method, params));
    match result {
        Ok(()) => Outcome::Sent,
        Err(Failure { lost: true, why }) => Outcome::Lost(why),
        Err(Failure { lost: false, why }) => Outcome::Refused(why),
    }
}

/// Tries what is ready, each attempt on a thread of its own so that a slow server holds up
/// no other; returns the threads (the proxy lets them go, tests wait for them).
pub fn start(
    config: &Arc<Config>,
    open: &Arc<Open>,
    outbox: &Arc<Outbox>,
    now: Instant,
) -> Vec<JoinHandle<()>> {
    outbox.retain(|b| config.sidecars.iter().any(|s| s.backend == b));
    outbox
        .take_ready(now)
        .into_iter()
        .map(|(backend, change)| {
            let (config, open, outbox) = (Arc::clone(config), Arc::clone(open), Arc::clone(outbox));
            let began = Instant::now();
            std::thread::spawn(move || {
                let outcome = attempt(&config, &*open, &backend, &change);
                // The wait counts from the end of the attempt, which may have been long.
                let fate = outbox.finish(&backend, &outcome, now + began.elapsed());
                report(&backend, &change, &outcome, fate);
            })
        })
        .collect()
}

/// Says what became of an attempt, when it is worth saying.
fn report(backend: &str, change: &Change, outcome: &Outcome, fate: Fate) {
    let what = change.describe();
    match (outcome, fate) {
        (Outcome::Sent, _) => info!("sent {what} to backend \"{backend}\""),
        (Outcome::Refused(why), _) => {
            warning!("backend \"{backend}\" refused {what}: {why}");
        }
        (Outcome::Lost(why), Fate::Retry(wait, 1)) => warning!(
            "cannot send {what} to backend \"{backend}\": {why}; trying again in {} seconds, \
             up to {} times",
            wait.as_secs(),
            RETRIES.len()
        ),
        (Outcome::Lost(_), Fate::Retry(..)) => {}
        (Outcome::Lost(why), Fate::GaveUp(tries)) => warning!(
            "gave up sending {what} to backend \"{backend}\" after {tries} tries: {why}; the \
             proxy's lists have it, the server's do not"
        ),
        (Outcome::Lost(_), Fate::Done) => {}
    }
}

/// Starts the thread that tells the servers, until the proxy stops. The configuration is
/// read again at each look, so a reload applies to the next one.
pub fn spawn(proxy: &Arc<crate::state::Proxy>) -> std::io::Result<JoinHandle<()>> {
    let proxy = Arc::clone(proxy);
    let open: Arc<Open> = Arc::new(open_sidecar);
    std::thread::Builder::new()
        .name("bans to servers".into())
        .spawn(move || {
            while !proxy.stopping() {
                let config = proxy.config();
                if config.access.sync.to_servers {
                    start(&config, &open, &proxy.access_sync, Instant::now());
                } else {
                    // Turned off by a reload: what waited is not sent.
                    proxy.access_sync.retain(|_| false);
                }
                std::thread::sleep(TICK);
            }
        })
}
