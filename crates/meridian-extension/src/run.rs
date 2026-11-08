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
/// component, so an extension cannot put a click event in a player's chat, and a line holds **no formatting
/// code** (`§`) and no character that changes how the text reads or hides in it (see [`is_refused`]), so
/// that it cannot be coloured, made to flicker or made to look like a line of the proxy or of a player.
/// [`Lines::new`] makes lines that pass the proxy's check ([`check_line`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lines(Vec<String>);

impl Lines {
    /// These lines, made valid: the characters the proxy refuses ([`is_refused`]: control characters, `§` and
    /// the marks that change the direction of the text or are invisible) are dropped from a line (a line
    /// break ends nothing: give another line), a line is cut to [`MAX_LINE_CHARS`] characters, and the lines
    /// past [`MAX_LINES`] are left out. What the extension means is kept; what the proxy would refuse is not.
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
                        .filter(|c| !is_refused(*c))
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

/// Whether a character may not be in a line: a control character; `§`, which the client reads as a
/// formatting code (colour, bold, flicker) in any text; the marks that set or change the direction of the
/// text (U+061C, U+200E, U+200F, U+202A to U+202E, U+2066 to U+2069, U+206A to U+206F) and the separators of
/// lines and paragraphs (U+2028, U+2029), which could make a line read differently than it is; and the
/// characters that show nothing or change nothing a reader sees, which hide text or stand in for a space:
/// the soft hyphen (U+00AD), the combining grapheme joiner (U+034F), the Hangul fillers (U+115F, U+1160,
/// U+3164, U+FFA0), the characters of no width (U+200B to U+200D, U+2060 to U+2064, U+FEFF), the variation
/// selectors (U+FE00 to U+FE0F), the interlinear annotation marks (U+FFF9 to U+FFFB) and the tag characters
/// (U+E0000 to U+E007F). **This is a prudent default**: an extension that is meant to colour its lines, or
/// to write an emoji that a variation selector turns into its picture, would need a field of its own, added
/// later, that the operator allows.
pub fn is_refused(c: char) -> bool {
    c.is_control()
        || c == '\u{a7}'
        || matches!(
            c,
            '\u{ad}'
                | '\u{34f}'
                | '\u{61c}'
                | '\u{115f}'
                | '\u{1160}'
                | '\u{200b}'..='\u{200f}'
                | '\u{2028}'..='\u{202e}'
                | '\u{2060}'..='\u{206f}'
                | '\u{3164}'
                | '\u{fe00}'..='\u{fe0f}'
                | '\u{feff}'
                | '\u{ffa0}'
                | '\u{fff9}'..='\u{fffb}'
                | '\u{e0000}'..='\u{e007f}'
        )
}

/// Whether `text` is a valid line, as the proxy checks the ones an extension sent. The error says what is
/// wrong in a few words and never quotes the text.
pub fn check_line(text: &str) -> Result<(), String> {
    if text.chars().count() > MAX_LINE_CHARS {
        return Err(format!("a line is longer than {MAX_LINE_CHARS} characters"));
    }
    if text.chars().any(is_refused) {
        return Err("a line holds a control, formatting or invisible character".into());
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

    /// A formatting code, a mark that changes the direction of the text and a character of no width are
    /// not shown to the player: they are dropped from what is made, and refused in what is read.
    #[test]
    fn no_formatting_code_and_no_hidden_or_reversing_character_reaches_the_player() {
        let made = Lines::new([
            "\u{a7}4Server\u{a7}r says \u{a7}kxx",
            "x\u{202e}evil\u{2066}y\u{200f}z",
            "a\u{200b}b\u{200c}c\u{200d}d\u{2060}e\u{feff}f\u{2028}g\u{2029}h",
            "a\u{ad}b\u{34f}c\u{61c}d\u{115f}e\u{1160}f\u{3164}g\u{ffa0}h",
            "a\u{206a}b\u{206f}c\u{fe0f}d\u{fe00}e\u{fff9}f\u{fffb}g\u{e0041}h\u{e007f}i",
            "caf\u{e9} \u{2014} \u{65e5}\u{672c}",
        ]);
        assert_eq!(
            made.as_slice(),
            [
                "4Serverr says kxx",
                "xevilyz",
                "abcdefgh",
                "abcdefgh",
                "abcdefghi",
                "caf\u{e9} \u{2014} \u{65e5}\u{672c}"
            ]
        );
        for line in made.as_slice() {
            check_line(line).unwrap();
        }
        for bad in [
            "\u{a7}c red",
            "x\u{202e}y",
            "x\u{2066}y",
            "x\u{200e}y",
            "x\u{200b}y",
            "x\u{2060}y",
            "x\u{feff}y",
            "x\u{2028}y",
            "x\u{85}y",
            "x\u{ad}y",
            "x\u{34f}y",
            "x\u{61c}y",
            "x\u{115f}y",
            "x\u{1160}y",
            "x\u{3164}y",
            "x\u{ffa0}y",
            "x\u{206a}y",
            "x\u{206f}y",
            "x\u{fe0f}y",
            "x\u{fe00}y",
            "x\u{fff9}y",
            "x\u{fffb}y",
            "x\u{e0041}y",
            "x\u{e0000}y",
            "x\u{e007f}y",
            "x\u{7f}y",
        ] {
            assert!(check_line(bad).is_err(), "{bad:?} was shown");
            assert!(
                !parse_run_answer(&json!({ "lines": [bad] })).is_ok(),
                "{bad:?}"
            );
        }
        // Text of other scripts and the usual marks of writing pass.
        for good in [
            "caf\u{e9}",
            "\u{65e5}\u{672c}\u{8a9e}",
            "\u{2014} \u{2026} \u{201c}ok\u{201d}",
            "100%",
        ] {
            assert!(check_line(good).is_ok(), "{good:?}");
        }
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
