//! What an extension may ask of the proxy (docs/EXTENSIONS.md, section 5), as both sides read and write it:
//! the orders, which an extension sends as JSON-RPC requests on the connection the proxy opened, once the
//! hello is done. Each needs its capability; without it the proxy answers `-32601`, as if the method did
//! not exist.
//!
//! The first order here is [`PERMISSIONS_INVALIDATE`], which makes the proxy forget the permissions it
//! kept of the extension's answers (`decide.command`, `cache_seconds`). The proxy reads its parameters with
//! [`parse_invalidate`]; the SDK writes them with [`InvalidateParams`]. A player's unique id has one
//! spelling for the two ([`canonical_uuid`]). The golden files `invalidate.*.json` and `order.error.*.json`
//! are the bytes both sides test against.
//!
//! Every order costs the extension's budget (one, for now). The proxy answers an order over the budget with
//! [`wire::code::OVER_BUDGET`] and one that finds too many running with [`wire::code::TOO_MANY_RUNNING`].
//! Neither ran.

use crate::wire::DiscoverMethod;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The method that makes the proxy forget what it kept of the permissions an extension decided
/// (capability `decide.command`).
pub const PERMISSIONS_INVALIDATE: &str = "meridian:permissions/invalidate";

/// `params` of [`PERMISSIONS_INVALIDATE`]: **exactly one** of the two keys. `player` is a unique id (with or
/// without its hyphens, in either case), `all` is `true`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvalidateParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub player: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub all: Option<bool>,
}

impl InvalidateParams {
    /// Forget what was kept for every player.
    pub fn all() -> Self {
        Self {
            player: None,
            all: Some(true),
        }
    }

    /// Forget what was kept for the player with this unique id.
    pub fn player(uuid: impl Into<String>) -> Self {
        Self {
            player: Some(uuid.into()),
            all: None,
        }
    }
}

/// What [`PERMISSIONS_INVALIDATE`] asks to forget.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Forget {
    /// What was kept for every player.
    All,
    /// What was kept for one player, by their unique id in its canonical spelling ([`canonical_uuid`]).
    Player(String),
}

/// Reads the `params` of [`PERMISSIONS_INVALIDATE`]. The error says what is wrong in a few words and never
/// quotes what was sent.
pub fn parse_invalidate(params: Option<&Value>) -> Result<Forget, String> {
    let params = params.ok_or("params are missing")?;
    let params: InvalidateParams = serde_json::from_value(params.clone())
        .map_err(|_| "params must be an object with \"player\" (a string) or \"all\" (true)")?;
    match (params.player, params.all) {
        (Some(_), Some(_)) => Err("give \"player\" or \"all\", not both".into()),
        (None, None) => Err("give \"player\" or \"all\"".into()),
        (None, Some(true)) => Ok(Forget::All),
        (None, Some(false)) => Err("\"all\" can only be true".into()),
        (Some(player), None) => canonical_uuid(&player)
            .map(Forget::Player)
            .ok_or_else(|| "\"player\" is not a unique id".into()),
    }
}

/// `result` of an order that has nothing to tell but that it was done.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ack {
    pub ok: bool,
}

impl Ack {
    pub fn done() -> Self {
        Self { ok: true }
    }
}

