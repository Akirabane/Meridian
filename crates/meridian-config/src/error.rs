//! What can be wrong with a configuration, worded for the person who wrote it.
use std::fmt;
use std::path::PathBuf;

/// Why a configuration could not be used.
#[derive(Debug)]
pub enum ConfigError {
    /// The file could not be read.
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The file is not valid TOML, or a setting has the wrong type or an unknown
    /// name. The message comes from the TOML reader and gives the line.
    Parse(String),
    /// `listen` is not an `ip:port` address.
    InvalidListen(String),
    /// No backend is configured.
    NoBackend,
    /// A backend has an empty name.
    EmptyBackendName,
    /// Two backends have the same name.
    DuplicateBackend(String),
    /// A backend's address is not `host:port`.
    InvalidAddress {
        backend: String,
        address: String,
        reason: &'static str,
    },
    /// `default-backend` names no configured backend.
    UnknownDefaultBackend(String),
    /// `connect-timeout-ms` is zero.
    ZeroConnectTimeout,
    /// `forwarding-secret` is empty while backends are configured.
    MissingForwardingSecret,
    /// A setting names a backend that is not configured. `setting` says where,
    /// such as `try` or `group "verified"`.
    UnknownBackend { setting: String, name: String },
    /// `try` lists the default backend, which is always tried first anyway.
    DefaultInTry(String),
    /// `try` lists a backend twice.
    DuplicateTry(String),
    /// `max-players` does not fit the server list's signed 32-bit number.
    MaxPlayersTooLarge(u32),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Read { path, source } => {
                write!(f, "cannot read {}: {source}", path.display())
            }
            ConfigError::Parse(message) => write!(f, "{message}"),
            ConfigError::InvalidListen(value) => write!(
                f,
                "listen = \"{value}\" is not an address with a port, such as \"0.0.0.0:25565\""
            ),
            ConfigError::NoBackend => {
                write!(
                    f,
                    "no backend is configured: add at least one [[backends]] entry"
                )
            }
            ConfigError::EmptyBackendName => write!(f, "a backend has an empty name"),
            ConfigError::DuplicateBackend(name) => {
                write!(f, "two backends are named \"{name}\": names must be unique")
            }
            ConfigError::InvalidAddress {
                backend,
                address,
                reason,
            } => write!(
                f,
                "backend \"{backend}\" has the address \"{address}\", which is not valid: {reason}"
            ),
            ConfigError::UnknownDefaultBackend(name) => write!(
                f,
                "default-backend = \"{name}\" names no configured backend"
            ),
            ConfigError::ZeroConnectTimeout => {
                write!(f, "connect-timeout-ms must be more than 0")
            }
            ConfigError::MissingForwardingSecret => {
                write!(f, "forwarding-secret must be set: the backends check it")
            }
            ConfigError::UnknownBackend { setting, name } => {
                write!(
                    f,
                    "{setting} names \"{name}\", which is not a configured backend"
                )
            }
            ConfigError::DefaultInTry(name) => write!(
                f,
                "try lists \"{name}\", the default backend: it is always tried first, so \
                 try lists only the backends that come after it"
            ),
            ConfigError::DuplicateTry(name) => write!(f, "try lists \"{name}\" twice"),
            ConfigError::MaxPlayersTooLarge(n) => write!(
                f,
                "max-players = {n} is too large: the most is {}, and 0 means no limit",
                i32::MAX
            ),
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ConfigError::Read { source, .. } => Some(source),
            _ => None,
        }
    }
}
