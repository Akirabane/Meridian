//! The notifications of the backend's management protocol, which tell the proxy that the
//! server is about to stop (docs/RESTART.md).
//!
//! A Horizon server serves vanilla's management protocol, JSON-RPC over a WebSocket, as
//! `management-server-*` in its `server.properties` set it up: a host and a port, a
//! secret that is the bearer token, and TLS, on unless `management-server-tls-enabled` is
//! false. It pushes `minecraft:notification/server/stopping` to every connected client
//! when the shutdown begins and `.../server/started` when the game is up again. The
//! supervisor serves it, so it answers while the game restarts.
//!
//! This module is a client of that protocol that only listens ([`watch`]). It keeps the
//! two notifications that matter in a short queue ([`Notices`]), which the proxy asks for
//! with the `notices` request. It is off unless the sidecar is given the backend's
//! `server.properties` (`--management-properties`).
//!
//! - The server is on this machine: an address that is not a loopback one is refused,
//!   since the certificate of a server that made its own is not checked (`tls.rs`).
//! - The settings are read again at every connection, since a server that restarts may
//!   have other ones.
//! - The secret is in no message, no error and no `Debug` output of this module.
//! - After a failure the sidecar waits, longer each time up to [`LONGEST_WAIT`], and
//!   never retries in a tight loop.
use crate::lists::{self, Lists, Reply};
use crate::tls;
use crate::websocket::{self, Next, Socket};
use rustls::{ClientConnection, StreamOwned};
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// The notices kept for the proxy: the oldest go first. A server tells two kinds, once
/// per run, so this is a lot.
pub const QUEUE_LEN: usize = 32;

/// The longest the proxy may be made to wait for a notice in one request.
pub const MAX_WAIT: Duration = Duration::from_secs(30);

/// The wait after the first failure to connect, and the longest it grows to.
const FIRST_WAIT: Duration = Duration::from_secs(1);
pub const LONGEST_WAIT: Duration = Duration::from_secs(30);

/// How often a connection that hears nothing looks at the stop flag.
const LOOK: Duration = Duration::from_millis(500);

/// How long the connection and the upgrade may take.
const CONNECT: Duration = Duration::from_secs(10);

/// What the server told, kept for the proxy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    /// Its number, from 1 up, in the order they came.
    pub seq: u64,
    /// The JSON-RPC message, as the server sent it.
    pub line: String,
    pub age: Duration,
}

/// What the proxy hears when it asks.
#[derive(Debug, PartialEq, Eq)]
pub struct Heard {
    /// The number of the last notice there has been, 0 if none.
    pub last: u64,
    /// Whether the sidecar is connected to the management protocol now.
    pub watching: bool,
    pub notices: Vec<Notice>,
}

struct Queue {
    last: u64,
    watching: bool,
    kept: VecDeque<(u64, String, Instant)>,
}

/// The notices of the management protocol, and whether the connection that hears them
/// is up. Shared between the connection's thread and the proxy's connections.
pub struct Notices {
    queue: Mutex<Queue>,
    changed: Condvar,
    /// The lists of the server (docs/OPERATING.md, section 5.10): what it announces of
    /// them, and the calls the proxy makes on them over the same connection.
    pub lists: Lists,
}

impl Default for Notices {
    fn default() -> Self {
        Self {
            queue: Mutex::new(Queue {
                last: 0,
                watching: false,
                kept: VecDeque::new(),
            }),
            changed: Condvar::new(),
            lists: Lists::default(),
        }
    }
}

impl fmt::Debug for Notices {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let queue = self.lock();
        f.debug_struct("Notices")
            .field("last", &queue.last)
            .field("watching", &queue.watching)
            .finish_non_exhaustive()
    }
}

impl Notices {
    /// Keeps `line` if it is a notice of the server's life, and says whether it was.
    pub fn offer(&self, line: &str) -> bool {
        if !is_notice(line) {
            return false;
        }
        let mut queue = self.lock();
        queue.last += 1;
        let seq = queue.last;
        queue.kept.push_back((seq, line.to_owned(), Instant::now()));
        while queue.kept.len() > QUEUE_LEN {
            queue.kept.pop_front();
        }
        drop(queue);
        self.changed.notify_all();
        true
    }

    /// The connection to the server is up or down.
    pub fn set_watching(&self, watching: bool) {
        self.lock().watching = watching;
    }

