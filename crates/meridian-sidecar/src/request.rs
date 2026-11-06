//! The player requests of a switch, as the proxy asks them, and the line each one
//! becomes on the backend's control endpoint.
//!
//! The lines are those of Horizon's player transfers (its `docs/server.md`,
//! "A proxy moving a player between two servers"):
//!
//! | Request | Line |
//! | --- | --- |
//! | expect | `player-expect <uuid> [seconds]` |
//! | export | `player-export <uuid> [freeze]` |
//! | import | `player-import <uuid> <json>`, the JSON `{"record": ..., "parts": [...], "announce": bool, "place": bool}`, and with an [`Arrival`] its `position`, `rotation`, `velocity` and `on_ground` |
//! | confirm | `player-confirm <uuid> <handover>` |
//! | release | `player-release <uuid> <handover> moved [announce]` or `player-release <uuid> <handover> abort` |
//! | drop | `player-drop <uuid> [handover]` |
//! | status | `transfer-status` (see [`STATUS_LINE`]) |
//! | notices | none: the sidecar answers it from the notices of the backend's management protocol it heard |
//!
//! `<handover>` is the id that a `player-export` with `freeze` puts in its record
//! (32 hexadecimal digits). The import names it through the record it carries; the
//! confirm, the release and, once the record was imported, the drop name it themselves.
//!
//! A player record is carried as it is: the sidecar never reads it.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The longest a player can be expected for, in seconds, as Horizon allows it.
pub const MAX_EXPECT_SECONDS: u64 = 60;

/// The longest command line Horizon's control endpoint takes, line break included:
/// 16 MiB. A longer one is refused and the connection closed.
pub const MAX_LINE: usize = 16 << 20;

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
    /// `announce` is false. The record is the one a [`Request::Export`] with `freeze`
    /// gave, with its `handover` id: the target refuses one without or with another's.
    /// With `place` false, the player stays held after the record is applied, until a
    /// [`Request::Confirm`] places them or a [`Request::Drop`] cancels the arrival.
    Import {
        uuid: String,
        record: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parts: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        announce: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        place: Option<bool>,
        /// Where the player arrives, instead of the record's place: with the
        /// `position` part only.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        arrival: Option<Arrival>,
    },
    /// Place a held player whose record was imported with `place` false. The
    /// `handover` is the id of the record that was imported.
    Confirm { uuid: String, handover: String },
    /// The switch is over: the player has `Moved` away for good, or the switch
    /// was abandoned (`Abort`) and the player stays. `announce` (with `Moved`
    /// only) sends the others the "left the game" line. The `handover` is the id the
    /// export with `freeze` gave; the source refuses `Moved` once the freeze has
    /// run out.
    Release {
        uuid: String,
        handover: String,
        outcome: Outcome,
        #[serde(default, skip_serializing_if = "is_false")]
        announce: bool,
    },
    /// Forget an expected or held player. With a `handover`, only the arrival of
    /// that record: it must be the imported record's, so that a late drop leaves a
    /// newer arrival alone. Before any import there is no id to name.
    Drop {
        uuid: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        handover: Option<String>,
    },
    /// The state of the backend: how many players it has, how many it can take,
    /// and whether it accepts transfers. Read only, and it names no player.
    // A struct variant, so that a field it does not have is refused like for the others.
    Status {},
    /// The notices of the backend's management protocol (`server/stopping`,
    /// `server/started`), which the sidecar hears itself: the request never reaches the
    /// backend's control endpoint. Without `after` the answer holds none and only says
    /// where the count stands; with it, the notices numbered above it, after waiting up
    /// to `wait_ms` for one if there are none.
    Notices {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wait_ms: Option<u64>,
    },
    /// The changes of the server's allowlist, bans and ip bans that the sidecar heard on the
    /// management protocol, numbered apart from the [`Request::Notices`]: same use of
    /// `after` and `wait_ms`. The answer also tells how many connections to the server there
    /// have been, which is how the proxy knows to read the lists again. The request never
    /// reaches the backend's control endpoint.
    AccessNotices {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wait_ms: Option<u64>,
    },
    /// A call of the server's management protocol on its lists: `method` is one of
    /// [`crate::lists::CALLABLE`] without `minecraft:` (`bans/add`, `allowlist`, ...) and
    /// `params` the parameters by name (`{"add": [...]}`). The answer is the result the
    /// server gave. The request never reaches the backend's control endpoint.
    Manage {
        method: String,
        #[serde(default)]
        params: Value,
    },
}

