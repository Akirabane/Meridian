//! The use of the balancer by the login, `/hub` and `/server` (docs/LOAD-BALANCING.md,
//! section 4). With `[balancing] enabled`:
//!
//! - a login is placed among the backends of role `lobby` that the player's route
//!   allows ([`login`]);
//! - `/hub` takes a lobby by the same rule, then a sublobby when no lobby can take the
//!   player ([`hub`]);
//! - a backend named on purpose, by a forced host or `/server <name>`, may pass its
//!   soft cap but never its hard cap, and never takes a player while drained
//!   ([`named`]).
//!
//! One place is reserved on the backend while the player is sent there. With balancing
//! off, none of this runs.
use crate::balance::{NoRoom, Refused, Reservation, Server};
use crate::log::{debug, warning};
use crate::session::{Connect, Shared};
use crate::status::Report;
use meridian_config::{Backend, Config, Role};
use meridian_net::{
    Profile,
    backend::{BackendLink, BackendOptions},
};
use std::time::{Duration, Instant};

/// A backend that took the player, with its link and the place reserved for them
/// (`None` when the ordinary loop chose it).
pub type Joined<'c> = (&'c Backend, BackendLink, Option<Reservation>);

/// What the balancer did for a login.
pub struct Balanced<'c> {
    /// The backend that took the player, if the balancer found one.
    pub joined: Option<Joined<'c>>,
    /// The backends the ordinary loop must not try: the balancer tried them, or
    /// refused them for want of room.
    pub tried: Vec<String>,
    /// Whether the balancer turned the player away because no lobby could take them
    /// (at their hard cap, drained, not accepting transfers or not heard from), and
    /// not because every one failed to answer.
    pub full: bool,
}

