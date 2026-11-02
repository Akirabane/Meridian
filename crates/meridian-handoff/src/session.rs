//! A relayed session whose backend can change: the relay of `meridian-net`, with the
//! ghost fed on the way and the player's ids rewritten, and with room for a second
//! backend connection while a switch prepares.
//!
//! Each backend connection is a *lane*, read by a thread of its own. Only the
//! active lane's packets reach the client: those of a lane that is no longer active
//! (the old backend after a switch) are dropped, and those of a lane a switch is
//! preparing go into the switch's target view instead (see `prepare`). One thread
//! copies the client's packets to the active lane.
//!
//! The locks are always taken in the same order, so that none waits on another
//! in a circle: a target's prepare state, then what goes to the client ([`Down`]),
//! then what goes to the backend ([`Up`]), then a lane's writer.
use crate::{
    configure::{ClientConfiguration, Digest, DigestBuilder},
    prepare::Target,
};
use horizon_protocol::{
    buf::{Reader, write_varint},
    connection::{Closer, ConnReader, ConnWriter},
    ids,
    packets::{configuration, play},
    text,
};
use meridian_ghost::{Packet, View};
use meridian_net::{
    Profile, Result,
    backend::BackendLink,
    client::ClientSession,
    relay::{CommandHook, Flow, Phase, Report},
};
use meridian_rewrite::{Swap, Teleports};
use std::sync::{
    Arc, Mutex, MutexGuard,
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc,
};

/// A session ready to be relayed. [`Session::new`] also gives the
/// [`SessionHandle`] that switches it to another backend, or ends it, from another
/// thread.
pub struct Session {
    shared: Arc<Shared>,
    client_reader: ConnReader,
    first: (Arc<Lane>, ConnReader),
}

/// Switches a session to another backend (`crate::switch`), or ends it, from another
/// thread. It can be cloned and kept after the session is over.
#[derive(Clone)]
pub struct SessionHandle(pub(crate) Arc<Shared>);

pub(crate) struct Shared {
    pub profile: Profile,
    down: Mutex<Down>,
    up: Mutex<Up>,
    client_closer: Closer,
    commands: Mutex<Option<Arc<CommandHook>>>,
    events: Mutex<Option<mpsc::Sender<Event>>>,
    next_lane: AtomicU64,
    /// A switch is under way; it closes what is left when it ends.
    pub switching: AtomicBool,
    /// The client has gone.
    pub client_gone: AtomicBool,
    /// The session is over: the client is closed.
    pub ended: AtomicBool,
}

/// What goes to the client.
pub(crate) struct Down {
    pub writer: ConnWriter,
    /// The phase the client reads packets in.
    pub phase: Phase,
    /// What the client holds, in the client's numbering.
    pub ghost: View,
    /// The lane whose packets reach the client.
    pub active: u64,
    /// The active backend's name.
    pub backend: String,
    /// The active backend's numbering to the client's.
    pub swap: Swap,
    /// The dimension the client is in, from Login and Respawn.
    pub dimension: Option<String>,
    /// Whether the first backend enforces signed chat, from its Login.
    pub secure_chat: Option<bool>,
    /// The first backend's configuration phase, as the client got it.
    digest: DigestBuilder,
    pub config_digest: Option<Digest>,
    pub flow: Flow,
}

/// What goes to the backend.
pub(crate) struct Up {
    pub lane: Arc<Lane>,
    pub swap: Swap,
    pub teleports: Teleports,
    /// What the client sent on its own in its configuration phase.
    pub client: ClientConfiguration,
    pub flow: Flow,
}

/// One connection to a backend.
pub(crate) struct Lane {
    pub id: u64,
    pub writer: Mutex<ConnWriter>,
    pub closer: Closer,
    /// The prepare state, for a switch's target.
    pub target: Option<Arc<Target>>,
}

/// How one thread of the session stopped.
pub(crate) enum Event {
    Client(End),
    Lane(u64, End),
}

/// How a copy stopped.
pub(crate) enum End {
    /// The side read from closed, or the side written to did.
    Closed,
    /// The side read from sent something that could not be read.
    Broken(horizon_protocol::Error),
}

