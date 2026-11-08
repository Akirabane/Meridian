//! The extension's own commands (docs/EXTENSIONS.md, 4.3, "The extension's own commands"), as both sides read
//! and write them: the request the proxy sends when a player types one, [`meridian:command/run`](RUN), and
//! the lines the extension answers.
//!
//! The extension declares its commands in the hello (`wire::CommandSpec`, under the `commands` capability).
//! The proxy sends [`RunParams`] and reads the answer with [`parse_run_answer`]; the SDK reads the params and
//! writes the answer with the same types. What the proxy accepts as a line is decided in one place,
//! [`check_line`], which the SDK also makes its lines with ([`Lines`]): an extension cannot send a line the
//! proxy would refuse. The golden files `run.*.json` are the bytes both sides test against.

use crate::decide::Player;
use crate::wire;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The method that runs one of the extension's own commands.
pub const RUN: &str = "meridian:command/run";

/// The most lines an answer holds.
pub const MAX_LINES: usize = 10;

/// The most characters of a line a player is shown.
pub const MAX_LINE_CHARS: usize = 256;

/// What the proxy tells the extension when a player types one of its commands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRequest {
    pub player: Player,
    /// The server the player is on.
    pub server: String,
    /// The command's **declared name**: when the player typed an alias, this is the name it stands for.
    pub command: String,
    /// What follows the command, in words: at most `decide::MAX_COMMAND_ARGS` of them, each cut to
    /// `decide::MAX_ARG_CHARS`. They are what the player typed: not to be trusted.
    pub args: Vec<String>,
}

/// `params` of [`RUN`]: the request is the one parameter, named `request` as `rpc.discover` lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunParams {
    pub request: RunRequest,
}

/// What a command says to the player who typed it: at most [`MAX_LINES`] lines of at most
/// [`MAX_LINE_CHARS`] characters, plain text. The proxy shows each as a literal, never as a JSON text
/// component, so an extension cannot put a click event in a player's chat. [`Lines::new`] makes lines that
/// pass the proxy's check ([`check_line`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lines(Vec<String>);

impl Lines {
    /// These lines, made valid: a control character is dropped from a line (a line break ends nothing: give
    /// another line), a line is cut to [`MAX_LINE_CHARS`] characters, and the lines past [`MAX_LINES`] are
    /// left out. What the extension means is kept; what the proxy would refuse is not.
    pub fn new<I, S>(lines: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self(
            lines
                .into_iter()
                .take(MAX_LINES)
                .map(|line| {
                    line.as_ref()
                        .chars()
                        .filter(|c| !c.is_control())
                        .take(MAX_LINE_CHARS)
                        .collect()
                })
                .collect(),
        )
    }

    /// One line.
    pub fn one(line: &str) -> Self {
        Self::new([line])
    }

    /// Nothing to say: the command ran and the player reads no line.
    pub fn none() -> Self {
        Self::default()
    }

    pub fn as_slice(&self) -> &[String] {
        &self.0
    }
}

/// Whether `text` is a valid line, as the proxy checks the ones an extension sent. The error says what is
/// wrong in a few words and never quotes the text.
pub fn check_line(text: &str) -> Result<(), String> {
    if text.chars().count() > MAX_LINE_CHARS {
        return Err(format!("a line is longer than {MAX_LINE_CHARS} characters"));
    }
    if text.chars().any(char::is_control) {
        return Err("a line holds a control character".into());
    }
    Ok(())
}

/// `result` of [`RUN`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunAnswer {
    pub lines: Vec<String>,
}

impl From<&Lines> for RunAnswer {
    fn from(lines: &Lines) -> Self {
        Self {
            lines: lines.0.clone(),
        }
    }
}

