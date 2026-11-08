//! The `[[extension]]` tables: the external programs the proxy connects to and asks for
//! decisions (docs/EXTENSIONS.md, 4.1). None by default, and then nothing here changes anything.
//! The token itself is never in the configuration: only the path of its file.
use super::*;
use std::net::IpAddr;

/// The shortest token accepted, in bytes: what the handshake of the extension protocol needs
/// (`meridian_extension::handshake::MIN_TOKEN_LEN`, which a test in the proxy keeps equal).
pub const MIN_EXTENSION_TOKEN: usize = 32;

/// The capability of the admission decision point (docs/EXTENSIONS.md, 4.2).
pub const DECIDE_LOGIN: &str = "decide.login";
/// The capability of the permission decision point for the proxy's commands.
pub const DECIDE_COMMAND: &str = "decide.command";
/// The capability of the choice of server.
pub const DECIDE_SERVER: &str = "decide.server";
/// The capability of the commands of the extension's own (docs/EXTENSIONS.md, 4.3).
pub const COMMANDS: &str = "commands";

/// Every capability an operator may grant (docs/EXTENSIONS.md, 2.3).
pub const CAPABILITIES: [&str; 12] = [
    DECIDE_LOGIN,
    DECIDE_COMMAND,
    COMMANDS,
    DECIDE_SERVER,
    "orders.read",
    "orders.message",
    "orders.move",
    "orders.moderate",
    "orders.operate",
    "orders.console",
    "events",
    "addresses",
];

/// The longest `[extension.commands] timeout-ms`: a thread waits that long for the lines of a command.
pub const MAX_OWN_COMMANDS_TIMEOUT_MS: u64 = 10_000;

/// The longest name of an extension, in characters.
pub const MAX_EXTENSION_NAME: usize = 32;

/// What a decision point does when there is no usable answer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnError {
    /// As if no extension were configured: the built-in modules decide.
    #[default]
    Default,
    /// Let the player in, or let the command run.
    Allow,
    /// Refuse the login or the command.
    Deny,
}

impl OnError {
    /// The name used in the file.
    pub fn name(self) -> &'static str {
        match self {
            OnError::Default => "default",
            OnError::Allow => "allow",
            OnError::Deny => "deny",
        }
    }
}

/// What the choice of server does when there is no usable answer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServerOnError {
    /// As if no extension were configured.
    #[default]
    Default,
    /// Refuse the player.
    Refuse,
}

impl ServerOnError {
    /// The name used in the file.
    pub fn name(self) -> &'static str {
        match self {
            ServerOnError::Default => "default",
            ServerOnError::Refuse => "refuse",
        }
    }
}

/// `[extension.decide-login]`: the admission at login.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginDecision {
    /// Milliseconds the proxy waits for the answer.
    pub timeout_ms: u64,
    pub on_error: OnError,
}

impl Default for LoginDecision {
    fn default() -> Self {
        Self {
            timeout_ms: 1_000,
            on_error: OnError::Default,
        }
    }
}

/// `[extension.decide-command]`: the permission of a command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandDecision {
    /// Milliseconds the proxy waits for the answer.
    pub timeout_ms: u64,
    /// What a command takes with no usable answer: `Default` (the file and the operators decide) or
    /// `Deny`. `Allow` is refused when the configuration is read (it would give every command to anyone
    /// while the extension is down), and the proxy never grants on it.
    pub on_error: OnError,
    /// The longest an answer may be kept, in seconds, whatever the extension asks.
    pub cache_seconds_max: u64,
}

impl Default for CommandDecision {
    fn default() -> Self {
        Self {
            timeout_ms: 250,
            on_error: OnError::Default,
            cache_seconds_max: 300,
        }
    }
}

/// `[extension.commands]`: the commands of the extension's own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnCommands {
    /// Milliseconds the proxy waits for the lines a command answers; after that the player reads that
    /// the command did not answer.
    pub timeout_ms: u64,
}

impl Default for OwnCommands {
    fn default() -> Self {
        Self { timeout_ms: 2_000 }
    }
}

/// `[extension.decide-server]`: the choice of server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerDecision {
    /// Milliseconds the proxy waits for the answer.
    pub timeout_ms: u64,
    pub on_error: ServerOnError,
}

