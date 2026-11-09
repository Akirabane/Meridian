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

use crate::decide::{Auth, ServerReport, Why};
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
            canonical_uuid(&cursor).ok_or_else(|| "\"cursor\" is not a unique id".to_owned())?,
        ),
    };
    Ok(Page { limit, after })
}

/// Reads the `params` of an order that takes none ([`SERVERS_LIST`], [`QUEUE_LIST`]): none, `null` and `{}` are what it
/// takes, and anything else is refused, in fixed words, so that a program that thinks it is filtering finds out.
pub fn parse_no_params(params: Option<&Value>) -> Result<(), String> {
    match params {
        None | Some(Value::Null) => Ok(()),
        Some(Value::Object(map)) if map.is_empty() => Ok(()),
        Some(_) => Err("this order takes no parameters".into()),
    }
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
/// that stays skipped or told twice among the players who are there at the cursor (one that joins with an id before it is not in the
/// pages that follow).
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
    who_of(params.player)
}

/// A text that is a unique id is one; any other is a name if it is 1 to [`MAX_NAME`] characters with no control
/// character. The error never quotes the text.
fn who_of(player: String) -> Result<Who, String> {
    if let Some(id) = canonical_uuid(&player) {
        return Ok(Who::Id(id));
    }
    let chars = player.chars().count();
    if chars == 0 || chars > MAX_NAME || player.chars().any(char::is_control) {
        return Err("\"player\" is neither a unique id nor a name".into());
    }
    Ok(Who::Name(player))
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
    /// Why it is `draining` or `not_offered`; absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<Why>,
    /// Whether it answered at the proxy's last round of readings; absent for a server the rounds do not read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub up: Option<bool>,
    /// What it last said of itself; absent for a server the rounds do not read, or that never answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<ServerReport>,
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

// ---------------------------------------------------------------------------------------------------------------
// The orders that message (capability `orders.message`): one player, or many

/// The method that tells one player a line in their chat.
pub const PLAYERS_TELL: &str = "meridian:players/tell";
/// The method that shows a line to every player, or to those on one server, as `/alert` does.
pub const BROADCAST: &str = "meridian:broadcast";

/// The most characters of a line an order shows: the same as a line an extension's own command answers (a line is
/// valid as [`crate::run::check_line`] says: no control, formatting or invisible character).
pub const MAX_TEXT: usize = crate::run::MAX_LINE_CHARS;

/// `params` of [`PLAYERS_TELL`]: who (a name in any case, or a unique id) and the line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TellParams {
    pub player: String,
    pub text: String,
}

/// `params` of [`BROADCAST`]: the line, and the server whose players are told (every player when it is left out).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BroadcastParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    pub text: String,
}

/// `result` of [`PLAYERS_TELL`] and [`BROADCAST`]: how many players were shown the line. A player who is not online
/// is not an error: `0`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Told {
    pub told: usize,
}

/// The line of an order that messages: not empty, and valid as a line an extension's own command answers is.
fn check_text(text: &str) -> Result<(), String> {
    if text.is_empty() {
        return Err("\"text\" is empty".into());
    }
    crate::run::check_line(text).map_err(|why| format!("\"text\": {why}"))
}

/// Reads the `params` of [`PLAYERS_TELL`]: who, and the line. The error never quotes what was sent.
pub fn parse_tell(params: Option<&Value>) -> Result<(Who, String), String> {
    const SHAPE: &str = "params must be an object with \"player\" and \"text\" (strings)";
    let params = params.ok_or("params are missing")?;
    if !params.is_object() {
        return Err(SHAPE.into());
    }
    let params: TellParams = serde_json::from_value(params.clone()).map_err(|_| SHAPE)?;
    let who = who_of(params.player)?;
    check_text(&params.text)?;
    Ok((who, params.text))
}

/// `text` made valid as the line of an order that messages ([`MAX_TEXT`] characters, none of those a line may not
/// hold): what a line may not hold is dropped (a line break ends nothing: send another line) and the line is cut.
/// What the extension means is kept; what the proxy would refuse is not. The SDK's `tell` and `broadcast` refuse a
/// line that is not valid, and do not change it: this is for a program that would rather have it cleaned.
pub fn clean_text(text: &str) -> String {
    text.chars()
        .filter(|c| !crate::run::is_refused(*c))
        .take(MAX_TEXT)
        .collect()
}

// ---------------------------------------------------------------------------------------------------------------
// The order that moves (capability `orders.move`)

/// The method that moves players to a server: one player, every player, or those of a server.
pub const PLAYERS_SEND: &str = "meridian:players/send";

