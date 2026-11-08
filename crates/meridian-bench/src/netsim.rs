//! `meridian-bench netsim`: a TCP relay that makes a link worse, to try the proxy on a bad network.
//!
//! It stands between two programs (the bots and the proxy, or the proxy and a server) and, for what
//! passes through it, adds a delay and a jitter, limits the rate, stalls the stream now and then,
//! and closes or resets connections at random. It keeps a log of what it did. See docs/NETSIM.md.
//!
//! What it does NOT do, plainly: it does not lose packets. TCP does not, and on 127.0.0.1 nothing is
//! lost anyway. A "stall" stops the relay from reading and from delivering for a while, so the
//! sender's buffers fill and its writes block, and then the data comes in a burst: that is what a loss
//! looks like to a TCP stream from above, not the retransmission, the cut of the congestion window or
//! the reordering that cause it. Nor does it simulate MTU and fragmentation, a router's buffers, a long
//! path's congestion control (the senders' windows are the local ones), NAT or other flows.
//!
//! How: for each connection, one reader and one writer thread a direction. The reader stamps every
//! chunk it reads with its arrival time and hands it to the writer, which waits until `arrival + delay
//! + jitter` (never before the chunk before it: TCP keeps the order) and writes it. The rate limit
//! lengthens that wait the same way; a stall makes the reader stop reading. The reader and the writer
//! share a queue of [`QUEUE_CHUNKS`] chunks (a pipe that holds about 1 MiB): when it is full the reader
//! stops reading, as a sender stops on a full TCP window, so memory stays bounded whatever `--rate` is.
//! A connection costs 2 descriptors (its two sockets, shared by the threads) and 5 threads (one that
//! serves it and a reader and a writer for each way): the relay is meant for up to [`SOFT_LIMIT`]
//! connections and says so when it goes over. On Linux, raise `ulimit -n` to at least twice that.
//!
//! The pure parts (the generator of random numbers, the delivery times, the stalls) take their times as
//! arguments, so that their tests need no clock.
//!
//! This module has a little unsafe code for Windows: the 1 ms timer the delays need (the default of
//! Windows is 15.6 ms) and the abortive close of `--rst`.
use serde_json::json;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// The connections the relay is meant for: 2 descriptors and 5 threads each (1,200 descriptors and
/// 3,000 threads at the limit). Past it, it says so (once, on the screen and in the log) instead of
/// slowing down without a word.
pub const SOFT_LIMIT: usize = 600;
/// The chunks a way holds between its reader and its writer (16 KiB at most each: about 1 MiB). A full
/// queue stops the reader, as a full window stops a TCP sender; it also means a way carries at most
/// about 1 MiB per delay (50 MB/s at 20 ms): a limit of the relay, not of the link it stands for.
pub const QUEUE_CHUNKS: usize = 64;
/// How long a read on a socket waits before the reader looks at the stop flag again: 5 wake-ups a
/// second for each reader, the price of a stop and a reset that work the same on every system.
const READ_TICK: Duration = Duration::from_millis(200);
/// How long a stop waits for the connections' threads to finish.
const STOP_WAIT: Duration = Duration::from_secs(5);
/// The bounds of the options: what the relay refuses beyond, at the reading of the command line.
const MAX_DELAY_MS: f64 = 60_000.0;
const MAX_STALL_MS: f64 = 600_000.0;
const MAX_STALL_EVERY_S: f64 = 86_400.0;
const MIN_RATE_KBIT: f64 = 1.0;
const MAX_RATE_KBIT: f64 = 100_000_000.0;
const MAX_RESETS_PER_MIN: f64 = 100_000.0;
const MAX_SECONDS: u64 = 7 * 86_400;
/// The most a reader takes from a socket at once.
const CHUNK: usize = 16 * 1024;
/// The most delivery measurements kept for the summary, a direction each.
const SAMPLES: usize = 1_000_000;

pub const USAGE: &str = "\
Usage: meridian-bench netsim --listen ADDR --to ADDR [OPTIONS]

A TCP relay that makes a link worse, to try the proxy on a bad network: it listens on ADDR, connects
each client to ADDR of --to, and degrades what passes. Put it between the bots and the proxy (the bots'
--server names the relay) or between the proxy and a server (the proxy's backend address names it).

It does NOT lose packets: TCP does not, and on 127.0.0.1 nothing is lost. A stall stops the relay from
reading and delivering for a while (the sender's buffers fill, its writes block), then the data comes
in a burst: that is what a loss looks like to a TCP stream from above, not the retransmission, the
cut of the congestion window or the reordering that cause it. No MTU, no router buffers, no long path.

Options (a direction is `up`, from the client to the target, or `down`, back):
  --listen ADDR        where to listen (host:port)
  --to ADDR            where to connect each client to (host:port)
  --delay MS           a fixed delay each way (default 0); --delay-up MS and --delay-down MS for one way
  --jitter MS          a spread around it: each chunk waits delay +- jitter, in order (default 0);
                       --jitter-up and --jitter-down
  --rate KBIT          the most a connection carries each way, in kilobits a second, from 1 (default none);
                       --rate-up and --rate-down
  --stall-every S      on average every S seconds, a connection stalls (default never)
  --stall-for MS       for how long (default 200); the stalls of the two ways are drawn apart
  --reset-per-min N    on average N times a minute, a random connection is closed (default 0)
  --rst                a closed connection is reset (RST) rather than closed with a FIN (Windows only)
  --seed N             the seed of every random draw (default 1): the same seed, the same degradation
  --log FILE           one JSON line for each event and a summary at the end
  --seconds N          stop after N seconds (default: Ctrl+C)
  --stop-file FILE     stop as soon as this file exists (for a script that cannot send Ctrl+C)
  -h, --help           show this help

The relay is meant for up to 600 connections (2 descriptors and 5 threads each; on Linux raise `ulimit -n`)
and says so when it goes over. A way holds at most 64 chunks (about 1 MiB) in flight: past that the reader
stops, as a sender does on a full TCP window. The options have bounds: a delay or jitter up to 60,000 ms, a
rate from 1 kbit a second, a stall up to 600,000 ms.";

/// What one way of a connection does to what passes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Way {
    pub delay_ms: f64,
    pub jitter_ms: f64,
    pub rate_kbit: Option<f64>,
    pub stall_every_s: f64,
    pub stall_for_ms: f64,
}

/// What the relay does, for both ways and for the connections as a whole.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    pub up: Way,
    pub down: Way,
    pub reset_per_min: f64,
    pub rst: bool,
    pub seed: u64,
}

// ---------------------------------------------------------------------------------------------
// The pure parts

