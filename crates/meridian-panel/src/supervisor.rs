//! The processes of the bench: starting and stopping them, their output, their
//! state and what they use.
//!
//! The panel starts only the processes of the bench file, and stops only the ones
//! it started: each is stopped through the handle of the child process it made,
//! never by a process id, so a process that later got the same id is never
//! touched.
use crate::bench::{Bench, Kind, ProcessSpec, eula_accepted};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

/// Lines of output kept per process.
pub const LOG_LINES: usize = 2000;
/// Samples of CPU and memory kept per process: five minutes at one every two seconds.
pub const HISTORY: usize = 150;
/// How often the state, the ports and the measures are looked at.
const TICK: Duration = Duration::from_millis(500);
/// Measures are taken every this many ticks.
const SAMPLE_EVERY: u32 = 4;

/// Where a process is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Stopped,
    /// Started, its port not answering yet.
    Starting,
    /// Its port answers.
    Online,
    /// Asked to stop.
    Stopping,
    /// It ended by itself; its exit code when there is one.
    Exited(Option<i32>),
}

impl State {
    pub fn name(self) -> &'static str {
        match self {
            State::Stopped => "stopped",
            State::Starting => "starting",
            State::Online => "online",
            State::Stopping => "stopping",
            State::Exited(_) => "exited",
        }
    }

    fn running(self) -> bool {
        matches!(self, State::Starting | State::Online | State::Stopping)
    }
}

/// One measure of a process.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    /// Seconds since the panel started.
    pub at: f64,
    /// Percent of one processor core; above 100 when it uses several.
    pub cpu: f32,
    /// Resident memory in megabytes.
    pub memory_mb: f64,
}

/// A line of output, numbered so the page asks only for the new ones.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub seq: u64,
    pub text: String,
}

/// What the page shows of a process.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub name: String,
    pub kind: Kind,
    pub port: u16,
    pub state: State,
    pub pid: Option<u32>,
    pub uptime: Option<Duration>,
    pub history: Vec<Sample>,
}

struct Running {
    child: Child,
    stdin: Option<ChildStdin>,
    started: Instant,
    /// Counts the starts, so a stop meant for one run never ends the next.
    run: u64,
}

struct Inner {
    state: State,
    running: Option<Running>,
    runs: u64,
}

struct Process {
    spec: ProcessSpec,
    inner: Mutex<Inner>,
    log: Mutex<Log>,
    history: Mutex<VecDeque<Sample>>,
}

#[derive(Default)]
struct Log {
    lines: VecDeque<Line>,
    next: u64,
}

impl Log {
    fn push(&mut self, text: String) {
        if self.lines.len() == LOG_LINES {
            self.lines.pop_front();
        }
        self.lines.push_back(Line {
            seq: self.next,
            text,
        });
        self.next += 1;
    }
}

/// The processes of one bench.
pub struct Supervisor {
    processes: Vec<Arc<Process>>,
    born: Instant,
    closing: AtomicBool,
}

impl Supervisor {
    /// The bench's processes, all stopped, and the thread that watches them.
    pub fn new(bench: &Bench) -> Arc<Self> {
        let supervisor = Arc::new(Self {
            processes: bench
                .processes
                .iter()
                .map(|spec| {
                    Arc::new(Process {
                        spec: spec.clone(),
                        inner: Mutex::new(Inner {
                            state: State::Stopped,
                            running: None,
                            runs: 0,
                        }),
                        log: Mutex::new(Log::default()),
                        history: Mutex::new(VecDeque::new()),
                    })
                })
                .collect(),
            born: Instant::now(),
            closing: AtomicBool::new(false),
        });
        let watched = Arc::downgrade(&supervisor);
        let _ = thread::Builder::new()
            .name("panel-watch".to_owned())
            .spawn(move || watch(watched));
        supervisor
    }

    /// The names of the processes, in the order of the bench file.
    pub fn names(&self) -> Vec<String> {
        self.processes.iter().map(|p| p.spec.name.clone()).collect()
    }

    fn process(&self, name: &str) -> Result<&Arc<Process>, String> {
        self.processes
            .iter()
            .find(|p| p.spec.name == name)
            .ok_or_else(|| format!("no process called \"{name}\" in the bench"))
    }

