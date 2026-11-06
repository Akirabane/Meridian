//! The operators of the servers, as the proxy reflects them (docs/OPERATING.md, section 5.9).
//!
//! The proxy keeps no list of operators of its own: each server owns its operators (Horizon's
//! `ops.json`), and with `[operators] from-servers` on the proxy reads them through the servers'
//! sidecars (`access/sync/hear.rs`) into this cache and never writes them back. A player who is
//! an operator of the server they are on, at `min-level` or above, is a member of
//! `[operators] group` of `permissions.toml` when the proxy decides about one of its commands.
//! The cache is memory only; nothing of it reaches `permissions.toml`.
use meridian_config::{AuthKind, OperatorSettings};
use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

/// One operator of a server, as the server lists them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Op {
    /// The unique id the server gives, when it gives one.
    pub id: Option<[u8; 16]>,
    pub name: String,
    /// The permission level, 0 to 4.
    pub level: u8,
}

/// The operators a server listed, and when its sidecar was last heard connected to it.
struct Heard {
    at: Instant,
    ops: Vec<Op>,
}

#[derive(Default)]
struct State {
    settings: OperatorSettings,
    servers: HashMap<String, Heard>,
}

/// What the servers said about their operators, by backend.
#[derive(Default)]
pub struct Operators {
    state: RwLock<State>,
}

impl Operators {
    fn read(&self) -> std::sync::RwLockReadGuard<'_, State> {
        self.state.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, State> {
        self.state.write().unwrap_or_else(|e| e.into_inner())
    }

    /// Puts `settings` in force. What was read from a server that does not count any more, or
    /// from any server once the table is off, is forgotten at once.
    pub fn configure(&self, settings: &OperatorSettings) {
        let mut state = self.write();
        if state.settings == *settings {
            return;
        }
        state
            .servers
            .retain(|backend, _| settings.from_servers && settings.counts(backend));
        state.settings = settings.clone();
    }

    /// The list `backend`'s server holds now, which replaces what was known of it: an operator
    /// the server no longer lists is no longer one.
    pub fn replace(&self, backend: &str, ops: Vec<Op>, now: Instant) {
        self.write()
            .servers
            .insert(backend.to_owned(), Heard { at: now, ops });
    }

    /// `backend`'s server was heard connected, with nothing new to say: what is known of it
    /// is believed for another `stale-after-seconds`.
    pub fn heard(&self, backend: &str, now: Instant) {
        if let Some(heard) = self.write().servers.get_mut(backend) {
            heard.at = now;
        }
    }

    /// The groups the player is a member of because they are an operator of `backend`, the server
    /// they are on: the group of `[operators]`, or none.
    pub fn groups_for(
        &self,
        backend: &str,
        name: &str,
        uuid: [u8; 16],
        auth: AuthKind,
    ) -> Vec<String> {
        self.groups_at(backend, name, uuid, auth, Instant::now())
    }

    pub(crate) fn groups_at(
        &self,
        backend: &str,
        name: &str,
        uuid: [u8; 16],
        auth: AuthKind,
        now: Instant,
    ) -> Vec<String> {
        let state = self.read();
        match level_in(&state, backend, name, uuid, auth, now) {
            Some(_) => vec![state.settings.group.clone()],
            None => Vec::new(),
        }
    }

    /// The level the player has as an operator of `backend`, if they count as one.
    pub fn level_of(
        &self,
        backend: &str,
        name: &str,
        uuid: [u8; 16],
        auth: AuthKind,
    ) -> Option<u8> {
        level_in(&self.read(), backend, name, uuid, auth, Instant::now())
    }
}

/// Whether what was heard from a server is still believed.
fn fresh(settings: &OperatorSettings, heard: &Heard, now: Instant) -> bool {
    now.saturating_duration_since(heard.at) <= Duration::from_secs(settings.stale_after_seconds)
}

/// The player's level as an operator of `backend`: the table is on, the backend counts, the
/// list is believed, and the player is in it at `min-level` or above. They are found by unique
/// id; by name only when the server gave no id for the entry and the player's account was
/// checked, since a name proves nothing otherwise (docs/OPERATING.md, section 5.9).
fn level_in(
    state: &State,
    backend: &str,
    name: &str,
    uuid: [u8; 16],
    auth: AuthKind,
    now: Instant,
) -> Option<u8> {
    let settings = &state.settings;
    if !settings.from_servers || !settings.counts(backend) {
        return None;
    }
    let heard = state.servers.get(backend)?;
    if !fresh(settings, heard, now) {
        return None;
    }
    heard
        .ops
        .iter()
        .find(|op| match op.id {
            Some(id) => id == uuid,
            None => auth == AuthKind::Verified && op.name.eq_ignore_ascii_case(name),
        })
        .map(|op| op.level)
        .filter(|level| *level >= settings.min_level)
}

#[cfg(test)]
mod tests;
