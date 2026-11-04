//! The `[fallback]` table: what the proxy does with a player whose server is lost while they
//! are in the game (docs/OPERATING.md, "When a server is lost").
use super::*;

/// The `[fallback]` table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fallback {
    /// Move a player whom their server disconnects, instead of disconnecting them.
    pub on_kick: bool,
    /// Move a player whose server's connection closes or breaks.
    pub on_crash: bool,
    /// The most servers the proxy tries for a player in a row, counting each server it
    /// connects to and each one that is lost in turn, until a server keeps them for a while.
    pub max_attempts: u32,
}

impl Default for Fallback {
    fn default() -> Self {
        Self {
            on_kick: true,
            on_crash: true,
            max_attempts: 3,
        }
    }
}

/// The `[fallback]` table as written.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct FileFallback {
    on_kick: Option<bool>,
    on_crash: Option<bool>,
    max_attempts: Option<u32>,
}

impl FileFallback {
    pub(crate) fn into_settings(self) -> Fallback {
        let defaults = Fallback::default();
        Fallback {
            on_kick: self.on_kick.unwrap_or(defaults.on_kick),
            on_crash: self.on_crash.unwrap_or(defaults.on_crash),
            max_attempts: self.max_attempts.unwrap_or(defaults.max_attempts),
        }
    }
}

impl Fallback {
    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        if self.max_attempts == 0 {
            return Err(ConfigError::ZeroFallbackAttempts);
        }
        Ok(())
    }
}
