//! One bot of the swarm: a player that logs in offline, goes through the configuration phase,
//! plays what the run asks (nothing but keep-alives, or moving and probing the latency), and
//! asks to be moved to another server when it is told to.
//!
//! A bot does what a vanilla client does that the proxy can see: it answers the keep-alives,
//! confirms every teleport (a switch ends with one), tells the server it received a chunk batch,
//! and reads everything it is sent. It decodes only the few packets it must answer.
use super::world::Rng;
use crate::echo;
use crate::player::{Failure, classify};
use horizon_protocol::connection::{ConnReader, ConnWriter, Connection};
use horizon_protocol::crypto::offline_uuid;
use horizon_protocol::ids::configuration as cfg;
use horizon_protocol::ids::play::{clientbound as pc, serverbound as ps};
use horizon_protocol::packets::play::{self, Clientbound as C};
use horizon_protocol::text;
use meridian_testkit::backend::acknowledge_configuration;
use meridian_testkit::body;
use meridian_testkit::client::FakeClient;
use meridian_testkit::trace::Recorder;
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long a connection or a login may take before the bot gives up.
const PATIENCE: Duration = Duration::from_secs(30);

/// How long a bot's read waits before it looks at whether the run is ending.
const TICK: Duration = Duration::from_secs(1);

/// The most waves of bots a run may have.
pub const MAX_WAVES: usize = 8;

/// What a bot does once it is in the world.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Nothing, and the keep-alives.
    Idle,
    /// Moves, sends a measuring packet now and then.
    Active,
}

/// What one bot is told.
#[derive(Clone, Debug)]
pub struct Plan {
    pub address: SocketAddr,
    pub name: String,
    /// The host written in the handshake: the proxy's routes read it.
    pub host: String,
    pub role: Role,
    /// Movement packets a second.
    pub up_rate: u32,
    /// Measuring packets a second.
    pub probe_rate: f64,
    /// Whether this bot asks to be moved when `Live::move_now` is set, and where to.
    pub moves_to: Option<String>,
    /// The wave it arrives in: what it counts, it counts for that wave too.
    pub wave: usize,
}

/// What every bot adds to, read once a second by the run.
#[derive(Default)]
pub struct Live {
    /// Bots in the play phase now.
    pub connected: AtomicI64,
    /// Bots that have received their world.
    pub joined: AtomicU64,
    pub logins: AtomicU64,
    pub failed: AtomicU64,
    pub exhausted: AtomicU64,
    /// Bots whose connection ended while the run still wanted them.
    pub dropped: AtomicU64,
    pub login_times: Mutex<Vec<Duration>>,
    /// The same, for each wave of bots: logged in, failed, in the world, cut off, and the times of
    /// their logins.
    pub wave_logins: [AtomicU64; MAX_WAVES],
    pub wave_failed: [AtomicU64; MAX_WAVES],
    pub wave_joined: [AtomicU64; MAX_WAVES],
    pub wave_dropped: [AtomicU64; MAX_WAVES],
    pub wave_login_times: Mutex<Vec<Vec<Duration>>>,
    /// While this is set, every round trip is kept with the moment it was read (milliseconds
    /// since the epoch) and the wave of the bot, whether or not a window is being measured.
    pub timed: AtomicBool,
    pub timed_round_trips: Mutex<Vec<(u64, usize, u32)>>,
    /// The last status line the sampler wrote, for a failure to show what the run was doing.
    pub status: Mutex<String>,
    /// Only while this is set are round trips kept.
    pub measuring: AtomicBool,
    /// Round trips in microseconds: all of the measured window, and those since the last
    /// reading of the status line.
    pub round_trips: Mutex<Vec<u32>>,
    pub recent_round_trips: Mutex<Vec<u32>>,
    pub down_packets: AtomicU64,
    pub down_bytes: AtomicU64,
    pub up_packets: AtomicU64,
    pub up_bytes: AtomicU64,
    pub keepalives: AtomicU64,
    /// Set to ask the bots that have somewhere to go to move.
    pub move_now: AtomicBool,
    /// How long each move took, as the player saw it: from the command to the proxy's line.
    pub moves: Mutex<Vec<Duration>>,
    pub moves_failed: AtomicU64,
    /// The first few reasons a bot failed.
    pub failures: Mutex<Vec<String>>,
    /// Set once the run is ending the bots: connections that close then are not dropped ones.
    ending: AtomicBool,
    /// A way to close each bot's connection from outside, so that the run can end them.
    closers: Mutex<Vec<horizon_protocol::connection::Closer>>,
}

