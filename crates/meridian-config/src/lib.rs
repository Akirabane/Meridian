//! The configuration of the proxy: where it listens, which backends it knows, and
//! the limits it applies. [`Config::load`] reads it from a TOML file, fills in the
//! defaults and checks it; `meridian.example.toml` at the root of the repository
//! shows every setting.
mod access;
mod auth;
mod balancing;
mod commands;
mod error;
mod extension;
mod fallback;
mod favicon;
mod metrics;
mod operators;
mod permissions;
mod proxy_protocol;
mod queue;
mod restart;
mod sidecar;

pub use access::{Access, AccessError, AccessSettings, AccessSync, Ban, Net, Refusal, Target};
pub use auth::{AuthSettings, OnLookupFailure};
pub use balancing::{BalancedServer, Balancing, Role, TieBreak};
pub use commands::{CommandSettings, ProxyCommand};
pub use error::ConfigError;
pub use extension::{
    CAPABILITIES, COMMANDS, CommandDecision, DECIDE_COMMAND, DECIDE_LOGIN, DECIDE_SERVER,
    Extension, LoginDecision, MAX_EXTENSION_NAME, MIN_EXTENSION_TOKEN, OnError, Orders,
    OwnCommands, ServerDecision, ServerOnError, opens_orders,
};
pub use fallback::Fallback;
pub use favicon::{FAVICON_SIDE, Favicon, MAX_FAVICON_BYTES};
pub use metrics::Metrics;
pub use operators::{MAX_OPERATOR_LEVEL, MAX_OPERATOR_STALE_SECONDS, OperatorSettings};
pub use permissions::{
    DEFAULT_GROUP, Member, PermissionGroup, PermissionSettings, Permissions, Setting,
    check_group_name, check_node, node,
};
pub use proxy_protocol::{ProxyProtocol, ProxyProtocolMode, TrustedNet};
pub use queue::{MAX_QUEUE_WAIT_SECONDS, QUEUE_SILENCE_LIMIT_MS, QueueSettings};
pub use restart::Restart;
pub use sidecar::{MIN_SIDECAR_SECRET, Sidecar, read_secret};

use access::FileAccess;
use auth::FileAuth;
use balancing::{FileBalancing, FileServer};
use commands::FileCommands;
use extension::FileExtension;
use fallback::FileFallback;
use metrics::FileMetrics;
use operators::FileOperators;
use permissions::FilePermissionSettings;
use proxy_protocol::FileProxyProtocol;
use queue::FileQueue;
use restart::FileRestart;

use serde::Deserialize;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

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
    /// The line the server list shows to clients that connect through this host, in
    /// place of [`Config::motd`]; `None` shows the proxy's.
    pub motd: Option<String>,
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

/// What the proxy knows about a player's account (DESIGN.md section 15.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthKind {
    /// The session service confirmed the account.
    Verified,
    /// The proxy let a name in without that check.
    Unverified,
}

impl AuthKind {
    /// The name used in the file.
    pub fn name(self) -> &'static str {
        match self {
            AuthKind::Verified => "verified",
            AuthKind::Unverified => "unverified",
        }
    }
}

/// Backends for one kind of player (`[[group]]` in the file).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub name: String,
    /// The kind of player this group is for.
    pub auth: AuthKind,
    /// The backends of the group, by name.
    pub backends: Vec<String>,
}

