//! The permissions in force (docs/OPERATING.md, section 5.9): the groups and the players in
//! them that `meridian-config` reads from `permissions.toml`, the copy the players' threads
//! ask and the console changes, and the file it is kept in.
//!
//! The console is the operator's: it needs no permission. What is checked is what a player
//! types in game (`commands.rs`).
use crate::log::warning;
use meridian_config::{AccessError, Permissions};
use std::path::{Path, PathBuf};
use std::sync::RwLock;

pub mod commands;

/// The permissions in force and the file they are written to.
#[derive(Default)]
pub struct Lists {
    state: RwLock<State>,
}

#[derive(Default)]
struct State {
    permissions: Permissions,
    /// Where a change is written; none for lists kept in memory only.
    file: Option<PathBuf>,
}

/// What the permissions hold, as a line to show.
pub fn summary(permissions: &Permissions) -> String {
    let count = |n: usize, one: &str, many: &str| match n {
        1 => format!("1 {one}"),
        n => format!("{n} {many}"),
    };
    format!(
        "{}, {}",
        count(permissions.groups().len(), "group", "groups"),
        count(permissions.members().len(), "player", "players")
    )
}

/// Reads the permissions from `file`: none when the file is not there, and `None`
/// when there is no file, for lists kept in memory only, which a reload leaves be.
pub fn read(file: Option<&Path>) -> Result<Option<Permissions>, AccessError> {
    file.map(Permissions::load).transpose()
}

impl Lists {
    fn read(&self) -> std::sync::RwLockReadGuard<'_, State> {
        self.state.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Puts `permissions`, read from `file`, in force.
    pub fn install(&self, permissions: Permissions, file: Option<PathBuf>) {
        *self.state.write().unwrap_or_else(|e| e.into_inner()) = State { permissions, file };
    }

    /// The permissions now in force.
    pub fn snapshot(&self) -> Permissions {
        self.read().permissions.clone()
    }

    /// Changes the permissions with `change`, which says what it did and whether it
    /// changed anything, and writes the file when it did. The change stands in memory even
    /// if the file cannot be written; the error says so.
    pub fn change<R>(
        &self,
        change: impl FnOnce(&mut Permissions) -> (R, bool),
    ) -> (R, Result<(), AccessError>) {
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        let (result, changed) = change(&mut state.permissions);
        let saved = match (&state.file, changed) {
            (Some(file), true) => state.permissions.save(file),
            _ => Ok(()),
        };
        (result, saved)
    }
}

/// Says that the permissions could not be written.
pub fn not_saved(error: &AccessError) -> String {
    let line = format!("{error}; the change holds until the proxy stops");
    warning!("{line}");
    line
}

#[cfg(test)]
mod tests;
