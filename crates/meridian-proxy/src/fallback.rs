//! A player whose server is lost while they are in the game: the connection closes or
//! breaks, or the server disconnects them. The session (`meridian-handoff`) keeps the player
//! and asks [`replacer`]'s closure for another server; this module chooses one (docs/OPERATING.md,
//! "When a server is lost").
//!
//! The servers tried are those a login would try for this player, in that order and by the same
//! rules (`route::candidates`, the health of the backends, and with balancing on the caps, the
//! drain and the roles of `placing::login`), without the servers that already lost the player in
//! this run of losses. A server that took the player and kept them a minute ([`SETTLED`]) ends
//! the run. The run is bounded by `[fallback] max-attempts`, so two servers that fail in turn
//! cannot pass a player back and forth: past it the player is disconnected with a message.
use crate::log::{info, warning};
use crate::placing;
use crate::route;
use crate::session::{Connect, Sessions};
use meridian_config::{AuthKind, Config};
use meridian_handoff::{Loss, Replacement, SessionHandle};
use meridian_net::{Profile, backend::BackendOptions};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

/// How long a server must keep a player for the run of losses before it to be over.
pub const SETTLED: Duration = Duration::from_secs(60);

/// Who is moved, and what to move them with.
pub struct Context<'a> {
    pub sessions: &'a Sessions,
    pub connect: &'a Connect,
    /// The configuration at the player's login: the one in force now is read when a server is
    /// lost, if the sessions follow it.
    pub config: &'a Config,
    pub handle: &'a SessionHandle,
    pub profile: &'a Profile,
    pub virtual_host: Option<&'a str>,
    pub auth: AuthKind,
    pub peer: SocketAddr,
}

/// The run of losses a player is in: the servers that lost them and how many were tried.
#[derive(Default)]
struct Run {
    lost: Vec<String>,
    tried: u32,
    /// When the last replacement took the player.
    since: Option<Instant>,
}

impl Run {
    /// Starts again when the server the player was on kept them long enough.
    fn settle(&mut self, now: Instant) {
        if self.since.is_some_and(|since| now - since >= SETTLED) {
            *self = Run::default();
        }
    }
}

/// The function the session calls when it loses the player's server.
pub fn replacer<'a>(context: Context<'a>) -> impl FnMut(&Loss) -> Result<Replacement, String> + 'a {
    let mut run = Run::default();
    move |loss| {
        let result = replace(&context, &mut run, loss);
        context.sessions.metrics().fallback(loss, result.is_ok());
        result
    }
}

fn replace(c: &Context<'_>, run: &mut Run, loss: &Loss) -> Result<Replacement, String> {
    let config = c.sessions.current_config(c.config);
    let name = &c.profile.name;
    let lost = c.handle.backend();
    let now = Instant::now();
    run.settle(now);
    if !run.lost.contains(&lost) {
        run.lost.push(lost.clone());
    }
    let what = match loss {
        Loss::Crashed(why) => format!("the connection was lost: {why}"),
        Loss::Kicked(reason) => format!("it disconnected them: {reason}"),
    };
    // A server that kicks a player is up; one that dropped them is not to be offered next.
    if matches!(loss, Loss::Crashed(_)) {
        c.sessions.health.failed(&lost, now);
    }
    let sorry = |why: &str| -> String {
        match loss {
            Loss::Kicked(reason) => format!("{reason}\n\n({why})"),
            Loss::Crashed(_) => format!("You lost the connection to {lost}. {why}"),
        }
    };
    let max = config.fallback.max_attempts;
    if run.tried >= max {
        warning!(
            "{}: {name}: lost backend \"{lost}\" ({what}); {max} servers were tried for them \
             already, so they are disconnected",
            c.peer
        );
        return Err(sorry("No other server could take you right now."));
    }

    // The servers a login would try, without those that lost the player in this run.
    let candidates = match route::candidates(&config, c.virtual_host, c.auth) {
        Ok(candidates) => candidates,
        Err(e) => {
            warning!("{}: {name}: lost backend \"{lost}\" ({what}); {e}", c.peer);
            return Err(sorry("There is no server to send you to."));
        }
    };
    let retry = Duration::from_secs(config.backend_retry_seconds);
    let mut candidates: Vec<String> = c
        .sessions
        .health
        .order(candidates, retry, now)
        .into_iter()
        .filter(|candidate| !run.lost.contains(candidate))
        .collect();
    candidates.truncate((max - run.tried) as usize);
    if candidates.is_empty() {
        warning!(
            "{}: {name}: lost backend \"{lost}\" ({what}); no other server to send them to",
            c.peer
        );
        return Err(sorry("There is no other server to send you to."));
    }

    let options = BackendOptions {
        forwarding_secret: config.forwarding_secret.as_bytes().to_vec(),
        connect_timeout: Duration::from_millis(config.connect_timeout_ms),
        player_address: c.peer.ip(),
    };
    // With balancing on, the balancer places the player among the lobbies first.
    let mut balanced = placing::login(
        &config,
        &c.sessions.shared(),
        c.connect,
        c.profile,
        &options,
        &candidates,
        c.virtual_host,
    );
    let mut joined = balanced.joined.take();
    let mut attempts = u32::from(joined.is_some());
    for candidate in &candidates {
        if joined.is_some() {
            break;
        }
        if balanced.tried.contains(candidate) {
            continue;
        }
        let Some(backend) = config.backend(candidate) else {
            continue;
        };
        attempts += 1;
        match (c.connect)(&backend.address, c.profile, &options) {
            Ok(link) => {
                c.sessions.health.answered(&backend.name);
                joined = Some((backend, link, None));
            }
            Err(e) => {
                c.sessions.health.failed(&backend.name, Instant::now());
                warning!(
                    "{}: {name} cannot join backend \"{}\" at {}: {e}",
                    c.peer,
                    backend.name,
                    backend.address
                );
            }
        }
    }
    run.tried += attempts.max(1);
    let Some((backend, link, reservation)) = joined else {
        warning!(
            "{}: {name}: lost backend \"{lost}\" ({what}); no other server took them",
            c.peer
        );
        return Err(sorry(if balanced.full {
            "The other servers are full right now."
        } else {
            "No other server could take you right now."
        }));
    };
    run.since = Some(Instant::now());
    info!(
        "{}: {name}: lost backend \"{lost}\" ({what}); moved to \"{}\"",
        c.peer, backend.name
    );
    let notice = match loss {
        Loss::Kicked(reason) => {
            format!(
                "You were disconnected from {lost}: {reason}. You were moved to {}.",
                backend.name
            )
        }
        Loss::Crashed(_) => format!(
            "You lost the connection to {lost}. You were moved to {}.",
            backend.name
        ),
    };
    Ok(Replacement {
        link,
        backend: backend.name.clone(),
        notice: Some(notice),
        keep: reservation.map(|r| Box::new(r) as Box<dyn std::any::Any + Send>),
    })
}

#[cfg(test)]
mod tests;
