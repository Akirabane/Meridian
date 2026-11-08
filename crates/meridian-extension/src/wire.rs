//! The messages of protocol 0 and the JSON-RPC 2.0 envelope they travel in (docs/EXTENSIONS.md,
//! sections 2 and 3).
//!
//! One WebSocket text message is one JSON-RPC message. Ids are numbers, and each direction has its own:
//! the proxy's requests are numbered by the proxy and the extension's by the extension. A field a side
//! does not know is ignored (section 3), so every type here reads what it knows and leaves the rest.
//!
//! The nonces and proofs are lower-case hexadecimal text on the wire. This module keeps them as the
//! text it was given; `handshake` makes and checks them.

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

/// The `Sec-WebSocket-Protocol` the proxy offers and the extension must choose (section 2.2).
pub const SUBPROTOCOL: &str = "meridian-extension";

/// The versions of the protocol this code speaks, newest last. Version 0 is experimental (section 3).
pub const SUPPORTED: &[u32] = &[0];

/// The longest message either side accepts, 256 KiB (section 2.1).
pub const MAX_MESSAGE: usize = 256 << 10;

/// The upgrade request the proxy sends to an extension that listens at `host:port`: it offers the
/// subprotocol and carries no secret and no `Origin` (section 2.2). `key` is a `Sec-WebSocket-Key`.
pub fn upgrade_request(host: &str, port: u16, key: &str) -> String {
    format!(
        "GET / HTTP/1.1\r\nHost: {host}:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\
         Sec-WebSocket-Protocol: {SUBPROTOCOL}\r\n\r\n"
    )
}

/// The methods of protocol 0.
pub mod method {
    /// The proxy's first message.
    pub const HELLO: &str = "meridian:hello";
    /// The proxy's proof, once it has checked the extension's.
    pub const CONFIRM: &str = "meridian:hello/confirm";
    /// Asked by either side, answered by both (as Horizon's management protocol does).
    pub const DISCOVER: &str = "rpc.discover";
    /// The same call, under the namespace the other methods have.
    pub const DISCOVER_ALIAS: &str = "meridian:rpc.discover";

    /// Whether `name` is the call that lists the methods a side serves, under either name.
    pub fn is_discover(name: &str) -> bool {
        name == DISCOVER || name == DISCOVER_ALIAS
    }
}

/// The JSON-RPC error codes this protocol uses.
pub mod code {
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;
    /// An order over the extension's budget (docs/EXTENSIONS.md, section 5): it was not run. Asking again
    /// later is fine. In the range JSON-RPC keeps for the errors of a server.
    pub const OVER_BUDGET: i64 = -32001;
    /// Too many orders of the extension are running already: this one was not run. Asking again later is
    /// fine.
    pub const TOO_MANY_RUNNING: i64 = -32002;
}

/// `params` of `meridian:hello`, the proxy's first message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    /// The versions the proxy speaks.
    pub protocol: Vec<u32>,
    /// The version of Meridian.
    pub proxy: String,
    /// 32 random bytes, in hexadecimal.
    pub proxy_nonce: String,
}

/// `result` of `meridian:hello`: the extension's choice, its proof, and what it declares.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloAnswer {
    /// The version the extension chose among the proxy's.
    pub protocol: u32,
    /// 32 random bytes, in hexadecimal.
    pub extension_nonce: String,
    /// `HMAC-SHA256(token, "meridian-extension/extension" | proxy_nonce | extension_nonce)`, in
    /// hexadecimal.
    pub proof: String,
    /// What the extension calls itself, for the logs. The operator's name for it is the configuration's.
    pub name: String,
    pub version: String,
    /// What the extension wants (section 2.3): `decide.login`, `orders.read`, and so on. The proxy
    /// grants the intersection with the configuration.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// The commands of its own it declares, with the `commands` capability (section 4.3).
    #[serde(default)]
    pub commands: Vec<CommandSpec>,
}

/// A command an extension declares in its hello.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandSpec {
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub usage: String,
}

/// The most characters of a command's name or alias.
pub const MAX_COMMAND_WORD: usize = 32;

/// The most commands a hello declares: the proxy reads no more, and the SDK panics at one more.
pub const MAX_COMMANDS: usize = 64;

