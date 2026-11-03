//! The login's use of the balancer (docs/LOAD-BALANCING.md, section 4): with
//! `[balancing] enabled`, a player is placed among the backends of role `lobby`
//! that their route allows, one place is reserved for them while they are sent
//! there, and the backends the balancer refuses are not tried again by the
//! ordinary loop. With balancing off, none of this runs.
use crate::balance::{NoRoom, Reservation, Server};
use crate::log::{debug, warning};
use crate::session::{Connect, Sessions};
use meridian_config::{Backend, Config, Role};
use meridian_net::{
    Profile,
    backend::{BackendLink, BackendOptions},
};
use std::time::{Duration, Instant};

/// A backend that took the player, with its link and the place reserved for them
/// (`None` when the ordinary loop chose it).
pub type Joined<'c, 's> = (&'c Backend, BackendLink, Option<Reservation<'s>>);

/// What the balancer did for a login.
pub struct Balanced<'c, 's> {
    /// The backend that took the player, if the balancer found one.
    pub joined: Option<Joined<'c, 's>>,
    /// The backends the ordinary loop must not try: the balancer tried them, or
    /// refused them for want of room.
    pub tried: Vec<String>,
    /// Whether the balancer found no room on any lobby that was reachable.
    pub full: bool,
}

/// Places a login. `candidates` are the backends the route allows, in the order the
/// ordinary loop would try them, and `host` is the host name the player typed.
pub fn login<'c, 's>(
    config: &'c Config,
    sessions: &'s Sessions,
    connect: &Connect,
    profile: &Profile,
    options: &BackendOptions,
    candidates: &[String],
    host: Option<&str>,
) -> Balanced<'c, 's> {
    let mut out = Balanced {
        joined: None,
        tried: Vec::new(),
        full: false,
    };
    if !config.balancing.enabled {
        return out;
    }
    let forced = host
        .and_then(|h| config.forced_backend(h))
        .filter(|f| candidates.iter().any(|c| c == f));
    // A forced host that is not balanced is tried first by the ordinary loop.
    if let Some(name) = forced {
        if config.balanced(name).is_none() {
            return out;
        }
    }
    let retry = Duration::from_secs(config.backend_retry_seconds);
    let now = Instant::now();
    sessions
        .balancer
        .retain(|name| config.balanced(name).is_some());
    let counts = || sessions.counts();
    let timing = (retry, now);

    // A server named on purpose may pass its soft cap, never its hard cap.
    if let Some(name) = forced {
        if let Some(server) = server_of(config, sessions, name, &out.tried, true, timing) {
            match sessions.balancer.reserve_named(server, counts) {
                Ok(reservation) => {
                    if join(
                        config,
                        sessions,
                        connect,
                        profile,
                        options,
                        name,
                        reservation,
                        &mut out,
                    ) {
                        return out;
                    }
                }
                Err(refused) => {
                    debug!("forced host: backend \"{name}\" refused: {refused}");
                    out.tried.push(name.to_owned());
                }
            }
        }
    }

    let lobbies: Vec<&str> = candidates
        .iter()
        .map(String::as_str)
        .filter(|c| Some(*c) != forced)
        .filter(|c| matches!(config.balanced(c), Some(b) if b.role == Role::Lobby))
        .collect();
    if lobbies.is_empty() {
        return out;
    }
    // First the lobbies that are not passed over; when none has room, those that
    // are, as the ordinary loop does, since an old failure alone refuses nobody.
    let mut passed_over_ok = false;
    loop {
        let servers: Vec<Server<'_>> = lobbies
            .iter()
            .filter_map(|name| {
                server_of(config, sessions, name, &out.tried, passed_over_ok, timing)
            })
            .collect();
        let reserved = sessions.balancer.reserve(
            &servers,
            counts,
            config.balancing.margin,
            config.balancing.tie_break,
        );
        match reserved {
            Ok(reservation) => {
                let name = reservation.backend().to_owned();
                if join(
                    config,
                    sessions,
                    connect,
                    profile,
                    options,
                    &name,
                    reservation,
                    &mut out,
                ) {
                    return out;
                }
            }
            Err(NoRoom) if !passed_over_ok => passed_over_ok = true,
            Err(NoRoom) => {
                let open: Vec<&str> = servers
                    .iter()
                    .filter(|s| s.available)
                    .map(|s| s.backend)
                    .collect();
                out.full = !open.is_empty();
                // They are at their hard cap: the ordinary loop must not try them.
                out.tried.extend(open.iter().map(|n| (*n).to_owned()));
                return out;
            }
        }
    }
}

/// Connects to `name` with the place already reserved. Whatever happens, `name` is
/// not tried again for this login.
#[allow(clippy::too_many_arguments)]
fn join<'c, 's>(
    config: &'c Config,
    sessions: &Sessions,
    connect: &Connect,
    profile: &Profile,
    options: &BackendOptions,
    name: &str,
    reservation: Reservation<'s>,
    out: &mut Balanced<'c, 's>,
) -> bool {
    let Some(backend) = config.backend(name) else {
        out.tried.push(name.to_owned());
        return false;
    };
    match connect(&backend.address, profile, options) {
        Ok(link) => {
            sessions.health.answered(name);
            out.joined = Some((backend, link, Some(reservation)));
            true
        }
        Err(e) => {
            sessions.health.failed(name, Instant::now());
            warning!(
                "balancer: cannot join backend \"{name}\" at {}: {e}",
                backend.address
            );
            out.tried.push(name.to_owned());
            false
        }
    }
}

/// `name` as the balancer sees it: its limits, and whether it takes players now. A
/// backend that failed this login is never available; one that recently failed is
/// only when `passed_over_ok`.
fn server_of<'n>(
    config: &Config,
    sessions: &Sessions,
    name: &'n str,
    tried: &[String],
    passed_over_ok: bool,
    (retry, now): (Duration, Instant),
) -> Option<Server<'n>> {
    let limits = config.balanced(name)?;
    let failed_now = tried.iter().any(|t| t == name);
    let passed_over = sessions.health.passed_over_for(name, retry, now).is_some();
    Some(Server {
        backend: name,
        soft_cap: limits.soft_cap,
        hard_cap: limits.hard_cap,
        available: !failed_now && (passed_over_ok || !passed_over),
        ping: None,
    })
}
