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
//! # When the backend goes away
//!
//! A session started with [`Session::run_with`] can keep the player when the active
//! backend is lost without the proxy having wanted it: the connection closed or broke,
//! or the backend disconnected the player in the play phase. The backend's own
//! disconnect is not passed on. The session asks for a [`Replacement`] and moves the
//! client the way a backend itself returns it to the configuration phase: it sends
//! Start Configuration, drops what the client sends until it acknowledges, and then
//! the replacement is the active lane and its configuration phase reaches the client as
//! the first backend's did, with a loading screen. Nothing is exported: the lost
//! backend is gone, and the player arrives on the replacement as a new arrival.
//! The ghost, the numbering and the configuration digest start again from the
//! replacement's packets.
//!
//! The client is not moved when the proxy ended the session itself ([`SessionHandle::close`]
//! and [`SessionHandle::disconnect`]), when the client left, when a switch is under way, or
//! when the client is in the configuration phase, where it cannot be taken back to the start.
//!
//! The locks are always taken in the same order, so that none waits on another
//! in a circle: a target's prepare state, then what goes to the client ([`Down`]),
//! then what goes to the backend ([`Up`]), then a lane's writer.
use crate::{
    configure::{ClientConfiguration, Digest, DigestBuilder, Summary},
    prepare::{Pose, Target},
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
use std::time::{Duration, Instant};

/// How long the client has to acknowledge that it goes back to the configuration phase,
/// when the player is moved off a backend that was lost.
#[cfg(not(test))]
const ACKNOWLEDGE_WITHIN: Duration = Duration::from_secs(15);
#[cfg(test)]
const ACKNOWLEDGE_WITHIN: Duration = Duration::from_millis(400);

/// What a session does when the backend a player is on is lost.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Fallback {
    /// Move the player when the backend disconnects them.
    pub on_kick: bool,
    /// Move the player when the connection to the backend closes or breaks.
    pub on_crash: bool,
}

/// How the player's backend was lost.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Loss {
    /// The connection closed or broke, for this reason.
    Crashed(String),
    /// The backend disconnected the player, with this reason as plain text.
    Kicked(String),
}

/// A backend to move the player to, already logged in and in its configuration phase.
pub struct Replacement {
    pub link: BackendLink,
    /// Its name.
    pub backend: String,
    /// Shown to the player in the chat once they are in the replacement's world.
    pub notice: Option<String>,
    /// Held until the player is on the replacement, then dropped: a place reserved on it.
    pub keep: Option<Box<dyn std::any::Any + Send>>,
}

/// A replacement waiting for the client to acknowledge.
struct Pending {
    lane: Arc<Lane>,
    reader: ConnReader,
    backend: String,
    notice: Option<String>,
    keep: Option<Box<dyn std::any::Any + Send>>,
}

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
    /// The active backend's name, as in [`Down::backend`], under a lock of its own: the
    /// reader of the active lane holds `down` while it writes to the client, which waits
    /// as long as the client does not read (a busy link), and what only asks where the
    /// player is (the proxy's counts, its metrics page, a login placing another player)
    /// must not wait for that. Changed only with `down` held, after `down.backend`.
    backend: Mutex<String>,
    /// For tests: when set, a move off a lost backend waits on the receiver once the client
    /// has been told to go back to the configuration phase, before it closes the lost
    /// connection, and says on the sender when it has closed it, so that a test can let the
    /// client acknowledge in between.
    #[cfg(test)]
    pub(crate) moving_gate: Mutex<Option<(mpsc::Receiver<()>, mpsc::Sender<()>)>>,
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
    /// The proxy ended the session ([`SessionHandle::close`], [`SessionHandle::disconnect`]).
    closing: AtomicBool,
    /// What to do when the backend is lost, from [`Session::run_with`].
    fallback_kick: AtomicBool,
    fallback_crash: AtomicBool,
    /// The player is being moved off a lost backend: what the client sends is dropped
    /// until it acknowledges.
    reconnecting: AtomicBool,
    pending: Mutex<Option<Pending>>,
    /// The backend's disconnect was passed on to the client: the close that follows it is
    /// the end the backend meant, not a loss.
    disconnected: AtomicBool,
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
    /// The same configuration, as the log of a refused switch describes it.
    pub config_summary: Option<Summary>,
    /// Packets the ghost could not read, for the log of the next switch.
    pub unreadable: Vec<String>,
    pub flow: Flow,
    /// While the reader thread answers a command the player typed: its id, and the messages other
    /// threads told the player meanwhile, written after the answer (see [`Answering`]).
    pub answering: Option<(std::thread::ThreadId, Vec<Vec<u8>>)>,
    /// Said in the chat once the next Login arrives: why the player was moved.
    notice: Option<String>,
}