    /// The notices after number `after`, waiting up to `wait` for one when there are
    /// none. Without `after`, none are given: the caller only learns where the count
    /// stands, to ask for what comes after. A caller that is behind by more than the
    /// queue holds gets the ones that are left.
    pub fn heard(&self, after: Option<u64>, wait: Duration) -> Heard {
        let wait = wait.min(MAX_WAIT);
        let until = Instant::now() + wait;
        let mut queue = self.lock();
        if let Some(after) = after {
            while queue.last <= after {
                let now = Instant::now();
                if now >= until {
                    break;
                }
                queue = self
                    .changed
                    .wait_timeout(queue, until - now)
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
        }
        let now = Instant::now();
        Heard {
            last: queue.last,
            watching: queue.watching,
            notices: after
                .map(|after| {
                    queue
                        .kept
                        .iter()
                        .filter(|(seq, ..)| *seq > after)
                        .map(|(seq, line, at)| Notice {
                            seq: *seq,
                            line: line.clone(),
                            age: now.saturating_duration_since(*at),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Whether `line` is a JSON-RPC message that tells the server is stopping or started.
/// The method is `minecraft:notification/server/stopping` there; the prefix is optional.
pub fn is_notice(line: &str) -> bool {
    let Ok(message) = serde_json::from_str::<serde_json::Value>(line) else {
        return false;
    };
    let Some(method) = message.get("method").and_then(|m| m.as_str()) else {
        return false;
    };
    matches!(
        method.strip_prefix("minecraft:").unwrap_or(method),
        "notification/server/stopping" | "notification/server/started"
    )
}

/// A secret that never shows: it is in no `Debug` output or message.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(text: impl Into<String>) -> Self {
        Secret(text.into())
    }

    fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<hidden>)")
    }
}

/// How the management protocol of the server is set up, from its `server.properties`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub host: String,
    pub port: u16,
    pub secret: Secret,
    pub tls: bool,
}

impl Settings {
    /// Reads the `management-server-*` keys of a `server.properties`. The error says
    /// what is not set up, never a value of the file.
    pub fn from_properties(text: &str) -> Result<Settings, String> {
        let get = |key: &str| {
            text.lines()
                .map(str::trim)
                .filter(|line| !line.starts_with('#') && !line.starts_with('!'))
                .filter_map(|line| line.split_once('='))
                .filter(|(name, _)| name.trim() == key)
                .map(|(_, value)| value.trim())
                .next_back()
        };
        if get("management-server-enabled") != Some("true") {
            return Err(
                "the server's management protocol is off (management-server-enabled is not true)"
                    .into(),
            );
        }
        let port: u16 = get("management-server-port")
            .and_then(|p| p.parse().ok())
            .filter(|p| *p != 0)
            .ok_or("management-server-port is not a port above 0")?;
        let secret = get("management-server-secret").unwrap_or_default();
        if secret.is_empty() {
            return Err("management-server-secret is empty".into());
        }
        if secret.contains(['\r', '\n']) || secret.contains(char::is_whitespace) {
            return Err("management-server-secret is not a token".into());
        }
        Ok(Settings {
            host: get("management-server-host")
                .filter(|h| !h.is_empty())
                .unwrap_or("localhost")
                .to_owned(),
            port,
            secret: Secret::new(secret),
            // As the server reads it: on unless it says false.
            tls: get("management-server-tls-enabled") != Some("false"),
        })
    }

    /// The loopback addresses of the host, in the order they resolve. An address that
    /// is not on this machine is refused: the certificate of a server that made its own
    /// is not checked, and the sidecar runs where the server does.
    pub fn addresses(&self) -> Result<Vec<std::net::SocketAddr>, String> {
        let resolved: Vec<_> = (self.host.as_str(), self.port)
            .to_socket_addrs()
            .map_err(|e| format!("cannot resolve management-server-host {}: {e}", self.host))?
            .collect();
        let local: Vec<_> = resolved
            .iter()
            .copied()
            .filter(|a| a.ip().is_loopback())
            .collect();
        if local.is_empty() || local.len() != resolved.len() {
            return Err(format!(
                "management-server-host {} is not an address of this machine: the sidecar \
                 reads the management protocol of its own server only, on 127.0.0.1 or ::1",
                self.host
            ));
        }
        Ok(local)
    }
}

/// The stream of a connection: in clear, or over TLS.
enum Link {
    Plain(TcpStream),
    Tls(Box<StreamOwned<ClientConnection, TcpStream>>),
}

impl Read for Link {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        match self {
            Link::Plain(s) => s.read(out),
            Link::Tls(s) => s.read(out),
        }
    }
}

impl Write for Link {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        match self {
            Link::Plain(s) => s.write(data),
            Link::Tls(s) => s.write(data),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Link::Plain(s) => s.flush(),
            Link::Tls(s) => s.flush(),
        }
    }
}

/// Connects to the first address that answers, and upgrades to a WebSocket.
fn connect(settings: &Settings) -> Result<Socket<Link>, String> {
    let mut last = String::from("no address");
    let mut stream = None;
    for address in settings.addresses()? {
        match TcpStream::connect_timeout(&address, CONNECT) {
            Ok(s) => {
                stream = Some(s);
                break;
            }
            Err(e) => last = format!("cannot connect to {address}: {e}"),
        }
    }
    let stream = stream.ok_or(last)?;
    let _ = stream.set_nodelay(true);
    stream
        .set_read_timeout(Some(CONNECT))
        .and_then(|()| stream.set_write_timeout(Some(CONNECT)))
        .map_err(|e| e.to_string())?;
    let link = if settings.tls {
        let name = rustls::pki_types::ServerName::try_from("localhost")
            .map_err(|e| e.to_string())?
            .to_owned();
        let connection = ClientConnection::new(tls::config()?, name).map_err(|e| e.to_string())?;
        Link::Tls(Box::new(StreamOwned::new(connection, stream)))
    } else {
        Link::Plain(stream)
    };
    let key = websocket::new_key().map_err(|e| e.to_string())?;
    let request = websocket::upgrade_request(
        &settings.host,
        settings.port,
        settings.secret.expose(),
        &key,
    );
    // The request holds the secret: an error of the upgrade says what the server did,
    // never what was sent.
    Socket::open(link, &request)
}

/// One connection, from the upgrade to its end: the notices it hears go to `notices`.
/// `connected` is set once the upgrade was accepted.
fn session(
    settings: &Settings,
    notices: &Notices,
    stop: &AtomicBool,
    connected: &mut bool,
) -> Result<(), String> {
    let mut socket = connect(settings)?;
    *connected = true;
    // From here a quiet connection is normal: the server speaks when it stops.
    match socket.stream_mut() {
        Link::Plain(s) => s.set_read_timeout(Some(LOOK)),
        Link::Tls(s) => s.sock.set_read_timeout(Some(LOOK)),
    }
    .map_err(|e| e.to_string())?;
    eprintln!(
        "meridian-sidecar: management protocol: connected to {}:{}",
        settings.host, settings.port
    );
    notices.lists.begin_connection();
    notices.set_watching(true);
    let result = listen(&mut socket, notices, stop);
    notices.lists.end_connection();
    result
}

/// What a connection hears and says until it ends: the notices go to their queues, the
/// calls the proxy made are written between two reads, and the server's answers go back
/// to whoever made them.
fn listen(socket: &mut Socket<Link>, notices: &Notices, stop: &AtomicBool) -> Result<(), String> {
    let mut waiting: HashMap<u64, Reply> = HashMap::new();
    while !stop.load(Ordering::Relaxed) {
        for call in notices.lists.take_calls() {
            // A call that cannot be written is dropped with the others: whoever made it
            // is told the connection ended.
            socket
                .send_text(call.text())
                .map_err(|e| format!("the connection broke: {e}"))?;
            waiting.insert(call.id(), call.into_reply());
        }
        match socket.read_message() {
            Ok(Next::Text(line)) => {
                if !lists::answer(&line, &mut waiting) && !notices.offer(&line) {
                    notices.lists.offer(&line);
                }
            }
            Ok(Next::Idle) => {}
            Ok(Next::Closed) => return Ok(()),
            Err(e) => return Err(format!("the connection broke: {e}")),
        }
    }
    Ok(())
}

/// Listens to the management protocol described by the `server.properties` at
/// `properties` until `stop` is set: it connects, hears, and connects again when the
/// connection ends, waiting longer after each failure. What it does is said on stderr,
/// once for each change.
pub fn watch(
    properties: PathBuf,
    notices: Arc<Notices>,
    stop: Arc<AtomicBool>,
) -> io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("management".into())
        .spawn(move || run(&properties, &notices, &stop, &mut |_| {}))
}

