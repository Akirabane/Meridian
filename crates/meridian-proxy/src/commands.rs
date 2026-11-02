//! The commands players type in game that the proxy answers itself (`[commands]`):
//! `/server` lists the backends, `/server <name>` moves the player to one, and
//! `/hub` or `/lobby` to the hub backend. Any other command, or one of these that
//! is turned off, goes to the backend.
use crate::route;
use meridian_config::{AuthKind, Config, ProxyCommand};
use meridian_net::relay::CommandHook;
use std::sync::Arc;

/// Who typed a command, and where they are.
#[derive(Clone, Debug)]
pub struct Typist {
    pub auth: AuthKind,
    /// The backend they are on.
    pub backend: String,
}

/// What the proxy does with a command it answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    /// Shows the player this line.
    Say(String),
    /// Moves the player to this backend.
    Move(String),
}

/// The hook the session asks about each command its player types. `typist`
/// says who they are and where they are now; `mover` starts a move and returns
/// the line the player sees at once.
pub fn hook(
    config: Arc<Config>,
    typist: impl Fn() -> Typist + Send + Sync + 'static,
    mover: impl Fn(&str) -> String + Send + Sync + 'static,
) -> Arc<CommandHook> {
    Arc::new(
        move |command: &str| match answer(&config, &typist(), command)? {
            Answer::Say(line) => Some(line),
            Answer::Move(target) => Some(mover(&target)),
        },
    )
}

/// The proxy's answer to `command` (without its slash), or `None` when the
/// command is the backend's.
pub fn answer(config: &Config, typist: &Typist, command: &str) -> Option<Answer> {
    let mut words = command.split_whitespace();
    let first = words.next()?;
    let rest: Vec<&str> = words.collect();
    let settings = &config.commands;
    if settings.is_enabled(ProxyCommand::Server) && ProxyCommand::Server.words().contains(&first) {
        return Some(match rest.as_slice() {
            [] => Answer::Say(list(config, typist)),
            [name] => move_to(config, typist, name),
            _ => Answer::Say("Usage: /server [name]".to_owned()),
        });
    }
    if settings.is_enabled(ProxyCommand::Hub) && ProxyCommand::Hub.words().contains(&first) {
        return Some(match rest.as_slice() {
            [] => move_to(config, typist, config.hub_backend()),
            _ => Answer::Say(format!("Usage: /{first}")),
        });
    }
    None
}

/// Whether `typist` may be sent to `backend`: a backend in a group for the other
/// kind of player is closed to them.
fn open_to(config: &Config, backend: &str, typist: &Typist) -> bool {
    config.backend(backend).is_some() && !route::serves_other_kind(config, backend, typist.auth)
}

fn list(config: &Config, typist: &Typist) -> String {
    if !config.commands.server_list {
        return "The list of servers is not available.".to_owned();
    }
    let names: Vec<String> = config
        .backends
        .iter()
        .filter(|b| open_to(config, &b.name, typist))
        .map(|b| {
            if b.name == config.default_backend {
                format!("{} (default)", b.name)
            } else {
                b.name.clone()
            }
        })
        .collect();
    format!(
        "Servers: {}. You are on {}.",
        names.join(", "),
        typist.backend
    )
}

fn move_to(config: &Config, typist: &Typist, target: &str) -> Answer {
    if !open_to(config, target, typist) {
        return Answer::Say(format!("There is no server called {target}."));
    }
    if target == typist.backend {
        return Answer::Say(format!("You are already on {target}."));
    }
    Answer::Move(target.to_owned())
}

#[cfg(test)]
mod tests;
