//! Telling that a backend is about to restart (docs/RESTART.md). A backend that goes
//! down takes its players with it, so the moment to move them is before it goes, not
//! when their connection breaks.
//!
//! Three things can say it: the backend announcing its own stop (Horizon's
//! `server/stopping` notification of the management protocol), the panel that
//! restarts it, and, as a last resort that works with nothing else, several rounds in
//! a row in which its sidecar does not answer. Each of them ends in [`Detector::announce`]:
//! the backend is marked draining, as the console's `drain` does, and the players are
//! handed to an [`Evacuate`], which moves them to the lobby or to another game server.
//!
//! The marking is undone when the backend is back, but only if the detector made it:
//! a backend the operator had drained, or drains while it restarts, stays drained.
//!
//! The evacuation is [`Evacuator`]: it moves the players one at a time with the logic of
//! `drain --move`, and only when `[restart] evacuate` is on.
use crate::balance::Balancer;
use crate::evacuate::Outcome;
use crate::log::{info, warning};
use crate::session::Sessions;
use crate::status::Statuses;
use meridian_config::Config;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::{Duration, Instant};

/// How the proxy came to know that a backend is going down.
// `Panel` is not sent by anything yet (docs/RESTART.md).
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    /// The backend said so itself: `server/stopping`.
    Announced,
    /// The panel that supervises it is restarting it.
    Panel,
    /// Its sidecar did not answer for several rounds in a row.
    Unreachable,
}

impl Cause {
    fn describe(self) -> &'static str {
        match self {
            Cause::Announced => "it announced its stop",
            Cause::Panel => "the panel is restarting it",
            Cause::Unreachable => "it stopped answering",
        }
    }
}

/// What moves the players off a backend that is going down. Called once for each
/// restart, with the backend already draining, so the players that are sent away
/// cannot be sent back to it.
pub trait Evacuate: Send + Sync {
    fn evacuate(&self, backend: &str, cause: Cause);
}

/// What the detector waits for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    /// Rounds without an answer from the sidecar before the backend counts as going
    /// down.
    pub misses: u32,
    /// How long a backend that was told to stop may go on answering before it counts
    /// as back. It answers for a while while it shuts down, so an answer straight
    /// after the notice does not mean that it did not stop.
    pub window: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Self::from(&meridian_config::Restart::default())
    }
}

impl From<&meridian_config::Restart> for Settings {
    fn from(restart: &meridian_config::Restart) -> Self {
        Self {
            misses: restart.silent_rounds,
            window: Duration::from_secs(restart.grace_seconds),
        }
    }
}

/// A backend that is going down, from the notice to its return.
struct Episode {
    cause: Cause,
    since: Instant,
    /// It stopped answering after the notice: it did go down.
    missed: bool,
    /// The detector drained it, so the detector undrains it. `false` when the
    /// operator had drained it already.
    ours: bool,
}

#[derive(Default)]
struct State {
    episodes: HashMap<String, Episode>,
    /// The rounds in a row each backend did not answer.
    misses: HashMap<String, u32>,
}

/// Which backends are going down, and what is done when one is.
pub struct Detector {
    balancer: Arc<Balancer>,
    evacuate: Arc<dyn Evacuate>,
    settings: Mutex<Settings>,
    state: Mutex<State>,
}

impl Detector {
    pub fn new(balancer: Arc<Balancer>, evacuate: Arc<dyn Evacuate>, settings: Settings) -> Self {
        Self {
            balancer,
            evacuate,
            settings: Mutex::new(settings),
            state: Mutex::default(),
        }
    }

    /// Puts `settings` in force from the next round, as a reload of the configuration does.
    pub fn configure(&self, settings: Settings) {
        *self.settings.lock().unwrap_or_else(|e| e.into_inner()) = settings;
    }

