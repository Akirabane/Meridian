//! The commands players type in game that the proxy answers itself (`[commands]`):
//! `/server` lists the backends, `/server <name>` asks to move to one, and `/hub`
//! or `/lobby` ask to move to the hub backend. Moving is not there yet, so a move
//! is answered the way the console's `send` answers it. Any other command, or one
//! of these that is turned off, goes to the backend.
use crate::route;
use meridian_config::{AuthKind, Config, ProxyCommand};
use meridian_net::relay::CommandHook;
use std::sync::Arc;

/// Who typed a command, and where they are.
#[derive(Clone, Debug)]
pub struct Typist {
    pub name: String,
    pub auth: AuthKind,
    /// The backend they are on.
    pub backend: String,
}

/// The hook the relay asks about each command `typist` types.
pub fn hook(config: Arc<Config>, typist: Typist) -> Arc<CommandHook> {
    Arc::new(move |command: &str| answer(&config, &typist, command))
}

/// The proxy's answer to `command` (without its slash), or `None` when the
/// command is the backend's.
pub fn answer(config: &Config, typist: &Typist, command: &str) -> Option<String> {
    let mut words = command.split_whitespace();
    let first = words.next()?;
    let rest: Vec<&str> = words.collect();
    let settings = &config.commands;
    if settings.is_enabled(ProxyCommand::Server) && ProxyCommand::Server.words().contains(&first) {
        return Some(match rest.as_slice() {
            [] => list(config, typist),
            [name] => move_to(config, typist, name),
            _ => "Usage: /server [name]".to_owned(),
        });
    }
    if settings.is_enabled(ProxyCommand::Hub) && ProxyCommand::Hub.words().contains(&first) {
        return Some(match rest.as_slice() {
            [] => move_to(config, typist, config.hub_backend()),
            _ => format!("Usage: /{first}"),
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

fn move_to(config: &Config, typist: &Typist, target: &str) -> String {
    if !open_to(config, target, typist) {
        return format!("There is no server called {target}.");
    }
    if target == typist.backend {
        return format!("You are already on {target}.");
    }
    // The same answer as the console's send until moving players is there.
    format!(
        "transfer not available yet: {} would move to {target}",
        typist.name
    )
}

#[cfg(test)]
mod tests;
