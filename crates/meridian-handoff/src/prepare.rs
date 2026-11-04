//! The target of a switch while it prepares: its play-phase packets go into the
//! target view, in the client's numbering, and the proxy answers for the client
//! what the target asks of a joining player (keep-alives, pings, teleports, chunk
//! batches, the end of loading), as Horizon's held player is expected to answer.
use crate::session::lock;
use horizon_protocol::{
    buf::{Reader, Writer, write_varint},
    connection::ConnWriter,
    ids::play::{clientbound as c, serverbound as s},
    packets::play::{self, Clientbound as C, Serverbound},
};
use meridian_ghost::{Packet, View};
use meridian_rewrite::Swap;
use std::{
    sync::{Condvar, Mutex, MutexGuard},
    time::Instant,
};

/// The chunks per tick the proxy asks of the target: the most a server takes
/// (`ChunkBatchSender`'s cap), so that the world arrives as fast as it can.
const CHUNKS_PER_TICK: f32 = 64.0;

/// The entity events that tell a player their operator level: 24 for level 0 to
/// 28 for level 4.
const PERMISSION_EVENTS: std::ops::RangeInclusive<i8> = 24..=28;

/// A switch's target lane, with what it has sent so far.
pub(crate) struct Target {
    state: Mutex<Preparing>,
    changed: Condvar,
}

pub(crate) struct Preparing {
    /// Still preparing: once the switch hands the client over, the lane is live.
    pub preparing: bool,
    /// What the target describes, in the client's numbering.
    pub view: View,
    client_id: i32,
    /// The target's numbering to the client's, from its Login.
    pub swap: Option<Swap>,
    pub dimension: Option<String>,
    pub secure_chat: Option<bool>,
    /// Whether the proxy answers the target's teleports itself. It stops before
    /// the import: the position the import sends is the client's to confirm.
    pub answer_teleports: bool,
    /// The teleports the proxy answered.
    pub answered: Vec<i32>,
    /// The id of the target's last Player Position, and how many it sent.
    pub last_teleport: Option<i32>,
    /// Where the target last put the player, for the answers to its teleports.
    pose: Pose,
    pub positions: u32,
    pub batches: u32,
    pub last_batch: Option<Instant>,
    /// When the target last moved the centre of the player's view: an import that
    /// moves the player does, before the chunks around the new place.
    pub last_center: Option<Instant>,
    loaded: bool,
    /// Packets the view does not keep but the client needs from the target, sent
    /// after the delta: the command tree, recipes, advancements, the player's own
    /// synced data.
    pub extras: Vec<Packet>,
    /// Why the target stopped, when it did.
    pub ended: Option<String>,
    /// Packets the view could not read, for the log.
    pub unreadable: Vec<String>,
}

/// The most unreadable packets kept for the log, per view.
const UNREADABLE_KEPT: usize = 8;

/// Notes a packet a view could not read: its id and why.
pub(crate) fn note_unreadable(list: &mut Vec<String>, id: i32, e: &horizon_protocol::Error) {
    if list.len() < UNREADABLE_KEPT {
        list.push(format!("packet {id}: {e}"));
    }
}

impl Target {
    pub fn new(client_id: i32) -> Self {
        Target {
            state: Mutex::new(Preparing {
                preparing: true,
                view: View::target(),
                client_id,
                swap: None,
                dimension: None,
                secure_chat: None,
                answer_teleports: true,
                answered: Vec::new(),
                last_teleport: None,
                pose: Pose::default(),
                positions: 0,
                batches: 0,
                last_batch: None,
                last_center: None,
                loaded: false,
                extras: Vec::new(),
                ended: None,
                unreadable: Vec::new(),
            }),
            changed: Condvar::new(),
        }
    }

