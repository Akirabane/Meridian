//! The permission of a command (docs/EXTENSIONS.md, 4.3): when a player types a command the proxy answers
//! itself, the extension that holds `decide.command` says, for each node of permission the command
//! checks, whether the player has it. **The extension is the source of rights for these commands**
//! (decision U2): `allow` gives a node even if `permissions.toml` and the operators of the server
//! refuse it, `deny` takes it even if they give it, and `default` leaves it to them, as without an
//! extension. [`merge`] is that rule, in one place.
//!
//! This is on the way of the player's packets: the call is made on the thread that copies them to the
//! server, so it waits at most the `timeout-ms` of the configuration (250 ms by default), and it is made
//! only for a command the proxy answers (any other command goes to the server untouched), once for all
//! the nodes it checks, and not at all for the nodes whose answer the proxy kept (`cache_seconds`, at
//! most `cache-seconds-max`). What is kept belongs to the session of the player, so it goes when they
//! leave, and it is dropped at a reload and when the extension says hello again.
//!
//! With no extension that holds the point, which is the default, nothing is asked and nothing is kept.

use super::{Link, Registry};
use crate::log::debug;
use meridian_config::{Config, DECIDE_COMMAND, OnError};
use meridian_extension::decide::{
    self, Auth, CommandParams, CommandRequest, MAX_ARG_CHARS, MAX_COMMAND_ARGS, NodeVerdict, Player,
};
use std::collections::HashMap;
use std::hash::{BuildHasher, Hash, Hasher, RandomState};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The label of this point in the metrics.
const POINT: &str = "command";

/// The most nodes whose answer a player's session keeps. A player cannot make nodes up (only the
/// servers there are have one), so this is a bound that is never reached, not a limit that is met.
const MAX_KEPT: usize = 512;

/// Whether the player has a node: the file's answer (`permissions.toml` and the operators of the server)
/// `file`, and what the extension said of the node. `Allow` and `Deny` win over the file, and only
/// `Default` leaves the question to it.
pub(crate) fn merge(file: bool, verdict: NodeVerdict) -> bool {
    match verdict {
        NodeVerdict::Allow => true,
        NodeVerdict::Deny => false,
        NodeVerdict::Default => file,
    }
}

/// What a node gets when there is no usable answer (a timeout, an invalid answer, no connection, an open
/// breaker, no free place): `deny` refuses it, and anything else leaves it to the file. **Never a
/// permission**: `on-error = "allow"` is refused when the configuration is read (it would give every
/// command of the proxy to whoever types it while the extension is down), and a policy of that kind that got
/// here some other way is read as `default`, so that no failure of an extension can grant anything.
pub(crate) fn policy_verdict(policy: OnError) -> NodeVerdict {
    match policy {
        OnError::Deny => NodeVerdict::Deny,
        OnError::Default | OnError::Allow => NodeVerdict::Default,
    }
}

/// Who typed a command, and what.
pub(crate) struct Typed<'a> {
    pub name: &'a str,
    /// The unique id as `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`.
    pub uuid: String,
    /// Whether the session service confirmed the account.
    pub verified: bool,
    /// The server the player is on.
    pub server: &'a str,
    /// The command as typed, without its slash.
    pub line: &'a str,
}

/// What a session keeps of the extension's answers: a verdict for a node, until a time, for the
/// registry's generation and the extension's epoch it was given under.
///
/// The extension decides with the request it was sent, which holds the server the player is on, the command
/// and its arguments as well as the node, so a verdict is kept for **that** request and no other: the key is
/// a digest of the server, the command and the arguments, with the node. `/send Bob lobby` and `/send all
/// east` are two questions, and a verdict given on one server is not reused on another. The cache is also
/// emptied when the player's server changes.
#[derive(Default)]
struct Kept {
    stamp: (u64, u64),
    /// The server the player was on when the answers were given.
    server: String,
    nodes: HashMap<(u64, String), (NodeVerdict, Instant)>,
}

/// The extension's say in the commands of one player's session.
pub(crate) struct Gate {
    registry: Arc<Registry>,
    /// The configuration in force now: a reload changes which extension is asked, and its deadline.
    config: Box<dyn Fn() -> Arc<Config> + Send + Sync>,
    kept: Mutex<Kept>,
    /// Keyed at random for each session, so that what a player types cannot be made to collide with a
    /// digest they did not type.
    digest: RandomState,
}

