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
//! a backend the operator had drained stays drained.
use crate::balance::Balancer;
use crate::log::{info, warning};
use meridian_config::Config;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::status::Statuses;

/// How the proxy came to know that a backend is going down.
// Nothing sends this yet: the sidecar or the panel will (docs/RESTART.md).
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

/// The evacuation until the players can be moved: it says what would be moved. The
/// players stay on the backend, as they do when the operator drains it.
pub struct LogOnly;

impl Evacuate for LogOnly {
    fn evacuate(&self, backend: &str, cause: Cause) {
        info!(
            "backend \"{backend}\" is going down ({}): its players stay on it, nothing moves them yet",
            cause.describe()
        );
    }
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
        Self {
            misses: 3,
            window: Duration::from_secs(60),
        }
    }
}

/// A backend that is going down, from the notice to its return.
struct Episode {
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
    settings: Settings,
    state: Mutex<State>,
}

impl Detector {
    pub fn new(balancer: Arc<Balancer>, evacuate: Arc<dyn Evacuate>, settings: Settings) -> Self {
        Self {
            balancer,
            evacuate,
            settings,
            state: Mutex::default(),
        }
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
    #[cfg_attr(not(test), allow(dead_code))]
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
        let mut state = self.lock();
        if answered {
            state.misses.remove(backend);
            let over = state.episodes.get(backend).is_some_and(|e| {
                e.missed || now.saturating_duration_since(e.since) >= self.settings.window
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
        if misses >= self.settings.misses {
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
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn is_restarting(&self, backend: &str) -> bool {
        self.lock().episodes.contains_key(backend)
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
// Nothing sends this yet: the sidecar or the panel will (docs/RESTART.md).
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notice {
    /// `server/stopping`: the shutdown began.
    Stopping,
    /// `server/started`: the game is up.
    Started,
}

/// The notice in one JSON-RPC message of the management protocol, if it is one. The
/// method is `minecraft:notification/server/stopping` there; the prefix is optional here.
#[cfg_attr(not(test), allow(dead_code))]
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

/// One round of the sidecars' states, as the poller left them: every backend with a
/// role either answered or is failing, and the detector is told which.
pub fn observe_round(config: &Config, statuses: &Statuses, detector: &Detector, now: Instant) {
    detector.retain(|b| config.balanced(b).is_some());
    for sidecar in &config.sidecars {
        if config.balanced(&sidecar.backend).is_some() {
            detector.observe(&sidecar.backend, !statuses.failing(&sidecar.backend), now);
        }
    }
}

#[cfg(test)]
mod tests;