/// `params` of [`PLAYERS_SEND`]: the server to send them to (`to`), and **exactly one** of who: `player` (a name in any
/// case or a unique id), `all` (`true`: every player) or `from` (a server: those on it).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendParams {
    pub to: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub player: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub all: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

/// Whom [`PLAYERS_SEND`] moves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// One player.
    Player(Who),
    /// Every player online.
    All,
    /// The players on this server.
    Server(String),
}

/// `result` of [`PLAYERS_SEND`]: how many players the proxy **started** to move, which are those who were not on the server
/// already. The moves go on after the answer, one after the other; how each ended is in the proxy's log (and, in
/// version 1.1, an event). `0` is not an error: nobody was there to move.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Started {
    pub started: usize,
}

/// A name of a server, as [`parse_send`] and [`parse_broadcast`] take it: 1 to [`MAX_SERVER`] characters, no control
/// character. Whether the proxy has it is for the proxy to say.
fn server_name(name: &str) -> bool {
    let chars = name.chars().count();
    chars > 0 && chars <= MAX_SERVER && !name.chars().any(char::is_control)
}

/// Reads the `params` of [`PLAYERS_SEND`]: whom to move, and where to. The error never quotes what was sent.
pub fn parse_send(params: Option<&Value>) -> Result<(Target, String), String> {
    const SHAPE: &str =
        "params must be an object with \"to\" and one of \"player\", \"all\" (true) and \"from\"";
    let params = params.ok_or("params are missing")?;
    if !params.is_object() {
        return Err(SHAPE.into());
    }
    let params: SendParams = serde_json::from_value(params.clone()).map_err(|_| SHAPE)?;
    if !server_name(&params.to) {
        return Err("\"to\" is not a name of a server".into());
    }
    let target = match (params.player, params.all, params.from) {
        (Some(player), None, None) => Target::Player(who_of(player)?),
        (None, Some(true), None) => Target::All,
        (None, Some(false), None) => return Err("\"all\" can only be true".into()),
        (None, None, Some(from)) => {
            if !server_name(&from) {
                return Err("\"from\" is not a name of a server".into());
            }
            Target::Server(from)
        }
        (None, None, None) => return Err("give one of \"player\", \"all\" and \"from\"".into()),
        _ => return Err("give only one of \"player\", \"all\" and \"from\"".into()),
    };
    Ok((target, params.to))
}

/// How `rpc.discover` of the proxy describes the order that moves.
pub fn move_methods() -> Vec<(&'static str, DiscoverMethod)> {
    vec![(
        PLAYERS_SEND,
        DiscoverMethod::with_params(
            PLAYERS_SEND,
            &[
                ("to", true, json!({ "type": "string" })),
                ("player", false, json!({ "type": "string" })),
                ("all", false, json!({ "type": "boolean" })),
                ("from", false, json!({ "type": "string" })),
            ],
        ),
    )]
}

// ---------------------------------------------------------------------------------------------------------------
// The order that expels (capability `orders.moderate`)

/// The method that kicks a player: their session ends, and they read why.
pub const PLAYERS_KICK: &str = "meridian:players/kick";

/// What a player who is kicked is told when the order gives no reason.
pub const DEFAULT_KICK_REASON: &str = "Kicked by an extension";

/// `params` of [`PLAYERS_KICK`]: who (a name in any case, or a unique id) and, if you like, the reason the player reads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KickParams {
    pub player: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// `result` of [`PLAYERS_KICK`]: how many players were kicked, 1 or 0 (nobody online is that: not an error).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Kicked {
    pub kicked: usize,
}

/// Reads the `params` of [`PLAYERS_KICK`]: who, and the reason ([`DEFAULT_KICK_REASON`] when it is left out). The reason is
/// shown as a literal, never as a JSON text component, and is valid as the line of an extension's own command is (not empty,
/// [`MAX_TEXT`] characters at most, none of a control, formatting or invisible character). The error never quotes what was sent.
pub fn parse_kick(params: Option<&Value>) -> Result<(Who, String), String> {
    const SHAPE: &str =
        "params must be an object with \"player\" and, if you like, \"reason\" (strings)";
    let params = params.ok_or("params are missing")?;
    if !params.is_object() {
        return Err(SHAPE.into());
    }
    let params: KickParams = serde_json::from_value(params.clone()).map_err(|_| SHAPE)?;
    let who = who_of(params.player)?;
    let reason = match params.reason {
        None => DEFAULT_KICK_REASON.to_owned(),
        Some(reason) => {
            if reason.is_empty() {
                return Err("\"reason\" is empty".into());
            }
            crate::run::check_line(&reason).map_err(|why| format!("\"reason\": {why}"))?;
            reason
        }
    };
    Ok((who, reason))
}

