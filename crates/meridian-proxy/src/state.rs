//! What the accept loop, the players' threads and the console share: the
//! configuration, which the console can reload, the sessions being relayed, the
//! stop flag, and how backends are reached.
use crate::session::{Connect, Sessions};
use meridian_config::Config;
use meridian_net::backend;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

/// How the console checks that a backend answers: a TCP connection opened and
/// closed again, which returns how long it took. A pipe or a stand-in in tests.
pub type Probe = dyn Fn(&str, Duration) -> Result<Duration, String> + Send + Sync;

/// The proxy's shared state.
pub struct Proxy {
    config: RwLock<Arc<Config>>,
    /// The file the configuration was read from, for `reload`.
    pub config_path: PathBuf,
    pub sessions: Sessions,
    stop: Arc<AtomicBool>,
    pub connect: Arc<Connect>,
    pub probe: Arc<Probe>,
}

impl Proxy {
    /// The state of a running proxy: backends are reached over TCP, and `stop`
    /// is the flag Ctrl+C sets.
    pub fn new(config: Config, config_path: PathBuf, stop: Arc<AtomicBool>) -> Self {
        Self {
            config: RwLock::new(Arc::new(config)),
            config_path,
            sessions: Sessions::default(),
            stop,
            connect: Arc::new(backend::connect),
            probe: Arc::new(tcp_probe),
        }
    }

    /// The configuration now in force. A login reads it once and keeps it, so a
    /// reload applies to the logins that start after it.
    pub fn config(&self) -> Arc<Config> {
        Arc::clone(&self.config.read().unwrap_or_else(|e| e.into_inner()))
    }

    /// Puts `config` in force for the logins that start from now on.
    pub fn set_config(&self, config: Config) {
        *self.config.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(config);
    }

    /// Whether a stop was asked for.
    pub fn stopping(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// Asks the proxy to stop, as Ctrl+C does.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Opens a TCP connection to `address` (`host:port`) and closes it, timing it.
fn tcp_probe(address: &str, timeout: Duration) -> Result<Duration, String> {
    let target = address
        .to_socket_addrs()
        .map_err(|e| format!("cannot resolve {address}: {e}"))?
        .next()
        .ok_or_else(|| format!("{address} has no address"))?;
    let started = Instant::now();
    TcpStream::connect_timeout(&target, timeout).map_err(|e| e.to_string())?;
    Ok(started.elapsed())
}
