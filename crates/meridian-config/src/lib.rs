//! The configuration of the proxy: where it listens, which backends it knows, and
//! the limits it applies. [`Config::load`] reads it from a TOML file, fills in the
//! defaults and checks it; `meridian.example.toml` at the root of the repository
//! shows every setting.
mod error;

pub use error::ConfigError;

use serde::Deserialize;
use std::net::SocketAddr;
use std::path::Path;

/// A server the proxy can send players to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Backend {
    /// The name used in routes and commands.
    pub name: String,
    /// Where the server listens, as `host:port`.
    pub address: String,
}

/// A host name that sends the players who connect through it to one backend
/// (`[[forced-host]]` in the file).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForcedHost {
    /// The host name the client puts in its handshake, in lower case.
    pub host: String,
    /// The backend it leads to.
    pub backend: String,
}

/// How the proxy decides whether to check a player's account (`auth-mode`,
/// DESIGN.md section 15.2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthMode {
    /// Always check; a client that cannot answer is refused.
    #[default]
    Online,
    /// Never check: every player is unverified. For closed networks only.
    Offline,
    /// Check names that belong to an account, let other names in unverified.
    Hybrid,
}

impl AuthMode {
    /// The name used in the file.
    pub fn name(self) -> &'static str {
        match self {
            AuthMode::Online => "online",
            AuthMode::Offline => "offline",
            AuthMode::Hybrid => "hybrid",
        }
    }
}

/// Everything the proxy reads from its configuration. Its `Debug` output hides
/// the forwarding secret, so a configuration can be logged safely.
#[derive(Clone, PartialEq, Eq)]
pub struct Config {
    /// The address players connect to.
    pub listen: SocketAddr,
    /// The servers behind the proxy.
    pub backends: Vec<Backend>,
    /// The backend a player is sent to when they log in.
    pub default_backend: String,
    /// The line shown in the server list.
    pub motd: String,
    /// Whether players are checked against the session service. Kept for the code
    /// that reads it today: it is `false` exactly when `auth_mode` is
    /// [`AuthMode::Offline`], and [`Config::validate`] refuses any other pairing.
    pub online_mode: bool,
    /// Packets of at least this many bytes are compressed; a negative value turns
    /// compression off.
    pub compression_threshold: i32,
    /// Milliseconds allowed to reach a backend.
    pub connect_timeout_ms: u64,
    /// The secret shared with the backends, which use it to check that a login
    /// really comes from the proxy (Velocity modern forwarding). Empty by default,
    /// and required as soon as a backend is configured.
    pub forwarding_secret: String,
    /// Backends tried in this order when the default backend cannot be reached
    /// (`try` in the file). Empty by default.
    pub try_backends: Vec<String>,
    /// The most players the proxy takes at once, shown in the server list; 0 means
    /// no limit.
    pub max_players: u32,
    /// Host names that choose the backend, in place of the default backend.
    pub forced_hosts: Vec<ForcedHost>,
    /// How players are logged in. Online by default.
    pub auth_mode: AuthMode,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: SocketAddr::from(([0, 0, 0, 0], 25565)),
            backends: Vec::new(),
            default_backend: String::new(),
            motd: "A Meridian proxy".to_owned(),
            online_mode: true,
            compression_threshold: 256,
            connect_timeout_ms: 5_000,
            forwarding_secret: String::new(),
            try_backends: Vec::new(),
            max_players: 20,
            forced_hosts: Vec::new(),
            auth_mode: AuthMode::Online,
        }
    }
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Taking every field apart makes a new field a compile error here, so it
        // cannot be left out, or shown, by accident.
        let Config {
            listen,
            backends,
            default_backend,
            motd,
            online_mode,
            compression_threshold,
            connect_timeout_ms,
            forwarding_secret,
            try_backends,
            max_players,
            forced_hosts,
            auth_mode,
        } = self;
        let secret = if forwarding_secret.is_empty() {
            "<empty>"
        } else {
            "<hidden>"
        };
        f.debug_struct("Config")
            .field("listen", listen)
            .field("backends", backends)
            .field("default_backend", default_backend)
            .field("motd", motd)
            .field("online_mode", online_mode)
            .field("compression_threshold", compression_threshold)
            .field("connect_timeout_ms", connect_timeout_ms)
            .field("forwarding_secret", &format_args!("{secret}"))
            .field("try_backends", try_backends)
            .field("max_players", max_players)
            .field("forced_hosts", forced_hosts)
            .field("auth_mode", auth_mode)
            .finish()
    }
}

