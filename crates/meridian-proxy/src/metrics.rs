//! The counters of the proxy, and the page that shows them (`[metrics]`, docs/CONFIG.md).
//!
//! When `[metrics] enabled` is on, a small HTTP server answers two paths, on the
//! loopback by default:
//!
//! - `GET /metrics`: the counters and gauges in the Prometheus text format;
//! - `GET /healthz`: `200` while the proxy takes players, `503` once it is stopping.
//!
//! The counters are atomics, bumped where the thing happens: no lock, no allocation and
//! no work on the path of a player's packets. A gauge such as the players on a backend is
//! worked out when the page is asked for, from what the console's `servers` reads. No
//! label ever carries a player's name, address or unique id: the labels are backend names
//! from the configuration and fixed words.
use crate::placing;
use crate::restart::Cause;
use crate::state::Proxy;
use meridian_handoff::{Done, Step};
use std::fmt::Write as _;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

/// The upper bounds, in seconds, of the transfer duration histograms.
const BOUNDS: [f64; 8] = [0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0];

/// A histogram of durations: one counter per bucket, the sum and the count.
#[derive(Default)]
pub struct Histogram {
    /// The observations in each bucket (not cumulative); the last is above every bound.
    buckets: [AtomicU64; BOUNDS.len() + 1],
    sum_micros: AtomicU64,
    count: AtomicU64,
}

impl Histogram {
    pub fn observe(&self, took: Duration) {
        let seconds = took.as_secs_f64();
        let bucket = BOUNDS
            .iter()
            .position(|bound| seconds <= *bound)
            .unwrap_or(BOUNDS.len());
        self.buckets[bucket].fetch_add(1, Ordering::Relaxed);
        self.sum_micros.fetch_add(
            u64::try_from(took.as_micros()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        self.count.fetch_add(1, Ordering::Relaxed);
    }

    fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    /// The lines of the histogram `name`, with `labels` (`phase="prepare"`).
    fn render(&self, out: &mut String, name: &str, labels: &str) {
        let mut cumulative = 0;
        for (i, bound) in BOUNDS.iter().enumerate() {
            cumulative += self.buckets[i].load(Ordering::Relaxed);
            let _ = writeln!(out, "{name}_bucket{{{labels},le=\"{bound}\"}} {cumulative}");
        }
        cumulative += self.buckets[BOUNDS.len()].load(Ordering::Relaxed);
        let _ = writeln!(out, "{name}_bucket{{{labels},le=\"+Inf\"}} {cumulative}");
        let sum = self.sum_micros.load(Ordering::Relaxed) as f64 / 1_000_000.0;
        let _ = writeln!(out, "{name}_sum{{{labels}}} {sum}");
        let _ = writeln!(out, "{name}_count{{{labels}}} {}", self.count());
    }
}

/// Why a login did not end in a player on a backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginFailure {
    /// The handshake or the login broke the protocol, or the connection failed.
    Protocol,
    /// The player was already connected and duplicates are refused.
    Duplicate,
    /// The proxy, or every lobby, was at its limit.
    Full,
    /// The routes left no backend to send the player to.
    NoRoute,
    /// No backend of the list took the player.
    Unreachable,
}

impl LoginFailure {
    const ALL: [LoginFailure; 5] = [
        LoginFailure::Protocol,
        LoginFailure::Duplicate,
        LoginFailure::Full,
        LoginFailure::NoRoute,
        LoginFailure::Unreachable,
    ];

    fn label(self) -> &'static str {
        match self {
            LoginFailure::Protocol => "protocol",
            LoginFailure::Duplicate => "duplicate",
            LoginFailure::Full => "full",
            LoginFailure::NoRoute => "no_route",
            LoginFailure::Unreachable => "unreachable",
        }
    }
}

/// The steps a transfer can fail at (`meridian_handoff::Step`).
const STEPS: [&str; 5] = ["check", "prepare", "freeze", "hand_over", "commit"];

/// The phases a transfer is timed in, and the whole of it.
const PHASES: [&str; 5] = ["prepare", "freeze", "hand_over", "commit", "total"];

