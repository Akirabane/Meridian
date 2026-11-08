//! The extension's own commands (docs/EXTENSIONS.md, 4.3, "The extension's own commands"): the commands an
//! extension that is granted `commands` declares in its hello. The proxy takes one from the player who
//! types it and **never passes it to the server**, as it does for its own; the extension runs it
//! (`meridian:command/run`), and the lines it answers are told to the player when they come.
//!
//! The call is not made on the thread that copies the player's packets: that thread takes the command
//! and goes on, and a thread of its own waits for the lines, at most the `timeout-ms` of
//! `[extension.commands]` (2000 ms by default), and tells them through the session's handle. A player has
//! one command running at a time, so that one player cannot hold the places of an extension that its
//! login and permission decisions need. A failure of a command never counts toward the extension's
//! breaker (an open one still stops the call): one slow command must not turn every login to the
//! on-error policy.
//!
//! **Which words are an extension's** is decided from the extensions' declarations in the order of the
//! configuration, not the order of the hellos (`Registry::claim`): a word that is a command of the proxy
//! that is enabled, or that an earlier extension has, is refused and logged once; a word that is not valid
//! is too. A command stays an extension's while the extension is configured, enabled and granted
//! `commands`, **also while it is down**: a player who types it then reads that it did not answer,
//! and not what a server says of a command it does not know (**provisional**: the user has yet to decide).

use super::command::{Gate, Typed, args_of, merge, player_of};
use super::{Link, Registry};
use crate::commands::Typist;
use crate::log::{debug, warning};
use meridian_config::{AuthKind, COMMANDS, Config, ProxyCommand, node};
use meridian_extension::decide::NodeVerdict;
use meridian_extension::run::{self, RunParams, RunRequest};
use meridian_extension::text;
use meridian_net::relay::TAKEN;
use serde_json::Value;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// What a player reads when the extension did not answer a command in time, or at all.
pub(crate) const NO_ANSWER: &str = "That command did not answer.";

/// What a player reads who types a command while another of theirs is still running.
pub(crate) const BUSY: &str = "Still working on your last command.";

/// The commands of the proxy, whose words an extension may not take while they are enabled.
const BUILT_IN: [ProxyCommand; 6] = [
    ProxyCommand::Server,
    ProxyCommand::Hub,
    ProxyCommand::Play,
    ProxyCommand::Send,
    ProxyCommand::Glist,
    ProxyCommand::Alert,
];

/// An extension's command that a typed word stands for.
pub(crate) struct Claim {
    pub link: Arc<Link>,
    /// The extension's name in the configuration.
    pub extension: String,
    /// The command's declared name: the one an alias stands for.
    pub command: String,
}

impl Registry {
    /// The command `word` stands for, if it is an extension's (see the module). A refusal is logged once for
    /// each hello, however often a player types the word.
    pub(crate) fn claim(&self, config: &Config, word: &str) -> Option<Claim> {
        let mut taken: Vec<String> = Vec::new();
        for extension in config.enabled_extensions().filter(|e| e.grants(COMMANDS)) {
            let Some(link) = self.link(&extension.name) else {
                continue;
            };
            for spec in link.declared() {
                let refuse = |what: &str, why: &str| {
                    self.refused(
                        &link,
                        &format!("the command word \"{}\" {why}", text::clean(what, 32)),
                    );
                };
                if let Err(why) = spec.check() {
                    refuse(&spec.name, &format!("is refused: {why}"));
                    continue;
                }
                let clash = |w: &str| -> Option<&'static str> {
                    if BUILT_IN
                        .iter()
                        .any(|b| config.commands.is_enabled(*b) && b.words().contains(&w))
                    {
                        Some("is a command of the proxy")
                    } else if taken.iter().any(|t| t == w) {
                        Some("is another extension's, or this one's, already")
                    } else {
                        None
                    }
                };
                if let Some(why) = clash(&spec.name) {
                    refuse(&spec.name, &format!("is refused: it {why}"));
                    continue;
                }
                let mut words = vec![spec.name.clone()];
                for alias in &spec.aliases {
                    match clash(alias) {
                        Some(why) => refuse(alias, &format!("is refused: it {why}")),
                        None => words.push(alias.clone()),
                    }
                }
                let found = words.iter().any(|w| w == word);
                taken.extend(words);
                if found {
                    return Some(Claim {
                        link: Arc::clone(&link),
                        extension: extension.name.clone(),
                        command: spec.name,
                    });
                }
            }
        }
        None
    }

    /// Logs `what` as a refusal of `link`'s declaration, once for each of its hellos.
    fn refused(&self, link: &Link, what: &str) {
        let key = format!("{}:{}:{what}", link.name(), link.epoch());
        let mut seen = self.refused.lock().unwrap_or_else(|e| e.into_inner());
        if seen.len() >= 1024 {
            seen.clear();
        }
        if seen.insert(key) {
            warning!("extension \"{}\": {what}", link.name());
        }
    }
}

