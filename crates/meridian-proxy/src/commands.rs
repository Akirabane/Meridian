//! The commands players type in game that the proxy answers itself (`[commands]`):
//! `/server` lists the backends, `/server <name>` moves the player to one, and
//! `/hub` or `/lobby` to the hub backend; and for those who have the permission,
//! `/send`, `/glist` and `/alert` (`staff.rs`). Any other command, or one of these that
//! is turned off, goes to the backend.
//!
//! What a player may use is decided by their permissions (`meridian-config`'s
//! `Permissions`): the first three are open to everyone until a group denies them, the
//! staff commands to those who are given them.
use crate::route;
use meridian_config::{AuthKind, Config, Permissions, ProxyCommand, Role, node};
use meridian_net::relay::CommandHook;
use std::sync::Arc;

/// Who typed a command, and where they are.
#[derive(Clone, Debug)]
pub struct Typist {
    pub auth: AuthKind,
    /// The backend they are on.
    pub backend: String,
    /// Who they are, for their permissions.
    pub name: String,
    pub uuid: [u8; 16],
    /// The permissions in force when they typed the command.
    pub permissions: Arc<Permissions>,
}

impl Typist {
    /// Whether the player has the permission `node`.
    pub fn has(&self, node: &str) -> bool {
        self.permissions.allows(&self.name, self.uuid, node)
    }
}

/// The players a `/send` names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SendTarget {
    /// Every player online.
    All,
    /// The players on this server (`server:name`).
    Server(String),
    /// The player called this.
    Player(String),
}

/// What the proxy does with a command it answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    /// Shows the player this line.
    Say(String),
    /// Moves the player to this backend.
    Move(String),
    /// Moves the player to the lobby the balancer finds for them.
    Hub,
    /// Moves the player to the server of role `game` the balancer finds for them.
    Play,
    /// `/send`: moves players to a server.
    Send { who: SendTarget, server: String },
    /// `/glist`: lists the players, of one server or of all.
    Glist(Option<String>),
    /// `/alert`: shows a message to the players of one server or of all.
    Alert {
        server: Option<String>,
        message: String,
    },
}

/// The hook the session asks about each command its player types. `typist`
/// says who they are and where they are now; `mover` starts a move and returns
/// the line the player sees at once.
pub fn hook(
    config: Arc<Config>,
    typist: impl Fn() -> Typist + Send + Sync + 'static,
    mover: impl Fn(&str) -> String + Send + Sync + 'static,
    placer: impl Fn(Role, &Typist) -> String + Send + Sync + 'static,
    staff: impl Fn(&Typist, Answer) -> String + Send + Sync + 'static,
) -> Arc<CommandHook> {
    Arc::new(move |command: &str| {
        let typist = typist();
        match answer(&config, &typist, command)? {
            Answer::Say(line) => Some(line),
            Answer::Move(target) => Some(mover(&target)),
            Answer::Hub => Some(placer(Role::Lobby, &typist)),
            Answer::Play => Some(placer(Role::Game, &typist)),
            answer => Some(staff(&typist, answer)),
        }
    })
}

