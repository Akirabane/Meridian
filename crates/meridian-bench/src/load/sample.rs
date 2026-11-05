//! What a run reads off the proxy process once a second: resident memory, committed memory,
//! threads, processor use, and how much memory the machine has left. The same reading is the
//! live status line, one line a second on the standard error, so that the run can be watched.
//!
//! The guards are here too: the sampler stops the run when the memory the machine has free falls
//! below the floor the run was given, when the proxy process is gone, when more than a tenth of the
//! bots failed or were dropped, or when a reading of one second takes over four.
use super::bot::Live;
use crate::cpu;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

const MIB: f64 = 1024.0 * 1024.0;
const GIB: f64 = MIB * 1024.0;

/// One reading of the proxy.
#[derive(Clone, Copy, Debug, Default)]
pub struct Sample {
    /// Seconds since the sampler started.
    pub at: f64,
    /// Resident memory (the working set on Windows), in bytes.
    pub rss: u64,
    /// Memory committed to the process (private bytes on Windows), in bytes.
    pub commit: u64,
    pub threads: u32,
    /// The proxy's processor use over the last second, in percent of one core.
    pub cpu: f32,
    /// This program's, as the proxy's, so that the load's own cost is on the page.
    pub bench_cpu: f32,
    /// This program's resident memory, in bytes: the swarm and the synthetic servers.
    pub bench_rss: u64,
    /// Memory the machine has available, in bytes.
    pub free: u64,
}

/// What the run tells the status line.
pub struct Watch {
    /// A few words: the scenario and the step.
    pub label: String,
    /// The bots the run launched.
    pub live: Arc<Live>,
    /// How many bots the step has.
    pub players: usize,
    /// The proxy's process, or `None` for a direct phase, which has nothing to read.
    pub proxy: Option<Pid>,
}

/// The sampler's thread and what it read.
pub struct Sampler {
    began: Instant,
    stop: Arc<AtomicBool>,
    samples: Arc<Mutex<Vec<Sample>>>,
    /// Set when the memory guard stopped the run.
    pub tripped: Arc<AtomicBool>,
}

impl Sampler {
    /// Starts reading once a second. `run_stop` is set when free memory falls under
    /// `min_free` bytes.
    pub fn start(watch: Watch, run_stop: Arc<AtomicBool>, min_free: u64) -> Sampler {
        let began = Instant::now();
        let stop = Arc::new(AtomicBool::new(false));
        let samples = Arc::new(Mutex::new(Vec::new()));
        let tripped = Arc::new(AtomicBool::new(false));
        {
            let (stop, samples, tripped) = (
                Arc::clone(&stop),
                Arc::clone(&samples),
                Arc::clone(&tripped),
            );
            // Ends by itself within a second of the flag, when the sampler is dropped.
            let _ = std::thread::Builder::new()
                .name("load-sampler".into())
                .spawn(move || {
                    follow(
                        &watch, began, &stop, &samples, &tripped, &run_stop, min_free,
                    )
                });
        }
        Sampler {
            began,
            stop,
            samples,
            tripped,
        }
    }

    /// Seconds since the sampler started: the clock the readings are on.
    pub fn now(&self) -> f64 {
        self.began.elapsed().as_secs_f64()
    }

    /// The readings so far.
    pub fn samples(&self) -> Vec<Sample> {
        self.samples.lock().unwrap().clone()
    }

