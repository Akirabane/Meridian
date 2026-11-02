//! A view: the state one side of a switch describes, read from its packets.
use crate::{
    Packet, bossbars::BossBars, chunks::Chunks, entities::Entities, ids, player::Player,
    scoreboard::Scoreboard, tab::Tab, world::World,
};
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
    pub(crate) player: Player,
    pub(crate) world: World,
    pub(crate) tab: Tab,
    pub(crate) bossbars: BossBars,
    pub(crate) scoreboard: Scoreboard,
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
            player: Player::default(),
            world: World::default(),
            tab: Tab::default(),
            bossbars: BossBars::default(),
            scoreboard: Scoreboard::default(),
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

    /// The player's own state in one line, for a log: the inventory (stacks
    /// held and the state id), the held slot, experience, health, game mode and
    /// effects, as the packets read so far give them.
    pub fn player_summary(&self) -> String {
        let p = &self.player;
        let inventory = match &p.inventory {
            Some(items) => format!(
                "{} stacks (state {})",
                items.iter().filter(|s| s.is_some()).count(),
                p.inventory_state
            ),
            None => "none".to_owned(),
        };
        let or_none = |v: Option<String>| v.unwrap_or_else(|| "none".to_owned());
        format!(
            "inventory {inventory}, held slot {}, experience {}, health {}, game mode {}, \
             {} effects",
            or_none(p.held_slot.map(|s| s.to_string())),
            or_none(
                p.experience
                    .map(|(_, level, total)| format!("level {level} total {total}"))
            ),
            or_none(
                p.health
                    .map(|(health, food, _)| format!("{health} food {food}"))
            ),
            or_none(p.game_mode.map(|m| m.to_string())),
            p.effects.len(),
        )
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
                self.player = Player {
                    game_mode: Some(p.spawn.game_type),
                    ..Player::default()
                };
                self.world = World::default();
                self.tab = Tab::default();
                self.bossbars = BossBars::default();
                self.scoreboard = Scoreboard::default();
            }
            // A new player in a new level; the world values the server sends again.
            C::Respawn(p) => {
                let radius = self.chunks.radius;
                self.player = Player {
                    game_mode: Some(p.spawn.game_type),
                    ..Player::default()
                };
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
                self.player.apply(other, self.player_id);
                self.world.apply(other);
                self.tab.apply(other);
                self.bossbars.apply(other);
                self.scoreboard.apply(other);
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
    /// entities, position, player state, world values, player list, boss bars and
    /// scoreboard. Whether packets were kept, and the player's own id,
    /// which each server numbers its own way, do not count.
    fn eq(&self, other: &Self) -> bool {
        self.placed == other.placed
            && self.chunks == other.chunks
            && self.entities == other.entities
            && self.player == other.player
            && self.world == other.world
            && self.tab == other.tab
            && self.bossbars == other.bossbars
            && self.scoreboard == other.scoreboard
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
            | ids::SET_HEALTH
            | ids::SET_EXPERIENCE
            | ids::PLAYER_ABILITIES
            | ids::SET_HELD_SLOT
            | ids::GAME_EVENT
            | ids::COOLDOWN
            | ids::CONTAINER_SET_CONTENT
            | ids::CONTAINER_SET_SLOT
            | ids::SET_PLAYER_INVENTORY
            | ids::SET_CURSOR_ITEM
            | ids::OPEN_SCREEN
            | ids::CONTAINER_CLOSE
            | ids::SET_TIME
            | ids::INITIALIZE_BORDER
            | ids::SET_BORDER_CENTER
            | ids::SET_BORDER_LERP_SIZE
            | ids::SET_BORDER_SIZE
            | ids::SET_BORDER_WARNING_DELAY
            | ids::SET_BORDER_WARNING_DISTANCE
            | ids::CHANGE_DIFFICULTY
            | ids::SET_DEFAULT_SPAWN_POSITION
            | ids::TICKING_STATE
            | ids::TICKING_STEP
            | ids::SET_SIMULATION_DISTANCE
            | ids::PLAYER_INFO_UPDATE
            | ids::PLAYER_INFO_REMOVE
            | ids::TAB_LIST
            | ids::BOSS_EVENT
            | ids::SET_OBJECTIVE
            | ids::SET_SCORE
            | ids::RESET_SCORE
            | ids::SET_DISPLAY_OBJECTIVE
            | ids::SET_PLAYER_TEAM
    )
}