/// Where players of one kind go (`[[route]]` in the file).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Route {
    pub auth: AuthKind,
    /// The group they are sent to.
    pub group: String,
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
    /// The picture the server list shows (`favicon`), already read from its file.
    pub favicon: Option<Favicon>,
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
    /// Groups of backends, each for one kind of player.
    pub groups: Vec<Group>,
    /// Which group each kind of player is sent to. When empty, every player uses
    /// the default backend and `try_backends`.
    pub routes: Vec<Route>,
    /// How the hybrid mode looks names up (`[auth]` in the file).
    pub auth: AuthSettings,
    /// What happens when a player who is already connected logs in again.
    pub on_duplicate: OnDuplicate,
    /// Connections one address may open in a minute, counted before the
    /// handshake; 0 means no limit.
    pub connections_per_minute: u32,
    /// Connections that may be logging in at once, from their accept to the end of
    /// their login; further ones are closed at once. 0 means no limit.
    pub max_pending_logins: u32,
    /// Connections one address may have logging in at once; 0 means no limit,
    /// the default, since many players may share an address (a school, a LAN).
    pub max_pending_logins_per_address: u32,
    /// Seconds a backend that failed to take a player is passed over, so the
    /// next players go straight to the next candidate; 0 never passes one over.
    pub backend_retry_seconds: u64,
    /// How many player names the server list shows.
    pub status_sample: u32,
    /// The commands players type that the proxy answers (`[commands]`).
    pub commands: CommandSettings,
    /// The sidecars of the backends that have one (`sidecar` and
    /// `sidecar-secret-file` in `[[backends]]`). A backend with none cannot be
    /// switched to or from.
    pub sidecars: Vec<Sidecar>,
    /// Milliseconds allowed for a sidecar to answer.
    pub sidecar_timeout_ms: u64,
    /// Whether a switch carries the player's position to the other server, where
    /// their client is at that moment. Without it they arrive where that server
    /// last had them, or at its spawn. It needs servers that take the place in the import
    /// request: Horizon does since #8735, merged into its main on 2026-11-05. It has not been
    /// run against a real server.
    pub switch_carry_position: bool,
    /// Whether the players of the server a player is moved to see them arrive: the
    /// join line and the entry in the player list. With `false` the arrival is quiet.
    pub switch_announce_arrival: bool,
    /// Whether the players of the server a player is moved from see the "left the game"
    /// line. With `false` the departure is quiet.
    pub switch_announce_departure: bool,
    /// Milliseconds a player may stay frozen on the old server during a switch before
    /// it is given up. Each server lets a freeze go by itself after its own
    /// `max_freeze_seconds` (30 s in Horizon by default), and this must be below it.
    pub switch_freeze_limit_ms: u64,
    /// Milliseconds kept in hand against the freeze the old server says is left:
    /// the player is placed on the new server only while the freeze left outlasts
    /// this and the time the last two requests are expected to take.
    pub switch_commit_margin_ms: u64,
    /// The role and limits of the backends that have a role (`role`, `soft-cap`
    /// and `hard-cap` in `[[backends]]`). Read and checked; nothing uses them yet
    /// (docs/LOAD-BALANCING.md, step 1).
    pub balanced: Vec<BalancedServer>,
    /// The `[balancing]` table.
    pub balancing: Balancing,
    /// The `[restart]` table.
    pub restart: Restart,
    /// The `[fallback]` table.
    pub fallback: Fallback,
    /// The `[proxy-protocol]` table.
    pub proxy_protocol: ProxyProtocol,
    /// The `[metrics]` table.
    pub metrics: Metrics,
    /// The `[access]` table.
    pub access: AccessSettings,
    /// The `[permissions]` table.
    pub permissions: PermissionSettings,
    /// The `[queue]` table.
    pub queue: QueueSettings,
    /// The `[operators]` table.
    pub operators: OperatorSettings,
    /// The `[[extension]]` tables: none by default.
    pub extensions: Vec<Extension>,
}

/// The most names the server list may show. The client only draws a short list,
/// and every name is sent to whoever asks for the status.
pub const MAX_STATUS_SAMPLE: u32 = 100;

