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
//! The orders that read ([`PLAYERS_LIST`], [`PLAYERS_FIND`], [`SERVERS_LIST`], [`QUEUE_LIST`]; capability `orders.read`) are
//! here too: what they ask, and the types of what they answer. A page of players is told in the order of their unique ids,
//! and its cursor is an id, not a place.
//!
//! Every order costs the extension's budget (one, for now). The proxy answers an order over the budget with
//! [`wire::code::OVER_BUDGET`] and one that finds too many running with [`wire::code::TOO_MANY_RUNNING`].
//! Neither ran.

use crate::decide::Auth;
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
    if !params.is_object() {
        return Err("params must be an object with \"player\" (a string) or \"all\" (true)".into());
    }
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

// ---------------------------------------------------------------------------------------------------------------
// The orders that read (capability `orders.read`): the players, one player, the servers and the queue

/// The method that lists the players, a page at a time.
pub const PLAYERS_LIST: &str = "meridian:players/list";
/// The method that finds one player, by name or by unique id.
pub const PLAYERS_FIND: &str = "meridian:players/find";
/// The method that lists the servers.
pub const SERVERS_LIST: &str = "meridian:servers/list";
/// The method that lists the players who wait in the queue.
pub const QUEUE_LIST: &str = "meridian:queue/list";

/// How many players a page holds when the order does not say.
pub const DEFAULT_PAGE: usize = 100;
/// The most players a page holds, and the most people the queue lists: 500 of either are about 100 KiB, well under
/// the 256 KiB of a message.
pub const MAX_PAGE: usize = 500;

/// `params` of [`PLAYERS_LIST`], all of them optional: `limit` is how many players the page holds (1 to
/// [`MAX_PAGE`], [`DEFAULT_PAGE`] when left out) and `cursor` is the `next` of the page before, to go on after it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// A page asked for: how many, and after whom.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page {
    pub limit: usize,
    /// The unique id the page goes on after, in its canonical spelling.
    pub after: Option<String>,
}

/// Reads the `params` of [`PLAYERS_LIST`]. None, `null` and `{}` are the first page of [`DEFAULT_PAGE`]. The error says
/// what is wrong in a few words and never quotes what was sent.
pub fn parse_list(params: Option<&Value>) -> Result<Page, String> {
    let params: ListParams = match params {
        None | Some(Value::Null) => ListParams::default(),
        Some(value) if !value.is_object() => {
            return Err(
                "params must be an object with \"limit\" (a number) and \"cursor\" (a string)"
                    .into(),
            );
        }
        Some(value) => serde_json::from_value(value.clone()).map_err(
            |_| "params must be an object with \"limit\" (a number) and \"cursor\" (a string)",
        )?,
    };
    let limit = match params.limit {
        None => DEFAULT_PAGE,
        Some(n) if (1..=MAX_PAGE as u32).contains(&n) => n as usize,
        Some(_) => return Err(format!("\"limit\" must be from 1 to {MAX_PAGE}")),
    };
    let after = match params.cursor {
        None => None,
        Some(cursor) => Some(
            canonical_uuid(&cursor)
                .ok_or_else(|| "\"cursor\" is not one that a page gave".to_owned())?,
        ),
    };
    Ok(Page { limit, after })
}

/// One player, as `players/list` and `players/find` tell them. The `address` is there only for an extension that was
/// granted `addresses`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerInfo {
    pub name: String,
    /// The unique id, as `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx` in lower case.
    pub uuid: String,
    pub auth: Auth,
    /// The server the player is on.
    pub server: String,
    /// How long they have been connected.
    pub seconds: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
}

/// `result` of [`PLAYERS_LIST`]: a page of players in the order of their unique ids, how many players there are in all,
/// and, when there are more after this page, the `cursor` that asks for the next.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayersPage {
    pub players: Vec<PlayerInfo>,
    pub total: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}

/// The page `page` asks for among `players`, in any order. The players are told in the order of their unique ids, and
/// the cursor is the unique id of the last of a page: players who join or leave between two pages do not make one
/// that stays skipped or told twice.
pub fn page_of(mut players: Vec<PlayerInfo>, page: &Page) -> PlayersPage {
    players.sort_by(|a, b| a.uuid.cmp(&b.uuid));
    let total = players.len();
    let mut rest: Vec<PlayerInfo> = match &page.after {
        Some(after) => players
            .into_iter()
            .filter(|p| p.uuid.as_str() > after.as_str())
            .collect(),
        None => players,
    };
    let more = rest.len() > page.limit;
    rest.truncate(page.limit);
    let next = if more {
        rest.last().map(|p| p.uuid.clone())
    } else {
        None
    };
    PlayersPage {
        players: rest,
        total,
        next,
    }
}