/// Places a login. `candidates` are the backends the route allows, in the order the
/// ordinary loop would try them, and `host` is the host name the player typed.
pub fn login<'c>(
    config: &'c Config,
    shared: &Shared,
    connect: &Connect,
    profile: &Profile,
    options: &BackendOptions,
    candidates: &[String],
    host: Option<&str>,
) -> Balanced<'c> {
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
    shared
        .balancer
        .retain(|name| config.balanced(name).is_some());
    let counts = || shared.counts();
    let timing = (retry, now);

    // A server named on purpose may pass its soft cap, never its hard cap.
    if let Some(name) = forced {
        if let Some(server) = server_of(config, shared, name, &out.tried, true, timing) {
            match shared.balancer.reserve_named(server, counts) {
                Ok(reservation) => {
                    if join(
                        config,
                        shared,
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
            .filter_map(|name| server_of(config, shared, name, &out.tried, passed_over_ok, timing))
            .collect();
        let reserved = shared.balancer.reserve(
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
                    shared,
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
                // Some lobby was not lost to a failed connection: the player is
                // turned away for want of room, not because nothing answers.
                out.full = lobbies.iter().any(|n| !out.tried.iter().any(|t| t == n));
                // None can take the player, whether it is at its hard cap, drained,
                // not accepting transfers or not heard from: the ordinary loop must
                // not try them.
                for name in &lobbies {
                    if !out.tried.iter().any(|t| t == name) {
                        out.tried.push((*name).to_owned());
                    }
                }
                return out;
            }
        }
    }
}

/// A place for a player who types `/server <target>`, or whom a forced host leads to
/// `target`. `Ok(None)` when balancing is off or `target` has no role: nothing is
/// reserved and the move goes on as it always did. `Err` is the line the player is
/// told.
pub fn named(
    config: &Config,
    shared: &Shared,
    target: &str,
) -> Result<Option<Reservation>, String> {
    if !config.balancing.enabled {
        return Ok(None);
    }
    let retry = Duration::from_secs(config.backend_retry_seconds);
    // A backend that recently failed a login is still one the player named.
    let Some(server) = server_of(config, shared, target, &[], true, (retry, Instant::now())) else {
        return Ok(None);
    };
    shared
        .balancer
        .reserve_named(server, || shared.counts())
        .map(Some)
        .map_err(|refused| match refused {
            Refused::AtHardCap => format!("{target} is full."),
            Refused::Unavailable => format!("{target} is not taking players right now."),
        })
}

/// Where `/hub` and `/lobby` lead for a player on `current` when `open_to` says which
/// backends they may be sent to: the best open server of role `lobby` by the rule of
/// the login, and a `sublobby` only when no lobby can take them. A place is reserved.
/// `Err` is the line the player is told.
pub fn hub(
    config: &Config,
    shared: &Shared,
    open_to: impl Fn(&str) -> bool,
    current: &str,
) -> Result<(String, Reservation), String> {
    in_roles(
        config,
        shared,
        &[Role::Lobby, Role::Sublobby],
        open_to,
        current,
        "No lobby has room right now.",
    )
}

/// Where `/play` leads for a player on `current`: the best open server of role `game`
/// by the rule of the login (fill one up to its soft cap, then open the next, overflow
/// up to the hard cap). A place is reserved. `Err` is the line the player is told.
pub fn game(
    config: &Config,
    shared: &Shared,
    open_to: impl Fn(&str) -> bool,
    current: &str,
) -> Result<(String, Reservation), String> {
    in_roles(
        config,
        shared,
        &[Role::Game],
        open_to,
        current,
        "No game server has room right now.",
    )
}

/// The best server of the first of `roles` that can take the player, as the login
/// would choose among them, with a place reserved on it; `none` is what the player is
/// told when no role can.
fn in_roles(
    config: &Config,
    shared: &Shared,
    roles: &[Role],
    open_to: impl Fn(&str) -> bool,
    current: &str,
    none: &str,
) -> Result<(String, Reservation), String> {
    let retry = Duration::from_secs(config.backend_retry_seconds);
    let now = Instant::now();
    shared
        .balancer
        .retain(|name| config.balanced(name).is_some());
    for &role in roles {
        let servers: Vec<Server<'_>> = config
            .servers_with_role(role)
            .filter(|s| open_to(&s.backend))
            .filter_map(|s| server_of(config, shared, &s.backend, &[], false, (retry, now)))
            .collect();
        let reserved = shared.balancer.reserve(
            &servers,
            || shared.counts(),
            config.balancing.margin,
            config.balancing.tie_break,
        );
        if let Ok(reservation) = reserved {
            let target = reservation.backend().to_owned();
            if target == current {
                return Err(format!("You are already on {target}."));
            }
            return Ok((target, reservation));
        }
    }
    Err(none.to_owned())
}

/// Where a player on the drained `from` can go: the best server of the same role by the
/// rule of the login, never `from`, and never one that serves the other kind of player
/// than `from` does (DESIGN.md section 15.3). A place is reserved on it. `Err` is why
/// the player has nowhere to go.
pub fn elsewhere(config: &Config, shared: &Shared, from: &str) -> Result<Reservation, String> {
    let Some(limits) = config.balanced(from) else {
        return Err(format!("{from} has no role"));
    };
    let role = limits.role;
    // Without routes, groups choose nothing. With them, the players of `from` are of the
    // kind its group is for.
    let kind = (!config.routes.is_empty())
        .then(|| {
            config
                .groups
                .iter()
                .find(|g| g.backends.iter().any(|b| b == from))
        })
        .flatten()
        .map(|g| g.auth);
    let retry = Duration::from_secs(config.backend_retry_seconds);
    let now = Instant::now();
    shared
        .balancer
        .retain(|name| config.balanced(name).is_some());
    let servers: Vec<Server<'_>> = config
        .servers_with_role(role)
        .filter(|s| s.backend != from)
        .filter(|s| kind.is_none_or(|k| !crate::route::serves_other_kind(config, &s.backend, k)))
        .filter_map(|s| server_of(config, shared, &s.backend, &[], false, (retry, now)))
        .collect();
    shared
        .balancer
        .reserve(
            &servers,
            || shared.counts(),
            config.balancing.margin,
            config.balancing.tie_break,
        )
        .map_err(|NoRoom| format!("no other server of role {} has room", role.name()))
}

/// Connects to `name` with the place already reserved. Whatever happens, `name` is
/// not tried again for this login.
#[allow(clippy::too_many_arguments)]
fn join<'c>(
    config: &'c Config,
    shared: &Shared,
    connect: &Connect,
    profile: &Profile,
    options: &BackendOptions,
    name: &str,
    reservation: Reservation,
    out: &mut Balanced<'c>,
) -> bool {
    let Some(backend) = config.backend(name) else {
        out.tried.push(name.to_owned());
        return false;
    };
    match connect(&backend.address, profile, options) {
        Ok(link) => {
            shared.health.answered(name);
            out.joined = Some((backend, link, Some(reservation)));
            true
        }
        Err(e) => {
            shared.health.failed(name, Instant::now());
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
    shared: &Shared,
    name: &'n str,
    tried: &[String],
    passed_over_ok: bool,
    (retry, now): (Duration, Instant),
) -> Option<Server<'n>> {
    let limits = config.balanced(name)?;
    let failed_now = tried.iter().any(|t| t == name);
    let passed_over = shared.health.passed_over_for(name, retry, now).is_some();
    let max_age = Duration::from_secs(config.balancing.state_max_age_seconds);
    // What the backend says of itself: nothing known is as if it accepted; an old
    // report is not believed, so the backend is not offered; a fresh one may refuse
    // transfers, and may be smaller than the caps written.
    let (believed, accepts, reported, capacity) = match shared.statuses.report(name, max_age, now) {
        Report::Unknown => (true, true, None, None),
        Report::Stale => (false, true, None, None),
        Report::Fresh(s) => (
            true,
            s.accepts_transfers.unwrap_or(true),
            s.players,
            s.capacity,
        ),
    };
    let ping = shared.statuses.ping(name, max_age, now);
    let drained = shared.balancer.is_drained(name);
    let hard_cap = capacity.map_or(limits.hard_cap, |c| {
        limits.hard_cap.min(u32::try_from(c).unwrap_or(u32::MAX))
    });
    Some(Server {
        backend: name,
        soft_cap: limits.soft_cap.min(hard_cap),
        hard_cap,
        available: believed
            && accepts
            && !drained
            && !failed_now
            && (passed_over_ok || !passed_over),
        ping,
        reported_players: reported.map(|p| usize::try_from(p).unwrap_or(usize::MAX)),
    })
}

/// What the console says of a balanced backend: its role, its players against its
/// caps, whether the balancer offers it, and how quickly it answered. `None` for a
/// backend with no role. `on` is the players its sessions count, and `restarting` why the
/// proxy believes the backend is going down, if it does (docs/RESTART.md).
pub fn describe(
    config: &Config,
    shared: &Shared,
    name: &str,
    on: usize,
    restarting: Option<&str>,
) -> Option<String> {
    let limits = config.balanced(name)?;
    let now = Instant::now();
    let retry = Duration::from_secs(config.backend_retry_seconds);
    let max_age = Duration::from_secs(config.balancing.state_max_age_seconds);
    let server = server_of(config, shared, name, &[], true, (retry, now))?;
    let candidate = server.with_players(on + shared.balancer.reserved(name));
    let report = shared.statuses.report(name, max_age, now);
    // The detector drained it; an operator who offered it again is believed over the detector.
    let restarting = restarting.filter(|_| shared.balancer.is_drained(name));
    let offered = if let Some(why) = restarting {
        format!("draining (restarting: {why})")
    } else if shared.balancer.is_drained(name) {
        "draining (drained from the console)".to_owned()
    } else if matches!(report, Report::Fresh(s) if s.accepts_transfers == Some(false)) {
        "draining (it does not accept transfers)".to_owned()
    } else if report == Report::Stale {
        format!(
            "not offered (no report for {} s)",
            config.balancing.state_max_age_seconds
        )
    } else if shared
        .balancer
        .is_closed_now(&candidate, config.balancing.margin)
    {
        "closed (soft cap reached)".to_owned()
    } else {
        "open".to_owned()
    };
    let mut line = format!(
        "role {}, {}/{} (hard {}): {offered}",
        limits.role.name(),
        candidate.players,
        candidate.soft_cap,
        candidate.hard_cap
    );
    if let Some(ping) = candidate.ping {
        line.push_str(&format!(", ping {} ms", ping.as_millis()));
    }
    if let Some(passed) = shared.health.describe(name, retry, now) {
        line.push_str(&format!(", {passed}"));
    }
    Some(line)
}