/// How a backend came to be thought of as going down (`restart::Cause`).
const CAUSES: [&str; 3] = ["announced", "panel", "unreachable"];

/// What the proxy counts. Everything is an atomic counter or a gauge set by one store.
#[derive(Default)]
pub struct Metrics {
    connections: AtomicU64,
    throttled: AtomicU64,
    logins: AtomicU64,
    login_failures: [AtomicU64; LoginFailure::ALL.len()],
    transfers: AtomicU64,
    transfer_failures: [AtomicU64; STEPS.len()],
    transfer_alerts: AtomicU64,
    phases: [Histogram; PHASES.len()],
    evacuations: AtomicU64,
    evacuated: AtomicU64,
    stayed: AtomicU64,
    restarts: [AtomicU64; CAUSES.len()],
    status_rounds: AtomicU64,
    last_status_round_micros: AtomicU64,
}

impl Metrics {
    /// A connection was accepted.
    pub fn connection(&self) {
        self.connections.fetch_add(1, Ordering::Relaxed);
    }

    /// A connection was closed unread: its address opened too many this minute.
    pub fn throttled(&self) {
        self.throttled.fetch_add(1, Ordering::Relaxed);
    }

    /// A player is on a backend.
    pub fn login(&self) {
        self.logins.fetch_add(1, Ordering::Relaxed);
    }

    pub fn login_failed(&self, why: LoginFailure) {
        let i = LoginFailure::ALL
            .iter()
            .position(|f| *f == why)
            .unwrap_or_default();
        self.login_failures[i].fetch_add(1, Ordering::Relaxed);
    }

    /// A transfer ended well, and took `took` in all.
    pub fn transfer_done(&self, done: &Done, took: Duration) {
        self.transfers.fetch_add(1, Ordering::Relaxed);
        for (phase, time) in self.phases.iter().zip(done.times) {
            phase.observe(time);
        }
        self.phases[PHASES.len() - 1].observe(took);
    }

    /// A transfer failed at `step` (one of [`STEPS`]; anything else counts as `check`).
    pub fn transfer_failed(&self, step: &str) {
        let i = STEPS.iter().position(|s| *s == step).unwrap_or_default();
        self.transfer_failures[i].fetch_add(1, Ordering::Relaxed);
    }

    /// A source refused to let a player go after the target had placed them: the
    /// `ALERT` of the log.
    pub fn alert(&self) {
        self.transfer_alerts.fetch_add(1, Ordering::Relaxed);
    }

    /// The players of a backend were sent elsewhere: how many moved, how many stayed.
    pub fn evacuation(&self, moved: usize, stayed: usize) {
        self.evacuations.fetch_add(1, Ordering::Relaxed);
        self.evacuated.fetch_add(moved as u64, Ordering::Relaxed);
        self.stayed.fetch_add(stayed as u64, Ordering::Relaxed);
    }

    /// A backend was found to be going down.
    pub fn restart_detected(&self, cause: Cause) {
        let i = match cause {
            Cause::Announced => 0,
            Cause::Panel => 1,
            Cause::Unreachable => 2,
        };
        self.restarts[i].fetch_add(1, Ordering::Relaxed);
    }

    /// A round of questions to the sidecars took `took`.
    pub fn status_round(&self, took: Duration) {
        self.status_rounds.fetch_add(1, Ordering::Relaxed);
        self.last_status_round_micros.store(
            u64::try_from(took.as_micros()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }
}

#[cfg(test)]
impl Metrics {
    /// How many logins failed for `why`.
    pub fn failures_of(&self, why: LoginFailure) -> u64 {
        let i = LoginFailure::ALL.iter().position(|f| *f == why).unwrap();
        self.login_failures[i].load(Ordering::Relaxed)
    }
}

/// The label of a transfer step.
pub fn step_label(step: Step) -> &'static str {
    match step {
        Step::Check => STEPS[0],
        Step::Prepare => STEPS[1],
        Step::Freeze => STEPS[2],
        Step::HandOver => STEPS[3],
        Step::Commit => STEPS[4],
    }
}

/// A label value: backslash, quote and line feed escaped, as the format asks.
fn escaped(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

fn family(out: &mut String, name: &str, kind: &str, help: &str) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} {kind}");
}

