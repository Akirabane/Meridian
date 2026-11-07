//! The four scenarios of the load runs, each for one number of bots: the proxy is the real
//! `meridian` program, the servers are synthetic ([`super::world`]), the players are the swarm
//! ([`super::bot`]), and what is read is the proxy process ([`super::sample`]).
//!
//! 1. **Idle**: the bots log in, hold the connection, answer keep-alives.
//! 2. **Active**: each bot is sent a stream and sends movement, and measuring packets time the
//!    round trip; the same load goes straight to the server first, so the proxy's share is the
//!    difference.
//! 3. **Moves**: some of the bots are moved to another server while the rest play.
//! 4. **Churn**: connections that come and go as fast as they can.
use super::bot::{self, Live, Plan, Role};
use super::remote::Windows;
use super::sample::{Sample, Sampler, Watch, mean, mib, peak, percentile};
use super::table::Row;
use super::transfer::{Sidecar, Transfers};
use super::world::{Counters, Options, Shape, Traffic, World};
use crate::proxy::Proxy;
use crate::stats::Summary;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

/// The secret the proxy and the synthetic servers share for the player information forwarding,
/// unless the run is given the secret of a real server (see [`Settings::forwarding`]).
pub const FORWARDING: &str = "meridian-load-forwarding-secret";

/// How the two sides of the proxy compress.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compression {
    /// The server and the proxy both compress from 256 bytes.
    Same,
    /// The server does not compress (a server on the same network, as it is advised); the
    /// proxy compresses toward the players from 256 bytes.
    ServerOff,
    /// Neither does.
    BothOff,
}

impl Compression {
    pub fn name(self) -> &'static str {
        match self {
            Compression::Same => "same",
            Compression::ServerOff => "server-off",
            Compression::BothOff => "both-off",
        }
    }

    /// What the server asks for.
    pub(super) fn server(self) -> Option<i32> {
        match self {
            Compression::Same => Some(256),
            Compression::ServerOff | Compression::BothOff => None,
        }
    }

    /// The proxy's `compression-threshold`, toward the players.
    pub(super) fn player(self) -> i32 {
        match self {
            Compression::Same | Compression::ServerOff => 256,
            Compression::BothOff => -1,
        }
    }
}

/// A real server a spectator's client reaches through a host name (see docs/BASELINE.md).
#[derive(Clone, Debug)]
pub struct Spectator {
    pub address: String,
    pub host: String,
    /// The port the proxy listens on, so that the client can be given it.
    pub port: u16,
}

/// What every scenario is told.
#[derive(Clone, Debug)]
pub struct Settings {
    pub proxy: PathBuf,
    pub dir: PathBuf,
    pub stop: Arc<AtomicBool>,
    /// The measured window of a step.
    pub window: Duration,
    pub shape: Shape,
    pub down_rate: u32,
    pub tracked: f64,
    pub chunks_per_second: f64,
    pub up_rate: u32,
    pub probe_rate: f64,
    pub keepalive: Duration,
    /// Bytes of memory the machine must keep free.
    pub min_free: u64,
    /// New bots a second; 0 starts them all at once.
    pub arrival: u32,
    /// The forwarding secret of the proxy and of the synthetic servers: a real server behind the
    /// same proxy (the spectator's) must share it.
    pub forwarding: String,
    /// The share of the bots, in percent, that move in the third scenario.
    pub movers_percent: usize,
    /// How long a connection of the churn stays, on average.
    pub churn_hold: Duration,
    pub spectator: Option<Spectator>,
    /// The proxy is on another machine: where it is and where the servers listen for it.
    pub remote: Option<super::remote::Remote>,
    /// When each phase of each step began and ended, for whoever reads the proxy's side.
    pub windows: Arc<super::remote::Windows>,
    /// The bots arrive in this many waves, `wave_gap` apart (a proxy of another machine only).
    pub waves: usize,
    pub wave_gap: Duration,
    /// The synthetic servers write what falls due in each tick of this length in one write, as a
    /// game server flushes a player's packets once a tick; `None` writes each packet when it is due.
    pub write_per_tick: Option<Duration>,
}

/// A server of the proxy's configuration.
pub(super) struct Server<'a> {
    pub(super) name: &'a str,
    pub(super) address: SocketAddr,
    pub(super) sidecar: Option<&'a str>,
}