impl Live {
    /// Closes every bot's connection: the bots' reads fail and they end.
    pub fn close_all(&self) {
        self.ending.store(true, Ordering::Relaxed);
        for closer in self.closers.lock().unwrap().drain(..) {
            closer.close();
        }
    }

    /// A bot whose thread could not start counts as failed.
    pub fn fail_to_start(&self, wave: usize, why: &str) {
        self.fail(
            wave,
            Failure::Other(format!("the bot's thread did not start: {why}")),
        );
    }

    /// A login that took `took`, for a bot of this wave.
    fn logged_in(&self, wave: usize, took: Duration) {
        self.login_times.lock().unwrap().push(took);
        let mut waves = self.wave_login_times.lock().unwrap();
        if waves.len() <= wave {
            waves.resize(wave + 1, Vec::new());
        }
        waves[wave].push(took);
        self.wave_logins[wave.min(MAX_WAVES - 1)].fetch_add(1, Ordering::Relaxed);
    }

    fn fail(&self, wave: usize, failure: Failure) {
        self.failed.fetch_add(1, Ordering::Relaxed);
        self.wave_failed[wave.min(MAX_WAVES - 1)].fetch_add(1, Ordering::Relaxed);
        let text = match failure {
            Failure::Exhausted(why) => {
                self.exhausted.fetch_add(1, Ordering::Relaxed);
                format!("out of ports or descriptors: {why}")
            }
            Failure::Other(why) => why,
        };
        let mut failures = self.failures.lock().unwrap();
        if failures.len() < 5 {
            failures.push(text);
        }
    }
}

/// A bot that connects, stays `hold`, and leaves: one turn of the churn.
pub fn visit(plan: &Plan, live: &Arc<Live>, stop: &AtomicBool, hold: Duration) {
    let began = Instant::now();
    let connection = match login(plan) {
        Ok(connection) => connection,
        Err(failure) => {
            live.fail(plan.wave, failure);
            return;
        }
    };
    live.logged_in(plan.wave, began.elapsed());
    live.logins.fetch_add(1, Ordering::Relaxed);
    // The bot reads what it is sent while it stays, so that it does not close on unread data,
    // which the system answers with a reset.
    let mut connection = connection;
    let leaves = Instant::now() + hold;
    while !stop.load(Ordering::Relaxed) {
        let Some(left) = leaves.checked_duration_since(Instant::now()) else {
            break;
        };
        let _ = connection
            .reader
            .set_read_timeout(Some(left.max(Duration::from_millis(1))));
        if connection.recv().is_err() {
            break;
        }
    }
    drop(connection);
}

/// Logs the bot in and plays until `stop` is set or the connection ends. Returns when it leaves.
pub fn run(plan: &Plan, live: &Arc<Live>, stop: &Arc<AtomicBool>) {
    let began = Instant::now();
    let connection = match login(plan) {
        Ok(connection) => connection,
        Err(failure) => {
            live.fail(plan.wave, failure);
            return;
        }
    };
    live.logged_in(plan.wave, began.elapsed());
    live.logins.fetch_add(1, Ordering::Relaxed);
    live.connected.fetch_add(1, Ordering::Relaxed);
    // The read wakes once a second to see whether the run is ending: closing the connection from
    // another thread does not always wake a read that waits on it (on Windows, now and then one
    // stays blocked for good).
    let _ = connection.reader.set_read_timeout(Some(TICK));
    let (reader, writer) = connection.split();
    if let Ok(closer) = reader.closer() {
        live.closers.lock().unwrap().push(closer);
    }
    let writer = Arc::new(Mutex::new(writer));
    let state = Arc::new(State {
        wave: plan.wave.min(MAX_WAVES - 1),
        ..State::default()
    });
    let acting = match plan.role {
        Role::Idle => None,
        Role::Active => {
            let (plan, live, stop, writer, state) = (
                plan.clone(),
                Arc::clone(live),
                Arc::clone(stop),
                Arc::clone(&writer),
                Arc::clone(&state),
            );
            std::thread::Builder::new()
                .name("load-bot-act".into())
                .stack_size(192 * 1024)
                .spawn(move || act(&plan, &live, &stop, &writer, &state))
                .ok()
        }
    };
    let ended = listen(reader, &writer, live, stop, &state);
    state.over.store(true, Ordering::Relaxed);
    live.connected.fetch_sub(1, Ordering::Relaxed);
    if !stop.load(Ordering::Relaxed) && !live.ending.load(Ordering::Relaxed) && ended.is_err() {
        live.dropped.fetch_add(1, Ordering::Relaxed);
        live.wave_dropped[state.wave].fetch_add(1, Ordering::Relaxed);
    }
    if let Some(thread) = acting {
        let _ = thread.join();
    }
}

