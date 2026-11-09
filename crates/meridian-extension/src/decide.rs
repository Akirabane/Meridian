//! The decision points (docs/EXTENSIONS.md, section 4), as both sides read and write them: the admission at
//! login (4.2), the choice of server (4.4) and the permission of a command (4.3).
//!
//! The proxy asks with [`LoginParams`] and reads the answer with [`parse_login_answer`], with
//! [`ServerParams`] and [`parse_server_answer`], or with [`CommandParams`] and [`parse_command_answer`];
//! the SDK reads the params and writes the answer with the same types. What the proxy accepts as a
//! refusal's reason is decided in one place, [`Reason`], which the SDK also makes its reasons with: an
//! extension cannot send one the proxy would refuse. The golden files `login.*.json`, `server.*.json` and
//! `command.*.json` are the bytes both sides test against.

use crate::wire;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The method of the admission at login.
pub const LOGIN: &str = "meridian:decide/login";

/// The most characters of a reason a player is shown (section 4.1).
pub const MAX_REASON_CHARS: usize = 256;

/// How the proxy knows who the player is (docs/OPERATING.md, section 5.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Auth {
    /// The session service confirmed the account.
    Verified,
    /// Offline mode, or an unverified player of the hybrid mode: **the name and the id prove nothing**.
    /// A word the extension does not know, which a later version of the proxy may send for a new kind
    /// of account (a field a side does not know is ignored, section 3), is read as this: nothing is
    /// taken as proved that was not said to be.
    #[serde(other)]
    Unverified,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Player {
    pub name: String,
    pub uuid: String,
    pub auth: Auth,
}

/// What the proxy tells the extension of a player who has logged in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginRequest {
    pub player: Player,
    /// The host name the player typed, when the handshake gave one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// The player's IP address, only with the `addresses` capability.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    /// Whether a session of the same player is open now (a second login).
    pub connected: bool,
}

/// `params` of [`LOGIN`]: the request is the one parameter, named `request` as `rpc.discover` lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginParams {
    pub request: LoginRequest,
}

/// The text a refused player reads: plain text of at most [`MAX_REASON_CHARS`] characters, with no
/// control character but the line break. The proxy shows it as a literal, never as a JSON text component
/// (section 4.1), and checks it with [`Reason::check`]; [`Reason::new`] makes one that passes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reason(String);

impl Reason {
    /// A reason from `text`: the control characters are dropped (the line break stays) and the text is
    /// cut to [`MAX_REASON_CHARS`] characters. What the extension means is kept; what the proxy would
    /// refuse is not.
    pub fn new(text: &str) -> Self {
        Self(
            text.chars()
                .filter(|c| !c.is_control() || *c == '\n')
                .take(MAX_REASON_CHARS)
                .collect(),
        )
    }

    /// Whether `text` is a valid reason, as the proxy checks the one an extension sent. The error says
    /// what is wrong in a few words and never quotes the text.
    pub fn check(text: &str) -> Result<(), String> {
        if text.chars().count() > MAX_REASON_CHARS {
            return Err(format!(
                "the reason is longer than {MAX_REASON_CHARS} characters"
            ));
        }
        if text.chars().any(|c| c.is_control() && c != '\n') {
            return Err("the reason holds a control character".into());
        }
        Ok(())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What an extension decides about a login.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Let the player go on to the built-in checks that follow, and record that the extension looked.
    Allow,
    /// Refuse the login. Without a reason the player reads the proxy's own words.
    Deny(Option<Reason>),
    /// As if no extension were asked: the player goes on to the built-in checks.
    Default,
}

impl Decision {
    /// A refusal with `reason`, made valid by [`Reason::new`].
    pub fn deny(reason: &str) -> Self {
        Self::Deny(Some(Reason::new(reason)))
    }
}

/// The three words of the answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Verdict {
    Allow,
    Deny,
    Default,
}

/// `result` of [`LOGIN`], as it is on the wire. The proxy reads it with [`parse_login_answer`], which
/// also checks the reason; the SDK writes it from a [`Decision`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginAnswer {
    decision: Verdict,
    /// Any JSON value on the wire: it is read, and must be text, only on `deny`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<Value>,
}

impl From<&Decision> for LoginAnswer {
    fn from(decision: &Decision) -> Self {
        match decision {
            Decision::Allow => Self {
                decision: Verdict::Allow,
                reason: None,
            },
            Decision::Deny(reason) => Self {
                decision: Verdict::Deny,
                reason: reason
                    .as_ref()
                    .map(|r| Value::String(r.as_str().to_owned())),
            },
            Decision::Default => Self {
                decision: Verdict::Default,
                reason: None,
            },
        }
    }
}

