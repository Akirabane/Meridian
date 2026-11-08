//! The facts the orders that read tell an extension (docs/EXTENSIONS.md, section 5), taken from the proxy's own state
//! when an order asks: the sessions that are relayed, the configuration in force, what the balancer makes of each
//! server and the queue. They are the facts the console's `list`, `servers` and `queue` print, as data and not as text.
//!
//! **No address of a server is told** (the extension is told what the specification lists: the servers, their roles,
//! caps, counts and states). A player's address is given only to a caller that says (`with_addresses`) the extension
//! was granted `addresses`.

use super::orders::Facts;
use crate::placing;
use crate::session::{Sessions, hyphenated};
use meridian_config::Config;
use meridian_extension::decide::Auth;
use meridian_extension::orders::{MAX_PAGE, PlayerInfo, QueueState, ServerInfo, Waiting};
use std::sync::{Arc, RwLock, Weak};

/// The proxy's own facts. It holds the sessions weakly: the registry that serves the orders is held by the sessions, and
/// a hold both ways would keep both for good.
pub(crate) struct ProxyFacts {
    sessions: Weak<Sessions>,
    config: Arc<RwLock<Arc<Config>>>,
}

impl ProxyFacts {
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

impl Facts for ProxyFacts {
    fn players(&self, with_addresses: bool) -> Vec<PlayerInfo> {
        let Some(sessions) = self.sessions.upgrade() else {
            return Vec::new();
        };
        sessions
            .listing()
            .into_iter()
            .map(|(player, verified)| PlayerInfo {
                name: player.name,
                uuid: hyphenated(player.uuid).to_ascii_lowercase(),
                auth: if verified {
                    Auth::Verified
                } else {
                    Auth::Unverified
                },
                server: player.backend,
                seconds: player.joined.elapsed().as_secs(),
                address: with_addresses.then(|| player.address.ip().to_string()),
            })
            .collect()
    }

    fn verified(&self, uuid: [u8; 16]) -> bool {
        self.sessions.upgrade().is_some_and(|sessions| {
            sessions.is_connected(&crate::session::Identity::Verified(uuid))
        })
    }

    fn servers(&self) -> Vec<ServerInfo> {
        let Some(sessions) = self.sessions.upgrade() else {
            return Vec::new();
        };
        let config = self.config();
        let players = sessions.players();
        let shared = sessions.shared();
        config
            .backends
            .iter()
            .map(|backend| {
                let on = players.iter().filter(|p| p.backend == backend.name).count();
                // `None` for a server with no role. The detector's reason for a restart is not asked: it only tells
                // `draining` from `draining`.
                let standing = placing::standing(&config, &shared, &backend.name, on, None);
                ServerInfo {
                    name: backend.name.clone(),
                    default: backend.name == config.default_backend,
                    players: on,
                    role: standing.as_ref().map(|s| s.role.name().to_owned()),
                    soft_cap: standing.as_ref().map(|s| s.soft_cap),
                    hard_cap: standing.as_ref().map(|s| s.hard_cap),
                    state: standing.as_ref().map(|s| s.offer.state().to_owned()),
                    ping_ms: standing
                        .as_ref()
                        .and_then(|s| s.ping)
                        .map(|p| u64::try_from(p.as_millis()).unwrap_or(u64::MAX)),
                }
            })
            .collect()
    }

    fn queue(&self) -> QueueState {
        let Some(sessions) = self.sessions.upgrade() else {
            return QueueState {
                enabled: false,
                total: 0,
                waiting: Vec::new(),
            };
        };
        let waiting = sessions.queue.waiting();
        QueueState {
            enabled: self.config().queue.enabled,
            total: waiting.len(),
            waiting: waiting
                .into_iter()
                .take(MAX_PAGE)
                .enumerate()
                .map(|(i, (name, waited))| Waiting {
                    position: i + 1,
                    name,
                    seconds: waited.as_secs(),
                })
                .collect(),
        }
    }
}