fn login(plan: &Plan) -> Result<Connection, Failure> {
    let stream = TcpStream::connect_timeout(&plan.address, PATIENCE).map_err(classify)?;
    let _ = stream.set_nodelay(true);
    let mut connection =
        Connection::from_stream(stream).map_err(|e| Failure::Other(e.to_string()))?;
    FakeClient {
        name: plan.name.clone(),
        uuid: offline_uuid(&plan.name),
        host: plan.host.clone(),
        port: plan.address.port(),
        after_login: Vec::new(),
    }
    .run(&mut connection, &Recorder::new())
    .map_err(Failure::Other)?;
    connection
        .reader
        .set_read_timeout(Some(PATIENCE))
        .map_err(|e| Failure::Other(e.to_string()))?;
    // The configuration phase: whatever the server sends, until it ends it.
    loop {
        let (id, _) = connection
            .recv()
            .map_err(|e| Failure::Other(format!("in the configuration phase: {e}")))?;
        if id == cfg::clientbound::FINISH_CONFIGURATION {
            break;
        }
    }
    connection
        .send(&match acknowledge_configuration() {
            meridian_testkit::peer::Step::Send { id, payload } => body(id, &payload),
            _ => unreachable!("an acknowledgement is a packet to send"),
        })
        .map_err(|e| Failure::Other(e.to_string()))?;
    Ok(connection)
}

/// What the two threads of a bot share.
#[derive(Default)]
struct State {
    /// The wave this bot arrived in.
    wave: usize,
    over: AtomicBool,
    /// The measuring packet in flight: its number, and when it left, in nanoseconds since
    /// `began`.
    probe_seq: AtomicU64,
    probe_sent: AtomicU64,
    /// When the bot asked to be moved, in nanoseconds since `began` plus one; 0 for never.
    asked: AtomicU64,
}

/// What the bot sends: movement, measuring packets, the command to move.
fn act(plan: &Plan, live: &Live, stop: &AtomicBool, writer: &Mutex<ConnWriter>, state: &State) {
    let mut rng = Rng::new(offline_uuid(&plan.name) as u64);
    let began = epoch();
    let up_every =
        (plan.up_rate > 0).then(|| Duration::from_secs_f64(1.0 / f64::from(plan.up_rate)));
    let probe_every =
        (plan.probe_rate > 0.0).then(|| Duration::from_secs_f64(1.0 / plan.probe_rate));
    let start = Instant::now();
    let phase = |every: Option<Duration>, rng: &mut Rng| {
        every.map(|e| start + Duration::from_nanos(rng.next() % e.as_nanos().max(1) as u64))
    };
    let (mut next_up, mut next_probe) = (phase(up_every, &mut rng), phase(probe_every, &mut rng));
    let (mut seq, mut x, mut asked) = (0u64, 0.0f64, false);
    while !stop.load(Ordering::Relaxed) && !state.over.load(Ordering::Relaxed) {
        let now = Instant::now();
        let due = [next_up, next_probe].into_iter().flatten().min();
        let wake = due.map_or(now + Duration::from_millis(50), |d| {
            d.min(now + Duration::from_millis(50))
        });
        if wake > now {
            std::thread::sleep(wake - now);
        }
        let now = Instant::now();
        let mut sends: Vec<Vec<u8>> = Vec::new();
        if let (Some(at), Some(every)) = (next_up, up_every)
            && at <= now
        {
            x += 0.2;
            sends.push(
                play::Serverbound::MovePlayerPos(play::MovePlayerPos {
                    x,
                    y: 70.0,
                    z: -x,
                    flags: 1,
                })
                .encode(),
            );
            next_up = Some(at + every);
        }
        if let (Some(at), Some(every)) = (next_probe, probe_every)
            && at <= now
        {
            seq += 1;
            state.probe_seq.store(seq, Ordering::Relaxed);
            state
                .probe_sent
                .store(began.elapsed().as_nanos() as u64, Ordering::Relaxed);
            sends.push(body(ps::CUSTOM_PAYLOAD, &echo::payload(seq, 64)));
            next_probe = Some(at + every);
        }
        if !asked
            && live.move_now.load(Ordering::Relaxed)
            && let Some(target) = &plan.moves_to
        {
            asked = true;
            state
                .asked
                .store(began.elapsed().as_nanos() as u64 + 1, Ordering::Relaxed);
            sends.push(
                play::Serverbound::ChatCommand(play::ChatCommand {
                    command: format!("server {target}"),
                })
                .encode(),
            );
        }
        if sends.is_empty() {
            continue;
        }
        let Ok(mut writer) = writer.lock() else {
            return;
        };
        for packet in sends {
            if writer.send(&packet).is_err() {
                return;
            }
            live.up_packets.fetch_add(1, Ordering::Relaxed);
            live.up_bytes
                .fetch_add(packet.len() as u64, Ordering::Relaxed);
        }
    }
}