/// What goes to the backend.
pub(crate) struct Up {
    pub lane: Arc<Lane>,
    pub swap: Swap,
    pub teleports: Teleports,
    /// What the client sent on its own in its configuration phase.
    pub client: ClientConfiguration,
    /// Where the client last said it stands and how it looks, from its moves.
    pub client_pose: Option<Pose>,
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
    /// The client acknowledged, and the replacement is the active lane.
    Resumed,
}

/// How a copy stopped.
pub(crate) enum End {
    /// The side read from closed, or the side written to did.
    Closed,
    /// The side read from sent something that could not be read.
    Broken(horizon_protocol::Error),
    /// The client could not be written to.
    ClientGone,
    /// The backend disconnected the player in the play phase, for this reason (plain
    /// text). Its packet was not passed to the client.
    Kicked(String),
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
                config_summary: None,
                unreadable: Vec::new(),
                flow: Flow::default(),
                answering: None,
                notice: None,
            }),
            up: Mutex::new(Up {
                lane: Arc::clone(&lane),
                swap: Swap::new(0, 0),
                teleports: Teleports::default(),
                client: ClientConfiguration::default(),
                client_pose: None,
                flow: Flow::default(),
            }),
            client_closer,
            commands: Mutex::new(None),
            events: Mutex::new(None),
            next_lane: AtomicU64::new(1),
            switching: AtomicBool::new(false),
            client_gone: AtomicBool::new(false),
            ended: AtomicBool::new(false),
            closing: AtomicBool::new(false),
            fallback_kick: AtomicBool::new(false),
            fallback_crash: AtomicBool::new(false),
            reconnecting: AtomicBool::new(false),
            pending: Mutex::new(None),
            disconnected: AtomicBool::new(false),
            backend: Mutex::new(String::new()),
            #[cfg(test)]
            moving_gate: Mutex::new(None),
        });
        let handle = SessionHandle(Arc::clone(&shared));
        *lock(&shared.backend) = name.to_owned();
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
        self.run_with(Fallback::default(), &mut |_| Err(String::new()))
    }

    /// As [`Session::run`], and when the active backend is lost in the way `fallback`
    /// names, `replace` is asked for another to move the player to (see the module
    /// documentation). `replace` runs on the thread that calls this. It answers the
    /// replacement, or the message that is shown to the player when there is none, before
    /// they are disconnected.
    pub fn run_with(
        self,
        fallback: Fallback,
        replace: &mut dyn FnMut(&Loss) -> std::result::Result<Replacement, String>,
    ) -> Result<Report> {
        let Session {
            shared,
            client_reader,
            first: (lane, backend_reader),
        } = self;
        shared
            .fallback_kick
            .store(fallback.on_kick, Ordering::SeqCst);
        shared
            .fallback_crash
            .store(fallback.on_crash, Ordering::SeqCst);
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
        // When the client must have acknowledged, while a player is being moved.
        let mut deadline: Option<Instant> = None;
        loop {
            let event = match deadline {
                None => match ended.recv() {
                    Ok(event) => event,
                    Err(_) => break,
                },
                Some(by) => {
                    match ended.recv_timeout(by.saturating_duration_since(Instant::now())) {
                        Ok(event) => event,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            give_up_moving(
                                &shared,
                                "The server you were on is gone, and your game did not follow.",
                            );
                            first = Some(End::Closed);
                            break;
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            };
            match event {
                Event::Client(end) => {
                    shared.client_gone.store(true, Ordering::SeqCst);
                    first = Some(end);
                    break;
                }
                Event::Resumed => deadline = None,
                Event::Lane(id, end) if id == shared.down().active => {
                    match lost(&shared, end, replace) {
                        Lost::Over(end) => {
                            first = Some(end);
                            break;
                        }
                        Lost::Moving => deadline = Some(Instant::now() + ACKNOWLEDGE_WITHIN),
                    }
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
        // A replacement the client never took.
        if let Some(pending) = lock(&shared.pending).take() {
            pending.lane.closer.close();
        }
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
            Some(End::Broken(e)) => Err(e.into()),
            _ => Ok(report),
        }
    }
}

/// What became of a lost backend.
enum Lost {
    /// The session ends, in this way.
    Over(End),
    /// The client was told to go back to the configuration phase, and a replacement waits
    /// for its acknowledgement.
    Moving,
}

/// Decides what to do when the active backend's lane ended in `end`: a replacement, or the
/// end of the session.
fn lost(
    shared: &Arc<Shared>,
    end: End,
    replace: &mut dyn FnMut(&Loss) -> std::result::Result<Replacement, String>,
) -> Lost {
    let loss = match &end {
        End::Kicked(reason) => Loss::Kicked(reason.clone()),
        End::Closed if shared.fallback_crash.load(Ordering::SeqCst) => {
            Loss::Crashed("the connection closed".to_owned())
        }
        End::Broken(e) if shared.fallback_crash.load(Ordering::SeqCst) => {
            Loss::Crashed(e.to_string())
        }
        _ => return Lost::Over(end),
    };
    // The proxy ended it, the client left or a switch is under way: not a loss to make good.
    let wanted_end = shared.closing.load(Ordering::SeqCst)
        || shared.disconnected.load(Ordering::SeqCst)
        || shared.client_gone.load(Ordering::SeqCst)
        || shared.switching.load(Ordering::SeqCst);
    let play = shared.down().phase == Phase::Play;
    if wanted_end || !play {
        // A kick that was held back is shown now, as the backend sent it.
        if let End::Kicked(reason) = &end {
            say_goodbye(shared, reason);
        }
        return Lost::Over(match end {
            End::Kicked(_) => End::Closed,
            other => other,
        });
    }
    match replace(&loss) {
        Err(message) => {
            say_goodbye(shared, &message);
            Lost::Over(End::Closed)
        }
        Ok(replacement) => match begin_moving(shared, replacement) {
            Ok(()) => Lost::Moving,
            Err(e) => {
                say_goodbye(
                    shared,
                    "The server you were on is gone, and no other could take you.",
                );
                Lost::Over(e)
            }
        },
    }
}

/// Disconnects the player with `message`, in the phase they are in.
fn say_goodbye(shared: &Shared, message: &str) {
    let reason = configuration::Disconnect {
        reason: text::literal(message),
    };
    let mut down = shared.down();
    let body = match down.phase {
        Phase::Configuration => configuration::Clientbound::Disconnect(reason).encode(),
        Phase::Play => play::Clientbound::Disconnect(reason).encode(),
    };
    let _ = down.writer.send(&body);
}

/// Ends the wait for an acknowledgement that did not come.
fn give_up_moving(shared: &Shared, message: &str) {
    shared.reconnecting.store(false, Ordering::SeqCst);
    if let Some(pending) = lock(&shared.pending).take() {
        pending.lane.closer.close();
    }
    say_goodbye(shared, message);
}

/// Sends the client back to the configuration phase and keeps `replacement` for when it
/// acknowledges.
fn begin_moving(shared: &Arc<Shared>, replacement: Replacement) -> std::result::Result<(), End> {
    let Replacement {
        link,
        backend,
        notice,
        keep,
    } = replacement;
    let (reader, writer) = link.connection.split();
    let closer = reader
        .closer()
        .map_err(|e| End::Broken(horizon_protocol::Error::new(e.to_string())))?;
    let lane = Arc::new(Lane {
        id: shared.lane_id(),
        writer: Mutex::new(writer),
        closer,
        target: None,
    });
    // The lost backend's lane, taken while it is still the active one: once the client is
    // told to go back to the configuration phase, it may acknowledge, and the replacement
    // become the active lane, before this thread closes the lost one.
    let lost = Arc::clone(&shared.up().lane);
    *lock(&shared.pending) = Some(Pending {
        lane,
        reader,
        backend,
        notice,
        keep,
    });
    // Before the packet, so that the client's acknowledgement finds it set.
    shared.reconnecting.store(true, Ordering::SeqCst);
    {
        let mut down = shared.down();
        let mut body = Vec::new();
        encode(&mut body, ids::play::clientbound::START_CONFIGURATION, &[]);
        if down.writer.send(&body).is_err() {
            shared.reconnecting.store(false, Ordering::SeqCst);
            if let Some(pending) = lock(&shared.pending).take() {
                pending.lane.closer.close();
            }
            return Err(End::ClientGone);
        }
        down.phase = Phase::Configuration;
    }
    #[cfg(test)]
    let gate = lock(&shared.moving_gate).take();
    #[cfg(test)]
    if let Some((open, _)) = &gate {
        let _ = open.recv_timeout(Duration::from_secs(5));
    }
    // The lost backend's connection is of no use any more. Not `shared.up().lane`: that may
    // be the replacement by now.
    lost.closer.close();
    #[cfg(test)]
    if let Some((_, closed)) = gate {
        let _ = closed.send(());
    }
    Ok(())
}

/// The client acknowledged: the replacement becomes the active lane, and everything that
/// described the first backend's session starts again from it.
fn activate(shared: &Arc<Shared>) {
    let Some(pending) = lock(&shared.pending).take() else {
        shared.reconnecting.store(false, Ordering::SeqCst);
        return;
    };
    let Pending {
        lane,
        reader,
        backend,
        notice,
        keep,
    } = pending;
    {
        let mut down = shared.down();
        let mut up = shared.up();
        down.active = lane.id;
        *lock(&shared.backend) = backend.clone();
        down.backend = backend;
        down.ghost = View::ghost();
        down.swap = Swap::new(0, 0);
        down.dimension = None;
        down.secure_chat = None;
        down.digest = DigestBuilder::default();
        down.config_digest = None;
        down.config_summary = None;
        down.unreadable.clear();
        down.notice = notice;
        up.lane = Arc::clone(&lane);
        up.swap = Swap::new(0, 0);
        up.teleports = Teleports::default();
        up.client_pose = None;
    }
    shared.disconnected.store(false, Ordering::SeqCst);
    shared.reconnecting.store(false, Ordering::SeqCst);
    if spawn_lane(shared, Arc::clone(&lane), reader).is_err() {
        lane.closer.close();
    }
    drop(keep);
    if let Some(events) = lock(&shared.events).clone() {
        let _ = events.send(Event::Resumed);
    }
}

impl SessionHandle {
    /// The phase the player's client is in, as far as the session has sent it.
    pub fn phase(&self) -> Phase {
        self.0.down().phase
    }

    /// The name of the backend the player is on. It never waits for the relay's
    /// writes to the client (see `Shared::backend`).
    pub fn backend(&self) -> String {
        lock(&self.0.backend).clone()
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
        })
        .encode();
        // A command is being answered: what another thread says waits for the answer, so that
        // "Sending..." is always read before "Sent." (the thread of a move can end first).
        if let Some((reader, waiting)) = down.answering.as_mut()
            && *reader != std::thread::current().id()
        {
            waiting.push(packet);
            return;
        }
        let _ = down.writer.send(&packet);
    }

    /// Whether a switch of this player is under way.
    pub fn is_switching(&self) -> bool {
        self.0.switching.load(Ordering::SeqCst)
    }

    /// Whether the session is over for the client: it left, or was closed.
    pub fn is_over(&self) -> bool {
        self.0.client_gone.load(Ordering::SeqCst)
    }

    /// Tells the player why the session ends, in the phase their client reads,
    /// then closes it.
    pub fn disconnect(&self, reason: configuration::Disconnect) {
        self.0.closing.store(true, Ordering::SeqCst);
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
        self.0.closing.store(true, Ordering::SeqCst);
        self.0.client_closer.close();
        self.0.up().lane.closer.close();
    }
}

impl Shared {
    pub fn down(&self) -> MutexGuard<'_, Down> {
        lock(&self.down)
    }

    /// Notes that the player is now on `backend`; called with `down` held, once
    /// `down.backend` is set.
    pub fn set_backend(&self, backend: &str) {
        *lock(&self.backend) = backend.to_owned();
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
                target.closed(match &end {
                    End::Broken(e) => Some(e),
                    End::Closed | End::ClientGone | End::Kicked(_) => None,
                });
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
        // The backend disconnects the player: with the fallback on, the player is moved
        // instead, and is not shown the disconnect.
        if down.phase == Phase::Play
            && id == ids::play::clientbound::DISCONNECT
            && shared.fallback_kick.load(Ordering::SeqCst)
            && !shared.closing.load(Ordering::SeqCst)
            && !shared.switching.load(Ordering::SeqCst)
            && !shared.client_gone.load(Ordering::SeqCst)
            && let Ok(play::Clientbound::Disconnect(d)) = play::Clientbound::decode(id, &payload)
        {
            return End::Kicked(text::plain(&d.reason, &Default::default()));
        }
        if id == ids::play::clientbound::DISCONNECT && down.phase == Phase::Play {
            shared.disconnected.store(true, Ordering::SeqCst);
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
            return End::ClientGone;
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
                        down.config_summary = Some(down.digest.summary());
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
                // Why the player was moved, once the new world has begun.
                if id == ids::play::clientbound::LOGIN
                    && let Some(notice) = down.notice.take()
                {
                    let chat = play::Clientbound::SystemChat(play::SystemChat {
                        content: text::literal(&notice),
                        overlay: false,
                    });
                    let _ = down.writer.send(&chat.encode());
                }
                let packet = Packet { id, payload };
                // A packet the ghost cannot read leaves it as it was; it is noted
                // for the log of the next switch.
                if let Err(e) = down.ghost.apply(&packet) {
                    crate::prepare::note_unreadable(&mut down.unreadable, id, &e);
                }
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
fn copy_to_backend(mut from: ConnReader, shared: &Arc<Shared>) -> End {
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
        // A player being moved off a lost backend: what they sent for it is dropped, up to
        // their acknowledgement, which is not passed on either: the replacement is in its
        // configuration phase already.
        if shared.reconnecting.load(Ordering::SeqCst) && sending == Phase::Play {
            if id == s::CONFIGURATION_ACKNOWLEDGED {
                sending = Phase::Configuration;
                activate(shared);
            }
            continue;
        }
        if sending == Phase::Play && answer_command(id, &payload, shared) {
            continue;
        }
        let mut up = shared.up();
        let payload = match sending {
            Phase::Play => {
                if let Some(pose) = up
                    .client_pose
                    .unwrap_or_default()
                    .sent_by_client(id, &payload)
                {
                    up.client_pose = Some(pose);
                }
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
    // From here until the answer is written, what other threads tell the player waits.
    let _answering = Answering::begin(shared);
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

/// The reader thread is answering a command. A command may start a thread that tells the player
/// how it ended (`/send`, `/server`): if that thread is quicker than the reader, its message would
/// come before the answer ("Could not move you" before "Moving you..."). So while this exists,
/// what other threads tell the player is kept, and written, in order, when it is dropped: after
/// the answer, which the reader writes before it returns. What the reader thread itself tells (an
/// alert shown to the player who typed it) is written at once, before the answer, as ever.
struct Answering<'a>(&'a Shared);

impl<'a> Answering<'a> {
    fn begin(shared: &'a Shared) -> Self {
        shared.down().answering = Some((std::thread::current().id(), Vec::new()));
        Self(shared)
    }
}

impl Drop for Answering<'_> {
    fn drop(&mut self) {
        let mut down = self.0.down();
        let Some((_, waiting)) = down.answering.take() else {
            return;
        };
        if down.phase == Phase::Play {
            for packet in waiting {
                let _ = down.writer.send(&packet);
            }
        }
    }
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
