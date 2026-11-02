//! One phase of a run: N players against one address, the direct backend or the
//! proxy, with the same packets at the same rate, and what the machine and the
//! proxy process did during the measured window.
use crate::cpu;
use crate::player::{self, Failure, Plan};
use crate::stats::Summary;
use std::net::SocketAddr;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

/// What every player of a phase does.
#[derive(Clone, Copy, Debug)]
pub struct Load {
    pub players: usize,
    pub window: Duration,
    pub rate: u32,
    pub size: usize,
}

/// What a phase measured.
#[derive(Clone, Debug, Default)]
pub struct Phase {
    pub logged_in: usize,
    pub failed: usize,
    /// Failures because the machine ran out of ports or descriptors.
    pub exhausted: usize,
    /// The first few failures, as they were reported.
    pub failures: Vec<String>,
    /// From the first connection to the last player logged in or failed.
    pub logins_took: Duration,
    pub login: Summary,
    pub round_trip: Summary,
    pub packets: u64,
    pub bytes: u64,
    /// The measured window, as it really lasted.
    pub window: Duration,
    /// CPU over the window, in percent of one core: the whole machine (out of
    /// 100 × cores), this program (players and backend), and the proxy.
    pub machine_cpu: f32,
    pub bench_cpu: f32,
    pub proxy_cpu: Option<f32>,
    /// The proxy's resident memory at the end of the window.
    pub proxy_memory: Option<u64>,
}

/// Runs `load` against `target`. `proxy` is the process to watch, if any.
pub fn phase(target: SocketAddr, load: Load, proxy: Option<Pid>, stop: &Arc<AtomicBool>) -> Phase {
    // A random pause first, so the players do not arrive at the same point of a
    // cycle of the target each time, such as the proxy's polling of its listener,
    // which the run's own fixed waits would otherwise line up with.
    std::thread::sleep(jitter(Duration::from_millis(100)));
    let start = Arc::new(Barrier::new(load.players + 1));
    let began = Instant::now();
    let players: Vec<_> = (0..load.players)
        .map(|i| {
            let plan = Plan {
                address: target,
                name: format!("Bench{i}"),
                rate: load.rate,
                size: load.size,
                window: load.window,
            };
            let (start, stop) = (Arc::clone(&start), Arc::clone(stop));
            std::thread::Builder::new()
                .name(format!("bench-player-{i}"))
                .stack_size(256 * 1024)
                .spawn(move || player::run(&plan, &start, &stop))
        })
        .collect();
    // Every player has logged in or failed: the window opens for all at once.
    // A player whose thread could not start counts as failed, and the barrier
    // still needs its place filled.
    let unstarted = players.iter().filter(|p| p.is_err()).count();
    let filler: Vec<_> = (0..unstarted)
        .map(|_| {
            let start = Arc::clone(&start);
            std::thread::spawn(move || {
                start.wait();
            })
        })
        .collect();
    start.wait();
    let logins_took = began.elapsed();
    let me = sysinfo::get_current_pid().ok();
    let watched: Vec<Pid> = me.into_iter().chain(proxy).collect();
    let mut system = System::new();
    system.refresh_cpu_usage();
    let meter = cpu::Meter::start(&watched);
    let window_began = Instant::now();
    let mut phase = Phase {
        logins_took,
        ..Phase::default()
    };
    let mut logins = Vec::new();
    let mut round_trips = Vec::new();
    for player in players {
        let outcome = match player.map(|p| p.join()) {
            Ok(Ok(outcome)) => outcome,
            _ => player::Outcome {
                failure: Some(Failure::Other("the player's thread did not run".into())),
                ..player::Outcome::default()
            },
        };
        if let Some(took) = outcome.login {
            phase.logged_in += 1;
            logins.push(took);
        }
        if let Some(failure) = outcome.failure {
            phase.failed += 1;
            let text = match failure {
                Failure::Exhausted(why) => {
                    phase.exhausted += 1;
                    format!("out of ports or descriptors: {why}")
                }
                Failure::Other(why) => why,
            };
            if phase.failures.len() < 3 {
                phase.failures.push(text);
            }
        }
        phase.packets += outcome.packets_sent;
        phase.bytes += outcome.bytes_sent;
        round_trips.extend(outcome.round_trips);
    }
    for f in filler {
        let _ = f.join();
    }
    phase.window = window_began.elapsed();
    let mut usage = meter.read().into_iter();
    phase.bench_cpu = me.and_then(|_| usage.next().flatten()).unwrap_or(0.0);
    phase.proxy_cpu = proxy.and_then(|_| usage.next().flatten());
    system.refresh_cpu_usage();
    phase.machine_cpu = system.global_cpu_usage() * system.cpus().len() as f32;
    phase.proxy_memory = proxy.and_then(memory_of);
    phase.login = Summary::of(logins);
    phase.round_trip = Summary::of(round_trips);
    phase
}

/// A random duration under `limit`.
fn jitter(limit: Duration) -> Duration {
    use std::hash::{BuildHasher, Hasher};
    // Each RandomState is seeded differently, which is all the randomness needed.
    let random = std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish();
    Duration::from_nanos(random % limit.as_nanos().max(1) as u64)
}

/// The proxy's resident memory now, before any player.
pub fn memory_of(pid: Pid) -> Option<u64> {
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing().with_memory(),
    );
    system.process(pid).map(|p| p.memory())
}
