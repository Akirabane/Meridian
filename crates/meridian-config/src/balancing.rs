//! The load-balancing settings: the role of each backend, how many players it is
//! filled to, and the `[balancing]` table. Step 1 of docs/LOAD-BALANCING.md: the
//! settings are read and checked, and nothing in the proxy uses them yet.
use crate::{Config, ConfigError};
use serde::Deserialize;

/// What a backend is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Where players arrive after login and where `/hub` leads.
    Lobby,
    /// A waiting room between the lobby and the games, where players are grouped.
    Sublobby,
    /// A server that runs the games.
    Game,
}

impl Role {
    /// The name used in the file.
    pub fn name(self) -> &'static str {
        match self {
            Role::Lobby => "lobby",
            Role::Sublobby => "sublobby",
            Role::Game => "game",
        }
    }
}

/// How a tie between servers that are equally good is broken.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TieBreak {
    /// The server the proxy measured as the quickest to answer.
    #[default]
    Ping,
    /// The server listed first.
    Order,
}

impl TieBreak {
    /// The name used in the file.
    pub fn name(self) -> &'static str {
        match self {
            TieBreak::Ping => "ping",
            TieBreak::Order => "order",
        }
    }
}

/// The role and the limits of one backend (`role`, `soft-cap` and `hard-cap` in
/// `[[backends]]`). A backend with no role is not balanced: it takes players only
/// where a route, a forced host or a command sends them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BalancedServer {
    /// The backend this is for.
    pub backend: String,
    pub role: Role,
    /// The number of players the server is filled to before the next one of its
    /// role is opened.
    pub soft_cap: u32,
    /// The most players the balancer ever places on it. It is above the soft cap
    /// only to take the overflow when every server of the role is at its soft cap.
    pub hard_cap: u32,
}

/// The `[balancing]` table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Balancing {
    /// Whether logins are placed by the balancer. Off by default: the proxy then
    /// chooses a backend exactly as it did before there were roles.
    pub enabled: bool,
    /// How far below its soft cap a server that reached it must fall before it is
    /// filled again, so that a server does not flip between open and closed with
    /// each player who leaves. A margin larger than the soft cap means the server
    /// is filled again only when empty.
    pub margin: u32,
    /// Seconds after which what a server last reported is no longer believed: the
    /// server is then not offered to new players.
    pub state_max_age_seconds: u64,
    /// How a tie is broken.
    pub tie_break: TieBreak,
    /// Seconds between two measures of how quickly each backend answers.
    pub probe_interval_seconds: u64,
}

impl Default for Balancing {
    fn default() -> Self {
        Self {
            enabled: false,
            margin: 2,
            state_max_age_seconds: 15,
            tie_break: TieBreak::Ping,
            probe_interval_seconds: 10,
        }
    }
}

/// The `[balancing]` table as written.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct FileBalancing {
    enabled: Option<bool>,
    margin: Option<u32>,
    state_max_age_seconds: Option<u64>,
    tie_break: Option<TieBreak>,
    probe_interval_seconds: Option<u64>,
}

impl FileBalancing {
    pub(crate) fn into_settings(self) -> Balancing {
        let defaults = Balancing::default();
        Balancing {
            enabled: self.enabled.unwrap_or(defaults.enabled),
            margin: self.margin.unwrap_or(defaults.margin),
            state_max_age_seconds: self
                .state_max_age_seconds
                .unwrap_or(defaults.state_max_age_seconds),
            tie_break: self.tie_break.unwrap_or(defaults.tie_break),
            probe_interval_seconds: self
                .probe_interval_seconds
                .unwrap_or(defaults.probe_interval_seconds),
        }
    }
}

/// The balancing keys of one `[[backends]]` entry, as written.
pub(crate) struct FileServer<'a> {
    pub(crate) name: &'a str,
    pub(crate) role: Option<Role>,
    pub(crate) soft_cap: Option<u32>,
    pub(crate) hard_cap: Option<u32>,
}