/// What the proxy makes of the `result` of [`RUN`]: the lines to show, or why the answer is not usable
/// (docs/EXTENSIONS.md, 4.1: an invalid answer is logged with the method and the field, never whole). A field
/// the proxy does not know is ignored.
pub fn parse_run_answer(result: &Value) -> Result<Vec<String>, String> {
    let answer: RunAnswer = wire::decode(Some(result))?;
    if answer.lines.len() > MAX_LINES {
        return Err(format!("the answer has more than {MAX_LINES} lines"));
    }
    for line in &answer.lines {
        check_line(line)?;
    }
    Ok(answer.lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decide::Auth;
    use crate::wire::Message;
    use serde_json::json;

    fn golden(name: &str) -> String {
        let path = format!("{}/golden/protocol-0/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
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

    fn party() -> RunRequest {
        RunRequest {
            player: Player {
                name: "Alex".into(),
                uuid: "0a2ea4ba-bced-39d2-80e1-1ad51a0e925c".into(),
                auth: Auth::Verified,
            },
            server: "lobby".into(),
            command: "party".into(),
            args: vec!["invite".into(), "Bob".into()],
        }
    }

    #[test]
    fn the_run_request_is_what_the_golden_file_says() {
        let Message::Request { method, params, .. } =
            Message::parse(&golden("run.request.json")).unwrap()
        else {
            panic!("a request");
        };
        assert_eq!(method, RUN);
        let read: RunParams = wire::decode(params.as_ref()).unwrap();
        assert_eq!(read.request, party());
        // And what is written is what the file holds.
        let written = serde_json::to_value(RunParams { request: party() }).unwrap();
        assert_eq!(Some(&written), params.as_ref());
    }

    #[test]
    fn the_answers_are_what_the_golden_files_say() {
        let lines = parse_run_answer(&golden_result("run.answer.json")).unwrap();
        assert_eq!(
            lines,
            ["Bob is invited to your party.", "Your party: Alex, Bob."]
        );
        assert_eq!(
            serde_json::to_value(RunAnswer::from(&Lines::new(&lines))).unwrap(),
            golden_result("run.answer.json")
        );
        assert_eq!(
            parse_run_answer(&golden_result("run.answer.empty.json")).unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(
            serde_json::to_value(RunAnswer::from(&Lines::none())).unwrap(),
            golden_result("run.answer.empty.json")
        );
    }

    #[test]
    fn lines_that_are_made_always_pass_the_check() {
        let long = "x".repeat(MAX_LINE_CHARS + 50);
        let many: Vec<String> = (0..MAX_LINES + 5).map(|i| format!("line {i}")).collect();
        let made = Lines::new([long.as_str(), "a\r\nb\u{1b}[31mc\0", "caf\u{e9}"]);
        assert_eq!(made.as_slice()[0].chars().count(), MAX_LINE_CHARS);
        assert_eq!(made.as_slice()[1], "ab[31mc");
        assert_eq!(made.as_slice()[2], "caf\u{e9}");
        for lines in [made, Lines::new(&many), Lines::one("hello"), Lines::none()] {
            assert!(lines.as_slice().len() <= MAX_LINES);
            for line in lines.as_slice() {
                check_line(line).unwrap();
            }
        }
        assert_eq!(Lines::new(&many).as_slice().len(), MAX_LINES);
        assert_eq!(Lines::new(&many).as_slice()[MAX_LINES - 1], "line 9");
    }

    #[test]
    fn an_answer_the_proxy_cannot_use_is_refused_without_quoting_it() {
        let eleven: Vec<String> = (0..=MAX_LINES).map(|i| format!("Secret {i}")).collect();
        let long = "Secret".repeat(100);
        for answer in [
            json!({"lines": eleven}),
            json!({"lines": [long]}),
            json!({"lines": ["Secret\u{1b}[31m"]}),
            json!({"lines": ["fine", "Secret\nline break"]}),
            json!({"lines": "Secret"}),
            json!({"lines": [7]}),
            json!({"lines": null}),
            // A missing field is not an empty answer.
            json!({}),
            json!(["Secret"]),
            json!("Secret"),
            Value::Null,
        ] {
            let why = parse_run_answer(&answer).unwrap_err();
            assert!(!why.contains("Secret"), "{answer}: {why}");
        }
    }

    #[test]
    fn a_field_nobody_knows_is_ignored_and_the_limits_are_inclusive() {
        let answer = json!({"lines": ["ok"], "later": {"x": 1}});
        assert_eq!(parse_run_answer(&answer).unwrap(), ["ok"]);
        let edge: Vec<String> = vec!["y".repeat(MAX_LINE_CHARS); MAX_LINES];
        assert_eq!(
            parse_run_answer(&json!({ "lines": edge })).unwrap().len(),
            MAX_LINES
        );
        // The request too: a field a later proxy adds is not an error for the extension.
        let mut request = serde_json::to_value(RunParams { request: party() }).unwrap();
        request["request"]["later"] = json!(true);
        let read: RunParams = wire::decode(Some(&request)).unwrap();
        assert_eq!(read.request, party());
    }
}
