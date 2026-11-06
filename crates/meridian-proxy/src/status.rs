//! What the backends report of themselves (docs/LOAD-BALANCING.md, section 3): the
//! proxy asks the sidecar of each balanced backend for its state at the probe
//! interval, keeps the last answer with the time it came, and the balancer reads it
//! with a limit on its age. A reply is the heartbeat: a backend whose answer is
//! older than `state-max-age-seconds` is not believed.
//!
//! Nothing here is needed unless `[balancing] enabled` is on, and a backend that has
//! never answered is treated as accepting players, as it is without balancing.
use crate::log::{info, warning};
use crate::state::Proxy;
use meridian_config::{Config, Sidecar};
use meridian_sidecar::{client::SidecarClient, request::ServerStatus};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// What is known of a backend's state now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Report {
    /// It never answered: nothing is known, and it is treated as accepting players.
    Unknown,
    /// It answered no longer ago than the limit.
    Fresh(ServerStatus),
    /// It answered once, but longer ago than the limit: it is not believed, and the
    /// backend is not offered to new players until it answers again.
    Stale,
}

/// The last state each backend reported, and when.
#[derive(Default)]
pub struct Statuses {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    reported: HashMap<String, (Instant, ServerStatus)>,
    /// How long a connection to each backend took the last time it was measured, and
    /// when.
    pings: HashMap<String, (Instant, Duration)>,
    /// The backends whose last attempt failed, so that a failure is logged once.
    failing: HashSet<String>,
    /// The backends that answered that they do not know the request, so that it is said
    /// once.
    unreported: HashSet<String>,
}

impl Statuses {
    /// A connection to `backend` took `took` at `at`.
    pub fn record_ping(&self, backend: &str, took: Duration, at: Instant) {
        self.lock().pings.insert(backend.to_owned(), (at, took));
    }

    /// How long a connection to `backend` took, if it was measured no longer ago than
    /// `max_age`. A backend that stops answering is not measured, so its last measure
    /// grows old and is not used.
    pub fn ping(&self, backend: &str, max_age: Duration, now: Instant) -> Option<Duration> {
        let (at, took) = *self.lock().pings.get(backend)?;
        (now.saturating_duration_since(at) <= max_age).then_some(took)
    }

    /// `backend` reported `status` at `at`.
    pub fn record(&self, backend: &str, status: ServerStatus, at: Instant) {
        let mut state = self.lock();
        state.reported.insert(backend.to_owned(), (at, status));
        state.failing.remove(backend);
        state.unreported.remove(backend);
    }

    /// `backend` answered that it does not know the state request: it is alive but has
    /// no state to give, so it is not failing. `true` the first time, which is when it is
    /// worth saying.
    fn unreported(&self, backend: &str) -> bool {
        let mut state = self.lock();
        state.failing.remove(backend);
        state.unreported.insert(backend.to_owned())
    }

    /// What is known of `backend` at `now`, believing a report for `max_age`.
    pub fn report(&self, backend: &str, max_age: Duration, now: Instant) -> Report {
        match self.lock().reported.get(backend) {
            None => Report::Unknown,
            Some((at, status)) if now.saturating_duration_since(*at) <= max_age => {
                Report::Fresh(*status)
            }
            Some(_) => Report::Stale,
        }
    }

    /// An attempt to read `backend`'s state failed. `true` the first time in a row,
    /// which is when it is worth saying.
    fn failed(&self, backend: &str) -> bool {
        self.lock().failing.insert(backend.to_owned())
    }

    /// Whether `backend` was failing before this success, which is worth saying.
    fn recovered(&self, backend: &str) -> bool {
        self.failing(backend)
    }

    /// Whether the last attempt to read `backend`'s state failed.
    pub fn failing(&self, backend: &str) -> bool {
        self.lock().failing.contains(backend)
    }