impl Default for ServerDecision {
    fn default() -> Self {
        Self {
            timeout_ms: 1_000,
            on_error: ServerOnError::Default,
        }
    }
}

/// One `[[extension]]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Extension {
    /// In logs, metrics and node names.
    pub name: String,
    /// An extension that is not enabled is read and checked, and the proxy does not connect to it.
    pub enabled: bool,
    /// Where it listens, as `host:port`: an address on the loopback, or `localhost`.
    pub address: String,
    /// The file that holds the token, already resolved against the folder of the configuration file.
    pub token_file: PathBuf,
    /// The capabilities the operator grants it.
    pub grant: Vec<String>,
    /// The most requests that may wait for its answers at once.
    pub max_in_flight: u32,
    /// Decisions in a row without a usable answer that open the breaker.
    pub breaker_failures: u32,
    /// How long the breaker stays open, in seconds.
    pub breaker_seconds: u64,
    pub decide_login: LoginDecision,
    pub decide_command: CommandDecision,
    pub decide_server: ServerDecision,
    pub own_commands: OwnCommands,
}

impl Extension {
    /// Whether the operator grants `capability` to this extension.
    pub fn grants(&self, capability: &str) -> bool {
        self.grant.iter().any(|c| c == capability)
    }
}

/// The `[[extension]]` as written.
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct FileExtension {
    name: String,
    enabled: Option<bool>,
    address: String,
    token_file: PathBuf,
    #[serde(default)]
    grant: Vec<String>,
    max_in_flight: Option<u32>,
    breaker_failures: Option<u32>,
    breaker_seconds: Option<u64>,
    decide_login: Option<FileLoginDecision>,
    decide_command: Option<FileCommandDecision>,
    decide_server: Option<FileServerDecision>,
    commands: Option<FileOwnCommands>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct FileOwnCommands {
    timeout_ms: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct FileLoginDecision {
    timeout_ms: Option<u64>,
    on_error: Option<OnError>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct FileCommandDecision {
    timeout_ms: Option<u64>,
    on_error: Option<OnError>,
    cache_seconds_max: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct FileServerDecision {
    timeout_ms: Option<u64>,
    on_error: Option<ServerOnError>,
}

impl FileExtension {
    pub(crate) fn into_settings(self, folder: &Path) -> Result<Extension, ConfigError> {
        // A table of a decision point that is not granted would be read and never used: say so.
        let table = |written: bool, table: &'static str, capability: &'static str| {
            if written && !self.grant.iter().any(|c| c == capability) {
                return Err(ConfigError::DecideWithoutGrant {
                    extension: self.name.clone(),
                    table,
                    capability,
                });
            }
            Ok(())
        };
        table(self.decide_login.is_some(), "decide-login", DECIDE_LOGIN)?;
        table(
            self.decide_command.is_some(),
            "decide-command",
            DECIDE_COMMAND,
        )?;
        table(self.decide_server.is_some(), "decide-server", DECIDE_SERVER)?;
        table(self.commands.is_some(), "commands", COMMANDS)?;
        let own = self.commands.and_then(|c| c.timeout_ms);
        let login = self.decide_login.unwrap_or(FileLoginDecision {
            timeout_ms: None,
            on_error: None,
        });
        let command = self.decide_command.unwrap_or(FileCommandDecision {
            timeout_ms: None,
            on_error: None,
            cache_seconds_max: None,
        });
        let server = self.decide_server.unwrap_or(FileServerDecision {
            timeout_ms: None,
            on_error: None,
        });
        let (login_default, command_default, server_default) = (
            LoginDecision::default(),
            CommandDecision::default(),
            ServerDecision::default(),
        );
        Ok(Extension {
            name: self.name,
            enabled: self.enabled.unwrap_or(true),
            address: self.address,
            token_file: sidecar::resolve(folder, &self.token_file),
            grant: self.grant,
            max_in_flight: self.max_in_flight.unwrap_or(64),
            breaker_failures: self.breaker_failures.unwrap_or(5),
            breaker_seconds: self.breaker_seconds.unwrap_or(30),
            decide_login: LoginDecision {
                timeout_ms: login.timeout_ms.unwrap_or(login_default.timeout_ms),
                on_error: login.on_error.unwrap_or(login_default.on_error),
            },
            decide_command: CommandDecision {
                timeout_ms: command.timeout_ms.unwrap_or(command_default.timeout_ms),
                on_error: command.on_error.unwrap_or(command_default.on_error),
                cache_seconds_max: command
                    .cache_seconds_max
                    .unwrap_or(command_default.cache_seconds_max),
            },
            decide_server: ServerDecision {
                timeout_ms: server.timeout_ms.unwrap_or(server_default.timeout_ms),
                on_error: server.on_error.unwrap_or(server_default.on_error),
            },
            own_commands: OwnCommands {
                timeout_ms: own.unwrap_or(OwnCommands::default().timeout_ms),
            },
        })
    }
}

/// The time the extensions may take, in milliseconds, between the login and the player's
/// arrival on a server, while the client sees nothing: the admission and the choice of server,
/// for the extensions that are enabled and granted them.
pub(crate) fn login_budget_ms(extensions: &[Extension]) -> u64 {
    extensions
        .iter()
        .filter(|e| e.enabled)
        .map(|e| {
            let login = if e.grants(DECIDE_LOGIN) {
                e.decide_login.timeout_ms
            } else {
                0
            };
            let server = if e.grants(DECIDE_SERVER) {
                e.decide_server.timeout_ms
            } else {
                0
            };
            login.saturating_add(server)
        })
        .fold(0, u64::saturating_add)
}

/// An address on this machine, written so that it can be checked without asking a name server:
/// an IP address on the loopback, or `localhost`. The proxy resolves the address again at each
/// connection and refuses any that is not on the loopback.
fn check_loopback(address: &str) -> Result<(), &'static str> {
    crate::check_address(address)?;
    let host = address.rsplit_once(':').map_or(address, |(host, _)| host);
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.eq_ignore_ascii_case("localhost") {
        return Ok(());
    }
    match host.parse::<IpAddr>() {
        Ok(ip) if ip.is_loopback() => Ok(()),
        Ok(_) => Err(
            "it is not on the loopback: an extension runs on this machine, on 127.0.0.1 or [::1]",
        ),
        Err(_) => Err("write an IP address on the loopback (127.0.0.1 or [::1]), or localhost"),
    }
}

fn check_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err("it is empty");
    }
    if name.chars().count() > MAX_EXTENSION_NAME {
        return Err("it is longer than 32 characters");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("it may hold only letters, digits, - and _");
    }
    Ok(())
}

/// The token of an enabled extension can be read and is long enough. The error names the file,
/// never its content.
fn check_token(path: &Path) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let length = bytes
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map_or(0, |last| last + 1);
    if length < MIN_EXTENSION_TOKEN {
        return Err(format!(
            "{} holds {length} bytes; a token has at least {MIN_EXTENSION_TOKEN}",
            path.display()
        ));
    }
    Ok(())
}