impl Gate {
    pub(crate) fn new(
        registry: Arc<Registry>,
        config: impl Fn() -> Arc<Config> + Send + Sync + 'static,
    ) -> Self {
        Self {
            registry,
            config: Box::new(config),
            kept: Mutex::new(Kept::default()),
            digest: RandomState::new(),
        }
    }

    /// What the extension says of each of `nodes` for the player who typed `typed`: nothing (an empty
    /// map, every node `default`) with no extension that holds the point, or one that did not declare it.
    pub(crate) fn verdicts(
        &self,
        typed: &Typed<'_>,
        nodes: &[String],
    ) -> HashMap<String, NodeVerdict> {
        let config = (self.config)();
        let Some(extension) = config
            .enabled_extensions()
            .find(|e| e.grants(DECIDE_COMMAND))
        else {
            return HashMap::new();
        };
        let (policy, max_cache) = (
            extension.decide_command.on_error,
            extension.decide_command.cache_seconds_max,
        );
        let timeout = Duration::from_millis(extension.decide_command.timeout_ms);
        // A reload may have just added the extension and not started it yet: no usable answer.
        let Some(link) = self.registry.link(&extension.name) else {
            debug!(
                "extension \"{}\" is not running; a command takes the on-error policy",
                extension.name
            );
            return all(nodes, policy_verdict(policy));
        };
        // What the extension did not declare is not used (docs/EXTENSIONS.md, 2.3).
        if let Some(info) = link.info()
            && !info.granted.iter().any(|c| c == DECIDE_COMMAND)
        {
            return HashMap::new();
        }

        // What is kept, and what is left to ask.
        let stamp = (self.registry.generation(), link.epoch());
        let asked_of = self.digest_of(typed);
        let now = Instant::now();
        let mut known = HashMap::new();
        let mut unknown = Vec::new();
        {
            let mut kept = self.kept.lock().unwrap_or_else(|e| e.into_inner());
            // A reload, a new hello or a change of server: what was said before is not said of this.
            if kept.stamp != stamp || kept.server != typed.server {
                kept.nodes.clear();
                kept.stamp = stamp;
                kept.server = typed.server.to_owned();
            }
            for node in nodes {
                match kept.nodes.get(&(asked_of, node.clone())) {
                    Some((verdict, until)) if *until > now => {
                        known.insert(node.clone(), *verdict);
                    }
                    _ => unknown.push(node.clone()),
                }
            }
        }
        if unknown.is_empty() {
            link.stats().decided(POINT, "cached");
            return known;
        }

        let params = CommandParams {
            request: request(typed, &unknown),
        };
        let params = serde_json::to_value(&params).unwrap_or(serde_json::Value::Null);
        let asked = unknown.clone();
        match link.call(decide::COMMAND, Some(&params), timeout, |result| {
            decide::parse_command_answer(result, &asked)
        }) {
            Ok(answers) => {
                let mut label = "default";
                for node in &unknown {
                    match answers.verdict(node) {
                        NodeVerdict::Deny => label = "deny",
                        NodeVerdict::Allow if label != "deny" => label = "allow",
                        _ => {}
                    }
                    known.insert(node.clone(), answers.verdict(node));
                }
                link.stats().decided(POINT, label);
                self.keep(
                    &link,
                    stamp,
                    typed.server,
                    asked_of,
                    &unknown,
                    &answers,
                    max_cache,
                );
                known
            }
            Err(why) => {
                debug!(
                    "extension \"{}\" gave no usable answer for a command: {why}",
                    extension.name
                );
                link.stats().decided(
                    POINT,
                    match policy_verdict(policy) {
                        NodeVerdict::Deny => "policy_deny",
                        _ => "policy_default",
                    },
                );
                // A failure is never kept: the next command asks again (the breaker spares the wait).
                known.extend(all(&unknown, policy_verdict(policy)));
                known
            }
        }
    }