    /// Starts the process `name`. A server whose EULA is not accepted is refused.
    pub fn start(&self, name: &str) -> Result<(), String> {
        let process = self.process(name)?;
        let spec = &process.spec;
        if spec.kind == Kind::Server && !eula_accepted(&spec.dir) {
            let bench_dir = spec.dir.parent().unwrap_or(&spec.dir);
            return Err(format!(
                "{name}: the Minecraft EULA is not accepted in {}. Read it at {}, and if you \
                 agree, accept it yourself with: {}",
                spec.dir.display(),
                crate::init::EULA_URL,
                crate::init::accept_command(bench_dir)
            ));
        }
        let mut inner = process.lock();
        if inner.state.running() {
            return Err(format!("{name} is already {}", inner.state.name()));
        }
        let mut child = Command::new(&spec.program)
            .args(&spec.args)
            .current_dir(&spec.dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("{name}: cannot start {}: {e}", spec.program.display()))?;
        if let Some(out) = child.stdout.take() {
            read_lines(Arc::clone(process), out, "");
        }
        if let Some(err) = child.stderr.take() {
            read_lines(Arc::clone(process), err, "[stderr] ");
        }
        inner.runs += 1;
        let pid = child.id();
        inner.running = Some(Running {
            stdin: child.stdin.take(),
            child,
            started: Instant::now(),
            run: inner.runs,
        });
        inner.state = State::Starting;
        drop(inner);
        process.history().clear();
        process
            .log()
            .push(format!("[panel] started, process id {pid}"));
        Ok(())
    }

    /// Asks the process `name` to stop with its console command, and kills it if it
    /// has not ended after its stop timeout. Returns at once.
    pub fn stop(&self, name: &str) -> Result<(), String> {
        let process = Arc::clone(self.process(name)?);
        let run = {
            let mut inner = process.lock();
            let Some(running) = inner.running.as_mut() else {
                return Err(format!("{name} is not running"));
            };
            let run = running.run;
            let line = format!("{}\n", process.spec.stop_command);
            let asked = running.stdin.as_mut().is_some_and(|stdin| {
                stdin
                    .write_all(line.as_bytes())
                    .and_then(|()| stdin.flush())
                    .is_ok()
            });
            inner.state = State::Stopping;
            if !asked {
                // No console to ask: the process is ended at once.
                kill(&mut inner, run);
            }
            run
        };
        process.log().push(format!(
            "[panel] stopping with \"{}\"",
            process.spec.stop_command
        ));
        let timeout = process.spec.stop_timeout;
        let _ = thread::Builder::new()
            .name(format!("panel-stop-{name}"))
            .spawn(move || {
                let until = Instant::now() + timeout;
                while Instant::now() < until {
                    if process.lock().running.as_ref().map(|r| r.run) != Some(run) {
                        return;
                    }
                    thread::sleep(TICK);
                }
                let mut inner = process.lock();
                if kill(&mut inner, run) {
                    drop(inner);
                    process.log().push(format!(
                        "[panel] still running after {} s: killed",
                        timeout.as_secs()
                    ));
                }
            });
        Ok(())
    }

    /// Stops the process `name`, waits for it to end, and starts it again. Runs in
    /// the background; returns at once.
    pub fn restart(self: &Arc<Self>, name: &str) -> Result<(), String> {
        let process = Arc::clone(self.process(name)?);
        let running = process.lock().state.running();
        if running {
            self.stop(name)?;
        }
        let supervisor = Arc::clone(self);
        let name = name.to_owned();
        let _ = thread::Builder::new()
            .name(format!("panel-restart-{name}"))
            .spawn(move || {
                let until = Instant::now() + process.spec.stop_timeout + Duration::from_secs(5);
                while process.lock().state.running() && Instant::now() < until {
                    thread::sleep(TICK);
                }
                if let Err(e) = supervisor.start(&name) {
                    process.log().push(format!("[panel] {e}"));
                }
            });
        Ok(())
    }