/// The loop of [`watch`]. `waited` is told each wait before it is taken (for tests).
fn run(properties: &Path, notices: &Notices, stop: &AtomicBool, waited: &mut dyn FnMut(Duration)) {
    let mut wait = FIRST_WAIT;
    let mut said = String::new();
    while !stop.load(Ordering::Relaxed) {
        let mut connected = false;
        let result = std::fs::read_to_string(properties)
            .map_err(|e| format!("cannot read {}: {e}", properties.display()))
            .and_then(|text| Settings::from_properties(&text))
            .and_then(|settings| session(&settings, notices, stop, &mut connected));
        notices.set_watching(false);
        let message = match &result {
            Ok(()) if stop.load(Ordering::Relaxed) => break,
            Ok(()) => "the server closed the management connection".to_owned(),
            Err(why) => why.clone(),
        };
        if connected {
            // It was up: the first wait again.
            wait = FIRST_WAIT;
        }
        if message != said {
            eprintln!("meridian-sidecar: management protocol: {message}; trying again");
            said = message;
        }
        waited(wait);
        let until = Instant::now() + wait;
        while !stop.load(Ordering::Relaxed) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(50));
        }
        wait = (wait * 2).min(LONGEST_WAIT);
    }
}

#[cfg(test)]
mod tests;