    /// Keeps `answers` for `nodes`, as long as the extension said and the configuration allows, if
    /// nothing changed meanwhile (a reload, a new hello).
    #[allow(clippy::too_many_arguments)]
    fn keep(
        &self,
        link: &Link,
        stamp: (u64, u64),
        server: &str,
        asked_of: u64,
        nodes: &[String],
        answers: &decide::NodeAnswers,
        max_cache: u64,
    ) {
        let seconds = answers.seconds().min(max_cache);
        if seconds == 0 {
            return;
        }
        let now = Instant::now();
        let until = now + Duration::from_secs(seconds);
        let mut kept = self.kept.lock().unwrap_or_else(|e| e.into_inner());
        if kept.stamp != stamp
            || kept.server != server
            || (self.registry.generation(), link.epoch()) != stamp
        {
            return;
        }
        // The answers that ran out are not worth a place: they go before a new one is refused.
        if kept.nodes.len() + nodes.len() > MAX_KEPT {
            kept.nodes.retain(|_, (_, until)| *until > now);
            if kept.nodes.len() + nodes.len() > MAX_KEPT {
                return;
            }
        }
        for node in nodes {
            kept.nodes
                .insert((asked_of, node.clone()), (answers.verdict(node), until));
        }
    }

    /// A digest of what the extension decides on besides the node: the server the player is on, the command
    /// and its arguments, as it is told them.
    fn digest_of(&self, typed: &Typed<'_>) -> u64 {
        let asked = request(typed, &[]);
        let mut hasher = self.digest.build_hasher();
        asked.server.hash(&mut hasher);
        asked.command.hash(&mut hasher);
        asked.args.hash(&mut hasher);
        hasher.finish()
    }

    /// How many answers are kept (to look at, and for tests).
    #[cfg(test)]
    pub(crate) fn kept_len(&self) -> usize {
        self.kept
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .nodes
            .len()
    }
}

fn all(nodes: &[String], verdict: NodeVerdict) -> HashMap<String, NodeVerdict> {
    nodes.iter().map(|n| (n.clone(), verdict)).collect()
}

/// The request for `typed`, asking about `nodes`. The command and its arguments are what the player
/// typed: at most [`MAX_COMMAND_ARGS`] arguments, each cut to [`MAX_ARG_CHARS`] characters.
fn request(typed: &Typed<'_>, nodes: &[String]) -> CommandRequest {
    let mut words = typed.line.split_whitespace();
    let command = words.next().unwrap_or_default();
    CommandRequest {
        player: Player {
            name: typed.name.to_owned(),
            uuid: typed.uuid.clone(),
            auth: if typed.verified {
                Auth::Verified
            } else {
                Auth::Unverified
            },
        },
        server: typed.server.to_owned(),
        command: command.chars().take(MAX_ARG_CHARS).collect(),
        args: words
            .take(MAX_COMMAND_ARGS)
            .map(|w| w.chars().take(MAX_ARG_CHARS).collect())
            .collect(),
        nodes: nodes.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rule of the specification (4.3, decision U2): `allow` and `deny` win over the file, and only
    /// `default` leaves the question to it.
    #[test]
    fn the_extensions_allow_and_deny_win_and_default_leaves_the_file() {
        for (file, verdict, has) in [
            (false, NodeVerdict::Allow, true),
            (true, NodeVerdict::Allow, true),
            (false, NodeVerdict::Deny, false),
            (true, NodeVerdict::Deny, false),
            (false, NodeVerdict::Default, false),
            (true, NodeVerdict::Default, true),
        ] {
            assert_eq!(
                merge(file, verdict),
                has,
                "file {file}, extension {verdict:?}"
            );
        }
    }

    /// No failure of the extension grants a node: `allow` cannot be configured, and if it got here it is
    /// `default`.
    #[test]
    fn what_a_node_gets_with_no_answer_is_never_a_permission() {
        assert_eq!(policy_verdict(OnError::Default), NodeVerdict::Default);
        assert_eq!(policy_verdict(OnError::Deny), NodeVerdict::Deny);
        assert_eq!(policy_verdict(OnError::Allow), NodeVerdict::Default);
    }

    #[test]
    fn the_request_tells_what_was_typed_cut_to_the_limits() {
        let typed = Typed {
            name: "Alex",
            uuid: "0a2ea4ba-bced-39d2-80e1-1ad51a0e925c".into(),
            verified: true,
            server: "lobby",
            line: "send  Bob\teast",
        };
        let asked = request(&typed, &["meridian.send".to_owned()]);
        assert_eq!(asked.command, "send");
        assert_eq!(asked.args, ["Bob", "east"]);
        assert_eq!(
            (asked.server.as_str(), asked.player.auth),
            ("lobby", Auth::Verified)
        );
        let long = format!("alert {}", "é".repeat(1000));
        let asked = request(
            &Typed {
                line: &long,
                ..typed
            },
            &[],
        );
        assert_eq!(asked.args[0].chars().count(), MAX_ARG_CHARS);
    }
}
