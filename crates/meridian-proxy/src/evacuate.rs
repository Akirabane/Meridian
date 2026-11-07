//! `drain <backend> --move`: sends the players of a drained backend to other backends
//! of the same role, one at a time, with a pause between two moves so that the targets
//! and their sidecars are not hit all at once. A player with nowhere to go stays where
//! they are and is listed in the summary; a player who is no longer on the backend when
//! their turn comes (the server stopped first and the fallback moved them) is counted apart.
//!
//! The choice of the target is the balancer's ([`crate::placing::elsewhere`]); the move
//! itself is `mover`'s, so the loop is tested here without a sidecar or a socket.
use crate::balance::Reservation;
use crate::log::{info, warning};
use crate::placing;
use crate::session::{Player, Sessions, Shared};
use crate::switching;
use meridian_config::Config;
use std::time::Duration;

/// The pause between the start of one move and the start of the next.
pub const PAUSE: Duration = Duration::from_millis(500);

/// What an evacuation did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// One line for each player who was moved or whose move failed.
    pub lines: Vec<String>,
    /// How many players were moved.
    pub moved: usize,
    /// The players who stayed, with why: no target, or a move that failed.
    pub stayed: Vec<(String, String)>,
    /// The players who were no longer on the backend when their turn came: the server
    /// stopped first and the fallback had moved them. With the server they are on now.
    pub already: Vec<(String, String)>,
}

/// Why the evacuation did not move a player.
#[derive(Debug, PartialEq, Eq)]
pub enum Unmoved {
    /// The move failed, or the player left first: the line that says why.
    Failed(String),
    /// The player is no longer on the backend being left: the fallback moved them. The
    /// server they are on now.
    Elsewhere(String),
}

impl Outcome {
    /// How many were moved, how many stayed, and, when there are some, how many were
    /// already moved by the fallback: the same words in the console and in the log.
    pub fn counts(&self) -> String {
        let mut counts = format!("{} moved, {} stayed", self.moved, self.stayed.len());
        if !self.already.is_empty() {
            counts.push_str(&format!(
                ", {} already moved by the fallback",
                self.already.len()
            ));
        }
        counts
    }

    /// One line for each player the fallback had already moved.
    pub fn already_lines(&self) -> Vec<String> {
        self.already
            .iter()
            .map(|(name, now)| format!("{name} is already on {now} (moved by the fallback)"))
            .collect()
    }

    /// The lines the console shows: each move, then a summary that names who stayed and
    /// who was already moved by the fallback.
    pub fn report(&self, backend: &str) -> Vec<String> {
        let mut lines = self.lines.clone();
        lines.push(format!("{backend}: {}", self.counts()));
        lines.extend(
            self.stayed
                .iter()
                .map(|(name, why)| format!("  {name} stays on {backend}: {why}")),
        );
        lines.extend(
            self.already_lines()
                .into_iter()
                .map(|line| format!("  {line}")),
        );
        lines
    }
}

/// Moves `players` (those on `backend`) away from it. `mover` moves one player to a
/// backend and returns the line to show, or why it did not; the place reserved on the
/// target is kept until it returns. `sleep` waits `PAUSE` between two moves.
pub fn run(
    config: &Config,
    shared: &Shared,
    backend: &str,
    players: &[Player],
    mover: &mut dyn FnMut(&Player, &str) -> Result<String, Unmoved>,
    pause: Duration,
    sleep: &mut dyn FnMut(Duration),
) -> Outcome {
    let mut out = Outcome::default();
    let mut tried = false;
    let affected = players.iter().filter(|p| p.backend == backend).count();
    for player in players.iter().filter(|p| p.backend == backend) {
        let place: Reservation = match placing::elsewhere(config, shared, backend) {
            Ok(place) => place,
            Err(why) => {
                out.stayed.push((player.name.clone(), why));
                continue;
            }
        };
        if tried {
            sleep(pause);
        }
        tried = true;
        match mover(player, place.backend()) {
            Ok(line) => {
                out.moved += 1;
                out.lines.push(line);
            }
            Err(Unmoved::Failed(why)) => {
                out.lines.push(why);
                out.stayed
                    .push((player.name.clone(), "the move failed".to_owned()));
            }
            Err(Unmoved::Elsewhere(now)) => out.already.push((player.name.clone(), now)),
        }
        // The place is given back here, once the move has ended.
        drop(place);
    }
    if affected > 0 {
        shared.metrics.evacuation(out.moved, out.stayed.len());
    }
    out
}

/// Sends the players who are on `server` now to other servers of its role, one at a time,
/// and waits for the last move. `None` when nobody is on it. The console's `drain --move`
/// and the evacuation of a backend that is going down (docs/RESTART.md) are this.
pub fn send_away(sessions: &Sessions, config: &Config, server: &str) -> Option<Outcome> {
    let players: Vec<Player> = sessions
        .players()
        .into_iter()
        .filter(|p| p.backend == server)
        .collect();
    if players.is_empty() {
        return None;
    }
    Some(run(
        config,
        &sessions.shared(),
        server,
        &players,
        &mut |player, target| move_one(sessions, config, player, target),
        PAUSE,
        &mut std::thread::sleep,
    ))
}

/// Moves one player to `target` and tells them how it went: the console's line, or why it
/// did not. `player` is the player as the evacuation saw them when it began, on the backend
/// being left.
fn move_one(
    sessions: &Sessions,
    config: &Config,
    player: &Player,
    target: &str,
) -> Result<String, Unmoved> {
    let Some((current, handle)) = sessions.session(&player.name) else {
        return Err(Unmoved::Failed(format!(
            "{} left before the switch",
            player.name
        )));
    };
    match switching::switch(
        config,
        &sessions.switcher,
        &handle,
        &current.name,
        current.address.ip(),
        target,
    ) {
        Ok(moved) => {
            let line = moved.describe(&current.name);
            info!("{line}");
            handle.tell(&format!("You are now on {}.", moved.to));
            Ok(line)
        }
        Err(why) => {
            // The server may have stopped while the evacuation waited for this player's turn,
            // and the fallback moved them: they are on another server and nothing went wrong.
            // The fallback told the player, and the summary says it.
            if let Some((now, _)) = sessions.session(&player.name) {
                if now.backend != player.backend {
                    return Err(Unmoved::Elsewhere(now.backend));
                }
            }
            warning!("{why}");
            handle.tell(&format!("Could not move you to {target}. {why}"));
            Err(Unmoved::Failed(why))
        }
    }
}

#[cfg(test)]
mod tests;
