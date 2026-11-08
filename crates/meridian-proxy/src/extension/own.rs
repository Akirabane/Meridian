//! The extension's own commands (docs/EXTENSIONS.md, 4.3, "The extension's own commands"): the commands an
//! extension that is granted `commands` declares in its hello. The proxy takes one from the player who
//! types it and **never passes it to the server**, as it does for its own; the extension runs it
//! (`meridian:command/run`), and the lines it answers are told to the player when they come.
//!
//! The call is not made on the thread that copies the player's packets: that thread takes the command
//! and goes on, and a thread of its own waits for the lines, at most the `timeout-ms` of
//! `[extension.commands]` (2000 ms by default), and tells them through the session's handle. A player has
//! one command running at a time, and an extension has at most half of its `max-in-flight` places running
//! commands (`Link::run_place`), so that neither one player nor many can hold the places that its login and
//! permission decisions need. A failure of a command never counts toward the extension's
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
use std::collections::HashMap;
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
#[derive(Clone)]
pub(crate) struct Claim {
    pub link: Arc<Link>,
    /// The extension's name in the configuration.
    pub extension: String,
    /// The command's declared name: the one an alias stands for.
    pub command: String,
}

/// The words the extensions hold, resolved when something they depend on changes and not for each command a
/// player types (`Registry::claim`).
pub(crate) struct Resolved {
    /// What it was resolved for: the extensions that are enabled and granted `commands`, in the order of
    /// the configuration, each with **the link it was resolved with** and how many hellos that link has been
    /// through (`None` for one that is not running), and which of the proxy's commands are enabled. The link
    /// itself, not only its count of hellos: a reload that changes an extension's connection settings
    /// replaces its link with a new one whose count starts again, and a count alone would take the new
    /// link's first hello for the old link's. The stamp holds the link, so that none can be freed and its
    /// address given to another.
    stamp: (Vec<Seen>, u8),
    words: HashMap<String, Claim>,
}

/// An extension the words were resolved for: its name, and its link with the number of hellos it had been
/// through, if it was running.
type Seen = (String, Option<(Arc<Link>, u64)>);

impl Registry {
    /// The command `word` stands for, if it is an extension's (see the module). The words are resolved again
    /// when an extension says hello, when a reload replaces its link (its connection settings changed), when
    /// the order of the extensions or which of them have `commands` changes, and when the proxy's commands
    /// that are enabled change; for every command a player types after that they are only looked up: most
    /// are the server's, and cost the comparison of what the words were resolved for. The cache is the
    /// registry's, and its lock is taken for each command typed: briefly, and on purpose.
    pub(crate) fn claim(&self, config: &Config, word: &str) -> Option<Claim> {
        let granted = || config.enabled_extensions().filter(|e| e.grants(COMMANDS));
        if granted().next().is_none() {
            // No extension has commands: what was resolved, and the links it holds, are let go.
            self.forget_words();
            return None;
        }
        let mask = built_in_mask(config);
        let mut cache = self.resolved.lock().unwrap_or_else(|e| e.into_inner());
        let current = cache.as_ref().is_some_and(|resolved| {
            let (names, built_in) = &resolved.stamp;
            *built_in == mask
                && names.len() == granted().count()
                && granted().zip(names).all(|(e, (name, was))| {
                    e.name == *name
                        && match (self.link(&e.name), was) {
                            (Some(link), Some((then, epoch))) => {
                                Arc::ptr_eq(&link, then) && link.epoch() == *epoch
                            }
                            (None, None) => true,
                            _ => false,
                        }
                })
        });
        if !current {
            *cache = Some(Arc::new(self.resolve(config, mask)));
        }
        cache.as_ref()?.words.get(word).cloned()
    }

    /// Lets go of the words that were resolved, and of the links they hold: a link that was stopped is not
    /// kept alive by the cache. The next command that is an extension's resolves them again.
    pub(crate) fn forget_words(&self) {
        *self.resolved.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// Reads the declarations of the extensions in the order of the configuration, and logs what it refuses.
    fn resolve(&self, config: &Config, built_in: u8) -> Resolved {
        let mut words: HashMap<String, Claim> = HashMap::new();
        let mut names = Vec::new();
        for extension in config.enabled_extensions().filter(|e| e.grants(COMMANDS)) {
            let link = self.link(&extension.name);
            names.push((
                extension.name.clone(),
                link.as_ref().map(|link| (Arc::clone(link), link.epoch())),
            ));
            let Some(link) = link else {
                continue;
            };
            for spec in link.declared() {
                let refuse = |what: &str, why: &str| {
                    warning!(
                        "extension \"{}\": the command word \"{}\" {why}",
                        link.name(),
                        text::clean(what, meridian_extension::wire::MAX_COMMAND_WORD)
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
                    } else if words.contains_key(w) {
                        Some("is another command's, already")
                    } else {
                        None
                    }
                };
                if let Some(why) = clash(&spec.name) {
                    refuse(&spec.name, &format!("is refused: it {why}"));
                    continue;
                }
                let mut mine = vec![spec.name.clone()];
                for alias in &spec.aliases {
                    match clash(alias) {
                        Some(why) => refuse(alias, &format!("is refused: it {why}")),
                        None => mine.push(alias.clone()),
                    }
                }
                for word in mine {
                    words.insert(
                        word,
                        Claim {
                            link: Arc::clone(&link),
                            extension: extension.name.clone(),
                            command: spec.name.clone(),
                        },
                    );
                }
            }
        }
        Resolved {
            stamp: (names, built_in),
            words,
        }
    }
}

/// Which of the proxy's commands are enabled, one bit each.
fn built_in_mask(config: &Config) -> u8 {
    BUILT_IN
        .iter()
        .enumerate()
        .filter(|(_, b)| config.commands.is_enabled(**b))
        .fold(0, |mask, (i, _)| mask | 1 << i)
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
        // Before the permission, which may ask the extension: a player who types the command again while it
        // runs costs it nothing.
        if self.running.load(Ordering::SeqCst) {
            return Some(BUSY.to_owned());
        }
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
        // The extension's places are shared with its decisions: half of them at most are for commands.
        let Some(place) = claim.link.run_place() else {
            claim
                .link
                .stats()
                .request(run::RUN, super::stats::Outcome::Busy, None);
            return Some(NO_ANSWER.to_owned());
        };
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
                let (_running, _place) = (running, place);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The flag that says a command is running is freed when the guard goes, whatever ended the thread
    /// that held it (a panic, or a thread that could not start and whose closure was dropped).
    #[test]
    fn the_flag_of_a_running_command_is_freed_when_its_guard_goes() {
        let flag = Arc::new(AtomicBool::new(true));
        drop(Running(Arc::clone(&flag)));
        assert!(!flag.load(Ordering::SeqCst));
        // A closure that is never run frees it too, as a thread that did not start does.
        flag.store(true, Ordering::SeqCst);
        let guard = Running(Arc::clone(&flag));
        let never_run = move || {
            let _guard = guard;
        };
        drop(never_run);
        assert!(!flag.load(Ordering::SeqCst));
        // And a panic in the thread.
        flag.store(true, Ordering::SeqCst);
        let guard = Running(Arc::clone(&flag));
        let joined = std::thread::spawn(move || {
            let _guard = guard;
            panic!("the command's thread failed");
        })
        .join();
        assert!(joined.is_err());
        assert!(!flag.load(Ordering::SeqCst));
    }
}