    /// Forgets the backends `keep` refuses, such as one a reload removed.
    pub fn retain(&self, mut keep: impl FnMut(&str) -> bool) {
        let mut state = self.lock();
        state.reported.retain(|b, _| keep(b));
        state.pings.retain(|b, _| keep(b));
        state.failing.retain(|b| keep(b));
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Asks the sidecar for the backend's state over the signed exchange.
fn ask_sidecar(sidecar: &Sidecar, timeout: Duration) -> Result<ServerStatus, String> {
    let secret = sidecar.read_secret()?;
    let mut client = SidecarClient::connect(&sidecar.address, &secret, timeout)
        .map_err(|e| format!("cannot reach it at {}: {e}", sidecar.address))?;
    client.status().map_err(|e| e.to_string())
}

/// One round: every sidecar of a backend that has a role is asked at once, each with
/// its own timeout, so that one that hangs does not hold the others back. An answer
/// is recorded; a failure keeps the last answer, which then grows old.
pub fn poll(
    config: &Config,
    ask: &(impl Fn(&Sidecar, Duration) -> Result<ServerStatus, String> + Sync),
    statuses: &Statuses,
) {
    let timeout = Duration::from_millis(config.sidecar_timeout_ms);
    statuses.retain(|b| config.balanced(b).is_some());
    std::thread::scope(|scope| {
        for sidecar in config
            .sidecars
            .iter()
            .filter(|s| config.balanced(&s.backend).is_some())
        {
            scope.spawn(move || match ask(sidecar, timeout) {
                Ok(status) => {
                    if statuses.recovered(&sidecar.backend) {
                        info!("backend \"{}\" reports its state again", sidecar.backend);
                    }
                    statuses.record(&sidecar.backend, status, Instant::now());
                }
                // The backend answered, with "unknown request": the Horizon server has no
                // state to report yet. It is alive; it is treated as accepting players.
                Err(why) if no_such_request(&why) => {
                    if statuses.unreported(&sidecar.backend) {
                        warning!(
                            "backend \"{}\" does not answer the state request: its state is unknown, so it is treated as accepting players",
                            sidecar.backend
                        );
                    }
                }
                Err(why) => {
                    if statuses.failed(&sidecar.backend) {
                        warning!(
                            "cannot read the state of backend \"{}\": {why}",
                            sidecar.backend
                        );
                    }
                }
            });
        }
    });
}

/// Whether `why`, the error of a state request, is the backend saying it does not know
/// the request: that is an answer, not a silence.
fn no_such_request(why: &str) -> bool {
    why.starts_with("the sidecar refused: unknown request")
}

/// How a connection to a backend is timed: [`crate::state::Probe`].
pub type Probe<'a> = &'a (dyn Fn(&str, Duration) -> Result<Duration, String> + Sync);

/// One round of measures: a connection is opened to every backend that has a role,
/// all at once, and the time it took is kept. Only needed for `tie-break = "ping"`.
/// A backend that does not answer is not measured, and its last measure grows old.
pub fn measure(config: &Config, probe: Probe<'_>, statuses: &Statuses) {
    let timeout = Duration::from_millis(config.connect_timeout_ms);
    std::thread::scope(|scope| {
        for server in &config.balanced {
            let Some(backend) = config.backend(&server.backend) else {
                continue;
            };
            scope.spawn(move || {
                if let Ok(took) = probe(&backend.address, timeout) {
                    statuses.record_ping(&backend.name, took, Instant::now());
                }
            });
        }
    });
}

/// How often the stop flag is looked at while waiting for the next round.
const TICK: Duration = Duration::from_millis(100);

/// Starts the thread that polls the sidecars every `probe-interval-seconds` while
/// `[balancing] enabled` is on, until the proxy stops. The configuration is read
/// again for each round, so a reload applies to the next one.
///
/// The interval runs from the start of a round, not from its end: a round held by a sidecar
/// that does not answer would otherwise put its timeout between two readings of every
/// backend, on top of the interval. A round that lasts longer is followed at once by the next.
pub fn spawn(proxy: &Arc<Proxy>) -> std::io::Result<std::thread::JoinHandle<()>> {
    let proxy = Arc::clone(proxy);
    std::thread::Builder::new()
        .name("backend states".into())
        .spawn(move || {
            while !proxy.stopping() {
                let round = Instant::now();
                let config = proxy.config();
                if config.balancing.enabled {
                    poll(&config, &ask_sidecar, &proxy.sessions.statuses);
                    crate::restart::observe_round(
                        &config,
                        &proxy.sessions.statuses,
                        &proxy.restarts,
                        Instant::now(),
                    );
                    if config.balancing.tie_break == meridian_config::TieBreak::Ping {
                        measure(&config, &*proxy.probe, &proxy.sessions.statuses);
                    }
                    proxy.sessions.metrics().status_round(round.elapsed());
                }
                let wait = Duration::from_secs(config.balancing.probe_interval_seconds);
                let until = round + wait;
                while !proxy.stopping() && Instant::now() < until {
                    std::thread::sleep(TICK);
                }
            }
        })
}

#[cfg(test)]
mod tests;