/// The most aliases of one command.
pub const MAX_COMMAND_ALIASES: usize = 8;

/// The most characters of a command's `usage`.
pub const MAX_COMMAND_USAGE: usize = 256;

impl CommandSpec {
    /// Every word a player types for the command: its name, then its aliases.
    pub fn words(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.name.as_str()).chain(self.aliases.iter().map(String::as_str))
    }

    /// Whether the declaration is one the proxy accepts: a name and at most [`MAX_COMMAND_ALIASES`] aliases
    /// of 1 to [`MAX_COMMAND_WORD`] characters among `a-z`, `0-9`, `-` and `_`, none given twice, and a
    /// `usage` of at most [`MAX_COMMAND_USAGE`] characters. The error says what is wrong and never quotes a
    /// word (the proxy logs it, and the word is the extension's). Whether a word is also taken by a command
    /// of the proxy or of another extension is for the proxy to say.
    pub fn check(&self) -> Result<(), String> {
        if self.aliases.len() > MAX_COMMAND_ALIASES {
            return Err(format!(
                "a command has at most {MAX_COMMAND_ALIASES} aliases"
            ));
        }
        if self.usage.chars().count() > MAX_COMMAND_USAGE {
            return Err(format!(
                "the usage of a command is longer than {MAX_COMMAND_USAGE} characters"
            ));
        }
        let mut seen: Vec<&str> = Vec::new();
        for word in self.words() {
            let fine = !word.is_empty()
                && word.chars().count() <= MAX_COMMAND_WORD
                && word.bytes().all(|b| {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_'
                });
            if !fine {
                return Err(format!(
                    "a command word has 1 to {MAX_COMMAND_WORD} characters among a-z, 0-9, - and _"
                ));
            }
            if seen.contains(&word) {
                return Err("a command word is given twice".into());
            }
            seen.push(word);
        }
        Ok(())
    }
}

/// `params` of `meridian:hello/confirm`: the proxy's proof.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Confirm {
    /// `HMAC-SHA256(token, "meridian-extension/proxy" | extension_nonce | proxy_nonce)`, in
    /// hexadecimal.
    pub proof: String,
}

/// `data` of the error `-32600` an extension answers when none of the proxy's versions is one it speaks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Supported {
    pub supported: Vec<u32>,
}

/// `result` of `rpc.discover`, in the shape of Horizon's management protocol (an OpenRPC document,
/// `management_schema.rs`), as far as this protocol needs it: the methods that side serves, each with the
/// parameters it takes. `openrpc` and `info` are what Horizon sends and are kept when they come; a reader
/// needs only `methods` (docs/EXTENSIONS.md, section 3). `rpc.discover` itself is not listed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Discover {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openrpc: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub info: Option<DiscoverInfo>,
    pub methods: Vec<DiscoverMethod>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoverInfo {
    pub title: String,
    pub version: String,
}

/// A method in `rpc.discover`: its name with the `meridian:` prefix, and its parameters (empty, or
/// left out, when it has none).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoverMethod {
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<DiscoverParam>,
}

/// A parameter of a method: its name, whether it is required, and a JSON Schema for it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoverParam {
    pub name: String,
    pub required: bool,
    #[serde(default)]
    pub schema: Value,
}

impl DiscoverMethod {
    /// A method that takes no parameters.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            params: Vec::new(),
        }
    }

    /// A method that takes the parameters `(name, required, JSON type)` that are listed, by name.
    pub fn with_params(name: impl Into<String>, params: &[(&str, bool, Value)]) -> Self {
        Self {
            name: name.into(),
            params: params
                .iter()
                .map(|(name, required, schema)| DiscoverParam {
                    name: (*name).into(),
                    required: *required,
                    schema: schema.clone(),
                })
                .collect(),
        }
    }

    /// A method that takes one required parameter, as the decision points take their `request`, of
    /// the JSON type `kind` (`"object"`, ...).
    pub fn with_request(name: impl Into<String>, kind: &str) -> Self {
        Self {
            name: name.into(),
            params: vec![DiscoverParam {
                name: "request".into(),
                required: true,
                schema: json!({ "type": kind }),
            }],
        }
    }
}