/// `params` of [`PLAYERS_FIND`]: a player's name, in any case, or a unique id, with or without its hyphens.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindParams {
    pub player: String,
}

/// Who [`PLAYERS_FIND`] looks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Who {
    /// A unique id, in its canonical spelling.
    Id(String),
    /// A name (the proxy compares it whatever its case).
    Name(String),
}

/// The longest name [`parse_find`] takes: the game's own limit is 16, and a longer text is not a name.
pub const MAX_NAME: usize = 32;

/// Reads the `params` of [`PLAYERS_FIND`]. A text that is a unique id is one; any other is a name if it is 1 to
/// [`MAX_NAME`] characters with no control character. The error never quotes what was sent.
pub fn parse_find(params: Option<&Value>) -> Result<Who, String> {
    let params = params.ok_or("params are missing")?;
    if !params.is_object() {
        return Err("params must be an object with \"player\" (a string)".into());
    }
    let params: FindParams = serde_json::from_value(params.clone())
        .map_err(|_| "params must be an object with \"player\" (a string)")?;
    if let Some(id) = canonical_uuid(&params.player) {
        return Ok(Who::Id(id));
    }
    let chars = params.player.chars().count();
    if chars == 0 || chars > MAX_NAME || params.player.chars().any(char::is_control) {
        return Err("\"player\" is neither a unique id nor a name".into());
    }
    Ok(Who::Name(params.player))
}

/// `result` of [`PLAYERS_FIND`]: the player, or `null` when nobody online is called that.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Found {
    pub player: Option<PlayerInfo>,
}

/// One server, as [`SERVERS_LIST`] tells it. A server with no role has only its name, whether it is the default and
/// its players. **No address of a server is told.**
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    /// Whether it is the server a player who has no other is sent to.
    pub default: bool,
    /// The players on it.
    pub players: usize,
    /// `lobby`, `sublobby` or `game`, for a server that the balancer places players on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub soft_cap: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hard_cap: Option<u32>,
    /// `open`, `closed` (its soft cap is reached), `draining` or `not_offered` (no recent report from it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// How long it took to answer the last time the proxy asked its sidecar, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ping_ms: Option<u64>,
}

/// `result` of [`SERVERS_LIST`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServersList {
    pub servers: Vec<ServerInfo>,
}

/// Someone who waits in the queue for a place on the proxy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Waiting {
    /// 1 for the one who has waited longest, who is let in first.
    pub position: usize,
    pub name: String,
    /// How long they have waited.
    pub seconds: u64,
}

/// `result` of [`QUEUE_LIST`]: whether the queue is on, how many wait in all, and the first [`MAX_PAGE`] of them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueState {
    pub enabled: bool,
    pub total: usize,
    pub waiting: Vec<Waiting>,
}