impl LoginAnswer {
    /// The decision, if the answer is valid. A reason on `allow` or `default` is not read, whatever it
    /// is, and an empty or null one on `deny` is no reason; a reason on `deny` must be text.
    pub fn into_decision(self) -> Result<Decision, String> {
        Ok(match self.decision {
            Verdict::Allow => Decision::Allow,
            Verdict::Default => Decision::Default,
            Verdict::Deny => Decision::Deny(reason_of(self.reason)?),
        })
    }
}

/// The reason of a refusal as it came on the wire: none when it is missing, null or empty, text that
/// passes [`Reason::check`] otherwise, and an error for anything else.
fn reason_of(reason: Option<Value>) -> Result<Option<Reason>, String> {
    match reason {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if text.is_empty() => Ok(None),
        Some(Value::String(text)) => {
            Reason::check(&text)?;
            Ok(Some(Reason(text)))
        }
        Some(_) => Err("the reason is not text".into()),
    }
}

/// Reads the `result` of [`LOGIN`] as the proxy must: the shape, and then the reason. An error says what
/// is wrong and never shows what the extension sent.
pub fn parse_login_answer(result: &Value) -> Result<Decision, String> {
    wire::decode::<LoginAnswer>(Some(result))
        .map_err(|why| why.replace("invalid parameters", "invalid answer"))?
        .into_decision()
}

/// The method of the choice of server.
pub const SERVER: &str = "meridian:decide/server";

/// Why the proxy is placing the player.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Placing {
    /// The player has logged in and the proxy chooses their first server.
    Login,
    /// The player's server was lost and the proxy chooses another ([`ServerRequest::lost`] names it).
    Fallback,
    /// A reason a later version of the proxy may add. Nothing is promised for it: an extension that
    /// does not know what it is should answer `default`.
    #[serde(other)]
    Other,
}

/// Why a server is not open to new players, when its `state` is `draining` or `not_offered`: one word of a fixed list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Why {
    /// The proxy believes it is going down for a restart (it announced its stop, or stopped answering).
    Restarting,
    /// Drained on purpose: from the console, or by an extension (`servers/drain`).
    Drained,
    /// It does not take the players the proxy sends (its transfers are off).
    NotAccepting,
    /// It has not reported its state recently enough to be believed.
    NoReport,
    /// A word that this version does not know.
    #[serde(other)]
    Other,
}

/// What a server last said of itself (Horizon's `transfer-status`), read by the proxy at its rounds of readings. A field the
/// server did not give is absent. `age_ms` is how long ago it was read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerReport {
    /// The players it counts: placed, held for a hand-over and still joining (Horizon's `online`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub players: Option<u64>,
    /// The most players it takes (`max_players`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity: Option<u64>,
    /// Players held for a hand-over, not placed yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held: Option<u64>,
    /// Places promised to players announced by the proxy, not logged in yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<u64>,
    /// Players it is handing over to another server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frozen: Option<u64>,
    /// Whether it takes no more ordinary logins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full: Option<bool>,
    /// Whether it has begun to stop and takes nobody any more.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopping: Option<bool>,
    /// Milliseconds since the reading.
    pub age_ms: u64,
}

/// A server the player may be sent to, as the proxy sees it now. With balancing off, or for a server
/// that has no role, everything but the name is null: the proxy knows nothing more of it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub name: String,
    /// `lobby`, `sublobby` or `game`.
    #[serde(default)]
    pub role: Option<String>,
    /// The players it counts: those on it, and the places held for players being sent there.
    #[serde(default)]
    pub players: Option<u32>,
    #[serde(default)]
    pub soft_cap: Option<u32>,
    #[serde(default)]
    pub hard_cap: Option<u32>,
    /// `open`, `closed` (at its soft cap), `draining` (kept from new players on purpose) or
    /// `not_offered` (no recent report from it). A word a later version adds is to be read as `closed`.
    #[serde(default)]
    pub state: Option<String>,
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

/// What the proxy tells the extension when it places a player.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerRequest {
    pub player: Player,
    pub reason: Placing,
    /// The host name the player typed, when the handshake gave one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// With [`Placing::Fallback`], the server the player lost; null otherwise.
    #[serde(default)]
    pub lost: Option<String>,
    /// What the routes allow, in the order the proxy would try them. Already without the servers that
    /// lost the player, and without those of the other kind of account.
    pub candidates: Vec<Candidate>,
}

/// `params` of [`SERVER`]: the request is the one parameter, named `request`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerParams {
    pub request: ServerRequest,
}

/// What an extension decides about the placing of a player.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Choice {
    /// The proxy goes on as it would without an extension: the routes, the health order and the balancer.
    Default,
    /// The proxy tries these servers in this order, and only these. Each must be a candidate. With
    /// balancing on, the balancer is not asked, and each server is held a place by the rule of a server
    /// the player named (the hard cap holds); one that is full or draining is passed over.
    Order(Vec<String>),
    /// Refuse the player. Without a reason they read the proxy's own words.
    Refuse(Option<Reason>),
}

