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
    /// A backend has `sidecar-notices` and no `sidecar`.
    NoticesWithoutSidecar(String),
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
    /// `switch-freeze-limit-ms` is zero.
    ZeroFreezeLimit,
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
    /// A backend has a `soft-cap` or a `hard-cap` and no `role`.
    LimitWithoutRole { backend: String, key: &'static str },
    /// A backend has a `role` and no `soft-cap`.
    RoleWithoutSoftCap(String),
    /// A backend's `soft-cap` is zero.
    ZeroSoftCap(String),
    /// A backend's `hard-cap` is below its `soft-cap`.
    HardCapBelowSoftCap {
        backend: String,
        soft_cap: u32,
        hard_cap: u32,
    },
    /// Backends have a role, and none is a lobby.
    NoLobby,
    /// `[balancing] enabled = true` while no backend has a role.
    BalancingWithoutRoles,
    /// `[balancing] state-max-age-seconds` is zero.
    ZeroStateMaxAge,
    /// `[balancing] probe-interval-seconds` is zero.
    ZeroProbeInterval,
    /// `[restart] silent-rounds` is zero.
    ZeroSilentRounds,
    /// `[restart] grace-seconds` is zero.
    ZeroRestartGrace,
    /// `[fallback] max-attempts` is zero.
    ZeroFallbackAttempts,
    /// `[proxy-protocol] mode` is `optional` or `required` and `trusted` is empty.
    ProxyProtocolWithoutTrusted,
    /// An entry of `[proxy-protocol] trusted` is not an address or a range.
    InvalidTrustedAddress { entry: String, reason: String },
    /// `[proxy-protocol] header-timeout-ms` is zero.
    ZeroProxyProtocolTimeout,
    /// `[metrics] bind` is not an address with a port.
    InvalidMetricsBind(String),
    /// `[metrics]` is on and listens where the proxy takes players.
    MetricsOnListen(std::net::SocketAddr),
    /// `[operators] min-level` is above the highest permission level.
    OperatorLevelTooHigh(u8),
    /// `[operators] group` is not a group name.
    BadOperatorGroup(String),
    /// `[operators] stale-after-seconds` is zero or too large.
    OperatorStaleOutOfRange(u64),
    /// `[operators] from` names a backend that has no sidecar to be read through.
    OperatorSourceWithoutSidecar(String),
    /// `[operators] from` names a backend twice.
    DuplicateOperatorSource(String),
    /// `[queue] wait-seconds` is zero.
    ZeroQueueWait,
    /// `[queue] max-length` is zero.
    ZeroQueueLength,
    /// `[queue] wait-seconds` leaves the client waiting too long in silence.
    QueueWaitTooLong {
        wait_seconds: u64,
        connect_timeout_ms: u64,
        /// What the extensions' login decisions may take, 0 without any.
        extension_ms: u64,
    },
    /// `favicon` names a file that is not a usable 64x64 PNG.
    InvalidFavicon {
        path: std::path::PathBuf,
        reason: String,
    },
    /// An `[[extension]]` has a name that cannot be used in logs, metrics and node names.
    InvalidExtensionName { name: String, reason: &'static str },
    /// Two extensions have the same name.
    DuplicateExtension(String),
    /// An extension's address is not `host:port` on the loopback.
    InvalidExtensionAddress {
        extension: String,
        address: String,
        reason: &'static str,
    },
    /// Two extensions name the same address.
    SharedExtensionAddress {
        first: String,
        second: String,
        address: String,
    },
    /// Two extensions name the same token file.
    SharedExtensionToken {
        first: String,
        second: String,
        path: std::path::PathBuf,
    },
    /// The token file of an enabled extension cannot be read or is too short.
    ExtensionToken { extension: String, reason: String },
    /// `grant` names a capability that does not exist.
    UnknownCapability {
        extension: String,
        capability: String,
    },
    /// `grant` names a capability twice.
    DuplicateCapability {
        extension: String,
        capability: String,
    },
    /// A decision point is granted to two extensions that are enabled.
    SharedDecisionPoint {
        capability: &'static str,
        first: String,
        second: String,
    },
    /// A `decide-*` table is written for an extension that is not granted its capability.
    DecideWithoutGrant {
        extension: String,
        table: &'static str,
        capability: &'static str,
    },
    /// `[extension.orders]` is written for an extension that is granted no order, and not `decide.command`.
    OrdersWithoutGrant { extension: String },
    /// `[extension.decide-command] on-error = "allow"`: with no usable answer it would give every command
    /// of the proxy to whoever types it.
    ExtensionCommandAllow { extension: String },
    /// A number of an extension that must be more than 0 is 0.
    ZeroExtensionSetting {
        extension: String,
        setting: &'static str,
    },
    /// An extension granted `commands` has too few places for its commands and its decisions.
    CommandsNeedPlaces { extension: String },
    /// A number of an extension is above the most it may be.
    ExtensionSettingAbove {
        extension: String,
        setting: &'static str,
        most: u64,
    },
    /// With the queue off, the extensions' login decisions and the connection to a backend take
    /// longer than a client waits in silence.
    ExtensionBudget {
        extension_ms: u64,
        connect_timeout_ms: u64,
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
            ConfigError::NoticesWithoutSidecar(backend) => write!(
                f,
                "backend \"{backend}\" has sidecar-notices but no sidecar address"
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
            ConfigError::ZeroFreezeLimit => {
                write!(f, "switch-freeze-limit-ms must be more than 0")
            }
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
            ConfigError::LimitWithoutRole { backend, key } => write!(
                f,
                "backend \"{backend}\" has {key} but no role: a limit applies to a backend with \
                 a role (lobby, sublobby or game)"
            ),
            ConfigError::RoleWithoutSoftCap(backend) => write!(
                f,
                "backend \"{backend}\" has a role but no soft-cap: the soft cap is the number \
                 of players it is filled to before the next server of its role is opened"
            ),
            ConfigError::ZeroSoftCap(backend) => {
                write!(
                    f,
                    "backend \"{backend}\" has a soft-cap of 0: it must be at least 1"
                )
            }
            ConfigError::HardCapBelowSoftCap {
                backend,
                soft_cap,
                hard_cap,
            } => write!(
                f,
                "backend \"{backend}\" has hard-cap = {hard_cap} below soft-cap = {soft_cap}: \
                 the hard cap is the most it ever takes, so it cannot be lower"
            ),
            ConfigError::NoLobby => write!(
                f,
                "backends have roles but none has role = \"lobby\": players need a lobby to \
                 arrive in"
            ),
            ConfigError::BalancingWithoutRoles => write!(
                f,
                "[balancing] enabled = true, but no backend has a role: give the backends a                  role and a soft-cap, or turn balancing off"
            ),
            ConfigError::ZeroStateMaxAge => {
                write!(f, "[balancing] state-max-age-seconds must be more than 0")
            }
            ConfigError::ZeroProbeInterval => {
                write!(f, "[balancing] probe-interval-seconds must be more than 0")
            }
            ConfigError::ZeroSilentRounds => {
                write!(f, "[restart] silent-rounds must be more than 0")
            }
            ConfigError::ZeroRestartGrace => {
                write!(f, "[restart] grace-seconds must be more than 0")
            }
            ConfigError::ZeroFallbackAttempts => {
                write!(f, "[fallback] max-attempts must be more than 0")
            }
            ConfigError::ProxyProtocolWithoutTrusted => write!(
                f,
                "[proxy-protocol] mode is not \"off\", but trusted is empty: list the addresses \
                 of the load balancers that send the header, because anyone who may send it can \
                 claim any address"
            ),
            ConfigError::InvalidTrustedAddress { entry, reason } => {
                write!(f, "[proxy-protocol] trusted has {entry:?}: {reason}")
            }
            ConfigError::ZeroProxyProtocolTimeout => {
                write!(f, "[proxy-protocol] header-timeout-ms must be more than 0")
            }
            ConfigError::InvalidMetricsBind(value) => write!(
                f,
                "[metrics] bind = \"{value}\" is not a host:port address with a port above 0"
            ),
            ConfigError::MetricsOnListen(address) => write!(
                f,
                "[metrics] bind = \"{address}\" is where the proxy takes players (listen): \
                 give the metrics a port of their own"
            ),
            ConfigError::OperatorLevelTooHigh(level) => write!(
                f,
                "[operators] min-level = {level} is above {}, the highest permission level",
                crate::operators::MAX_OPERATOR_LEVEL
            ),
            ConfigError::BadOperatorGroup(why) => write!(f, "[operators] group: {why}"),
            ConfigError::OperatorStaleOutOfRange(seconds) => write!(
                f,
                "[operators] stale-after-seconds = {seconds} must be between 1 and {}",
                crate::operators::MAX_OPERATOR_STALE_SECONDS
            ),
            ConfigError::OperatorSourceWithoutSidecar(name) => write!(
                f,
                "[operators] from names \"{name}\", which has no [[sidecars]] entry: its operators \
                 are read through its sidecar"
            ),
            ConfigError::DuplicateOperatorSource(name) => {
                write!(f, "[operators] from names \"{name}\" twice")
            }
            ConfigError::ZeroQueueWait => write!(f, "[queue] wait-seconds must be more than 0"),
            ConfigError::ZeroQueueLength => write!(f, "[queue] max-length must be more than 0"),
            ConfigError::QueueWaitTooLong {
                wait_seconds,
                connect_timeout_ms,
                extension_ms,
            } => write!(
                f,
                "[queue] wait-seconds = {wait_seconds} is too long: the player's client sees nothing                  from the proxy while it waits and gives up after 30 seconds, so the wait (at most                  {}) plus connect-timeout-ms ({connect_timeout_ms} ms){} must stay under {} ms",
                crate::queue::MAX_QUEUE_WAIT_SECONDS,
                if *extension_ms > 0 {
                    format!(
                        " and the timeouts of the extensions' decide-login and decide-server ({extension_ms} ms)"
                    )
                } else {
                    String::new()
                },
                crate::queue::QUEUE_SILENCE_LIMIT_MS
            ),
            ConfigError::InvalidFavicon { path, reason } => {
                write!(f, "favicon {}: {reason}", path.display())
            }
            ConfigError::InvalidExtensionName { name, reason } => {
                write!(f, "[[extension]] name \"{name}\": {reason}")
            }
            ConfigError::DuplicateExtension(name) => {
                write!(
                    f,
                    "[[extension]] \"{name}\" is written twice (names are compared without regard to case: \"Perms\" and \"perms\" are the same)"
                )
            }
            ConfigError::InvalidExtensionAddress {
                extension,
                address,
                reason,
            } => write!(
                f,
                "extension \"{extension}\": address \"{address}\": {reason}"
            ),
            ConfigError::SharedExtensionAddress {
                first,
                second,
                address,
            } => write!(
                f,
                "extensions \"{first}\" and \"{second}\" both use the address {address}"
            ),
            ConfigError::SharedExtensionToken {
                first,
                second,
                path,
            } => write!(
                f,
                "extensions \"{first}\" and \"{second}\" both use the token file {}: each needs a token of its own",
                path.display()
            ),
            ConfigError::ExtensionToken { extension, reason } => {
                write!(f, "extension \"{extension}\": token-file: {reason}")
            }
            ConfigError::UnknownCapability {
                extension,
                capability,
            } => write!(
                f,
                "extension \"{extension}\": grant names \"{capability}\", which is not a capability (see docs/EXTENSIONS.md, 2.3)"
            ),
            ConfigError::DuplicateCapability {
                extension,
                capability,
            } => write!(
                f,
                "extension \"{extension}\": grant names \"{capability}\" twice"
            ),
            ConfigError::SharedDecisionPoint {
                capability,
                first,
                second,
            } => write!(
                f,
                "extensions \"{first}\" and \"{second}\" are both granted {capability}: a decision point has one extension at most"
            ),
            ConfigError::DecideWithoutGrant {
                extension,
                table,
                capability,
            } => write!(
                f,
                "extension \"{extension}\": [extension.{table}] is written and grant does not hold \"{capability}\""
            ),
            ConfigError::OrdersWithoutGrant { extension } => write!(
                f,
                "extension \"{extension}\": [extension.orders] is written and grant holds no \"orders.*\" and not \"decide.command\""
            ),
            ConfigError::ExtensionCommandAllow { extension } => write!(
                f,
                "extension \"{extension}\": [extension.decide-command] on-error = \"allow\" would give every command of the proxy to anyone while the extension is down: use \"default\" or \"deny\""
            ),
            ConfigError::ZeroExtensionSetting { extension, setting } => {
                write!(
                    f,
                    "extension \"{extension}\": {setting} must be more than 0"
                )
            }
            ConfigError::CommandsNeedPlaces { extension } => write!(
                f,
                "extension \"{extension}\": max-in-flight must be at least 2 when \"commands\" is granted: its commands use at most half of its places, so that its decisions keep one"
            ),
            ConfigError::ExtensionSettingAbove {
                extension,
                setting,
                most,
            } => write!(
                f,
                "extension \"{extension}\": {setting} must be at most {most}"
            ),
            ConfigError::ExtensionBudget {
                extension_ms,
                connect_timeout_ms,
            } => write!(
                f,
                "the timeouts of the extensions' decide-login and decide-server ({extension_ms} ms) plus connect-timeout-ms ({connect_timeout_ms} ms) must stay under {} ms: the player's client sees nothing from the proxy while they run and gives up after 30 seconds",
                crate::queue::QUEUE_SILENCE_LIMIT_MS
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