impl Session {
    /// Takes over both connections, both at the start of the configuration phase,
    /// as [`meridian_net::client::accept`] and [`meridian_net::backend::connect`]
    /// leave them. `backend` is the name of the backend the player starts on.
    pub fn new(
        client: ClientSession,
        backend: BackendLink,
        name: &str,
    ) -> Result<(Session, SessionHandle)> {
        let (client_reader, client_writer) = client.connection.split();
        let client_closer = client_reader.closer()?;
        let (backend_reader, backend_writer) = backend.connection.split();
        let lane = Arc::new(Lane {
            id: 0,
            writer: Mutex::new(backend_writer),
            closer: backend_reader.closer()?,
            target: None,
        });
        let shared = Arc::new(Shared {
            profile: client.profile,
            down: Mutex::new(Down {
                writer: client_writer,
                phase: Phase::Configuration,
                ghost: View::ghost(),
                active: lane.id,
                backend: name.to_owned(),
                swap: Swap::new(0, 0),
                dimension: None,
                secure_chat: None,
                digest: DigestBuilder::default(),
                config_digest: None,
                flow: Flow::default(),
            }),
            up: Mutex::new(Up {
                lane: Arc::clone(&lane),
                swap: Swap::new(0, 0),
                teleports: Teleports::default(),
                client: ClientConfiguration::default(),
                flow: Flow::default(),
            }),
            client_closer,
            commands: Mutex::new(None),
            events: Mutex::new(None),
            next_lane: AtomicU64::new(1),
            switching: AtomicBool::new(false),
            client_gone: AtomicBool::new(false),
            ended: AtomicBool::new(false),
        });
        let handle = SessionHandle(Arc::clone(&shared));
        Ok((
            Session {
                shared,
                client_reader,
                first: (lane, backend_reader),
            },
            handle,
        ))
    }

    /// Lets `hook` answer the commands the player types in the play phase.
    pub fn with_commands(self, hook: Arc<CommandHook>) -> Self {
        *lock(&self.shared.commands) = Some(hook);
        self
    }

    /// Relays the session until the client or the active backend closes it, and
    /// says what it carried. A switch under way when the client leaves finishes
    /// on its own thread (DESIGN.md decision 10).
    pub fn run(self) -> Result<Report> {
        let Session {
            shared,
            client_reader,
            first: (lane, backend_reader),
        } = self;
        let (events, ended) = mpsc::channel();
        *lock(&shared.events) = Some(events.clone());
        let to_backend = {
            let shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("session-to-backend".to_owned())
                .spawn(move || {
                    let end = copy_to_backend(client_reader, &shared);
                    let _ = events.send(Event::Client(end));
                })?
        };
        spawn_lane(&shared, lane, backend_reader)?;
        let mut first = None;
        for event in ended.iter() {
            match event {
                Event::Client(end) => {
                    shared.client_gone.store(true, Ordering::SeqCst);
                    first = Some(end);
                    break;
                }
                Event::Lane(id, end) if id == shared.down().active => {
                    first = Some(end);
                    break;
                }
                // A lane that is not active, or no longer: an old backend after a
                // switch, or a target the switch gave up.
                Event::Lane(..) => {}
            }
        }
        *lock(&shared.events) = None;
        shared.client_gone.store(true, Ordering::SeqCst);
        shared.client_closer.close();
        shared.ended.store(true, Ordering::SeqCst);
        // A switch under way closes the lanes itself when it is done.
        if !shared.switching.load(Ordering::SeqCst) {
            shared.up().lane.closer.close();
        }
        let _ = to_backend.join();
        let report = Report {
            to_client: shared.down().flow,
            to_backend: shared.up().flow,
        };
        match first {
            Some(End::Closed) | None => Ok(report),
            Some(End::Broken(e)) => Err(e.into()),
        }
    }
}

impl SessionHandle {
    /// The phase the player's client is in, as far as the session has sent it.
    pub fn phase(&self) -> Phase {
        self.0.down().phase
    }

    /// The name of the backend the player is on.
    pub fn backend(&self) -> String {
        self.0.down().backend.clone()
    }

    /// The player.
    pub fn profile(&self) -> &Profile {
        &self.0.profile
    }

