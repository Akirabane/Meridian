//! What the console and the staff commands in game (`/send`, `/glist`, `/alert`) share:
//! moving players, listing them by server, and showing them a message. The console calls
//! these as the operator; in game `commands.rs` decides, from the player's permissions,
//! whether the command is theirs to use, then [`run`] does it.
use crate::commands::{Answer, SendTarget, Typist};
use crate::log::{info, warning};
use crate::session::{Player, Shared};
use crate::switching::{self, Switcher};
use meridian_config::Config;
use meridian_handoff::SessionHandle;
use std::sync::Arc;

/// What an alert is shown after.
const ALERT_PREFIX: &str = "[Alert] ";

/// Moves each of `moving` to `backend`, one after the other: a line for each, which says how
/// it went or at which step it failed, and how many failed. Each player is told too.
pub fn move_all(
    config: &Config,
    shared: &Shared,
    switcher: &Switcher,
    moving: &[Player],
    backend: &str,
) -> (Vec<String>, usize) {
    let mut lines = Vec::new();
    let mut failed = 0;
    for player in moving {
        let Some((player, handle)) = shared.session(&player.name) else {
            lines.push(format!("{} left before the switch", player.name));
            continue;
        };
        match switching::switch(
            config,
            switcher,
            &handle,
            &player.name,
            player.address.ip(),
            backend,
        ) {
            Ok(moved) => {
                let line = moved.describe(&player.name);
                info!("{line}");
                handle.tell(&format!("You are now on {}.", moved.to));
                lines.push(line);
                // Already in the log; the console says it too.
                lines.extend(moved.alert);
            }
            Err(why) => {
                warning!("{why}");
                handle.tell(&format!("Could not move you to {backend}. {why}"));
                lines.push(why);
                failed += 1;
            }
        }
    }
    (lines, failed)
}

/// The players a `send` names: one by name, every one, or those on a server.
pub fn pick(shared: &Shared, config: &Config, who: &SendTarget) -> Result<Vec<Player>, String> {
    match who {
        SendTarget::All => Ok(shared.players()),
        SendTarget::Server(name) => {
            let backend = config
                .backend(name)
                .ok_or_else(|| format!("there is no server called {name}"))?;
            Ok(shared
                .players()
                .into_iter()
                .filter(|p| p.backend == backend.name)
                .collect())
        }
        SendTarget::Player(name) => shared
            .session(name)
            .map(|(player, _)| vec![player])
            .ok_or_else(|| format!("{name} is not online")),
    }
}

/// The players online, by server: a line for each server that has some, or for `server`
/// alone, which must be a configured one.
pub fn glist(
    shared: &Shared,
    config: &Config,
    server: Option<&str>,
) -> Result<Vec<String>, String> {
    let players = shared.players();
    let on = |backend: &str| -> Vec<&str> {
        players
            .iter()
            .filter(|p| p.backend == backend)
            .map(|p| p.name.as_str())
            .collect()
    };
    let line = |backend: &str| {
        let names = on(backend);
        if names.is_empty() {
            format!("{backend} (0)")
        } else {
            format!("{backend} ({}): {}", names.len(), names.join(", "))
        }
    };
    if let Some(server) = server {
        let backend = config
            .backend(server)
            .ok_or_else(|| format!("there is no server called {server}"))?;
        return Ok(vec![line(&backend.name)]);
    }
    if players.is_empty() {
        return Ok(vec!["no players online".to_owned()]);
    }
    let mut lines = vec![match players.len() {
        1 => "1 player online:".to_owned(),
        n => format!("{n} players online:"),
    }];
    lines.extend(
        config
            .backends
            .iter()
            .filter(|b| !on(&b.name).is_empty())
            .map(|b| line(&b.name)),
    );
    Ok(lines)
}

/// Shows `message` in the chat of every player online, or of those on `server`, and returns
/// how many were told.
pub fn alert(
    shared: &Shared,
    config: &Config,
    server: Option<&str>,
    message: &str,
) -> Result<usize, String> {
    alert_counted(shared, config, server, message).map(|(found, _)| found)
}

/// [`alert`], counting the players **shown** the message: those whose session was in the play phase (a session that is not
/// reads no chat). For the orders of an extension, which tell it how many were shown.
pub fn alert_shown(
    shared: &Shared,
    config: &Config,
    server: Option<&str>,
    message: &str,
) -> Result<usize, String> {
    alert_counted(shared, config, server, message).map(|(_, shown)| shown)
}

