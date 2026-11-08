//! The events the proxy tells an extension granted `events` (docs/EXTENSIONS.md, section 6): JSON-RPC notifications, with no id
//! and no answer, that are **never a decision**. The proxy and the SDK both read and write these shapes.
//!
//! What an event carries is small and plain: a name, a unique id, how the account was checked, the name of a server and, to an
//! extension granted `addresses` only, the address a player connected from. Every word that says why or how (`reason`, `outcome`,
//! `cause`) is one of a fixed few, never a text written by a player or a server. No packet, no chat, no secret is in an event.
//!
//! Every event carries `seq`, the number of the event on this connection (it starts at 1 at each connection and goes up by one for
//! each event the proxy tried to send, **including the ones it dropped**, so that a gap is a loss), and `at`, the time in
//! milliseconds since the Unix epoch by the proxy's clock. `events/dropped` has no `seq`: the `seq` of the next event shows the gap.
//!
//! **Protocol 0, so a field is optional**: these structures do not refuse a field they do not know, and a new field can be added
//! to an event without changing the version. A method this SDK does not know is [`Event::Unknown`].

use crate::decide::Player;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A player joined a server: their session is open.
pub const PLAYERS_JOINED: &str = "meridian:notification/players/joined";
/// A player's session ended.
pub const PLAYERS_LEFT: &str = "meridian:notification/players/left";
/// The proxy dropped events for want of room in the queue of this connection (the newest are the ones dropped).
pub const EVENTS_DROPPED: &str = "meridian:notification/events/dropped";

/// `players/joined`: who, and the server they are on. `address` is there only for an extension granted `addresses`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Joined {
    pub seq: u64,
    pub at: u64,
    pub player: Player,
    pub server: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
}

/// Why a session ended, in one of a few words.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LeftReason {
    /// The player closed the connection, or the connection ended (the default when the proxy did not end it).
    Left,
    /// An operator, the console or an extension kicked the player.
    Kicked,
    /// The same player logged in again and the proxy replaced the first session.
    Replaced,
    /// The proxy was stopping.
    Shutdown,
    /// The session broke.
    Error,
    /// A word that this version does not know, which a later proxy may send: nothing more is said.
    #[serde(other)]
    Other,
}

/// `players/left`: who, the server they were on when it ended, and why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Left {
    pub seq: u64,
    pub at: u64,
    pub player: Player,
    pub server: String,
    pub reason: LeftReason,
}

/// `events/dropped`: how many events the proxy dropped since the last it told. The SDK also makes one of these when its own queue
/// overflowed (`at` is then the SDK's clock). An extension that needs to be exact asks `players/list` again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dropped {
    pub at: u64,
    pub count: u64,
}

/// The most characters of the name of a method [`Event::Unknown`] keeps.
pub const MAX_UNKNOWN_METHOD: usize = 64;

/// An event, as an extension reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Joined(Joined),
    Left(Left),
    Dropped(Dropped),
    /// A method this SDK does not know, or one whose parameters are not the shape it knows.
    Unknown(String),
}

impl Event {
    /// Reads the notification `method` with `params`. Never fails: what is not understood is [`Event::Unknown`].
    pub fn parse(method: &str, params: Option<&Value>) -> Event {
        fn read<T: serde::de::DeserializeOwned>(params: Option<&Value>) -> Option<T> {
            serde_json::from_value(params?.clone()).ok()
        }
        let event = match method {
            PLAYERS_JOINED => read(params).map(Event::Joined),
            PLAYERS_LEFT => read(params).map(Event::Left),
            EVENTS_DROPPED => read(params).map(Event::Dropped),
            _ => None,
        };
        // A method this SDK does not know is cleaned and cut: it is a name a reader may log.
        event.unwrap_or_else(|| Event::Unknown(crate::text::clean(method, MAX_UNKNOWN_METHOD)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decide::Auth;
    use serde_json::json;

    fn player() -> Player {
        Player {
            name: "Alex".into(),
            uuid: "0a2ea4ba-bced-39d2-80e1-1ad51a0e925c".into(),
            auth: Auth::Verified,
        }
    }

    #[test]
    fn what_the_proxy_writes_is_what_the_sdk_reads_and_the_address_is_left_out_when_there_is_none()
    {
        let joined = Joined {
            seq: 1,
            at: 2,
            player: player(),
            server: "lobby".into(),
            address: None,
        };
        let written = serde_json::to_value(&joined).unwrap();
        assert_eq!(
            written,
            json!({"seq": 1, "at": 2, "server": "lobby",
                   "player": {"name": "Alex", "uuid": "0a2ea4ba-bced-39d2-80e1-1ad51a0e925c", "auth": "verified"}})
        );
        assert_eq!(
            Event::parse(PLAYERS_JOINED, Some(&written)),
            Event::Joined(joined.clone())
        );
        let with = Joined {
            address: Some("203.0.113.9".into()),
            ..joined
        };
        let written = serde_json::to_value(&with).unwrap();
        assert_eq!(written["address"], "203.0.113.9");
        assert_eq!(
            Event::parse(PLAYERS_JOINED, Some(&written)),
            Event::Joined(with)
        );
    }

    #[test]
    fn a_reason_is_one_word_and_a_word_nobody_knows_is_other() {
        for (reason, word) in [
            (LeftReason::Left, "left"),
            (LeftReason::Kicked, "kicked"),
            (LeftReason::Replaced, "replaced"),
            (LeftReason::Shutdown, "shutdown"),
            (LeftReason::Error, "error"),
        ] {
            assert_eq!(serde_json::to_value(reason).unwrap(), json!(word));
            assert_eq!(
                serde_json::from_value::<LeftReason>(json!(word)).unwrap(),
                reason
            );
        }
        assert_eq!(
            serde_json::from_value::<LeftReason>(json!("exploded")).unwrap(),
            LeftReason::Other
        );
    }

    #[test]
    fn the_name_of_an_unknown_method_is_clean_and_short() {
        let dirty = format!(
            "meridian:notification/\u{1b}[31m\n{}\u{202e}",
            "y".repeat(200)
        );
        let Event::Unknown(name) = Event::parse(&dirty, None) else {
            panic!("not unknown");
        };
        assert_eq!(name.chars().count(), MAX_UNKNOWN_METHOD);
        assert!(name.starts_with("meridian:notification/[31m"), "{name}");
        assert!(!name.chars().any(|c| c.is_control() || c == '\u{202e}'));
    }

    #[test]
    fn what_is_not_understood_is_unknown_and_never_an_error() {
        let bad = json!({"seq": "one"});
        for (method, params) in [
            (PLAYERS_JOINED, Some(&bad)),
            (PLAYERS_LEFT, None),
            ("meridian:notification/servers/up", Some(&bad)),
            ("", None),
        ] {
            assert_eq!(
                Event::parse(method, params),
                Event::Unknown(method.to_owned())
            );
        }
        assert_eq!(
            Event::parse(
                EVENTS_DROPPED,
                Some(&json!({"at": 1, "count": 4, "more": true}))
            ),
            Event::Dropped(Dropped { at: 1, count: 4 })
        );
    }
}