    /// Shows the player `message` in the chat. Nothing is sent outside the play
    /// phase, where the client reads no chat.
    pub fn tell(&self, message: &str) {
        let mut down = self.0.down();
        if down.phase != Phase::Play {
            return;
        }
        let packet = play::Clientbound::SystemChat(play::SystemChat {
            content: text::literal(message),
            overlay: false,
        });
        let _ = down.writer.send(&packet.encode());
    }

    /// Whether the session is over for the client: it left, or was closed.
    pub fn is_over(&self) -> bool {
        self.0.client_gone.load(Ordering::SeqCst)
    }

    /// Tells the player why the session ends, in the phase their client reads,
    /// then closes it.
    pub fn disconnect(&self, reason: configuration::Disconnect) {
        {
            let mut down = self.0.down();
            let body = match down.phase {
                Phase::Configuration => configuration::Clientbound::Disconnect(reason).encode(),
                Phase::Play => play::Clientbound::Disconnect(reason).encode(),
            };
            let _ = down.writer.send(&body);
        }
        self.close();
    }

    /// Closes the session without a word.
    pub fn close(&self) {
        self.0.client_closer.close();
        self.0.up().lane.closer.close();
    }
}

impl Shared {
    pub fn down(&self) -> MutexGuard<'_, Down> {
        lock(&self.down)
    }

    pub fn up(&self) -> MutexGuard<'_, Up> {
        lock(&self.up)
    }

    /// A number for a new lane.
    pub fn lane_id(&self) -> u64 {
        self.next_lane.fetch_add(1, Ordering::SeqCst)
    }
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// Starts the thread that reads a lane.
pub(crate) fn spawn_lane(shared: &Arc<Shared>, lane: Arc<Lane>, reader: ConnReader) -> Result<()> {
    let events = lock(&shared.events).clone();
    let shared = Arc::clone(shared);
    std::thread::Builder::new()
        .name(format!("session-lane-{}", lane.id))
        .spawn(move || {
            let end = copy_from_backend(reader, &lane, &shared);
            if let Some(target) = &lane.target {
                target.closed();
            }
            if let Some(events) = events {
                let _ = events.send(Event::Lane(lane.id, end));
            }
        })?;
    Ok(())
}

/// Copies a lane's packets: to the switch's target view while it prepares, to the
/// client while it is the active lane, nowhere otherwise.
fn copy_from_backend(mut from: ConnReader, lane: &Lane, shared: &Shared) -> End {
    let mut body = Vec::new();
    loop {
        let (id, payload) = match from.recv() {
            Ok(packet) => packet,
            Err(e) if e.is_end_of_stream() => return End::Closed,
            Err(e) => return End::Broken(e),
        };
        if let Some(target) = &lane.target
            && target.take(&lane.writer, id, &payload)
        {
            continue;
        }
        let mut down = shared.down();
        if down.active != lane.id {
            continue;
        }
        let payload = if down.phase == Phase::Play
            && !down.swap.is_identity()
            && meridian_rewrite::clientbound_has_ids(id)
        {
            rewrite(down.swap.clientbound(id, payload.clone()), payload)
        } else {
            payload
        };
        encode(&mut body, id, &payload);
        if down.writer.send(&body).is_err() {
            return End::Closed;
        }
        add(&mut down.flow, &body);
        match down.phase {
            Phase::Configuration => {
                if down.config_digest.is_none() {
                    down.digest.add(id, &payload);
                }
                if id == ids::configuration::clientbound::FINISH_CONFIGURATION {
                    if down.config_digest.is_none() {
                        down.config_digest = Some(down.digest.finish());
                    }
                    down.phase = Phase::Play;
                }
            }
            Phase::Play => {
                if id == ids::play::clientbound::START_CONFIGURATION {
                    down.phase = Phase::Configuration;
                    continue;
                }
                follow(&mut down, id, &payload);
                let packet = Packet { id, payload };
                // A packet the ghost cannot read leaves it as it was.
                let _ = down.ghost.apply(&packet);
                if id == ids::play::clientbound::PLAYER_POSITION
                    && let Ok(play::Clientbound::PlayerPosition(p)) = packet.decode()
                {
                    shared.up().teleports.sent(p.teleport);
                }
            }
        }
    }
}