    pub fn lock(&self) -> MutexGuard<'_, Preparing> {
        lock(&self.state)
    }

    /// Takes a packet of the target while it prepares, and returns `true`;
    /// `false` once the lane is live, when the packet goes to the client.
    pub fn take(&self, writer: &Mutex<ConnWriter>, id: i32, payload: &[u8]) -> bool {
        let mut state = self.lock();
        if !state.preparing {
            return false;
        }
        state.take(writer, id, payload);
        self.changed.notify_all();
        true
    }

    /// The target's connection is over.
    /// The target's connection is over: `broken` is the error that ended it, or
    /// `None` when the target closed it. A server closes a connection without a
    /// word on a packet it cannot read, so a close is worth naming as such.
    pub fn closed(&self, broken: Option<&horizon_protocol::Error>) {
        self.lock().ended.get_or_insert_with(|| match broken {
            Some(e) => format!("the connection to the target broke: {e}"),
            None => "the target closed the connection (a server does that, without a word, \
                     on a packet it cannot read)"
                .to_owned(),
        });
        self.changed.notify_all();
    }

    /// Waits until `ready` gives an answer, the target stops, or `deadline`
    /// passes: `Ok(answer)`, or `Err` with why.
    pub fn wait<R>(
        &self,
        deadline: Instant,
        late: &str,
        mut ready: impl FnMut(&Preparing) -> Option<R>,
    ) -> Result<R, String> {
        let mut state = self.lock();
        loop {
            if let Some(why) = &state.ended {
                return Err(why.clone());
            }
            if let Some(answer) = ready(&state) {
                return Ok(answer);
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(late.to_owned());
            }
            // Wakes at least every 50 ms, for conditions on time.
            let wait = (deadline - now).min(std::time::Duration::from_millis(50));
            state = self
                .changed
                .wait_timeout(state, wait)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}

impl Preparing {
    fn take(&mut self, writer: &Mutex<ConnWriter>, id: i32, payload: &[u8]) {
        if let Some((answer, payload)) = meridian_rewrite::answer(false, id, payload) {
            send(writer, answer, &payload);
            return;
        }
        match id {
            c::LOGIN => {
                let Ok(C::Login(mut login)) = C::decode(id, payload) else {
                    self.ended = Some("the target's Login is unreadable".into());
                    return;
                };
                self.swap = Some(Swap::new(login.player_id, self.client_id));
                self.dimension = Some(login.spawn.dimension.clone());
                self.secure_chat = Some(login.enforces_secure_chat);
                // The view is in the client's numbering, the player's id too.
                login.player_id = self.client_id;
                let _ = self.view.apply(&Packet::of(&C::Login(login)));
            }
            c::RESPAWN => {
                if let Ok(C::Respawn(respawn)) = C::decode(id, payload) {
                    self.dimension = Some(respawn.spawn.dimension);
                }
                self.apply(id, payload);
            }
            c::PLAYER_POSITION => {
                if let Ok(C::PlayerPosition(p)) = C::decode(id, payload) {
                    let (at, yaw, pitch) = self.pose.placed(&p);
                    if self.answer_teleports {
                        // The answer says where the player now is, as the client's
                        // does; a server cannot read one without it.
                        let answer = Serverbound::AcceptTeleportation(play::AcceptTeleportation {
                            id: p.teleport,
                            x: at[0],
                            y: at[1],
                            z: at[2],
                            yaw,
                            pitch,
                        });
                        send_body(writer, &answer.encode());
                        self.answered.push(p.teleport);
                    }
                    self.last_teleport = Some(p.teleport);
                    self.positions += 1;
                }
                self.apply(id, payload);
            }
            c::CHUNK_BATCH_FINISHED => {
                send(
                    writer,
                    s::CHUNK_BATCH_RECEIVED,
                    &CHUNKS_PER_TICK.to_be_bytes(),
                );
                self.batches += 1;
                self.last_batch = Some(Instant::now());
                // The client says it has loaded once the ground under it is there.
                if !self.loaded && self.positions > 0 {
                    send(writer, s::PLAYER_LOADED, &[]);
                    self.loaded = true;
                }
            }
            c::SET_CHUNK_CACHE_CENTER => {
                self.last_center = Some(Instant::now());
                self.apply(id, payload);
            }
            c::COOKIE_REQUEST => {
                if let Ok(key) = Reader::new(payload).string(32767) {
                    let mut w = Writer::new();
                    w.string(&key);
                    w.bool(false);
                    send(writer, s::COOKIE_RESPONSE, &w.data);
                }
            }
            c::DISCONNECT => self.ended = Some("the target disconnected the player".into()),
            c::START_CONFIGURATION => {
                self.ended = Some("the target sent the player back to configuration".into());
            }
            c::TRANSFER => self.ended = Some("the target sends the player elsewhere".into()),
            c::COMMANDS
            | c::UPDATE_RECIPES
            | c::RECIPE_BOOK_ADD
            | c::RECIPE_BOOK_REMOVE
            | c::RECIPE_BOOK_SETTINGS
            | c::UPDATE_ADVANCEMENTS
            | c::SELECT_ADVANCEMENTS_TAB => self.extras.push(Packet {
                id,
                payload: payload.to_vec(),
            }),
            c::ENTITY_EVENT if self.permission_event(id, payload) => {}
            c::SET_ENTITY_DATA if self.is_own(payload) => {
                let payload = self.swapped(id, payload);
                self.extras.push(Packet { id, payload });
            }
            _ => self.apply(id, payload),
        }
    }

    /// Keeps the target's permission level for the client, and says whether the
    /// packet was one: the player's entity event 24 to 28 (`PlayerList.
    /// sendPlayerPermissionLevel`, 24 plus the level). The views keep no entity
    /// events, and the client learns its level from nothing else, so it would keep
    /// that of its first server. The last one is sent after the delta, in the
    /// client's numbering, before the command tree it goes with; it is sent whatever
    /// the client holds, so that a target with a lower level lowers it.
    fn permission_event(&mut self, id: i32, payload: &[u8]) -> bool {
        let Ok(C::EntityEvent(event)) = C::decode(id, payload) else {
            return false;
        };
        let own = self.swap.is_some_and(|swap| swap.backend == event.id);
        if !own || !PERMISSION_EVENTS.contains(&event.event) {
            return false;
        }
        self.extras.retain(|packet| packet.id != c::ENTITY_EVENT);
        let payload = self.swapped(id, payload);
        let at = self
            .extras
            .iter()
            .position(|packet| packet.id == c::COMMANDS)
            .unwrap_or(self.extras.len());
        self.extras.insert(at, Packet { id, payload });
        true
    }

    /// Whether a packet that starts with an entity id is about the player.
    fn is_own(&self, payload: &[u8]) -> bool {
        let id = Reader::new(payload).varint().ok();
        self.swap.is_some_and(|swap| Some(swap.backend) == id)
    }

    fn swapped(&self, id: i32, payload: &[u8]) -> Vec<u8> {
        match self.swap {
            Some(swap) => swap
                .clientbound(id, payload.to_vec())
                .unwrap_or_else(|_| payload.to_vec()),
            None => payload.to_vec(),
        }
    }

    fn apply(&mut self, id: i32, payload: &[u8]) {
        let payload = self.swapped(id, payload);
        // A packet the view cannot read leaves it as it was; it is noted for the log.
        if let Err(e) = self.view.apply(&Packet { id, payload }) {
            note_unreadable(&mut self.unreadable, id, &e);
        }
    }
}

fn send(writer: &Mutex<ConnWriter>, id: i32, payload: &[u8]) {
    let mut body = Vec::with_capacity(payload.len() + 2);
    write_varint(&mut body, id);
    body.extend_from_slice(payload);
    send_body(writer, &body);
}

fn send_body(writer: &Mutex<ConnWriter>, body: &[u8]) {
    // A target that has gone is seen by its reader.
    let _ = lock(writer).send(body);
}

/// Where the player stands and how they look: where the target last put them,
/// or where their client last said it is.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Pose {
    pub at: [f64; 3],
    pub yaw: f32,
    pub pitch: f32,
    /// Whether the client last said it stands on the ground; `None` until a move
    /// said.
    pub on_ground: Option<bool>,
}

