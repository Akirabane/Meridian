//! What the backends report of themselves (docs/LOAD-BALANCING.md, section 3): the
//! proxy asks the sidecar of each balanced backend for its state at the probe
//! interval, keeps the last answer with the time it came, and the balancer reads it
//! with a limit on its age. A reply is the heartbeat: a backend whose answer is
//! older than `state-max-age-seconds` is not believed.
//!
//! Nothing here is needed unless `[balancing] enabled` is on, and a backend that has
//! never answered is treated as accepting players, as it is without balancing.
use crate::extension::Registry;
use crate::extension::events::Kind;
use crate::log::{info, warning};
use crate::restart::Detector;
use crate::state::Proxy;
use meridian_config::{Config, Sidecar};
use meridian_extension::decide::{ServerReport, Why};
use meridian_extension::events::DownCause;
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
    /// Whether each backend answered at the last round: what the extensions are told a
    /// change of. A backend is in it from the first round that read it, which sets the
    /// state it starts from and is not a change.
    answering: HashMap<String, bool>,
    /// The state each backend was last seen in by the extensions (`servers/state`): set by the first round that reads
    /// it, which is not a change.
    told: HashMap<String, StateSeen>,
}

/// What `servers/state` tells of a backend, and a change of which is told.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateSeen {
    /// `open`, `closed`, `draining` or `not_offered`.
    pub state: &'static str,
    pub why: Option<Why>,
    /// What the backend said of its stop at its last reading that is believed.
    pub stopping: Option<bool>,
}

/// The longest age a report is given with: a reading older than that is `not_offered` long before.
const MAX_AGE_MS: u64 = 86_400_000;

