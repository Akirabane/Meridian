//! The player requests of a switch, as the proxy asks them, and the line each one
//! becomes on the backend's control endpoint.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// One player request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum Request {
    /// The player will arrive through the proxy: hold them when they log in.
    Expect { uuid: String, name: String },
    /// The player's record, to move to another backend.
    Export { uuid: String },
    /// Put a record on the held player.
    Import { uuid: String, record: Value },
    /// The switch is over: the player has `Moved` away for good, or the switch
    /// was abandoned (`Abort`) and the player stays.
    Release { uuid: String, outcome: Outcome },
    /// Forget an expected or held player.
    Drop { uuid: String },
}

/// How a switch ended, for [`Request::Release`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Moved,
    Abort,
}

impl Request {
    /// The command line for the backend's control endpoint: the command's name,
    /// a space, then its arguments as one JSON object.
    ///
    /// The exact form belongs to Horizon's held-player work, which has not
    /// published it yet; this is the one place to change when it does.
    pub fn endpoint_line(&self) -> String {
        let (command, args) = match self {
            Request::Expect { uuid, name } => {
                ("player-expect", json!({ "uuid": uuid, "name": name }))
            }
            Request::Export { uuid } => ("player-export", json!({ "uuid": uuid })),
            Request::Import { uuid, record } => {
                ("player-import", json!({ "uuid": uuid, "record": record }))
            }
            Request::Release { uuid, outcome } => (
                "player-release",
                json!({ "uuid": uuid, "outcome": outcome }),
            ),
            Request::Drop { uuid } => ("player-drop", json!({ "uuid": uuid })),
        };
        format!("{command} {args}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UUID: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn requests_read_and_write_with_their_op() {
        let request = Request::Release {
            uuid: UUID.into(),
            outcome: Outcome::Moved,
        };
        let text = serde_json::to_string(&request).unwrap();
        assert_eq!(
            text,
            format!(r#"{{"op":"release","uuid":"{UUID}","outcome":"moved"}}"#)
        );
        assert_eq!(serde_json::from_str::<Request>(&text).unwrap(), request);
        assert!(serde_json::from_str::<Request>(r#"{"op":"teleport"}"#).is_err());
    }

    #[test]
    fn each_request_has_its_endpoint_command() {
        let lines = [
            Request::Expect {
                uuid: UUID.into(),
                name: "Alex".into(),
            },
            Request::Export { uuid: UUID.into() },
            Request::Import {
                uuid: UUID.into(),
                record: json!({ "health": 20 }),
            },
            Request::Release {
                uuid: UUID.into(),
                outcome: Outcome::Abort,
            },
            Request::Drop { uuid: UUID.into() },
        ]
        .map(|r| r.endpoint_line());
        let commands: Vec<&str> = lines.iter().map(|l| l.split_once(' ').unwrap().0).collect();
        assert_eq!(
            commands,
            [
                "player-expect",
                "player-export",
                "player-import",
                "player-release",
                "player-drop"
            ]
        );
        let args: Value = serde_json::from_str(lines[3].split_once(' ').unwrap().1).unwrap();
        assert_eq!(args, json!({ "uuid": UUID, "outcome": "abort" }));
    }
}
