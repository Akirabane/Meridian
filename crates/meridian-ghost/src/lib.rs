//! The ghost: a model of what the client holds, kept from the clientbound packets
//! of the Play phase that go through the proxy, and the delta that turns it into
//! what another server describes (DESIGN.md sections 4, 5 and 8).
//!
//! One type, [`View`], serves both sides of a switch. The ghost is fed every
//! packet the client is sent; the target view is fed the target server's packets
//! during the prepare phase and keeps what it needs to send them again. [`delta`]
//! compares the two and returns the packets that bring the client from the ghost to
//! the target view, in bundles, never with a packet that would show the switch
//! ([`forbidden`]).
//!
//! Entity ids, teleport ids and the player's own id are left as the target server
//! numbers them. Translating them into the client's numbering is the job of the
//! rewriting layer (`meridian-rewrite`, DESIGN.md section 7), which does not exist
//! yet; the delta goes through it before it reaches the client.
//!
//! # Known limits
//!
//! - **Teleport Entity with the rotate-delta flag** (`ROTATE_DELTA`): the turn it
//!   gives the entity's velocity is not modelled. The ghost's velocity can then
//!   differ from the client's, which costs one Teleport Entity too many in the
//!   delta. For which entities a server sends the flag is not verified.
//! - **Values that cannot be unset.** Synced data, equipment and attributes can be
//!   set by a packet but not removed. An entity whose ghost has an entry the target
//!   lacks is removed and made again. The player cannot be made again: attributes
//!   the ghost has and the target lacks stay on the client, and the ghost then
//!   differs from the target. How often a server sends attributes to one player and
//!   not the other is not verified.
//! - **The player's own synced data** (Set Entity Data for the player's id) is not
//!   kept.
//! - **Times as last sent.** Effect durations, cooldowns, the border's size change
//!   and the time of day are kept as the packet gave them, not counted down. The
//!   delta sends the target's values as they were last sent.
//! - **Respawn** is modelled as a new player in an empty level that keeps the view
//!   distance and the world values. Which world values the client keeps through a
//!   Respawn is not verified; the delta never sends one.
//! - **A column's hash** is 64 bits. Two different columns with the same hash would
//!   leave the old one in place; the odds are about one in 2^64 a pair.
//! - **Chunk columns** that the client holds with the same blocks but reached by
//!   other packets are sent again: equal blocks are not recognised, only equal
//!   packets.
//! - **Not kept yet**: the recipe book, advancements, map data, resource packs and
//!   the server's command tree.
mod bossbars;
mod chunks;
mod delta;
mod entities;
mod player;
mod scoreboard;
mod tab;
mod view;
mod world;

pub use delta::{BUNDLE_LIMIT, delta};
pub use view::View;

use horizon_protocol::{buf::Reader, ids::play::clientbound as ids, packets::play};

/// A clientbound Play packet: its id and the bytes after the id, as the test kit
/// and the relay carry them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packet {
    pub id: i32,
    pub payload: Vec<u8>,
}

impl Packet {
    /// A typed packet, encoded.
    pub fn of(packet: &play::Clientbound) -> Self {
        let body = packet.encode();
        let mut r = Reader::new(&body);
        let id = r.varint().expect("an encoded packet starts with its id");
        Packet {
            id,
            payload: r.rest().to_vec(),
        }
    }

    /// The packet decoded, as `View::apply` reads it.
    pub fn decode(&self) -> horizon_protocol::Result<play::Clientbound> {
        play::Clientbound::decode(self.id, &self.payload)
    }
}

/// Whether the client must never get this packet after its first login, because
/// it would show a loading screen, start a new world or end the connection
/// (PACKETS.md, category 5): Login, Respawn, Start Configuration, Transfer,
/// Disconnect, and Game Event 13 (`LEVEL_CHUNKS_LOAD_START`).
pub fn forbidden(packet: &Packet) -> bool {
    match packet.id {
        ids::LOGIN | ids::RESPAWN | ids::START_CONFIGURATION | ids::TRANSFER | ids::DISCONNECT => {
            true
        }
        ids::GAME_EVENT => packet.payload.first() == Some(&LEVEL_CHUNKS_LOAD_START),
        _ => false,
    }
}

/// `ClientboundGameEventPacket.LEVEL_CHUNKS_LOAD_START`.
const LEVEL_CHUNKS_LOAD_START: u8 = 13;

#[cfg(test)]
mod tests;
