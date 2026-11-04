//! What a switch asks of the backends on either side, through each one's sidecar
//! (DESIGN.md section 6, decision 8): the five player requests of Horizon's
//! held-player work.
//!
//! [`PlayerControl`] is that contract and nothing more. [`SidecarClient`] is the
//! real one; the tests use a fake that answers from memory.
use meridian_sidecar::client::SidecarClient;
use serde_json::Value;

pub use meridian_sidecar::request::{Arrival, Outcome, record_handover};

/// A player record as `player-export` gives it. The proxy carries it from one
/// backend to the other without reading it, except for its dimension and its
/// `handover` id. The target reads only its player compound and that id: the other
/// fields above it are for the proxy's checks and logs, and changing them changes
/// nothing.
pub type Record = Value;

/// The player requests of a switch. Each error is the reason the backend or its
/// sidecar gave, as text for the person who asked for the switch.
pub trait PlayerControl {
    /// The player will log in through the proxy within `seconds`: the backend
    /// holds them when they do, without placing them in its world.
    fn expect(&mut self, uuid: &str, seconds: Option<u64>) -> Result<(), String>;

    /// The player's record; with `freeze`, their own inputs stop being acted on
    /// until a release, and the record carries the `handover` id that the later
    /// requests name, with the ticks the freeze has left. A freeze asked again gives
    /// the same record, the same id, and does not freeze again.
    fn export(&mut self, uuid: &str, freeze: bool) -> Result<Exported, String>;

    /// After a request that was lost on the way (no answer in time, a connection that
    /// broke), makes the connection ready to ask the same thing again, and says
    /// whether it is. `false` for a request the backend answered, even with a
    /// refusal, and for a control that has nothing to reopen.
    fn recover(&mut self) -> Result<bool, String> {
        Ok(false)
    }

    /// Puts `record` on the held player: every part of it, or only `parts`. With
    /// `place`, the player is placed in the world at once; without, they stay held
    /// until [`PlayerControl::confirm`], and a [`PlayerControl::drop_player`] still
    /// cancels the arrival with nothing saved or said. An `arrival` puts the
    /// player there rather than at the record's place, with the `position` part.
    fn import(
        &mut self,
        uuid: &str,
        record: Record,
        parts: Option<Vec<String>>,
        announce: Option<bool>,
        place: bool,
        arrival: Option<Arrival>,
    ) -> Result<Imported, String>;

    /// Places a player imported without being placed, and announces them if the
    /// import said so. `handover` is the id of the record that was imported.
    fn confirm(&mut self, uuid: &str, handover: &str) -> Result<(), String>;

    /// Ends the hand-over `handover` of a frozen player: they have moved away, or
    /// they stay. The source refuses `Moved` once the freeze has run out.
    fn release(
        &mut self,
        uuid: &str,
        handover: &str,
        outcome: Outcome,
        announce: bool,
    ) -> Result<(), String>;

    /// Forgets an expected or held player. With a `handover`, only the arrival of
    /// that record, which must be the imported one; before any import there is no
    /// id to name.
    fn drop_player(&mut self, uuid: &str, handover: Option<&str>) -> Result<(), String>;
}

impl PlayerControl for SidecarClient {
    fn expect(&mut self, uuid: &str, seconds: Option<u64>) -> Result<(), String> {
        SidecarClient::expect(self, uuid, seconds)
            .map(drop)
            .map_err(|e| e.to_string())
    }

    fn export(&mut self, uuid: &str, freeze: bool) -> Result<Exported, String> {
        let answer = SidecarClient::export(self, uuid, freeze).map_err(|e| e.to_string())?;
        // The endpoint answers {"ok": true, "record": {...}}, and for a frozen player
        // "freeze_ticks_left" beside the record.
        let record = answer
            .get("record")
            .filter(|record| record.is_object())
            .cloned()
            .ok_or_else(|| format!("the export answer has no record: {answer}"))?;
        Ok(Exported {
            record,
            freeze_ticks_left: answer.get("freeze_ticks_left").and_then(Value::as_u64),
        })
    }

    fn recover(&mut self) -> Result<bool, String> {
        SidecarClient::recover(self).map_err(|e| format!("cannot reach the sidecar again: {e}"))
    }

    fn import(
        &mut self,
        uuid: &str,
        record: Record,
        parts: Option<Vec<String>>,
        announce: Option<bool>,
        place: bool,
        arrival: Option<Arrival>,
    ) -> Result<Imported, String> {
        let answer =
            SidecarClient::import(self, uuid, record, parts, announce, Some(place), arrival)
                .map_err(|e| e.to_string())?;
        Ok(Imported {
            recentered: answer.get("recentered").and_then(Value::as_bool),
        })
    }

    fn confirm(&mut self, uuid: &str, handover: &str) -> Result<(), String> {
        SidecarClient::confirm(self, uuid, handover)
            .map(drop)
            .map_err(|e| e.to_string())
    }

    fn release(
        &mut self,
        uuid: &str,
        handover: &str,
        outcome: Outcome,
        announce: bool,
    ) -> Result<(), String> {
        SidecarClient::release(self, uuid, handover, outcome, announce)
            .map(drop)
            .map_err(|e| e.to_string())
    }

    fn drop_player(&mut self, uuid: &str, handover: Option<&str>) -> Result<(), String> {
        SidecarClient::drop_player(self, uuid, handover)
            .map(drop)
            .map_err(|e| e.to_string())
    }
}

/// What an export gives.
#[derive(Clone, Debug, PartialEq)]
pub struct Exported {
    pub record: Record,
    /// The ticks (50 ms each) before the server lets the freeze go by itself, as
    /// the server counted them when it answered; `None` for a player not frozen, and
    /// for a server that does not say.
    pub freeze_ticks_left: Option<u64>,
}

/// What a target says of an import.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Imported {
    /// Whether the import moved the player to another chunk, whose surroundings
    /// the target is still to send; `None` when the target does not say.
    pub recentered: Option<bool>,
}

/// A unique id as the requests take it: 32 hexadecimal digits.
pub fn uuid_text(uuid: [u8; 16]) -> String {
    format!("{:032x}", u128::from_be_bytes(uuid))
}

/// The dimension a record puts the player in, when it says.
pub fn record_dimension(record: &Record) -> Option<&str> {
    record.get("dimension").and_then(Value::as_str)
}