impl Discover {
    /// A document that lists `methods` for protocol `version`, titled as this protocol is.
    pub fn new(version: u32, methods: Vec<DiscoverMethod>) -> Self {
        Self {
            openrpc: Some("1.3.2".into()),
            info: Some(DiscoverInfo {
                title: "Meridian extension protocol".into(),
                version: version.to_string(),
            }),
            methods,
        }
    }
}

/// The `error` of a JSON-RPC answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    /// The error of a hello with no version in common, which lists the versions the extension speaks.
    pub fn version_mismatch(supported: &[u32]) -> Self {
        Self {
            code: code::INVALID_REQUEST,
            message: "no protocol version in common".into(),
            data: Some(json!({ "supported": supported })),
        }
    }

    /// The versions a mismatch error lists, if this is one.
    pub fn supported(&self) -> Option<Vec<u32>> {
        if self.code != code::INVALID_REQUEST {
            return None;
        }
        let data: Supported = serde_json::from_value(self.data.clone()?).ok()?;
        Some(data.supported)
    }
}

/// The id of a JSON-RPC request. The proxy numbers its own requests; an extension may use numbers or
/// text (a program in another language often takes a UUID), and the answer carries the id exactly as
/// it came.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Id {
    Number(u64),
    Text(String),
}

impl From<u64> for Id {
    fn from(id: u64) -> Self {
        Self::Number(id)
    }
}

/// A message read off the wire.
#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    /// A request, which has an id and wants an answer, or a notification, which has none.
    Request {
        id: Option<Id>,
        method: String,
        params: Option<Value>,
    },
    /// The answer to a request this side sent. `id` is `None` only when the other side could not read
    /// the request's id.
    Response {
        id: Option<Id>,
        outcome: Result<Value, RpcError>,
    },
}

impl Message {
    /// Reads one text message. The error says what is wrong in a few words, and never quotes the
    /// message, which may carry a player's data.
    pub fn parse(text: &str) -> Result<Self, String> {
        let value: Value = serde_json::from_str(text).map_err(|_| "not JSON".to_string())?;
        let Some(object) = value.as_object() else {
            return Err("not a JSON object".into());
        };
        if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Err("not JSON-RPC 2.0".into());
        }
        let id = match object.get("id") {
            None | Some(Value::Null) => None,
            Some(Value::String(text)) => Some(Id::Text(text.clone())),
            Some(id) => Some(Id::Number(
                id.as_u64()
                    .ok_or("an id that is neither a whole number nor text")?,
            )),
        };
        if let Some(method) = object.get("method") {
            let method = method.as_str().ok_or("a method that is not text")?;
            return Ok(Self::Request {
                id,
                method: method.to_string(),
                params: object.get("params").cloned(),
            });
        }
        if let Some(error) = object.get("error") {
            let error: RpcError =
                serde_json::from_value(error.clone()).map_err(|_| "an invalid error")?;
            return Ok(Self::Response {
                id,
                outcome: Err(error),
            });
        }
        match object.get("result") {
            Some(result) => Ok(Self::Response {
                id,
                outcome: Ok(result.clone()),
            }),
            None => Err("neither a method, a result nor an error".into()),
        }
    }
}

/// Reads `params` or `result` as the type a method defines. The error says what is wrong with the
/// shape and never shows a value: serde's own message quotes the value it found (`invalid type: string
/// "Alex"`), which may be a player's name, and the message ends in a log line.
pub fn decode<T: DeserializeOwned>(value: Option<&Value>) -> Result<T, String> {
    let value = value.ok_or("no parameters")?;
    serde_json::from_value(value.clone())
        .map_err(|e| format!("invalid parameters: {}", shape_of(&e)))
}

/// What serde_json's error says without the value it found. Only the name of a missing field (the
/// type's own) and what was expected (also the type's own) are kept.
fn shape_of(error: &serde_json::Error) -> String {
    let text = error.to_string();
    if let Some(rest) = text.strip_prefix("missing field `")
        && let Some((name, _)) = rest.split_once('`')
    {
        return format!("missing field `{name}`");
    }
    for kind in ["invalid type", "invalid value", "invalid length"] {
        if text.starts_with(&format!("{kind}: "))
            // The value may itself contain ", expected ": what was expected is after the last one.
            && let Some((_, expected)) = text.rsplit_once(", expected ")
        {
            let expected = expected
                .rsplit_once(" at line ")
                .map_or(expected, |(e, _)| e);
            return format!("{kind}, expected {expected}");
        }
    }
    "the shape is not the one expected".into()
}

