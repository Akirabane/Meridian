//! What the orders that act on the players use (docs/EXTENSIONS.md, section 5): the proxy's own sessions and
//! `staff::alert`, the function that the console's `alert` and the staff's `/alert` call. A line is shown as a literal
//! (`SessionHandle::tell`), never as a JSON text component, so an extension cannot put a click event in a chat.

use super::link::SendLot;
use super::orders::{Actions, MovePlan, Paid, Refusal};
use crate::log::warning;
use crate::placing::{self, Offer};
use crate::session::{Player, Sessions, Shared, hyphenated};
use crate::staff;
use crate::switching::Switcher;
use meridian_config::Config;
use meridian_extension::orders::{Target, Who};
use std::sync::{Arc, RwLock, Weak};

/// The proxy's own actions. It holds the sessions weakly, as [`super::facts::ProxyFacts`] does.
pub(crate) struct ProxyActions {
    sessions: Weak<Sessions>,
    config: Arc<RwLock<Arc<Config>>>,
}

impl ProxyActions {
    pub(crate) fn new(sessions: &Arc<Sessions>, config: &Arc<RwLock<Arc<Config>>>) -> Self {
        Self {
            sessions: Arc::downgrade(sessions),
            config: Arc::clone(config),
        }
    }

    /// The configuration in force now.
    fn config(&self) -> Arc<Config> {
        Arc::clone(&self.config.read().unwrap_or_else(|e| e.into_inner()))
    }
}

impl Actions for ProxyActions {
    fn tell(&self, who: &Who, text: &str) -> usize {
        // 1 when the player was shown the line, 0 when nobody is online by that name or their session reads no chat yet.
        let Some(sessions) = self.sessions.upgrade() else {
            return 0;
        };
        let handle = match who {
            Who::Name(name) => sessions.session(name).map(|(_, handle)| handle),
            Who::Id(id) => sessions
                .shared()
                .sessions()
                .into_iter()
                .find(|(player, _)| hyphenated(player.uuid).eq_ignore_ascii_case(id))
                .map(|(_, handle)| handle),
        };
        // Shown, not only found: a session that is not in the play phase reads no chat.
        usize::from(handle.is_some_and(|handle| handle.try_tell(text)))
    }

    fn audience(&self, server: Option<&str>) -> Result<usize, String> {
        let Some(sessions) = self.sessions.upgrade() else {
            return Ok(0);
        };
        let players = sessions.players();
        match server {
            None => Ok(players.len()),
            Some(name) => {
                let config = self.config();
                let backend = config
                    .backend(name)
                    .ok_or_else(|| "there is no such server".to_owned())?;
                Ok(players.iter().filter(|p| p.backend == backend.name).count())
            }
        }
    }

    fn broadcast(&self, server: Option<&str>, text: &str) -> Result<usize, String> {
        let Some(sessions) = self.sessions.upgrade() else {
            return Ok(0);
        };
        staff::alert_shown(&sessions.shared(), &self.config(), server, text)
    }

    fn kick(&self, who: &Who, reason: &str) -> Option<String> {
        let sessions = self.sessions.upgrade()?;
        let kicked = match who {
            Who::Name(name) => sessions.kick(name, reason),
            Who::Id(id) => sessions.kick_matching(reason, |player| {
                hyphenated(player.uuid).eq_ignore_ascii_case(id)
            }),
        };
        kicked.map(|player| player.name)
    }

    fn plan_send(&self, target: &Target, to: &str) -> Result<Box<dyn MovePlan>, Refusal> {
        let config = self.config();
        let backend = config.backend(to).ok_or(Refusal::UnknownDestination)?;
        let Some(sessions) = self.sessions.upgrade() else {
            return Ok(Box::new(Planned::nobody()));
        };
        let shared = sessions.shared();
        let all = shared.players();
        // A server that takes no players now (draining, silent, or off transfers) is refused at once: sending to it would tell
        // each player that they could not be moved. A server with no role is not known to the balancer, and is taken.
        let on = all.iter().filter(|p| p.backend == backend.name).count();
        if let Some(standing) = placing::standing(&config, &shared, &backend.name, on, None)
            && !matches!(standing.offer, Offer::Open | Offer::Closed)
        {
            return Err(Refusal::DestinationNotTakingPlayers);
        }
        let players: Vec<Player> = match target {
            Target::All => all,
            Target::Server(name) => {
                let source = config.backend(name).ok_or(Refusal::UnknownSource)?;
                all.into_iter()
                    .filter(|p| p.backend == source.name)
                    .collect()
            }
            Target::Player(Who::Name(name)) => all
                .into_iter()
                .filter(|p| p.name.eq_ignore_ascii_case(name))
                .take(1)
                .collect(),
            Target::Player(Who::Id(id)) => all
                .into_iter()
                .filter(|p| hyphenated(p.uuid).eq_ignore_ascii_case(id))
                .take(1)
                .collect(),
        };
        // Those who are on the server already are not moved (as the console's and the staff's `send` do).
        let moving: Vec<Player> = players
            .into_iter()
            .filter(|p| p.backend != backend.name)
            .collect();
        Ok(Box::new(Planned {
            moving,
            destination: backend.name.clone(),
            run: Some((config, shared, Arc::clone(&sessions.switcher))),
        }))
    }
}

/// Players to move to a server: started on a thread of their own, as the staff's `/send` does, so that the order answers
/// when the moves start and not when they end (a move waits for the servers).
struct Planned {
    moving: Vec<Player>,
    destination: String,
    /// What the moves need; `None` for a plan with nobody to move.
    run: Option<(Arc<Config>, Shared, Arc<Switcher>)>,
}

impl Planned {
    fn nobody() -> Self {
        Self {
            moving: Vec::new(),
            destination: String::new(),
            run: None,
        }
    }
}

impl MovePlan for Planned {
    fn count(&self) -> usize {
        self.moving.len()
    }

    fn start(self: Box<Self>, _paid: Paid, lot: SendLot) -> Result<usize, Refusal> {
        let Planned {
            moving,
            destination,
            run,
        } = *self;
        let started = moving.len();
        let Some((config, shared, switcher)) = run.filter(|_| started > 0) else {
            return Ok(0);
        };
        std::thread::Builder::new()
            .name("extension-send".into())
            .spawn(move || {
                // The extension's place for a send is held until every move is done, and let go however this ends.
                let _lot = lot;
                let (_, failed) =
                    staff::move_all(&config, &shared, &switcher, &moving, &destination);
                if failed > 0 {
                    warning!("an extension sent players to {destination}: {failed} failed");
                }
            })
            .map(|_| started)
            .map_err(|e| {
                warning!("cannot start a thread to send players: {e}");
                Refusal::CannotStart
            })
    }
}