impl Choice {
    /// Try these servers, in this order.
    pub fn order<I, S>(servers: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::Order(servers.into_iter().map(Into::into).collect())
    }

    /// A refusal with `reason`, made valid by [`Reason::new`].
    pub fn refuse(reason: &str) -> Self {
        Self::Refuse(Some(Reason::new(reason)))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ChoiceWord {
    Default,
    Order,
    Refuse,
}

/// `result` of [`SERVER`], as it is on the wire. The proxy reads it with [`parse_server_answer`], which
/// also checks the servers and the reason; the SDK writes it from a [`Choice`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerAnswer {
    choice: ChoiceWord,
    /// Any JSON value on the wire: it is read, and must be a list of names, only on `order`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    servers: Option<Value>,
    /// Any JSON value on the wire: it is read, and must be text, only on `refuse`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<Value>,
}

impl From<&Choice> for ServerAnswer {
    fn from(choice: &Choice) -> Self {
        match choice {
            Choice::Default => Self {
                choice: ChoiceWord::Default,
                servers: None,
                reason: None,
            },
            Choice::Order(servers) => Self {
                choice: ChoiceWord::Order,
                servers: Some(Value::Array(
                    servers.iter().cloned().map(Value::String).collect(),
                )),
                reason: None,
            },
            Choice::Refuse(reason) => Self {
                choice: ChoiceWord::Refuse,
                servers: None,
                reason: reason
                    .as_ref()
                    .map(|r| Value::String(r.as_str().to_owned())),
            },
        }
    }
}

impl ServerAnswer {
    /// The choice, if the answer is valid: on `order`, a list of at least one name, each one of
    /// `offered` (a name twice counts once, at its first place); on `refuse`, a reason as for a login's
    /// `deny`. `servers` on another word, and `reason` on another word, are not read. An error says what
    /// is wrong and never shows the names the extension sent.
    pub fn into_choice(self, offered: &[String]) -> Result<Choice, String> {
        Ok(match self.choice {
            ChoiceWord::Default => Choice::Default,
            ChoiceWord::Refuse => Choice::Refuse(reason_of(self.reason)?),
            ChoiceWord::Order => {
                let Some(Value::Array(names)) = self.servers else {
                    return Err("an order needs a list of servers".into());
                };
                let mut order: Vec<String> = Vec::with_capacity(names.len().min(offered.len()));
                for name in names {
                    let Value::String(name) = name else {
                        return Err("a server of the order is not text".into());
                    };
                    if !offered.contains(&name) {
                        return Err("the order names a server that was not offered".into());
                    }
                    if !order.contains(&name) {
                        order.push(name);
                    }
                }
                if order.is_empty() {
                    return Err("the order is empty".into());
                }
                Choice::Order(order)
            }
        })
    }
}

/// Reads the `result` of [`SERVER`] as the proxy must: the shape, then the servers against `offered`
/// (the names of the candidates it sent), then the reason. An error says what is wrong and never shows
/// what the extension sent.
pub fn parse_server_answer(result: &Value, offered: &[String]) -> Result<Choice, String> {
    wire::decode::<ServerAnswer>(Some(result))
        .map_err(|why| why.replace("invalid parameters", "invalid answer"))?
        .into_choice(offered)
}

/// The method of the permission of a command.
pub const COMMAND: &str = "meridian:decide/command";

/// The most arguments of a command the proxy tells the extension; the rest are left out.
pub const MAX_COMMAND_ARGS: usize = 32;

/// The most characters of an argument the proxy tells the extension; the rest of it is cut.
pub const MAX_ARG_CHARS: usize = 256;

/// What the proxy tells the extension when a player types a command the proxy answers itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandRequest {
    pub player: Player,
    /// The server the player is on.
    pub server: String,
    /// The command as typed, without its slash.
    pub command: String,
    /// What follows it, in words: at most [`MAX_COMMAND_ARGS`] of them, each cut to [`MAX_ARG_CHARS`].
    pub args: Vec<String>,
    /// Every node the command checks, which the answer may speak of.
    pub nodes: Vec<String>,
}

/// `params` of [`COMMAND`]: the request is the one parameter, named `request`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandParams {
    pub request: CommandRequest,
}

/// What an extension says of one node of permission.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeVerdict {
    /// The player has the node, **even if `permissions.toml` or the operators of the server refuse it**:
    /// the extension is the source of rights for the proxy's commands (docs/EXTENSIONS.md, 4.3).
    Allow,
    /// The player has not the node, even if `permissions.toml` gives it.
    Deny,
    /// Left to `permissions.toml` and the operators of the server, as without an extension.
    Default,
}

/// What an extension answers about the permission of a command: a verdict for each node it speaks of
/// (a node left out is [`NodeVerdict::Default`]), and how long the proxy may keep them for this player.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct NodeAnswers {
    nodes: std::collections::BTreeMap<String, NodeVerdict>,
    #[serde(skip_serializing_if = "is_zero")]
    cache_seconds: u64,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

