//! The `[operators]` table: the operators of the servers, read from them (docs/CONFIG.md).
//!
//! The proxy keeps no list of operators of its own. With `from-servers` on it reads each
//! server's list through that server's sidecar and treats a player who is an operator of the
//! server they are on, at `min-level` or above, as a member of `group` of `permissions.toml`.
use super::*;

/// The highest permission level an operator has, as Horizon (and vanilla) number them.
pub const MAX_OPERATOR_LEVEL: u8 = 4;

/// The most seconds a list read from a server is believed after the server was last heard.
pub const MAX_OPERATOR_STALE_SECONDS: u64 = 86_400;

/// The `[operators]` table. Off by default: nobody is an operator of the proxy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperatorSettings {
    /// Whether the operators of the servers are read.
    pub from_servers: bool,
    /// The group of `permissions.toml` that the operators are members of.
    pub group: String,
    /// The lowest permission level that counts (0 to 4).
    pub min_level: u8,
    /// The backends whose operators count; empty means every backend that has a sidecar.
    pub from: Vec<String>,
    /// Seconds after which the list read from a server is forgotten if the server has not
    /// been heard since.
    pub stale_after_seconds: u64,
}

impl Default for OperatorSettings {
    fn default() -> Self {
        Self {
            from_servers: false,
            group: "operator".to_owned(),
            min_level: MAX_OPERATOR_LEVEL,
            from: Vec::new(),
            stale_after_seconds: 600,
        }
    }
}

/// The `[operators]` table as written.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct FileOperators {
    from_servers: Option<bool>,
    group: Option<String>,
    min_level: Option<u8>,
    from: Option<Vec<String>>,
    stale_after_seconds: Option<u64>,
}

impl FileOperators {
    pub(crate) fn into_settings(self) -> OperatorSettings {
        let defaults = OperatorSettings::default();
        OperatorSettings {
            from_servers: self.from_servers.unwrap_or(defaults.from_servers),
            group: self.group.unwrap_or(defaults.group),
            min_level: self.min_level.unwrap_or(defaults.min_level),
            from: self.from.unwrap_or(defaults.from),
            stale_after_seconds: self
                .stale_after_seconds
                .unwrap_or(defaults.stale_after_seconds),
        }
    }
}

impl OperatorSettings {
    /// Whether the operators of `backend` count: it is one of `from`, or `from` is empty.
    /// (Whether the backend has a sidecar to be read through is the proxy's to know.)
    pub fn counts(&self, backend: &str) -> bool {
        self.from.is_empty() || self.from.iter().any(|b| b == backend)
    }

    /// Checked against the configuration it is part of. The level is checked whether or not the
    /// table is on; the rest only when it is.
    pub(crate) fn validate(&self, config: &Config) -> Result<(), ConfigError> {
        if self.min_level > MAX_OPERATOR_LEVEL {
            return Err(ConfigError::OperatorLevelTooHigh(self.min_level));
        }
        if !self.from_servers {
            return Ok(());
        }
        if let Err(why) = check_group_name(&self.group) {
            return Err(ConfigError::BadOperatorGroup(why));
        }
        if self.stale_after_seconds == 0 || self.stale_after_seconds > MAX_OPERATOR_STALE_SECONDS {
            return Err(ConfigError::OperatorStaleOutOfRange(
                self.stale_after_seconds,
            ));
        }
        for (i, name) in self.from.iter().enumerate() {
            config.known_backend("[operators] from", name)?;
            if !config.sidecars.iter().any(|s| &s.backend == name) {
                return Err(ConfigError::OperatorSourceWithoutSidecar(name.clone()));
            }
            if self.from[..i].contains(name) {
                return Err(ConfigError::DuplicateOperatorSource(name.clone()));
            }
        }
        Ok(())
    }
}