/// How `rpc.discover` of the proxy describes the order that expels.
pub fn moderate_methods() -> Vec<(&'static str, DiscoverMethod)> {
    vec![(
        PLAYERS_KICK,
        DiscoverMethod::with_params(
            PLAYERS_KICK,
            &[
                ("player", true, json!({ "type": "string" })),
                (
                    "reason",
                    false,
                    json!({ "type": "string", "maxLength": MAX_TEXT }),
                ),
            ],
        ),
    )]
}

// ---------------------------------------------------------------------------------------------------------------
// The orders that operate (capability `orders.operate`): drain, undrain and reload

/// The method that stops offering a server to new players, and, if asked, sends its players elsewhere.
pub const SERVERS_DRAIN: &str = "meridian:servers/drain";
/// The method that offers a drained server to new players again.
pub const SERVERS_UNDRAIN: &str = "meridian:servers/undrain";
/// The method that makes the proxy read its configuration file again.
pub const RELOAD: &str = "meridian:reload";

/// `params` of [`SERVERS_DRAIN`]: the server, and `move` (`true`: its players are sent, one at a time, to other servers of its
/// role; `false` or left out: they stay).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DrainParams {
    pub server: String,
    #[serde(default, rename = "move", skip_serializing_if = "Option::is_none")]
    pub move_players: Option<bool>,
}

/// `params` of [`SERVERS_UNDRAIN`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UndrainParams {
    pub server: String,
}

/// `result` of [`SERVERS_DRAIN`]: whether the server was drained **now** (`false`: it was drained already), and how many
/// players the proxy **started** to send away (0 unless `move`, and 0 if nobody was on it). The moves go on after the answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Drained {
    pub drained: bool,
    pub moving: usize,
}

/// `result` of [`SERVERS_UNDRAIN`]: whether the server was drained and is offered again now (`false`: it was not drained, which is
/// not an error).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Undrained {
    pub undrained: bool,
}

/// Reads the `params` of [`SERVERS_DRAIN`]: the server and whether to move its players. The error never quotes what was sent.
pub fn parse_drain(params: Option<&Value>) -> Result<(String, bool), String> {
    const SHAPE: &str = "params must be an object with \"server\" (a string) and, if you like, \"move\" (true or false)";
    let params = params.ok_or("params are missing")?;
    if !params.is_object() {
        return Err(SHAPE.into());
    }
    let params: DrainParams = serde_json::from_value(params.clone()).map_err(|_| SHAPE)?;
    if !server_name(&params.server) {
        return Err("\"server\" is not a name of a server".into());
    }
    Ok((params.server, params.move_players.unwrap_or(false)))
}

/// Reads the `params` of [`SERVERS_UNDRAIN`]: the server. The error never quotes what was sent.
pub fn parse_undrain(params: Option<&Value>) -> Result<String, String> {
    const SHAPE: &str = "params must be an object with \"server\" (a string)";
    let params = params.ok_or("params are missing")?;
    if !params.is_object() {
        return Err(SHAPE.into());
    }
    let params: UndrainParams = serde_json::from_value(params.clone()).map_err(|_| SHAPE)?;
    if !server_name(&params.server) {
        return Err("\"server\" is not a name of a server".into());
    }
    Ok(params.server)
}

/// How `rpc.discover` of the proxy describes the orders that operate.
pub fn operate_methods() -> Vec<(&'static str, DiscoverMethod)> {
    vec![
        (
            SERVERS_DRAIN,
            DiscoverMethod::with_params(
                SERVERS_DRAIN,
                &[
                    ("server", true, json!({ "type": "string" })),
                    ("move", false, json!({ "type": "boolean" })),
                ],
            ),
        ),
        (
            SERVERS_UNDRAIN,
            DiscoverMethod::with_params(
                SERVERS_UNDRAIN,
                &[("server", true, json!({ "type": "string" }))],
            ),
        ),
        (RELOAD, DiscoverMethod::new(RELOAD)),
    ]
}

// ---------------------------------------------------------------------------------------------------------------
// The order that runs a console line (capability `orders.console`)

/// The method that runs one line as the operator's console does.
pub const CONSOLE_EXECUTE: &str = "meridian:console/execute";

/// The most characters of the line [`CONSOLE_EXECUTE`] takes.
pub const MAX_CONSOLE_LINE: usize = 1024;

/// The most characters of output [`CONSOLE_EXECUTE`] gives back, all lines together (an empty line counts as one). What is past it is
/// left out, and the answer says so.
pub const MAX_CONSOLE_OUTPUT: usize = 8192;

/// The last line of an output that was cut.
pub const OUTPUT_TRUNCATED: &str = "[output truncated]";