impl NodeAnswers {
    /// No opinion on any node: all of them are `default`.
    pub fn new() -> Self {
        Self::default()
    }

    /// The player has `node`, whatever the file says.
    pub fn allow(mut self, node: &str) -> Self {
        self.nodes.insert(node.to_owned(), NodeVerdict::Allow);
        self
    }

    /// The player has not `node`, whatever the file says.
    pub fn deny(mut self, node: &str) -> Self {
        self.nodes.insert(node.to_owned(), NodeVerdict::Deny);
        self
    }

    /// Every node of `request` gets `verdict`.
    pub fn all(request: &CommandRequest, verdict: NodeVerdict) -> Self {
        Self {
            nodes: request.nodes.iter().map(|n| (n.clone(), verdict)).collect(),
            cache_seconds: 0,
        }
    }

    /// Lets the proxy keep these answers for this player for `seconds` (0, the default, keeps none). The
    /// proxy keeps them no longer than its own `cache-seconds-max`.
    pub fn cache_seconds(mut self, seconds: u64) -> Self {
        self.cache_seconds = seconds;
        self
    }

    /// What was said of `node`; [`NodeVerdict::Default`] when nothing was.
    pub fn verdict(&self, node: &str) -> NodeVerdict {
        self.nodes
            .get(node)
            .copied()
            .unwrap_or(NodeVerdict::Default)
    }

    /// How long the proxy may keep the answers, in seconds.
    pub fn seconds(&self) -> u64 {
        self.cache_seconds
    }
}

