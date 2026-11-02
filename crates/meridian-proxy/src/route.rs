//! Which backends a player who logs in may be sent to, in the order to try them,
//! and whether there is room for them. Both are pure functions of the
//! configuration and what the login told the proxy, so they are tested here
//! without any connection.
use meridian_config::{AuthKind, Config};
use std::fmt;

/// Why no backend can be chosen. The configuration is checked when it is loaded,
/// so these come from a configuration built in code, or from a login mode that
/// lets in a kind of player the routes do not cover.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RouteError {
    /// A forced host, the default backend or `try` names a backend that is not
    /// configured.
    UnknownBackend(String),
    /// Routes are configured, but none is for this kind of player.
    NoRoute(AuthKind),
    /// The route for this kind of player names a group that is not configured.
    NoGroup { auth: AuthKind, group: String },
    /// Every candidate was left out, because it serves the other kind of player.
    NoCandidate(AuthKind),
}

impl fmt::Display for RouteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RouteError::UnknownBackend(name) => {
                write!(f, "\"{name}\" is not a configured backend")
            }
            RouteError::NoRoute(auth) => {
                write!(f, "no route is configured for {} players", auth.name())
            }
            RouteError::NoGroup { auth, group } => write!(
                f,
                "the route for {} players names group \"{group}\", which is not configured",
                auth.name()
            ),
            RouteError::NoCandidate(auth) => {
                write!(f, "no backend is open to {} players", auth.name())
            }
        }
    }
}

impl std::error::Error for RouteError {}

/// The backends to try for a player, in order, each once:
///
/// 1. the backend forced for `host`, the host name of the handshake, if there is
///    one (case and a final dot do not matter);
/// 2. otherwise, when routes are configured, the backends of the group the route
///    for `auth` leads to, in the group's order;
/// 3. otherwise the default backend;
/// 4. then the backends of `try`, in order.
///
/// When routes are configured, a backend that belongs to a group of the other
/// kind is left out wherever it came from: a verified and an unverified player
/// must never meet on one server (DESIGN.md section 15.3).
pub fn candidates(
    config: &Config,
    host: Option<&str>,
    auth: AuthKind,
) -> Result<Vec<String>, RouteError> {
    let mut first: Vec<&str> = Vec::new();
    if let Some(forced) = host.and_then(|h| config.forced_backend(h)) {
        first.push(forced);
    } else if !config.routes.is_empty() {
        let route = config
            .routes
            .iter()
            .find(|r| r.auth == auth)
            .ok_or(RouteError::NoRoute(auth))?;
        let group = config
            .group(&route.group)
            .ok_or_else(|| RouteError::NoGroup {
                auth,
                group: route.group.clone(),
            })?;
        first.extend(group.backends.iter().map(String::as_str));
    } else {
        first.push(&config.default_backend);
    }

    let mut chosen: Vec<String> = Vec::new();
    for name in first
        .into_iter()
        .chain(config.try_backends.iter().map(String::as_str))
    {
        if config.backend(name).is_none() {
            return Err(RouteError::UnknownBackend(name.to_owned()));
        }
        if chosen.iter().any(|c| c == name) || serves_other_kind(config, name, auth) {
            continue;
        }
        chosen.push(name.to_owned());
    }
    if chosen.is_empty() {
        return Err(RouteError::NoCandidate(auth));
    }
    Ok(chosen)
}

/// Whether `backend` is in a group for the other kind of player, once routes
/// are configured. Without routes, groups do not choose anything.
pub(crate) fn serves_other_kind(config: &Config, backend: &str, auth: AuthKind) -> bool {
    !config.routes.is_empty()
        && config
            .groups
            .iter()
            .any(|g| g.auth != auth && g.backends.iter().any(|b| b == backend))
}

/// The proxy is full: the reason given to a player who is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Full;

impl Full {
    /// The text of the Disconnect packet. Vanilla's own key for it is
    /// `multiplayer.disconnect.server_full`.
    pub const MESSAGE: &'static str = "The server is full";
}

impl fmt::Display for Full {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(Full::MESSAGE)
    }
}

impl std::error::Error for Full {}

/// Whether one more player may come in when `online` players already are.
/// `max_players` 0 means no limit.
pub fn admit(max_players: u32, online: usize) -> Result<(), Full> {
    if max_players == 0 || online < max_players as usize {
        Ok(())
    } else {
        Err(Full)
    }
}

#[cfg(test)]
mod tests;
