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
    /// `[auth] negative-cache-seconds` is longer than `lookup-cache-seconds`.
    NegativeCacheLonger { positive: u64, negative: u64 },
    /// `[auth] lookups-per-minute` is zero.
    ZeroLookupsPerMinute,
    /// A backend has a `sidecar` and no `sidecar-secret-file`.
    SidecarWithoutSecret(String),
    /// A backend has a `sidecar-secret-file` and no `sidecar`.
    SecretWithoutSidecar(String),
    /// A sidecar's address is not `host:port`.
    InvalidSidecarAddress {
        backend: String,
        address: String,
        reason: &'static str,
    },
    /// A sidecar's secret file cannot be read or is too short.
    SidecarSecret { backend: String, reason: String },
    /// Two backends name the same sidecar address.
    SharedSidecarAddress {
        first: String,
        second: String,
        address: String,
    },
    /// Two backends name the same secret file.
    SharedSidecarSecret {
        first: String,
        second: String,
        path: std::path::PathBuf,
    },
    /// `sidecar-timeout-ms` is zero.
    ZeroSidecarTimeout,
    /// `status-sample` is above [`crate::MAX_STATUS_SAMPLE`].
    StatusSampleTooLarge(u32),
    /// `[commands] enabled` lists a command twice.
    DuplicateCommand(&'static str),
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
    /// A forced host is not a bare host name.
    InvalidForcedHost { host: String, reason: &'static str },
    /// Two forced hosts have the same host name.
    DuplicateForcedHost(String),
    /// A group has an empty name.
    EmptyGroupName,
    /// Two groups have the same name.
    DuplicateGroup(String),
    /// A group lists no backend.
    EmptyGroup(String),
    /// A group lists a backend twice.
    DuplicateInGroup { group: String, backend: String },
    /// A backend is in a verified group and in an unverified group.
    MixedAuthBackend {
        backend: String,
        verified_group: String,
        unverified_group: String,
    },
    /// Two routes are for the same kind of player.
    DuplicateRoute(crate::AuthKind),
    /// A route names a group that is not configured.
    UnknownRouteGroup {
        auth: crate::AuthKind,
        group: String,
    },
    /// A route leads to a group of the other kind.
    RouteAuthMismatch {
        auth: crate::AuthKind,
        group: String,
        group_auth: crate::AuthKind,
    },
    /// The login mode lets in a kind of player that no route covers.
    MissingRoute {
        auth: crate::AuthKind,
        mode: crate::AuthMode,
    },
    /// `online-mode` and `auth-mode` say different things.
    AuthModeConflict {
        online_mode: bool,
        auth_mode: crate::AuthMode,
    },
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
            ConfigError::NegativeCacheLonger { positive, negative } => write!(
                f,
                "[auth] negative-cache-seconds = {negative} is longer than lookup-cache-seconds = \
                 {positive}: a free name can become an account's at any time, so \"no account\" \
                 is remembered for less time than \"account\""
            ),
            ConfigError::DuplicateCommand(name) => {
                write!(f, "[commands] enabled lists \"{name}\" twice")
            }
            ConfigError::SidecarWithoutSecret(backend) => write!(
                f,
                "backend \"{backend}\" has a sidecar but no sidecar-secret-file: the secret \
                 is kept in a file of its own, never in the configuration"
            ),
            ConfigError::SecretWithoutSidecar(backend) => write!(
                f,
                "backend \"{backend}\" has a sidecar-secret-file but no sidecar address"
            ),
            ConfigError::InvalidSidecarAddress {
                backend,
                address,
                reason,
            } => write!(
                f,
                "backend \"{backend}\" has the sidecar address \"{address}\", which is not valid: {reason}"
            ),
            ConfigError::SidecarSecret { backend, reason } => {
                write!(f, "the sidecar secret of backend \"{backend}\": {reason}")
            }
            ConfigError::SharedSidecarAddress {
                first,
                second,
                address,
            } => write!(
                f,
                "backends \"{first}\" and \"{second}\" both have the sidecar {address}: each \
                 backend has a sidecar of its own"
            ),
            ConfigError::SharedSidecarSecret {
                first,
                second,
                path,
            } => write!(
                f,
                "backends \"{first}\" and \"{second}\" both use the secret file {}: each \
                 sidecar has a secret of its own",
                path.display()
            ),
            ConfigError::ZeroSidecarTimeout => {
                write!(f, "sidecar-timeout-ms must be more than 0")
            }
            ConfigError::StatusSampleTooLarge(n) => write!(
                f,
                "status-sample = {n} is too large: the server list shows at most {} names",
                crate::MAX_STATUS_SAMPLE
            ),
            ConfigError::ZeroLookupsPerMinute => {
                write!(f, "[auth] lookups-per-minute must be at least 1")
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
            ConfigError::InvalidForcedHost { host, reason } => {
                write!(f, "forced-host \"{host}\" is not valid: {reason}")
            }
            ConfigError::EmptyGroupName => write!(f, "a [[group]] has an empty name"),
            ConfigError::DuplicateGroup(name) => {
                write!(f, "two groups are named \"{name}\": names must be unique")
            }
            ConfigError::EmptyGroup(name) => {
                write!(f, "group \"{name}\" lists no backend")
            }
            ConfigError::DuplicateInGroup { group, backend } => {
                write!(f, "group \"{group}\" lists \"{backend}\" twice")
            }
            ConfigError::MixedAuthBackend {
                backend,
                verified_group,
                unverified_group,
            } => write!(
                f,
                "backend \"{backend}\" is in the verified group \"{verified_group}\" and in the \
                 unverified group \"{unverified_group}\": a backend serves one kind of player, \
                 because a verified and an unverified player get different unique ids and the \
                 server would keep two files for one name"
            ),
            ConfigError::DuplicateRoute(auth) => {
                write!(
                    f,
                    "two [[route]] entries are for auth = \"{}\"",
                    auth.name()
                )
            }
            ConfigError::UnknownRouteGroup { auth, group } => write!(
                f,
                "the route for auth = \"{}\" names group \"{group}\", which is not configured",
                auth.name()
            ),
            ConfigError::RouteAuthMismatch {
                auth,
                group,
                group_auth,
            } => write!(
                f,
                "the route for auth = \"{}\" leads to group \"{group}\", which is for {} players",
                auth.name(),
                group_auth.name()
            ),
            ConfigError::MissingRoute { auth, mode } => write!(
                f,
                "auth-mode = \"{}\" lets {} players in, but no [[route]] says where they go: \
                 add a route with auth = \"{}\"",
                mode.name(),
                auth.name(),
                auth.name()
            ),
            ConfigError::AuthModeConflict {
                online_mode,
                auth_mode,
            } => write!(
                f,
                "online-mode = {online_mode} and auth-mode = \"{}\" disagree: online-mode is \
                 the older setting, so remove it and keep auth-mode",
                auth_mode.name()
            ),
            ConfigError::DuplicateForcedHost(host) => write!(
                f,
                "two [[forced-host]] entries have the host \"{host}\": each host leads to one backend"
            ),
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
