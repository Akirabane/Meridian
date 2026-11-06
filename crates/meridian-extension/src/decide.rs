//! The decision points (docs/EXTENSIONS.md, section 4), as both sides read and write them. This is the
//! admission at login (4.2); the others come with their own pull requests.
//!
//! The proxy asks with [`LoginParams`] and reads the answer with [`parse_login_answer`]; the SDK reads the
//! params and writes the answer with the same types. What the proxy accepts as a refusal's reason is
//! decided in one place, [`Reason`], which the SDK also makes its reasons with: an extension cannot send
//! one the proxy would refuse. The golden files `login.*.json` are the bytes both sides test against.

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
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
                reason: reason.as_ref().map(|r| r.as_str().to_owned()),
            },
            Decision::Default => Self {
                decision: Verdict::Default,
                reason: None,
            },
        }
    }
}

impl LoginAnswer {
    /// The decision, if the answer is valid. A reason on `allow` or `default` is not read, and an empty
    /// one on `deny` is no reason.
    pub fn into_decision(self) -> Result<Decision, String> {
        Ok(match self.decision {
            Verdict::Allow => Decision::Allow,
            Verdict::Default => Decision::Default,
            Verdict::Deny => match self.reason.as_deref() {
                None | Some("") => Decision::Deny(None),
                Some(text) => {
                    Reason::check(text)?;
                    Decision::Deny(Some(Reason(text.to_owned())))
                }
            },
        })
    }
}

/// Reads the `result` of [`LOGIN`] as the proxy must: the shape, and then the reason. An error says what
/// is wrong and never shows what the extension sent.
pub fn parse_login_answer(result: &Value) -> Result<Decision, String> {
    wire::decode::<LoginAnswer>(Some(result))
        .map_err(|why| why.replace("invalid parameters", "invalid answer"))?
        .into_decision()
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
        // The kinds of account are the two words of the specification.
        assert!(serde_json::from_value::<Auth>(json!("premium")).is_err());
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
            (json!({"decision": "deny", "reason": 7}), "invalid type"),
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
}
