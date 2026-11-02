//! Chunks: which columns the client holds, and a hash of what each one holds.
//!
//! A chunk's hash covers its chunk packet and every later packet that changed it
//! (block updates, light updates, biomes), in order. Two views with the same hash
//! at a position were sent the same packets for it, so the client holds the same
//! column; two views with different hashes may still hold the same blocks, and the
//! chunk is then sent again. A difference is never missed, at the price of
//! sometimes sending a chunk that was already right.
//!
//! The ghost keeps only the hashes: about 40 bytes a chunk with the map's own
//! cost, some 25 KB for a view distance of 12. The target view also keeps the
//! packets, since the delta sends them: that is the full chunk data, tens of
//! kilobytes a chunk, a few megabytes for a player in the middle of a switch.
use crate::Packet;
use horizon_protocol::packets::play::{self, Clientbound as C};
use std::{
    collections::BTreeMap,
    hash::{DefaultHasher, Hasher},
};

/// One column the client holds.
#[derive(Clone, Debug)]
pub(crate) struct Chunk {
    hash: u64,
    /// The packets that rebuild the column, oldest first: kept by a target view
    /// only.
    packets: Vec<Packet>,
}

impl PartialEq for Chunk {
    /// Two columns are the same when the same packets built them.
    fn eq(&self, other: &Self) -> bool {
        self.hash == other.hash
    }
}

/// The client's chunk cache.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Chunks {
    /// `SetChunkCacheCenter`; the client starts at 0, 0.
    pub center: (i32, i32),
    /// The view distance from Login or `SetChunkCacheRadius`.
    pub radius: Option<i32>,
    pub loaded: BTreeMap<(i32, i32), Chunk>,
}

impl Chunks {
    /// A chunk packet for the column at `x`, `z`: it replaces whatever the
    /// column held. `keep` keeps the packet.
    pub fn loaded(&mut self, x: i32, z: i32, raw: &Packet, keep: bool) {
        let chunk = Chunk {
            hash: fold(0, raw),
            packets: if keep { vec![raw.clone()] } else { Vec::new() },
        };
        self.loaded.insert((x, z), chunk);
    }

    /// Reads a packet that concerns the chunk cache, other than a chunk packet
    /// ([`Chunks::loaded`]). `keep` keeps the packets.
    pub fn apply(&mut self, packet: &C, raw: &Packet, keep: bool) {
        match packet {
            C::ForgetLevelChunk(p) => {
                self.loaded.remove(&(p.pos[0], p.pos[1]));
            }
            C::LightUpdate(p) => self.change((p.x, p.z), raw, keep),
            C::BlockUpdate(p) => self.change((p.pos[0] >> 4, p.pos[2] >> 4), raw, keep),
            C::SectionBlocksUpdate(p) => self.change((p.section[0], p.section[2]), raw, keep),
            // One packet for several columns: each gets its own share, so that a
            // column can be sent again alone.
            C::ChunksBiomes(p) => {
                for data in &p.chunks {
                    let one = Packet::of(&C::ChunksBiomes(play::ChunksBiomes {
                        chunks: vec![data.clone()],
                    }));
                    self.change((data.pos[0], data.pos[1]), &one, keep);
                }
            }
            C::SetChunkCacheCenter(p) => {
                self.center = (p.x, p.z);
                self.drop_out_of_range();
            }
            C::SetChunkCacheRadius(p) => {
                self.radius = Some(p.distance);
                self.drop_out_of_range();
            }
            _ => {}
        }
    }

    /// The view distance a Login sets.
    pub fn set_radius(&mut self, radius: i32) {
        self.radius = Some(radius);
        self.drop_out_of_range();
    }

    /// A packet that changed a loaded column; the client ignores one for a
    /// column it does not hold.
    fn change(&mut self, at: (i32, i32), raw: &Packet, keep: bool) {
        if let Some(chunk) = self.loaded.get_mut(&at) {
            chunk.hash = fold(chunk.hash, raw);
            if keep {
                chunk.packets.push(raw.clone());
            }
        }
    }

    /// `ClientChunkCache`: the client keeps the columns within
    /// `max(2, radius) + 3` of the centre (`calculateStorageRange`,
    /// `Storage.inRange`) and loses the others when the centre or the radius
    /// moves. Forgetting them here as well errs on the safe side: a column the
    /// client happened to keep is sent again.
    fn drop_out_of_range(&mut self) {
        let Some(radius) = self.radius else {
            return;
        };
        let range = radius.max(2) + 3;
        let (cx, cz) = self.center;
        self.loaded
            .retain(|&(x, z), _| (x - cx).abs() <= range && (z - cz).abs() <= range);
    }

    /// The packets that bring the client from `self` to `to`, with the columns
    /// nearest to `around` first. `position` is called once the columns next to
    /// the player have been sent, so that the player arrives on ground that is
    /// there.
    pub fn delta(
        &self,
        to: &Chunks,
        around: (i32, i32),
        out: &mut Vec<Packet>,
        mut position: impl FnMut(&mut Vec<Packet>),
    ) {
        // The radius and the centre first: the client ignores a column outside
        // its range (`ClientChunkCache.replaceWithPacketData`).
        if to.radius != self.radius
            && let Some(distance) = to.radius
        {
            out.push(Packet::of(&C::SetChunkCacheRadius(play::Distance {
                distance,
            })));
        }
        if to.center != self.center {
            let (x, z) = to.center;
            out.push(Packet::of(&C::SetChunkCacheCenter(
                play::SetChunkCacheCenter { x, z },
            )));
        }
        let mut wanted: Vec<(&(i32, i32), &Chunk)> = to.loaded.iter().collect();
        wanted.sort_by_key(|&(&(x, z), _)| {
            let (dx, dz) = (i64::from(x - around.0), i64::from(z - around.1));
            (dx.abs().max(dz.abs()), dx * dx + dz * dz, x, z)
        });
        let mut placed = false;
        for (&(x, z), chunk) in wanted {
            let ring = (x - around.0).abs().max((z - around.1).abs());
            if ring > 1 && !placed {
                position(out);
                placed = true;
            }
            // A column the client holds is replaced in place by the new one
            // (`ClientChunkCache.replaceWithPacketData`): no Forget before it.
            if self.loaded.get(&(x, z)) != Some(chunk) {
                out.extend(chunk.packets.iter().cloned());
            }
        }
        if !placed {
            position(out);
        }
        // Columns the target does not have go last: they are the old world, and
        // nothing of the new one waits for them.
        for &(x, z) in self.loaded.keys() {
            if !to.loaded.contains_key(&(x, z)) {
                out.push(Packet::of(&C::ForgetLevelChunk(play::ForgetLevelChunk {
                    pos: [x, z],
                })));
            }
        }
    }

    /// Whether every column of this view can be sent again.
    pub fn complete(&self) -> bool {
        self.loaded.values().all(|c| !c.packets.is_empty())
    }
}

/// The hash of a column after `packet`.
fn fold(hash: u64, packet: &Packet) -> u64 {
    // A fixed key: both views are hashed in the same process.
    let mut h = DefaultHasher::new();
    h.write_u64(hash);
    h.write_i32(packet.id);
    h.write(&packet.payload);
    h.finish()
}
