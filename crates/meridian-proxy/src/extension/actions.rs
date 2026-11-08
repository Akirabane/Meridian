//! What the orders that act on the players use (docs/EXTENSIONS.md, section 5): the proxy's own sessions and
//! `staff::alert`, the function that the console's `alert` and the staff's `/alert` call. A line is shown as a literal
//! (`SessionHandle::tell`), never as a JSON text component, so an extension cannot put a click event in a chat.

use super::orders::Actions;
use crate::session::{Sessions, hyphenated};
use crate::staff;
use meridian_config::Config;
use meridian_extension::orders::Who;
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
        match handle {
            Some(handle) => {
                handle.tell(text);
                1
            }
            None => 0,
        }
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
        staff::alert(&sessions.shared(), &self.config(), server, text)
    }
}
