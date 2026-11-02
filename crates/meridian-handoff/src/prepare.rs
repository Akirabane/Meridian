//! The target of a switch while it prepares: its play-phase packets go into the
//! target view, in the client's numbering, and the proxy answers for the client
//! what the target asks of a joining player (keep-alives, pings, teleports, chunk
//! batches, the end of loading), as Horizon's held player is expected to answer.
use crate::session::lock;
use horizon_protocol::{
    buf::{Reader, Writer, write_varint},
    connection::ConnWriter,
    ids::play::{clientbound as c, serverbound as s},
    packets::play::Clientbound as C,
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
    pub positions: u32,
    pub batches: u32,
    pub last_batch: Option<Instant>,
    loaded: bool,
    /// Packets the view does not keep but the client needs from the target, sent
    /// after the delta: the command tree, recipes, advancements, the player's own
    /// synced data.
    pub extras: Vec<Packet>,
    /// Why the target stopped, when it did.
    pub ended: Option<String>,
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
                positions: 0,
                batches: 0,
                last_batch: None,
                loaded: false,
                extras: Vec::new(),
                ended: None,
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
    pub fn closed(&self) {
        self.lock()
            .ended
            .get_or_insert_with(|| "the target closed the connection".to_owned());
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
                    if self.answer_teleports {
                        let mut body = Vec::new();
                        write_varint(&mut body, p.teleport);
                        send(writer, s::ACCEPT_TELEPORTATION, &body);
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
            c::SET_ENTITY_DATA if self.is_own(payload) => {
                let payload = self.swapped(id, payload);
                self.extras.push(Packet { id, payload });
            }
            _ => self.apply(id, payload),
        }
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
        // A packet the view cannot read leaves it as it was.
        let _ = self.view.apply(&Packet { id, payload });
    }
}

fn send(writer: &Mutex<ConnWriter>, id: i32, payload: &[u8]) {
    let mut body = Vec::with_capacity(payload.len() + 2);
    write_varint(&mut body, id);
    body.extend_from_slice(payload);
    // A target that has gone is seen by its reader.
    let _ = lock(writer).send(&body);
}