    /// Starts every stopped process: the servers first, then, once they answer or
    /// after a minute, the proxy. Runs in the background; returns at once.
    pub fn start_all(self: &Arc<Self>) {
        let supervisor = Arc::clone(self);
        let _ = thread::Builder::new()
            .name("panel-start-all".to_owned())
            .spawn(move || {
                let servers = supervisor.of_kind(Kind::Server);
                for p in &servers {
                    supervisor.start_quietly(p);
                }
                let until = Instant::now() + Duration::from_secs(60);
                while Instant::now() < until
                    && servers.iter().any(|p| p.lock().state == State::Starting)
                {
                    thread::sleep(TICK);
                }
                for p in supervisor.of_kind(Kind::Proxy) {
                    supervisor.start_quietly(&p);
                }
            });
    }

    /// Stops every running process: the servers first, then the proxy once they
    /// have ended. Runs in the background; returns at once.
    pub fn stop_all(self: &Arc<Self>) {
        let supervisor = Arc::clone(self);
        let _ = thread::Builder::new()
            .name("panel-stop-all".to_owned())
            .spawn(move || {
                let servers = supervisor.of_kind(Kind::Server);
                for p in &servers {
                    let _ = supervisor.stop(&p.spec.name);
                }
                let longest = servers
                    .iter()
                    .map(|p| p.spec.stop_timeout)
                    .max()
                    .unwrap_or_default();
                let until = Instant::now() + longest + Duration::from_secs(5);
                while Instant::now() < until && servers.iter().any(|p| p.lock().state.running()) {
                    thread::sleep(TICK);
                }
                for p in supervisor.of_kind(Kind::Proxy) {
                    let _ = supervisor.stop(&p.spec.name);
                }
            });
    }

    fn start_quietly(&self, process: &Arc<Process>) {
        if process.lock().state.running() {
            return;
        }
        if let Err(e) = self.start(&process.spec.name) {
            process.log().push(format!("[panel] {e}"));
        }
    }

    fn of_kind(&self, kind: Kind) -> Vec<Arc<Process>> {
        self.processes
            .iter()
            .filter(|p| p.spec.kind == kind)
            .cloned()
            .collect()
    }

    /// Writes `line` to the console of `name`.
    pub fn send(&self, name: &str, line: &str) -> Result<(), String> {
        let process = self.process(name)?;
        let line = line.trim_end_matches(['\r', '\n']);
        if line.contains(['\r', '\n']) {
            return Err("one line at a time".to_owned());
        }
        {
            let mut inner = process.lock();
            let stdin = inner
                .running
                .as_mut()
                .and_then(|r| r.stdin.as_mut())
                .ok_or_else(|| format!("{name} is not running"))?;
            stdin
                .write_all(format!("{line}\n").as_bytes())
                .and_then(|()| stdin.flush())
                .map_err(|e| format!("{name}: cannot write to its console: {e}"))?;
        }
        process.log().push(format!("> {line}"));
        Ok(())
    }

    /// Writes `line` to the console of `name` and returns the lines it printed
    /// within `wait`.
    pub fn ask(&self, name: &str, line: &str, wait: Duration) -> Result<Vec<String>, String> {
        let process = self.process(name)?;
        let from = process.log().next;
        self.send(name, line)?;
        thread::sleep(wait);
        Ok(process
            .log()
            .lines
            .iter()
            .filter(|l| l.seq > from)
            .map(|l| l.text.clone())
            .collect())
    }

    /// The lines of `name` numbered `since` or later, and the number of the next.
    pub fn log(&self, name: &str, since: u64) -> Result<(Vec<Line>, u64), String> {
        let process = self.process(name)?;
        let log = process.log();
        Ok((
            log.lines
                .iter()
                .filter(|l| l.seq >= since)
                .cloned()
                .collect(),
            log.next,
        ))
    }

    /// What the page shows of each process.
    pub fn snapshots(&self) -> Vec<Snapshot> {
        self.processes
            .iter()
            .map(|p| {
                let inner = p.lock();
                Snapshot {
                    name: p.spec.name.clone(),
                    kind: p.spec.kind,
                    port: p.spec.port,
                    state: inner.state,
                    pid: inner.running.as_ref().map(|r| r.child.id()),
                    uptime: inner.running.as_ref().map(|r| r.started.elapsed()),
                    history: p.history().iter().copied().collect(),
                }
            })
            .collect()
    }