/// A backend that did not answer after it did, or answers after it did not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub backend: String,
    /// What it does now.
    pub answers: bool,
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

    /// The last state `backend` reported, and when, however old.
    pub fn reading(&self, backend: &str) -> Option<(Instant, ServerStatus)> {
        self.lock().reported.get(backend).copied()
    }

    /// Whether `backend` answered at the last round that read it; `None` for one no round read.
    pub fn answering(&self, backend: &str) -> Option<bool> {
        self.lock().answering.get(backend).copied()
    }

    /// Forgets the state `servers/state` last saw of every backend: the next round that sees one sets where it starts from.
    pub fn forget_states(&self) {
        self.lock().told.clear();
    }

    /// `backend` is in state `now` at this round. Whether that is a change since the round before: the first round that
    /// sees a backend sets the state it starts from, and is not a change.
    pub fn saw_state(&self, backend: &str, now: StateSeen) -> bool {
        let before = self.lock().told.insert(backend.to_owned(), now.clone());
        before.is_some_and(|before| before != now)
    }

    /// An attempt to read `backend`'s state failed. `true` the first time in a row,
    /// which is when it is worth saying.
    fn failed(&self, backend: &str) -> bool {
        self.lock().failing.insert(backend.to_owned())
    }

    /// `backend` answered (or did not) at this round. The change, if that is **not** what it did at the
    /// last round: the first round that reads a backend sets the state it starts from, and is not a change.
    fn saw(&self, backend: &str, answers: bool) -> Option<Change> {
        let before = self.lock().answering.insert(backend.to_owned(), answers)?;
        (before != answers).then(|| Change {
            backend: backend.to_owned(),
            answers,
        })
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
        state.answering.retain(|b, _| keep(b));
        state.told.retain(|b, _| keep(b));
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
///
/// Gives back the backends whose state changed since the round before ([`tell`] tells it to the
/// extensions): one that answers is up, one that does not is down, and one that says it does not
/// know the request is alive, so it is up.
pub fn poll(
    config: &Config,
    ask: &(impl Fn(&Sidecar, Duration) -> Result<ServerStatus, String> + Sync),
    statuses: &Statuses,
) -> Vec<Change> {
    let timeout = Duration::from_millis(config.sidecar_timeout_ms);
    statuses.retain(|b| config.balanced(b).is_some());
    let changes = Mutex::new(Vec::new());
    let changed = &changes;
    std::thread::scope(|scope| {
        for sidecar in config
            .sidecars
            .iter()
            .filter(|s| config.balanced(&s.backend).is_some())
        {
            scope.spawn(move || {
                let result = ask(sidecar, timeout);
                // A backend that says it does not know the request is alive: it answers.
                let answers = match &result {
                    Ok(_) => true,
                    Err(why) => no_such_request(why),
                };
                if let Some(change) = statuses.saw(&sidecar.backend, answers) {
                    changed
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(change);
                }
                record(statuses, sidecar, result);
            });
        }
    });
    changes.into_inner().unwrap_or_else(|e| e.into_inner())
}

/// Keeps what `sidecar` answered: a state is recorded; a backend that does not know the request is alive but has
/// none; any other failure is said once.
fn record(statuses: &Statuses, sidecar: &Sidecar, result: Result<ServerStatus, String>) {
    match result {
        Ok(status) => {
            if statuses.recovered(&sidecar.backend) {
                info!("backend \"{}\" reports its state again", sidecar.backend);
            }
            statuses.record(&sidecar.backend, status, Instant::now());
        }
        // The backend answered, with "unknown request": a Horizon build from before
        // `transfer-status` has no state to report. It is alive; it is treated as
        // accepting players.
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
    }
}

/// Tells the extensions that listen which backends changed state: `servers/up` for one that answers
/// again and `servers/down` for one that stopped answering, with the cause `restart` if the detector
/// already has it going down for a restart (usually because it announced its stop: at the first round
/// without an answer the detector has not yet counted `silent-rounds` of them, so a server that is only
/// silent is `unreachable`; the silence counts only for an episode that is already open), and
/// `unreachable` if not.
pub fn tell(extensions: &Registry, detector: &Detector, changes: Vec<Change>) {
    for change in changes {
        extensions.emit(|| {
            if change.answers {
                Kind::ServerUp {
                    server: change.backend,
                }
            } else {
                let restarting = detector.is_restarting(&change.backend);
                Kind::ServerDown {
                    server: change.backend,
                    cause: Some(if restarting {
                        DownCause::Restart
                    } else {
                        DownCause::Unreachable
                    }),
                }
            }
        });
    }
}

/// What `backend` last said of itself, as an extension is told it (`report` of `servers/list` and of the candidates of
/// `decide/server`), with its age at `now`. `None` for a backend no round read, or that never answered.
pub fn report_of(statuses: &Statuses, backend: &str, now: Instant) -> Option<ServerReport> {
    let (at, status) = statuses.reading(backend)?;
    let age = u64::try_from(now.saturating_duration_since(at).as_millis()).unwrap_or(u64::MAX);
    Some(ServerReport {
        players: status.players,
        capacity: status.capacity,
        held: status.held,
        expected: status.expected,
        frozen: status.frozen,
        full: status.full,
        stopping: status.stopping,
        age_ms: age.min(MAX_AGE_MS),
    })
}

/// After a round: the backends with a role whose state for new players changed since the round before (`state`, `why`,
/// or what they said of their stop), as `servers/state` tells it. One at most for each backend: a round is read once.
/// `on` gives the players the sessions count on a backend.
pub fn state_changes(
    config: &Config,
    shared: &crate::session::Shared,
    on: impl Fn(&str) -> usize,
    now: Instant,
) -> Vec<Kind> {
    let max_age = Duration::from_secs(config.balancing.state_max_age_seconds);
    config
        .balanced
        .iter()
        .filter_map(|b| {
            let restarting = shared.restarting(&b.backend);
            let standing = crate::placing::standing(
                config,
                shared,
                &b.backend,
                on(&b.backend),
                restarting.as_deref(),
            )?;
            // Only a report that is believed says what the backend does: an old `stopping` is not news.
            let fresh = match shared.statuses.report(&b.backend, max_age, now) {
                Report::Fresh(status) => Some(status),
                Report::Unknown | Report::Stale => None,
            };
            let told = StateSeen {
                state: standing.offer.state(),
                why: standing.offer.why(),
                stopping: fresh.and_then(|s| s.stopping),
            };
            shared
                .statuses
                .saw_state(&b.backend, told.clone())
                .then(|| Kind::ServerState {
                    server: b.backend.clone(),
                    state: told.state.to_owned(),
                    why: told.why,
                    players: u64::try_from(standing.players).ok(),
                    capacity: fresh.and_then(|s| s.capacity),
                    stopping: told.stopping,
                })
        })
        .collect()
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
                    let changes = poll(&config, &ask_sidecar, &proxy.sessions.statuses);
                    crate::restart::observe_round(
                        &config,
                        &proxy.sessions.statuses,
                        &proxy.restarts,
                        Instant::now(),
                    );
                    // After the detector read the round: a stop it announced or deduced is the cause.
                    tell(&proxy.sessions.extensions, &proxy.restarts, changes);
                    // Then what changed of each backend's state for new players, once for this round.
                    let shared = proxy.sessions.shared();
                    let counts = shared.counts();
                    let on = |backend: &str| counts.get(backend).copied().unwrap_or(0);
                    for kind in state_changes(&config, &shared, on, Instant::now()) {
                        proxy.sessions.extensions.emit(move || kind);
                    }
                    if config.balancing.tie_break == meridian_config::TieBreak::Ping {
                        measure(&config, &*proxy.probe, &proxy.sessions.statuses);
                    }
                    proxy.sessions.metrics().status_round(round.elapsed());
                } else {
                    // No rounds: what `servers/state` last saw is forgotten, so that the first round after balancing is on
                    // again sets where each server starts from, and tells nothing.
                    proxy.sessions.statuses.forget_states();
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