/// The line a [`Request::Status`] becomes on the backend's control endpoint: Horizon's
/// `transfer-status` (its `docs/server.md`), a read that names no player and changes
/// nothing. A Horizon build from before it answers `unknown request`, and the proxy then
/// goes on as if the server accepted players.
pub const STATUS_LINE: &str = "transfer-status";

/// What a backend reports of itself, as the proxy reads it from the answer to
/// [`Request::Status`]. A field the backend leaves out is unknown, not zero or false,
/// and any other field of the answer is ignored.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ServerStatus {
    /// The players on the backend now. From Horizon's `online`, which counts the placed
    /// players, the held ones and the logins still joining, as the player limit does;
    /// `players` when the answer has no `online`.
    pub players: Option<u64>,
    /// The most players it can take: Horizon's `max_players`, or `capacity`.
    pub capacity: Option<u64>,
    /// Whether it takes players sent by the proxy (`accepts_transfers`). Horizon's
    /// answer to `transfer-status` has no such field: a server that answers it has
    /// transfers on, and one that has them off answers an error instead, which
    /// [`crate::client::SidecarClient::status`] reads as `Some(false)`.
    pub accepts_transfers: Option<bool>,
}

impl ServerStatus {
    /// Reads the answer of the backend: a JSON object, whose `players` (or `online`),
    /// `capacity` (or `max_players`) are whole numbers and `accepts_transfers` a boolean
    /// when they are there. `full` and the other fields of Horizon's answer are not
    /// read: a server is full when `online` reaches `max_players`, which the caps say.
    pub fn from_value(answer: &Value) -> Result<ServerStatus, String> {
        let Value::Object(map) = answer else {
            return Err(format!("the status is not a JSON object: {answer}"));
        };
        let number = |key: &str| match map.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => value
                .as_u64()
                .map(Some)
                .ok_or_else(|| format!("{key} is not a whole number: {value}")),
        };
        let accepts_transfers = match map.get("accepts_transfers") {
            None | Some(Value::Null) => None,
            Some(Value::Bool(b)) => Some(*b),
            Some(other) => {
                return Err(format!("accepts_transfers is not true or false: {other}"));
            }
        };
        // Horizon's `held`, `expected` and `frozen` are numbers too, but are not read.
        let horizon = map.contains_key("max_players");
        Ok(ServerStatus {
            players: match number("online")? {
                Some(online) => Some(online),
                None => number("players")?,
            },
            capacity: match number("max_players")? {
                Some(max) => Some(max),
                None => number("capacity")?,
            },
            accepts_transfers: accepts_transfers.or(horizon.then_some(true)),
        })
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// Where an imported player arrives, given in the import request itself: the
/// target reads only the record's player compound, never the fields above it,
/// and these take the place of the compound's. Horizon takes them with the
/// `position` part only (`docs/server.md`, "Where the player arrives").
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Arrival {
    /// In blocks.
    pub position: [f64; 3],
    /// Yaw and pitch, in degrees; Horizon cuts the pitch to ±90.
    pub rotation: [f32; 2],
    /// In blocks per tick.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub velocity: Option<[f64; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_ground: Option<bool>,
}

/// The farthest an arrival may be from the origin, east-west and north-south. Horizon takes
/// 30,000,512 (the record's own limit); the world border's 30,000,000 is kept here.
pub const MAX_COORDINATE: f64 = 30_000_000.0;
/// The highest or lowest an arrival may be, as Horizon checks it (20,000,000 up or down).
pub const MAX_HEIGHT: f64 = 20_000_000.0;
/// The fastest a velocity component may be, in blocks per tick. This only keeps absurd numbers
/// out. Horizon itself takes a component over 10 as 0, without an error, so a fast player
/// (an explosion, an elytra, a knockback) arrives still instead of not arriving: a velocity
/// between 10 and this is sent, not refused.
pub const MAX_VELOCITY: f64 = 100.0;

impl Arrival {
    /// Why Horizon would refuse it, before it is sent.
    fn check(&self) -> Result<(), String> {
        let [x, y, z] = self.position;
        if !(x.is_finite() && y.is_finite() && z.is_finite())
            || x.abs() > MAX_COORDINATE
            || z.abs() > MAX_COORDINATE
            || y.abs() > MAX_HEIGHT
        {
            return Err(format!(
                "an arrival position is finite, within {MAX_COORDINATE} blocks of the origin \
                 east-west and north-south and within {MAX_HEIGHT} up or down: {:?}",
                self.position
            ));
        }
        if !self.rotation.iter().all(|v| v.is_finite()) {
            return Err(format!(
                "an arrival rotation is finite: {:?}",
                self.rotation
            ));
        }
        if let Some(velocity) = self.velocity
            && !velocity
                .iter()
                .all(|v| v.is_finite() && v.abs() <= MAX_VELOCITY)
        {
            return Err(format!(
                "an arrival velocity is finite and at most {MAX_VELOCITY} blocks per tick: \
                 {velocity:?}"
            ));
        }
        Ok(())
    }
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

/// A hand-over id as Horizon writes it, 32 lowercase hexadecimal digits, from 32
/// hexadecimal digits of either case. `None` for anything else.
pub fn normalize_handover(text: &str) -> Option<String> {
    (text.len() == 32 && text.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| text.to_ascii_lowercase())
}

/// The hand-over id of a record, as an export with `freeze` puts it in. `None` for
/// a record without one (an export without `freeze` has `null`) or with one that is
/// not an id.
pub fn record_handover(record: &Value) -> Option<String> {
    normalize_handover(record.get("handover")?.as_str()?)
}

impl Request {
    /// The hand-over the request names, as a line writes it.
    fn handover(&self) -> Result<Option<String>, String> {
        let named = match self {
            Request::Confirm { handover, .. } | Request::Release { handover, .. } => handover,
            Request::Drop {
                handover: Some(handover),
                ..
            } => handover,
            _ => return Ok(None),
        };
        normalize_handover(named).map(Some).ok_or_else(|| {
            format!("{named:?} is not a hand-over id: 32 hexadecimal digits, as an export gives it")
        })
    }