/// How `rpc.discover` of the proxy describes the orders that read.
pub fn read_methods() -> Vec<(&'static str, DiscoverMethod)> {
    vec![
        (
            PLAYERS_LIST,
            DiscoverMethod::with_params(
                PLAYERS_LIST,
                &[
                    (
                        "limit",
                        false,
                        json!({ "type": "integer", "minimum": 1, "maximum": MAX_PAGE }),
                    ),
                    ("cursor", false, json!({ "type": "string" })),
                ],
            ),
        ),
        (
            PLAYERS_FIND,
            DiscoverMethod::with_params(
                PLAYERS_FIND,
                &[("player", true, json!({ "type": "string" }))],
            ),
        ),
        (SERVERS_LIST, DiscoverMethod::new(SERVERS_LIST)),
        (QUEUE_LIST, DiscoverMethod::new(QUEUE_LIST)),
    ]
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
            json!([ALEX]),
            json!([null, true]),
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

    // ---- the orders that read ------------------------------------------------------------------------

    fn player(name: &str, uuid: &str) -> PlayerInfo {
        PlayerInfo {
            name: name.into(),
            uuid: uuid.into(),
            auth: Auth::Verified,
            server: "lobby".into(),
            seconds: 1,
            address: None,
        }
    }

    /// Seven players whose ids sort in the order of the digit that starts them.
    fn seven() -> Vec<PlayerInfo> {
        (1..=7)
            .rev()
            .map(|n| {
                player(
                    &format!("P{n}"),
                    &format!("{n}0000000-0000-0000-0000-000000000000"),
                )
            })
            .collect()
    }

    #[test]
    fn a_page_asks_for_how_many_and_after_whom_and_no_parameter_is_the_first_page_of_a_hundred() {
        let first = Page {
            limit: DEFAULT_PAGE,
            after: None,
        };
        for none in [None, Some(&Value::Null), Some(&json!({}))] {
            assert_eq!(parse_list(none), Ok(first.clone()));
        }
        assert_eq!(parse_list(Some(&json!({"limit": 500}))).unwrap().limit, 500);
        assert_eq!(parse_list(Some(&json!({"limit": 1}))).unwrap().limit, 1);
        // The cursor is read in its one spelling.
        let page = parse_list(Some(&json!({"cursor": ALEX.to_ascii_uppercase()}))).unwrap();
        assert_eq!(page.after.as_deref(), Some(ALEX));
        let secret = "ZZZ-not-a-cursor-ZZZ";
        for bad in [
            json!({"limit": 0}),
            json!({"limit": 501}),
            json!({"limit": -1}),
            json!({"limit": "ten"}),
            json!({"limit": 1.5}),
            json!({"cursor": secret}),
            json!({"cursor": 7}),
            json!({"more": 1}),
            json!([]),
            json!([3]),
            json!([3, ALEX]),
            json!("page"),
        ] {
            let why = parse_list(Some(&bad)).unwrap_err();
            assert!(!why.contains("ZZZ") && !why.is_empty(), "{bad}: {why}");
        }
    }

    #[test]
    fn the_pages_cover_the_players_once_in_the_order_of_their_ids() {
        let all = seven();
        let mut told = Vec::new();
        let mut cursor = None;
        let mut pages = 0;
        let mut ended = false;
        // Bounded: pages that never end (a cursor that goes nowhere) fail the test instead of keeping it running.
        for _ in 0..10 {
            let page = Page {
                limit: 3,
                after: cursor.clone(),
            };
            let got = page_of(all.clone(), &page);
            assert_eq!(got.total, 7);
            assert!(got.players.len() <= 3);
            told.extend(got.players.iter().map(|p| p.name.clone()));
            pages += 1;
            match got.next {
                Some(next) => {
                    // The cursor is the id of the last player of the page.
                    assert_eq!(Some(&next), got.players.last().map(|p| &p.uuid));
                    cursor = Some(next);
                }
                None => {
                    ended = true;
                    break;
                }
            }
        }
        assert!(ended, "the pages did not end: {told:?}");
        assert_eq!(told, ["P1", "P2", "P3", "P4", "P5", "P6", "P7"]);
        assert_eq!(pages, 3);
        // A page that holds the rest has no cursor, and neither has one that holds exactly the rest.
        assert_eq!(
            page_of(
                all.clone(),
                &Page {
                    limit: 7,
                    after: None
                }
            )
            .next,
            None
        );
        assert_eq!(
            page_of(
                all.clone(),
                &Page {
                    limit: 500,
                    after: None
                }
            )
            .players
            .len(),
            7
        );
        // After the last one: an empty page, still with the total.
        let end = page_of(
            all,
            &Page {
                limit: 3,
                after: Some("70000000-0000-0000-0000-000000000000".into()),
            },
        );
        assert_eq!((end.players.len(), end.total, end.next), (0, 7, None));
    }

    /// Players who join or leave between two pages make no player skipped or told twice: the cursor is an id, not a place.
    #[test]
    fn players_who_come_and_go_between_two_pages_skip_and_repeat_nobody() {
        let mut all = seven();
        let first = page_of(
            all.clone(),
            &Page {
                limit: 3,
                after: None,
            },
        );
        let names: Vec<_> = first.players.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["P1", "P2", "P3"]);
        // P1 (told already) leaves, a player whose id sorts before the cursor joins, and one that sorts after does.
        all.retain(|p| p.name != "P1");
        all.push(player("Early", "05000000-0000-0000-0000-000000000000"));
        all.push(player("Late", "45000000-0000-0000-0000-000000000000"));
        let second = page_of(
            all.clone(),
            &Page {
                limit: 3,
                after: first.next,
            },
        );
        let names: Vec<_> = second.players.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            ["P4", "Late", "P5"],
            "Early sorts before the cursor and is not in this page"
        );
    }

    #[test]
    fn a_player_is_found_by_unique_id_or_by_name_and_nothing_else_is_taken_for_either() {
        let find = |v: Value| parse_find(Some(&v));
        assert_eq!(
            find(json!({"player": ALEX.replace('-', "").to_ascii_uppercase()})),
            Ok(Who::Id(ALEX.into()))
        );
        assert_eq!(
            find(json!({"player": "Steve"})),
            Ok(Who::Name("Steve".into()))
        );
        assert_eq!(
            find(json!({"player": "n".repeat(MAX_NAME)})),
            Ok(Who::Name("n".repeat(MAX_NAME)))
        );
        let secret = "ZZZ".repeat(20);
        for bad in [
            json!({}),
            json!({"player": ""}),
            json!({"player": "n".repeat(MAX_NAME + 1)}),
            json!({"player": format!("{secret}\n")}),
            json!({"player": "bell\u{7}"}),
            json!({"player": 7}),
            json!({"player": "Steve", "more": 1}),
            json!(["Steve"]),
            json!("Steve"),
            json!(null),
        ] {
            let why = find(bad.clone()).unwrap_err();
            assert!(!why.contains("ZZZ") && !why.is_empty(), "{bad}: {why}");
        }
        assert!(parse_find(None).is_err());
    }

    #[test]
    fn the_requests_and_answers_that_read_are_what_the_golden_files_say() {
        // The requests: read by the proxy, and written by the SDK as the same value.
        let Message::Request { params, .. } =
            Message::parse(&golden("players-list.request.json")).unwrap()
        else {
            panic!("not a request");
        };
        assert_eq!(
            parse_list(params.as_ref()),
            Ok(Page {
                limit: 2,
                after: Some(ALEX.into())
            })
        );
        assert_eq!(
            serde_json::to_value(ListParams {
                limit: Some(2),
                cursor: Some(ALEX.into())
            })
            .unwrap(),
            params.unwrap()
        );
        let Message::Request { params, .. } =
            Message::parse(&golden("players-find.request.json")).unwrap()
        else {
            panic!("not a request");
        };
        assert_eq!(parse_find(params.as_ref()), Ok(Who::Name("Bob".into())));
        // The answers: they read into the types, and the types write the same value.
        fn result(name: &str) -> Value {
            match Message::parse(&golden(name)).unwrap() {
                Message::Response {
                    outcome: Ok(result),
                    ..
                } => result,
                other => panic!("{other:?}"),
            }
        }
        let page: PlayersPage = serde_json::from_value(result("players-list.answer.json")).unwrap();
        assert_eq!((page.players.len(), page.total), (2, 5));
        assert_eq!(page.players[1].address.as_deref(), Some("203.0.113.7"));
        assert_eq!(page.players[0].address, None);
        assert_eq!(
            serde_json::to_value(&page).unwrap(),
            result("players-list.answer.json")
        );
        let found: Found = serde_json::from_value(result("players-find.answer.json")).unwrap();
        assert_eq!(found.player.as_ref().map(|p| p.name.as_str()), Some("Bob"));
        assert_eq!(
            serde_json::to_value(&found).unwrap(),
            result("players-find.answer.json")
        );
        let none: Found = serde_json::from_value(result("players-find.answer.none.json")).unwrap();
        assert_eq!(none.player, None);
        assert_eq!(
            serde_json::to_value(&none).unwrap(),
            result("players-find.answer.none.json")
        );
        let servers: ServersList =
            serde_json::from_value(result("servers-list.answer.json")).unwrap();
        assert_eq!(servers.servers.len(), 3);
        assert_eq!(servers.servers[2].role, None);
        assert_eq!(
            serde_json::to_value(&servers).unwrap(),
            result("servers-list.answer.json")
        );
        let queue: QueueState = serde_json::from_value(result("queue-list.answer.json")).unwrap();
        assert_eq!(
            (queue.enabled, queue.total, queue.waiting.len()),
            (true, 2, 2)
        );
        assert_eq!(
            serde_json::to_value(&queue).unwrap(),
            result("queue-list.answer.json")
        );
    }

    /// 500 players of the longest the game allows, in the most the page may hold, are well inside a message.
    #[test]
    fn the_biggest_page_fits_in_a_message_with_room_to_spare() {
        let players: Vec<_> = (0..MAX_PAGE)
            .map(|n| PlayerInfo {
                name: "N".repeat(16),
                uuid: format!("{n:08x}-0000-0000-0000-000000000000"),
                auth: Auth::Unverified,
                server: "s".repeat(32),
                seconds: u64::MAX,
                address: Some("2001:0db8:85a3:0000:0000:8a2e:0370:7334".into()),
            })
            .collect();
        let page = page_of(
            players,
            &Page {
                limit: MAX_PAGE,
                after: None,
            },
        );
        let text = wire::result(&wire::Id::Number(1), &page);
        assert!(text.len() < wire::MAX_MESSAGE / 2, "{} bytes", text.len());
    }

    #[test]
    fn the_orders_that_read_are_described_for_discover() {
        let methods = read_methods();
        let names: Vec<_> = methods.iter().map(|(_, m)| m.name.as_str()).collect();
        assert_eq!(
            names,
            [PLAYERS_LIST, PLAYERS_FIND, SERVERS_LIST, QUEUE_LIST]
        );
        assert!(methods.iter().all(|(name, m)| *name == m.name));
        assert!(methods[1].1.params[0].required);
    }
}