/// A request, as the text of one message. `params` is left out when it is `None`.
pub fn request(id: impl Into<Id>, method: &str, params: Option<&impl Serialize>) -> String {
    let mut message = json!({ "jsonrpc": "2.0", "id": id.into(), "method": method });
    if let Some(params) = params {
        message["params"] = serde_json::to_value(params).unwrap_or(Value::Null);
    }
    message.to_string()
}

/// A notification: a request with no id, which gets no answer.
pub fn notification(method: &str, params: &impl Serialize) -> String {
    json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": serde_json::to_value(params).unwrap_or(Value::Null),
    })
    .to_string()
}

/// The answer to request `id`.
pub fn result(id: &Id, result: &impl Serialize) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": serde_json::to_value(result).unwrap_or(Value::Null),
    })
    .to_string()
}

/// The error answer to request `id`, or to a request whose id could not be read (`None`).
pub fn error(id: Option<&Id>, error: &RpcError) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": serde_json::to_value(error).unwrap_or(Value::Null),
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A golden file: the bytes the proxy's tests and the SDK's both read.
    fn golden(name: &str) -> String {
        let path = format!("{}/golden/protocol-0/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
    }

    fn value(text: &str) -> Value {
        serde_json::from_str(text).unwrap()
    }

    /// The golden file, read as a message and written again, is the same JSON.
    fn same_json(name: &str, written: &str) {
        assert_eq!(value(&golden(name)), value(written), "{name}");
    }

    fn spec(name: &str, aliases: &[&str]) -> CommandSpec {
        CommandSpec {
            name: name.into(),
            aliases: aliases.iter().map(|a| (*a).to_owned()).collect(),
            usage: String::new(),
        }
    }

    #[test]
    fn a_command_is_declared_with_plain_lower_case_words_given_once() {
        assert_eq!(spec("party", &["p", "pa-rty_2"]).check(), Ok(()));
        assert_eq!(spec("a", &[]).check(), Ok(()));
        assert_eq!(
            spec(&"x".repeat(MAX_COMMAND_WORD), &[]).check(),
            Ok(()),
            "the longest word"
        );
        let declared = spec("party", &["p"]);
        assert_eq!(declared.words().collect::<Vec<_>>(), ["party", "p"]);
        for bad in [
            spec("", &[]),
            spec("Party", &[]),
            spec("par ty", &[]),
            spec("/party", &[]),
            spec("part\u{e9}", &[]),
            spec("par\nty", &[]),
            spec(&"x".repeat(MAX_COMMAND_WORD + 1), &[]),
            spec("party", &[""]),
            spec("party", &["P"]),
            // A word twice: the name and an alias, or two aliases.
            spec("party", &["party"]),
            spec("party", &["p", "p"]),
        ] {
            let why = bad.check().unwrap_err();
            // The word is the extension's: it is never quoted.
            assert!(!why.contains("Party") && !why.contains('\n'), "{why}");
        }
    }

    #[test]
    fn a_command_has_at_most_eight_aliases_and_a_usage_of_at_most_256_characters() {
        let aliases = |n: usize| -> Vec<String> { (0..n).map(|i| format!("a{i}")).collect() };
        let with = |n: usize, usage: &str| CommandSpec {
            name: "party".into(),
            aliases: aliases(n),
            usage: usage.into(),
        };
        assert_eq!(with(MAX_COMMAND_ALIASES, "").check(), Ok(()));
        assert_eq!(
            with(0, &"u".repeat(MAX_COMMAND_USAGE)).check(),
            Ok(()),
            "the longest usage"
        );
        let why = with(MAX_COMMAND_ALIASES + 1, "").check().unwrap_err();
        assert!(why.contains("at most 8 aliases"), "{why}");
        let why = with(0, &"PrivateUsage".repeat(30)).check().unwrap_err();
        assert!(why.contains("256") && !why.contains("Private"), "{why}");
        assert_eq!(
            (MAX_COMMANDS, MAX_COMMAND_ALIASES, MAX_COMMAND_USAGE),
            (64, 8, 256)
        );
    }

    #[test]
    fn the_hello_request_is_what_the_golden_file_says() {
        let Message::Request { id, method, params } =
            Message::parse(&golden("hello.request.json")).unwrap()
        else {
            panic!("not a request");
        };
        assert_eq!((id, method.as_str()), (Some(Id::Number(1)), method::HELLO));
        let hello: Hello = decode(params.as_ref()).unwrap();
        assert_eq!(hello.protocol, vec![0]);
        assert_eq!(hello.proxy, "0.1.0");
        assert_eq!(hello.proxy_nonce.len(), 64);
        same_json(
            "hello.request.json",
            &request(1, method::HELLO, Some(&hello)),
        );
    }

    #[test]
    fn the_hello_answer_is_what_the_golden_file_says() {
        let Message::Response { id, outcome } =
            Message::parse(&golden("hello.answer.json")).unwrap()
        else {
            panic!("not a response");
        };
        assert_eq!(id, Some(Id::Number(1)));
        let answer: HelloAnswer = decode(outcome.as_ref().ok()).unwrap();
        assert_eq!(answer.protocol, 0);
        assert_eq!(answer.name, "perms");
        assert_eq!(answer.capabilities, ["decide.command", "commands"]);
        assert_eq!(
            answer.commands,
            [CommandSpec {
                name: "party".into(),
                aliases: vec!["p".into()],
                usage: "/party <invite|leave> [player]".into(),
            }]
        );
        same_json("hello.answer.json", &result(&Id::Number(1), &answer));
    }

    #[test]
    fn the_confirmation_is_what_the_golden_files_say() {
        let Message::Request { id, method, params } =
            Message::parse(&golden("confirm.request.json")).unwrap()
        else {
            panic!("not a request");
        };
        assert_eq!(
            (id, method.as_str()),
            (Some(Id::Number(2)), method::CONFIRM)
        );
        let confirm: Confirm = decode(params.as_ref()).unwrap();
        assert_eq!(confirm.proof.len(), 64);
        same_json(
            "confirm.request.json",
            &request(2, method::CONFIRM, Some(&confirm)),
        );
        same_json("confirm.answer.json", &result(&Id::Number(2), &json!({})));
    }

    #[test]
    fn a_version_mismatch_lists_what_the_extension_speaks() {
        let Message::Response { id, outcome } =
            Message::parse(&golden("version-mismatch.error.json")).unwrap()
        else {
            panic!("not a response");
        };
        assert_eq!(id, Some(Id::Number(1)));
        let error_read = outcome.unwrap_err();
        assert_eq!(error_read.code, code::INVALID_REQUEST);
        assert_eq!(error_read.supported(), Some(vec![1, 2]));
        same_json(
            "version-mismatch.error.json",
            &error(Some(&Id::Number(1)), &RpcError::version_mismatch(&[1, 2])),
        );
    }

    #[test]
    fn only_a_mismatch_error_lists_versions() {
        assert_eq!(RpcError::new(code::INVALID_REQUEST, "no").supported(), None);
        let other = RpcError {
            data: Some(json!({ "supported": [1] })),
            ..RpcError::new(code::INTERNAL_ERROR, "no")
        };
        assert_eq!(other.supported(), None);
    }

    #[test]
    fn rpc_discover_is_what_the_golden_files_say() {
        let Message::Request { id, method, params } =
            Message::parse(&golden("discover.request.json")).unwrap()
        else {
            panic!("not a request");
        };
        assert_eq!(
            (id, method.as_str(), params),
            (Some(Id::Number(3)), method::DISCOVER, None)
        );
        same_json(
            "discover.request.json",
            &request(3, method::DISCOVER, None::<&Value>),
        );

        let Message::Response { outcome, .. } =
            Message::parse(&golden("discover.answer.json")).unwrap()
        else {
            panic!("not a response");
        };
        let document: Discover = decode(outcome.as_ref().ok()).unwrap();
        assert_eq!(
            document,
            Discover::new(
                0,
                vec![
                    DiscoverMethod::with_params(
                        method::HELLO,
                        &[
                            (
                                "protocol",
                                true,
                                json!({ "type": "array", "items": { "type": "integer" } })
                            ),
                            ("proxy", true, json!({ "type": "string" })),
                            ("proxy_nonce", true, json!({ "type": "string" })),
                        ]
                    ),
                    DiscoverMethod::with_params(
                        method::CONFIRM,
                        &[("proof", true, json!({ "type": "string" }))]
                    ),
                    DiscoverMethod::with_request("meridian:decide/login", "object"),
                    DiscoverMethod::with_request("meridian:decide/command", "object"),
                    DiscoverMethod::with_request("meridian:command/run", "object"),
                ]
            )
        );
        same_json("discover.answer.json", &result(&Id::Number(3), &document));
    }

    #[test]
    fn a_field_nobody_knows_is_ignored_and_a_missing_optional_one_is_empty() {
        let text = r#"{"jsonrpc":"2.0","id":9,"result":{"protocol":0,"extension_nonce":"aa",
            "proof":"bb","name":"n","version":"1","something_new":{"a":1}}}"#;
        let Message::Response { outcome, .. } = Message::parse(text).unwrap() else {
            panic!("not a response");
        };
        let answer: HelloAnswer = decode(outcome.as_ref().ok()).unwrap();
        assert!(answer.capabilities.is_empty() && answer.commands.is_empty());
    }

    #[test]
    fn what_is_not_a_json_rpc_message_is_refused_without_quoting_it() {
        for (text, why) in [
            ("hello", "not JSON"),
            ("[1]", "not a JSON object"),
            (r#"{"id":1,"method":"x"}"#, "not JSON-RPC 2.0"),
            (
                r#"{"jsonrpc":"1.0","id":1,"method":"x"}"#,
                "not JSON-RPC 2.0",
            ),
            (
                r#"{"jsonrpc":"2.0","id":1.5,"method":"x"}"#,
                "an id that is neither a whole number nor text",
            ),
            (
                r#"{"jsonrpc":"2.0","id":{},"method":"x"}"#,
                "an id that is neither a whole number nor text",
            ),
            (
                r#"{"jsonrpc":"2.0","id":1,"method":4}"#,
                "a method that is not text",
            ),
            (
                r#"{"jsonrpc":"2.0","id":1}"#,
                "neither a method, a result nor an error",
            ),
            (
                r#"{"jsonrpc":"2.0","id":1,"error":"secret-player-data"}"#,
                "an invalid error",
            ),
        ] {
            assert_eq!(Message::parse(text), Err(why.to_string()), "{text}");
        }
    }

    #[test]
    fn a_notification_has_no_id_and_an_unreadable_request_is_answered_with_a_null_one() {
        assert_eq!(
            Message::parse(r#"{"jsonrpc":"2.0","method":"x"}"#),
            Ok(Message::Request {
                id: None,
                method: "x".into(),
                params: None,
            })
        );
        let answer = error(None, &RpcError::new(code::INVALID_REQUEST, "bad"));
        assert_eq!(value(&answer)["id"], Value::Null);
        assert_eq!(
            Message::parse(&answer),
            Ok(Message::Response {
                id: None,
                outcome: Err(RpcError::new(code::INVALID_REQUEST, "bad")),
            })
        );
    }

    #[test]
    fn a_result_that_is_null_is_still_a_result() {
        assert_eq!(
            Message::parse(r#"{"jsonrpc":"2.0","id":4,"result":null}"#),
            Ok(Message::Response {
                id: Some(Id::Number(4)),
                outcome: Ok(Value::Null),
            })
        );
    }

    #[test]
    fn a_hello_missing_a_field_is_refused() {
        let params = value(r#"{"protocol":[0],"proxy":"0.1.0"}"#);
        assert!(decode::<Hello>(Some(&params)).is_err());
        assert!(decode::<Hello>(None).is_err());
    }

    #[test]
    fn a_string_id_is_answered_as_it_came_and_a_number_stays_a_number() {
        let Message::Request { id, .. } =
            Message::parse(&golden("discover.request.string-id.json")).unwrap()
        else {
            panic!("not a request");
        };
        let id = id.unwrap();
        assert_eq!(id, Id::Text("3f2c9d5e-8a41-4b7e-9c10-5d6e7f809a1b".into()));
        let answer = Discover {
            openrpc: None,
            info: None,
            methods: vec![DiscoverMethod::new("meridian:players/list")],
        };
        same_json("discover.answer.string-id.json", &result(&id, &answer));
        // The proxy's own ids are numbers, and a number is written as a number.
        assert_eq!(value(&request(7, "x", None::<&Value>))["id"], json!(7));
        // An answer to an error carries the text id too.
        let text = error(Some(&id), &RpcError::new(code::METHOD_NOT_FOUND, "no"));
        assert_eq!(
            value(&text)["id"],
            json!("3f2c9d5e-8a41-4b7e-9c10-5d6e7f809a1b")
        );
    }

    #[test]
    fn a_discover_document_needs_only_its_methods() {
        let text = r#"{"methods":[{"name":"meridian:decide/login"},
            {"name":"m","params":[{"name":"p","required":false}],"description":"ignored"}]}"#;
        let document: Discover = decode(Some(&value(text))).unwrap();
        assert_eq!(document.openrpc, None);
        assert!(document.methods[0].params.is_empty());
        assert_eq!(document.methods[1].params[0].schema, Value::Null);
        // A method with no parameters is written without the list, as Horizon's reader may expect.
        let written = serde_json::to_value(&document.methods[0]).unwrap();
        assert_eq!(written, json!({ "name": "meridian:decide/login" }));
    }

    #[test]
    fn the_upgrade_offers_the_subprotocol_and_carries_no_secret() {
        let request = upgrade_request("127.0.0.1", 25590, "dGhlIHNhbXBsZSBub25jZQ==");
        assert!(request.starts_with("GET / HTTP/1.1\r\nHost: 127.0.0.1:25590\r\n"));
        assert!(request.contains("Sec-WebSocket-Protocol: meridian-extension\r\n"));
        assert!(request.contains("Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n"));
        let lower = request.to_ascii_lowercase();
        assert!(!lower.contains("authorization") && !lower.contains("origin"));
        assert!(request.ends_with("\r\n\r\n"));
    }

    #[test]
    fn discover_is_asked_under_either_name_and_is_not_listed_in_its_own_answer() {
        assert!(method::is_discover("rpc.discover"));
        assert!(method::is_discover("meridian:rpc.discover"));
        assert!(!method::is_discover("meridian:hello"));
        assert!(!method::is_discover("minecraft:rpc.discover"));
        let Message::Response { outcome, .. } =
            Message::parse(&golden("discover.answer.json")).unwrap()
        else {
            panic!("not a response");
        };
        let document: Discover = decode(outcome.as_ref().ok()).unwrap();
        assert!(
            document
                .methods
                .iter()
                .all(|m| !method::is_discover(&m.name))
        );
        // The two methods of the handshake are listed.
        let names: Vec<_> = document.methods.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&method::HELLO) && names.contains(&method::CONFIRM));
    }

    #[test]
    fn what_is_wrong_with_parameters_never_quotes_a_value() {
        let wrong = |params: &str| {
            let value = value(params);
            decode::<Hello>(Some(&value)).unwrap_err()
        };
        // A string where a list is wanted, a number where a string is, a missing field.
        let errors = [
            wrong(r#"{"protocol":"Alex","proxy":"x","proxy_nonce":"y"}"#),
            wrong(r#"{"protocol":[0],"proxy":"x","proxy_nonce":7}"#),
            wrong(r#"{"protocol":[0],"proxy":"x"}"#),
            wrong(r#"{"protocol":["Alex, expected 3"],"proxy":"x","proxy_nonce":"y"}"#),
            wrong(r#"{"protocol":[-12345],"proxy":"x","proxy_nonce":"y"}"#),
            wrong(r#""Alex""#),
        ];
        for error in &errors {
            for secret in ["Alex", "12345", "7"] {
                assert!(!error.contains(secret), "{error}");
            }
            assert!(!error.contains("line"), "{error}");
        }
        assert_eq!(errors[2], "invalid parameters: missing field `proxy_nonce`");
        assert_eq!(
            errors[0],
            "invalid parameters: invalid type, expected a sequence"
        );
        assert_eq!(
            errors[1],
            "invalid parameters: invalid type, expected a string"
        );
    }
}