    /// The player the request is about; `None` for a request that names no one.
    fn uuid(&self) -> Option<&str> {
        match self {
            Request::Expect { uuid, .. }
            | Request::Export { uuid, .. }
            | Request::Import { uuid, .. }
            | Request::Release { uuid, .. }
            | Request::Confirm { uuid, .. }
            | Request::Drop { uuid, .. } => Some(uuid),
            Request::Status {}
            | Request::Notices { .. }
            | Request::AccessNotices { .. }
            | Request::Manage { .. } => None,
        }
    }

    /// The command line for the backend's control endpoint, or why the request
    /// cannot be one. What is checked here is what the line's form needs; the
    /// backend checks the rest, such as the record and the names of the parts.
    pub fn endpoint_line(&self) -> Result<String, String> {
        let line = self.line()?;
        // The endpoint reads the line and its break in 16 MiB at most; it answers a
        // longer one with an error and closes the connection.
        if line.len() + 1 > MAX_LINE {
            return Err(format!(
                "the request is longer than the {} MiB a backend takes",
                MAX_LINE >> 20
            ));
        }
        Ok(line)
    }

    fn line(&self) -> Result<String, String> {
        let Some(named) = self.uuid() else {
            return Ok(STATUS_LINE.to_owned());
        };
        let uuid = normalize_uuid(named).ok_or_else(|| {
            format!("{named:?} is not a unique id: 32 hexadecimal digits, with or without dashes")
        })?;
        let handover = self.handover()?.unwrap_or_default();
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
                place,
                arrival,
                ..
            } => {
                if !record.is_object() {
                    return Err("a record is a JSON object, as player-export gives it".into());
                }
                // The target takes a record only with the hand-over id of an export
                // with freeze, and no other.
                if record_handover(record).is_none() {
                    return Err(
                        "the record has no hand-over id: export it with freeze, and import it as \
                         it was given"
                            .into(),
                    );
                }
                if let Some(arrival) = arrival {
                    arrival.check()?;
                    if parts
                        .as_ref()
                        .is_some_and(|parts| !parts.iter().any(|p| p == "position"))
                    {
                        return Err("an arrival goes with the position part".into());
                    }
                }
                let mut body = Map::new();
                body.insert("record".into(), record.clone());
                if let Some(parts) = parts {
                    body.insert("parts".into(), Value::from(parts.clone()));
                }
                if let Some(announce) = announce {
                    body.insert("announce".into(), Value::Bool(*announce));
                }
                if let Some(place) = place {
                    body.insert("place".into(), Value::Bool(*place));
                }
                if let Some(arrival) = arrival {
                    body.insert("position".into(), Value::from(arrival.position.to_vec()));
                    body.insert("rotation".into(), Value::from(arrival.rotation.to_vec()));
                    if let Some(velocity) = arrival.velocity {
                        body.insert("velocity".into(), Value::from(velocity.to_vec()));
                    }
                    if let Some(on_ground) = arrival.on_ground {
                        body.insert("on_ground".into(), Value::Bool(on_ground));
                    }
                }
                // Compact JSON has no line break, so the request stays one line.
                format!("player-import {uuid} {}", Value::Object(body))
            }
            Request::Release {
                outcome: Outcome::Moved,
                announce: false,
                ..
            } => format!("player-release {uuid} {handover} moved"),
            Request::Release {
                outcome: Outcome::Moved,
                announce: true,
                ..
            } => format!("player-release {uuid} {handover} moved announce"),
            Request::Release {
                outcome: Outcome::Abort,
                announce: false,
                ..
            } => format!("player-release {uuid} {handover} abort"),
            Request::Release {
                outcome: Outcome::Abort,
                announce: true,
                ..
            } => {
                return Err(
                    "announce goes with moved only: an aborted switch keeps the player".into(),
                );
            }
            Request::Confirm { .. } => format!("player-confirm {uuid} {handover}"),
            Request::Drop { handover: None, .. } => format!("player-drop {uuid}"),
            Request::Drop { .. } => format!("player-drop {uuid} {handover}"),
            Request::Status {} => STATUS_LINE.to_owned(),
            Request::Notices { .. } | Request::AccessNotices { .. } | Request::Manage { .. } => {
                return Err(
                    "notices and management calls are made by the sidecar and never sent to \
                     the backend's control endpoint"
                        .into(),
                );
            }
        })
    }
}

