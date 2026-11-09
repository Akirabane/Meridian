//! The choice of server (docs/EXTENSIONS.md, 4.4): when the proxy places a player, at login and when their
//! server is lost, the extension that holds `decide.server` is shown the servers the routes allow and may
//! put them in an order of its own, or refuse the player. It never names a server the proxy did not offer:
//! an answer that does is invalid. `default`, and every case where there is no usable answer and the
//! operator chose `default`, leave the routes, the health order and the balancer as they are.
//!
//! This runs on the thread that is handling the player, once for each placing, and waits at most the
//! `timeout-ms` of the configuration (at the fallback, at most [`FALLBACK_TIMEOUT`]: the player is waiting
//! in the configuration phase, and a failure there is never a refusal). With no extension that holds the point, which is the default, nothing is
//! asked and nothing is looked up.

use super::login::DEFAULT_REASON;
use super::{Link, Registry};
use crate::log::debug;
use crate::session::Shared;
use meridian_config::{Config, DECIDE_SERVER, ServerOnError};
use meridian_extension::decide::{
    self, Auth, Candidate, Choice, Placing, Player, ServerParams, ServerRequest,
};
use std::time::Duration;

/// The label of this point in the metrics.
const POINT: &str = "server";

/// The longest the placing of a player who lost their server waits for the extension.
pub(crate) const FALLBACK_TIMEOUT: Duration = Duration::from_millis(500);

/// Who is being placed, and why.
pub(crate) struct Who<'a> {
    pub name: &'a str,
    /// The unique id as `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`.
    pub uuid: String,
    /// Whether the session service confirmed the account.
    pub verified: bool,
    pub host: Option<&'a str>,
    /// The server the player lost, at the fallback.
    pub lost: Option<&'a str>,
}

/// What the placing does next.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// As it would without an extension.
    Default,
    /// Try these servers in this order, and only these. Each is one of the candidates.
    Order(Vec<String>),
    /// Refused by the extension `by`, with `message` for the player.
    Refuse { by: String, message: String },
}

/// Asks the extension that holds `decide.server`, if there is one, where `who` goes among `candidates`.
pub(crate) fn choose(
    registry: &Registry,
    config: &Config,
    shared: &Shared,
    who: &Who<'_>,
    candidates: &[String],
) -> Outcome {
    let Some(extension) = config
        .enabled_extensions()
        .find(|e| e.grants(DECIDE_SERVER))
    else {
        return Outcome::Default;
    };
    // A player who lost their server is already in the game: an extension that is silent, slow or wrong
    // must not cost them their connection, so there the on-error policy is `default` whatever the
    // operator wrote. Only an explicit `refuse` of the extension refuses them.
    let policy = if who.lost.is_some() {
        ServerOnError::Default
    } else {
        extension.decide_server.on_error
    };
    let mut timeout = Duration::from_millis(extension.decide_server.timeout_ms);
    if who.lost.is_some() {
        timeout = timeout.min(FALLBACK_TIMEOUT);
    }
    // A reload may have just added the extension and not started it yet: no usable answer.
    let Some(link) = registry.link(&extension.name) else {
        debug!(
            "extension \"{}\" is not running; the placing takes the on-error policy",
            extension.name
        );
        return by_policy(None, policy, &extension.name);
    };
    // What the extension did not declare is not used (docs/EXTENSIONS.md, 2.3).
    if let Some(info) = link.info()
        && !info.granted.iter().any(|c| c == DECIDE_SERVER)
    {
        return Outcome::Default;
    }
    let params = ServerParams {
        request: ServerRequest {
            player: Player {
                name: who.name.to_owned(),
                uuid: who.uuid.clone(),
                auth: if who.verified {
                    Auth::Verified
                } else {
                    Auth::Unverified
                },
            },
            reason: if who.lost.is_some() {
                Placing::Fallback
            } else {
                Placing::Login
            },
            host: who.host.map(str::to_owned),
            lost: who.lost.map(str::to_owned),
            candidates: describe(config, shared, candidates),
        },
    };
    let params = serde_json::to_value(&params).unwrap_or(serde_json::Value::Null);
    match link.call(decide::SERVER, Some(&params), timeout, |result| {
        decide::parse_server_answer(result, candidates)
    }) {
        Ok(choice) => answered(&link, choice, &extension.name),
        Err(why) => {
            debug!(
                "extension \"{}\" gave no usable answer for a placing: {why}",
                extension.name
            );
            by_policy(Some(&link), policy, &extension.name)
        }
    }
}

/// The candidates as the extension is told of them: what the balancer knows of each (its role, its
/// players against its caps, whether it is offered), and the name only for one it knows nothing of.
fn describe(config: &Config, shared: &Shared, candidates: &[String]) -> Vec<Candidate> {
    let counts = shared.counts();
    let now = std::time::Instant::now();
    candidates
        .iter()
        .map(|name| {
            let on = counts.get(name).copied().unwrap_or(0);
            let restarting = shared.restarting(name);
            let standing =
                crate::placing::standing(config, shared, name, on, restarting.as_deref());
            let number = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
            Candidate {
                name: name.clone(),
                role: standing.as_ref().map(|s| s.role.name().to_owned()),
                players: standing.as_ref().map(|s| number(s.players)),
                soft_cap: standing.as_ref().map(|s| s.soft_cap),
                hard_cap: standing.as_ref().map(|s| s.hard_cap),
                state: standing.as_ref().map(|s| s.offer.state().to_owned()),
                why: standing.as_ref().and_then(|s| s.offer.why()),
                up: shared.statuses.answering(name),
                report: crate::status::report_of(&shared.statuses, name, now),
            }
        })
        .collect()
}

/// An answer the extension gave.
fn answered(link: &Link, choice: Choice, name: &str) -> Outcome {
    match choice {
        Choice::Default => {
            link.stats().decided(POINT, "default");
            Outcome::Default
        }
        Choice::Order(servers) => {
            link.stats().decided(POINT, "order");
            debug!(
                "extension \"{name}\" ordered {} server(s) for a placing",
                servers.len()
            );
            Outcome::Order(servers)
        }
        Choice::Refuse(reason) => {
            link.stats().decided(POINT, "refuse");
            Outcome::Refuse {
                by: name.to_owned(),
                message: reason
                    .map_or_else(|| DEFAULT_REASON.to_owned(), |r| r.as_str().to_owned()),
            }
        }
    }
}

/// No usable answer (a timeout, an invalid answer, no connection, an open breaker, no free place):
/// what the operator chose for it.
fn by_policy(link: Option<&Link>, policy: ServerOnError, name: &str) -> Outcome {
    let label = match policy {
        ServerOnError::Default => "policy_default",
        ServerOnError::Refuse => "policy_refuse",
    };
    if let Some(link) = link {
        link.stats().decided(POINT, label);
    }
    match policy {
        ServerOnError::Default => Outcome::Default,
        ServerOnError::Refuse => Outcome::Refuse {
            by: name.to_owned(),
            message: DEFAULT_REASON.to_owned(),
        },
    }
}
