//! What a switch asks of the backends on either side, through each one's sidecar
//! (DESIGN.md section 6, decision 8): the five player requests of Horizon's
//! held-player work.
//!
//! [`PlayerControl`] is that contract and nothing more. [`SidecarClient`] is the
//! real one; the tests use a fake that answers from memory.
use meridian_sidecar::client::SidecarClient;
use serde_json::Value;

pub use meridian_sidecar::request::Outcome;

/// A player record as `player-export` gives it. The proxy carries it from one
/// backend to the other without reading it, except for its dimension.
pub type Record = Value;

/// The player requests of a switch. Each error is the reason the backend or its
/// sidecar gave, as text for the person who asked for the switch.
pub trait PlayerControl {
    /// The player will log in through the proxy within `seconds`: the backend
    /// holds them when they do, without placing them in its world.
    fn expect(&mut self, uuid: &str, seconds: Option<u64>) -> Result<(), String>;

    /// The player's record; with `freeze`, their own inputs stop being acted on
    /// until a release.
    fn export(&mut self, uuid: &str, freeze: bool) -> Result<Record, String>;

    /// Puts `record` on the held player and places them in the world: every part
    /// of it, or only `parts`.
    fn import(
        &mut self,
        uuid: &str,
        record: Record,
        parts: Option<Vec<String>>,
        announce: Option<bool>,
    ) -> Result<(), String>;

    /// Ends the hand-over of a frozen player: they have moved away, or they stay.
    fn release(&mut self, uuid: &str, outcome: Outcome, announce: bool) -> Result<(), String>;

    /// Forgets an expected or held player.
    fn drop_player(&mut self, uuid: &str) -> Result<(), String>;
}

impl PlayerControl for SidecarClient {
    fn expect(&mut self, uuid: &str, seconds: Option<u64>) -> Result<(), String> {
        SidecarClient::expect(self, uuid, seconds)
            .map(drop)
            .map_err(|e| e.to_string())
    }

    fn export(&mut self, uuid: &str, freeze: bool) -> Result<Record, String> {
        let answer = SidecarClient::export(self, uuid, freeze).map_err(|e| e.to_string())?;
        // The endpoint answers {"ok": true, "record": {...}}.
        answer
            .get("record")
            .filter(|record| record.is_object())
            .cloned()
            .ok_or_else(|| format!("the export answer has no record: {answer}"))
    }

    fn import(
        &mut self,
        uuid: &str,
        record: Record,
        parts: Option<Vec<String>>,
        announce: Option<bool>,
    ) -> Result<(), String> {
        SidecarClient::import(self, uuid, record, parts, announce)
            .map(drop)
            .map_err(|e| e.to_string())
    }

    fn release(&mut self, uuid: &str, outcome: Outcome, announce: bool) -> Result<(), String> {
        SidecarClient::release(self, uuid, outcome, announce)
            .map(drop)
            .map_err(|e| e.to_string())
    }

    fn drop_player(&mut self, uuid: &str) -> Result<(), String> {
        SidecarClient::drop_player(self, uuid)
            .map(drop)
            .map_err(|e| e.to_string())
    }
}

/// A unique id as the requests take it: 32 hexadecimal digits.
pub fn uuid_text(uuid: [u8; 16]) -> String {
    format!("{:032x}", u128::from_be_bytes(uuid))
}

/// The dimension a record puts the player in, when it says.
pub fn record_dimension(record: &Record) -> Option<&str> {
    record.get("dimension").and_then(Value::as_str)
}