/// A unique id as `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`, in lower case: from the same with or without its
/// hyphens, in either case. `None` for anything else.
pub fn canonical_uuid(text: &str) -> Option<String> {
    let hex: String = match text.len() {
        32 => text.to_owned(),
        36 => {
            let bytes = text.as_bytes();
            if [8, 13, 18, 23].iter().any(|&i| bytes[i] != b'-') {
                return None;
            }
            text.replace('-', "")
        }
        _ => return None,
    };
    if hex.len() != 32 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let hex = hex.to_ascii_lowercase();
    Some(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}

/// How `rpc.discover` of the proxy describes [`PERMISSIONS_INVALIDATE`].
pub fn invalidate_method() -> DiscoverMethod {
    DiscoverMethod::with_params(
        PERMISSIONS_INVALIDATE,
        &[
            ("player", false, json!({ "type": "string" })),
            ("all", false, json!({ "type": "boolean" })),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{self, Message};

    fn golden(name: &str) -> String {
        let path = format!("{}/golden/protocol-0/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
    }

    fn value(text: &str) -> Value {
        serde_json::from_str(text).unwrap()
    }

    const ALEX: &str = "0a2ea4ba-bced-39d2-80e1-1ad51a0e925c";

    #[test]
    fn the_requests_are_what_the_golden_files_say_and_the_proxy_reads_them() {
        for (name, params, forget) in [
            (
                "invalidate.request.json",
                InvalidateParams::player(ALEX),
                Forget::Player(ALEX.into()),
            ),
            (
                "invalidate.request.all.json",
                InvalidateParams::all(),
                Forget::All,
            ),
        ] {
            let Message::Request {
                id,
                method,
                params: read,
            } = Message::parse(&golden(name)).unwrap()
            else {
                panic!("{name}");
            };
            assert_eq!(method, PERMISSIONS_INVALIDATE);
            assert_eq!(parse_invalidate(read.as_ref()), Ok(forget), "{name}");
            // What the SDK writes is the same bytes, as a value.
            let id = match id.unwrap() {
                wire::Id::Number(n) => n,
                other => panic!("{other:?}"),
            };
            let written = wire::request(id, PERMISSIONS_INVALIDATE, Some(&params));
            assert_eq!(value(&written), value(&golden(name)), "{name}");
        }
    }

    #[test]
    fn the_answer_and_the_two_errors_are_what_the_golden_files_say() {
        match Message::parse(&golden("invalidate.answer.json")).unwrap() {
            Message::Response {
                id: Some(id),
                outcome: Ok(result),
            } => {
                assert_eq!(result, serde_json::to_value(Ack::done()).unwrap());
                assert_eq!(
                    value(&wire::result(&id, &Ack::done())),
                    value(&golden("invalidate.answer.json"))
                );
            }
            other => panic!("{other:?}"),
        }
        for (name, code, message) in [
            (
                "order.error.over-budget.json",
                wire::code::OVER_BUDGET,
                "over the budget of orders",
            ),
            (
                "order.error.too-many-running.json",
                wire::code::TOO_MANY_RUNNING,
                "too many orders running",
            ),
        ] {
            match Message::parse(&golden(name)).unwrap() {
                Message::Response {
                    id: Some(id),
                    outcome: Err(error),
                } => {
                    assert_eq!(
                        (error.code, error.message.as_str()),
                        (code, message),
                        "{name}"
                    );
                    assert_eq!(
                        value(&wire::error(Some(&id), &wire::RpcError::new(code, message))),
                        value(&golden(name))
                    );
                }
                other => panic!("{other:?}"),
            }
        }
        // The two codes are the ones JSON-RPC keeps for the errors of a server, and differ.
        assert!((-32099..=-32000).contains(&wire::code::OVER_BUDGET));
        assert!((-32099..=-32000).contains(&wire::code::TOO_MANY_RUNNING));
        assert_ne!(wire::code::OVER_BUDGET, wire::code::TOO_MANY_RUNNING);
    }

    #[test]
    fn exactly_one_of_player_and_all_is_read_and_the_error_never_quotes_what_was_sent() {
        let read = |v: Value| parse_invalidate(Some(&v));
        assert_eq!(read(json!({"all": true})), Ok(Forget::All));
        assert_eq!(
            read(json!({"player": ALEX.to_ascii_uppercase()})),
            Ok(Forget::Player(ALEX.into()))
        );
        assert_eq!(
            read(json!({"player": ALEX.replace('-', "")})),
            Ok(Forget::Player(ALEX.into()))
        );
        let secret = "ZZZ-not-an-id-ZZZ";
        for bad in [
            json!({}),
            json!({"all": false}),
            json!({"all": true, "player": ALEX}),
            json!({"player": secret}),
            json!({"player": ""}),
            json!({"player": 7}),
            json!({"all": "yes"}),
            json!({"all": true, "extra": 1}),
            json!([]),
            json!("all"),
            json!(null),
        ] {
            let why = read(bad.clone()).unwrap_err();
            assert!(!why.contains("ZZZ") && !why.is_empty(), "{bad}: {why}");
        }
        assert!(parse_invalidate(None).is_err());
    }

    #[test]
    fn a_unique_id_has_one_spelling() {
        assert_eq!(canonical_uuid(ALEX).as_deref(), Some(ALEX));
        for bad in [
            "",
            "0a2ea4ba",
            "0a2ea4ba-bced-39d2-80e1-1ad51a0e925",
            "0a2ea4ba-bced-39d2-80e1-1ad51a0e925cc",
            "0a2ea4babced-39d2-80e1-1ad51a0e925c-",
            "0a2ea4ba_bced_39d2_80e1_1ad51a0e925c",
            "0a2ea4ba-bced-39d2-80e1-1ad51a0e925g",
            "0a2ea4babced39d280e11ad51a0e925g",
            // 32 or 36 bytes, but not that many characters.
            "é2ea4babced39d280e11ad51a0e925c",
        ] {
            assert_eq!(canonical_uuid(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_method_is_described_for_discover() {
        let method = invalidate_method();
        assert_eq!(method.name, PERMISSIONS_INVALIDATE);
        let names: Vec<_> = method
            .params
            .iter()
            .map(|p| (p.name.as_str(), p.required))
            .collect();
        assert_eq!(names, [("player", false), ("all", false)]);
    }
}
