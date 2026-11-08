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
use crate::session::{Connect, Sessions, hyphenated};
use meridian_config::{AuthKind, Config};
use meridian_handoff::{Loss, Notice, Replacement, SessionHandle};
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
pub fn replacer<'a>(context: Context<'a>) -> impl FnMut(&Loss) -> Result<Replacement, Notice> + 'a {
    let mut run = Run::default();
    move |loss| {
        let from = context.handle.backend();
        let result = replace(&context, &mut run, loss);
        context.sessions.metrics().fallback(loss, result.is_ok());
        // Told to the extensions that listen: the server that was lost, why, and the one chosen for the player, if there is one.
        let to = result.as_ref().ok().map(|r| r.backend.clone());
        context
            .sessions
            .extensions
            .emit(|| crate::extension::events::Kind::Fallback {
                player: crate::extension::events::Kind::player(
                    &context.profile.name,
                    context.profile.uuid,
                    context.auth == AuthKind::Verified,
                ),
                from,
                to,
                loss: match loss {
                    Loss::Crashed(_) => meridian_extension::events::Loss::Crashed,
                    Loss::Kicked(_) => meridian_extension::events::Loss::Kicked,
                },
            });
        result
    }
}

fn replace(c: &Context<'_>, run: &mut Run, loss: &Loss) -> Result<Replacement, Notice> {
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
    // What the player is told when no server can take them. The reason of a kick is the
    // server's own component, so the client translates it.
    let sorry = |why: &str| -> Notice {
        match loss {
            Loss::Kicked(reason) => Notice::around("", reason, &format!("\n\n({why})")),
            Loss::Crashed(_) => format!("You lost the connection to {lost}. {why}").into(),
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
    // The extension that decides the choice of server, if there is one, may put the candidates in an
    // order of its own, or refuse to place the player (docs/EXTENSIONS.md, 4.4).
    let (candidates, ordered) = match crate::extension::server::choose(
        &c.sessions.extensions,
        &config,
        &c.sessions.shared(),
        &crate::extension::server::Who {
            name,
            uuid: hyphenated(c.profile.uuid),
            verified: c.auth == AuthKind::Verified,
            host: c.virtual_host,
            lost: Some(&lost),
        },
        &candidates,
    ) {
        crate::extension::server::Outcome::Default => (candidates, false),
        crate::extension::server::Outcome::Order(servers) => (servers, true),
        crate::extension::server::Outcome::Refuse { by, message } => {
            warning!(
                "{}: {name}: lost backend \"{lost}\" ({what}); the extension \"{by}\" refuses to place them",
                c.peer
            );
            return Err(sorry(&message));
        }
    };
    // With balancing on, the balancer places the player among the lobbies first. Servers an extension
    // ordered are tried in its order, each held a place by the rule of a server the player named.
    let mut balanced = if ordered {
        placing::ordered(
            &config,
            &c.sessions.shared(),
            c.connect,
            c.profile,
            &options,
            &candidates,
            &format!("{}: {name}", c.peer),
        )
    } else {
        placing::login(
            &config,
            &c.sessions.shared(),
            c.connect,
            c.profile,
            &options,
            &candidates,
            c.virtual_host,
        )
    };
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
    // Players still counted on the lost server: each session is handled in turn, and one that
    // was moved before this one is no longer on it.
    let on_it = c.sessions.counts().get(&lost).copied().unwrap_or(0);
    info!(
        "{}",
        moved_line(
            c.peer,
            &Moved {
                name,
                lost: &lost,
                what: &what,
                to: &backend.name,
            },
            now.elapsed(),
            attempts,
            on_it,
        )
    );
    let on_active = {
        let (peer, name, lost, to) = (c.peer, name.clone(), lost.clone(), backend.name.clone());
        Box::new(move || {
            info!("{}", followed_line(peer, &name, &lost, &to, now.elapsed()));
        }) as Box<dyn FnOnce() + Send>
    };
    let notice = match loss {
        Loss::Kicked(reason) => Notice::around(
            &format!("You were disconnected from {lost}: "),
            reason,
            &format!(". You were moved to {}.", backend.name),
        ),
        Loss::Crashed(_) => format!(
            "You lost the connection to {lost}. You were moved to {}.",
            backend.name
        )
        .into(),
    };
    Ok(Replacement {
        link,
        backend: backend.name.clone(),
        notice: Some(notice),
        keep: reservation.map(|r| Box::new(r) as Box<dyn std::any::Any + Send>),
        on_active: Some(on_active),
    })
}

/// Who was moved from where, for the two lines of the log that tell it.
struct Moved<'a> {
    name: &'a str,
    lost: &'a str,
    what: &'a str,
    to: &'a str,
}

/// The line of a player the proxy found another server for. `took` is the time from the loss
/// being heard to the other server having taken the player's login, `attempts` the servers
/// tried, and `on_it` the players counted on the lost server when this loss was handled.
fn moved_line(
    peer: SocketAddr,
    moved: &Moved<'_>,
    took: Duration,
    attempts: u32,
    on_it: usize,
) -> String {
    let Moved {
        name,
        lost,
        what,
        to,
    } = moved;
    format!(
        "{peer}: {name}: lost backend \"{lost}\" ({what}); moved to \"{to}\" in {} ms \
         (servers tried: {attempts}; players on \"{lost}\" then: {on_it})",
        took.as_millis()
    )
}

/// The line that follows it, when the player's game has followed: the time since the loss was
/// heard, until the client acknowledged and the other server's connection became the one it
/// plays on. Its configuration and its world still reach the player after that.
fn followed_line(peer: SocketAddr, name: &str, lost: &str, to: &str, since: Duration) -> String {
    format!(
        "{peer}: {name}: on \"{to}\" {} ms after the loss of \"{lost}\": the game followed",
        since.as_millis()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A run of losses ends when a server kept the player a minute, and not before.
    #[test]
    fn a_run_ends_when_a_server_keeps_the_player() {
        let start = Instant::now();
        let mut run = Run {
            lost: vec!["lobby".into()],
            tried: 2,
            since: Some(start),
        };
        run.settle(start + SETTLED - Duration::from_secs(1));
        assert_eq!((run.lost.len(), run.tried), (1, 2));
        run.settle(start + SETTLED);
        assert_eq!((run.lost.len(), run.tried), (0, 0));
        assert!(run.since.is_none());
        // No server has taken the player yet: nothing to settle.
        let mut run = Run {
            tried: 1,
            ..Run::default()
        };
        run.settle(start + SETTLED * 10);
        assert_eq!(run.tried, 1);
    }

    /// The first line keeps its start, so that what looked for it still finds it, and adds the
    /// time, the servers tried and the players on the lost server.
    #[test]
    fn the_line_of_a_move_says_how_long_it_took() {
        let peer: SocketAddr = "198.51.100.7:50123".parse().unwrap();
        let line = moved_line(
            peer,
            &Moved {
                name: "Alex",
                lost: "lobby",
                what: "the connection was lost: the connection closed",
                to: "east",
            },
            Duration::from_micros(41_900),
            2,
            3,
        );
        assert_eq!(
            line,
            "198.51.100.7:50123: Alex: lost backend \"lobby\" (the connection was lost: the \
             connection closed); moved to \"east\" in 41 ms (servers tried: 2; players on \
             \"lobby\" then: 3)"
        );
        assert!(line.contains("moved to \"east\""));
    }

    #[test]
    fn the_line_of_a_game_that_followed_says_when() {
        let peer: SocketAddr = "198.51.100.7:50123".parse().unwrap();
        assert_eq!(
            followed_line(peer, "Alex", "lobby", "east", Duration::from_millis(187)),
            "198.51.100.7:50123: Alex: on \"east\" 187 ms after the loss of \"lobby\": the \
             game followed"
        );
    }
}