    /// Kills every process still running, without waiting: for when the panel
    /// ends.
    pub fn kill_all(&self) {
        self.closing.store(true, Ordering::Relaxed);
        for p in &self.processes {
            let mut inner = p.lock();
            if let Some(run) = inner.running.as_ref().map(|r| r.run) {
                kill(&mut inner, run);
            }
        }
    }
}

impl Process {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn log(&self) -> MutexGuard<'_, Log> {
        self.log.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn history(&self) -> MutexGuard<'_, VecDeque<Sample>> {
        self.history.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Kills the child of run `run`, if it is still the one running. Returns whether
/// it did.
fn kill(inner: &mut Inner, run: u64) -> bool {
    match inner.running.as_mut() {
        Some(running) if running.run == run => {
            // Through the child's own handle: never another process.
            let _ = running.child.kill();
            true
        }
        _ => false,
    }
}

/// Copies the lines `from` prints into the process's log until it closes.
fn read_lines(process: Arc<Process>, from: impl Read + Send + 'static, prefix: &'static str) {
    let _ = thread::Builder::new()
        .name(format!("panel-out-{}", process.spec.name))
        .spawn(move || {
            let mut reader = BufReader::new(from);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match reader.read_until(b'\n', &mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(_) => {
                        let text = String::from_utf8_lossy(&buf);
                        let text = text.trim_end_matches(['\r', '\n']);
                        process.log().push(format!("{prefix}{text}"));
                    }
                }
            }
        });
}

/// Watches the processes until the supervisor is dropped: notes the ones that
/// ended, the ones whose port answers, and measures them.
fn watch(supervisor: std::sync::Weak<Supervisor>) {
    let mut system = sysinfo::System::new();
    let mut tick = 0u32;
    loop {
        thread::sleep(TICK);
        let Some(supervisor) = supervisor.upgrade() else {
            return;
        };
        if supervisor.closing.load(Ordering::Relaxed) {
            return;
        }
        for p in &supervisor.processes {
            check(p);
        }
        tick = tick.wrapping_add(1);
        if tick % SAMPLE_EVERY == 0 {
            measure(&supervisor, &mut system);
        }
    }
}

/// Notes whether `process` ended, and whether its port answers.
fn check(process: &Process) {
    let mut inner = process.lock();
    let Some(running) = inner.running.as_mut() else {
        return;
    };
    match running.child.try_wait() {
        Ok(Some(status)) => {
            let code = status.code();
            let asked = inner.state == State::Stopping;
            inner.running = None;
            inner.state = if asked {
                State::Stopped
            } else {
                State::Exited(code)
            };
            drop(inner);
            process.log().push(match code {
                Some(code) => format!("[panel] ended with code {code}"),
                None => "[panel] ended".to_owned(),
            });
        }
        Ok(None) if inner.state == State::Starting => {
            drop(inner);
            if port_answers(process.spec.port) {
                let mut inner = process.lock();
                if inner.state == State::Starting {
                    inner.state = State::Online;
                    drop(inner);
                    process.log().push(format!(
                        "[panel] online: port {} answers",
                        process.spec.port
                    ));
                }
            }
        }
        _ => {}
    }
}

fn port_answers(port: u16) -> bool {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&address, Duration::from_millis(300)).is_ok()
}

/// Takes one sample of each running process.
fn measure(supervisor: &Supervisor, system: &mut sysinfo::System) {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate};
    let pids: Vec<(usize, Pid)> = supervisor
        .processes
        .iter()
        .enumerate()
        .filter_map(|(i, p)| {
            p.lock()
                .running
                .as_ref()
                .map(|r| (i, Pid::from_u32(r.child.id())))
        })
        .collect();
    if pids.is_empty() {
        return;
    }
    let only: Vec<Pid> = pids.iter().map(|(_, pid)| *pid).collect();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&only),
        true,
        ProcessRefreshKind::nothing().with_cpu().with_memory(),
    );
    let at = supervisor.born.elapsed().as_secs_f64();
    for (i, pid) in pids {
        let Some(found) = system.process(pid) else {
            continue;
        };
        let sample = Sample {
            at,
            cpu: found.cpu_usage(),
            memory_mb: found.memory() as f64 / (1024.0 * 1024.0),
        };
        let mut history = supervisor.processes[i].history();
        if history.len() == HISTORY {
            history.pop_front();
        }
        history.push_back(sample);
    }
}