/// A player's end of the extensions' commands: which of the commands they type are an extension's, and
/// the one they have running.
pub(crate) struct Own {
    registry: Arc<Registry>,
    /// The configuration in force now: a reload changes the extensions, their grants and their deadlines.
    config: Box<dyn Fn() -> Arc<Config> + Send + Sync>,
    /// Tells the player a line (`SessionHandle::tell`).
    tell: Arc<dyn Fn(&str) + Send + Sync>,
    running: Arc<AtomicBool>,
}

/// A command is running for the player; it is not any more when this goes, however the thread ends.
struct Running(Arc<AtomicBool>);

impl Drop for Running {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl Own {
    pub(crate) fn new(
        registry: Arc<Registry>,
        config: impl Fn() -> Arc<Config> + Send + Sync + 'static,
        tell: impl Fn(&str) + Send + Sync + 'static,
    ) -> Self {
        Self {
            registry,
            config: Box::new(config),
            tell: Arc::new(tell),
            running: Arc::new(AtomicBool::new(false)),
        }
    }

    /// What the hook of the player's commands answers to `line` (the command without its slash) if it is a
    /// command of an extension: `None` when it is not (it is the proxy's own, or the server's). A command the
    /// player may not use is answered at once; one that may be used is **taken** ([`TAKEN`]): the hook
    /// returns at once and the lines come later.
    ///
    /// The permission is the node `ext.<extension>.<command>` through the same chain as the proxy's own
    /// commands: `permissions.toml` and the operators of the server, and what the extension that holds
    /// `decide.command` says if it is the extension whose command this is (what a player typed after the
    /// command is not told to another).
    pub(crate) fn take(&self, typist: &Typist, line: &str, gate: Option<&Gate>) -> Option<String> {
        let first = line.split_whitespace().next()?;
        let config = (self.config)();
        let claim = self.registry.claim(&config, first)?;
        let node = node::extension(&claim.extension, &claim.command);
        let typed = Typed {
            name: &typist.name,
            uuid: crate::session::hyphenated(typist.uuid),
            verified: typist.auth == AuthKind::Verified,
            server: &typist.backend,
            line,
        };
        let verdict = gate
            .and_then(|gate| {
                gate.verdicts_of(&typed, std::slice::from_ref(&node), Some(&claim.extension))
                    .get(&node)
                    .copied()
            })
            .unwrap_or(NodeVerdict::Default);
        if !merge(typist.has(&node), verdict) {
            return Some(format!("You do not have permission to use /{first}."));
        }
        if self.running.swap(true, Ordering::SeqCst) {
            return Some(BUSY.to_owned());
        }
        let running = Running(Arc::clone(&self.running));
        let timeout = config
            .extensions
            .iter()
            .find(|e| e.name == claim.extension)
            .map_or(Duration::from_secs(2), |e| {
                Duration::from_millis(e.own_commands.timeout_ms)
            });
        let params = serde_json::to_value(RunParams {
            request: RunRequest {
                player: player_of(&typed),
                server: typist.backend.clone(),
                command: claim.command.clone(),
                args: args_of(&typed),
            },
        })
        .unwrap_or(Value::Null);
        let tell = Arc::clone(&self.tell);
        let name = claim.extension;
        let link = claim.link;
        let spawned = std::thread::Builder::new()
            .name(format!("extension-{name}-run"))
            .spawn(move || {
                let _running = running;
                match link.call_counting(
                    run::RUN,
                    Some(&params),
                    timeout,
                    run::parse_run_answer,
                    false,
                ) {
                    Ok(lines) => lines.iter().for_each(|line| tell(line)),
                    Err(why) => {
                        debug!("extension \"{name}\" gave no lines for a command: {why}");
                        tell(NO_ANSWER);
                    }
                }
            });
        match spawned {
            Ok(_) => Some(TAKEN.to_owned()),
            Err(e) => {
                // The closure that was not run held the guard: it is dropped with it.
                warning!("cannot start a thread for an extension's command: {e}");
                Some(NO_ANSWER.to_owned())
            }
        }
    }
}
