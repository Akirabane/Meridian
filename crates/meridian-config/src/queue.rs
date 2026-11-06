//! The `[queue]` table: the wait for a place when the proxy is full (docs/CONFIG.md).
use super::*;

/// The longest wait that can be set, in seconds. The player's client gives up when
/// nothing reaches it for 30 seconds (vanilla's read timeout), and the proxy sends it
/// nothing while it waits; what is left covers reaching the backend after the wait
/// (`connect-timeout-ms`) and the few packets that start the configuration.
pub const MAX_QUEUE_WAIT_SECONDS: u64 = 20;

/// What the client's silence may add up to, in milliseconds: the wait plus the time
/// to reach a backend must stay under the 30 seconds it waits, with a margin.
pub const QUEUE_SILENCE_LIMIT_MS: u64 = 27_000;

/// The `[queue]` table. Off by default: a player who finds the proxy full is refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueueSettings {
    /// Whether a player who finds no place waits for one.
    pub enabled: bool,
    /// Seconds a player waits before they are refused as they would be without a queue.
    pub wait_seconds: u64,
    /// Players who may wait at once; one more is refused at once.
    pub max_length: u32,
}

impl Default for QueueSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            wait_seconds: 15,
            max_length: 50,
        }
    }
}

/// The `[queue]` table as written.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct FileQueue {
    enabled: Option<bool>,
    wait_seconds: Option<u64>,
    max_length: Option<u32>,
}

impl FileQueue {
    pub(crate) fn into_settings(self) -> QueueSettings {
        let defaults = QueueSettings::default();
        QueueSettings {
            enabled: self.enabled.unwrap_or(defaults.enabled),
            wait_seconds: self.wait_seconds.unwrap_or(defaults.wait_seconds),
            max_length: self.max_length.unwrap_or(defaults.max_length),
        }
    }
}

impl QueueSettings {
    /// Checked when the queue is on; the values of a queue that is off are not used.
    /// `extension_ms` is what the extensions that decide a login may take (docs/EXTENSIONS.md, 4.2):
    /// the client sees nothing during that time either.
    pub(crate) fn validate(
        &self,
        connect_timeout_ms: u64,
        extension_ms: u64,
    ) -> Result<(), ConfigError> {
        if !self.enabled {
            return Ok(());
        }
        if self.wait_seconds == 0 {
            return Err(ConfigError::ZeroQueueWait);
        }
        if self.max_length == 0 {
            return Err(ConfigError::ZeroQueueLength);
        }
        if self.wait_seconds > MAX_QUEUE_WAIT_SECONDS
            || self
                .wait_seconds
                .saturating_mul(1000)
                .saturating_add(connect_timeout_ms)
                .saturating_add(extension_ms)
                > QUEUE_SILENCE_LIMIT_MS
        {
            return Err(ConfigError::QueueWaitTooLong {
                wait_seconds: self.wait_seconds,
                connect_timeout_ms,
                extension_ms,
            });
        }
        Ok(())
    }
}
