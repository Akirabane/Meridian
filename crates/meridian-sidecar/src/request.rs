//! The player requests of a switch, as the proxy asks them, and the line each one
//! becomes on the backend's control endpoint.
//!
//! The lines are those of Horizon's held-player work (its `docs/server.md`,
//! "Players arriving from another server"):
//!
//! | Request | Line |
//! | --- | --- |
//! | expect | `player-expect <uuid> [seconds]` |
//! | export | `player-export <uuid> [freeze]` |
//! | import | `player-import <uuid> <json>`, the JSON `{"record": ..., "parts": [...], "announce": bool}` |
//! | release | `player-release <uuid> moved [announce]` or `player-release <uuid> abort` |
//! | drop | `player-drop <uuid>` |
//!
//! A player record is carried as it is: the sidecar never reads it.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The longest a player can be expected for, in seconds, as Horizon allows it.
pub const MAX_EXPECT_SECONDS: u64 = 60;

/// One player request. `uuid` is 32 hexadecimal digits, with or without dashes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Request {
    /// The player will arrive through the proxy within `seconds` (Horizon's
    /// default, 10, when left out; 60 at most): hold them when they log in.
    Expect {
        uuid: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seconds: Option<u64>,
    },
    /// The player's record, to move to another backend. With `freeze`, the
    /// player's own inputs stop being acted on until a release.
    Export {
        uuid: String,
        #[serde(default, skip_serializing_if = "is_false")]
        freeze: bool,
    },
    /// Put a record on the held player and let them into the world: every part
    /// of it, or only `parts`; the others are told the player came unless
    /// `announce` is false.
    Import {
        uuid: String,
        record: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parts: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        announce: Option<bool>,
    },
    /// The switch is over: the player has `Moved` away for good, or the switch
    /// was abandoned (`Abort`) and the player stays. `announce` (with `Moved`
    /// only) sends the others the "left the game" line.
    Release {
        uuid: String,
        outcome: Outcome,
        #[serde(default, skip_serializing_if = "is_false")]
        announce: bool,
    },
    /// Forget an expected or held player.
    Drop { uuid: String },
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// How a switch ended, for [`Request::Release`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Moved,
    Abort,
}

/// A unique id as Horizon writes it in an answer, lowercase with dashes
/// (`123e4567-e89b-12d3-a456-426614174000`), from 32 hexadecimal digits with or
/// without dashes. `None` for anything else.
pub fn normalize_uuid(text: &str) -> Option<String> {
    let plain: String = text.chars().filter(|c| *c != '-').collect();
    if plain.len() != 32 || !plain.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let plain = plain.to_ascii_lowercase();
    Some(format!(
        "{}-{}-{}-{}-{}",
        &plain[..8],
        &plain[8..12],
        &plain[12..16],
        &plain[16..20],
        &plain[20..]
    ))
}

impl Request {
    fn uuid(&self) -> &str {
        match self {
            Request::Expect { uuid, .. }
            | Request::Export { uuid, .. }
            | Request::Import { uuid, .. }
            | Request::Release { uuid, .. }
            | Request::Drop { uuid } => uuid,
        }
    }