/// One notice of the backend's management protocol, as the sidecar passes it on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoticeHeard {
    /// Its number, from 1 up, in the order the sidecar heard them.
    pub seq: u64,
    /// The JSON-RPC message, as the backend sent it.
    pub line: String,
    /// How long ago the sidecar heard it.
    pub age: std::time::Duration,
}

/// What the sidecar answers to [`Request::Notices`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NoticeReport {
    /// Whether the sidecar listens to the management protocol at all: it does not
    /// unless it was given the backend's `server.properties`.
    pub enabled: bool,
    /// Whether the connection to the backend's management protocol is up now.
    pub watching: bool,
    /// The number of the last notice there has been, 0 if none. It starts again from 0
    /// when the sidecar does.
    pub last: u64,
    pub notices: Vec<NoticeHeard>,
}

impl NoticeReport {
    /// The answer as the sidecar writes it.
    pub fn to_value(&self) -> Value {
        serde_json::json!({
            "enabled": self.enabled,
            "watching": self.watching,
            "last": self.last,
            "notices": self.notices.iter().map(|n| serde_json::json!({
                "seq": n.seq,
                "line": n.line,
                "age_ms": u64::try_from(n.age.as_millis()).unwrap_or(u64::MAX),
            })).collect::<Vec<_>>(),
        })
    }