/// The balanced servers among the backends written, refusing a limit without a
/// role and a role without a soft cap. The hard cap defaults to the soft cap.
pub(crate) fn servers(written: &[FileServer<'_>]) -> Result<Vec<BalancedServer>, ConfigError> {
    let mut servers = Vec::new();
    for w in written {
        let Some(role) = w.role else {
            for (key, value) in [("soft-cap", w.soft_cap), ("hard-cap", w.hard_cap)] {
                if value.is_some() {
                    return Err(ConfigError::LimitWithoutRole {
                        backend: w.name.to_owned(),
                        key,
                    });
                }
            }
            continue;
        };
        let soft_cap = w
            .soft_cap
            .ok_or_else(|| ConfigError::RoleWithoutSoftCap(w.name.to_owned()))?;
        servers.push(BalancedServer {
            backend: w.name.to_owned(),
            role,
            soft_cap,
            hard_cap: w.hard_cap.unwrap_or(soft_cap),
        });
    }
    Ok(servers)
}

impl Balancing {
    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        if self.state_max_age_seconds == 0 {
            return Err(ConfigError::ZeroStateMaxAge);
        }
        if self.probe_interval_seconds == 0 {
            return Err(ConfigError::ZeroProbeInterval);
        }
        Ok(())
    }
}

impl Config {
    /// The balancing limits of `backend`, if it has a role.
    pub fn balanced(&self, backend: &str) -> Option<&BalancedServer> {
        self.balanced.iter().find(|s| s.backend == backend)
    }

    /// The balanced servers of `role`, in the order the backends are listed.
    pub fn servers_with_role(&self, role: Role) -> impl Iterator<Item = &BalancedServer> {
        self.balanced.iter().filter(move |s| s.role == role)
    }

    /// Each limit is above zero and the hard cap is not below the soft cap, the
    /// `[balancing]` table is usable, and a configuration that balances has a lobby
    /// for players to arrive in.
    pub(crate) fn validate_balancing(&self) -> Result<(), ConfigError> {
        for s in &self.balanced {
            if s.soft_cap == 0 {
                return Err(ConfigError::ZeroSoftCap(s.backend.clone()));
            }
            if s.hard_cap < s.soft_cap {
                return Err(ConfigError::HardCapBelowSoftCap {
                    backend: s.backend.clone(),
                    soft_cap: s.soft_cap,
                    hard_cap: s.hard_cap,
                });
            }
        }
        self.balancing.validate()?;
        if self.balancing.enabled && self.balanced.is_empty() {
            return Err(ConfigError::BalancingWithoutRoles);
        }
        if !self.balanced.is_empty() && self.servers_with_role(Role::Lobby).next().is_none() {
            return Err(ConfigError::NoLobby);
        }
        Ok(())
    }

    /// What the operator should know of the balancing settings, one line each. The file is
    /// used all the same: nothing here is wrong in itself.
    ///
    /// A report is believed for `state-max-age-seconds`. A round of readings starts every
    /// `probe-interval-seconds`, or at once after a round that lasted longer, and a server that
    /// does not answer holds the round for `sidecar-timeout-ms`, then, with `tie-break =
    /// "ping"`, for `connect-timeout-ms`. A server that answers is read at the start of one
    /// round and as late as `sidecar-timeout-ms` into the next. When that gap is longer than
    /// the age a report is believed for, one silent server makes the reports of all the others
    /// too old in turn, and they are not offered to new players.
    pub fn balancing_warnings(&self) -> Vec<String> {
        let read = self
            .sidecars
            .iter()
            .any(|s| self.balanced(&s.backend).is_some());
        if !self.balancing.enabled || !read {
            return Vec::new();
        }
        let ping = self.balancing.tie_break == TieBreak::Ping;
        let held =
            self.sidecar_timeout_ms
                .saturating_add(if ping { self.connect_timeout_ms } else { 0 });
        let interval = self.balancing.probe_interval_seconds.saturating_mul(1_000);
        let gap = interval.max(held).saturating_add(self.sidecar_timeout_ms);
        let age = self.balancing.state_max_age_seconds.saturating_mul(1_000);
        if age >= gap {
            return Vec::new();
        }
        let lower = if ping {
            "sidecar-timeout-ms or connect-timeout-ms"
        } else {
            "sidecar-timeout-ms"
        };
        vec![format!(
            "state-max-age-seconds is {} s, but up to {} ms can pass between two readings of a \
             server that answers, while another does not (probe-interval-seconds {} s, \
             sidecar-timeout-ms {} ms{}): the reports of the servers that answer would then be \
             too old in turn, and they would not be offered to new players. Raise \
             state-max-age-seconds to {} or more, or lower {lower}",
            self.balancing.state_max_age_seconds,
            gap,
            self.balancing.probe_interval_seconds,
            self.sidecar_timeout_ms,
            if ping {
                format!(
                    ", connect-timeout-ms {} ms for tie-break = \"ping\"",
                    self.connect_timeout_ms
                )
            } else {
                String::new()
            },
            gap.div_ceil(1_000),
        )]
    }
}
