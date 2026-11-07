//! The synthetic game server of the load runs: it takes any number of players through the
//! login as a Horizon server behind the proxy does, sends each one a world of the size the run
//! asks for, then streams to them what a busy server does (small packets, a chunk now and then,
//! keep-alives) and answers the measuring packets at once.
//!
//! It is not a game server and plays no game: it is the cheapest thing that sends the proxy the
//! traffic of one, so that a run measures the proxy. With a [`Transfers`] it also plays the part
//! of Horizon's held player, so that the proxy's switches run end to end against it.
use super::transfer::{Signal, Transfers};
use crate::echo;
use horizon_protocol::buf::write_varint;
use horizon_protocol::connection::{ConnWriter, Connection};
use horizon_protocol::ids::configuration as cfg;
use horizon_protocol::ids::play::{clientbound as pc, serverbound as ps};
use horizon_protocol::packets::play::{self, Clientbound as C};
use meridian_testkit::backend::FakeBackend;
use meridian_testkit::body;
use meridian_testkit::held::{
    CLIENT_ID, OVERWORLD, TARGET_ID, accepted, add_entity, effect, expect, login, packet, position,
    send_typed, varint,
};
use meridian_testkit::peer::Step;
use meridian_testkit::trace::Recorder;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The queue of waiting connections, as long as the proxy's (see `crate::backend`).
const BACKLOG: i32 = 4096;

/// How long a held player waits for the proxy's next step of a switch.
const HELD_WAIT: Duration = Duration::from_secs(30);

/// How big the world each player is sent is.
#[derive(Clone, Copy, Debug)]
pub struct Shape {
    /// The view distance: `(2r + 1)^2` columns are sent at the join. 0 sends none, and no
    /// entity either.
    pub view_radius: i32,
    /// Entities near the player, each one known to the proxy's ghost.
    pub entities: usize,
    /// Bytes of registry data sent in the configuration phase.
    pub registry_bytes: usize,
}

/// What the server streams to a player once they are in.
#[derive(Clone, Copy, Debug)]
pub enum Traffic {
    /// Keep-alives only.
    Idle,
    Active {
        /// Small packets a second, of 50 to 200 bytes.
        packets_per_second: u32,
        /// The share of them that change an entity the proxy's ghost knows.
        tracked: f64,
        /// Chunk packets a second, of 2 to 30 KB.
        chunks_per_second: f64,
    },
}

/// What a server is told to do.
#[derive(Clone, Debug)]
pub struct Options {
    /// The server's name, in messages.
    pub name: &'static str,
    /// The compression threshold it asks for; `None` turns compression off.
    pub compression: Option<i32>,
    /// The forwarding secret: with it the server asks for the player information.
    pub secret: Option<Vec<u8>>,
    pub shape: Shape,
    pub traffic: Traffic,
    /// How often it sends a keep-alive.
    pub keepalive: Duration,
    /// What falls due in each tick of this length is written in one write, on a clock every
    /// player shares, as a game server flushes a player's packets once a tick; `None` writes each
    /// packet when it is due.
    pub write_per_tick: Option<Duration>,
    /// 1 or 2: which world it sends, so that two servers' chunks and entities differ.
    pub flavour: u8,
    /// The hand-overs of switches, when the proxy moves players between servers.
    pub transfers: Option<Arc<Transfers>>,
    /// Where it listens: this machine's loopback and a free port, unless the proxy is on another
    /// machine and reaches the server by the network address and the port it was told.
    pub bind: SocketAddr,
}

/// What a server counted.
#[derive(Debug, Default)]
pub struct Counters {
    pub logged_in: AtomicU64,
    pub refused: AtomicU64,
    /// Players in the play phase now.
    pub online: AtomicI64,
    pub keepalives_sent: AtomicU64,
    pub keepalives_answered: AtomicU64,
    /// What the players sent, and what was sent to them, in packets and bytes before
    /// compression.
    pub up_packets: AtomicU64,
    pub up_bytes: AtomicU64,
    pub down_packets: AtomicU64,
    pub down_bytes: AtomicU64,
    /// The writes the stream to the players took: one a packet, or one a tick with
    /// `write_per_tick`.
    pub down_writes: AtomicU64,
    /// Held players placed by a switch.
    pub arrivals: AtomicU64,
}