/// Each extension has its own name, address and token file, its address is on the loopback, its
/// capabilities exist, its numbers are not zero, its token can be read, and no decision point is
/// granted to two extensions that are enabled.
pub(crate) fn validate(extensions: &[Extension]) -> Result<(), ConfigError> {
    for (i, e) in extensions.iter().enumerate() {
        check_name(&e.name).map_err(|reason| ConfigError::InvalidExtensionName {
            name: e.name.clone(),
            reason,
        })?;
        // The case does not tell two names apart: the permission nodes of an extension's commands
        // (`ext.<name>.<command>`) are lower case, so `Perms` and `perms` would share them.
        if extensions[..i]
            .iter()
            .any(|o| o.name.eq_ignore_ascii_case(&e.name))
        {
            return Err(ConfigError::DuplicateExtension(e.name.clone()));
        }
        check_loopback(&e.address).map_err(|reason| ConfigError::InvalidExtensionAddress {
            extension: e.name.clone(),
            address: e.address.clone(),
            reason,
        })?;
        for other in &extensions[..i] {
            if other.address.eq_ignore_ascii_case(&e.address) {
                return Err(ConfigError::SharedExtensionAddress {
                    first: other.name.clone(),
                    second: e.name.clone(),
                    address: e.address.clone(),
                });
            }
            if sidecar::same_file(&other.token_file, &e.token_file) {
                return Err(ConfigError::SharedExtensionToken {
                    first: other.name.clone(),
                    second: e.name.clone(),
                    path: e.token_file.clone(),
                });
            }
        }
        for (j, capability) in e.grant.iter().enumerate() {
            if !CAPABILITIES.contains(&capability.as_str()) {
                return Err(ConfigError::UnknownCapability {
                    extension: e.name.clone(),
                    capability: capability.clone(),
                });
            }
            if e.grant[..j].contains(capability) {
                return Err(ConfigError::DuplicateCapability {
                    extension: e.name.clone(),
                    capability: capability.clone(),
                });
            }
        }
        let zero = |setting: &'static str, value: u64| {
            if value == 0 {
                Err(ConfigError::ZeroExtensionSetting {
                    extension: e.name.clone(),
                    setting,
                })
            } else {
                Ok(())
            }
        };
        zero("max-in-flight", u64::from(e.max_in_flight))?;
        zero("breaker-failures", u64::from(e.breaker_failures))?;
        zero("breaker-seconds", e.breaker_seconds)?;
        zero("decide-login timeout-ms", e.decide_login.timeout_ms)?;
        zero("decide-command timeout-ms", e.decide_command.timeout_ms)?;
        zero("decide-server timeout-ms", e.decide_server.timeout_ms)?;
        zero("commands timeout-ms", e.own_commands.timeout_ms)?;
        // The commands of an extension use at most half of its places (`max-in-flight`): with one place the
        // commands would take it and every decision would find none.
        if e.grants(COMMANDS) && e.max_in_flight < 2 {
            return Err(ConfigError::CommandsNeedPlaces {
                extension: e.name.clone(),
            });
        }
        if e.own_commands.timeout_ms > MAX_OWN_COMMANDS_TIMEOUT_MS {
            return Err(ConfigError::ExtensionSettingAbove {
                extension: e.name.clone(),
                setting: "commands timeout-ms",
                most: MAX_OWN_COMMANDS_TIMEOUT_MS,
            });
        }
        // With no usable answer (the extension down, slow or saturated) `allow` would give every command of
        // the proxy, `/send` and `/alert` among them, to whoever types it: a fail-open on the system of
        // rights. The login and the choice of server may `allow`: the bans, the whitelist and the caps
        // come before them, and there `allow` only lets the proxy's own policy go on.
        if e.decide_command.on_error == OnError::Allow {
            return Err(ConfigError::ExtensionCommandAllow {
                extension: e.name.clone(),
            });
        }
        if e.enabled {
            check_token(&e.token_file).map_err(|reason| ConfigError::ExtensionToken {
                extension: e.name.clone(),
                reason,
            })?;
            // Each decision point has one extension at most.
            for capability in [DECIDE_LOGIN, DECIDE_COMMAND, DECIDE_SERVER] {
                if !e.grants(capability) {
                    continue;
                }
                if let Some(other) = extensions[..i]
                    .iter()
                    .find(|o| o.enabled && o.grants(capability))
                {
                    return Err(ConfigError::SharedDecisionPoint {
                        capability,
                        first: other.name.clone(),
                        second: e.name.clone(),
                    });
                }
            }
        }
    }
    Ok(())
}

impl Config {
    /// The extensions that are enabled.
    pub fn enabled_extensions(&self) -> impl Iterator<Item = &Extension> {
        self.extensions.iter().filter(|e| e.enabled)
    }

    /// With the queue off, the extensions and the connection to a backend still have to fit in
    /// the silence a client bears (the queue's own check adds its wait when it is on).
    pub(crate) fn validate_login_budget(&self) -> Result<(), ConfigError> {
        let extension_ms = login_budget_ms(&self.extensions);
        if self.queue.enabled || extension_ms == 0 {
            return Ok(());
        }
        if extension_ms.saturating_add(self.connect_timeout_ms) > queue::QUEUE_SILENCE_LIMIT_MS {
            return Err(ConfigError::ExtensionBudget {
                extension_ms,
                connect_timeout_ms: self.connect_timeout_ms,
            });
        }
        Ok(())
    }
}