/// The page of `/metrics`.
pub fn render(proxy: &Proxy) -> String {
    let config = proxy.config();
    let sessions = &proxy.sessions;
    let m = sessions.metrics();
    let shared = sessions.shared();
    let counts = shared.counts();
    let mut out = String::new();
    let get = |counter: &AtomicU64| counter.load(Ordering::Relaxed);

    family(
        &mut out,
        "meridian_info",
        "gauge",
        "The version of the proxy.",
    );
    let _ = writeln!(
        out,
        "meridian_info{{version=\"{}\"}} 1",
        escaped(env!("CARGO_PKG_VERSION"))
    );

    family(
        &mut out,
        "meridian_players",
        "gauge",
        "Players connected to the proxy.",
    );
    let _ = writeln!(out, "meridian_players {}", sessions.players().len());

    family(
        &mut out,
        "meridian_backend_players",
        "gauge",
        "Players on each backend.",
    );
    for b in &config.backends {
        let _ = writeln!(
            out,
            "meridian_backend_players{{backend=\"{}\"}} {}",
            escaped(&b.name),
            counts.get(&b.name).copied().unwrap_or(0)
        );
    }

    // What the balancer makes of the backends that have a role.
    let standings: Vec<(&str, placing::Standing)> = config
        .backends
        .iter()
        .filter_map(|b| {
            let on = counts.get(&b.name).copied().unwrap_or(0);
            let restarting = proxy.restarts.describe(&b.name);
            placing::standing(&config, &shared, &b.name, on, restarting.as_deref())
                .map(|s| (b.name.as_str(), s))
        })
        .collect();
    family(
        &mut out,
        "meridian_backend_soft_cap",
        "gauge",
        "The soft cap of each backend that has a role.",
    );
    for (name, s) in &standings {
        let _ = writeln!(
            out,
            "meridian_backend_soft_cap{{backend=\"{}\"}} {}",
            escaped(name),
            s.soft_cap
        );
    }
    family(
        &mut out,
        "meridian_backend_hard_cap",
        "gauge",
        "The hard cap of each backend that has a role.",
    );
    for (name, s) in &standings {
        let _ = writeln!(
            out,
            "meridian_backend_hard_cap{{backend=\"{}\"}} {}",
            escaped(name),
            s.hard_cap
        );
    }
    family(
        &mut out,
        "meridian_backend_state",
        "gauge",
        "1 for the state the balancer sees a backend in: open, closed, draining or not_offered.",
    );
    for (name, s) in &standings {
        for state in ["open", "closed", "draining", "not_offered"] {
            let _ = writeln!(
                out,
                "meridian_backend_state{{backend=\"{}\",state=\"{state}\"}} {}",
                escaped(name),
                u8::from(s.offer.state() == state)
            );
        }
    }

    family(
        &mut out,
        "meridian_connections_total",
        "counter",
        "Connections accepted, whatever they came to.",
    );
    let _ = writeln!(out, "meridian_connections_total {}", get(&m.connections));
    family(
        &mut out,
        "meridian_connections_throttled_total",
        "counter",
        "Connections closed unread because their address opened too many this minute.",
    );
    let _ = writeln!(
        out,
        "meridian_connections_throttled_total {}",
        get(&m.throttled)
    );
    family(
        &mut out,
        "meridian_logins_total",
        "counter",
        "Players who logged in and were sent to a backend.",
    );
    let _ = writeln!(out, "meridian_logins_total {}", get(&m.logins));
    family(
        &mut out,
        "meridian_login_failures_total",
        "counter",
        "Logins that did not end on a backend, by reason.",
    );
    for (why, counter) in LoginFailure::ALL.iter().zip(&m.login_failures) {
        let _ = writeln!(
            out,
            "meridian_login_failures_total{{reason=\"{}\"}} {}",
            why.label(),
            get(counter)
        );
    }

    family(
        &mut out,
        "meridian_transfers_total",
        "counter",
        "Transfers of a player to another backend that succeeded.",
    );
    let _ = writeln!(out, "meridian_transfers_total {}", get(&m.transfers));
    family(
        &mut out,
        "meridian_transfer_failures_total",
        "counter",
        "Transfers that failed, by the step they failed at.",
    );
    for (step, counter) in STEPS.iter().zip(&m.transfer_failures) {
        let _ = writeln!(
            out,
            "meridian_transfer_failures_total{{step=\"{step}\"}} {}",
            get(counter)
        );
    }
    family(
        &mut out,
        "meridian_transfer_alerts_total",
        "counter",
        "Transfers where the source refused to let the player go after the target placed them (ALERT in the log).",
    );
    let _ = writeln!(
        out,
        "meridian_transfer_alerts_total {}",
        get(&m.transfer_alerts)
    );
    family(
        &mut out,
        "meridian_transfer_duration_seconds",
        "histogram",
        "How long the phases of a successful transfer took.",
    );
    for (phase, histogram) in PHASES.iter().zip(&m.phases) {
        histogram.render(
            &mut out,
            "meridian_transfer_duration_seconds",
            &format!("phase=\"{phase}\""),
        );
    }

    family(
        &mut out,
        "meridian_evacuations_total",
        "counter",
        "Evacuations of a backend that had players on it.",
    );
    let _ = writeln!(out, "meridian_evacuations_total {}", get(&m.evacuations));
    family(
        &mut out,
        "meridian_evacuated_players_total",
        "counter",
        "Players an evacuation moved, or left where they were.",
    );
    let _ = writeln!(
        out,
        "meridian_evacuated_players_total{{result=\"moved\"}} {}",
        get(&m.evacuated)
    );
    let _ = writeln!(
        out,
        "meridian_evacuated_players_total{{result=\"stayed\"}} {}",
        get(&m.stayed)
    );
    family(
        &mut out,
        "meridian_restarts_detected_total",
        "counter",
        "Backends found to be going down, by how it was found.",
    );
    for (cause, counter) in CAUSES.iter().zip(&m.restarts) {
        let _ = writeln!(
            out,
            "meridian_restarts_detected_total{{cause=\"{cause}\"}} {}",
            get(counter)
        );
    }

    family(
        &mut out,
        "meridian_status_rounds_total",
        "counter",
        "Rounds of questions to the sidecars.",
    );
    let _ = writeln!(
        out,
        "meridian_status_rounds_total {}",
        get(&m.status_rounds)
    );
    family(
        &mut out,
        "meridian_status_round_duration_seconds",
        "gauge",
        "How long the last round of questions to the sidecars took.",
    );
    let _ = writeln!(
        out,
        "meridian_status_round_duration_seconds {}",
        get(&m.last_status_round_micros) as f64 / 1_000_000.0
    );
    out
}