    /// The readings between two moments of [`Sampler::now`].
    pub fn between(&self, from: f64, to: f64) -> Vec<Sample> {
        self.samples()
            .into_iter()
            .filter(|s| s.at >= from && s.at <= to)
            .collect()
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn follow(
    watch: &Watch,
    began: Instant,
    stop: &AtomicBool,
    samples: &Mutex<Vec<Sample>>,
    tripped: &AtomicBool,
    run_stop: &AtomicBool,
    min_free: u64,
) {
    let me = sysinfo::get_current_pid().ok();
    let pids: Vec<Pid> = watch.proxy.into_iter().chain(me).collect();
    let mut last = (
        0u64,
        watch.live.logins.load(Ordering::Relaxed),
        watch.live.down_bytes.load(Ordering::Relaxed),
    );
    while !stop.load(Ordering::Relaxed) {
        let meter = cpu::Meter::start(&pids);
        let second = Instant::now();
        while second.elapsed() < Duration::from_secs(1) {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let usage = meter.read();
        let (proxy_cpu, bench_cpu) = match (watch.proxy, me) {
            (Some(_), Some(_)) => (
                usage.first().copied().flatten(),
                usage.get(1).copied().flatten(),
            ),
            (None, Some(_)) => (None, usage.first().copied().flatten()),
            _ => (None, None),
        };
        let mut system = System::new();
        system.refresh_memory();
        let free = system.available_memory();
        let bench_rss = me.map_or(0, |pid| {
            system.refresh_processes_specifics(
                ProcessesToUpdate::Some(&[pid]),
                true,
                ProcessRefreshKind::nothing().with_memory(),
            );
            system.process(pid).map_or(0, |p| p.memory())
        });
        let (rss, commit) = match watch.proxy {
            Some(pid) => {
                system.refresh_processes_specifics(
                    ProcessesToUpdate::Some(&[pid]),
                    true,
                    ProcessRefreshKind::nothing().with_memory(),
                );
                system
                    .process(pid)
                    .map_or((0, 0), |p| (p.memory(), p.virtual_memory()))
            }
            None => (0, 0),
        };
        let sample = Sample {
            at: began.elapsed().as_secs_f64(),
            rss,
            commit,
            threads: watch.proxy.and_then(cpu::threads).unwrap_or(0),
            cpu: proxy_cpu.unwrap_or(0.0),
            bench_cpu: bench_cpu.unwrap_or(0.0),
            bench_rss,
            free,
        };
        samples.lock().unwrap().push(sample);
        // The status line: what a person watching the run reads.
        let live = &watch.live;
        let logins = live.logins.load(Ordering::Relaxed);
        let down = live.down_bytes.load(Ordering::Relaxed);
        let mut recent: Vec<u32> = std::mem::take(&mut *live.recent_round_trips.lock().unwrap());
        recent.sort_unstable();
        let p99 = (!recent.is_empty()).then(|| {
            recent[((recent.len() as f64 * 0.99).ceil() as usize).clamp(1, recent.len()) - 1]
        });
        let line = format!(
            "[{} t={:>3.0}s] bots {}/{} joined {} logins/s {} | proxy rss {:.0} MiB cpu {:.0}% thr {} | rtt p99 {} | down {:.1} MB/s | free {:.1} GiB",
            watch.label,
            sample.at,
            live.connected.load(Ordering::Relaxed),
            watch.players,
            live.joined.load(Ordering::Relaxed),
            logins.saturating_sub(last.1),
            sample.rss as f64 / MIB,
            sample.cpu,
            sample.threads,
            p99.map_or("-".to_owned(), |us| format!(
                "{:.2} ms",
                f64::from(us) / 1000.0
            )),
            down.saturating_sub(last.2) as f64 / 1e6,
            sample.free as f64 / GIB,
        );
        eprintln!("{line}");
        if let Ok(mut status) = live.status.lock() {
            *status = line;
        }
        last = (0, logins, down);
        // The guards: each one that trips stops the run, and says why.
        let lost = live.dropped.load(Ordering::Relaxed) + live.failed.load(Ordering::Relaxed);
        let reason = if free < min_free {
            Some(format!(
                "free memory is {:.1} GiB, under the floor of {:.1} GiB",
                free as f64 / GIB,
                min_free as f64 / GIB
            ))
        } else if watch.proxy.is_some() && rss == 0 {
            Some("the proxy process is gone".to_owned())
        } else if lost > (watch.players as u64 / 10).max(10) {
            Some(format!(
                "{lost} bots failed to log in or were dropped, over a tenth of the {}",
                watch.players
            ))
        } else if second.elapsed() > Duration::from_secs(4) {
            Some(format!(
                "a reading that should take a second took {:.1} s: the machine or this program is starved",
                second.elapsed().as_secs_f64()
            ))
        } else {
            None
        };
        if let Some(reason) = reason {
            eprintln!("meridian-bench: {reason}: stopping the run");
            tripped.store(true, Ordering::Relaxed);
            run_stop.store(true, Ordering::Relaxed);
        }
    }
}

/// The mean of `f` over the samples.
pub fn mean(samples: &[Sample], f: impl Fn(&Sample) -> f64) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    samples.iter().map(f).sum::<f64>() / samples.len() as f64
}

/// The largest of `f` over the samples.
pub fn peak(samples: &[Sample], f: impl Fn(&Sample) -> f64) -> f64 {
    samples.iter().map(f).fold(0.0, f64::max)
}

/// The nearest-rank percentile `p` of `f` over the samples.
pub fn percentile(samples: &[Sample], p: f64, f: impl Fn(&Sample) -> f64) -> f64 {
    let mut values: Vec<f64> = samples.iter().map(f).collect();
    values.sort_by(|a, b| a.total_cmp(b));
    if values.is_empty() {
        return 0.0;
    }
    values[((p / 100.0 * values.len() as f64).ceil() as usize).clamp(1, values.len()) - 1]
}

/// Bytes as MiB.
pub fn mib(bytes: f64) -> f64 {
    bytes / MIB
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statistics_of_the_samples() {
        let samples: Vec<Sample> = (1..=10)
            .map(|i| Sample {
                cpu: i as f32 * 10.0,
                ..Sample::default()
            })
            .collect();
        assert_eq!(mean(&samples, |s| f64::from(s.cpu)), 55.0);
        assert_eq!(peak(&samples, |s| f64::from(s.cpu)), 100.0);
        assert_eq!(percentile(&samples, 50.0, |s| f64::from(s.cpu)), 50.0);
        assert_eq!(percentile(&samples, 95.0, |s| f64::from(s.cpu)), 100.0);
        assert_eq!(mean(&[], |s| f64::from(s.cpu)), 0.0);
    }
}