    fn settings(&self) -> Settings {
        *self.settings.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `backend` is going down, as far as `cause` shows. It stops being offered to new
    /// players and its players are handed to the evacuation. `true` the first time for
    /// a restart; a second notice for the same restart does nothing.
    pub fn announce(&self, backend: &str, cause: Cause, now: Instant) -> bool {
        let mut state = self.lock();
        if state.episodes.contains_key(backend) {
            return false;
        }
        let ours = self.balancer.drain(backend);
        state.episodes.insert(
            backend.to_owned(),
            Episode {
                cause,
                since: now,
                // It was marked for not answering: it already went quiet.
                missed: cause == Cause::Unreachable,
                ours,
            },
        );
        drop(state);
        warning!(
            "backend \"{backend}\" is going down ({}): it takes no new player",
            cause.describe()
        );
        self.evacuate.evacuate(backend, cause);
        true
    }

    /// A message of the backend's management protocol, one JSON-RPC line. `server/stopping` is
    /// [`Detector::announce`]; `server/started` ends the restart. Anything else is not
    /// for the detector, and `false`.
    pub fn handle_notification(&self, backend: &str, line: &str, now: Instant) -> bool {
        match notice(line) {
            Some(Notice::Stopping) => {
                self.announce(backend, Cause::Announced, now);
                true
            }
            Some(Notice::Started) => {
                self.back(backend);
                true
            }
            None => false,
        }
    }

    /// The result of one round of asking `backend`'s sidecar: whether it answered.
    /// Several rounds without an answer make it going down; an answer after that, or an
    /// answer long enough after the notice, makes it back.
    pub fn observe(&self, backend: &str, answered: bool, now: Instant) {
        let settings = self.settings();
        let mut state = self.lock();
        if answered {
            state.misses.remove(backend);
            let over = state.episodes.get(backend).is_some_and(|e| {
                e.missed || now.saturating_duration_since(e.since) >= settings.window
            });
            drop(state);
            if over {
                self.back(backend);
            }
            return;
        }
        let misses = {
            let count = state.misses.entry(backend.to_owned()).or_insert(0);
            *count = count.saturating_add(1);
            *count
        };
        if let Some(episode) = state.episodes.get_mut(backend) {
            episode.missed = true;
            return;
        }
        drop(state);
        if misses >= settings.misses {
            self.announce(backend, Cause::Unreachable, now);
        }
    }

    /// `backend` is up again: it is offered to players again if the detector was the one
    /// that stopped offering it.
    pub fn back(&self, backend: &str) {
        let Some(episode) = self.lock().episodes.remove(backend) else {
            return;
        };
        if episode.ours {
            self.balancer.undrain(backend);
        }
        info!("backend \"{backend}\" is back");
    }

    /// Whether `backend` is going down now.
    pub fn is_restarting(&self, backend: &str) -> bool {
        self.lock().episodes.contains_key(backend)
    }

    /// For the console's list of backends: why `backend` is going down, if it is.
    pub fn describe(&self, backend: &str) -> Option<String> {
        self.lock()
            .episodes
            .get(backend)
            .map(|e| e.cause.describe().to_owned())
    }

    /// The operator drained `backend`: it stays drained when the backend is back, even if
    /// the detector had drained it first.
    pub fn keep_drained(&self, backend: &str) {
        if let Some(episode) = self.lock().episodes.get_mut(backend) {
            episode.ours = false;
        }
    }

    /// Forgets the backends `keep` refuses, such as one a reload removed.
    pub fn retain(&self, mut keep: impl FnMut(&str) -> bool) {
        let mut state = self.lock();
        state.episodes.retain(|b, _| keep(b));
        state.misses.retain(|b, _| keep(b));
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// What a notification of the management protocol says of the server's life.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notice {
    /// `server/stopping`: the shutdown began.
    Stopping,
    /// `server/started`: the game is up.
    Started,
}

/// The notice in one JSON-RPC message of the management protocol, if it is one. The
/// method is `minecraft:notification/server/stopping` there; the prefix is optional here.
pub fn notice(line: &str) -> Option<Notice> {
    let message: serde_json::Value = serde_json::from_str(line).ok()?;
    let method = message.get("method")?.as_str()?;
    let method = method.strip_prefix("minecraft:").unwrap_or(method);
    match method {
        "notification/server/stopping" => Some(Notice::Stopping),
        "notification/server/started" => Some(Notice::Started),
        _ => None,
    }
}

/// How the players of a backend are sent away when the evacuation is on:
/// [`crate::evacuate::send_away`] in a running proxy, a stand-in in tests.
type Away = dyn Fn(&Sessions, &Config, &str) -> Option<Outcome> + Send + Sync;

/// The evacuation of a running proxy: with `[restart] evacuate` on, the players of a
/// backend that is going down are sent to other servers of its role, one at a time, as
/// `drain <backend> --move` sends them. With it off, which is the default, they stay and
/// the backend is only marked draining.
///
/// The moves take a pause each, so they run on a thread of their own and do not hold the
/// poller. A player with nowhere to go stays on the backend and is named in the log.
pub struct Evacuator {
    sessions: Arc<Sessions>,
    config: Arc<RwLock<Arc<Config>>>,
    away: Arc<Away>,
}

impl Evacuator {
    pub fn new(sessions: Arc<Sessions>, config: Arc<RwLock<Arc<Config>>>) -> Self {
        Self::with(sessions, config, Arc::new(crate::evacuate::send_away))
    }

    fn with(sessions: Arc<Sessions>, config: Arc<RwLock<Arc<Config>>>, away: Arc<Away>) -> Self {
        Self {
            sessions,
            config,
            away,
        }
    }

    fn config(&self) -> Arc<Config> {
        Arc::clone(&self.config.read().unwrap_or_else(|e| e.into_inner()))
    }

    /// The evacuation, on this thread: `None` when it is off or nobody is on `backend`.
    fn run(&self, backend: &str, cause: Cause) -> Option<Outcome> {
        let config = self.config();
        if !config.restart.evacuate {
            info!(
                "backend \"{backend}\" is going down ({}): its players stay on it ([restart] evacuate is off)",
                cause.describe()
            );
            return None;
        }
        let Some(outcome) = (self.away)(&self.sessions, &config, backend) else {
            info!("backend \"{backend}\" is going down: nobody is on it");
            return None;
        };
        info!("backend \"{backend}\" is going down: {}", outcome.counts());
        for (name, why) in &outcome.stayed {
            warning!("{name} stays on {backend}: {why}");
        }
        for line in outcome.already_lines() {
            info!("{line}");
        }
        Some(outcome)
    }
}

impl Evacuate for Evacuator {
    fn evacuate(&self, backend: &str, cause: Cause) {
        if !self.config().restart.evacuate {
            self.run(backend, cause);
            return;
        }
        // The detector holds this as an `Arc<dyn Evacuate>` and the thread outlives the
        // call: it takes the same handles.
        let evacuation = Evacuator::with(
            Arc::clone(&self.sessions),
            Arc::clone(&self.config),
            Arc::clone(&self.away),
        );
        let backend = backend.to_owned();
        let spawned = std::thread::Builder::new()
            .name("restart evacuation".into())
            .spawn(move || {
                evacuation.run(&backend, cause);
            });
        if let Err(e) = spawned {
            warning!("cannot start the evacuation: {e}");
        }
    }
}

/// One round of the sidecars' states, as the poller left them: every backend with a
/// role either answered or is failing, and the detector is told which.
pub fn observe_round(config: &Config, statuses: &Statuses, detector: &Detector, now: Instant) {
    detector.configure(Settings::from(&config.restart));
    detector.retain(|b| config.balanced(b).is_some());
    for sidecar in &config.sidecars {
        if config.balanced(&sidecar.backend).is_some() {
            detector.observe(&sidecar.backend, !statuses.failing(&sidecar.backend), now);
        }
    }
}

#[cfg(test)]
mod tests;
