//! The `[auth]` table: how the hybrid login mode looks names up (DESIGN.md
//! section 15.2). The mode itself is the top-level `auth-mode`.
use crate::ConfigError;
use serde::Deserialize;

/// What happens to a player whose name could not be looked up, because the
/// account service did not answer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnLookupFailure {
    /// Refuse the login. The recommended choice: the name might be an account's.
    #[default]
    Refuse,
    /// Let the player in as unverified.
    Unverified,
}

impl OnLookupFailure {
    /// The name used in the file.
    pub fn name(self) -> &'static str {
        match self {
            OnLookupFailure::Refuse => "refuse",
            OnLookupFailure::Unverified => "unverified",
        }
    }
}

/// How names are looked up in the hybrid mode. Only that mode reads them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthSettings {
    /// Seconds a name found to be an account's is remembered.
    pub lookup_cache_seconds: u64,
    /// Seconds a name found to be no account's is remembered. A free name can be
    /// taken at any time, so this is shorter than `lookup_cache_seconds`.
    pub negative_cache_seconds: u64,
    /// What happens when the account service does not answer.
    pub on_lookup_failure: OnLookupFailure,
    /// Lookups one address may cause in a minute. Names already remembered do
    /// not count.
    pub lookups_per_minute: u32,
}

impl Default for AuthSettings {
    fn default() -> Self {
        Self {
            lookup_cache_seconds: 3600,
            negative_cache_seconds: 300,
            on_lookup_failure: OnLookupFailure::Refuse,
            lookups_per_minute: 10,
        }
    }
}

impl AuthSettings {
    /// A remembered "no account" never outlives a remembered "account", and an
    /// address may make at least one lookup a minute.
    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        if self.negative_cache_seconds > self.lookup_cache_seconds {
            return Err(ConfigError::NegativeCacheLonger {
                positive: self.lookup_cache_seconds,
                negative: self.negative_cache_seconds,
            });
        }
        if self.lookups_per_minute == 0 {
            return Err(ConfigError::ZeroLookupsPerMinute);
        }
        Ok(())
    }
}

/// The `[auth]` table as written.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct FileAuth {
    lookup_cache_seconds: Option<u64>,
    negative_cache_seconds: Option<u64>,
    on_lookup_failure: Option<OnLookupFailure>,
    lookups_per_minute: Option<u32>,
}

impl FileAuth {
    pub(crate) fn into_settings(self) -> AuthSettings {
        let defaults = AuthSettings::default();
        AuthSettings {
            lookup_cache_seconds: self
                .lookup_cache_seconds
                .unwrap_or(defaults.lookup_cache_seconds),
            negative_cache_seconds: self
                .negative_cache_seconds
                .unwrap_or(defaults.negative_cache_seconds),
            on_lookup_failure: self.on_lookup_failure.unwrap_or(defaults.on_lookup_failure),
            lookups_per_minute: self
                .lookups_per_minute
                .unwrap_or(defaults.lookups_per_minute),
        }
    }
}