/// What a second login of a connected player does: the same unique id for a
/// verified player, the same name whatever its case for an unverified one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnDuplicate {
    /// The new login is refused and the session already open goes on.
    #[default]
    Refuse,
    /// The session already open ends and the new login goes on.
    Replace,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: SocketAddr::from(([0, 0, 0, 0], 25565)),
            backends: Vec::new(),
            default_backend: String::new(),
            motd: "A Meridian proxy".to_owned(),
            favicon: None,
            online_mode: true,
            compression_threshold: 256,
            connect_timeout_ms: 5_000,
            forwarding_secret: String::new(),
            try_backends: Vec::new(),
            max_players: 20,
            forced_hosts: Vec::new(),
            auth_mode: AuthMode::Online,
            groups: Vec::new(),
            routes: Vec::new(),
            auth: AuthSettings::default(),
            on_duplicate: OnDuplicate::Refuse,
            connections_per_minute: 20,
            max_pending_logins: 2048,
            max_pending_logins_per_address: 0,
            backend_retry_seconds: 10,
            status_sample: 12,
            commands: CommandSettings::default(),
            sidecars: Vec::new(),
            sidecar_timeout_ms: 5_000,
            switch_carry_position: false,
            switch_announce_arrival: true,
            switch_announce_departure: true,
            switch_freeze_limit_ms: 8_000,
            switch_commit_margin_ms: 2_000,
            balanced: Vec::new(),
            balancing: Balancing::default(),
            restart: Restart::default(),
            fallback: Fallback::default(),
            proxy_protocol: ProxyProtocol::default(),
            metrics: Metrics::default(),
            access: AccessSettings::default(),
            permissions: PermissionSettings::default(),
            queue: QueueSettings::default(),
            operators: OperatorSettings::default(),
            extensions: Vec::new(),
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
            favicon,
            online_mode,
            compression_threshold,
            connect_timeout_ms,
            forwarding_secret,
            try_backends,
            max_players,
            forced_hosts,
            auth_mode,
            groups,
            routes,
            auth,
            on_duplicate,
            connections_per_minute,
            max_pending_logins,
            max_pending_logins_per_address,
            backend_retry_seconds,
            status_sample,
            commands,
            sidecars,
            sidecar_timeout_ms,
            switch_carry_position,
            switch_announce_arrival,
            switch_announce_departure,
            switch_freeze_limit_ms,
            switch_commit_margin_ms,
            balanced,
            balancing,
            restart,
            fallback,
            proxy_protocol,
            metrics,
            access,
            permissions,
            queue,
            operators,
            extensions,
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
            .field("favicon", favicon)
            .field("online_mode", online_mode)
            .field("compression_threshold", compression_threshold)
            .field("connect_timeout_ms", connect_timeout_ms)
            .field("forwarding_secret", &format_args!("{secret}"))
            .field("try_backends", try_backends)
            .field("max_players", max_players)
            .field("forced_hosts", forced_hosts)
            .field("auth_mode", auth_mode)
            .field("groups", groups)
            .field("routes", routes)
            .field("auth", auth)
            .field("on_duplicate", on_duplicate)
            .field("connections_per_minute", connections_per_minute)
            .field("max_pending_logins", max_pending_logins)
            .field(
                "max_pending_logins_per_address",
                max_pending_logins_per_address,
            )
            .field("backend_retry_seconds", backend_retry_seconds)
            .field("status_sample", status_sample)
            .field("commands", commands)
            // The paths of the secret files only: the secrets are never kept.
            .field("sidecars", sidecars)
            .field("sidecar_timeout_ms", sidecar_timeout_ms)
            .field("switch_carry_position", switch_carry_position)
            .field("switch_announce_arrival", switch_announce_arrival)
            .field("switch_announce_departure", switch_announce_departure)
            .field("switch_freeze_limit_ms", switch_freeze_limit_ms)
            .field("switch_commit_margin_ms", switch_commit_margin_ms)
            .field("balanced", balanced)
            .field("balancing", balancing)
            .field("restart", restart)
            .field("fallback", fallback)
            .field("proxy_protocol", proxy_protocol)
            .field("metrics", metrics)
            .field("access", access)
            .field("permissions", permissions)
            .field("queue", queue)
            .field("operators", operators)
            // The paths of the token files only: the tokens are never kept.
            .field("extensions", extensions)
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
    favicon: Option<PathBuf>,
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
    #[serde(default, rename = "group")]
    groups: Vec<FileGroup>,
    #[serde(default, rename = "route")]
    routes: Vec<FileRoute>,
    #[serde(default)]
    auth: FileAuth,
    on_duplicate: Option<OnDuplicate>,
    connections_per_minute: Option<u32>,
    max_pending_logins: Option<u32>,
    max_pending_logins_per_address: Option<u32>,
    backend_retry_seconds: Option<u64>,
    status_sample: Option<u32>,
    #[serde(default)]
    commands: FileCommands,
    sidecar_timeout_ms: Option<u64>,
    switch_carry_position: Option<bool>,
    switch_announce_arrival: Option<bool>,
    switch_announce_departure: Option<bool>,
    switch_freeze_limit_ms: Option<u64>,
    switch_commit_margin_ms: Option<u64>,
    #[serde(default)]
    balancing: FileBalancing,
    #[serde(default)]
    restart: FileRestart,
    #[serde(default)]
    fallback: FileFallback,
    #[serde(default)]
    proxy_protocol: FileProxyProtocol,
    #[serde(default)]
    metrics: FileMetrics,
    #[serde(default)]
    access: FileAccess,
    #[serde(default)]
    permissions: FilePermissionSettings,
    #[serde(default)]
    queue: FileQueue,
    #[serde(default)]
    operators: FileOperators,
    #[serde(default, rename = "extension")]
    extensions: Vec<FileExtension>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileGroup {
    name: String,
    auth: AuthKind,
    backends: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileRoute {
    auth: AuthKind,
    group: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileForcedHost {
    host: String,
    backend: String,
    motd: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct FileBackend {
    name: String,
    address: String,
    sidecar: Option<String>,
    sidecar_secret_file: Option<PathBuf>,
    #[serde(default)]
    sidecar_notices: bool,
    role: Option<Role>,
    soft_cap: Option<u32>,
    hard_cap: Option<u32>,
}

impl Config {
    /// Reads, completes and checks the configuration file at `path`.
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_owned(),
            source,
        })?;
        let folder = path.parent().unwrap_or(Path::new(""));
        Config::from_toml_str_in(&text, folder)
    }

    /// Reads, completes and checks a configuration written in TOML. A setting that
    /// is left out takes its value from [`Config::default`], and a missing
    /// `default-backend` is the first backend. Relative paths, such as a
    /// sidecar's secret file, are taken from the current folder.
    pub fn from_toml_str(text: &str) -> Result<Config, ConfigError> {
        Config::from_toml_str_in(text, Path::new(""))
    }

    /// [`Config::from_toml_str`], with relative paths taken from `folder`, the
    /// folder of the configuration file.
    pub fn from_toml_str_in(text: &str, folder: &Path) -> Result<Config, ConfigError> {
        let file: File = toml::from_str(text).map_err(|e| ConfigError::Parse(e.to_string()))?;
        let defaults = Config::default();
        let listen = match file.listen {
            Some(value) => value
                .parse()
                .map_err(|_| ConfigError::InvalidListen(value))?,
            None => defaults.listen,
        };
        let mut sidecars = Vec::new();
        for b in &file.backends {
            if b.sidecar_notices && b.sidecar.is_none() {
                return Err(ConfigError::NoticesWithoutSidecar(b.name.clone()));
            }
            match (&b.sidecar, &b.sidecar_secret_file) {
                (None, None) => {}
                (Some(address), Some(secret_file)) => sidecars.push(Sidecar {
                    backend: b.name.clone(),
                    address: address.clone(),
                    secret_file: sidecar::resolve(folder, secret_file),
                    notices: b.sidecar_notices,
                }),
                (Some(_), None) => {
                    return Err(ConfigError::SidecarWithoutSecret(b.name.clone()));
                }
                (None, Some(_)) => {
                    return Err(ConfigError::SecretWithoutSidecar(b.name.clone()));
                }
            }
        }
        let balanced = balancing::servers(
            &file
                .backends
                .iter()
                .map(|b| FileServer {
                    name: &b.name,
                    role: b.role,
                    soft_cap: b.soft_cap,
                    hard_cap: b.hard_cap,
                })
                .collect::<Vec<_>>(),
        )?;
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
            favicon: file
                .favicon
                .map(|path| Favicon::read(favicon::resolve(folder, &path)))
                .transpose()?,
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
                    motd: f.motd,
                })
                .collect(),
            auth_mode,
            groups: file
                .groups
                .into_iter()
                .map(|g| Group {
                    name: g.name,
                    auth: g.auth,
                    backends: g.backends,
                })
                .collect(),
            routes: file
                .routes
                .into_iter()
                .map(|r| Route {
                    auth: r.auth,
                    group: r.group,
                })
                .collect(),
            auth: file.auth.into_settings(),
            on_duplicate: file.on_duplicate.unwrap_or(defaults.on_duplicate),
            connections_per_minute: file
                .connections_per_minute
                .unwrap_or(defaults.connections_per_minute),
            max_pending_logins: file
                .max_pending_logins
                .unwrap_or(defaults.max_pending_logins),
            max_pending_logins_per_address: file
                .max_pending_logins_per_address
                .unwrap_or(defaults.max_pending_logins_per_address),
            backend_retry_seconds: file
                .backend_retry_seconds
                .unwrap_or(defaults.backend_retry_seconds),
            status_sample: file.status_sample.unwrap_or(defaults.status_sample),
            commands: file.commands.into_settings(),
            sidecars,
            sidecar_timeout_ms: file
                .sidecar_timeout_ms
                .unwrap_or(defaults.sidecar_timeout_ms),
            switch_carry_position: file
                .switch_carry_position
                .unwrap_or(defaults.switch_carry_position),
            switch_announce_arrival: file
                .switch_announce_arrival
                .unwrap_or(defaults.switch_announce_arrival),
            switch_announce_departure: file
                .switch_announce_departure
                .unwrap_or(defaults.switch_announce_departure),
            switch_freeze_limit_ms: file
                .switch_freeze_limit_ms
                .unwrap_or(defaults.switch_freeze_limit_ms),
            switch_commit_margin_ms: file
                .switch_commit_margin_ms
                .unwrap_or(defaults.switch_commit_margin_ms),
            balanced,
            balancing: file.balancing.into_settings(),
            restart: file.restart.into_settings(),
            fallback: file.fallback.into_settings(),
            proxy_protocol: file.proxy_protocol.into_settings()?,
            metrics: file.metrics.into_settings()?,
            access: file.access.into_settings(folder),
            permissions: file.permissions.into_settings(folder),
            queue: file.queue.into_settings(),
            operators: file.operators.into_settings(),
            extensions: file
                .extensions
                .into_iter()
                .map(|e| e.into_settings(folder))
                .collect::<Result<_, _>>()?,
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
        self.validate_groups()?;
        self.validate_routes()?;
        self.auth.validate()?;
        if self.status_sample > MAX_STATUS_SAMPLE {
            return Err(ConfigError::StatusSampleTooLarge(self.status_sample));
        }
        self.commands.validate(self)?;
        self.validate_balancing()?;
        self.restart.validate()?;
        self.fallback.validate()?;
        self.proxy_protocol.validate()?;
        self.metrics.validate(self.listen)?;
        self.queue.validate(
            self.connect_timeout_ms,
            extension::login_budget_ms(&self.extensions),
        )?;
        extension::validate(&self.extensions)?;
        self.validate_login_budget()?;
        for s in &self.sidecars {
            if self.backend(&s.backend).is_none() {
                return Err(ConfigError::UnknownBackend {
                    setting: "a sidecar".to_owned(),
                    name: s.backend.clone(),
                });
            }
        }
        sidecar::validate(&self.sidecars)?;
        self.operators.validate(self)?;
        if self.sidecar_timeout_ms == 0 {
            return Err(ConfigError::ZeroSidecarTimeout);
        }
        if self.connect_timeout_ms == 0 {
            return Err(ConfigError::ZeroConnectTimeout);
        }
        if self.switch_freeze_limit_ms == 0 {
            return Err(ConfigError::ZeroFreezeLimit);
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

    /// The group of this name.
    pub fn group(&self, name: &str) -> Option<&Group> {
        self.groups.iter().find(|g| g.name == name)
    }

    /// The group players of this kind are routed to, when routes are configured.
    pub fn route_group(&self, auth: AuthKind) -> Option<&Group> {
        let route = self.routes.iter().find(|r| r.auth == auth)?;
        self.group(&route.group)
    }

    /// Groups have unique names, list configured backends once each, and never
    /// share a backend with a group of the other kind: a verified and an
    /// unverified player have different unique ids for the same name, so one
    /// server would keep two files for them (DESIGN.md section 15.3).
    fn validate_groups(&self) -> Result<(), ConfigError> {
        for (i, group) in self.groups.iter().enumerate() {
            if group.name.trim().is_empty() {
                return Err(ConfigError::EmptyGroupName);
            }
            if self.groups[..i].iter().any(|g| g.name == group.name) {
                return Err(ConfigError::DuplicateGroup(group.name.clone()));
            }
            if group.backends.is_empty() {
                return Err(ConfigError::EmptyGroup(group.name.clone()));
            }
            for (j, backend) in group.backends.iter().enumerate() {
                self.known_backend(&format!("group \"{}\"", group.name), backend)?;
                if group.backends[..j].contains(backend) {
                    return Err(ConfigError::DuplicateInGroup {
                        group: group.name.clone(),
                        backend: backend.clone(),
                    });
                }
                let other = self.groups[..i]
                    .iter()
                    .find(|g| g.auth != group.auth && g.backends.contains(backend));
                if let Some(other) = other {
                    let (verified, unverified) = match group.auth {
                        AuthKind::Verified => (group, other),
                        AuthKind::Unverified => (other, group),
                    };
                    return Err(ConfigError::MixedAuthBackend {
                        backend: backend.clone(),
                        verified_group: verified.name.clone(),
                        unverified_group: unverified.name.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    /// Each route leads to a group of its own kind, each kind has one route at
    /// most, and once routes are written every kind the login mode lets in has
    /// one. The hybrid mode lets in both kinds, so it needs both routes even when
    /// none is written: otherwise the two kinds would meet on the default backend.
    fn validate_routes(&self) -> Result<(), ConfigError> {
        for (i, route) in self.routes.iter().enumerate() {
            if self.routes[..i].iter().any(|r| r.auth == route.auth) {
                return Err(ConfigError::DuplicateRoute(route.auth));
            }
            let Some(group) = self.group(&route.group) else {
                return Err(ConfigError::UnknownRouteGroup {
                    auth: route.auth,
                    group: route.group.clone(),
                });
            };
            if group.auth != route.auth {
                return Err(ConfigError::RouteAuthMismatch {
                    auth: route.auth,
                    group: group.name.clone(),
                    group_auth: group.auth,
                });
            }
        }
        let needed: &[AuthKind] = match self.auth_mode {
            AuthMode::Online => &[AuthKind::Verified],
            AuthMode::Offline => &[AuthKind::Unverified],
            AuthMode::Hybrid => &[AuthKind::Verified, AuthKind::Unverified],
        };
        if !self.routes.is_empty() || self.auth_mode == AuthMode::Hybrid {
            for &auth in needed {
                if !self.routes.iter().any(|r| r.auth == auth) {
                    return Err(ConfigError::MissingRoute {
                        auth,
                        mode: self.auth_mode,
                    });
                }
            }
        }
        Ok(())
    }

    /// The backend forced for the host name a client put in its handshake, if any.
    /// Case does not matter, and a final dot is ignored.
    pub fn forced_backend(&self, host: &str) -> Option<&str> {
        self.forced_host(host).map(|f| f.backend.as_str())
    }

    /// The forced host a client's handshake host name matches, by the same rules as
    /// [`Config::forced_backend`].
    pub fn forced_host(&self, host: &str) -> Option<&ForcedHost> {
        let host = host.strip_suffix('.').unwrap_or(host);
        self.forced_hosts
            .iter()
            .find(|f| f.host.eq_ignore_ascii_case(host))
    }

    /// The line the server list shows to a client whose handshake carried `host`: the
    /// `motd` of the forced host it matches, or the proxy's own.
    pub fn motd_for(&self, host: Option<&str>) -> &str {
        host.and_then(|h| self.forced_host(h))
            .and_then(|f| f.motd.as_deref())
            .unwrap_or(&self.motd)
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
pub(crate) fn check_address(address: &str) -> Result<(), &'static str> {
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

#[cfg(test)]
mod favicon_tests;

#[cfg(test)]
mod auth_tests;

#[cfg(test)]
mod commands_tests;

#[cfg(test)]
mod balancing_tests;

#[cfg(test)]
mod metrics_tests;

#[cfg(test)]
mod restart_tests;

#[cfg(test)]
mod proxy_protocol_tests;

#[cfg(test)]
mod access_tests;

#[cfg(test)]
mod operators_tests;
#[cfg(test)]
mod permissions_tests;
#[cfg(test)]
mod queue_tests;
#[cfg(test)]
mod sidecar_tests;

#[cfg(test)]
mod extension_tests;

#[cfg(test)]
mod fallback_tests;