/// The file as written. Every setting but the backends may be left out.
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct File {
    listen: Option<String>,
    #[serde(default)]
    backends: Vec<FileBackend>,
    default_backend: Option<String>,
    motd: Option<String>,
    online_mode: Option<bool>,
    compression_threshold: Option<i32>,
    connect_timeout_ms: Option<u64>,
    forwarding_secret: Option<String>,
    #[serde(default, rename = "try")]
    try_backends: Vec<String>,
    max_players: Option<u32>,
    #[serde(default, rename = "forced-host")]
    forced_hosts: Vec<FileForcedHost>,
    auth_mode: Option<AuthMode>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileForcedHost {
    host: String,
    backend: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileBackend {
    name: String,
    address: String,
}

impl Config {
    /// Reads, completes and checks the configuration file at `path`.
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_owned(),
            source,
        })?;
        Config::from_toml_str(&text)
    }

    /// Reads, completes and checks a configuration written in TOML. A setting that
    /// is left out takes its value from [`Config::default`], and a missing
    /// `default-backend` is the first backend.
    pub fn from_toml_str(text: &str) -> Result<Config, ConfigError> {
        let file: File = toml::from_str(text).map_err(|e| ConfigError::Parse(e.to_string()))?;
        let defaults = Config::default();
        let listen = match file.listen {
            Some(value) => value
                .parse()
                .map_err(|_| ConfigError::InvalidListen(value))?,
            None => defaults.listen,
        };
        let backends: Vec<Backend> = file
            .backends
            .into_iter()
            .map(|b| Backend {
                name: b.name,
                address: b.address,
            })
            .collect();
        let default_backend = match file.default_backend {
            Some(name) => name,
            None => backends.first().map(|b| b.name.clone()).unwrap_or_default(),
        };
        let auth_mode = auth_mode(file.online_mode, file.auth_mode)?;
        let config = Config {
            listen,
            backends,
            default_backend,
            motd: file.motd.unwrap_or(defaults.motd),
            online_mode: auth_mode != AuthMode::Offline,
            compression_threshold: file
                .compression_threshold
                .unwrap_or(defaults.compression_threshold),
            connect_timeout_ms: file
                .connect_timeout_ms
                .unwrap_or(defaults.connect_timeout_ms),
            forwarding_secret: file.forwarding_secret.unwrap_or(defaults.forwarding_secret),
            try_backends: file.try_backends,
            max_players: file.max_players.unwrap_or(defaults.max_players),
            // Host names do not depend on case.
            forced_hosts: file
                .forced_hosts
                .into_iter()
                .map(|f| ForcedHost {
                    host: f.host.to_ascii_lowercase(),
                    backend: f.backend,
                })
                .collect(),
            auth_mode,
        };
        config.validate()?;
        Ok(config)
    }

    /// Checks what the types alone cannot: there is at least one backend, names
    /// are unique and not empty, addresses are `host:port`, the default backend
    /// exists, the fallback list names other backends once each, the connect
    /// timeout is not zero, the player limit fits the server list and the
    /// forwarding secret is set. The first problem found is returned.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.backends.is_empty() {
            return Err(ConfigError::NoBackend);
        }
        for (i, backend) in self.backends.iter().enumerate() {
            if backend.name.trim().is_empty() {
                return Err(ConfigError::EmptyBackendName);
            }
            if self.backends[..i].iter().any(|b| b.name == backend.name) {
                return Err(ConfigError::DuplicateBackend(backend.name.clone()));
            }
            check_address(&backend.address).map_err(|reason| ConfigError::InvalidAddress {
                backend: backend.name.clone(),
                address: backend.address.clone(),
                reason,
            })?;
        }
        if self.backend(&self.default_backend).is_none() {
            return Err(ConfigError::UnknownDefaultBackend(
                self.default_backend.clone(),
            ));
        }
        for (i, name) in self.try_backends.iter().enumerate() {
            self.known_backend("try", name)?;
            if *name == self.default_backend {
                return Err(ConfigError::DefaultInTry(name.clone()));
            }
            if self.try_backends[..i].contains(name) {
                return Err(ConfigError::DuplicateTry(name.clone()));
            }
        }
        for (i, forced) in self.forced_hosts.iter().enumerate() {
            check_host(&forced.host).map_err(|reason| ConfigError::InvalidForcedHost {
                host: forced.host.clone(),
                reason,
            })?;
            if self.forced_hosts[..i]
                .iter()
                .any(|f| f.host.eq_ignore_ascii_case(&forced.host))
            {
                return Err(ConfigError::DuplicateForcedHost(forced.host.clone()));
            }
            self.known_backend(&format!("forced-host \"{}\"", forced.host), &forced.backend)?;
        }
        if self.online_mode != (self.auth_mode != AuthMode::Offline) {
            return Err(ConfigError::AuthModeConflict {
                online_mode: self.online_mode,
                auth_mode: self.auth_mode,
            });
        }
        if self.connect_timeout_ms == 0 {
            return Err(ConfigError::ZeroConnectTimeout);
        }
        // The server list carries the limit as a signed 32-bit number.
        if i32::try_from(self.max_players).is_err() {
            return Err(ConfigError::MaxPlayersTooLarge(self.max_players));
        }
        if self.forwarding_secret.is_empty() {
            return Err(ConfigError::MissingForwardingSecret);
        }
        Ok(())
    }

    /// The backend with this name.
    pub fn backend(&self, name: &str) -> Option<&Backend> {
        self.backends.iter().find(|b| b.name == name)
    }

    /// The backend forced for the host name a client put in its handshake, if any.
    /// Case does not matter, and a final dot is ignored.
    pub fn forced_backend(&self, host: &str) -> Option<&str> {
        let host = host.strip_suffix('.').unwrap_or(host);
        self.forced_hosts
            .iter()
            .find(|f| f.host.eq_ignore_ascii_case(host))
            .map(|f| f.backend.as_str())
    }

    /// Refuses a reference to a backend that is not configured; `setting` says
    /// where the reference was written.
    fn known_backend(&self, setting: &str, name: &str) -> Result<(), ConfigError> {
        match self.backend(name) {
            Some(_) => Ok(()),
            None => Err(ConfigError::UnknownBackend {
                setting: setting.to_owned(),
                name: name.to_owned(),
            }),
        }
    }
}