/// What the page answers to a request: the status line, the type and the body.
#[derive(Debug, PartialEq, Eq)]
pub struct Answer {
    pub status: u16,
    pub content_type: &'static str,
    pub body: String,
}

const TEXT: &str = "text/plain; charset=utf-8";
const PROMETHEUS: &str = "text/plain; version=0.0.4; charset=utf-8";

/// The answer to `method` `path`.
pub fn respond(proxy: &Proxy, method: &str, path: &str) -> Answer {
    let path = path.split(['?', '#']).next().unwrap_or("");
    let plain = |status, body: &str| Answer {
        status,
        content_type: TEXT,
        body: format!("{body}\n"),
    };
    match path {
        "/metrics" | "/healthz" if method != "GET" && method != "HEAD" => {
            plain(405, "method not allowed")
        }
        "/metrics" => Answer {
            status: 200,
            content_type: PROMETHEUS,
            body: render(proxy),
        },
        "/healthz" if proxy.stopping() => plain(503, "stopping"),
        "/healthz" => plain(200, "ok"),
        _ => plain(404, "not found"),
    }
}

/// How many connections the page serves at once; the others are closed unanswered.
const MAX_CONNECTIONS: usize = 8;
/// How long a client has to send its request and take the answer.
const IO_TIMEOUT: Duration = Duration::from_secs(2);
/// The most of a request that is read: the line and the headers.
const MAX_REQUEST: usize = 8 * 1024;
/// How often the stop flag is looked at while no one connects.
const POLL: Duration = Duration::from_millis(100);