/// Reads the `result` of [`COMMAND`] as the proxy must. Only the nodes in `asked` are read, and each
/// must be `allow`, `deny` or `default`; a node that was not asked is left out whatever it says (so an
/// answer cannot grow what the proxy keeps), and so is a field nobody knows. `nodes` may be missing, and
/// then every node is `default`. `cache_seconds` must be a whole number of seconds, or missing. An error
/// says what is wrong and never shows what the extension sent.
pub fn parse_command_answer(result: &Value, asked: &[String]) -> Result<NodeAnswers, String> {
    #[derive(Deserialize)]
    struct Wire {
        #[serde(default)]
        nodes: Option<Value>,
        #[serde(default)]
        cache_seconds: Option<Value>,
    }
    let wire: Wire = wire::decode(Some(result))
        .map_err(|why| why.replace("invalid parameters", "invalid answer"))?;
    let mut answers = NodeAnswers::new();
    match wire.nodes {
        None | Some(Value::Null) => {}
        Some(Value::Object(nodes)) => {
            for node in asked {
                let Some(word) = nodes.get(node) else {
                    continue;
                };
                let verdict = match word.as_str() {
                    Some("allow") => NodeVerdict::Allow,
                    Some("deny") => NodeVerdict::Deny,
                    Some("default") => NodeVerdict::Default,
                    _ => {
                        return Err(
                            "a node has an answer that is not allow, deny or default".into()
                        );
                    }
                };
                answers.nodes.insert(node.clone(), verdict);
            }
        }
        Some(_) => return Err("nodes is not an object".into()),
    }
    answers.cache_seconds = match wire.cache_seconds {
        None | Some(Value::Null) => 0,
        Some(value) => value
            .as_u64()
            .ok_or_else(|| "cache_seconds is not a whole number of seconds".to_owned())?,
    };
    Ok(answers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::Message;
    use serde_json::json;

    fn golden(name: &str) -> String {
        let path = format!("{}/golden/protocol-0/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
    }

    fn value(text: &str) -> Value {
        serde_json::from_str(text).unwrap()
    }

    fn golden_result(name: &str) -> Value {
        match Message::parse(&golden(name)).unwrap() {
            Message::Response {
                outcome: Ok(result),
                ..
            } => result,
            other => panic!("{other:?}"),
        }
    }

    fn alex() -> LoginRequest {
        LoginRequest {
            player: Player {
                name: "Alex".into(),
                uuid: "0a2ea4ba-bced-39d2-80e1-1ad51a0e925c".into(),
                auth: Auth::Verified,
            },
            host: Some("play.example.org".into()),
            address: Some("203.0.113.7".into()),
            connected: false,
        }
    }

    #[test]
    fn the_login_request_is_what_the_golden_file_says() {
        let Message::Request { id, method, params } =
            Message::parse(&golden("login.request.json")).unwrap()
        else {
            panic!("a request");
        };
        assert_eq!((id, method.as_str()), (Some(4.into()), LOGIN));
        let read: LoginParams = wire::decode(params.as_ref()).unwrap();
        assert_eq!(read, LoginParams { request: alex() });
        // And written again, it is the same JSON.
        let written = wire::request(4, LOGIN, Some(&LoginParams { request: alex() }));
        assert_eq!(value(&written), value(&golden("login.request.json")));
    }

    #[test]
    fn the_answers_are_what_the_golden_files_say() {
        for (name, decision) in [
            ("login.answer.allow.json", Decision::Allow),
            ("login.answer.default.json", Decision::Default),
            (
                "login.answer.deny.json",
                Decision::deny("The event starts at 20:00."),
            ),
        ] {
            let result = golden_result(name);
            assert_eq!(parse_login_answer(&result).unwrap(), decision, "{name}");
            let written = wire::result(&4.into(), &LoginAnswer::from(&decision));
            assert_eq!(value(&written), value(&golden(name)), "{name}");
        }
    }

    #[test]
    fn what_the_proxy_does_not_send_is_left_out_and_what_is_missing_is_none() {
        let mut request = alex();
        request.host = None;
        request.address = None;
        let written = serde_json::to_value(&request).unwrap();
        assert_eq!(
            written,
            json!({"player": {"name": "Alex", "uuid": "0a2ea4ba-bced-39d2-80e1-1ad51a0e925c", "auth": "verified"}, "connected": false})
        );
        assert_eq!(
            serde_json::from_value::<LoginRequest>(written).unwrap(),
            request
        );
        // `connected` and the player are always sent: a request without them is not one.
        assert!(serde_json::from_value::<LoginRequest>(json!({"connected": true})).is_err());
        assert!(
            serde_json::from_value::<LoginRequest>(
                json!({"player": {"name": "A", "uuid": "x", "auth": "verified"}})
            )
            .is_err()
        );
        // A field nobody knows is ignored (section 3).
        let mut extra = serde_json::to_value(alex()).unwrap();
        extra["added_later"] = json!(1);
        assert_eq!(
            serde_json::from_value::<LoginRequest>(extra).unwrap(),
            alex()
        );
        // The kinds of account are the two words of the specification, and a word that is not one of them
        // is an account that is not known to be verified, not a request that cannot be read.
        assert_eq!(
            serde_json::from_value::<Auth>(json!("verified")).unwrap(),
            Auth::Verified
        );
        assert_eq!(
            serde_json::from_value::<Auth>(json!("unverified")).unwrap(),
            Auth::Unverified
        );
        assert_eq!(
            serde_json::from_value::<Auth>(json!("premium")).unwrap(),
            Auth::Unverified
        );
        let mut later = serde_json::to_value(alex()).unwrap();
        later["player"]["auth"] = json!("xbox");
        let read: LoginRequest = serde_json::from_value(later).unwrap();
        assert_eq!(read.player.auth, Auth::Unverified);
        assert_eq!(
            serde_json::to_value(Auth::Unverified).unwrap(),
            json!("unverified")
        );
    }

    #[test]
    fn a_reason_is_plain_text_of_at_most_256_characters() {
        assert!(Reason::check("The event starts at 20:00.").is_ok());
        assert!(Reason::check("two\nlines").is_ok());
        assert!(Reason::check(&"é".repeat(256)).is_ok());
        assert!(Reason::check(&"é".repeat(257)).is_err());
        for bad in ["bell\u{7}", "esc\u{1b}[31m", "tab\there", "nul\0", "cr\r"] {
            assert!(Reason::check(bad).is_err(), "{bad:?}");
        }
        // The error never quotes the text.
        let why = Reason::check("secret\u{7}").unwrap_err();
        assert!(!why.contains("secret"), "{why}");
    }

    #[test]
    fn a_reason_that_is_made_always_passes_the_check() {
        for text in [
            "plain",
            "two\nlines",
            "esc\u{1b}[31mred\u{7}",
            &"x".repeat(1000),
            &format!("{}\u{7}", "é".repeat(300)),
            "",
        ] {
            let reason = Reason::new(text);
            assert!(Reason::check(reason.as_str()).is_ok(), "{text:?}");
            assert!(reason.as_str().chars().count() <= MAX_REASON_CHARS);
        }
        assert_eq!(Reason::new("a\u{7}b\nc").as_str(), "ab\nc");
        assert_eq!(Reason::new(&"x".repeat(300)).as_str().len(), 256);
    }

    #[test]
    fn an_answer_the_proxy_cannot_use_is_refused_without_quoting_it() {
        for (bad, expected) in [
            (json!({"decision": "maybe"}), "the shape"),
            (json!({"decision": 3}), "invalid type"),
            (json!({}), "missing field `decision`"),
            (json!("allow"), "invalid type"),
            (json!(null), "invalid type"),
            (
                json!({"decision": "deny", "reason": "x".repeat(257)}),
                "longer than 256",
            ),
            (
                json!({"decision": "deny", "reason": "Steve\u{1b}[0m"}),
                "control character",
            ),
            (json!({"decision": "deny", "reason": 7}), "not text"),
            (json!({"decision": "deny", "reason": ["a"]}), "not text"),
            (
                json!({"decision": "deny", "reason": {"text": "a"}}),
                "not text",
            ),
        ] {
            let why = parse_login_answer(&bad).unwrap_err();
            assert!(why.contains(expected), "{bad}: {why}");
            assert!(!why.contains("maybe") && !why.contains("Steve"), "{why}");
        }
    }

    #[test]
    fn a_field_nobody_knows_is_ignored_and_a_reason_on_allow_is_not_read() {
        let allow = json!({"decision": "allow", "reason": "ignored\u{7}", "later": [1]});
        assert_eq!(parse_login_answer(&allow).unwrap(), Decision::Allow);
        let default = json!({"decision": "default", "reason": "x".repeat(999)});
        assert_eq!(parse_login_answer(&default).unwrap(), Decision::Default);
        // What the reason is does not matter where it is not read: it never makes an answer fail.
        for odd in [
            json!(7),
            json!(true),
            json!([1]),
            json!({"a": 1}),
            json!(null),
        ] {
            for word in ["allow", "default"] {
                let answer = json!({"decision": word, "reason": odd});
                assert!(parse_login_answer(&answer).is_ok(), "{answer}");
            }
        }
        // On `deny`, null is no reason.
        assert_eq!(
            parse_login_answer(&json!({"decision": "deny", "reason": null})).unwrap(),
            Decision::Deny(None)
        );
        // A refusal without a reason, or with an empty one, is a refusal without a reason.
        assert_eq!(
            parse_login_answer(&json!({"decision": "deny"})).unwrap(),
            Decision::Deny(None)
        );
        assert_eq!(
            parse_login_answer(&json!({"decision": "deny", "reason": ""})).unwrap(),
            Decision::Deny(None)
        );
        assert_eq!(
            LoginAnswer::from(&Decision::Deny(None)),
            LoginAnswer {
                decision: Verdict::Deny,
                reason: None
            }
        );
    }

    // ---- the choice of server ------------------------------------------------------------------

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_owned()).collect()
    }

    fn offered() -> Vec<String> {
        names(&["lobby", "lobby2", "build"])
    }

    fn server_request() -> ServerRequest {
        let seen = |name: &str, players, state: &str| Candidate {
            name: name.into(),
            role: Some("lobby".into()),
            players: Some(players),
            soft_cap: Some(20),
            hard_cap: Some(30),
            state: Some(state.into()),
            why: None,
            up: None,
            report: None,
        };
        ServerRequest {
            player: alex().player,
            reason: Placing::Login,
            host: Some("play.example.org".into()),
            lost: None,
            candidates: vec![
                seen("lobby", 12, "open"),
                seen("lobby2", 3, "draining"),
                Candidate {
                    name: "build".into(),
                    role: None,
                    players: None,
                    soft_cap: None,
                    hard_cap: None,
                    state: None,
                    why: None,
                    up: None,
                    report: None,
                },
            ],
        }
    }

    #[test]
    fn the_server_request_is_what_the_golden_file_says() {
        let Message::Request { id, method, params } =
            Message::parse(&golden("server.request.json")).unwrap()
        else {
            panic!("a request");
        };
        assert_eq!((id, method.as_str()), (Some(5.into()), SERVER));
        let read: ServerParams = wire::decode(params.as_ref()).unwrap();
        assert_eq!(
            read,
            ServerParams {
                request: server_request()
            }
        );
        let written = wire::request(
            5,
            SERVER,
            Some(&ServerParams {
                request: server_request(),
            }),
        );
        assert_eq!(value(&written), value(&golden("server.request.json")));
    }

    #[test]
    fn the_server_answers_are_what_the_golden_files_say() {
        for (name, choice) in [
            ("server.answer.default.json", Choice::Default),
            (
                "server.answer.order.json",
                Choice::order(["lobby2", "lobby"]),
            ),
            (
                "server.answer.refuse.json",
                Choice::refuse("No game is running."),
            ),
        ] {
            let result = golden_result(name);
            assert_eq!(
                parse_server_answer(&result, &offered()).unwrap(),
                choice,
                "{name}"
            );
            let written = wire::result(&5.into(), &ServerAnswer::from(&choice));
            assert_eq!(value(&written), value(&golden(name)), "{name}");
        }
    }

    #[test]
    fn a_fallback_names_the_lost_server_and_a_reason_it_does_not_know_is_other() {
        let mut request = server_request();
        request.reason = Placing::Fallback;
        request.lost = Some("lobby".into());
        request.host = None;
        let written = serde_json::to_value(&request).unwrap();
        assert_eq!(written["reason"], json!("fallback"));
        assert_eq!(written["lost"], json!("lobby"));
        assert!(written.get("host").is_none());
        assert_eq!(
            serde_json::from_value::<ServerRequest>(written).unwrap(),
            request
        );
        // `lost` is null, not left out, when nothing was lost (as the golden file has it).
        assert_eq!(
            serde_json::to_value(server_request()).unwrap()["lost"],
            Value::Null
        );
        let mut later = serde_json::to_value(server_request()).unwrap();
        later["reason"] = json!("evacuation");
        later["added_later"] = json!(1);
        later["candidates"][0]["added_later"] = json!(1);
        assert_eq!(
            serde_json::from_value::<ServerRequest>(later)
                .unwrap()
                .reason,
            Placing::Other
        );
        // A candidate the proxy knows nothing of but the name is still one.
        let bare: Candidate = serde_json::from_value(json!({"name": "build"})).unwrap();
        assert_eq!(bare.role, None);
        assert_eq!(bare.state, None);
    }

    #[test]
    fn an_order_names_offered_servers_once_each_in_the_order_given() {
        let answer = |servers: Value| json!({"choice": "order", "servers": servers});
        assert_eq!(
            parse_server_answer(&answer(json!(["build"])), &offered()).unwrap(),
            Choice::order(["build"])
        );
        // A name twice counts once, at its first place.
        assert_eq!(
            parse_server_answer(
                &answer(json!(["lobby2", "lobby", "lobby2", "lobby"])),
                &offered()
            )
            .unwrap(),
            Choice::order(["lobby2", "lobby"])
        );
        // What the proxy offered is the whole of what may be named, and a name is exact.
        for (bad, expected) in [
            (answer(json!(["east"])), "was not offered"),
            (answer(json!(["lobby", "east"])), "was not offered"),
            (answer(json!(["Lobby"])), "was not offered"),
            (answer(json!([])), "is empty"),
            (answer(json!("lobby")), "needs a list"),
            (answer(json!(null)), "needs a list"),
            (answer(json!([1])), "is not text"),
            (answer(json!([["lobby"]])), "is not text"),
            (json!({"choice": "order"}), "needs a list"),
        ] {
            let why = parse_server_answer(&bad, &offered()).unwrap_err();
            assert!(why.contains(expected), "{bad}: {why}");
            // The names the extension sent are not quoted back.
            assert!(!why.contains("east") && !why.contains("Lobby"), "{why}");
        }
        // With nothing offered, no order is valid.
        assert!(parse_server_answer(&answer(json!(["lobby"])), &[]).is_err());
    }

    #[test]
    fn a_refusal_of_a_server_is_read_as_a_denial_of_a_login_is() {
        assert_eq!(
            parse_server_answer(&json!({"choice": "refuse"}), &offered()).unwrap(),
            Choice::Refuse(None)
        );
        assert_eq!(
            parse_server_answer(&json!({"choice": "refuse", "reason": ""}), &offered()).unwrap(),
            Choice::Refuse(None)
        );
        assert_eq!(
            parse_server_answer(&json!({"choice": "refuse", "reason": null}), &offered()).unwrap(),
            Choice::Refuse(None)
        );
        for (bad, expected) in [
            (
                json!({"choice": "refuse", "reason": "x".repeat(257)}),
                "longer than 256",
            ),
            (
                json!({"choice": "refuse", "reason": "a\u{1b}[0m"}),
                "control character",
            ),
            (json!({"choice": "refuse", "reason": 7}), "not text"),
        ] {
            let why = parse_server_answer(&bad, &offered()).unwrap_err();
            assert!(why.contains(expected), "{bad}: {why}");
        }
        // The same limits as the SDK's own reasons.
        assert!(matches!(Choice::refuse("a\u{7}b"), Choice::Refuse(Some(r)) if r.as_str() == "ab"));
    }

    #[test]
    fn what_is_not_read_on_a_word_never_makes_an_answer_fail() {
        for odd in [json!(7), json!(true), json!({"a": 1}), json!(null)] {
            for word in ["default", "refuse"] {
                let answer = json!({"choice": word, "servers": odd});
                assert!(parse_server_answer(&answer, &offered()).is_ok(), "{answer}");
            }
            for word in ["default", "order"] {
                let answer = json!({"choice": word, "servers": ["lobby"], "reason": odd});
                assert!(parse_server_answer(&answer, &offered()).is_ok(), "{answer}");
            }
        }
        // A word that is not one of the three is not an answer, and the error does not quote it.
        for bad in [
            json!({"choice": "maybe"}),
            json!({}),
            json!("order"),
            json!(3),
        ] {
            let why = parse_server_answer(&bad, &offered()).unwrap_err();
            assert!(!why.contains("maybe"), "{why}");
        }
    }

    // ---- the permission of a command ------------------------------------------------------------

    fn command_request() -> CommandRequest {
        CommandRequest {
            player: alex().player,
            server: "lobby".into(),
            command: "server".into(),
            args: vec!["east".into()],
            nodes: vec!["meridian.server".into(), "meridian.server.east".into()],
        }
    }

    #[test]
    fn the_command_request_is_what_the_golden_file_says() {
        let Message::Request { id, method, params } =
            Message::parse(&golden("command.request.json")).unwrap()
        else {
            panic!("a request");
        };
        assert_eq!((id, method.as_str()), (Some(6.into()), COMMAND));
        let read: CommandParams = wire::decode(params.as_ref()).unwrap();
        assert_eq!(
            read,
            CommandParams {
                request: command_request()
            }
        );
        let written = wire::request(
            6,
            COMMAND,
            Some(&CommandParams {
                request: command_request(),
            }),
        );
        assert_eq!(value(&written), value(&golden("command.request.json")));
    }

    #[test]
    fn the_command_answers_are_what_the_golden_files_say() {
        let asked = command_request().nodes;
        for (name, answers) in [
            (
                "command.answer.json",
                NodeAnswers::new()
                    .allow("meridian.server")
                    .deny("meridian.server.east")
                    .cache_seconds(60),
            ),
            ("command.answer.default.json", NodeAnswers::new()),
        ] {
            let result = golden_result(name);
            assert_eq!(
                parse_command_answer(&result, &asked).unwrap(),
                answers,
                "{name}"
            );
            let written = wire::result(&6.into(), &answers);
            assert_eq!(value(&written), value(&golden(name)), "{name}");
        }
        let answers = parse_command_answer(&golden_result("command.answer.json"), &asked).unwrap();
        assert_eq!(answers.verdict("meridian.server"), NodeVerdict::Allow);
        assert_eq!(answers.verdict("meridian.server.east"), NodeVerdict::Deny);
        assert_eq!(answers.verdict("meridian.hub"), NodeVerdict::Default);
        assert_eq!(answers.seconds(), 60);
    }

    #[test]
    fn only_the_nodes_that_were_asked_are_read_and_a_missing_field_is_default() {
        let asked = vec!["meridian.hub".to_owned()];
        // A node that was not asked is left out, whatever it says, even something that is no verdict.
        let answers = parse_command_answer(
            &json!({"nodes": {"meridian.hub": "deny", "meridian.alert": "allow", "x": 7}, "later": 1}),
            &asked,
        )
        .unwrap();
        assert_eq!(answers, NodeAnswers::new().deny("meridian.hub"));
        for empty in [json!({}), json!({"nodes": null}), json!({"nodes": {}})] {
            assert_eq!(
                parse_command_answer(&empty, &asked).unwrap(),
                NodeAnswers::new()
            );
        }
        // Many nodes sent, a few asked: what is kept is bounded by what was asked.
        let many: serde_json::Map<String, Value> = (0..5000)
            .map(|i| (format!("n{i}"), json!("allow")))
            .collect();
        let kept = parse_command_answer(&json!({ "nodes": many }), &asked).unwrap();
        assert_eq!(kept, NodeAnswers::new());
    }

    #[test]
    fn an_answer_that_is_not_one_is_refused_without_quoting_it() {
        let asked = vec!["meridian.hub".to_owned()];
        for (bad, expected) in [
            (
                json!({"nodes": {"meridian.hub": "maybe"}}),
                "not allow, deny or default",
            ),
            (
                json!({"nodes": {"meridian.hub": true}}),
                "not allow, deny or default",
            ),
            (
                json!({"nodes": {"meridian.hub": null}}),
                "not allow, deny or default",
            ),
            (json!({"nodes": ["meridian.hub"]}), "not an object"),
            (json!({"nodes": "allow"}), "not an object"),
            (json!({"cache_seconds": -1}), "whole number"),
            (json!({"cache_seconds": 1.5}), "whole number"),
            (json!({"cache_seconds": "60"}), "whole number"),
            (json!("allow"), "invalid"),
            (json!(null), "invalid"),
        ] {
            let why = parse_command_answer(&bad, &asked).unwrap_err();
            assert!(why.contains(expected), "{bad}: {why}");
            assert!(!why.contains("maybe"), "{why}");
        }
        // A huge number is a number of seconds: the proxy bounds it with its own maximum.
        let huge = parse_command_answer(&json!({"cache_seconds": u64::MAX}), &asked).unwrap();
        assert_eq!(huge.seconds(), u64::MAX);
    }

    #[test]
    fn a_handler_can_give_one_verdict_to_every_node_of_a_request() {
        let all = NodeAnswers::all(&command_request(), NodeVerdict::Deny);
        assert_eq!(all.verdict("meridian.server"), NodeVerdict::Deny);
        assert_eq!(all.verdict("meridian.server.east"), NodeVerdict::Deny);
        assert_eq!(all.verdict("meridian.hub"), NodeVerdict::Default);
        // Nothing to keep: no `cache_seconds` on the wire.
        assert!(
            serde_json::to_value(&all)
                .unwrap()
                .get("cache_seconds")
                .is_none()
        );
    }
}
