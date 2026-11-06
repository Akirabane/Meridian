//! The admission at login (docs/EXTENSIONS.md, 4.2): once a player has logged in to the proxy and the
//! built-in bans and whitelist have let them through, the extension that holds `decide.login` may refuse
//! them. It can only add a refusal: `allow` and `default` both leave the player to the checks that follow
//! (the second login, the player limit and the queue, the choice of server), and the bans and the
//! whitelist were settled before it was asked, so an `allow` never lets in a player they turned away.
//!
//! This runs on the thread that is handling the player, once for each login, and waits at most the
//! `timeout-ms` of the configuration. With no extension that holds the point, which is the default,
//! nothing is asked and the cost is a look at the (empty) list of extensions.

use super::{Link, Registry};
use crate::log::debug;
use meridian_config::{Config, DECIDE_LOGIN, OnError};
use meridian_extension::decide::{self, Auth, Decision, LoginParams, LoginRequest, Player};
use std::net::SocketAddr;
use std::time::Duration;

/// What a refused player reads when the extension gave no reason, and when the on-error policy refuses.
pub(crate) const DEFAULT_REASON: &str = "You cannot join this server right now.";

/// The label of this point in the metrics.
const POINT: &str = "login";

/// Who is logging in, as the proxy knows them.
pub(crate) struct Who<'a> {
    pub peer: SocketAddr,
    pub name: &'a str,
    /// The unique id as `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`.
    pub uuid: String,
    /// Whether the session service confirmed the account.
    pub verified: bool,
    pub host: Option<&'a str>,
    /// Whether a session of the same player is open now.
    pub connected: bool,
}

/// What the login does next.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// On to the checks that follow.
    Go,
    /// Refused by the extension `by`, with `message` for the player.
    Refuse { by: String, message: String },
}

/// Asks the extension that holds `decide.login`, if there is one, whether `who` may go on.
pub(crate) fn admit(registry: &Registry, config: &Config, who: &Who<'_>) -> Outcome {
    let Some(extension) = config.enabled_extensions().find(|e| e.grants(DECIDE_LOGIN)) else {
        return Outcome::Go;
    };
    let policy = extension.decide_login.on_error;
    let timeout = Duration::from_millis(extension.decide_login.timeout_ms);
    // A reload may have just added the extension and not started it yet: no usable answer.
    let Some(link) = registry.link(&extension.name) else {
        debug!(
            "{}: extension \"{}\" is not running; the login takes the on-error policy",
            who.peer, extension.name
        );
        return by_policy(None, policy, &extension.name);
    };
    let info = link.info();
    // What the extension did not declare is not used (docs/EXTENSIONS.md, 2.3).
    if let Some(info) = &info
        && !info.granted.iter().any(|c| c == DECIDE_LOGIN)
    {
        return Outcome::Go;
    }
    // The address is told only to an extension that asked for it and was granted it.
    let address = (extension.grants("addresses")
        && info
            .as_ref()
            .is_some_and(|i| i.granted.iter().any(|c| c == "addresses")))
    .then(|| who.peer.ip().to_string());
    let params = LoginParams {
        request: LoginRequest {
            player: Player {
                name: who.name.to_owned(),
                uuid: who.uuid.clone(),
                auth: if who.verified {
                    Auth::Verified
                } else {
                    Auth::Unverified
                },
            },
            host: who.host.map(str::to_owned),
            address,
            connected: who.connected,
        },
    };
    let params = serde_json::to_value(&params).unwrap_or(serde_json::Value::Null);
    match link.call(
        decide::LOGIN,
        Some(&params),
        timeout,
        decide::parse_login_answer,
    ) {
        Ok(decision) => answered(&link, decision, who, &extension.name),
        Err(why) => {
            debug!(
                "{}: extension \"{}\" gave no usable answer for a login: {why}",
                who.peer, extension.name
            );
            by_policy(Some(&link), policy, &extension.name)
        }
    }
}

/// An answer the extension gave.
fn answered(link: &Link, decision: Decision, who: &Who<'_>, name: &str) -> Outcome {
    match decision {
        Decision::Allow => {
            link.stats().decided(POINT, "allow");
            debug!("{}: extension \"{name}\" allowed a login", who.peer);
            Outcome::Go
        }
        Decision::Default => {
            link.stats().decided(POINT, "default");
            Outcome::Go
        }
        Decision::Deny(reason) => {
            link.stats().decided(POINT, "deny");
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
fn by_policy(link: Option<&Link>, policy: OnError, name: &str) -> Outcome {
    let label = match policy {
        OnError::Default => "policy_default",
        OnError::Allow => "policy_allow",
        OnError::Deny => "policy_deny",
    };
    if let Some(link) = link {
        link.stats().decided(POINT, label);
    }
    match policy {
        OnError::Default | OnError::Allow => Outcome::Go,
        OnError::Deny => Outcome::Refuse {
            by: name.to_owned(),
            message: DEFAULT_REASON.to_owned(),
        },
    }
}