/// Listens on the `[metrics] bind` address while it is on and serves the page until the
/// proxy stops. `None` when the setting is off.
pub fn spawn(proxy: &Arc<Proxy>) -> Option<io::Result<std::thread::JoinHandle<()>>> {
    let config = proxy.config();
    if !config.metrics.enabled {
        return None;
    }
    Some(TcpListener::bind(config.metrics.bind).and_then(|listener| {
        crate::log::info!(
            "metrics and health check on http://{} (/metrics, /healthz)",
            config.metrics.bind
        );
        if !config.metrics.bind.ip().is_loopback() {
            crate::log::warning!(
                "the metrics page on {} can be reached from other machines, and has no \
                 password: bind it to 127.0.0.1, or keep the port closed to everyone but \
                 the monitor",
                config.metrics.bind
            );
        }
        serve_in_thread(listener, Arc::clone(proxy))
    }))
}

/// Serves `listener` on a thread of its own until the proxy stops.
pub fn serve_in_thread(
    listener: TcpListener,
    proxy: Arc<Proxy>,
) -> io::Result<std::thread::JoinHandle<()>> {
    listener.set_nonblocking(true)?;
    std::thread::Builder::new()
        .name("metrics".into())
        .spawn(move || serve(&listener, &proxy))
}

fn serve(listener: &TcpListener, proxy: &Arc<Proxy>) {
    let active = Arc::new(AtomicUsize::new(0));
    while !proxy.stopping() {
        match listener.accept() {
            Ok((stream, _)) => {
                if active.fetch_add(1, Ordering::Relaxed) >= MAX_CONNECTIONS {
                    active.fetch_sub(1, Ordering::Relaxed);
                    continue;
                }
                let (proxy, done) = (Arc::clone(proxy), Arc::clone(&active));
                let spawned = std::thread::Builder::new()
                    .name("metrics client".into())
                    .spawn(move || {
                        let _ = answer(stream, &proxy);
                        done.fetch_sub(1, Ordering::Relaxed);
                    });
                if spawned.is_err() {
                    active.fetch_sub(1, Ordering::Relaxed);
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => std::thread::sleep(POLL),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => {
                crate::log::warning!("metrics: cannot accept a connection: {e}");
                std::thread::sleep(POLL);
            }
        }
    }
}

/// Reads one request from `stream` and answers it; the connection is closed after.
fn answer(mut stream: TcpStream, proxy: &Proxy) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let mut request = Vec::new();
    let mut chunk = [0u8; 1024];
    while !request.windows(4).any(|w| w == b"\r\n\r\n") && request.len() < MAX_REQUEST {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        request.extend_from_slice(&chunk[..n]);
    }
    let text = String::from_utf8_lossy(&request);
    let mut words = text.lines().next().unwrap_or("").split_whitespace();
    let (method, path) = (words.next().unwrap_or(""), words.next().unwrap_or(""));
    let answer = if method.is_empty() || path.is_empty() {
        Answer {
            status: 400,
            content_type: TEXT,
            body: "bad request\n".to_owned(),
        }
    } else {
        respond(proxy, method, path)
    };
    let reason = match answer.status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        503 => "Service Unavailable",
        _ => "Error",
    };
    let allow = if answer.status == 405 {
        "Allow: GET, HEAD\r\n"
    } else {
        ""
    };
    write!(
        stream,
        "HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\nContent-Length: {}\r\n{allow}Cache-Control: no-store\r\nConnection: close\r\n\r\n",
        answer.status,
        answer.content_type,
        answer.body.len()
    )?;
    if method != "HEAD" {
        stream.write_all(answer.body.as_bytes())?;
    }
    stream.flush()
}

#[cfg(test)]
mod tests;