/// The instant the bots count from; the same for every bot of the process.
fn epoch() -> Instant {
    use std::sync::OnceLock;
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

/// Reads what the server sends until the connection ends or the run stops.
fn listen(
    mut reader: ConnReader,
    writer: &Mutex<ConnWriter>,
    live: &Live,
    stop: &AtomicBool,
    state: &State,
) -> Result<(), String> {
    let began = epoch();
    let mut joined = false;
    let send = |packet: Vec<u8>| -> Result<(), String> {
        writer
            .lock()
            .map_err(|_| "poisoned".to_owned())?
            .send(&packet)
            .map_err(|e| e.to_string())
    };
    // The read blocks for as long as the server is quiet, a second at a time; the run ends it by
    // closing the connection ([`Live::close_all`]), and if that does not wake the read, the
    // next second's end of wait finds `ending`.
    loop {
        if stop.load(Ordering::Relaxed) || live.ending.load(Ordering::Relaxed) {
            return Ok(());
        }
        let waited = Instant::now();
        let (id, payload) = match reader.recv() {
            Ok(packet) => packet,
            // A read that ran its whole second is the wait ending, not the connection: what has
            // been read stays in the reader, and the next read goes on from there.
            Err(_) if waited.elapsed() >= TICK - Duration::from_millis(100) => continue,
            Err(e) => return Err(e.to_string()),
        };
        live.down_packets.fetch_add(1, Ordering::Relaxed);
        live.down_bytes
            .fetch_add(payload.len() as u64 + 1, Ordering::Relaxed);
        match id {
            pc::KEEP_ALIVE => {
                live.keepalives.fetch_add(1, Ordering::Relaxed);
                send(body(ps::KEEP_ALIVE, &payload))?;
            }
            pc::PLAYER_POSITION => {
                // Every teleport is confirmed, as a client does; the first one ends the join.
                if let Ok(C::PlayerPosition(p)) = C::decode(id, &payload) {
                    send(
                        play::Serverbound::AcceptTeleportation(play::AcceptTeleportation {
                            id: p.teleport,
                            x: p.position[0],
                            y: p.position[1],
                            z: p.position[2],
                            yaw: p.yaw,
                            pitch: p.pitch,
                        })
                        .encode(),
                    )?;
                }
                if !joined {
                    joined = true;
                    live.joined.fetch_add(1, Ordering::Relaxed);
                    live.wave_joined[state.wave].fetch_add(1, Ordering::Relaxed);
                }
            }
            pc::CHUNK_BATCH_FINISHED => {
                send(body(ps::CHUNK_BATCH_RECEIVED, &9.0f32.to_be_bytes()))?;
            }
            pc::CUSTOM_PAYLOAD => {
                if let Some(seq) = echo::seq(&payload)
                    && seq == state.probe_seq.load(Ordering::Relaxed)
                {
                    let measuring = live.measuring.load(Ordering::Relaxed);
                    let timed = live.timed.load(Ordering::Relaxed);
                    if measuring || timed {
                        let sent = state.probe_sent.load(Ordering::Relaxed);
                        let now = began.elapsed().as_nanos() as u64;
                        let micros =
                            (now.saturating_sub(sent) / 1_000).min(u64::from(u32::MAX)) as u32;
                        if measuring {
                            live.round_trips.lock().unwrap().push(micros);
                        }
                        live.recent_round_trips.lock().unwrap().push(micros);
                        if timed {
                            live.timed_round_trips.lock().unwrap().push((
                                super::remote::Windows::now_ms(),
                                state.wave,
                                micros,
                            ));
                        }
                    }
                }
            }
            pc::SYSTEM_CHAT => {
                let asked = state.asked.load(Ordering::Relaxed);
                if asked != 0
                    && let Ok(C::SystemChat(chat)) = C::decode(id, &payload)
                {
                    let said = text::plain(&chat.content, &Default::default());
                    if said.starts_with("You are now on ") {
                        let took = began.elapsed().as_nanos() as u64 - (asked - 1);
                        live.moves.lock().unwrap().push(Duration::from_nanos(took));
                        state.asked.store(0, Ordering::Relaxed);
                    } else if said.starts_with("Could not move you") {
                        live.moves_failed.fetch_add(1, Ordering::Relaxed);
                        state.asked.store(0, Ordering::Relaxed);
                    }
                }
            }
            pc::DISCONNECT => return Err("the server disconnected the bot".into()),
            _ => {}
        }
    }
}