/// `params` of [`CONSOLE_EXECUTE`]: the line, as the operator would type it at the console.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecuteParams {
    pub line: String,
}

/// `result` of [`CONSOLE_EXECUTE`]: what the console said, one string for each line, as plain text (no control character, no `§`),
/// and whether it was cut at [`MAX_CONSOLE_OUTPUT`] characters (the last line is then [`OUTPUT_TRUNCATED`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Executed {
    pub lines: Vec<String>,
    pub truncated: bool,
}

/// Reads the `params` of [`CONSOLE_EXECUTE`]: one line, not empty, of [`MAX_CONSOLE_LINE`] characters at most, with no control,
/// formatting (`§`) or invisible character: a line break would make two commands of one. The error never quotes what was sent.
pub fn parse_execute(params: Option<&Value>) -> Result<String, String> {
    const SHAPE: &str = "params must be an object with \"line\" (a string)";
    let params = params.ok_or("params are missing")?;
    if !params.is_object() {
        return Err(SHAPE.into());
    }
    let params: ExecuteParams = serde_json::from_value(params.clone()).map_err(|_| SHAPE)?;
    if params.line.trim().is_empty() {
        return Err("\"line\" is empty".into());
    }
    if params.line.chars().count() > MAX_CONSOLE_LINE {
        return Err(format!(
            "\"line\" is longer than {MAX_CONSOLE_LINE} characters"
        ));
    }
    if params.line.chars().any(crate::run::is_refused) {
        return Err(
            "\"line\" holds a control, formatting or invisible character: one line, plain text"
                .into(),
        );
    }
    Ok(params.line)
}

/// What the console said, as [`CONSOLE_EXECUTE`] gives it back: each line made plain text (what a line may not hold is dropped, see
/// [`clean_text`]), at most [`MAX_CONSOLE_OUTPUT`] characters in all; past that the line that crosses the limit is cut, the others
/// are left out, and the last line is [`OUTPUT_TRUNCATED`].
pub fn console_output(lines: &[String]) -> Executed {
    let mut kept = Vec::new();
    let mut room = MAX_CONSOLE_OUTPUT;
    for line in lines {
        let line: String = line
            .chars()
            .filter(|c| !crate::run::is_refused(*c))
            .collect();
        let cost = line.chars().count().max(1);
        if cost > room {
            if room > 0 {
                kept.push(line.chars().take(room).collect());
            }
            kept.push(OUTPUT_TRUNCATED.to_owned());
            return Executed {
                lines: kept,
                truncated: true,
            };
        }
        room -= cost;
        kept.push(line);
    }
    Executed {
        lines: kept,
        truncated: false,
    }
}

/// How `rpc.discover` of the proxy describes the order that runs a console line.
pub fn console_methods() -> Vec<(&'static str, DiscoverMethod)> {
    vec![(
        CONSOLE_EXECUTE,
        DiscoverMethod::with_params(
            CONSOLE_EXECUTE,
            &[(
                "line",
                true,
                json!({ "type": "string", "maxLength": MAX_CONSOLE_LINE }),
            )],
        ),
    )]
}

/// The longest name of a server [`parse_broadcast`] takes.
pub const MAX_SERVER: usize = 64;

/// Reads the `params` of [`BROADCAST`]: the server (if any) and the line. Whether the server exists is for the proxy
/// to say. The error never quotes what was sent.
pub fn parse_broadcast(params: Option<&Value>) -> Result<(Option<String>, String), String> {
    const SHAPE: &str =
        "params must be an object with \"text\" and, if you like, \"server\" (strings)";
    let params = params.ok_or("params are missing")?;
    if !params.is_object() {
        return Err(SHAPE.into());
    }
    let params: BroadcastParams = serde_json::from_value(params.clone()).map_err(|_| SHAPE)?;
    if params
        .server
        .as_deref()
        .is_some_and(|server| !server_name(server))
    {
        return Err("\"server\" is not a name of a server".into());
    }
    check_text(&params.text)?;
    Ok((params.server, params.text))
}