    /// Reads the answer.
    pub fn from_value(answer: &Value) -> Result<NoticeReport, String> {
        let Value::Object(map) = answer else {
            return Err(format!("the notices are not a JSON object: {answer}"));
        };
        let flag = |key: &str| {
            map.get(key)
                .and_then(Value::as_bool)
                .ok_or_else(|| format!("{key} is not true or false"))
        };
        let last = map
            .get("last")
            .and_then(Value::as_u64)
            .ok_or("last is not a whole number")?;
        let mut notices = Vec::new();
        for item in map
            .get("notices")
            .and_then(Value::as_array)
            .ok_or("notices is not a list")?
        {
            let seq = item.get("seq").and_then(Value::as_u64);
            let line = item.get("line").and_then(Value::as_str);
            let age = item.get("age_ms").and_then(Value::as_u64);
            let (Some(seq), Some(line), Some(age)) = (seq, line, age) else {
                return Err(format!("a notice is not usable: {item}"));
            };
            notices.push(NoticeHeard {
                seq,
                line: line.to_owned(),
                age: std::time::Duration::from_millis(age),
            });
        }
        Ok(NoticeReport {
            enabled: flag("enabled")?,
            watching: flag("watching")?,
            last,
            notices,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const PLAIN: &str = "123E4567E89B12D3A456426614174000";
    const DASHED: &str = "123e4567-e89b-12d3-a456-426614174000";
    const HANDOVER: &str = "9f2c4be0a1d34e6b8c0f1e2d3c4b5a69";

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
            handover: HANDOVER.into(),
            outcome: Outcome::Moved,
            announce: true,
        };
        let text = serde_json::to_string(&request).unwrap();
        assert_eq!(
            text,
            format!(
                r#"{{"op":"release","uuid":"{DASHED}","handover":"{HANDOVER}","outcome":"moved","announce":true}}"#
            )
        );
        assert_eq!(serde_json::from_str::<Request>(&text).unwrap(), request);
        // A drop may leave the hand-over out, a confirm and a release may not.
        assert_eq!(
            serde_json::from_str::<Request>(&format!(r#"{{"op":"drop","uuid":"{DASHED}"}}"#))
                .unwrap(),
            Request::Drop {
                uuid: DASHED.into(),
                handover: None
            }
        );
        for without in [
            format!(r#"{{"op":"confirm","uuid":"{DASHED}"}}"#),
            format!(r#"{{"op":"release","uuid":"{DASHED}","outcome":"abort"}}"#),
        ] {
            assert!(
                serde_json::from_str::<Request>(&without).is_err(),
                "{without}"
            );
        }
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
            "handover": HANDOVER,
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
            place: None,
            arrival: None,
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
            place: Some(false),
            arrival: None,
        });
        let body: Value = serde_json::from_str(some.splitn(3, ' ').nth(2).unwrap()).unwrap();
        assert_eq!(
            body,
            json!({ "record": record, "parts": ["inventory", "experience"], "announce": false, "place": false })
        );

        let e = Request::Import {
            uuid: PLAIN.into(),
            record: json!("not an object"),
            parts: None,
            announce: None,
            place: None,
            arrival: None,
        }
        .endpoint_line()
        .unwrap_err();
        assert!(e.contains("JSON object"), "{e}");
    }

    /// The target takes a record with the id of an export with freeze only: one
    /// without, with a `null` (an export without freeze), or with a bad one is not sent.
    #[test]
    fn an_import_needs_the_hand_over_id_of_its_record() {
        for handover in [
            json!(null),
            json!(""),
            json!(7),
            json!("9f2c4be0a1d34e6b8c0f1e2d3c4b5a6"),
            json!("9f2c4be0-a1d3-4e6b-8c0f-1e2d3c4b5a69"),
            json!("9f2c4be0a1d34e6b8c0f1e2d3c4b5a6g"),
        ] {
            let e = Request::Import {
                uuid: PLAIN.into(),
                record: json!({ "version": 1, "handover": handover }),
                parts: None,
                announce: None,
                place: None,
                arrival: None,
            }
            .endpoint_line()
            .unwrap_err();
            assert!(e.contains("no hand-over id"), "{handover}: {e}");
        }
        let e = Request::Import {
            uuid: PLAIN.into(),
            record: json!({ "version": 1 }),
            parts: None,
            announce: None,
            place: None,
            arrival: None,
        }
        .endpoint_line()
        .unwrap_err();
        assert!(e.contains("export it with freeze"), "{e}");
        // The id is read in either case and the record is sent as it is.
        let upper = json!({ "handover": HANDOVER.to_uppercase() });
        assert_eq!(record_handover(&upper).as_deref(), Some(HANDOVER));
    }

    /// The endpoint takes 16 MiB a line, break included, and closes on a longer one.
    #[test]
    fn a_line_longer_than_the_endpoint_takes_is_not_sent() {
        let import = |padding: usize| {
            let mut line = Request::Import {
                uuid: PLAIN.into(),
                record: json!({ "handover": HANDOVER, "player": "" }),
                parts: None,
                announce: None,
                place: None,
                arrival: None,
            };
            let Request::Import { record, .. } = &mut line else {
                unreachable!()
            };
            record["player"] = Value::String("A".repeat(padding));
            line.endpoint_line()
        };
        let empty = import(0).unwrap().len();
        let room = MAX_LINE - 1 - empty;
        // The longest line that still fits, with its break, and one byte more.
        assert_eq!(import(room).unwrap().len() + 1, MAX_LINE);
        let e = import(room + 1).unwrap_err();
        assert!(e.contains("16 MiB"), "{e}");
    }

    /// An arrival goes to the backend as the import's own fields, beside the
    /// record, and crosses the proxy's exchange with the sidecar as it is.
    #[test]
    fn an_arrival_goes_beside_the_record() {
        let arrival = Arrival {
            position: [12.5, 71.0, -3.5],
            rotation: [45.0, 10.0],
            velocity: Some([0.0, 0.0, 0.0]),
            on_ground: Some(true),
        };
        let request = Request::Import {
            uuid: PLAIN.into(),
            record: json!({ "version": 1, "handover": HANDOVER }),
            parts: None,
            announce: None,
            place: Some(false),
            arrival: Some(arrival),
        };
        let body: Value =
            serde_json::from_str(line(request.clone()).splitn(3, ' ').nth(2).unwrap()).unwrap();
        assert_eq!(
            body,
            json!({
                "record": { "version": 1, "handover": HANDOVER },
                "place": false,
                "position": [12.5, 71.0, -3.5],
                "rotation": [45.0, 10.0],
                "velocity": [0.0, 0.0, 0.0],
                "on_ground": true,
            })
        );
        let wire = serde_json::to_string(&request).unwrap();
        assert_eq!(serde_json::from_str::<Request>(&wire).unwrap(), request);
        // Velocity and the ground are optional.
        let bare = Request::Import {
            uuid: PLAIN.into(),
            record: json!({ "version": 1, "handover": HANDOVER }),
            parts: Some(vec!["position".into()]),
            announce: None,
            place: None,
            arrival: Some(Arrival {
                velocity: None,
                on_ground: None,
                ..arrival
            }),
        };
        let body: Value = serde_json::from_str(line(bare).splitn(3, ' ').nth(2).unwrap()).unwrap();
        assert!(body.get("velocity").is_none() && body.get("on_ground").is_none());
    }

    /// What Horizon would refuse is refused before it is sent: an arrival without
    /// the position part, or numbers out of its bounds.
    #[test]
    fn an_arrival_horizon_would_refuse_is_not_sent() {
        let arrival = Arrival {
            position: [0.0, 64.0, 0.0],
            rotation: [0.0, 0.0],
            velocity: None,
            on_ground: None,
        };
        let import = |parts: Option<Vec<String>>, arrival: Arrival| {
            Request::Import {
                uuid: PLAIN.into(),
                record: json!({ "handover": HANDOVER }),
                parts,
                announce: None,
                place: None,
                arrival: Some(arrival),
            }
            .endpoint_line()
        };
        let e = import(Some(vec!["inventory".into()]), arrival).unwrap_err();
        assert!(e.contains("position part"), "{e}");
        for bad in [
            Arrival {
                position: [f64::NAN, 64.0, 0.0],
                ..arrival
            },
            Arrival {
                position: [30_000_001.0, 64.0, 0.0],
                ..arrival
            },
            Arrival {
                rotation: [f32::INFINITY, 0.0],
                ..arrival
            },
            Arrival {
                velocity: Some([0.0, 101.0, 0.0]),
                ..arrival
            },
        ] {
            assert!(import(None, bad).is_err(), "{bad:?}");
        }
    }

    /// The limits of an arrival are Horizon's: the same edges are taken, the next step is not.
    #[test]
    fn the_bounds_of_an_arrival_are_where_horizon_stops_taking_it() {
        let arrival = |position, velocity| Arrival {
            position,
            rotation: [0.0, 0.0],
            velocity,
            on_ground: None,
        };
        let taken = |a: Arrival| a.check().is_ok();
        // East-west and north-south: 30,000,000 either way, not a block more.
        for edge in [30_000_000.0, -30_000_000.0] {
            assert!(taken(arrival([edge, 64.0, 0.0], None)), "x {edge}");
            assert!(taken(arrival([0.0, 64.0, edge], None)), "z {edge}");
        }
        for past in [30_000_001.0, -30_000_001.0, 30_000_512.0] {
            assert!(!taken(arrival([past, 64.0, 0.0], None)), "x {past}");
            assert!(!taken(arrival([0.0, 64.0, past], None)), "z {past}");
        }
        // Up and down: 20,000,000, which Horizon holds to, not the 30,000,000 of the others.
        for edge in [20_000_000.0, -20_000_000.0] {
            assert!(taken(arrival([0.0, edge, 0.0], None)), "y {edge}");
        }
        for past in [20_000_001.0, -20_000_001.0, 25_000_000.0, 30_000_000.0] {
            assert!(!taken(arrival([0.0, past, 0.0], None)), "y {past}");
        }
        // The three axes at once, each at its own edge.
        assert!(taken(arrival(
            [30_000_000.0, -20_000_000.0, 30_000_000.0],
            None
        )));
        // Velocity: a component over 10 is not refused (Horizon takes it as 0 and the move goes
        // on); the bound only keeps absurd numbers out.
        for edge in [
            10.0,
            -10.0,
            10.000_001,
            11.0,
            50.0,
            MAX_VELOCITY,
            -MAX_VELOCITY,
        ] {
            for axis in 0..3 {
                let mut v = [0.0; 3];
                v[axis] = edge;
                assert!(taken(arrival([0.0, 64.0, 0.0], Some(v))), "{v:?}");
            }
        }
        for past in [100.000_001, -100.000_001, 101.0, 1e9] {
            for axis in 0..3 {
                let mut v = [0.0; 3];
                v[axis] = past;
                assert!(!taken(arrival([0.0, 64.0, 0.0], Some(v))), "{v:?}");
            }
        }
        // And what is not a number is neither a position nor a velocity.
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(!taken(arrival([0.0, bad, 0.0], None)), "y {bad}");
            assert!(
                !taken(arrival([0.0, 64.0, 0.0], Some([bad, 0.0, 0.0]))),
                "v {bad}"
            );
        }
    }

    #[test]
    fn release_says_moved_or_abort_and_announce_goes_with_moved() {
        let release = |outcome, announce| Request::Release {
            uuid: DASHED.into(),
            handover: HANDOVER.into(),
            outcome,
            announce,
        };
        assert_eq!(
            line(release(Outcome::Moved, false)),
            format!("player-release {DASHED} {HANDOVER} moved")
        );
        assert_eq!(
            line(release(Outcome::Moved, true)),
            format!("player-release {DASHED} {HANDOVER} moved announce")
        );
        assert_eq!(
            line(release(Outcome::Abort, false)),
            format!("player-release {DASHED} {HANDOVER} abort")
        );
        assert!(release(Outcome::Abort, true).endpoint_line().is_err());
    }

    #[test]
    fn confirm_is_its_unique_id_and_its_hand_over() {
        assert_eq!(
            line(Request::Confirm {
                uuid: PLAIN.into(),
                handover: HANDOVER.to_uppercase()
            }),
            format!("player-confirm {DASHED} {HANDOVER}")
        );
        let text = format!(r#"{{"op":"confirm","uuid":"{DASHED}","handover":"{HANDOVER}"}}"#);
        assert_eq!(
            serde_json::from_str::<Request>(&text).unwrap(),
            Request::Confirm {
                uuid: DASHED.into(),
                handover: HANDOVER.into()
            }
        );
    }

    #[test]
    fn drop_is_its_unique_id_and_maybe_a_hand_over() {
        assert_eq!(
            line(Request::Drop {
                uuid: PLAIN.into(),
                handover: None
            }),
            format!("player-drop {DASHED}")
        );
        assert_eq!(
            line(Request::Drop {
                uuid: PLAIN.into(),
                handover: Some(HANDOVER.into())
            }),
            format!("player-drop {DASHED} {HANDOVER}")
        );
    }

    #[test]
    fn a_bad_unique_id_never_becomes_a_line() {
        for uuid in ["Alex", "", "123e4567-e89b-12d3-a456-4266141740 0", "x stop"] {
            let e = Request::Drop {
                uuid: uuid.into(),
                handover: None,
            }
            .endpoint_line()
            .unwrap_err();
            assert!(e.contains("is not a unique id"), "{uuid}: {e}");
        }
    }

    /// A hand-over id is text in a line: anything but 32 hexadecimal digits is
    /// refused, so that it can never add a word or a command.
    #[test]
    fn a_bad_hand_over_never_becomes_a_line() {
        for handover in [
            "",
            "9f2c4be0a1d34e6b8c0f1e2d3c4b5a6",
            "9f2c4be0a1d34e6b8c0f1e2d3c4b5a690",
            "9f2c4be0-a1d3-4e6b-8c0f-1e2d3c4b5a69",
            "9f2c4be0a1d34e6b8c0f1e2d3c4b5a6g",
            "9f2c4be0a1d34e6b8c0f1e2d3c4b5a6 moved",
        ] {
            for request in [
                Request::Confirm {
                    uuid: DASHED.into(),
                    handover: handover.into(),
                },
                Request::Release {
                    uuid: DASHED.into(),
                    handover: handover.into(),
                    outcome: Outcome::Abort,
                    announce: false,
                },
                Request::Drop {
                    uuid: DASHED.into(),
                    handover: Some(handover.into()),
                },
            ] {
                let e = request.endpoint_line().unwrap_err();
                assert!(e.contains("is not a hand-over id"), "{handover:?}: {e}");
            }
        }
    }

    #[test]
    fn status_names_no_player_and_is_one_line() {
        assert_eq!(line(Request::Status {}), STATUS_LINE);
        let text = serde_json::to_string(&Request::Status {}).unwrap();
        assert_eq!(text, r#"{"op":"status"}"#);
        assert_eq!(
            serde_json::from_str::<Request>(&text).unwrap(),
            Request::Status {}
        );
        // Any other field is refused, as for every request.
        assert!(serde_json::from_str::<Request>(r#"{"op":"status","uuid":"x"}"#).is_err());
    }

    #[test]
    fn a_status_is_read_as_far_as_the_backend_gives_it() {
        let status = |v: Value| ServerStatus::from_value(&v);
        assert_eq!(
            status(json!({"players": 3, "capacity": 20, "accepts_transfers": false})),
            Ok(ServerStatus {
                players: Some(3),
                capacity: Some(20),
                accepts_transfers: Some(false),
            })
        );
        // What is left out, or null, is unknown, and other fields are ignored.
        assert_eq!(status(json!({})), Ok(ServerStatus::default()));
        assert_eq!(
            status(json!({"players": null, "activity": "idle", "tps": 19.9})),
            Ok(ServerStatus::default())
        );
        assert_eq!(
            status(json!({"capacity": 8})),
            Ok(ServerStatus {
                capacity: Some(8),
                ..ServerStatus::default()
            })
        );
    }

    /// The answer of Horizon's `transfer-status`, as its `docs/server.md` gives it.
    #[test]
    fn horizons_answer_to_transfer_status_is_read() {
        assert_eq!(STATUS_LINE, "transfer-status");
        let answer = |text: &str| ServerStatus::from_value(&serde_json::from_str(text).unwrap());
        // `online` is what the player limit counts, so it is the players; `players` are the
        // placed ones only. A server that answers has transfers on.
        assert_eq!(
            answer(
                r#"{"ok":true,"players":12,"held":1,"expected":2,"frozen":0,"online":13,"max_players":20,"full":false}"#
            ),
            Ok(ServerStatus {
                players: Some(13),
                capacity: Some(20),
                accepts_transfers: Some(true),
            })
        );
        assert_eq!(
            answer(
                r#"{"ok":true,"players":19,"held":1,"expected":0,"frozen":0,"online":20,"max_players":20,"full":true}"#
            ),
            Ok(ServerStatus {
                players: Some(20),
                capacity: Some(20),
                accepts_transfers: Some(true),
            })
        );
        // An answer that names `accepts_transfers` is believed over the default.
        assert_eq!(
            answer(r#"{"ok":true,"online":0,"max_players":20,"accepts_transfers":false}"#)
                .unwrap()
                .accepts_transfers,
            Some(false)
        );
    }

    #[test]
    fn a_status_that_is_not_one_is_refused() {
        for bad in [
            json!([]),
            json!("ok"),
            json!(null),
            json!({"players": "3"}),
            json!({"players": -1}),
            json!({"players": 1.5}),
            json!({"capacity": true}),
            json!({"accepts_transfers": "yes"}),
            json!({"accepts_transfers": 1}),
            json!({"online": "13", "max_players": 20}),
            json!({"online": 13, "max_players": -1}),
        ] {
            assert!(ServerStatus::from_value(&bad).is_err(), "{bad}");
        }
    }
}
