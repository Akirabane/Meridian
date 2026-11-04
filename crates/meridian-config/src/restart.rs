//! The `[restart]` table: what the proxy does when it learns that a backend is about to
//! restart (docs/RESTART.md).
use super::*;

/// The `[restart]` table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Restart {
    /// Whether the players of a backend that is going down are sent to other servers of
    /// its role, one at a time. Off by default: the backend is then only marked draining,
    /// and its players stay on it, as they do after `drain`.
    pub evacuate: bool,
    /// How many rounds in a row the sidecar of a backend may stay silent before the
    /// backend counts as going down.
    pub silent_rounds: u32,
    /// Seconds a backend that was told to stop may go on answering before it counts as
    /// back: it answers for a while as it shuts down.
    pub grace_seconds: u64,
}

impl Default for Restart {
    fn default() -> Self {
        Self {
            evacuate: false,
            silent_rounds: 3,
            grace_seconds: 60,
        }
    }
}

/// The `[restart]` table as written.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct FileRestart {
    evacuate: Option<bool>,
    silent_rounds: Option<u32>,
    grace_seconds: Option<u64>,
}

impl FileRestart {
    pub(crate) fn into_settings(self) -> Restart {
        let defaults = Restart::default();
        Restart {
            evacuate: self.evacuate.unwrap_or(defaults.evacuate),
            silent_rounds: self.silent_rounds.unwrap_or(defaults.silent_rounds),
            grace_seconds: self.grace_seconds.unwrap_or(defaults.grace_seconds),
        }
    }
}

impl Restart {
    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        if self.silent_rounds == 0 {
            return Err(ConfigError::ZeroSilentRounds);
        }
        if self.grace_seconds == 0 {
            return Err(ConfigError::ZeroRestartGrace);
        }
        Ok(())
    }
}
