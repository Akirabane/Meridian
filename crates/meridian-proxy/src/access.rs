//! The bans and the whitelist in force (docs/OPERATING.md, "Bans and whitelist"). The
//! lists are those of `meridian-config`'s [`Access`]; this is the copy the players'
//! threads read and the console changes, the file it is kept in, and what happens to the
//! players already connected when a ban is made.
//!
//! An address is checked first, as the connection is taken and before a byte is read
//! from it; a name and a unique id once the player is identified, before any backend is
//! asked for them (`session::serve`).
use crate::log::{info, warning};
use crate::session::Sessions;
use meridian_config::{Access, AccessError, Refusal};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

pub mod commands;

/// The lists in force and the file they are written to.
#[derive(Default)]
pub struct Lists {
    state: RwLock<State>,
}

#[derive(Default)]
struct State {
    access: Access,
    /// Where a change is written; none for lists kept in memory only.
    file: Option<PathBuf>,
}

/// What the lists hold, as a line to show.
pub fn summary(access: &Access) -> String {
    let bans = match access.bans().len() {
        1 => "1 ban".to_owned(),
        n => format!("{n} bans"),
    };
    let whitelist = if access.whitelist_on() {
        format!("whitelist on, {} listed", access.allowed().len())
    } else {
        "whitelist off".to_owned()
    };
    format!("{bans}, {whitelist}")
}

/// Reads the lists from `file`: empty ones when the file is not there, and `None`
/// when there is no file, for lists kept in memory only, which a reload leaves be.
pub fn read(file: Option<&Path>) -> Result<Option<Access>, AccessError> {
    file.map(Access::load).transpose()
}

impl Lists {
    fn read(&self) -> std::sync::RwLockReadGuard<'_, State> {
        self.state.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Puts `access`, read from `file`, in force.
    pub fn install(&self, access: Access, file: Option<PathBuf>) {
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        *state = State { access, file };
    }

    /// The lists now in force.
    pub fn snapshot(&self) -> Access {
        self.read().access.clone()
    }

    /// Whether `address` is banned, by its own entry or a range's.
    pub fn address_banned(&self, address: IpAddr) -> bool {
        self.read().access.address_ban(address).is_some()
    }

    /// Why the player may not join, if they may not.
    pub fn refusal(&self, name: &str, uuid: [u8; 16], address: IpAddr) -> Option<Refusal> {
        self.read().access.refusal(name, uuid, address)
    }

    /// Changes the lists with `change`, which says what it did and whether it changed
    /// anything, and writes the file when it did. The change stands in memory even if the
    /// file cannot be written, so that a ban is never held back by a full disk; the
    /// error says so.
    pub fn change<R>(
        &self,
        change: impl FnOnce(&mut Access) -> (R, bool),
    ) -> (R, Result<(), AccessError>) {
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        let (result, changed) = change(&mut state.access);
        let saved = match (&state.file, changed) {
            (Some(file), true) => state.access.save(file),
            _ => Ok(()),
        };
        (result, saved)
    }
}

/// Ends the sessions of the players who are banned now, telling each the reason, and
/// returns a line for each. A player who is not whitelisted is not sent away: the
/// whitelist is for those who join (`kick` ends a session by hand).
pub fn enforce(sessions: &Sessions) -> Vec<String> {
    let access = sessions.access.snapshot();
    let mut lines = Vec::new();
    for player in sessions.players() {
        let Some(ban) = access.ban_of(&player.name, player.uuid, player.address.ip()) else {
            continue;
        };
        let reason = Refusal::Banned(ban.reason.clone()).message();
        if sessions.kick(&player.name, &reason).is_some() {
            let line = format!("kicked {} from {}: banned", player.name, player.backend);
            info!("{line}");
            lines.push(line);
        }
    }
    lines
}

/// Says that the lists could not be written.
pub fn not_saved(error: &AccessError) -> String {
    let line = format!("{error}; the change holds until the proxy stops");
    warning!("{line}");
    line
}

#[cfg(test)]
mod tests;
