//! The processor time of the processes a run watches: this program and the proxy.
//!
//! On Windows, the time the system gives for a process is counted at each clock
//! tick, about every 15.6 ms, by charging the tick to the thread running at that
//! moment. The work of a run comes in bursts far shorter than a tick, woken by the
//! network, and is mostly missed: a proxy relaying 2,000 packets a second read as
//! 0 %. Windows also counts, exactly, the processor cycles each thread ran. This
//! module reads those and turns them into time with the cycles a second of one busy
//! core, measured once by a busy loop: an estimate, not an exact processor time.
//!
//! Elsewhere the system's own figure is used, through sysinfo; Linux counts it
//! exactly.
//!
//! This is the one module of this crate with unsafe code, for the Windows calls.
use std::time::{Duration, Instant};
use sysinfo::Pid;

pub use imp::method;

/// Reads the processor use of some processes over a stretch of time.
pub struct Meter {
    began: Instant,
    inner: imp::Meter,
}

impl Meter {
    /// Starts measuring `pids`.
    pub fn start(pids: &[Pid]) -> Meter {
        Meter {
            inner: imp::Meter::start(pids),
            began: Instant::now(),
        }
    }

    /// The processor use of each process since [`Meter::start`], in percent of one
    /// core, in the order given; `None` for one that could not be read.
    pub fn read(mut self) -> Vec<Option<f32>> {
        let elapsed = self.began.elapsed().max(Duration::from_millis(1));
        self.inner.read(elapsed)
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod imp {
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};
    use sysinfo::Pid;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::Threading::{
        GetCurrentThread, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows_sys::Win32::System::WindowsProgramming::{
        QueryProcessCycleTime, QueryThreadCycleTime,
    };

    /// An open handle to a process, closed when dropped, whatever happened.
    struct Process(HANDLE);

    impl Process {
        fn open(pid: Pid) -> Option<Process> {
            // SAFETY: OpenProcess takes plain values and returns a new handle, or
            // null when the process cannot be opened; no memory is shared.
            let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid.as_u32()) };
            (!handle.is_null()).then_some(Process(handle))
        }

        fn cycles(&self) -> Option<u64> {
            let mut cycles = 0u64;
            // SAFETY: the handle is open, with the access QueryProcessCycleTime
            // needs, until this value is dropped; `cycles` is a u64 it may write.
            let ok = unsafe { QueryProcessCycleTime(self.0, &mut cycles) };
            (ok != 0).then_some(cycles)
        }
    }

    impl Drop for Process {
        fn drop(&mut self) {
            // SAFETY: the handle came from OpenProcess and is closed here only, once.
            unsafe { CloseHandle(self.0) };
        }
    }

    /// The cycles the calling thread has run.
    fn thread_cycles() -> Option<u64> {
        let mut cycles = 0u64;
        // SAFETY: GetCurrentThread returns a pseudo handle for this thread, valid
        // while it runs and never closed; `cycles` is a u64 the call may write.
        let ok = unsafe { QueryThreadCycleTime(GetCurrentThread(), &mut cycles) };
        (ok != 0).then_some(cycles)
    }

    /// The cycles a second of one busy core: the highest of three busy loops of
    /// 100 ms, since the thread may lose its core during one. Measured once, at
    /// the first call.
    fn cycles_per_second() -> Option<f64> {
        static RATE: OnceLock<Option<f64>> = OnceLock::new();
        *RATE.get_or_init(|| {
            (0..3)
                .filter_map(|_| {
                    let began = Instant::now();
                    let first = thread_cycles()?;
                    while began.elapsed() < Duration::from_millis(100) {
                        std::hint::spin_loop();
                    }
                    let last = thread_cycles()?;
                    Some(last.saturating_sub(first) as f64 / began.elapsed().as_secs_f64())
                })
                .reduce(f64::max)
                .filter(|rate| *rate > 0.0)
        })
    }

    /// How processor time is read, for the report. The first call measures the
    /// cycles a second, which takes a third of a second of one core.
    pub fn method() -> String {
        match cycles_per_second() {
            Some(rate) => format!(
                "On Windows it is each process's processor cycles (QueryProcessCycleTime), \
                 turned into time with the cycles a second of one busy core, measured by a \
                 busy loop at the start of the run ({:.2} GHz): an estimate, not an exact \
                 processor time. The time Windows itself gives is counted by clock ticks of \
                 about 15.6 ms and misses most of the short bursts of work of a run.",
                rate / 1e9
            ),
            None => "On this Windows machine the processor cycles could not be read, \
                     so no processor time is shown."
                .to_owned(),
        }
    }

    pub struct Meter {
        /// Each process with its cycles at the start, or `None`.
        processes: Vec<Option<(Process, u64)>>,
    }

    impl Meter {
        pub fn start(pids: &[Pid]) -> Meter {
            // Measured now, so a run's first window does not include the loop.
            let _ = cycles_per_second();
            let processes = pids
                .iter()
                .map(|&pid| {
                    let process = Process::open(pid)?;
                    let cycles = process.cycles()?;
                    Some((process, cycles))
                })
                .collect();
            Meter { processes }
        }

        pub fn read(&mut self, elapsed: Duration) -> Vec<Option<f32>> {
            let rate = cycles_per_second();
            self.processes
                .iter()
                .map(|entry| {
                    let (process, first) = entry.as_ref()?;
                    let used = process.cycles()?.saturating_sub(*first) as f64 / rate?;
                    Some((used / elapsed.as_secs_f64() * 100.0) as f32)
                })
                .collect()
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use std::time::Duration;
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

    /// How processor time is read, for the report.
    pub fn method() -> String {
        "It is each process's processor time as the system counts it.".to_owned()
    }

    pub struct Meter {
        system: System,
        pids: Vec<Pid>,
    }

    impl Meter {
        pub fn start(pids: &[Pid]) -> Meter {
            let mut meter = Meter {
                system: System::new(),
                pids: pids.to_vec(),
            };
            meter.refresh();
            meter
        }

        fn refresh(&mut self) {
            self.system.refresh_cpu_usage();
            self.system.refresh_processes_specifics(
                ProcessesToUpdate::Some(&self.pids),
                true,
                ProcessRefreshKind::nothing().with_cpu(),
            );
        }

        pub fn read(&mut self, _elapsed: Duration) -> Vec<Option<f32>> {
            self.refresh();
            self.pids
                .iter()
                .map(|&pid| self.system.process(pid).map(|p| p.cpu_usage()))
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two seconds of a busy loop in this process read as at least 80 % of one
    /// core. Other tests running at the same time only add to it. On a busy
    /// machine the loop itself may lose its core for a while, which the meter
    /// rightly shows, so the loop gets three tries; a meter that misses work, as
    /// the time Windows counts by tick does, fails all three.
    #[test]
    fn a_busy_loop_reads_as_a_busy_core() {
        let Ok(me) = sysinfo::get_current_pid() else {
            eprintln!("skipped: this system does not give the test's process id");
            return;
        };
        let mut readings = Vec::new();
        for _ in 0..3 {
            let meter = Meter::start(&[me]);
            let began = Instant::now();
            while began.elapsed() < Duration::from_secs(2) {
                std::hint::spin_loop();
            }
            let Some(used) = meter.read()[0] else {
                eprintln!("skipped: this system gives no processor time for the test's process");
                return;
            };
            if used >= 80.0 {
                return;
            }
            readings.push(used);
        }
        panic!("three busy loops of 2 s read as {readings:.1?} % of a core");
    }
}