/// How `rpc.discover` of the proxy describes the orders that message.
pub fn message_methods() -> Vec<(&'static str, DiscoverMethod)> {
    vec![
        (
            PLAYERS_TELL,
            DiscoverMethod::with_params(
                PLAYERS_TELL,
                &[
                    ("player", true, json!({ "type": "string" })),
                    (
                        "text",
                        true,
                        json!({ "type": "string", "maxLength": MAX_TEXT }),
                    ),
                ],
            ),
        ),
        (
            BROADCAST,
            DiscoverMethod::with_params(
                BROADCAST,
                &[
                    ("server", false, json!({ "type": "string" })),
                    (
                        "text",
                        true,
                        json!({ "type": "string", "maxLength": MAX_TEXT }),
                    ),
                ],
            ),
        ),
    ]
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
    fn a_limit_and_a_cursor_that_are_null_are_the_first_page_and_an_order_with_no_params_takes_none()
     {
        let first = Page {
            limit: DEFAULT_PAGE,
            after: None,
        };
        assert_eq!(
            parse_list(Some(&json!({"limit": null, "cursor": null}))),
            Ok(first.clone())
        );
        assert_eq!(parse_list(Some(&json!({"limit": null}))), Ok(first));
        for ok in [None, Some(&Value::Null), Some(&json!({}))] {
            assert_eq!(parse_no_params(ok), Ok(()));
        }
        for bad in [
            json!({"x": 1}),
            json!([]),
            json!([1]),
            json!("x"),
            json!(0),
            json!(false),
        ] {
            assert_eq!(
                parse_no_params(Some(&bad)),
                Err("this order takes no parameters".to_owned()),
                "{bad}"
            );
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

    // ---- the orders that message ---------------------------------------------------------------------

    #[test]
    fn a_player_is_told_by_name_or_id_and_the_line_is_checked_like_the_line_of_a_command() {
        let tell = |v: Value| parse_tell(Some(&v));
        assert_eq!(
            tell(json!({"player": "Steve", "text": "Hello."})),
            Ok((Who::Name("Steve".into()), "Hello.".into()))
        );
        assert_eq!(
            tell(json!({"player": ALEX.to_ascii_uppercase(), "text": "x"})),
            Ok((Who::Id(ALEX.into()), "x".into()))
        );
        // The longest line, and one more.
        assert!(tell(json!({"player": "a-b", "text": "é".repeat(MAX_TEXT)})).is_ok());
        let secret = "ZZZ-must-not-come-back";
        for bad in [
            json!({"player": "Steve", "text": ""}),
            json!({"player": "Steve", "text": "é".repeat(MAX_TEXT + 1)}),
            json!({"player": "Steve", "text": format!("{secret}\n")}),
            json!({"player": "Steve", "text": format!("{secret}\u{a7}c")}),
            json!({"player": "Steve", "text": format!("{secret}\u{202e}")}),
            json!({"player": "Steve"}),
            json!({"text": "hi"}),
            json!({"player": "", "text": "hi"}),
            json!({"player": 7, "text": "hi"}),
            json!({"player": "Steve", "text": "hi", "more": 1}),
            json!(["Steve", "hi"]),
            json!("hi"),
            json!(null),
        ] {
            let why = tell(bad.clone()).unwrap_err();
            assert!(!why.contains("ZZZ") && !why.is_empty(), "{bad}: {why}");
        }
        assert!(parse_tell(None).is_err());
    }

    #[test]
    fn a_broadcast_has_a_line_and_may_name_a_server() {
        let cast = |v: Value| parse_broadcast(Some(&v));
        assert_eq!(
            cast(json!({"text": "Restart in 5 minutes."})),
            Ok((None, "Restart in 5 minutes.".into()))
        );
        assert_eq!(
            cast(json!({"server": "east", "text": "Hi"})),
            Ok((Some("east".into()), "Hi".into()))
        );
        let secret = "ZZZ-must-not-come-back";
        for bad in [
            json!({}),
            json!({"server": "east"}),
            json!({"server": "", "text": "Hi"}),
            json!({"server": "x".repeat(MAX_SERVER + 1), "text": "Hi"}),
            json!({"server": format!("{secret}\n"), "text": "Hi"}),
            json!({"server": 3, "text": "Hi"}),
            json!({"text": ""}),
            json!({"text": format!("{secret}\u{7}")}),
            json!({"text": "é".repeat(MAX_TEXT + 1)}),
            json!({"text": "Hi", "more": 1}),
            json!(["Hi"]),
            json!(null),
        ] {
            let why = cast(bad.clone()).unwrap_err();
            assert!(!why.contains("ZZZ") && !why.is_empty(), "{bad}: {why}");
        }
        assert!(parse_broadcast(None).is_err());
    }

    #[test]
    fn a_line_can_be_cleaned_into_one_the_proxy_takes() {
        let dirty = format!("a\nb\u{a7}c\u{202e}d{}", "x".repeat(2 * MAX_TEXT));
        let clean = clean_text(&dirty);
        assert!(
            clean.starts_with("abcdx") && clean.chars().count() == MAX_TEXT,
            "{clean:?}"
        );
        assert!(crate::run::check_line(&clean).is_ok());
        assert_eq!(clean_text("Hello."), "Hello.");
        // Nothing but what a line may not hold: nothing is left, and an order says the line is empty.
        assert_eq!(clean_text("\n\u{a7}"), "");
    }

    #[test]
    fn what_the_sdk_writes_for_the_orders_that_message_is_what_the_proxy_reads() {
        let written = serde_json::to_value(TellParams {
            player: "Steve".into(),
            text: "Hello.".into(),
        })
        .unwrap();
        assert_eq!(written, json!({"player": "Steve", "text": "Hello."}));
        assert!(parse_tell(Some(&written)).is_ok());
        // `server` is left out when it is not given, not written as null.
        let written = serde_json::to_value(BroadcastParams {
            server: None,
            text: "Hi".into(),
        })
        .unwrap();
        assert_eq!(written, json!({"text": "Hi"}));
        assert_eq!(
            serde_json::to_value(Told { told: 3 }).unwrap(),
            json!({"told": 3})
        );
        let methods = message_methods();
        let names: Vec<_> = methods.iter().map(|(_, m)| m.name.as_str()).collect();
        assert_eq!(names, [PLAYERS_TELL, BROADCAST]);
    }

    // ---- the order that moves ------------------------------------------------------------------------

    #[test]
    fn whom_to_send_and_where_are_read_with_exactly_one_of_player_all_and_from() {
        let send = |v: Value| parse_send(Some(&v));
        assert_eq!(
            send(json!({"to": "east", "player": "Steve"})),
            Ok((Target::Player(Who::Name("Steve".into())), "east".into()))
        );
        assert_eq!(
            send(json!({"to": "east", "player": ALEX.to_ascii_uppercase()})),
            Ok((Target::Player(Who::Id(ALEX.into())), "east".into()))
        );
        assert_eq!(
            send(json!({"to": "east", "all": true})),
            Ok((Target::All, "east".into()))
        );
        assert_eq!(
            send(json!({"to": "east", "from": "lobby"})),
            Ok((Target::Server("lobby".into()), "east".into()))
        );
        let secret = "ZZZ-must-not-come-back";
        for bad in [
            json!({}),
            json!({"to": "east"}),
            json!({"player": "Steve"}),
            json!({"to": "east", "all": false}),
            json!({"to": "east", "player": "Steve", "all": true}),
            json!({"to": "east", "player": "Steve", "from": "lobby"}),
            json!({"to": "east", "all": true, "from": "lobby"}),
            json!({"to": "", "all": true}),
            json!({"to": "x".repeat(MAX_SERVER + 1), "all": true}),
            json!({"to": format!("{secret}\n"), "all": true}),
            json!({"to": "east", "from": format!("{secret}\n")}),
            json!({"to": "east", "from": ""}),
            json!({"to": "east", "player": ""}),
            json!({"to": "east", "player": format!("{secret}\n")}),
            json!({"to": "east", "all": "yes"}),
            json!({"to": 7, "all": true}),
            json!({"to": "east", "all": true, "more": 1}),
            json!(["east", "all"]),
            json!("all"),
            json!(null),
        ] {
            let why = send(bad.clone()).unwrap_err();
            assert!(!why.contains("ZZZ") && !why.is_empty(), "{bad}: {why}");
        }
        assert!(parse_send(None).is_err());
    }

    #[test]
    fn what_the_sdk_writes_for_the_order_that_moves_is_what_the_proxy_reads() {
        let written = serde_json::to_value(SendParams {
            to: "east".into(),
            player: Some("Steve".into()),
            ..SendParams::default()
        })
        .unwrap();
        // The keys that are not given are left out, not written as null.
        assert_eq!(written, json!({"to": "east", "player": "Steve"}));
        assert!(parse_send(Some(&written)).is_ok());
        assert_eq!(
            serde_json::to_value(Started { started: 4 }).unwrap(),
            json!({"started": 4})
        );
        let methods = move_methods();
        assert_eq!(methods.len(), 1);
        assert_eq!(methods[0].0, PLAYERS_SEND);
        assert!(methods[0].1.params[0].required && !methods[0].1.params[1].required);
    }

    // ---- the order that expels -----------------------------------------------------------------------

    #[test]
    fn a_player_is_kicked_by_name_or_id_with_a_reason_that_is_checked_like_a_line() {
        let kick = |v: Value| parse_kick(Some(&v));
        assert_eq!(
            kick(json!({"player": "Steve"})),
            Ok((Who::Name("Steve".into()), DEFAULT_KICK_REASON.to_owned()))
        );
        assert_eq!(
            kick(json!({"player": ALEX.to_ascii_uppercase(), "reason": "Spam."})),
            Ok((Who::Id(ALEX.into()), "Spam.".to_owned()))
        );
        assert!(kick(json!({"player": "Steve", "reason": "é".repeat(MAX_TEXT)})).is_ok());
        // A null reason is no reason.
        assert_eq!(
            kick(json!({"player": "Steve", "reason": null})),
            Ok((Who::Name("Steve".into()), DEFAULT_KICK_REASON.to_owned()))
        );
        let secret = "ZZZ-must-not-come-back";
        for bad in [
            json!({"player": "Steve", "reason": ""}),
            json!({"player": "Steve", "reason": "é".repeat(MAX_TEXT + 1)}),
            json!({"player": "Steve", "reason": format!("{secret}\n")}),
            json!({"player": "Steve", "reason": format!("{secret}\u{a7}4")}),
            json!({"player": "Steve", "reason": format!("{secret}\u{202e}")}),
            json!({"player": "Steve", "reason": 5}),
            json!({"reason": "Spam."}),
            json!({"player": "", "reason": "Spam."}),
            json!({"player": format!("{secret}\n")}),
            json!({"player": "Steve", "more": 1}),
            json!(["Steve", "Spam."]),
            json!(null),
        ] {
            let why = kick(bad.clone()).unwrap_err();
            assert!(!why.contains("ZZZ") && !why.is_empty(), "{bad}: {why}");
        }
        assert!(parse_kick(None).is_err());
    }

    #[test]
    fn what_the_sdk_writes_for_the_order_that_expels_is_what_the_proxy_reads() {
        let written = serde_json::to_value(KickParams {
            player: "Steve".into(),
            reason: None,
        })
        .unwrap();
        assert_eq!(written, json!({"player": "Steve"}));
        assert!(parse_kick(Some(&written)).is_ok());
        assert_eq!(
            serde_json::to_value(Kicked { kicked: 1 }).unwrap(),
            json!({"kicked": 1})
        );
        let methods = moderate_methods();
        assert_eq!((methods.len(), methods[0].0), (1, PLAYERS_KICK));
        assert!(methods[0].1.params[0].required && !methods[0].1.params[1].required);
    }

    // ---- the orders that operate ---------------------------------------------------------------------

    #[test]
    fn drain_takes_a_server_and_whether_to_move_its_players_and_undrain_a_server() {
        let drain = |v: Value| parse_drain(Some(&v));
        assert_eq!(
            drain(json!({"server": "lobby"})),
            Ok(("lobby".into(), false))
        );
        assert_eq!(
            drain(json!({"server": "lobby", "move": true})),
            Ok(("lobby".into(), true))
        );
        assert_eq!(
            drain(json!({"server": "lobby", "move": false})),
            Ok(("lobby".into(), false))
        );
        assert_eq!(
            drain(json!({"server": "lobby", "move": null})),
            Ok(("lobby".into(), false))
        );
        let secret = "ZZZ-must-not-come-back";
        for bad in [
            json!({}),
            json!({"move": true}),
            json!({"server": ""}),
            json!({"server": "x".repeat(MAX_SERVER + 1)}),
            json!({"server": format!("{secret}\n")}),
            json!({"server": "lobby", "move": "yes"}),
            json!({"server": "lobby", "more": 1}),
            json!({"server": 7}),
            json!(["lobby"]),
            json!("lobby"),
            json!(null),
        ] {
            let why = drain(bad.clone()).unwrap_err();
            assert!(!why.contains("ZZZ") && !why.is_empty(), "{bad}: {why}");
        }
        assert!(parse_drain(None).is_err());
        assert_eq!(
            parse_undrain(Some(&json!({"server": "lobby"}))),
            Ok("lobby".into())
        );
        for bad in [
            json!({}),
            json!({"server": ""}),
            json!({"server": "lobby", "move": true}),
            json!(["lobby"]),
            json!(null),
        ] {
            assert!(parse_undrain(Some(&bad)).is_err(), "{bad}");
        }
        assert!(parse_undrain(None).is_err());
    }

    #[test]
    fn what_the_sdk_writes_for_the_orders_that_operate_is_what_the_proxy_reads() {
        let written = serde_json::to_value(DrainParams {
            server: "lobby".into(),
            move_players: Some(true),
        })
        .unwrap();
        // The key is `move`, which is a word of the language: the field is not called that.
        assert_eq!(written, json!({"server": "lobby", "move": true}));
        assert_eq!(parse_drain(Some(&written)), Ok(("lobby".into(), true)));
        let left_out = serde_json::to_value(DrainParams {
            server: "lobby".into(),
            move_players: None,
        })
        .unwrap();
        assert_eq!(left_out, json!({"server": "lobby"}));
        assert_eq!(
            serde_json::to_value(Drained {
                drained: true,
                moving: 3
            })
            .unwrap(),
            json!({"drained": true, "moving": 3})
        );
        assert_eq!(
            serde_json::to_value(Undrained { undrained: false }).unwrap(),
            json!({"undrained": false})
        );
        let methods = operate_methods();
        let names: Vec<_> = methods.iter().map(|(_, m)| m.name.as_str()).collect();
        assert_eq!(names, [SERVERS_DRAIN, SERVERS_UNDRAIN, RELOAD]);
    }

    // ---- the order that runs a console line ----------------------------------------------------------

    #[test]
    fn a_console_line_is_one_plain_line_and_the_error_never_quotes_it() {
        let line = |v: Value| parse_execute(Some(&v));
        assert_eq!(line(json!({"line": "list"})), Ok("list".into()));
        assert_eq!(
            line(json!({"line": "kick Steve  Spam, again"})),
            Ok("kick Steve  Spam, again".into())
        );
        assert_eq!(
            line(json!({"line": "x".repeat(MAX_CONSOLE_LINE)})).map(|l| l.len()),
            Ok(MAX_CONSOLE_LINE)
        );
        let secret = "ZZZ-must-not-come-back";
        for bad in [
            json!({"line": ""}),
            json!({"line": "   "}),
            json!({"line": "x".repeat(MAX_CONSOLE_LINE + 1)}),
            json!({"line": format!("{secret}\nend")}),
            json!({"line": format!("{secret}\r")}),
            json!({"line": format!("{secret}\t")}),
            json!({"line": format!("{secret}\u{a7}4")}),
            json!({"line": format!("{secret}\u{202e}")}),
            json!({"line": format!("{secret}\0")}),
            json!({"line": 5}),
            json!({"command": "list"}),
            json!({"line": "list", "more": 1}),
            json!(["list"]),
            json!("list"),
            json!(null),
        ] {
            let why = line(bad.clone()).unwrap_err();
            assert!(!why.contains("ZZZ") && !why.is_empty(), "{bad}: {why}");
        }
        assert!(parse_execute(None).is_err());
    }

    #[test]
    fn the_output_is_plain_lines_and_is_cut_where_the_limit_says_with_a_mark() {
        let said = |lines: &[&str]| {
            console_output(&lines.iter().map(|l| (*l).to_owned()).collect::<Vec<_>>())
        };
        assert_eq!(
            said(&[]),
            Executed {
                lines: vec![],
                truncated: false
            }
        );
        // What a line may not hold is dropped; the line breaks end nothing: a line is a line.
        assert_eq!(
            said(&["a\u{a7}4b\x1b[0m", "", "c\nd"]),
            Executed {
                lines: vec!["a4b[0m".into(), "".into(), "cd".into()],
                truncated: false
            }
        );
        // Exactly the limit is not cut; one more is.
        let full = "x".repeat(MAX_CONSOLE_OUTPUT);
        assert!(!said(&[&full]).truncated);
        let over = said(&[&full, "more"]);
        assert_eq!(
            (
                over.truncated,
                over.lines.len(),
                over.lines.last().map(String::as_str)
            ),
            (true, 2, Some(OUTPUT_TRUNCATED))
        );
        assert_eq!(over.lines[0].chars().count(), MAX_CONSOLE_OUTPUT);
        let cut = said(&[&"y".repeat(MAX_CONSOLE_OUTPUT - 3), "abcdef", "never"]);
        assert_eq!(cut.lines[1], "abc");
        assert_eq!(cut.lines.len(), 3);
        assert!(cut.truncated);
        // Empty lines count, so many of them are bounded too.
        let many = said(&vec![""; MAX_CONSOLE_OUTPUT + 5]);
        assert!(many.truncated && many.lines.len() <= MAX_CONSOLE_OUTPUT + 2);
        assert!(!said(&vec![""; MAX_CONSOLE_OUTPUT]).truncated);
    }

    #[test]
    fn what_the_sdk_writes_for_the_order_that_runs_a_console_line_is_what_the_proxy_reads() {
        let written = serde_json::to_value(ExecuteParams {
            line: "alert hello".into(),
        })
        .unwrap();
        assert_eq!(written, json!({"line": "alert hello"}));
        assert_eq!(parse_execute(Some(&written)), Ok("alert hello".into()));
        assert_eq!(
            serde_json::to_value(Executed {
                lines: vec!["sent".into()],
                truncated: false
            })
            .unwrap(),
            json!({"lines": ["sent"], "truncated": false})
        );
        let methods = console_methods();
        assert_eq!((methods.len(), methods[0].0), (1, CONSOLE_EXECUTE));
        assert!(methods[0].1.params[0].required);
    }
}