/// The alert: how many players it found, and how many of them were shown it.
fn alert_counted(
    shared: &Shared,
    config: &Config,
    server: Option<&str>,
    message: &str,
) -> Result<(usize, usize), String> {
    let backend = match server {
        Some(name) => Some(
            config
                .backend(name)
                .ok_or_else(|| format!("there is no server called {name}"))?
                .name
                .as_str(),
        ),
        None => None,
    };
    let told: Vec<SessionHandle> = shared
        .sessions()
        .into_iter()
        .filter(|(player, _)| backend.is_none_or(|b| player.backend == b))
        .map(|(_, handle)| handle)
        .collect();
    let line = format!("{ALERT_PREFIX}{message}");
    let shown = told.iter().filter(|handle| handle.try_tell(&line)).count();
    Ok((told.len(), shown))
}

/// Where a command typed in game is run from: the player's own session and what the proxy
/// shares.
pub struct Context {
    pub config: Arc<Config>,
    pub shared: Shared,
    pub switcher: Arc<Switcher>,
    pub handle: SessionHandle,
}

/// What the console says in lower case, as a sentence for a player: "there is no server
/// called x" begins with a capital; a line that begins with a name keeps it as typed.
fn sentence(text: &str) -> String {
    match text.strip_prefix("there ") {
        Some(rest) => format!("There {rest}."),
        None => format!("{text}."),
    }
}

/// Runs a staff command that `commands.rs` found the player has the permission for, and
/// returns the line they are shown at once. `/send` goes on in a thread of its own, since a
/// move waits for the servers; the player is told how it ended.
pub fn run(context: &Context, typist: &Typist, answer: Answer) -> String {
    let (config, shared) = (&context.config, &context.shared);
    match answer {
        Answer::Glist(server) => match glist(shared, config, server.as_deref()) {
            Ok(lines) => lines.join("\n"),
            Err(why) => sentence(&why),
        },
        Answer::Alert { server, message } => {
            match alert(shared, config, server.as_deref(), &message) {
                Ok(told) => {
                    info!(
                        "{} alerted {} to {told}: {message}",
                        typist.name,
                        server.as_deref().unwrap_or("every server")
                    );
                    match told {
                        1 => "Alert shown to 1 player.".to_owned(),
                        n => format!("Alert shown to {n} players."),
                    }
                }
                Err(why) => sentence(&why),
            }
        }
        Answer::Send { who, server } => {
            let Some(backend) = config.backend(&server) else {
                return format!("There is no server called {server}.");
            };
            let players = match pick(shared, config, &who) {
                Ok(players) => players,
                Err(why) => return sentence(&why),
            };
            let moving: Vec<Player> = players
                .iter()
                .filter(|p| p.backend != backend.name)
                .cloned()
                .collect();
            if moving.is_empty() {
                return if players.is_empty() {
                    "There is no one to send.".to_owned()
                } else {
                    format!("Already on {}.", backend.name)
                };
            }
            let target = backend.name.clone();
            info!(
                "{} sends {} to {target}",
                typist.name,
                match moving.len() {
                    1 => moving[0].name.clone(),
                    n => format!("{n} players"),
                }
            );
            let started = format!(
                "Sending {} to {target}...",
                match moving.len() {
                    1 => "1 player".to_owned(),
                    n => format!("{n} players"),
                }
            );
            let (config, shared, switcher, handle) = (
                Arc::clone(&context.config),
                context.shared.clone(),
                Arc::clone(&context.switcher),
                context.handle.clone(),
            );
            let spawned = std::thread::Builder::new()
                .name(format!("send-{}", typist.name))
                .spawn(move || {
                    let (_, failed) = move_all(&config, &shared, &switcher, &moving, &target);
                    handle.tell(&match failed {
                        0 => format!("Sent to {target}."),
                        n => format!("Sent to {target}, {n} failed; the log says why."),
                    });
                });
            match spawned {
                Ok(_) => started,
                Err(e) => {
                    warning!("cannot start a thread for /send: {e}");
                    "Cannot send players right now.".to_owned()
                }
            }
        }
        // The other answers are not staff commands.
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests;