    /// The command line for the backend's control endpoint, or why the request
    /// cannot be one. What is checked here is what the line's form needs; the
    /// backend checks the rest, such as the record and the names of the parts.
    pub fn endpoint_line(&self) -> Result<String, String> {
        let uuid = normalize_uuid(self.uuid()).ok_or_else(|| {
            format!(
                "{:?} is not a unique id: 32 hexadecimal digits, with or without dashes",
                self.uuid()
            )
        })?;
        Ok(match self {
            Request::Expect { seconds: None, .. } => format!("player-expect {uuid}"),
            Request::Expect {
                seconds: Some(seconds),
                ..
            } => {
                if *seconds == 0 || *seconds > MAX_EXPECT_SECONDS {
                    return Err(format!(
                        "expect takes from 1 to {MAX_EXPECT_SECONDS} seconds, not {seconds}"
                    ));
                }
                format!("player-expect {uuid} {seconds}")
            }
            Request::Export { freeze: false, .. } => format!("player-export {uuid}"),
            Request::Export { freeze: true, .. } => format!("player-export {uuid} freeze"),
            Request::Import {
                record,
                parts,
                announce,
                ..
            } => {
                if !record.is_object() {
                    return Err("a record is a JSON object, as player-export gives it".into());
                }
                let mut body = Map::new();
                body.insert("record".into(), record.clone());
                if let Some(parts) = parts {
                    body.insert("parts".into(), Value::from(parts.clone()));
                }
                if let Some(announce) = announce {
                    body.insert("announce".into(), Value::Bool(*announce));
                }
                // Compact JSON has no line break, so the request stays one line.
                format!("player-import {uuid} {}", Value::Object(body))
            }
            Request::Release {
                outcome: Outcome::Moved,
                announce: false,
                ..
            } => format!("player-release {uuid} moved"),
            Request::Release {
                outcome: Outcome::Moved,
                announce: true,
                ..
            } => format!("player-release {uuid} moved announce"),
            Request::Release {
                outcome: Outcome::Abort,
                announce: false,
                ..
            } => format!("player-release {uuid} abort"),
            Request::Release {
                outcome: Outcome::Abort,
                announce: true,
                ..
            } => {
                return Err(
                    "announce goes with moved only: an aborted switch keeps the player".into(),
                );
            }
            Request::Drop { .. } => format!("player-drop {uuid}"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const PLAIN: &str = "123E4567E89B12D3A456426614174000";
    const DASHED: &str = "123e4567-e89b-12d3-a456-426614174000";

    fn line(request: Request) -> String {
        request.endpoint_line().unwrap()
    }

    #[test]
    fn a_unique_id_is_written_lowercase_with_dashes() {
        assert_eq!(normalize_uuid(PLAIN).as_deref(), Some(DASHED));
        assert_eq!(normalize_uuid(DASHED).as_deref(), Some(DASHED));
        assert_eq!(
            normalize_uuid("123E4567-E89B-12D3-A456-426614174000").as_deref(),
            Some(DASHED)
        );
        for bad in [
            "",
            "123e4567",
            "123e4567-e89b-12d3-a456-42661417400g",
            &format!("{PLAIN}0"),
        ] {
            assert_eq!(normalize_uuid(bad), None, "{bad}");
        }
    }

    #[test]
    fn requests_read_and_write_with_their_op() {
        let request = Request::Release {
            uuid: DASHED.into(),
            outcome: Outcome::Moved,
            announce: true,
        };
        let text = serde_json::to_string(&request).unwrap();
        assert_eq!(
            text,
            format!(r#"{{"op":"release","uuid":"{DASHED}","outcome":"moved","announce":true}}"#)
        );
        assert_eq!(serde_json::from_str::<Request>(&text).unwrap(), request);
        // Options left out take their defaults.
        assert_eq!(
            serde_json::from_str::<Request>(&format!(r#"{{"op":"export","uuid":"{DASHED}"}}"#))
                .unwrap(),
            Request::Export {
                uuid: DASHED.into(),
                freeze: false
            }
        );
        for bad in [
            r#"{"op":"teleport"}"#.to_owned(),
            format!(r#"{{"op":"drop","uuid":"{DASHED}","command":"stop"}}"#),
        ] {
            assert!(serde_json::from_str::<Request>(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn expect_is_its_unique_id_and_maybe_a_time() {
        assert_eq!(
            line(Request::Expect {
                uuid: PLAIN.into(),
                seconds: None
            }),
            format!("player-expect {DASHED}")
        );
        assert_eq!(
            line(Request::Expect {
                uuid: PLAIN.into(),
                seconds: Some(30)
            }),
            format!("player-expect {DASHED} 30")
        );
        for seconds in [0, 61] {
            let e = Request::Expect {
                uuid: PLAIN.into(),
                seconds: Some(seconds),
            }
            .endpoint_line()
            .unwrap_err();
            assert!(e.contains("from 1 to 60"), "{e}");
        }
    }

    #[test]
    fn export_may_freeze() {
        assert_eq!(
            line(Request::Export {
                uuid: DASHED.into(),
                freeze: false
            }),
            format!("player-export {DASHED}")
        );
        assert_eq!(
            line(Request::Export {
                uuid: DASHED.into(),
                freeze: true
            }),
            format!("player-export {DASHED} freeze")
        );
    }

    #[test]
    fn import_carries_the_record_as_it_is_on_one_line() {
        let record = json!({
            "version": 1,
            "data_version": 5023,
            "uuid": DASHED,
            "name": "Alex",
            "player": "AAEC",
            "advancements": { "minecraft:story/root": { "done": true } },
            "a key the sidecar does not know": [1, 2, 3],
        });
        let all = line(Request::Import {
            uuid: PLAIN.into(),
            record: record.clone(),
            parts: None,
            announce: None,
        });
        let (head, body) = all.split_at(format!("player-import {DASHED} ").len());
        assert_eq!(head, format!("player-import {DASHED} "));
        assert!(!all.contains('\n'));
        let body: Value = serde_json::from_str(body).unwrap();
        assert_eq!(body, json!({ "record": record }));

        let some = line(Request::Import {
            uuid: PLAIN.into(),
            record: record.clone(),
            parts: Some(vec!["inventory".into(), "experience".into()]),
            announce: Some(false),
        });
        let body: Value = serde_json::from_str(some.splitn(3, ' ').nth(2).unwrap()).unwrap();
        assert_eq!(
            body,
            json!({ "record": record, "parts": ["inventory", "experience"], "announce": false })
        );

        let e = Request::Import {
            uuid: PLAIN.into(),
            record: json!("not an object"),
            parts: None,
            announce: None,
        }
        .endpoint_line()
        .unwrap_err();
        assert!(e.contains("JSON object"), "{e}");
    }

    #[test]
    fn release_says_moved_or_abort_and_announce_goes_with_moved() {
        let release = |outcome, announce| Request::Release {
            uuid: DASHED.into(),
            outcome,
            announce,
        };
        assert_eq!(
            line(release(Outcome::Moved, false)),
            format!("player-release {DASHED} moved")
        );
        assert_eq!(
            line(release(Outcome::Moved, true)),
            format!("player-release {DASHED} moved announce")
        );
        assert_eq!(
            line(release(Outcome::Abort, false)),
            format!("player-release {DASHED} abort")
        );
        assert!(release(Outcome::Abort, true).endpoint_line().is_err());
    }

    #[test]
    fn drop_is_its_unique_id() {
        assert_eq!(
            line(Request::Drop { uuid: PLAIN.into() }),
            format!("player-drop {DASHED}")
        );
    }

    #[test]
    fn a_bad_unique_id_never_becomes_a_line() {
        for uuid in ["Alex", "", "123e4567-e89b-12d3-a456-4266141740 0", "x stop"] {
            let e = Request::Drop { uuid: uuid.into() }
                .endpoint_line()
                .unwrap_err();
            assert!(e.contains("is not a unique id"), "{uuid}: {e}");
        }
    }
}
