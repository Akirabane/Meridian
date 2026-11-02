//! The delta: the packets that turn what the client holds (the ghost) into what
//! the target server describes (the target view).
use crate::{Packet, View, forbidden, ids};
use horizon_protocol::packets::play::{self, Clientbound as C};

/// The most packets one bundle may hold: the client refuses the next one with
/// "Too many packets in a bundle" (`BundlerInfo.BUNDLE_SIZE_LIMIT`).
pub const BUNDLE_LIMIT: usize = 4096;

/// The packets that bring a client holding `from` to `to`, wrapped in bundles.
///
/// The order, after DESIGN.md sections 4 and 8:
///
/// 1. the entities that must go: those the target does not have, and those it has
///    under the same id as another entity (another UUID or type), which are made
///    again further down;
/// 2. the chunk cache's radius and centre, then the target's chunks nearest to the
///    player first, skipping those the client already holds; the player's position
///    comes once the columns next to the player are there;
/// 3. the old chunks the target does not have;
/// 4. the target's entities, made or updated, then the rides and leads between
///    them.
///
/// The client applies a bundle in one go, so no half-switched world is drawn. A
/// delta of more than [`BUNDLE_LIMIT`] packets (a full view at a large view
/// distance) is cut into several bundles, which the client applies in order, one
/// after the other: the first holds the entity removals, the nearest chunks and the
/// position.
///
/// `to` must be a target view ([`View::target`]) that kept its packets. Entity and
/// teleport ids are the target server's; the rewriting layer maps them.
pub fn delta(from: &View, to: &View) -> Vec<Packet> {
    debug_assert!(to.complete(), "the target view must keep its packets");
    let mut out = Vec::new();
    let removed = from.entities.to_remove(&to.entities);
    if !removed.is_empty() {
        out.push(Packet::of(&C::RemoveEntities(play::RemoveEntities {
            ids: removed.clone(),
        })));
    }
    let around = to.placed.as_ref().map_or(to.chunks.center, |p| {
        (column(p.position[0]), column(p.position[2]))
    });
    from.chunks
        .delta(&to.chunks, around, &mut out, |out: &mut Vec<Packet>| {
            if let Some(p) = &to.placed
                && from.placed.as_ref() != Some(p)
            {
                out.push(Packet::of(&C::PlayerPosition(play::PlayerPosition {
                    teleport: p.teleport,
                    position: p.position,
                    delta: p.velocity,
                    yaw: p.yaw,
                    pitch: p.pitch,
                    relatives: 0,
                })));
            }
        });
    from.entities.delta(&to.entities, &removed, &mut out);
    // Built only from packets that can never be forbidden; this is the last line
    // of defence if that ever changes.
    out.retain(|p| !forbidden(p));
    bundle(out)
}

/// The chunk column a coordinate is in.
fn column(v: f64) -> i32 {
    (v.floor() as i32) >> 4
}

/// Wraps `packets` in bundles of at most [`BUNDLE_LIMIT`] packets.
fn bundle(packets: Vec<Packet>) -> Vec<Packet> {
    let delimiter = Packet {
        id: ids::BUNDLE_DELIMITER,
        payload: Vec::new(),
    };
    let mut out = Vec::with_capacity(packets.len() + 2);
    for part in packets.chunks(BUNDLE_LIMIT) {
        out.push(delimiter.clone());
        out.extend_from_slice(part);
        out.push(delimiter.clone());
    }
    out
}