/// Notes what a switch checks from the active backend's Login and Respawn.
fn follow(down: &mut Down, id: i32, payload: &[u8]) {
    use ids::play::clientbound as c;
    if id != c::LOGIN && id != c::RESPAWN {
        return;
    }
    match play::Clientbound::decode(id, payload) {
        Ok(play::Clientbound::Login(login)) => {
            if down.secure_chat.is_none() {
                down.secure_chat = Some(login.enforces_secure_chat);
                // The first backend numbers the player as the client does.
                down.swap = Swap::new(login.player_id, login.player_id);
            }
            down.dimension = Some(login.spawn.dimension);
        }
        Ok(play::Clientbound::Respawn(respawn)) => {
            down.dimension = Some(respawn.spawn.dimension);
        }
        _ => {}
    }
}

/// Copies the client's packets to the active lane until reading or writing fails.
fn copy_to_backend(mut from: ConnReader, shared: &Shared) -> End {
    use ids::{configuration::serverbound as cfg, play::serverbound as s};
    let mut body = Vec::new();
    let mut sending = Phase::Configuration;
    let mut configured = false;
    loop {
        let (id, payload) = match from.recv() {
            Ok(packet) => packet,
            Err(e) if e.is_end_of_stream() => return End::Closed,
            Err(e) => return End::Broken(e),
        };
        if sending == Phase::Play && answer_command(id, &payload, shared) {
            continue;
        }
        let mut up = shared.up();
        let payload = match sending {
            Phase::Play => {
                if id == s::ACCEPT_TELEPORTATION
                    && let Ok(teleport) = Reader::new(&payload).varint()
                    && !up.teleports.forward(teleport)
                {
                    continue;
                }
                if up.swap.is_identity() || !meridian_rewrite::serverbound_has_ids(id) {
                    payload
                } else {
                    rewrite(up.swap.serverbound(id, payload.clone()), payload)
                }
            }
            Phase::Configuration => {
                if !configured {
                    up.client.record(id, &payload);
                }
                payload
            }
        };
        encode(&mut body, id, &payload);
        // A write fails only when the backend has gone: its lane's reader ends the
        // session, which closes the client and stops this thread.
        if lock(&up.lane.writer).send(&body).is_ok() {
            add(&mut up.flow, &body);
        }
        sending = match (sending, id) {
            (Phase::Configuration, cfg::FINISH_CONFIGURATION) => {
                configured = true;
                Phase::Play
            }
            (Phase::Play, s::CONFIGURATION_ACKNOWLEDGED) => Phase::Configuration,
            (phase, _) => phase,
        };
    }
}

/// A rewritten payload, or the packet as it was when it could not be read: the
/// backend or the client judges it.
fn rewrite(rewritten: horizon_protocol::Result<Vec<u8>>, original: Vec<u8>) -> Vec<u8> {
    rewritten.unwrap_or(original)
}

/// When the play packet `id` is a command the session's hook answers, sends the
/// player the answer and returns `true`.
fn answer_command(id: i32, payload: &[u8], shared: &Shared) -> bool {
    if id != ids::play::serverbound::CHAT_COMMAND
        && id != ids::play::serverbound::CHAT_COMMAND_SIGNED
    {
        return false;
    }
    let Some(hook) = lock(&shared.commands).clone() else {
        return false;
    };
    let command = match play::Serverbound::decode(id, payload) {
        Ok(play::Serverbound::ChatCommand(c)) => c.command,
        Ok(play::Serverbound::ChatCommandSigned(c)) => c.command,
        _ => return false,
    };
    let Some(reply) = hook(&command) else {
        return false;
    };
    let mut down = shared.down();
    if down.phase != Phase::Play {
        return false;
    }
    let message = play::Clientbound::SystemChat(play::SystemChat {
        content: text::literal(&reply),
        overlay: false,
    });
    let _ = down.writer.send(&message.encode());
    true
}

pub(crate) fn encode(body: &mut Vec<u8>, id: i32, payload: &[u8]) {
    body.clear();
    write_varint(body, id);
    body.extend_from_slice(payload);
}

pub(crate) fn add(flow: &mut Flow, body: &[u8]) {
    flow.packets += 1;
    flow.bytes += body.len() as u64;
}
