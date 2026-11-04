//! The `[metrics]` table: the page of counters and the health check (docs/CONFIG.md).
use super::*;

/// The `[metrics]` table. Off by default; when on, it listens on the loopback only
/// unless `bind` says otherwise.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Metrics {
    /// Whether the proxy serves `/metrics` and `/healthz`.
    pub enabled: bool,
    /// Where it listens, as `host:port`.
    pub bind: SocketAddr,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            enabled: false,
            bind: SocketAddr::from(([127, 0, 0, 1], 9100)),
        }
    }
}

/// The `[metrics]` table as written.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct FileMetrics {
    enabled: Option<bool>,
    bind: Option<String>,
}

impl FileMetrics {
    pub(crate) fn into_settings(self) -> Result<Metrics, ConfigError> {
        let defaults = Metrics::default();
        let bind = match self.bind {
            Some(value) => value
                .parse()
                .map_err(|_| ConfigError::InvalidMetricsBind(value))?,
            None => defaults.bind,
        };
        Ok(Metrics {
            enabled: self.enabled.unwrap_or(defaults.enabled),
            bind,
        })
    }
}

impl Metrics {
    pub(crate) fn validate(&self, listen: SocketAddr) -> Result<(), ConfigError> {
        if self.enabled && self.bind.port() == 0 {
            return Err(ConfigError::InvalidMetricsBind(self.bind.to_string()));
        }
        if self.enabled && self.bind.port() == listen.port() {
            let same = self.bind.ip() == listen.ip()
                || self.bind.ip().is_unspecified()
                || listen.ip().is_unspecified();
            if same {
                return Err(ConfigError::MetricsOnListen(self.bind));
            }
        }
        Ok(())
    }
}
