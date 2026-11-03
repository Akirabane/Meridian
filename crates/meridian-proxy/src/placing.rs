//! The login's use of the balancer (docs/LOAD-BALANCING.md, section 4): with
//! `[balancing] enabled`, a player is placed among the backends of role `lobby`
//! that their route allows, one place is reserved for them while they are sent
//! there, and the backends the balancer refuses are not tried again by the
//! ordinary loop. With balancing off, none of this runs.
use crate::balance::{NoRoom, Reservation, Server};
use crate::log::{debug, warning};
use crate::session::{Connect, Sessions};
use crate::status::Report;
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
    /// Whether the balancer turned the player away because no lobby could take them
    /// (at their hard cap, drained, not accepting transfers or not heard from), and
    /// not because every one failed to answer.
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
    let max_age = Duration::from_secs(config.balancing.state_max_age_seconds);
    // What the backend says of itself: nothing known is as if it accepted; an old
    // report is not believed, so the backend is not offered; a fresh one may refuse
    // transfers, and may be smaller than the caps written.
    let (believed, accepts, reported, capacity) = match sessions.statuses.report(name, max_age, now)
    {
        Report::Unknown => (true, true, None, None),
        Report::Stale => (false, true, None, None),
        Report::Fresh(s) => (
            true,
            s.accepts_transfers.unwrap_or(true),
            s.players,
            s.capacity,
        ),
    };
    let ping = sessions.statuses.ping(name, max_age, now);
    let drained = sessions.balancer.is_drained(name);
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
/// backend with no role. `on` is the players its sessions count.
pub fn describe(config: &Config, sessions: &Sessions, name: &str, on: usize) -> Option<String> {
    let limits = config.balanced(name)?;
    let now = Instant::now();
    let retry = Duration::from_secs(config.backend_retry_seconds);
    let max_age = Duration::from_secs(config.balancing.state_max_age_seconds);
    let server = server_of(config, sessions, name, &[], true, (retry, now))?;
    let candidate = server.with_players(on + sessions.balancer.reserved(name));
    let report = sessions.statuses.report(name, max_age, now);
    let offered = if sessions.balancer.is_drained(name) {
        "draining (drained from the console)".to_owned()
    } else if matches!(report, Report::Fresh(s) if s.accepts_transfers == Some(false)) {
        "draining (it does not accept transfers)".to_owned()
    } else if report == Report::Stale {
        format!(
            "not offered (no report for {} s)",
            config.balancing.state_max_age_seconds
        )
    } else if sessions
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
    if let Some(passed) = sessions.health.describe(name, retry, now) {
        line.push_str(&format!(", {passed}"));
    }
    Some(line)
}
