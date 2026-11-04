//! `drain <backend> --move`: sends the players of a drained backend to other backends
//! of the same role, one at a time, with a pause between two moves so that the targets
//! and their sidecars are not hit all at once. A player with nowhere to go stays where
//! they are and is listed in the summary.
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
}

impl Outcome {
    /// The lines the console shows: each move, then a summary that names who stayed.
    pub fn report(&self, backend: &str) -> Vec<String> {
        let mut lines = self.lines.clone();
        lines.push(format!(
            "{backend}: {} moved, {} stayed",
            self.moved,
            self.stayed.len()
        ));
        lines.extend(
            self.stayed
                .iter()
                .map(|(name, why)| format!("  {name} stays on {backend}: {why}")),
        );
        lines
    }
}

/// Moves `players` (those on `backend`) away from it. `mover` moves one player to a
/// backend and returns the line to show, or why it failed; the place reserved on the
/// target is kept until it returns. `sleep` waits `PAUSE` between two moves.
pub fn run(
    config: &Config,
    shared: &Shared,
    backend: &str,
    players: &[Player],
    mover: &mut dyn FnMut(&Player, &str) -> Result<String, String>,
    pause: Duration,
    sleep: &mut dyn FnMut(Duration),
) -> Outcome {
    let mut out = Outcome::default();
    let mut tried = false;
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
            Err(why) => {
                out.lines.push(why.clone());
                out.stayed
                    .push((player.name.clone(), "the move failed".to_owned()));
            }
        }
        // The place is given back here, once the move has ended.
        drop(place);
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
/// failed.
fn move_one(
    sessions: &Sessions,
    config: &Config,
    player: &Player,
    target: &str,
) -> Result<String, String> {
    let Some((player, handle)) = sessions.session(&player.name) else {
        return Err(format!("{} left before the switch", player.name));
    };
    match switching::switch(
        config,
        &sessions.switcher,
        &handle,
        &player.name,
        player.address.ip(),
        target,
    ) {
        Ok(moved) => {
            let line = moved.describe(&player.name);
            info!("{line}");
            handle.tell(&format!("You are now on {}.", moved.to));
            Ok(line)
        }
        Err(why) => {
            warning!("{why}");
            handle.tell(&format!("Could not move you to {target}. {why}"));
            Err(why)
        }
    }
}

#[cfg(test)]
mod tests;
