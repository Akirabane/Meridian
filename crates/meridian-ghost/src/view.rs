//! A view: the state one side of a switch describes, read from its packets.
use crate::{Packet, chunks::Chunks, entities::Entities, ids};
use horizon_protocol::{
    buf::Reader,
    packets::play::{self, Clientbound as C, relative},
};

/// Where the server last put the player (Player Position).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Placed {
    pub position: [f64; 3],
    pub velocity: [f64; 3],
    pub yaw: f32,
    pub pitch: f32,
    /// The teleport id the client confirms, in the numbering of the server that
    /// sent it.
    pub teleport: i32,
}

/// What a client holds, as far as the packets it was sent tell.
///
/// The ghost ([`View::ghost`]) follows the packets the client gets. The target view
/// ([`View::target`]) follows the target server's packets during a switch and also
/// keeps what it needs to send them again.
#[derive(Clone, Debug)]
pub struct View {
    keep: bool,
    /// The player's own entity id, from Login.
    player_id: Option<i32>,
    pub(crate) placed: Option<Placed>,
    pub(crate) chunks: Chunks,
    pub(crate) entities: Entities,
}

impl View {
    /// A view that follows what the client is sent.
    pub fn ghost() -> Self {
        Self::new(false)
    }

    /// A view of a switch's target server, which keeps the packets the delta
    /// sends.
    pub fn target() -> Self {
        Self::new(true)
    }

    fn new(keep: bool) -> Self {
        View {
            keep,
            player_id: None,
            placed: None,
            chunks: Chunks::default(),
            entities: Entities::default(),
        }
    }

    /// The player's own entity id, once a Login was read.
    pub fn player_id(&self) -> Option<i32> {
        self.player_id
    }

    /// How many chunks and entities the view holds.
    pub fn counts(&self) -> (usize, usize) {
        (self.chunks.loaded.len(), self.entities.by_id.len())
    }

    /// Whether this view can be the target of a delta: it kept the packets of
    /// every chunk it holds.
    pub fn complete(&self) -> bool {
        self.chunks.complete()
    }

    /// Reads one clientbound Play packet. Packets that change nothing the view
    /// keeps are skipped without being decoded.
    pub fn apply(&mut self, packet: &Packet) -> horizon_protocol::Result<()> {
        if packet.id == ids::LEVEL_CHUNK_WITH_LIGHT {
            // The column's position is at the start; the rest is the chunk data,
            // which the view never needs to read.
            let mut r = Reader::new(&packet.payload);
            let (x, z) = (r.i32()?, r.i32()?);
            self.chunks.loaded(x, z, packet, self.keep);
            return Ok(());
        }
        if !read(packet.id) {
            return Ok(());
        }
        let decoded = packet.decode()?;
        match &decoded {
            // A new level: the client starts from an empty one.
            C::Login(p) => {
                self.player_id = Some(p.player_id);
                self.placed = None;
                self.chunks = Chunks::default();
                self.chunks.set_radius(p.chunk_radius);
                self.entities = Entities::default();
            }
            C::Respawn(_) => {
                let radius = self.chunks.radius;
                self.placed = None;
                self.chunks = Chunks {
                    radius,
                    ..Chunks::default()
                };
                self.entities = Entities::default();
            }
            C::PlayerPosition(p) => self.place(p),
            other => {
                self.chunks.apply(other, packet, self.keep);
                self.entities.apply(other);
            }
        }
        Ok(())
    }

    /// Player Position: absolute, or relative to where the player was for the
    /// parts its flags name.
    fn place(&mut self, p: &play::PlayerPosition) {
        let was = self.placed.clone().unwrap_or(Placed {
            position: [0.0; 3],
            velocity: [0.0; 3],
            yaw: 0.0,
            pitch: 0.0,
            teleport: 0,
        });
        let r = p.relatives;
        let pick = |flag: i32, new: f64, old: f64| if r & flag != 0 { old + new } else { new };
        let mut position = [0.0; 3];
        let mut velocity = [0.0; 3];
        for i in 0..3 {
            position[i] = pick(
                [relative::X, relative::Y, relative::Z][i],
                p.position[i],
                was.position[i],
            );
            velocity[i] = pick(
                [relative::DELTA_X, relative::DELTA_Y, relative::DELTA_Z][i],
                p.delta[i],
                was.velocity[i],
            );
        }
        self.placed = Some(Placed {
            position,
            velocity,
            yaw: pick(relative::Y_ROT, f64::from(p.yaw), f64::from(was.yaw)) as f32,
            pitch: pick(relative::X_ROT, f64::from(p.pitch), f64::from(was.pitch)) as f32,
            teleport: p.teleport,
        });
    }
}

impl PartialEq for View {
    /// Two views are equal when a client would hold the same: the same chunks,
    /// entities and position. Whether packets were kept, and the player's own id,
    /// which each server numbers its own way, do not count.
    fn eq(&self, other: &Self) -> bool {
        self.placed == other.placed
            && self.chunks == other.chunks
            && self.entities == other.entities
    }
}

/// The packets a view reads (other than chunk packets, which it reads without
/// decoding them).
fn read(id: i32) -> bool {
    matches!(
        id,
        ids::LOGIN
            | ids::RESPAWN
            | ids::PLAYER_POSITION
            | ids::FORGET_LEVEL_CHUNK
            | ids::LIGHT_UPDATE
            | ids::BLOCK_UPDATE
            | ids::SECTION_BLOCKS_UPDATE
            | ids::CHUNKS_BIOMES
            | ids::SET_CHUNK_CACHE_CENTER
            | ids::SET_CHUNK_CACHE_RADIUS
            | ids::ADD_ENTITY
            | ids::REMOVE_ENTITIES
            | ids::MOVE_ENTITY_POS
            | ids::MOVE_ENTITY_POS_ROT
            | ids::MOVE_ENTITY_ROT
            | ids::TELEPORT_ENTITY
            | ids::ENTITY_POSITION_SYNC
            | ids::ROTATE_HEAD
            | ids::SET_ENTITY_MOTION
            | ids::SET_ENTITY_DATA
            | ids::SET_EQUIPMENT
            | ids::UPDATE_ATTRIBUTES
            | ids::UPDATE_MOB_EFFECT
            | ids::REMOVE_MOB_EFFECT
            | ids::SET_PASSENGERS
            | ids::SET_ENTITY_LINK
    )
}