/// The flag of a move that says the client stands on the ground
/// (`ServerboundMovePlayerPacket`'s first bit).
const ON_GROUND: u8 = 1;

impl Pose {
    /// The pose a client says it has, from a packet it sends in the play phase:
    /// its moves, and its answers to teleports, which give where the teleport
    /// put it. `None` for any other packet.
    pub fn sent_by_client(&self, id: i32, payload: &[u8]) -> Option<Pose> {
        use horizon_protocol::ids::play::serverbound as s;
        if !matches!(
            id,
            s::MOVE_PLAYER_POS
                | s::MOVE_PLAYER_POS_ROT
                | s::MOVE_PLAYER_ROT
                | s::MOVE_PLAYER_STATUS_ONLY
                | s::ACCEPT_TELEPORTATION
        ) {
            return None;
        }
        let ground = |flags: u8| Some(flags & ON_GROUND != 0);
        let pose = |at, yaw, pitch, on_ground| {
            Some(Pose {
                at,
                yaw,
                pitch,
                on_ground,
            })
        };
        match Serverbound::decode(id, payload).ok()? {
            Serverbound::MovePlayerPos(p) => {
                pose([p.x, p.y, p.z], self.yaw, self.pitch, ground(p.flags))
            }
            Serverbound::MovePlayerPosRot(p) => {
                pose([p.x, p.y, p.z], p.yaw, p.pitch, ground(p.flags))
            }
            Serverbound::MovePlayerRot(p) => pose(self.at, p.yaw, p.pitch, ground(p.flags)),
            Serverbound::MovePlayerStatus(p) => {
                pose(self.at, self.yaw, self.pitch, ground(p.flags))
            }
            Serverbound::AcceptTeleportation(p) => {
                pose([p.x, p.y, p.z], p.yaw, p.pitch, self.on_ground)
            }
            _ => None,
        }
    }

    /// Applies Player Position, absolute or relative to the pose for the parts its
    /// flags name, and returns the new pose.
    pub fn placed(&mut self, p: &play::PlayerPosition) -> ([f64; 3], f32, f32) {
        use play::relative;
        let pick = |flag: i32, new: f64, old: f64| {
            if p.relatives & flag != 0 {
                old + new
            } else {
                new
            }
        };
        for (i, flag) in [relative::X, relative::Y, relative::Z]
            .into_iter()
            .enumerate()
        {
            self.at[i] = pick(flag, p.position[i], self.at[i]);
        }
        self.yaw = pick(relative::Y_ROT, f64::from(p.yaw), f64::from(self.yaw)) as f32;
        self.pitch = pick(relative::X_ROT, f64::from(p.pitch), f64::from(self.pitch)) as f32;
        (self.at, self.yaw, self.pitch)
    }
}