/// The proxy's configuration for a run: offline, no limits, the servers, the compression, and
/// for a spectator the host that leads to a real server.
pub(super) fn configuration(
    listen: SocketAddr,
    servers: &[Server<'_>],
    threshold: i32,
    forwarding: &str,
    spectator: Option<&Spectator>,
) -> String {
    let mut text = format!(
        "listen = \"{listen}\"\n\
         max-players = 0\n\
         connections-per-minute = 0\n\
         max-pending-logins = 0\n\
         max-pending-logins-per-address = 0\n\
         auth-mode = \"offline\"\n\
         compression-threshold = {threshold}\n\
         connect-timeout-ms = 5000\n\
         forwarding-secret = \"{forwarding}\"\n\
         default-backend = \"{}\"\n",
        servers[0].name
    );
    if let Some(spectator) = spectator {
        text.push_str(&format!(
            "\n[[forced-host]]\nhost = \"{}\"\nbackend = \"spectator\"\n",
            spectator.host
        ));
    }
    for server in servers {
        text.push_str(&format!(
            "\n[[backends]]\nname = \"{}\"\naddress = \"{}\"\n",
            server.name, server.address
        ));
        if let Some(sidecar) = server.sidecar {
            text.push_str(&format!(
                "sidecar = \"{sidecar}\"\nsidecar-secret-file = \"{}.key\"\n",
                server.name
            ));
        }
    }
    if let Some(spectator) = spectator {
        text.push_str(&format!(
            "\n[[backends]]\nname = \"spectator\"\naddress = \"{}\"\n",
            spectator.address
        ));
    }
    text
}

fn start_proxy(s: &Settings, servers: &[Server<'_>], threshold: i32) -> Result<Proxy, String> {
    for server in servers {
        if server.sidecar.is_some() {
            std::fs::write(
                s.dir.join(format!("{}.key", server.name)),
                super::transfer::SECRET,
            )
            .map_err(|e| e.to_string())?;
        }
    }
    let mut proxy = Proxy::start_with(&s.proxy, &s.dir, |listen| {
        let listen = match &s.spectator {
            Some(spectator) => SocketAddr::from((listen.ip(), spectator.port)),
            None => listen,
        };
        configuration(
            listen,
            servers,
            threshold,
            &s.forwarding,
            s.spectator.as_ref(),
        )
    })
    .map_err(|e| e.to_string())?;
    // A moment for the proxy to settle before its idle size is read.
    std::thread::sleep(Duration::from_millis(500));
    if !proxy.alive() {
        return Err(format!("the proxy stopped at start: {}", proxy.log_tail()));
    }
    Ok(proxy)
}

/// What a synthetic server is told, apart from the run's settings.
pub(super) struct Spec {
    pub name: &'static str,
    /// 1 or 2: which world it sends.
    pub flavour: u8,
    pub traffic: Traffic,
    /// The compression threshold it asks for; `None` for none.
    pub compression: Option<i32>,
    /// Whether the proxy is in front of it, which asks for the player information forwarding.
    pub behind_proxy: bool,
    pub transfers: Option<Arc<Transfers>>,
    /// A world with nothing in it: the login and the join, no chunks and no entities.
    pub bare: bool,
}

pub(super) fn start_world(s: &Settings, spec: Spec) -> Result<World, String> {
    World::start(Options {
        name: spec.name,
        compression: spec.compression,
        secret: spec.behind_proxy.then(|| s.forwarding.as_bytes().to_vec()),
        shape: if spec.bare {
            Shape {
                view_radius: 0,
                entities: 0,
                ..s.shape
            }
        } else {
            s.shape
        },
        traffic: spec.traffic,
        keepalive: s.keepalive,
        write_per_tick: s.write_per_tick,
        flavour: spec.flavour,
        transfers: spec.transfers,
        bind: s.remote.as_ref().map_or_else(super::world::loopback, |r| {
            // The first port of the range is the lobby's, the second the east server's.
            r.server_address(usize::from(spec.name == "east"))
        }),
    })
    .map_err(|e| e.to_string())
}

pub(super) fn active_traffic(s: &Settings) -> Traffic {
    Traffic::Active {
        packets_per_second: s.down_rate,
        tracked: s.tracked,
        chunks_per_second: s.chunks_per_second,
    }
}

/// The plans of `n` bots called `{prefix}{i}`.
pub(super) fn plans(
    s: &Settings,
    address: SocketAddr,
    n: usize,
    prefix: &str,
    role: Role,
    movers: usize,
    target: Option<&str>,
) -> Vec<Plan> {
    (0..n)
        .map(|i| Plan {
            address,
            name: format!("{prefix}{i}"),
            host: address.ip().to_string(),
            role,
            up_rate: if role == Role::Active { s.up_rate } else { 0 },
            probe_rate: if role == Role::Active {
                s.probe_rate
            } else {
                0.0
            },
            moves_to: (i < movers).then(|| target.map(str::to_owned)).flatten(),
            wave: 0,
        })
        .collect()
}

/// What one wave of bots did.
pub(super) struct WaveResult {
    pub(super) bots: usize,
    pub(super) logged: u64,
    pub(super) failed: u64,
    pub(super) dropped: u64,
    pub(super) logins: Summary,
    /// From the first bot of the wave starting to the last one being in, or giving up.
    pub(super) joined_ms: (u64, u64),
    /// The round trips of the bots of the earlier waves, in the ten seconds before this wave
    /// began, while it joined, and in the ten seconds after (or until the next wave): the figure
    /// of a load that is still arriving. Empty for the first wave.
    pub(super) rest_before: Summary,
    pub(super) rest_during: Summary,
    pub(super) rest_after: Summary,
}

/// Where the bots of each wave started and ended arriving, and their threads.
pub(super) struct Arrivals {
    pub(super) handles: Vec<JoinHandle<()>>,
    /// For each wave: when it began, when its last bot was in (or the wait ran out), its bots.
    pub(super) spans: Vec<(u64, u64, usize)>,
}

/// Brings `n` bots in `s.waves` waves of equal size (the last takes the rest), `s.wave_gap` apart:
/// each wave starts together, and the next one after the gap that follows the last of this one being in.
/// Each wave's start and end are written to the windows file.
pub(super) fn arrive(
    s: &Settings,
    label: &str,
    n: usize,
    live: &Arc<Live>,
    address: SocketAddr,
    prefix: &str,
    role: Role,
) -> Arrivals {
    let waves = s.waves.clamp(1, bot::MAX_WAVES);
    let mut all = plans(s, address, n, prefix, role, 0, None).into_iter();
    let (mut handles, mut spans) = (Vec::new(), Vec::new());
    for w in 0..waves {
        let count = n / waves + if w + 1 == waves { n % waves } else { 0 };
        let mut wave: Vec<Plan> = all.by_ref().take(count).collect();
        for plan in &mut wave {
            plan.wave = w;
        }
        let from = Windows::now_ms();
        handles.extend(launch(s, wave, live));
        let done = |counts: &[AtomicU64; bot::MAX_WAVES]| {
            counts[w].load(Ordering::Relaxed) + live.wave_failed[w].load(Ordering::Relaxed)
                >= count as u64
        };
        wait_until(s, Duration::from_secs(120), || done(&live.wave_logins));
        wait_until(s, Duration::from_secs(60), || done(&live.wave_joined));
        let to = Windows::now_ms();
        let phase = if waves > 1 {
            format!("join wave {}", w + 1)
        } else {
            "join".to_owned()
        };
        s.windows.record(label, n, &phase, from, to);
        spans.push((from, to, count));
        if w + 1 < waves {
            pause(s, s.wave_gap);
        }
    }
    Arrivals { handles, spans }
}

/// The figures of each wave, from what the bots counted.
pub(super) fn wave_results(
    live: &Live,
    spans: &[(u64, u64, usize)],
    measure_from: u64,
) -> Vec<WaveResult> {
    const SIDE: u64 = 10_000;
    let timed = live.timed_round_trips.lock().unwrap().clone();
    let logins = live.wave_login_times.lock().unwrap().clone();
    let rest = |wave: usize, from: u64, to: u64| {
        Summary::of(
            timed
                .iter()
                .filter(|(at, w, _)| *w < wave && *at >= from && *at <= to)
                .map(|(_, _, micros)| us(*micros))
                .collect(),
        )
    };
    spans
        .iter()
        .enumerate()
        .map(|(w, &(from, to, bots))| {
            let next = spans.get(w + 1).map_or(measure_from, |s| s.0);
            WaveResult {
                bots,
                logged: live.wave_logins[w].load(Ordering::Relaxed),
                failed: live.wave_failed[w].load(Ordering::Relaxed),
                dropped: live.wave_dropped[w].load(Ordering::Relaxed),
                logins: Summary::of(logins.get(w).cloned().unwrap_or_default()),
                joined_ms: (from, to),
                rest_before: rest(w, from.saturating_sub(SIDE), from),
                rest_during: rest(w, from, to),
                rest_after: rest(w, to, (to + SIDE).min(next.max(to))),
            }
        })
        .collect()
}

/// Starts the bots, one thread each, `arrival` a second.
pub(super) fn launch(s: &Settings, plans: Vec<Plan>, live: &Arc<Live>) -> Vec<JoinHandle<()>> {
    let mut handles = Vec::new();
    for plan in plans {
        if s.stop.load(Ordering::Relaxed) {
            break;
        }
        let plan_wave = plan.wave;
        let (bot_live, stop) = (Arc::clone(live), Arc::clone(&s.stop));
        let spawned = std::thread::Builder::new()
            .name("load-bot".into())
            .stack_size(256 * 1024)
            .spawn(move || bot::run(&plan, &bot_live, &stop));
        match spawned {
            Ok(handle) => handles.push(handle),
            Err(e) => {
                live.fail_to_start(plan_wave, &e.to_string());
            }
        }
        if s.arrival > 0 {
            std::thread::sleep(Duration::from_secs_f64(1.0 / f64::from(s.arrival)));
        }
    }
    handles
}

/// Waits until `done` is true, `limit` has passed, or the run is stopped.
pub(super) fn wait_until(s: &Settings, limit: Duration, done: impl Fn() -> bool) -> bool {
    let began = Instant::now();
    while began.elapsed() < limit && !s.stop.load(Ordering::Relaxed) {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    done()
}

/// Sleeps `span`, or until the run is stopped.
pub(super) fn pause(s: &Settings, span: Duration) {
    let began = Instant::now();
    while began.elapsed() < span && !s.stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Resident memory, committed memory and threads of the proxy now.
fn proc_now(pid: Pid) -> (u64, u64, u32) {
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing().with_memory(),
    );
    let (rss, commit) = system
        .process(pid)
        .map_or((0, 0), |p| (p.memory(), p.virtual_memory()));
    (rss, commit, crate::cpu::threads(pid).unwrap_or(0))
}

pub(super) fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

pub(super) fn us(value: u32) -> Duration {
    Duration::from_micros(u64::from(value))
}

/// Ends the bots and waits for their threads.
pub(super) fn end(live: &Live, handles: Vec<JoinHandle<()>>) {
    live.close_all();
    for handle in handles {
        let _ = handle.join();
    }
}

/// The first line of a failure, for the row.
pub(super) fn first_failure(live: &Live) -> String {
    live.failures
        .lock()
        .unwrap()
        .first()
        .cloned()
        .unwrap_or_default()
}

/// For finding out why something failed: the proxy's whole log, kept where the run is told.
fn save_log(log: &str) {
    if let Some(path) = std::env::var_os("MERIDIAN_LOAD_SAVE_LOG") {
        let _ = std::fs::write(path, log);
    }
}

/// Memory the machine has free now, in bytes.
fn free_now() -> u64 {
    let mut system = System::new();
    system.refresh_memory();
    system.available_memory()
}

// ---------------------------------------------------------------------------------------------
// 1. Idle

/// S1: `n` bots log in, in a burst (or at the run's arrival rate), and hold the connection.
pub fn idle(s: &Settings, n: usize, bare: bool) -> Result<Row, String> {
    let world = start_world(
        s,
        Spec {
            name: "lobby",
            flavour: 1,
            traffic: Traffic::Idle,
            compression: Some(256),
            behind_proxy: true,
            transfers: None,
            bare,
        },
    )?;
    let mut proxy = start_proxy(
        s,
        &[Server {
            name: "lobby",
            address: world.address,
            sidecar: None,
        }],
        256,
    )?;
    let pid = proxy.pid;
    let (rss0, commit0, threads0) = proc_now(pid);
    let live = Arc::new(Live::default());
    let label = if bare { "S1 idle bare" } else { "S1 idle" };
    let sampler = Sampler::start(
        Watch {
            label: label.into(),
            live: Arc::clone(&live),
            players: n,
            proxy: Some(pid),
        },
        Arc::clone(&s.stop),
        s.min_free,
    );
    let began = Instant::now();
    let handles = launch(
        s,
        plans(s, proxy.address, n, "Idle", Role::Idle, 0, None),
        &live,
    );
    let all =
        |live: &Live| live.logins.load(Ordering::Relaxed) + live.failed.load(Ordering::Relaxed);
    wait_until(s, Duration::from_secs(120), || all(&live) >= n as u64);
    let logins_took = began.elapsed();
    wait_until(s, Duration::from_secs(60), || {
        live.joined.load(Ordering::Relaxed) + live.failed.load(Ordering::Relaxed) >= n as u64
    });
    pause(s, Duration::from_secs(3));
    let (rss_joined, commit_joined, threads_joined) = proc_now(pid);
    let (from, until) = (sampler.now(), sampler.now() + s.window.as_secs_f64());
    pause(s, s.window);
    let window = sampler.between(from, until.min(sampler.now()));
    let (rss1, commit1, threads1) = proc_now(pid);
    let (sent, answered) = (
        world.counters.keepalives_sent.load(Ordering::Relaxed),
        world.counters.keepalives_answered.load(Ordering::Relaxed),
    );
    let alive = proxy.alive();
    let warnings = proxy.warnings();
    let tripped = sampler.tripped.load(Ordering::Relaxed);
    let logins = Summary::of(live.login_times.lock().unwrap().clone());
    let logged = live.logins.load(Ordering::Relaxed);
    // Counted before the bots are ended: ending them closes every connection.
    let dropped = live.dropped.load(Ordering::Relaxed);
    end(&live, handles);
    drop(sampler);
    proxy.stop().map_err(|e| e.to_string())?;
    let players = logged.max(1) as f64;
    Ok(Row::new()
        .with("scenario", if bare { "idle-bare" } else { "idle" })
        .with("players", n)
        .with("logged_in", logged)
        .with("failed", live.failed.load(Ordering::Relaxed))
        .num("login_p50_ms", ms(logins.p50), 1)
        .num("login_p95_ms", ms(logins.p95), 1)
        .num("login_p99_ms", ms(logins.p99), 1)
        .num("login_max_ms", ms(logins.max), 1)
        .num("logins_per_s", logged as f64 / logins_took.as_secs_f64(), 0)
        .num("rss_empty_mib", mib(rss0 as f64), 1)
        .num("rss_joined_mib", mib(rss_joined as f64), 1)
        .num("rss_end_mib", mib(rss1 as f64), 1)
        .num(
            "rss_per_player_kib",
            (rss_joined.saturating_sub(rss0)) as f64 / 1024.0 / players,
            1,
        )
        .num("commit_empty_mib", mib(commit0 as f64), 1)
        .num("commit_end_mib", mib(commit1.max(commit_joined) as f64), 1)
        .with("threads_empty", threads0)
        .with("threads_joined", threads_joined.max(threads1))
        .num("cpu_idle_pct", mean(&window, |x| f64::from(x.cpu)), 2)
        .num("cpu_idle_max_pct", peak(&window, |x| f64::from(x.cpu)), 2)
        .with("keepalives_sent", sent)
        .with("keepalives_answered", answered)
        .with("dropped", dropped)
        .with("warnings", warnings)
        .with("proxy_alive", alive)
        .with("guard_tripped", tripped)
        .with("first_failure", first_failure(&live)))
}

// ---------------------------------------------------------------------------------------------
// 2. Active

/// What one phase of the active scenario measured.
pub(super) struct Played {
    pub(super) logged: u64,
    pub(super) failed: u64,
    pub(super) dropped: u64,
    pub(super) logins: Summary,
    pub(super) round_trips: Summary,
    pub(super) samples: Vec<Sample>,
    pub(super) seconds: f64,
    pub(super) down_bytes: f64,
    pub(super) down_packets: f64,
    pub(super) up_packets: f64,
    pub(super) rss_after: u64,
    pub(super) threads_after: u32,
    pub(super) tripped: bool,
    pub(super) first_failure: String,
    /// The last status line, for a test to say what the run was doing when it failed.
    #[cfg(test)]
    pub(super) status: String,
    /// When the measured window began and ended, in milliseconds since the epoch.
    pub(super) measured_ms: (u64, u64),
    /// How each wave of bots fared, and how the earlier waves' bots did while it arrived.
    pub(super) waves: Vec<WaveResult>,
}

/// `n` active bots against `address` for the window. `proxy` is the process to read, if any.
pub(super) fn play(
    s: &Settings,
    label: &str,
    address: SocketAddr,
    n: usize,
    proxy: Option<&mut Proxy>,
) -> Result<Played, String> {
    let pid = proxy.as_ref().map(|p| p.pid);
    let live = Arc::new(Live::default());
    let sampler = Sampler::start(
        Watch {
            label: label.into(),
            live: Arc::clone(&live),
            players: n,
            proxy: pid,
        },
        Arc::clone(&s.stop),
        s.min_free,
    );
    if s.waves > 1 {
        live.timed.store(true, Ordering::Relaxed);
    }
    let arrivals = arrive(s, label, n, &live, address, "Act", Role::Active);
    let handles = arrivals.handles;
    // The join of every player sends a whole world: let it be taken before the window opens.
    pause(s, Duration::from_secs(5));
    live.recent_round_trips.lock().unwrap().clear();
    live.round_trips.lock().unwrap().clear();
    let before = (
        live.down_bytes.load(Ordering::Relaxed),
        live.down_packets.load(Ordering::Relaxed),
        live.up_packets.load(Ordering::Relaxed),
    );
    live.measuring.store(true, Ordering::Relaxed);
    let (from, began) = (sampler.now(), Instant::now());
    let measured_from = Windows::now_ms();
    pause(s, s.window);
    live.measuring.store(false, Ordering::Relaxed);
    let measured_ms = (measured_from, Windows::now_ms());
    s.windows
        .record(label, n, "measure", measured_ms.0, measured_ms.1);
    let (until, seconds) = (sampler.now(), began.elapsed().as_secs_f64());
    let after = (
        live.down_bytes.load(Ordering::Relaxed),
        live.down_packets.load(Ordering::Relaxed),
        live.up_packets.load(Ordering::Relaxed),
    );
    let (rss_after, _, threads_after) = pid.map_or((0, 0, 0), proc_now);
    let samples = sampler.between(from, until);
    let tripped = sampler.tripped.load(Ordering::Relaxed);
    let round_trips: Vec<Duration> = live
        .round_trips
        .lock()
        .unwrap()
        .iter()
        .map(|&v| us(v))
        .collect();
    let played = Played {
        logged: live.logins.load(Ordering::Relaxed),
        failed: live.failed.load(Ordering::Relaxed),
        dropped: live.dropped.load(Ordering::Relaxed),
        logins: Summary::of(live.login_times.lock().unwrap().clone()),
        round_trips: Summary::of(round_trips),
        samples,
        seconds,
        down_bytes: (after.0 - before.0) as f64,
        down_packets: (after.1 - before.1) as f64,
        up_packets: (after.2 - before.2) as f64,
        rss_after,
        threads_after,
        tripped,
        first_failure: first_failure(&live),
        #[cfg(test)]
        status: live.status.lock().map(|s| s.clone()).unwrap_or_default(),
        measured_ms,
        waves: wave_results(&live, &arrivals.spans, measured_ms.0),
    };
    end(&live, handles);
    drop(sampler);
    Ok(played)
}

/// S2: the same `n` active bots straight to the server, then through the proxy.
pub fn active(
    s: &Settings,
    direct_settings: &Settings,
    n: usize,
    compression: Compression,
) -> Result<Row, String> {
    let direct = {
        let world = start_world(
            s,
            Spec {
                name: "lobby",
                flavour: 1,
                traffic: active_traffic(s),
                compression: compression.server(),
                behind_proxy: false,
                transfers: None,
                bare: false,
            },
        )?;
        let mut direct = play(direct_settings, "S2 direct", world.address, n, None)?;
        // The direct phase reads no proxy: its processor use is this program's.
        direct.samples.clear();
        direct
    };
    let world = start_world(
        s,
        Spec {
            name: "lobby",
            flavour: 1,
            traffic: active_traffic(s),
            compression: compression.server(),
            behind_proxy: true,
            transfers: None,
            bare: false,
        },
    )?;
    let mut proxy = start_proxy(
        s,
        &[Server {
            name: "lobby",
            address: world.address,
            sidecar: None,
        }],
        compression.player(),
    )?;
    let (rss0, _, threads0) = proc_now(proxy.pid);
    let address = proxy.address;
    let proxied = play(s, "S2 proxy", address, n, Some(&mut proxy))?;
    let alive = proxy.alive();
    let warnings = proxy.warnings();
    proxy.stop().map_err(|e| e.to_string())?;
    let (cpu_mean, cpu_p95) = (
        mean(&proxied.samples, |x| f64::from(x.cpu)),
        percentile(&proxied.samples, 95.0, |x| f64::from(x.cpu)),
    );
    let players = proxied.logged.max(1) as f64;
    let (d, p) = (direct.round_trips, proxied.round_trips);
    let added = |a: Duration, b: Duration| (a.as_secs_f64() - b.as_secs_f64()) * 1e6;
    Ok(Row::new()
        .with("scenario", "active")
        .with("players", n)
        .with("compression", compression.name())
        .with("logged_in", proxied.logged)
        .with("failed", proxied.failed + direct.failed)
        .with("dropped", proxied.dropped + direct.dropped)
        .num("direct_rtt_p50_us", d.p50.as_secs_f64() * 1e6, 0)
        .num("direct_rtt_p95_us", d.p95.as_secs_f64() * 1e6, 0)
        .num("direct_rtt_p99_us", d.p99.as_secs_f64() * 1e6, 0)
        .num("proxy_rtt_p50_us", p.p50.as_secs_f64() * 1e6, 0)
        .num("proxy_rtt_p95_us", p.p95.as_secs_f64() * 1e6, 0)
        .num("proxy_rtt_p99_us", p.p99.as_secs_f64() * 1e6, 0)
        .num("proxy_rtt_max_us", p.max.as_secs_f64() * 1e6, 0)
        .num("added_p50_us", added(p.p50, d.p50), 0)
        .num("added_p95_us", added(p.p95, d.p95), 0)
        .num("added_p99_us", added(p.p99, d.p99), 0)
        .with("rtt_samples", p.count)
        .num("proxy_cpu_mean_pct", cpu_mean, 1)
        .num("proxy_cpu_p95_pct", cpu_p95, 1)
        .num("proxy_cpu_per_player_pct", cpu_mean / players, 3)
        .num(
            "bench_cpu_mean_pct",
            mean(&proxied.samples, |x| f64::from(x.bench_cpu)),
            0,
        )
        .num(
            "bench_rss_peak_mib",
            peak(&proxied.samples, |x| mib(x.bench_rss as f64)),
            0,
        )
        .num("rss_empty_mib", mib(rss0 as f64), 1)
        .num("rss_loaded_mib", mib(proxied.rss_after as f64), 1)
        .num(
            "rss_per_player_kib",
            proxied.rss_after.saturating_sub(rss0) as f64 / 1024.0 / players,
            1,
        )
        .num(
            "rss_peak_mib",
            peak(&proxied.samples, |x| mib(x.rss as f64)),
            1,
        )
        .num(
            "commit_peak_mib",
            peak(&proxied.samples, |x| mib(x.commit as f64)),
            1,
        )
        .with("threads_empty", threads0)
        .with("threads_loaded", proxied.threads_after)
        .num(
            "down_mb_per_s",
            proxied.down_bytes / proxied.seconds / 1e6,
            2,
        )
        .num(
            "down_packets_per_s",
            proxied.down_packets / proxied.seconds,
            0,
        )
        .num("up_packets_per_s", proxied.up_packets / proxied.seconds, 0)
        .num("window_s", proxied.seconds, 1)
        .num("login_p50_ms", ms(proxied.logins.p50), 1)
        .num("login_p99_ms", ms(proxied.logins.p99), 1)
        .with("warnings", warnings)
        .with("proxy_alive", alive)
        .with("guard_tripped", proxied.tripped || direct.tripped)
        .with("first_failure", proxied.first_failure))
}

// ---------------------------------------------------------------------------------------------
// 3. Moves

/// What the proxy logs of a switch that was done: its steps and what it sent.
#[derive(Debug, PartialEq)]
pub struct Moved {
    pub took: u64,
    pub prepare: u64,
    pub freeze: u64,
    pub hand_over: u64,
    pub commit: u64,
    pub packets: u64,
    pub bytes: u64,
    pub chunks: u64,
    pub entities: u64,
}

/// Reads the line `Moved::describe` of the proxy writes:
/// `X moved from A to B in 120 ms (prepare 50 ms, freeze 10 ms, hand over 40 ms, commit 20 ms):
/// 200 packets, 123456 bytes (169 chunks, 30 entities)`.
pub fn parse_moved(line: &str) -> Option<Moved> {
    let rest = line.split_once(" moved from ")?.1;
    let number = |text: &str, before: &str| -> Option<u64> {
        let end = text.find(before)?;
        text[..end]
            .rsplit(|c: char| !c.is_ascii_digit())
            .next()?
            .parse()
            .ok()
    };
    Some(Moved {
        took: number(rest.split_once(" in ")?.1, " ms (prepare")?,
        prepare: number(rest.split_once("(prepare ")?.1, " ms,")?,
        freeze: number(rest.split_once("freeze ")?.1, " ms,")?,
        hand_over: number(rest.split_once("hand over ")?.1, " ms,")?,
        commit: number(rest.split_once("commit ")?.1, " ms)")?,
        packets: number(rest.split_once("): ")?.1, " packets")?,
        bytes: number(rest.split_once("packets, ")?.1, " bytes")?,
        chunks: number(rest.split_once("bytes (")?.1, " chunks")?,
        entities: number(rest.split_once("chunks, ")?.1, " entities")?,
    })
}

/// The median of numbers, 0 for none.
fn median(mut values: Vec<u64>) -> f64 {
    values.sort_unstable();
    values.get(values.len() / 2).copied().unwrap_or(0) as f64
}

/// S3: `n` bots play on the first server, and `movers_percent` of them are moved to the second
/// at once, as they would by typing `/server`.
pub fn moves(s: &Settings, n: usize) -> Result<Row, String> {
    let movers = (n * s.movers_percent / 100).max(1);
    let transfers = Arc::new(Transfers::default());
    let lobby = start_world(
        s,
        Spec {
            name: "lobby",
            flavour: 1,
            traffic: active_traffic(s),
            compression: Some(256),
            behind_proxy: true,
            transfers: Some(Arc::clone(&transfers)),
            bare: false,
        },
    )?;
    let east = start_world(
        s,
        Spec {
            name: "east",
            flavour: 2,
            traffic: active_traffic(s),
            compression: Some(256),
            behind_proxy: true,
            transfers: Some(Arc::clone(&transfers)),
            bare: false,
        },
    )?;
    let sidecars = [
        Sidecar::start("lobby", Arc::clone(&transfers)).map_err(|e| e.to_string())?,
        Sidecar::start("east", Arc::clone(&transfers)).map_err(|e| e.to_string())?,
    ];
    let mut proxy = start_proxy(
        s,
        &[
            Server {
                name: "lobby",
                address: lobby.address,
                sidecar: Some(&sidecars[0].address),
            },
            Server {
                name: "east",
                address: east.address,
                sidecar: Some(&sidecars[1].address),
            },
        ],
        256,
    )?;
    let pid = proxy.pid;
    let (rss0, _, threads0) = proc_now(pid);
    let live = Arc::new(Live::default());
    let sampler = Sampler::start(
        Watch {
            label: "S3 moves".into(),
            live: Arc::clone(&live),
            players: n,
            proxy: Some(pid),
        },
        Arc::clone(&s.stop),
        s.min_free,
    );
    let handles = launch(
        s,
        plans(
            s,
            proxy.address,
            n,
            "Mov",
            Role::Active,
            movers,
            Some("east"),
        ),
        &live,
    );
    wait_until(s, Duration::from_secs(120), || {
        live.logins.load(Ordering::Relaxed) + live.failed.load(Ordering::Relaxed) >= n as u64
    });
    wait_until(s, Duration::from_secs(60), || {
        live.joined.load(Ordering::Relaxed) + live.failed.load(Ordering::Relaxed) >= n as u64
    });
    pause(s, Duration::from_secs(5));
    // A quiet stretch first: what the proxy holds and how the others' round trips are before.
    live.measuring.store(true, Ordering::Relaxed);
    live.round_trips.lock().unwrap().clear();
    let (b0, b1) = (sampler.now(), sampler.now() + 8.0);
    pause(s, Duration::from_secs(8));
    let quiet = sampler.between(b0, b1);
    let quiet_rtt = Summary::of(
        std::mem::take(&mut *live.round_trips.lock().unwrap())
            .into_iter()
            .map(us)
            .collect(),
    );
    let (rss_before, _, threads_before) = proc_now(pid);
    // The moves, all at once.
    let began = sampler.now();
    live.move_now.store(true, Ordering::Relaxed);
    let all_moved = wait_until(s, Duration::from_secs(60), || {
        (live.moves.lock().unwrap().len() as u64 + live.moves_failed.load(Ordering::Relaxed))
            >= movers as u64
    });
    let moved_at = sampler.now();
    pause(s, Duration::from_secs(6));
    let during = sampler.between(began, sampler.now());
    let moving_rtt = Summary::of(
        std::mem::take(&mut *live.round_trips.lock().unwrap())
            .into_iter()
            .map(us)
            .collect(),
    );
    let (rss_after, _, threads_after) = proc_now(pid);
    let tripped = sampler.tripped.load(Ordering::Relaxed);
    let durations = Summary::of(live.moves.lock().unwrap().clone());
    let done = live.moves.lock().unwrap().len();
    let failed_moves = live.moves_failed.load(Ordering::Relaxed);
    let log = proxy.log_text();
    save_log(&log);
    let logged: Vec<Moved> = log.lines().filter_map(parse_moved).collect();
    let alive = proxy.alive();
    let warnings = proxy.warnings();
    let arrivals = east.counters.arrivals.load(Ordering::Relaxed);
    let on_east = east.counters.online.load(Ordering::Relaxed);
    let asked = &transfers.asked;
    let mut first = first_failure(&live);
    if done as u64 + failed_moves < movers as u64 {
        // A move nobody answered: what the proxy said about moves, to find out why.
        let said: Vec<&str> = log
            .lines()
            .filter(|l| l.contains("switch") || l.contains("move"))
            .filter(|l| !l.contains(" moved from "))
            .take(3)
            .collect();
        first = format!("{first} {}", said.join(" | "));
    }
    end(&live, handles);
    drop(sampler);
    proxy.stop().map_err(|e| e.to_string())?;
    let _ = (&lobby.counters, &Counters::default());
    let per = |f: fn(&Moved) -> u64| median(logged.iter().map(f).collect());
    Ok(Row::new()
        .with("scenario", "moves")
        .with("players", n)
        .with("movers", movers)
        .with("moved", done)
        .with("move_failed", failed_moves)
        .with("all_answered", all_moved)
        .num("move_p50_ms", ms(durations.p50), 0)
        .num("move_p95_ms", ms(durations.p95), 0)
        .num("move_max_ms", ms(durations.max), 0)
        .num("burst_took_ms", (moved_at - began) * 1e3, 0)
        .num("proxy_prepare_ms", per(|m| m.prepare), 0)
        .num("proxy_freeze_ms", per(|m| m.freeze), 0)
        .num("proxy_hand_over_ms", per(|m| m.hand_over), 0)
        .num("proxy_commit_ms", per(|m| m.commit), 0)
        .num("proxy_total_ms", per(|m| m.took), 0)
        .num("delta_packets", per(|m| m.packets), 0)
        .num("delta_kib", per(|m| m.bytes) / 1024.0, 0)
        .num("delta_chunks", per(|m| m.chunks), 0)
        .num("delta_entities", per(|m| m.entities), 0)
        .num("rss_empty_mib", mib(rss0 as f64), 1)
        .num("rss_before_mib", mib(rss_before as f64), 1)
        .num("rss_peak_mib", peak(&during, |x| mib(x.rss as f64)), 1)
        .num("rss_after_mib", mib(rss_after as f64), 1)
        .num(
            "rss_peak_extra_per_mover_kib",
            (peak(&during, |x| x.rss as f64) - rss_before as f64) / 1024.0 / movers as f64,
            0,
        )
        .with("threads_before", threads_before.max(threads0))
        .num("threads_peak", peak(&during, |x| f64::from(x.threads)), 0)
        .with("threads_after", threads_after)
        .num("cpu_quiet_pct", mean(&quiet, |x| f64::from(x.cpu)), 1)
        .num(
            "cpu_moving_peak_pct",
            peak(&during, |x| f64::from(x.cpu)),
            1,
        )
        .num(
            "others_rtt_p99_quiet_us",
            quiet_rtt.p99.as_secs_f64() * 1e6,
            0,
        )
        .num(
            "others_rtt_p99_moving_us",
            moving_rtt.p99.as_secs_f64() * 1e6,
            0,
        )
        .num(
            "others_rtt_max_moving_us",
            moving_rtt.max.as_secs_f64() * 1e6,
            0,
        )
        .with("arrivals_on_east", arrivals)
        .with("online_on_east", on_east)
        .with("sidecar_imports", asked.import.load(Ordering::Relaxed))
        .with("warnings", warnings)
        .with("proxy_alive", alive)
        .with("guard_tripped", tripped)
        .with("first_failure", first))
}

// ---------------------------------------------------------------------------------------------
// 4. Churn

/// How many sockets are in TIME_WAIT on this machine, or `None` when it cannot be read: a
/// run of churn spends the machine's ephemeral ports faster than the system gives them back.
pub fn time_wait() -> Option<usize> {
    let out = std::process::Command::new("netstat")
        .args(["-an", "-p", "tcp"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .or_else(|| {
            std::process::Command::new("netstat")
                .arg("-an")
                .output()
                .ok()
                .filter(|o| o.status.success())
        })?;
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| l.contains("TIME_WAIT"))
            .count(),
    )
}

/// Waits for the sockets of an earlier run to leave TIME_WAIT, up to five minutes.
pub fn let_ports_rest(s: &Settings, under: usize) {
    let began = Instant::now();
    while let Some(waiting) = time_wait() {
        if waiting < under
            || began.elapsed() > Duration::from_secs(300)
            || s.stop.load(Ordering::Relaxed)
        {
            break;
        }
        eprintln!("meridian-bench: {waiting} sockets in TIME_WAIT, waiting for under {under}");
        std::thread::sleep(Duration::from_secs(10));
    }
}

/// S4: `n` bots that each connect, stay a moment, leave, and do it again, until `cycles` have
/// been made or the window has passed.
pub fn churn(s: &Settings, n: usize, cycles: u64) -> Result<Row, String> {
    let world = start_world(
        s,
        Spec {
            name: "lobby",
            flavour: 1,
            traffic: Traffic::Idle,
            compression: Some(256),
            behind_proxy: true,
            transfers: None,
            bare: true,
        },
    )?;
    let mut proxy = start_proxy(
        s,
        &[Server {
            name: "lobby",
            address: world.address,
            sidecar: None,
        }],
        256,
    )?;
    let pid = proxy.pid;
    let (rss0, _, threads0) = proc_now(pid);
    let waiting_before = time_wait();
    let live = Arc::new(Live::default());
    let sampler = Sampler::start(
        Watch {
            label: "S4 churn".into(),
            live: Arc::clone(&live),
            players: n,
            proxy: Some(pid),
        },
        Arc::clone(&s.stop),
        s.min_free,
    );
    let made = Arc::new(AtomicU64::new(0));
    let (from, began) = (sampler.now(), Instant::now());
    let handles: Vec<_> = (0..n)
        .filter_map(|worker| {
            let (live, stop, made) = (Arc::clone(&live), Arc::clone(&s.stop), Arc::clone(&made));
            let (address, window) = (proxy.address, s.window);
            let hold_ms = s.churn_hold.as_millis() as u64;
            std::thread::Builder::new()
                .name("load-churn".into())
                .stack_size(256 * 1024)
                .spawn(move || {
                    let mut cycle = 0u64;
                    while began.elapsed() < window
                        && made.load(Ordering::Relaxed) < cycles
                        && !stop.load(Ordering::Relaxed)
                    {
                        let plan = Plan {
                            address,
                            name: format!("Ch{worker}x{cycle}"),
                            host: address.ip().to_string(),
                            role: Role::Idle,
                            up_rate: 0,
                            probe_rate: 0.0,
                            moves_to: None,
                            wave: 0,
                        };
                        cycle += 1;
                        // Stays a while, as a player who joins and leaves at once does.
                        let spread = hold_ms.max(2);
                        let hold = Duration::from_millis(
                            spread / 2 + (worker as u64 * 37 + cycle * 101) % spread,
                        );
                        bot::visit(&plan, &live, &stop, hold);
                        made.fetch_add(1, Ordering::Relaxed);
                    }
                })
                .ok()
        })
        .collect();
    for handle in handles {
        let _ = handle.join();
    }
    let seconds = began.elapsed().as_secs_f64();
    let until = sampler.now();
    let during = sampler.between(from, until);
    // The proxy at rest again: its threads and memory must come back.
    pause(s, Duration::from_secs(5));
    let (rss1, _, threads1) = proc_now(pid);
    let waiting_after = time_wait();
    let tripped = sampler.tripped.load(Ordering::Relaxed);
    let logins = Summary::of(live.login_times.lock().unwrap().clone());
    let done = made.load(Ordering::Relaxed);
    let alive = proxy.alive();
    let warnings = proxy.warnings();
    save_log(&proxy.log_text());
    let first = first_failure(&live);
    let failed = live.failed.load(Ordering::Relaxed);
    let exhausted = live.exhausted.load(Ordering::Relaxed);
    let logged = live.logins.load(Ordering::Relaxed);
    drop(sampler);
    proxy.stop().map_err(|e| e.to_string())?;
    Ok(Row::new()
        .with("scenario", "churn")
        .with("workers", n)
        .with("cycles", done)
        .with("logged_in", logged)
        .with("failed", failed)
        .with("out_of_ports", exhausted)
        .num("cycles_per_s", done as f64 / seconds, 0)
        .num("login_p50_ms", ms(logins.p50), 1)
        .num("login_p95_ms", ms(logins.p95), 1)
        .num("login_p99_ms", ms(logins.p99), 1)
        .num("login_max_ms", ms(logins.max), 1)
        .num("proxy_cpu_mean_pct", mean(&during, |x| f64::from(x.cpu)), 1)
        .num("proxy_cpu_peak_pct", peak(&during, |x| f64::from(x.cpu)), 1)
        .num("rss_empty_mib", mib(rss0 as f64), 1)
        .num("rss_peak_mib", peak(&during, |x| mib(x.rss as f64)), 1)
        .num("rss_after_mib", mib(rss1 as f64), 1)
        .with("threads_empty", threads0)
        .num("threads_peak", peak(&during, |x| f64::from(x.threads)), 0)
        .with("threads_after", threads1)
        .with(
            "time_wait_before",
            waiting_before.map_or(String::new(), |n| n.to_string()),
        )
        .with(
            "time_wait_after",
            waiting_after.map_or(String::new(), |n| n.to_string()),
        )
        .with("warnings", warnings)
        .with("proxy_alive", alive)
        .with("guard_tripped", tripped)
        .with("first_failure", first))
}

/// Checks the machine has the floor of free memory before a step; the sampler checks during.
pub fn check_memory(s: &Settings) -> Result<(), String> {
    let free = free_now();
    if free < s.min_free {
        return Err(format!(
            "the machine has {:.1} GiB of memory free, under the floor of {:.1} GiB",
            free as f64 / (1u64 << 30) as f64,
            s.min_free as f64 / (1u64 << 30) as f64
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_of_the_proxy_about_a_move_is_read() {
        let line = "2026-11-05T10:00:00Z INFO Mov3 moved from lobby to east in 123 ms (prepare 50 ms, \
                    freeze 10 ms, hand over 40 ms, commit 20 ms): 200 packets, 123456 bytes (169 chunks, \
                    30 entities)";
        assert_eq!(
            parse_moved(line),
            Some(Moved {
                took: 123,
                prepare: 50,
                freeze: 10,
                hand_over: 40,
                commit: 20,
                packets: 200,
                bytes: 123_456,
                chunks: 169,
                entities: 30,
            })
        );
        assert_eq!(parse_moved("Mov3 could not move from lobby to east"), None);
    }

    #[test]
    fn the_configuration_names_each_server_its_sidecar_and_the_spectator_host() {
        let servers = [
            Server {
                name: "lobby",
                address: "127.0.0.1:1".parse().unwrap(),
                sidecar: Some("127.0.0.1:2"),
            },
            Server {
                name: "east",
                address: "127.0.0.1:3".parse().unwrap(),
                sidecar: None,
            },
        ];
        let spectator = Spectator {
            address: "127.0.0.1:9".into(),
            host: "localhost".into(),
            port: 25565,
        };
        let text = configuration(
            "127.0.0.1:25565".parse().unwrap(),
            &servers,
            256,
            FORWARDING,
            Some(&spectator),
        );
        for needle in [
            "default-backend = \"lobby\"",
            "sidecar = \"127.0.0.1:2\"",
            "sidecar-secret-file = \"lobby.key\"",
            "[[forced-host]]\nhost = \"localhost\"\nbackend = \"spectator\"",
            "name = \"spectator\"\naddress = \"127.0.0.1:9\"",
        ] {
            assert!(text.contains(needle), "{needle} missing from {text}");
        }
    }

    /// What the harness writes is a configuration the proxy accepts, and with a spectator the host
    /// `localhost` leads to the real server while the bots' `127.0.0.1` does not.
    #[test]
    fn the_configuration_is_one_the_proxy_reads_and_routes_by_host() {
        let dir = std::env::temp_dir().join(format!("meridian-load-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("lobby.key"), super::super::transfer::SECRET).unwrap();
        std::fs::write(dir.join("east.key"), super::super::transfer::SECRET).unwrap();
        let servers = [
            Server {
                name: "lobby",
                address: "127.0.0.1:25601".parse().unwrap(),
                sidecar: Some("127.0.0.1:25580"),
            },
            Server {
                name: "east",
                address: "127.0.0.1:25602".parse().unwrap(),
                sidecar: Some("127.0.0.1:25581"),
            },
        ];
        let spectator = Spectator {
            address: "127.0.0.1:25610".into(),
            host: "localhost".into(),
            port: 25565,
        };
        let text = configuration(
            "127.0.0.1:25565".parse().unwrap(),
            &servers,
            256,
            FORWARDING,
            Some(&spectator),
        );
        let config = meridian_config::Config::from_toml_str_in(&text, &dir).expect(&text);
        assert_eq!(config.forced_backend("localhost"), Some("spectator"));
        assert_eq!(config.forced_backend("LOCALHOST."), Some("spectator"));
        assert_eq!(config.forced_backend("127.0.0.1"), None);
        assert_eq!(config.default_backend, "lobby");
        assert_eq!(config.sidecars.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn compressions_say_what_each_side_does() {
        assert_eq!(Compression::Same.server(), Some(256));
        assert_eq!(Compression::ServerOff.server(), None);
        assert_eq!(Compression::ServerOff.player(), 256);
        assert_eq!(Compression::BothOff.player(), -1);
    }
}