/// A small generator of random numbers (xorshift64*), seeded: the same seed, the same draws.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    /// A generator for `seed`, apart from the others made with another `salt`.
    pub fn new(seed: u64, salt: u64) -> Self {
        let mut z = seed.wrapping_add(salt.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        Self(z | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A number in [0, 1).
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// An exponential draw with this mean (the gap between events of a steady random rate).
    pub fn exponential(&mut self, mean: f64) -> f64 {
        -mean * (1.0 - self.unit()).ln()
    }
}

fn secs(s: f64) -> Duration {
    // Never a panic, whatever the number: not finite is nothing, and a very long time is cut at 115 days.
    if s.is_finite() {
        Duration::from_secs_f64(s.clamp(0.0, 1.0e7))
    } else {
        Duration::ZERO
    }
}

/// When each chunk of one way is handed on: the delay and jitter, the order kept, the rate limit.
/// Times are those since the relay started, passed in, so that a test needs no clock.
pub struct Shaper {
    way: Way,
    rng: Rng,
    last: Duration,
    pipe_free: Duration,
}

impl Shaper {
    pub fn new(way: Way, seed: u64, salt: u64) -> Self {
        Self {
            way,
            rng: Rng::new(seed, salt),
            last: Duration::ZERO,
            pipe_free: Duration::ZERO,
        }
    }

    /// When a chunk of `bytes` that arrived at `arrival` is to be written.
    pub fn schedule(&mut self, arrival: Duration, bytes: usize) -> Duration {
        let spread = if self.way.jitter_ms > 0.0 {
            (self.rng.unit() * 2.0 - 1.0) * self.way.jitter_ms
        } else {
            0.0
        };
        let mut due = arrival + secs((self.way.delay_ms + spread) / 1000.0);
        // TCP keeps the order: a chunk never goes before the one before it.
        due = due.max(self.last);
        if let Some(kbit) = self.way.rate_kbit {
            // The chunk takes `bytes * 8 / rate` seconds of the pipe, after the ones before it.
            let ready = due.max(self.pipe_free);
            self.pipe_free = ready + secs(bytes as f64 * 8.0 / (kbit * 1000.0));
            due = self.pipe_free;
        }
        self.last = due;
        due
    }
}

/// When a way stalls: windows of `stall_for_ms` that start at random, on average every
/// `stall_every_s` seconds, drawn once and in order.
pub struct StallPlan {
    every: f64,
    len: Duration,
    rng: Rng,
    start: Duration,
    end: Duration,
    seen: u64,
}

impl StallPlan {
    /// The plan of a way, or `None` when it never stalls.
    pub fn new(way: &Way, seed: u64, salt: u64) -> Option<Self> {
        if way.stall_every_s <= 0.0 || way.stall_for_ms <= 0.0 {
            return None;
        }
        let mut rng = Rng::new(seed, salt);
        let len = secs(way.stall_for_ms / 1000.0);
        let start = secs(rng.exponential(way.stall_every_s));
        Some(Self {
            every: way.stall_every_s,
            len,
            rng,
            start,
            end: start + len,
            seen: 0,
        })
    }

    /// When the stall that covers `now` ends, if one does. Asked with times that do not go back.
    pub fn pause_until(&mut self, now: Duration) -> Option<Duration> {
        while self.end <= now {
            self.start = self.end + secs(self.rng.exponential(self.every));
            self.end = self.start + self.len;
        }
        if now >= self.start {
            self.seen += 1;
            Some(self.end)
        } else {
            None
        }
    }
}

/// Whether `live` connections are over what the relay is meant for.
pub fn over_limit(live: usize) -> bool {
    live > SOFT_LIMIT
}

// ---------------------------------------------------------------------------------------------
// Windows: the 1 ms timer and the abortive close

#[cfg(windows)]
#[allow(unsafe_code)]
mod os {
    use std::net::TcpStream;
    use std::os::windows::io::AsRawSocket;

    #[link(name = "winmm")]
    unsafe extern "system" {
        fn timeBeginPeriod(ms: u32) -> u32;
        fn timeEndPeriod(ms: u32) -> u32;
    }

    #[link(name = "ws2_32")]
    unsafe extern "system" {
        fn setsockopt(socket: usize, level: i32, name: i32, value: *const u8, len: i32) -> i32;
    }

    /// The system timer at 1 ms for as long as this lives: a sleep of 1 ms then lasts 1 to 2 ms,
    /// not 15.6.
    pub struct Timer;

    impl Timer {
        pub fn start() -> Timer {
            // SAFETY: a call with a number, no pointer; paired with the one in `drop`.
            unsafe { timeBeginPeriod(1) };
            Timer
        }
    }

    impl Drop for Timer {
        fn drop(&mut self) {
            // SAFETY: the same number as `start`.
            unsafe { timeEndPeriod(1) };
        }
    }

    /// Makes the close of this socket abortive (SO_LINGER, on, 0 s): it sends an RST. True if the
    /// system accepted it.
    pub fn abortive(stream: &TcpStream) -> bool {
        #[repr(C)]
        struct Linger {
            on: u16,
            seconds: u16,
        }
        const SOL_SOCKET: i32 = 0xffff;
        const SO_LINGER: i32 = 0x0080;
        let linger = Linger { on: 1, seconds: 0 };
        // SAFETY: the socket is open (the stream holds it); the value is a 4 byte struct that lives
        // through the call, and its length is given.
        unsafe {
            setsockopt(
                stream.as_raw_socket() as usize,
                SOL_SOCKET,
                SO_LINGER,
                (&raw const linger).cast::<u8>(),
                std::mem::size_of::<Linger>() as i32,
            ) == 0
        }
    }
}

#[cfg(not(windows))]
mod os {
    use std::net::TcpStream;

    pub struct Timer;

    impl Timer {
        pub fn start() -> Timer {
            Timer
        }
    }

    /// Not available here: `--rst` is refused when the options are read.
    pub fn abortive(_stream: &TcpStream) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------------------------
// The log and the figures

/// The log: one JSON line for each event, with the time since the relay started.
struct Log {
    started: Instant,
    file: Mutex<Option<std::io::BufWriter<std::fs::File>>>,
}

impl Log {
    fn open(path: Option<&PathBuf>, started: Instant) -> std::io::Result<Self> {
        let file = match path {
            Some(p) => Some(std::io::BufWriter::new(std::fs::File::create(p)?)),
            None => None,
        };
        Ok(Self {
            started,
            file: Mutex::new(file),
        })
    }

    fn event(&self, mut value: serde_json::Value) {
        if let Ok(mut guard) = self.file.lock() {
            if let Some(file) = guard.as_mut() {
                value["t_ms"] = json!(self.started.elapsed().as_millis() as u64);
                let _ = writeln!(file, "{value}");
                // Each line goes to the file at once: a relay that is killed leaves its log whole.
                let _ = file.flush();
            }
        }
    }

    fn flush(&self) {
        if let Ok(mut guard) = self.file.lock() {
            if let Some(file) = guard.as_mut() {
                let _ = file.flush();
            }
        }
    }
}

/// What the relay counts while it runs.
#[derive(Default)]
struct Stats {
    connections: AtomicU64,
    live: AtomicUsize,
    peak: AtomicUsize,
    bytes_up: AtomicU64,
    bytes_down: AtomicU64,
    stalls: AtomicU64,
    resets: AtomicU64,
    over_limit: AtomicBool,
    /// How long each chunk was held, from its arrival to its write, in microseconds; and by how
    /// much that missed what was planned. A direction each (up, down).
    added_us: [Mutex<Vec<u32>>; 2],
    error_us: [Mutex<Vec<u32>>; 2],
}

fn mean_and_p99(samples: &Mutex<Vec<u32>>) -> (f64, f64) {
    let mut v = samples.lock().map(|g| g.clone()).unwrap_or_default();
    if v.is_empty() {
        return (0.0, 0.0);
    }
    v.sort_unstable();
    let mean = v.iter().map(|&x| f64::from(x)).sum::<f64>() / v.len() as f64;
    (mean, f64::from(v[(v.len() * 99 / 100).min(v.len() - 1)]))
}

/// The summary of a run.
#[derive(Clone, Debug)]
pub struct Summary {
    pub connections: u64,
    pub peak: usize,
    pub bytes_up: u64,
    pub bytes_down: u64,
    pub stalls: u64,
    pub resets: u64,
    pub over_limit: bool,
    /// Held mean and p99, in microseconds, up and down.
    pub added_up: (f64, f64),
    pub added_down: (f64, f64),
    /// By how much the writes missed the plan: mean and p99, in microseconds.
    pub error_up: (f64, f64),
    pub error_down: (f64, f64),
}

impl Summary {
    pub fn text(&self) -> String {
        format!(
            "netsim: {} connections (peak {}), {} bytes up, {} bytes down, {} stalls, {} resets{}\n\
             netsim: a chunk was held on average {:.1} ms up (p99 {:.1}) and {:.1} ms down (p99 {:.1}); \
             the writes missed their time by {:.2} ms on average up (p99 {:.2}) and {:.2} ms down (p99 {:.2})",
            self.connections,
            self.peak,
            self.bytes_up,
            self.bytes_down,
            self.stalls,
            self.resets,
            if self.over_limit {
                ", OVER THE SOFT LIMIT of 600 connections"
            } else {
                ""
            },
            self.added_up.0 / 1000.0,
            self.added_up.1 / 1000.0,
            self.added_down.0 / 1000.0,
            self.added_down.1 / 1000.0,
            self.error_up.0 / 1000.0,
            self.error_up.1 / 1000.0,
            self.error_down.0 / 1000.0,
            self.error_down.1 / 1000.0,
        )
    }
}

// ---------------------------------------------------------------------------------------------
// The relay

/// The two ends of a live connection, so that the chaos thread can close it.
struct Live {
    id: u64,
    client: Arc<TcpStream>,
    target: Arc<TcpStream>,
    /// Set by the chaos thread: the readers of this connection see it within a read timeout and end.
    /// (A shutdown does not wake a blocked read everywhere: on Windows it may not, until the peer closes.)
    dead: Arc<AtomicBool>,
}

struct Shared {
    settings: Settings,
    started: Instant,
    log: Log,
    stats: Stats,
    live: Mutex<Vec<Live>>,
    stop: Arc<AtomicBool>,
}

/// Sleeps until `due` (a time since `started`), to the millisecond. It looks at the stop flag at
/// least every 10 ms: false when the relay was told to stop meanwhile, true when `due` has come.
fn wait_until(started: Instant, due: Duration, stop: &AtomicBool) -> bool {
    loop {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        let now = started.elapsed();
        if now >= due {
            return true;
        }
        let left = due - now;
        if left > Duration::from_micros(1500) {
            thread::sleep((left - Duration::from_millis(1)).min(Duration::from_millis(10)));
        } else {
            thread::yield_now();
        }
    }
}

/// A chunk on its way from a reader to a writer: when it arrived, and what it holds.
type Chunk = (Duration, Vec<u8>);

/// Hands a chunk to the writer. While the queue is full it waits (as a sender does on a full window),
/// looking at the stop flag. False when the writer is gone or the relay is stopping.
fn hand_over(tx: &mpsc::SyncSender<Chunk>, mut item: Chunk, stop: &AtomicBool) -> bool {
    loop {
        match tx.try_send(item) {
            Ok(()) => return true,
            Err(mpsc::TrySendError::Disconnected(_)) => return false,
            Err(mpsc::TrySendError::Full(back)) => {
                if stop.load(Ordering::Relaxed) {
                    return false;
                }
                item = back;
                thread::sleep(Duration::from_millis(1));
            }
        }
    }
}

/// One way of one connection: a reader and a writer that share a bounded queue. The two sockets are
/// shared (`&TcpStream` reads and writes), so nothing is cloned and nothing can fail on a descriptor
/// here. The threads are added to `handles`; an error (a thread that cannot be started) is returned.
fn pump(
    shared: &Arc<Shared>,
    id: u64,
    way_index: usize,
    from: Arc<TcpStream>,
    to: Arc<TcpStream>,
    dead: &Arc<AtomicBool>,
    handles: &mut Vec<thread::JoinHandle<()>>,
) -> std::io::Result<()> {
    let way = if way_index == 0 {
        shared.settings.up.clone()
    } else {
        shared.settings.down.clone()
    };
    let name = if way_index == 0 { "up" } else { "down" };
    let salt = id * 4 + way_index as u64;
    let (tx, rx) = mpsc::sync_channel::<Chunk>(QUEUE_CHUNKS);
    let reader = {
        let shared = Arc::clone(shared);
        let from = Arc::clone(&from);
        let dead = Arc::clone(dead);
        let mut plan = StallPlan::new(&way, shared.settings.seed, salt + 2);
        thread::Builder::new()
            .stack_size(128 * 1024)
            .spawn(move || {
                let mut buffer = vec![0u8; CHUNK];
                let mut source: &TcpStream = &from;
                loop {
                    if shared.stop.load(Ordering::Relaxed) || dead.load(Ordering::Relaxed) {
                        break;
                    }
                    if let Some(plan) = plan.as_mut() {
                        let now = shared.started.elapsed();
                        if let Some(end) = plan.pause_until(now) {
                            shared.stats.stalls.fetch_add(1, Ordering::Relaxed);
                            shared.log.event(json!({
                                "event": "stall", "id": id, "way": name,
                                "ms": (end - now).as_millis() as u64,
                            }));
                            // A stop ends a stall at once.
                            if !wait_until(shared.started, end, &shared.stop) {
                                break;
                            }
                        }
                    }
                    match source.read(&mut buffer) {
                        // The sockets have a read timeout (READ_TICK): nothing came, look at the flags again.
                        Err(e)
                            if matches!(
                                e.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                            ) =>
                        {
                            continue;
                        }
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let counter = if way_index == 0 {
                                &shared.stats.bytes_up
                            } else {
                                &shared.stats.bytes_down
                            };
                            counter.fetch_add(n as u64, Ordering::Relaxed);
                            let chunk = (shared.started.elapsed(), buffer[..n].to_vec());
                            if !hand_over(&tx, chunk, &shared.stop) {
                                break;
                            }
                        }
                    }
                }
                // The reader is done: the writer finishes what it has, then passes the end on.
            })?
    };
    handles.push(reader);
    let writer = {
        let shared = Arc::clone(shared);
        let mut shaper = Shaper::new(way, shared.settings.seed, salt);
        thread::Builder::new()
            .stack_size(128 * 1024)
            .spawn(move || {
                let mut sink: &TcpStream = &to;
                for (arrival, data) in rx {
                    let due = shaper.schedule(arrival, data.len());
                    if !wait_until(shared.started, due, &shared.stop) {
                        break;
                    }
                    let now = shared.started.elapsed();
                    let record = |v: &Mutex<Vec<u32>>, micros: u128| {
                        if let Ok(mut g) = v.lock() {
                            if g.len() < SAMPLES {
                                g.push(micros.min(u128::from(u32::MAX)) as u32);
                            }
                        }
                    };
                    record(
                        &shared.stats.added_us[way_index],
                        (now - arrival).as_micros(),
                    );
                    record(
                        &shared.stats.error_us[way_index],
                        now.saturating_sub(due).as_micros(),
                    );
                    if sink.write_all(&data).is_err() {
                        let _ = from.shutdown(Shutdown::Read);
                        break;
                    }
                }
                let _ = to.shutdown(Shutdown::Write);
            })?
    };
    handles.push(writer);
    Ok(())
}

/// A client has connected: connect to the target, then pump both ways until the connection ends.
/// Every way out writes its event: `target-refused` (no connection was opened), or `open` and then
/// `close` (and `refused` between them when the threads could not be started).
fn serve(shared: Arc<Shared>, client: TcpStream, target_addr: SocketAddr) {
    let id = shared.stats.connections.fetch_add(1, Ordering::Relaxed) + 1;
    let target = match TcpStream::connect_timeout(&target_addr, Duration::from_secs(5)) {
        Ok(t) => t,
        Err(e) => {
            shared
                .log
                .event(json!({"event": "target-refused", "id": id, "error": e.to_string()}));
            return;
        }
    };
    let _ = client.set_nodelay(true);
    let _ = target.set_nodelay(true);
    let live = shared.stats.live.fetch_add(1, Ordering::Relaxed) + 1;
    shared.stats.peak.fetch_max(live, Ordering::Relaxed);
    if over_limit(live) && !shared.stats.over_limit.swap(true, Ordering::Relaxed) {
        eprintln!(
            "netsim: {live} connections: over the {SOFT_LIMIT} the relay is meant for (2 descriptors and 5 threads \
             each); its own delays may grow from here"
        );
        shared
            .log
            .event(json!({"event": "over-soft-limit", "live": live, "limit": SOFT_LIMIT}));
    }
    shared
        .log
        .event(json!({"event": "open", "id": id, "live": live}));
    // A read that waits no longer than READ_TICK: the readers look at the stop flag and at `dead` between reads.
    let _ = client.set_read_timeout(Some(READ_TICK));
    let _ = target.set_read_timeout(Some(READ_TICK));
    let client = Arc::new(client);
    let target = Arc::new(target);
    let dead = Arc::new(AtomicBool::new(false));
    if let Ok(mut list) = shared.live.lock() {
        list.push(Live {
            id,
            client: Arc::clone(&client),
            target: Arc::clone(&target),
            dead: Arc::clone(&dead),
        });
    }
    let opened = Instant::now();
    let mut handles = Vec::new();
    // A stop that came while this connection was being made closes it at once.
    if shared.stop.load(Ordering::Relaxed) {
        let _ = client.shutdown(Shutdown::Both);
        let _ = target.shutdown(Shutdown::Both);
    }
    let started = pump(
        &shared,
        id,
        0,
        Arc::clone(&client),
        Arc::clone(&target),
        &dead,
        &mut handles,
    )
    .and_then(|()| {
        pump(
            &shared,
            id,
            1,
            Arc::clone(&target),
            Arc::clone(&client),
            &dead,
            &mut handles,
        )
    });
    if let Err(e) = started {
        // The system would not give a thread: the connection is cut, and said so, with no panic.
        shared
            .log
            .event(json!({"event": "refused", "id": id, "error": e.to_string()}));
        let _ = client.shutdown(Shutdown::Both);
        let _ = target.shutdown(Shutdown::Both);
    }
    for h in handles {
        let _ = h.join();
    }
    if let Ok(mut list) = shared.live.lock() {
        list.retain(|l| l.id != id);
    }
    shared.log.event(json!({
        "event": "close", "id": id, "open_ms": opened.elapsed().as_millis() as u64,
    }));
    // Counted out last: a stop that waits for 0 live connections waits for the `close` line too.
    shared.stats.live.fetch_sub(1, Ordering::Relaxed);
}

/// The chaos: at random moments, on average `reset_per_min` a minute, a live connection is closed.
fn chaos(shared: Arc<Shared>) {
    let mut rng = Rng::new(shared.settings.seed, 0xC4A05);
    let mean = 60.0 / shared.settings.reset_per_min;
    while !shared.stop.load(Ordering::Relaxed) {
        let wait = rng.exponential(mean);
        let until = Instant::now() + secs(wait);
        while Instant::now() < until {
            if shared.stop.load(Ordering::Relaxed) {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let Ok(list) = shared.live.lock() else {
            continue;
        };
        if list.is_empty() {
            continue;
        }
        let victim = &list[(rng.next_u64() % list.len() as u64) as usize];
        victim.dead.store(true, Ordering::Relaxed);
        let kind = if shared.settings.rst
            && os::abortive(&victim.client)
            && os::abortive(&victim.target)
        {
            // Reading is shut so that the threads end and the sockets close at once: abortively.
            let _ = victim.client.shutdown(Shutdown::Read);
            let _ = victim.target.shutdown(Shutdown::Read);
            "rst"
        } else {
            let _ = victim.client.shutdown(Shutdown::Both);
            let _ = victim.target.shutdown(Shutdown::Both);
            "fin"
        };
        shared.stats.resets.fetch_add(1, Ordering::Relaxed);
        shared
            .log
            .event(json!({"event": "reset", "id": victim.id, "kind": kind}));
    }
}

/// A relay that is running: where it listens, and how to stop it.
pub struct Relay {
    pub addr: SocketAddr,
    shared: Arc<Shared>,
    threads: Vec<thread::JoinHandle<()>>,
    _timer: os::Timer,
}

impl Relay {
    /// Starts a relay on `listener`, towards `target`.
    pub fn start(
        listener: TcpListener,
        target: SocketAddr,
        settings: Settings,
        log_path: Option<&PathBuf>,
    ) -> std::io::Result<Relay> {
        let started = Instant::now();
        let addr = listener.local_addr()?;
        listener.set_nonblocking(true)?;
        let shared = Arc::new(Shared {
            log: Log::open(log_path, started)?,
            settings: settings.clone(),
            started,
            stats: Stats::default(),
            live: Mutex::new(Vec::new()),
            stop: Arc::new(AtomicBool::new(false)),
        });
        shared.log.event(json!({
            "event": "start", "listen": addr.to_string(), "target": target.to_string(),
            "up": way_json(&settings.up), "down": way_json(&settings.down),
            "reset_per_min": settings.reset_per_min, "rst": settings.rst, "seed": settings.seed,
            "soft_limit": SOFT_LIMIT, "queue_chunks": QUEUE_CHUNKS,
        }));
        let timer = os::Timer::start();
        let mut threads = Vec::new();
        {
            let shared = Arc::clone(&shared);
            threads.push(thread::spawn(move || {
                while !shared.stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((client, _)) => {
                            let _ = client.set_nonblocking(false);
                            let shared = Arc::clone(&shared);
                            // A thread that cannot be started drops the client (its socket closes).
                            let _ =
                                thread::Builder::new().spawn(move || serve(shared, client, target));
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => thread::sleep(Duration::from_millis(50)),
                    }
                }
            }));
        }
        if settings.reset_per_min > 0.0 {
            let shared = Arc::clone(&shared);
            threads.push(thread::spawn(move || chaos(shared)));
        }
        Ok(Relay {
            addr,
            shared,
            threads,
            _timer: timer,
        })
    }

    /// Stops the relay and returns the summary. It stops accepting, shuts every live connection (their
    /// threads end at once, a stall too), waits up to [`STOP_WAIT`] for them to finish (each writes its
    /// `close` event), and only then counts: the summary holds what the queues had in them. The log gets
    /// it as its last line. (A relay that is killed instead writes no summary.)
    pub fn stop(self) -> Summary {
        let Relay {
            shared, threads, ..
        } = self;
        shared.stop.store(true, Ordering::Relaxed);
        if let Ok(list) = shared.live.lock() {
            for l in list.iter() {
                let _ = l.client.shutdown(Shutdown::Both);
                let _ = l.target.shutdown(Shutdown::Both);
            }
        }
        for t in threads {
            let _ = t.join();
        }
        let deadline = Instant::now() + STOP_WAIT;
        while shared.stats.live.load(Ordering::Relaxed) > 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        let left = shared.stats.live.load(Ordering::Relaxed);
        if left > 0 {
            shared
                .log
                .event(json!({"event": "stop-timeout", "connections_left": left}));
        }
        let summary = summary_of(&shared.stats);
        shared.log.event(json!({
            "event": "summary", "connections": summary.connections, "peak": summary.peak,
            "bytes_up": summary.bytes_up, "bytes_down": summary.bytes_down,
            "stalls": summary.stalls, "resets": summary.resets, "over_limit": summary.over_limit,
            "held_up_us": {"mean": summary.added_up.0, "p99": summary.added_up.1},
            "held_down_us": {"mean": summary.added_down.0, "p99": summary.added_down.1},
            "missed_up_us": {"mean": summary.error_up.0, "p99": summary.error_up.1},
            "missed_down_us": {"mean": summary.error_down.0, "p99": summary.error_down.1},
        }));
        shared.log.flush();
        summary
    }

    /// The bytes read from the clients so far (for the tests of the queue's bound).
    #[cfg(test)]
    fn bytes_up(&self) -> u64 {
        self.shared.stats.bytes_up.load(Ordering::Relaxed)
    }
}

fn summary_of(s: &Stats) -> Summary {
    Summary {
        connections: s.connections.load(Ordering::Relaxed),
        peak: s.peak.load(Ordering::Relaxed),
        bytes_up: s.bytes_up.load(Ordering::Relaxed),
        bytes_down: s.bytes_down.load(Ordering::Relaxed),
        stalls: s.stalls.load(Ordering::Relaxed),
        resets: s.resets.load(Ordering::Relaxed),
        over_limit: s.over_limit.load(Ordering::Relaxed),
        added_up: mean_and_p99(&s.added_us[0]),
        added_down: mean_and_p99(&s.added_us[1]),
        error_up: mean_and_p99(&s.error_us[0]),
        error_down: mean_and_p99(&s.error_us[1]),
    }
}

fn way_json(way: &Way) -> serde_json::Value {
    json!({
        "delay_ms": way.delay_ms, "jitter_ms": way.jitter_ms, "rate_kbit": way.rate_kbit,
        "stall_every_s": way.stall_every_s, "stall_for_ms": way.stall_for_ms,
    })
}

// ---------------------------------------------------------------------------------------------
// The command line

struct Options {
    listen: String,
    to: String,
    settings: Settings,
    log: Option<PathBuf>,
    seconds: Option<u64>,
    stop_file: Option<PathBuf>,
}

fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Option<Options>, String> {
    let mut listen = None;
    let mut to = None;
    let mut settings = Settings {
        seed: 1,
        up: Way {
            stall_for_ms: 200.0,
            ..Way::default()
        },
        down: Way {
            stall_for_ms: 200.0,
            ..Way::default()
        },
        ..Settings::default()
    };
    let mut log = None;
    let mut seconds = None;
    let mut stop_file = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let arg = arg
            .into_string()
            .map_err(|a| format!("{a:?} is not text"))?;
        let mut value = |name: &str| {
            args.next()
                .and_then(|v| v.into_string().ok())
                .ok_or_else(|| format!("{name} needs a value"))
        };
        // A number from `low` to `high`: anything else (a negative one, one that is not finite, 1e300) is
        // refused here, with the name of the option, and never reaches the relay.
        let number = |text: String, name: &str, low: f64, high: f64| {
            text.parse::<f64>()
                .ok()
                .filter(|n| n.is_finite() && *n >= low && *n <= high)
                .ok_or_else(|| format!("{name} takes a number from {low} to {high}, not {text:?}"))
        };
        let whole = |text: String, name: &str, low: u64, high: u64| {
            text.parse::<u64>()
                .ok()
                .filter(|n| *n >= low && *n <= high)
                .ok_or_else(|| {
                    format!("{name} takes a whole number from {low} to {high}, not {text:?}")
                })
        };
        match arg.as_str() {
            "-h" | "--help" => return Ok(None),
            "--listen" => listen = Some(value("--listen")?),
            "--to" => to = Some(value("--to")?),
            "--delay" => {
                let n = number(value("--delay")?, "--delay", 0.0, MAX_DELAY_MS)?;
                settings.up.delay_ms = n;
                settings.down.delay_ms = n;
            }
            "--delay-up" => {
                settings.up.delay_ms =
                    number(value("--delay-up")?, "--delay-up", 0.0, MAX_DELAY_MS)?
            }
            "--delay-down" => {
                settings.down.delay_ms =
                    number(value("--delay-down")?, "--delay-down", 0.0, MAX_DELAY_MS)?;
            }
            "--jitter" => {
                let n = number(value("--jitter")?, "--jitter", 0.0, MAX_DELAY_MS)?;
                settings.up.jitter_ms = n;
                settings.down.jitter_ms = n;
            }
            "--jitter-up" => {
                settings.up.jitter_ms =
                    number(value("--jitter-up")?, "--jitter-up", 0.0, MAX_DELAY_MS)?
            }
            "--jitter-down" => {
                settings.down.jitter_ms =
                    number(value("--jitter-down")?, "--jitter-down", 0.0, MAX_DELAY_MS)?;
            }
            "--rate" => {
                let n = number(value("--rate")?, "--rate", MIN_RATE_KBIT, MAX_RATE_KBIT)?;
                settings.up.rate_kbit = Some(n);
                settings.down.rate_kbit = settings.up.rate_kbit;
            }
            "--rate-up" => {
                settings.up.rate_kbit = Some(number(
                    value("--rate-up")?,
                    "--rate-up",
                    MIN_RATE_KBIT,
                    MAX_RATE_KBIT,
                )?);
            }
            "--rate-down" => {
                settings.down.rate_kbit = Some(number(
                    value("--rate-down")?,
                    "--rate-down",
                    MIN_RATE_KBIT,
                    MAX_RATE_KBIT,
                )?);
            }
            "--stall-every" => {
                let n = number(
                    value("--stall-every")?,
                    "--stall-every",
                    0.0,
                    MAX_STALL_EVERY_S,
                )?;
                settings.up.stall_every_s = n;
                settings.down.stall_every_s = n;
            }
            "--stall-for" => {
                let n = number(value("--stall-for")?, "--stall-for", 0.0, MAX_STALL_MS)?;
                settings.up.stall_for_ms = n;
                settings.down.stall_for_ms = n;
            }
            "--reset-per-min" => {
                settings.reset_per_min = number(
                    value("--reset-per-min")?,
                    "--reset-per-min",
                    0.0,
                    MAX_RESETS_PER_MIN,
                )?;
            }
            "--rst" => {
                if !cfg!(windows) {
                    return Err("--rst needs Windows in this version".into());
                }
                settings.rst = true;
            }
            "--seed" => settings.seed = whole(value("--seed")?, "--seed", 0, u64::MAX)?,
            "--log" => log = Some(PathBuf::from(value("--log")?)),
            "--stop-file" => stop_file = Some(PathBuf::from(value("--stop-file")?)),
            "--seconds" => seconds = Some(whole(value("--seconds")?, "--seconds", 1, MAX_SECONDS)?),
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(Some(Options {
        listen: listen.ok_or("--listen is needed")?,
        to: to.ok_or("--to is needed")?,
        settings,
        log,
        seconds,
        stop_file,
    }))
}

/// `meridian-bench netsim ...`.
pub fn main(args: Vec<OsString>) -> ExitCode {
    let options = match parse(args) {
        Ok(Some(o)) => o,
        Ok(None) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            eprintln!("meridian-bench netsim: {message}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let resolve = |text: &str, name: &str| {
        text.to_socket_addrs()
            .ok()
            .and_then(|mut a| a.next())
            .ok_or_else(|| format!("{name} {text:?} is not an address"))
    };
    let (listen, target) = match (
        resolve(&options.listen, "--listen"),
        resolve(&options.to, "--to"),
    ) {
        (Ok(l), Ok(t)) => (l, t),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("meridian-bench netsim: {e}");
            return ExitCode::from(2);
        }
    };
    let listener = match TcpListener::bind(listen) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("meridian-bench netsim: cannot listen on {listen}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let relay = match Relay::start(
        listener,
        target,
        options.settings.clone(),
        options.log.as_ref(),
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("meridian-bench netsim: {e}");
            return ExitCode::FAILURE;
        }
    };
    let s = &options.settings;
    eprintln!(
        "netsim: listening on {}, relaying to {target}; up {} ms +-{} ms, down {} ms +-{} ms, \
         {} resets a minute{}; meant for up to {SOFT_LIMIT} connections ({} descriptors, {} threads), {QUEUE_CHUNKS} chunks a way in flight",
        relay.addr,
        s.up.delay_ms,
        s.up.jitter_ms,
        s.down.delay_ms,
        s.down.jitter_ms,
        s.reset_per_min,
        if s.up.stall_every_s > 0.0 {
            ", stalls on"
        } else {
            ""
        },
        SOFT_LIMIT * 2,
        SOFT_LIMIT * 5,
    );
    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = Arc::clone(&stop);
        let _ = ctrlc::set_handler(move || stop.store(true, Ordering::Relaxed));
    }
    let until = options
        .seconds
        .map(|s| Instant::now() + Duration::from_secs(s));
    while !stop.load(Ordering::Relaxed)
        && until.is_none_or(|u| Instant::now() < u)
        && !options.stop_file.as_ref().is_some_and(|f| f.exists())
    {
        thread::sleep(Duration::from_millis(100));
    }
    let summary = relay.stop();
    eprintln!("{}", summary.text());
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn way(delay: f64, jitter: f64) -> Way {
        Way {
            delay_ms: delay,
            jitter_ms: jitter,
            ..Way::default()
        }
    }

    #[test]
    fn the_same_seed_draws_the_same_numbers_and_another_does_not() {
        let mut a = Rng::new(7, 1);
        let mut b = Rng::new(7, 1);
        let mut c = Rng::new(7, 2);
        let a: Vec<u64> = (0..8).map(|_| a.next_u64()).collect();
        let b: Vec<u64> = (0..8).map(|_| b.next_u64()).collect();
        let c: Vec<u64> = (0..8).map(|_| c.next_u64()).collect();
        assert_eq!(a, b);
        assert_ne!(a, c);
        let mut r = Rng::new(7, 1);
        assert!((0..1000).all(|_| (0.0..1.0).contains(&r.unit())));
    }

    #[test]
    fn a_fixed_delay_is_the_arrival_plus_the_delay() {
        let mut s = Shaper::new(way(20.0, 0.0), 1, 0);
        assert_eq!(s.schedule(ms(100), 10), ms(120));
        assert_eq!(s.schedule(ms(150), 10), ms(170));
    }

    #[test]
    fn jitter_stays_inside_its_bounds_and_the_order_is_kept() {
        let mut s = Shaper::new(way(20.0, 15.0), 3, 0);
        let mut last = Duration::ZERO;
        for i in 0..2000u64 {
            let arrival = ms(i * 2);
            let due = s.schedule(arrival, 100);
            assert!(due >= last, "the order is kept: {due:?} after {last:?}");
            assert!(
                due >= arrival + ms(5),
                "never under delay - jitter: {due:?}"
            );
            last = due;
        }
        // A chunk whose own draw would pass the one before it is held back to it: with arrivals
        // 1 ms apart and a spread of 15 ms the due times are not all different.
        let mut s = Shaper::new(way(20.0, 15.0), 3, 0);
        let dues: Vec<Duration> = (0..200u64).map(|i| s.schedule(ms(i), 10)).collect();
        assert!(dues.windows(2).any(|w| w[0] == w[1]));
    }

    #[test]
    fn a_rate_limit_spaces_the_chunks_by_the_time_they_take_on_the_pipe() {
        // 8 kbit a second: 1000 bytes take exactly one second.
        let mut s = Shaper::new(
            Way {
                rate_kbit: Some(8.0),
                ..Way::default()
            },
            1,
            0,
        );
        assert_eq!(s.schedule(ms(0), 1000), Duration::from_secs(1));
        // The second chunk arrives at once but waits for the pipe.
        assert_eq!(s.schedule(ms(10), 1000), Duration::from_secs(2));
        // One that arrives after the pipe is free is delayed by its own time only.
        assert_eq!(s.schedule(Duration::from_secs(5), 500), ms(5500));
    }

    #[test]
    fn stalls_come_in_windows_of_the_length_asked_and_the_same_seed_gives_the_same_ones() {
        let w = Way {
            stall_every_s: 1.0,
            stall_for_ms: 200.0,
            ..Way::default()
        };
        assert!(StallPlan::new(&Way::default(), 1, 0).is_none());
        let walk = |seed| {
            let mut p = StallPlan::new(&w, seed, 5).unwrap();
            let mut windows = Vec::new();
            let mut t = Duration::ZERO;
            while t < Duration::from_secs(60) {
                if let Some(end) = p.pause_until(t) {
                    assert!(end > t);
                    windows.push(end);
                    t = end;
                } else {
                    t += ms(10);
                }
            }
            windows
        };
        let a = walk(1);
        assert_eq!(a, walk(1));
        assert_ne!(a, walk(2));
        // About one a second over a minute, each 200 ms long: not none, not hundreds.
        assert!((20..=140).contains(&a.len()), "{} windows", a.len());
        assert!(a.windows(2).all(|w| w[1] - w[0] >= ms(200)));
    }

    #[test]
    fn the_soft_limit_is_600_connections() {
        assert!(!over_limit(600));
        assert!(over_limit(601));
    }

    /// An echo server on a port of its own; returns the address.
    fn echo() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                thread::spawn(move || {
                    let mut s = stream;
                    let mut buffer = [0u8; 4096];
                    while let Ok(n) = s.read(&mut buffer) {
                        if n == 0 || s.write_all(&buffer[..n]).is_err() {
                            break;
                        }
                    }
                });
            }
        });
        addr
    }

    fn relay_to(target: SocketAddr, settings: Settings) -> Relay {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        Relay::start(listener, target, settings, None).unwrap()
    }

    fn round_trip(client: &mut TcpStream, byte: u8) -> Duration {
        let began = Instant::now();
        client.write_all(&[byte]).unwrap();
        let mut back = [0u8; 1];
        client.read_exact(&mut back).unwrap();
        assert_eq!(back[0], byte);
        began.elapsed()
    }

    #[test]
    fn an_echo_through_the_relay_takes_at_least_the_two_delays() {
        let relay = relay_to(
            echo(),
            Settings {
                up: way(20.0, 0.0),
                down: way(20.0, 0.0),
                seed: 1,
                ..Settings::default()
            },
        );
        let mut client = TcpStream::connect(relay.addr).unwrap();
        client.set_nodelay(true).unwrap();
        // Every round trip is 40 ms plus what the machine adds: the lower bound is firm, the upper
        // one generous for a loaded machine.
        let times: Vec<Duration> = (0..10).map(|i| round_trip(&mut client, i)).collect();
        for t in &times {
            assert!(
                *t >= ms(38),
                "a round trip of {t:?} is under the two delays"
            );
        }
        let best = times.iter().min().unwrap();
        assert!(*best < ms(120), "the best round trip took {best:?}");
        drop(client);
        let summary = relay.stop();
        assert_eq!(summary.connections, 1);
        assert!(summary.bytes_up >= 10 && summary.bytes_down >= 10);
        assert!(
            summary.added_up.0 >= 19_000.0,
            "held {} us up",
            summary.added_up.0
        );
    }

    #[test]
    fn a_megabyte_goes_through_unchanged_with_a_delay_and_a_jitter() {
        let relay = relay_to(
            echo(),
            Settings {
                up: way(5.0, 4.0),
                down: way(5.0, 4.0),
                seed: 9,
                ..Settings::default()
            },
        );
        let client = TcpStream::connect(relay.addr).unwrap();
        let pattern: Vec<u8> = (0..1_000_000u32).map(|i| (i * 31 % 251) as u8).collect();
        let mut writer = client.try_clone().unwrap();
        let sent = pattern.clone();
        let w = thread::spawn(move || writer.write_all(&sent).unwrap());
        let mut got = vec![0u8; pattern.len()];
        let mut reader = client;
        reader.read_exact(&mut got).unwrap();
        w.join().unwrap();
        assert!(
            got == pattern,
            "the bytes that came back are not the ones sent"
        );
        relay.stop();
    }

    #[test]
    fn a_stall_freezes_the_stream_for_about_its_length_and_nothing_is_lost() {
        let relay = relay_to(
            echo(),
            Settings {
                up: Way {
                    stall_every_s: 0.2,
                    stall_for_ms: 250.0,
                    ..Way::default()
                },
                down: Way::default(),
                seed: 4,
                ..Settings::default()
            },
        );
        let mut client = TcpStream::connect(relay.addr).unwrap();
        client.set_nodelay(true).unwrap();
        let mut worst = Duration::ZERO;
        for i in 0..40u8 {
            worst = worst.max(round_trip(&mut client, i));
            thread::sleep(ms(30));
        }
        assert!(
            worst >= ms(100),
            "no stall was felt in 40 round trips: worst {worst:?}"
        );
        assert!(worst < ms(1500), "a stall lasted {worst:?}");
        drop(client);
        let summary = relay.stop();
        assert!(
            summary.stalls >= 1,
            "{} stalls in the summary",
            summary.stalls
        );
    }

    #[test]
    fn a_reset_closes_the_connection_and_the_summary_counts_it() {
        let relay = relay_to(
            echo(),
            Settings {
                reset_per_min: 6000.0,
                seed: 2,
                ..Settings::default()
            },
        );
        let mut client = TcpStream::connect(relay.addr).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut byte = [0u8; 1];
        // The relay closes the connection on its own, within a second or so: a read ends.
        let ended = client.read(&mut byte);
        assert!(
            matches!(ended, Ok(0) | Err(_)),
            "the read returned {ended:?}"
        );
        let summary = relay.stop();
        assert!(summary.resets >= 1, "{} resets", summary.resets);
    }

    #[test]
    fn the_log_holds_a_line_for_each_event_and_a_summary() {
        let path = std::env::temp_dir().join(format!("netsim-test-{}.jsonl", std::process::id()));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let relay = Relay::start(listener, echo(), Settings::default(), Some(&path)).unwrap();
        let mut client = TcpStream::connect(relay.addr).unwrap();
        round_trip(&mut client, 1);
        drop(client);
        thread::sleep(ms(200));
        relay.stop();
        let text = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        let kinds: Vec<String> = text
            .lines()
            .map(|l| {
                serde_json::from_str::<serde_json::Value>(l).unwrap()["event"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert_eq!(kinds.first().map(String::as_str), Some("start"));
        assert!(kinds.iter().any(|k| k == "open"), "{kinds:?}");
        assert_eq!(kinds.last().map(String::as_str), Some("summary"));
    }

    #[test]
    fn the_options_are_read_and_a_wrong_one_is_refused() {
        let args = |v: &[&str]| v.iter().map(OsString::from).collect::<Vec<_>>();
        let o = parse(args(&[
            "--listen",
            "127.0.0.1:1",
            "--to",
            "127.0.0.1:2",
            "--delay",
            "20",
            "--delay-down",
            "30",
            "--jitter",
            "5",
            "--rate",
            "500",
            "--stall-every",
            "10",
            "--reset-per-min",
            "3",
            "--seed",
            "7",
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(o.settings.up.delay_ms, 20.0);
        assert_eq!(o.settings.down.delay_ms, 30.0);
        assert_eq!(o.settings.up.jitter_ms, 5.0);
        assert_eq!(o.settings.down.rate_kbit, Some(500.0));
        assert_eq!(o.settings.up.stall_for_ms, 200.0);
        assert_eq!(o.settings.reset_per_min, 3.0);
        assert_eq!(o.settings.seed, 7);
        assert!(parse(args(&["--help"])).unwrap().is_none());
        let o = parse(args(&[
            "--listen",
            "a:1",
            "--to",
            "b:2",
            "--stop-file",
            "x.stop",
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(o.stop_file, Some(PathBuf::from("x.stop")));
        assert!(parse(args(&["--listen", "x"])).is_err(), "--to is needed");
        assert!(parse(args(&["--to", "x", "--listen", "y", "--delay", "-3"])).is_err());
        assert!(parse(args(&["--to", "x", "--listen", "y", "--nope"])).is_err());
    }

    #[test]
    fn a_stop_closes_a_live_connection_and_every_thread_ends() {
        let path = std::env::temp_dir().join(format!("netsim-stop-{}.jsonl", std::process::id()));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let settings = Settings {
            up: way(5.0, 0.0),
            down: way(5.0, 0.0),
            ..Settings::default()
        };
        let relay = Relay::start(listener, echo(), settings, Some(&path)).unwrap();
        let mut client = TcpStream::connect(relay.addr).unwrap();
        round_trip(&mut client, 1);
        // The client stays connected: the stop must close the connection itself.
        let began = Instant::now();
        let summary = relay.stop();
        assert!(
            began.elapsed() < Duration::from_secs(4),
            "the stop took {:?}",
            began.elapsed()
        );
        assert_eq!(summary.connections, 1);
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut byte = [0u8; 1];
        let ended = client.read(&mut byte);
        assert!(
            matches!(ended, Ok(0) | Err(_)),
            "the read returned {ended:?}"
        );
        let text = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        let kinds: Vec<String> = text
            .lines()
            .map(|l| {
                serde_json::from_str::<serde_json::Value>(l).unwrap()["event"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        let close = kinds
            .iter()
            .position(|k| k == "close")
            .expect("a close line");
        let last = kinds.len() - 1;
        assert_eq!(kinds[last], "summary");
        assert!(
            close < last,
            "the close line comes before the summary: {kinds:?}"
        );
    }

    #[test]
    fn a_stop_ends_a_long_stall_at_once() {
        let relay = relay_to(
            echo(),
            Settings {
                up: Way {
                    stall_every_s: 0.05,
                    stall_for_ms: 30_000.0,
                    ..Way::default()
                },
                ..Settings::default()
            },
        );
        // The first stall window starts about 50 ms after the relay: a client that comes later finds
        // its reader stalled at once, for 30 seconds.
        thread::sleep(ms(400));
        let mut client = TcpStream::connect(relay.addr).unwrap();
        client.write_all(&[1]).unwrap();
        thread::sleep(ms(300));
        let began = Instant::now();
        let summary = relay.stop();
        assert!(
            began.elapsed() < Duration::from_secs(4),
            "a stall of 30 s held the stop for {:?}",
            began.elapsed()
        );
        assert!(summary.stalls >= 1, "{} stalls", summary.stalls);
    }

    #[test]
    fn a_slow_rate_stops_the_sender_and_the_relay_reads_only_what_its_queue_holds() {
        // 8 kbit a second: the writer delivers 1 KB a second. The reader must stop when the queue
        // (64 chunks of 16 KiB) is full, and the client's writes then block on the full buffers: the
        // relay does not read the 200 MiB the client is trying to send, nor hold it in memory.
        let relay = relay_to(
            echo(),
            Settings {
                up: Way {
                    rate_kbit: Some(8.0),
                    ..Way::default()
                },
                ..Settings::default()
            },
        );
        let client = TcpStream::connect(relay.addr).unwrap();
        let done = Arc::new(AtomicBool::new(false));
        let writer = {
            let mut w = client.try_clone().unwrap();
            let done = Arc::clone(&done);
            thread::spawn(move || {
                let block = vec![7u8; 1 << 20];
                for _ in 0..200 {
                    if w.write_all(&block).is_err() {
                        break;
                    }
                }
                done.store(true, Ordering::Relaxed);
            })
        };
        thread::sleep(Duration::from_millis(1500));
        let read = relay.bytes_up();
        assert!(
            read < 32 * 1024 * 1024,
            "the relay read {read} bytes of a sender it should have stopped"
        );
        assert!(
            !done.load(Ordering::Relaxed),
            "the client wrote its 200 MiB: nothing stopped it"
        );
        relay.stop();
        drop(client);
        writer.join().unwrap();
    }

    #[test]
    fn the_options_have_bounds_and_huge_or_tiny_values_are_refused() {
        let args = |v: &[&str]| v.iter().map(OsString::from).collect::<Vec<_>>();
        let with = |extra: &[&str]| {
            let mut v = vec!["--listen", "127.0.0.1:1", "--to", "127.0.0.1:2"];
            v.extend_from_slice(extra);
            parse(args(&v))
        };
        for bad in [
            ["--delay", "1e300"],
            ["--delay", "60001"],
            ["--delay", "-1"],
            ["--delay", "nan"],
            ["--delay", "inf"],
            ["--jitter", "1e19"],
            ["--delay-up", "1e300"],
            ["--rate", "1e-300"],
            ["--rate", "0"],
            ["--rate", "0.5"],
            ["--rate", "1e19"],
            ["--stall-for", "600001"],
            ["--stall-for", "1e300"],
            ["--stall-every", "86401"],
            ["--reset-per-min", "100001"],
            ["--seconds", "1e19"],
            ["--seconds", "0"],
            ["--seconds", "604801"],
            ["--seed", "1e19"],
            ["--seed", "18446744073709551616"],
        ] {
            assert!(with(&bad).is_err(), "{bad:?} was accepted");
        }
        for good in [
            ["--delay", "60000"],
            ["--delay", "1e-300"],
            ["--rate", "1"],
            ["--stall-for", "600000"],
            ["--seed", "18446744073709551615"],
            ["--seconds", "604800"],
        ] {
            assert!(with(&good).is_ok(), "{good:?} was refused");
        }
        // A number that is not finite, or very long, is nothing or a long time, never a panic.
        assert_eq!(secs(f64::NAN), Duration::ZERO);
        assert_eq!(secs(f64::INFINITY), Duration::ZERO);
        assert!(secs(1e300) <= Duration::from_secs(10_000_000));
        let mut shaper = Shaper::new(way(1e-300, 0.0), 1, 0);
        assert_eq!(shaper.schedule(ms(5), 10), ms(5));
    }
}