/// The login mode from the two settings that can give it. `auth-mode` is the
/// setting to use; `online-mode` is the older one and stays accepted: alone, `true`
/// means online and `false` offline. When both are written they must agree, and
/// `online-mode` cannot be written with `hybrid`.
fn auth_mode(
    online_mode: Option<bool>,
    auth_mode: Option<AuthMode>,
) -> Result<AuthMode, ConfigError> {
    match (online_mode, auth_mode) {
        (None, None) => Ok(AuthMode::default()),
        (Some(true), None) => Ok(AuthMode::Online),
        (Some(false), None) => Ok(AuthMode::Offline),
        (None, Some(mode)) => Ok(mode),
        (Some(true), Some(AuthMode::Online)) => Ok(AuthMode::Online),
        (Some(false), Some(AuthMode::Offline)) => Ok(AuthMode::Offline),
        (Some(online_mode), Some(auth_mode)) => Err(ConfigError::AuthModeConflict {
            online_mode,
            auth_mode,
        }),
    }
}

/// Checks a forced host: a bare host name, without a port or a path.
fn check_host(host: &str) -> Result<(), &'static str> {
    if host.is_empty() {
        return Err("it is empty");
    }
    if host.chars().any(char::is_whitespace) {
        return Err("it contains a space");
    }
    if host.contains(':') {
        return Err("it has a port: the port players use does not choose the backend");
    }
    if host.contains('/') {
        return Err("it has a path: write the host name alone");
    }
    if host.ends_with('.') {
        return Err("it ends with a dot: write it without the final dot");
    }
    Ok(())
}

/// Checks that `address` is `host:port` without resolving the host: the host is
/// a name, an IPv4 address or a bracketed IPv6 address, and the port is 1 to 65535.
fn check_address(address: &str) -> Result<(), &'static str> {
    if address.chars().any(char::is_whitespace) {
        return Err("it contains a space");
    }
    let Some((host, port)) = address.rsplit_once(':') else {
        return Err("it needs a port, as host:port");
    };
    if host.is_empty() {
        return Err("the host is empty");
    }
    if host.starts_with('[') != host.ends_with(']') {
        return Err("an IPv6 host must be written in brackets, as [::1]:25566");
    }
    if host.starts_with('[') {
        if host[1..host.len() - 1]
            .parse::<std::net::Ipv6Addr>()
            .is_err()
        {
            return Err("the host in brackets is not an IPv6 address");
        }
    } else if host.contains(':') {
        return Err("an IPv6 host must be written in brackets, as [::1]:25566");
    }
    match port.parse::<u16>() {
        Ok(0) | Err(_) => Err("the port must be a number from 1 to 65535"),
        Ok(_) => Ok(()),
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod routing_tests;