/// The proxy's answer to `command` (without its slash), or `None` when the
/// command is the backend's.
pub fn answer(config: &Config, typist: &Typist, command: &str) -> Option<Answer> {
    let mut words = command.split_whitespace();
    let first = words.next()?;
    let rest: Vec<&str> = words.collect();
    let settings = &config.commands;
    let denied = || {
        Some(Answer::Say(format!(
            "You do not have permission to use /{first}."
        )))
    };
    if settings.is_enabled(ProxyCommand::Server) && ProxyCommand::Server.words().contains(&first) {
        if !typist.has(node::SERVER) {
            return denied();
        }
        return Some(match rest.as_slice() {
            [] => Answer::Say(list(config, typist)),
            [name] => move_to(config, typist, name),
            _ => Answer::Say("Usage: /server [name]".to_owned()),
        });
    }
    if settings.is_enabled(ProxyCommand::Hub) && ProxyCommand::Hub.words().contains(&first) {
        if !typist.has(node::HUB) {
            return denied();
        }
        return Some(match rest.as_slice() {
            // With balancing on and no hub written, the balancer picks the lobby.
            [] if config.balancing.enabled
                && config.commands.hub_backend.is_none()
                && config.servers_with_role(Role::Lobby).next().is_some() =>
            {
                Answer::Hub
            }
            [] => move_to(config, typist, config.hub_backend()),
            _ => Answer::Say(format!("Usage: /{first}")),
        });
    }
    if settings.is_enabled(ProxyCommand::Play) && ProxyCommand::Play.words().contains(&first) {
        if !typist.has(node::PLAY) {
            return denied();
        }
        return Some(match rest.as_slice() {
            // The balancer places players on servers of role `game` only when it is on.
            [] if config.balancing.enabled
                && config.servers_with_role(Role::Game).next().is_some() =>
            {
                Answer::Play
            }
            [] => Answer::Say("There is no game server to play on.".to_owned()),
            _ => Answer::Say(format!("Usage: /{first}")),
        });
    }
    if settings.is_enabled(ProxyCommand::Send) && ProxyCommand::Send.words().contains(&first) {
        if !typist.has(node::SEND) {
            return denied();
        }
        return Some(match rest.as_slice() {
            [who, server] => Answer::Send {
                who: send_target(who),
                server: (*server).to_owned(),
            },
            _ => Answer::Say("Usage: /send <player|all|server:name> <server>".to_owned()),
        });
    }
    if settings.is_enabled(ProxyCommand::Glist) && ProxyCommand::Glist.words().contains(&first) {
        if !typist.has(node::GLIST) {
            return denied();
        }
        return Some(match rest.as_slice() {
            [] => Answer::Glist(None),
            [server] => Answer::Glist(Some((*server).to_owned())),
            _ => Answer::Say("Usage: /glist [server]".to_owned()),
        });
    }
    if settings.is_enabled(ProxyCommand::Alert) && ProxyCommand::Alert.words().contains(&first) {
        if !typist.has(node::ALERT) {
            return denied();
        }
        return Some(alert(&rest));
    }
    None
}

/// Who `/send` names: `all`, `server:<name>`, or a player.
fn send_target(word: &str) -> SendTarget {
    if word.eq_ignore_ascii_case("all") {
        SendTarget::All
    } else if let Some(server) = word.strip_prefix("server:") {
        SendTarget::Server(server.to_owned())
    } else {
        SendTarget::Player(word.to_owned())
    }
}

/// `/alert [server:name] <message>`: the server may also end the line.
fn alert(words: &[&str]) -> Answer {
    let usage = || Answer::Say("Usage: /alert [server:name] <message>".to_owned());
    let (server, message) = match words {
        [first, message @ ..] if first.starts_with("server:") => (Some(&first[7..]), message),
        [message @ .., last] if last.starts_with("server:") && !message.is_empty() => {
            (Some(&last[7..]), message)
        }
        message => (None, message),
    };
    if message.is_empty() || server.is_some_and(str::is_empty) {
        return usage();
    }
    Answer::Alert {
        server: server.map(str::to_owned),
        message: message.join(" "),
    }
}

/// Whether `typist` may be sent to `backend`: a backend in a group for the other
/// kind of player is closed to them.
pub fn open_to(config: &Config, backend: &str, typist: &Typist) -> bool {
    config.backend(backend).is_some() && !route::serves_other_kind(config, backend, typist.auth)
}

fn list(config: &Config, typist: &Typist) -> String {
    if !config.commands.server_list {
        return "The list of servers is not available.".to_owned();
    }
    let names: Vec<String> = config
        .backends
        .iter()
        .filter(|b| open_to(config, &b.name, typist) && typist.has(&node::server(&b.name)))
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
    if !typist.has(&node::server(target)) {
        return Answer::Say(format!("You do not have permission to go to {target}."));
    }
    if target == typist.backend {
        return Answer::Say(format!("You are already on {target}."));
    }
    Answer::Move(target.to_owned())
}

#[cfg(test)]
mod tests;