/// A running server.
pub struct World {
    pub address: SocketAddr,
    pub counters: Arc<Counters>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// Where a server listens when nothing says otherwise.
pub fn loopback() -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, 0))
}

fn listen(bind: SocketAddr) -> io::Result<TcpListener> {
    use socket2::{Domain, Protocol, Socket, Type};
    // A server of a fixed port is started again on it, one run after another: the connections of
    // the one before may still be in TIME_WAIT, and a Unix system refuses the port for them unless
    // told otherwise (Windows does not, and there the option would let another program take it),
    // and the one before may not have let go of the port yet. So it is asked again for a moment.
    let until = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let socket = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP))?;
        #[cfg(unix)]
        socket.set_reuse_address(true)?;
        match socket.bind(&bind.into()) {
            Ok(()) => {
                socket.listen(if cfg!(windows) { -BACKLOG } else { BACKLOG })?;
                return Ok(socket.into());
            }
            Err(e) if e.kind() == io::ErrorKind::AddrInUse && std::time::Instant::now() < until => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                return Err(io::Error::new(
                    e.kind(),
                    format!("cannot listen on {bind}: {e}"),
                ));
            }
        }
    }
}

impl World {
    pub fn start(options: Options) -> io::Result<World> {
        let listener = listen(options.bind)?;
        let address = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let counters = Arc::new(Counters::default());
        let pool = Arc::new(Pool::new(options.flavour));
        let thread = {
            let (stop, counters) = (Arc::clone(&stop), Arc::clone(&counters));
            std::thread::Builder::new()
                .name(format!("load-{}", options.name))
                .spawn(move || {
                    loop {
                        let accepted = listener.accept();
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        let Ok((stream, _)) = accepted else {
                            std::thread::sleep(Duration::from_millis(1));
                            continue;
                        };
                        let (options, pool, counters) =
                            (options.clone(), Arc::clone(&pool), Arc::clone(&counters));
                        let refused = Arc::clone(&counters);
                        let spawned = std::thread::Builder::new()
                            .name(format!("load-{}-player", options.name))
                            .stack_size(384 * 1024)
                            .spawn(move || {
                                if let Err(why) = serve(stream, &options, &pool, &counters) {
                                    counters.refused.fetch_add(1, Ordering::Relaxed);
                                    if std::env::var_os("MERIDIAN_LOAD_VERBOSE").is_some() {
                                        eprintln!("load-{}: {why}", options.name);
                                    }
                                }
                            });
                        if spawned.is_err() {
                            refused.refused.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                })?
        };
        Ok(World {
            address,
            counters,
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for World {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take()
            && TcpStream::connect_timeout(&self.address, Duration::from_secs(1)).is_ok()
        {
            let _ = thread.join();
        }
    }
}

/// A small, fast generator: the load needs bytes that are not all the same, not good randomness.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1)
    }

    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// A number in `low..high`.
    pub fn between(&mut self, low: usize, high: usize) -> usize {
        low + (self.next() % (high - low).max(1) as u64) as usize
    }

    pub fn chance(&mut self, p: f64) -> bool {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64 <= p
    }
}

/// Chunk data of 2 to 30 KB that compresses about as chunks do, made once and copied.
struct Pool(Vec<Vec<u8>>);

impl Pool {
    fn new(flavour: u8) -> Pool {
        let mut rng = Rng::new(0x5eed + u64::from(flavour));
        let palette: Vec<u8> = (0..24).map(|_| rng.next() as u8).collect();
        let sizes = (0..32).map(|i| 2_048 + i * (30_720 - 2_048) / 31);
        Pool(
            sizes
                .map(|size| {
                    let mut data = Vec::with_capacity(size);
                    let mut last = palette[0];
                    while data.len() < size {
                        // Long runs of a few values, with now and then another: as blocks and
                        // light are, in a world that is mostly stone and air.
                        if rng.chance(0.2) {
                            last = palette[rng.between(0, palette.len())];
                        }
                        data.push(last);
                    }
                    data
                })
                .collect(),
        )
    }

    /// The packet of the column at `x`, `z`: its id and payload.
    fn column(&self, x: i32, z: i32, rng: &mut Rng) -> (i32, Vec<u8>) {
        let data = &self.0[rng.between(0, self.0.len())];
        let mut payload = Vec::with_capacity(8 + data.len());
        payload.extend_from_slice(&x.to_be_bytes());
        payload.extend_from_slice(&z.to_be_bytes());
        payload.extend_from_slice(data);
        (pc::LEVEL_CHUNK_WITH_LIGHT, payload)
    }
}

/// The first id of the entities of a world: the two servers number them apart, so that a
/// switch has entities to remove and to add.
fn entity_base(flavour: u8) -> i32 {
    100 * i32::from(flavour)
}

/// Sends `packets` and counts them.
fn send_all(
    connection: &mut Connection,
    counters: &Counters,
    packets: impl IntoIterator<Item = (i32, Vec<u8>)>,
) -> Result<(), String> {
    for (id, payload) in packets {
        let b = body(id, &payload);
        connection.send(&b).map_err(|e| e.to_string())?;
        counters.down_packets.fetch_add(1, Ordering::Relaxed);
        counters
            .down_bytes
            .fetch_add(b.len() as u64, Ordering::Relaxed);
    }
    Ok(())
}

/// The columns of the view, in rows.
fn view(radius: i32) -> impl Iterator<Item = (i32, i32)> {
    (-radius..=radius).flat_map(move |x| (-radius..=radius).map(move |z| (x, z)))
}

/// The join of an ordinary player: Login, the columns in a batch, the place, the entities.
fn join(
    connection: &mut Connection,
    options: &Options,
    pool: &Pool,
    counters: &Counters,
    rng: &mut Rng,
) -> Result<(), String> {
    let shape = options.shape;
    let mut first = login(CLIENT_ID, OVERWORLD);
    if let C::Login(l) = &mut first {
        l.chunk_radius = shape.view_radius.max(2);
    }
    let mut packets = vec![packet(&first)];
    if shape.view_radius > 0 {
        packets.push((pc::CHUNK_BATCH_START, Vec::new()));
        let mut count = 0;
        for (x, z) in view(shape.view_radius) {
            packets.push(pool.column(x, z, rng));
            count += 1;
        }
        packets.push((pc::CHUNK_BATCH_FINISHED, varint(count)));
    }
    packets.push(packet(&position(1, 8.0)));
    if shape.view_radius > 0 {
        for i in 0..shape.entities {
            packets.push(packet(&add_entity(
                entity_base(options.flavour) + i as i32,
                1000 + i as u128,
            )));
        }
    }
    send_all(connection, counters, packets)
}

/// What a held player does until the proxy has had the player's record imported and placed:
/// the part of Horizon's held player the proxy's switch needs, as the test kit's fake target plays
/// it (docs/server.md, "A proxy moving a player between two servers"), with a world of the run's
/// size.
fn held(
    connection: &mut Connection,
    options: &Options,
    pool: &Pool,
    counters: &Counters,
    rng: &mut Rng,
    signals: &std::sync::mpsc::Receiver<Signal>,
) -> Result<(), String> {
    let shape = options.shape;
    let mut first = login(TARGET_ID, OVERWORLD);
    if let C::Login(l) = &mut first {
        l.chunk_radius = shape.view_radius.max(2);
    }
    let mut packets = vec![packet(&first)];
    let mut count = 0;
    if shape.view_radius > 0 {
        for (x, z) in view(shape.view_radius) {
            packets.push(pool.column(x, z, rng));
            count += 1;
        }
    }
    packets.push(packet(&position(1, 40.0)));
    // The target's own entity 5 is the client's player id; the others are its world's.
    packets.push(packet(&add_entity(CLIENT_ID, 500)));
    if shape.view_radius > 0 {
        for i in 0..shape.entities {
            packets.push(packet(&add_entity(
                entity_base(options.flavour) + i as i32,
                2000 + i as u128,
            )));
        }
    }
    packets.push((pc::COMMANDS, vec![0, 0]));
    packets.push((pc::CHUNK_BATCH_FINISHED, varint(count)));
    send_all(connection, counters, packets)?;
    // The proxy answers for the client, as a client does: the first teleport, the chunk batch,
    // the end of loading.
    let step = |what: &'static str| move |e: String| format!("held player, {what}: {e}");
    if accepted(connection).map_err(step("the first teleport"))? != (1, [40.0, 70.0, 40.0]) {
        return Err("the proxy confirmed another teleport".into());
    }
    expect(connection, ps::CHUNK_BATCH_RECEIVED).map_err(step("the chunk batch"))?;
    expect(connection, ps::PLAYER_LOADED).map_err(step("the end of loading"))?;
    wait(signals, Signal::Import)?;
    // What the import changes: a position the client must confirm.
    send_typed(connection, &position(2, 41.0));
    // The proxy may answer the client's confirmation of that position itself, and the target
    // places the player when it is asked to, not when the client has answered: it waits for that.
    // What the client sends meanwhile stays unread until the player is live.
    wait(signals, Signal::Confirm)?;
    send_typed(connection, &effect(TARGET_ID));
    counters.arrivals.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

fn wait(signals: &std::sync::mpsc::Receiver<Signal>, wanted: Signal) -> Result<(), String> {
    match signals.recv_timeout(HELD_WAIT) {
        Ok(got) if got == wanted => Ok(()),
        Ok(got) => Err(format!("expected {wanted:?}, got {got:?}")),
        Err(_) => Err(format!("no {wanted:?}")),
    }
}

/// One connection, from the login to the player's leaving.
fn serve(
    stream: TcpStream,
    options: &Options,
    pool: &Arc<Pool>,
    counters: &Arc<Counters>,
) -> Result<(), String> {
    let mut connection = Connection::from_stream(stream).map_err(|e| e.to_string())?;
    let login_done = FakeBackend {
        compression: options.compression,
        forwarding_secret: options.secret.clone(),
        online_mode: false,
        after_login: Vec::<Step>::new(),
    }
    .run(&mut connection, &Recorder::new())?;
    let mut rng = Rng::new(login_done.uuid as u64 ^ (login_done.uuid >> 64) as u64);
    // The configuration phase: registry data, then its end, which the player answers.
    let mut registry = vec![0u8; options.shape.registry_bytes];
    for (i, byte) in registry.iter_mut().enumerate() {
        *byte = b"minecraft:registry/entry "[i % 25];
    }
    send_all(
        &mut connection,
        counters,
        [
            (cfg::clientbound::REGISTRY_DATA, registry),
            (cfg::clientbound::FINISH_CONFIGURATION, Vec::new()),
        ],
    )?;
    loop {
        let (id, _) = connection.recv().map_err(|e| e.to_string())?;
        if id == cfg::serverbound::FINISH_CONFIGURATION {
            break;
        }
    }
    let uuid = format!("{:032x}", login_done.uuid);
    let expected = options
        .transfers
        .as_ref()
        .and_then(|transfers| transfers.take_expected(&uuid));
    match expected {
        Some(signals) => held(&mut connection, options, pool, counters, &mut rng, &signals)?,
        None => join(&mut connection, options, pool, counters, &mut rng)?,
    }
    counters.logged_in.fetch_add(1, Ordering::Relaxed);
    counters.online.fetch_add(1, Ordering::Relaxed);
    let (mut reader, writer) = connection.split();
    let _ = reader.set_read_timeout(None);
    if let (Some(transfers), Ok(closer)) = (&options.transfers, reader.closer()) {
        transfers.keep(options.name, &uuid, closer);
    }
    let writer = Arc::new(Mutex::new(writer));
    let closed = Arc::new(AtomicBool::new(false));
    let streaming = {
        let (writer, closed) = (Arc::clone(&writer), Arc::clone(&closed));
        let (options, pool, counters) = (options.clone(), Arc::clone(pool), Arc::clone(counters));
        let seed = rng.next();
        std::thread::Builder::new()
            .name(format!("load-{}-stream", options.name))
            .stack_size(256 * 1024)
            .spawn(move || stream_to(&writer, &closed, &options, &pool, seed, &counters))
    };
    let result = listen_to(&mut reader, &writer, counters);
    closed.store(true, Ordering::Relaxed);
    counters.online.fetch_sub(1, Ordering::Relaxed);
    if let Some(transfers) = &options.transfers {
        transfers.forget(options.name, &uuid);
    }
    if let Ok(thread) = streaming {
        let _ = thread.join();
    }
    result
}

/// What the player sends: counted, keep-alives noted, measuring packets answered at once.
fn listen_to(
    reader: &mut horizon_protocol::connection::ConnReader,
    writer: &Arc<Mutex<ConnWriter>>,
    counters: &Counters,
) -> Result<(), String> {
    let mut reply = Vec::new();
    loop {
        let (id, payload) = match reader.recv() {
            Ok(packet) => packet,
            // The player leaving is the end; anything else is said, for the run to show.
            Err(e) => {
                if !e.is_end_of_stream() && std::env::var_os("MERIDIAN_LOAD_VERBOSE").is_some() {
                    eprintln!("load: reading from a player: {e}");
                }
                return Ok(());
            }
        };
        counters.up_packets.fetch_add(1, Ordering::Relaxed);
        counters
            .up_bytes
            .fetch_add(payload.len() as u64 + 1, Ordering::Relaxed);
        if id == ps::KEEP_ALIVE {
            counters.keepalives_answered.fetch_add(1, Ordering::Relaxed);
        } else if id == ps::CUSTOM_PAYLOAD && echo::is_echo(&payload) {
            reply.clear();
            reply.extend_from_slice(&body(pc::CUSTOM_PAYLOAD, &payload));
            let mut writer = writer.lock().map_err(|_| "poisoned".to_owned())?;
            if writer.send(&reply).is_err() {
                return Ok(());
            }
        }
    }
}

/// What the server sends a player once they are in, until they leave.
fn stream_to(
    writer: &Arc<Mutex<ConnWriter>>,
    closed: &AtomicBool,
    options: &Options,
    pool: &Pool,
    seed: u64,
    counters: &Counters,
) {
    let mut rng = Rng::new(seed);
    let shape = options.shape;
    let started = Instant::now();
    let (small_every, chunk_every) = match options.traffic {
        Traffic::Idle => (None, None),
        Traffic::Active {
            packets_per_second,
            chunks_per_second,
            ..
        } => (
            (packets_per_second > 0)
                .then(|| Duration::from_secs_f64(1.0 / f64::from(packets_per_second))),
            (chunks_per_second > 0.0).then(|| Duration::from_secs_f64(1.0 / chunks_per_second)),
        ),
    };
    // Players do not all start on the same beat.
    let phase = |every: Duration, rng: &mut Rng| {
        started + Duration::from_nanos(rng.next() % every.as_nanos().max(1) as u64)
    };
    let mut next_small = small_every.map(|e| phase(e, &mut rng));
    let mut next_chunk = chunk_every.map(|e| phase(e, &mut rng));
    let mut next_keepalive = started
        + options
            .keepalive
            .mul_f64(0.5 + (rng.next() % 50) as f64 / 100.0);
    let (mut position, mut column, mut keepalive_id) = ([0.0f64; 3], 0usize, 0u64);
    let columns: Vec<(i32, i32)> = view(shape.view_radius).collect();
    let mut scratch = Vec::new();
    // With ticks, what falls due is kept here and written at the tick that follows it.
    let tick = options.write_per_tick;
    let mut tick_at = tick.map(|t| next_tick(Instant::now(), t));
    let batch: std::cell::RefCell<Vec<Vec<u8>>> = std::cell::RefCell::new(Vec::new());
    let mut sent = |writer: &Arc<Mutex<ConnWriter>>, id: i32, payload: &[u8]| -> bool {
        scratch.clear();
        write_varint(&mut scratch, id);
        scratch.extend_from_slice(payload);
        if tick.is_some() {
            batch.borrow_mut().push(scratch.clone());
        } else {
            let Ok(mut writer) = writer.lock() else {
                return false;
            };
            if writer.send(&scratch).is_err() {
                return false;
            }
            counters.down_writes.fetch_add(1, Ordering::Relaxed);
        }
        counters.down_packets.fetch_add(1, Ordering::Relaxed);
        counters
            .down_bytes
            .fetch_add(scratch.len() as u64, Ordering::Relaxed);
        true
    };
    // A tick's packets, framed one after the other into one buffer, in one write.
    let flush = |writer: &Arc<Mutex<ConnWriter>>| -> bool {
        let bodies = std::mem::take(&mut *batch.borrow_mut());
        if bodies.is_empty() {
            return true;
        }
        let Ok(mut writer) = writer.lock() else {
            return false;
        };
        let mut out = Vec::new();
        for body in &bodies {
            let Ok(contents) = horizon_protocol::frame::contents(body, writer.threshold) else {
                return false;
            };
            writer.frame_into(&contents, &mut out);
        }
        if writer.write_raw(&out).is_err() {
            return false;
        }
        counters.down_writes.fetch_add(1, Ordering::Relaxed);
        true
    };
    while !closed.load(Ordering::Relaxed) {
        let due = [next_small, next_chunk, Some(next_keepalive)]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(next_keepalive);
        let now = Instant::now();
        // What is due by `limit` is sent now: by now, or with ticks by the tick that has come.
        let limit = match tick_at {
            None => now,
            Some(at) if now < at => {
                std::thread::sleep((at - now).min(Duration::from_millis(100)));
                continue;
            }
            Some(at) => at,
        };
        if due > limit {
            if let (Some(at), Some(t)) = (tick_at, tick) {
                // Everything due by this tick is queued: one write, then the next tick (a
                // tick the thread was too late for is not made up: its packets go in this one).
                if !flush(writer) {
                    return;
                }
                tick_at = Some(next_tick(now.max(at), t));
                continue;
            }
            // Never longer than a beat, so a player who left is noticed.
            std::thread::sleep((due - now).min(Duration::from_millis(100)));
            continue;
        }
        if next_keepalive <= limit {
            keepalive_id += 1;
            if !sent(writer, pc::KEEP_ALIVE, &keepalive_id.to_be_bytes()) {
                return;
            }
            counters.keepalives_sent.fetch_add(1, Ordering::Relaxed);
            next_keepalive += options.keepalive;
        }
        if let (Some(at), Some(every), Traffic::Active { tracked, .. }) =
            (next_small, small_every, options.traffic)
        {
            if at <= limit {
                let ok = if shape.entities > 0 && rng.chance(tracked) {
                    position[0] += 0.1;
                    position[2] -= 0.1;
                    let id = entity_base(options.flavour) + rng.between(0, shape.entities) as i32;
                    let (pid, payload) = packet(&C::TeleportEntity(play::TeleportEntity {
                        id,
                        position,
                        delta: [0.0; 3],
                        yaw: 0.0,
                        pitch: 0.0,
                        relatives: 0,
                        on_ground: true,
                    }));
                    sent(writer, pid, &payload)
                } else {
                    let size = rng.between(50, 200);
                    let mut payload = Vec::with_capacity(size + 20);
                    write_varint(&mut payload, "meridian:load".len() as i32);
                    payload.extend_from_slice(b"meridian:load");
                    payload.resize(size, 0x33);
                    sent(writer, pc::CUSTOM_PAYLOAD, &payload)
                };
                if !ok {
                    return;
                }
                next_small = Some(at + every);
            }
        }
        if let (Some(at), Some(every)) = (next_chunk, chunk_every)
            && at <= limit
        {
            let (x, z) = columns
                .get(column % columns.len().max(1))
                .copied()
                .unwrap_or((0, 0));
            column += 1;
            let (id, payload) = pool.column(x, z, &mut rng);
            if !sent(writer, id, &payload) {
                return;
            }
            next_chunk = Some(at + every);
        }
    }
}

/// The first tick after `now`, on a clock every player of this program shares: a game server
/// ticks once for all its players, so their batches leave together.
fn next_tick(now: Instant, tick: Duration) -> Instant {
    static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let epoch = *EPOCH.get_or_init(Instant::now);
    let tick_nanos = tick.as_nanos().max(1);
    let ticks = now.saturating_duration_since(epoch).as_nanos() / tick_nanos + 1;
    epoch + Duration::from_nanos(u64::try_from(ticks * tick_nanos).unwrap_or(u64::MAX))
}

/// Data of about `size` bytes that compresses as chunks do: for the micro-measures.
pub fn chunk_like(size: usize) -> Vec<u8> {
    let pool = Pool::new(1);
    pool.0
        .into_iter()
        .min_by_key(|data| data.len().abs_diff(size))
        .unwrap_or_default()
}

#[cfg(test)]
mod tick_tests {
    use super::*;
    use horizon_protocol::connection::Pipe;

    /// The stream of one player at 100 small packets a second for `seconds`, over an in-memory
    /// pipe: the packets that reached the player, and the writes they took.
    fn stream(write_per_tick: Option<Duration>, seconds: f64) -> (u64, u64) {
        let (server, mut client) = Pipe::pair();
        let (_reader, writer) = Connection::local(server).split();
        let writer = Arc::new(Mutex::new(writer));
        let options = Options {
            name: "tick",
            compression: None,
            secret: None,
            shape: Shape {
                view_radius: 0,
                entities: 0,
                registry_bytes: 0,
            },
            traffic: Traffic::Active {
                packets_per_second: 100,
                tracked: 0.0,
                chunks_per_second: 0.0,
            },
            keepalive: Duration::from_secs(60),
            write_per_tick,
            flavour: 1,
            transfers: None,
            bind: loopback(),
        };
        let closed = Arc::new(AtomicBool::new(false));
        let counters = Arc::new(Counters::default());
        let thread = {
            let (writer, closed, counters) = (
                Arc::clone(&writer),
                Arc::clone(&closed),
                Arc::clone(&counters),
            );
            std::thread::spawn(move || {
                stream_to(&writer, &closed, &options, &Pool::new(1), 7, &counters)
            })
        };
        std::thread::sleep(Duration::from_secs_f64(seconds));
        closed.store(true, Ordering::Relaxed);
        thread.join().unwrap();
        // Everything written is in the pipe: read it to the end without waiting.
        client.set_nonblocking(true);
        let (mut reader, _writer) = Connection::local(client).split();
        let mut packets = 0;
        while reader.recv().is_ok() {
            packets += 1;
        }
        (packets, counters.down_writes.load(Ordering::Relaxed))
    }

    /// With ticks of 50 ms the same stream reaches the player in a twentieth of a second's
    /// batches, one write each, and the number of packets stays what it was.
    #[test]
    fn a_tick_of_packets_is_one_write_and_the_volume_stays() {
        let (plain, plain_writes) = stream(None, 3.0);
        let (ticked, ticked_writes) = stream(Some(Duration::from_millis(50)), 3.0);
        assert_eq!(plain_writes, plain, "one write a packet without ticks");
        assert!(
            (270..=310).contains(&plain),
            "100 a second for 3 s: {plain}"
        );
        let apart = (ticked as f64 - plain as f64).abs() / plain as f64;
        assert!(
            apart <= 0.05,
            "{ticked} packets with ticks, {plain} without"
        );
        // 20 ticks a second for 3 s, about 5 packets in each.
        assert!(
            (50..=62).contains(&ticked_writes),
            "{ticked_writes} writes for {ticked} packets"
        );
    }

    #[test]
    fn ticks_fall_on_one_clock() {
        let tick = Duration::from_millis(50);
        // The clock starts at its first use.
        let _ = next_tick(Instant::now(), tick);
        let now = Instant::now();
        let (a, b) = (
            next_tick(now, tick),
            next_tick(now + Duration::from_millis(3), tick),
        );
        assert!(a > now && a <= now + tick);
        // Two players asking a moment apart in the same tick get the same boundary.
        assert!(b == a || b == a + tick);
        assert_eq!(next_tick(a, tick), a + tick);
    }
}
