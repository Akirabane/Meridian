//! What the accept loop, the players' threads and the console share: the
//! configuration, which the console can reload, the sessions being relayed, the
//! stop flag, and how backends are reached.
use crate::restart::{Cause, Detector, Evacuate, Evacuator, Settings};
use crate::session::{Connect, Sessions};
use crate::throttle::Throttle;
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
    /// Shared with the evacuation of a backend that is going down, which runs on a
    /// thread of its own and reads the configuration in force when it starts.
    config: Arc<RwLock<Arc<Config>>>,
    /// The file the configuration was read from, for `reload`.
    pub config_path: PathBuf,
    pub sessions: Arc<Sessions>,
    stop: Arc<AtomicBool>,
    pub connect: Arc<Connect>,
    pub probe: Arc<Probe>,
    /// The connections each address opened this minute.
    pub throttle: Throttle,
    /// The connections that are logging in now.
    pub logins: Arc<crate::throttle::Logins>,
    /// Which backends are going down (docs/RESTART.md).
    pub restarts: Detector,
    /// The bans and whitelist changes still to be told to the servers (`[access.sync]`).
    pub access_sync: Arc<crate::access::sync::Outbox>,
}

impl Proxy {
    /// The state of a running proxy: backends are reached over TCP, and `stop`
    /// is the flag Ctrl+C sets.
    pub fn new(config: Config, config_path: PathBuf, stop: Arc<AtomicBool>) -> Self {
        let sessions = Arc::new(Sessions::default());
        sessions.operators.follow(&config);
        let settings = Settings::from(&config.restart);
        let config = Arc::new(RwLock::new(Arc::new(config)));
        sessions.follow_config(Arc::clone(&config));
        let restarts = Detector::new(
            Arc::clone(&sessions.balancer),
            Arc::new(Counted {
                metrics: Arc::clone(sessions.metrics()),
                inner: Evacuator::new(Arc::clone(&sessions), Arc::clone(&config)),
            }),
            settings,
        );
        Self {
            config,
            config_path,
            sessions,
            stop,
            connect: Arc::new(backend::connect),
            probe: Arc::new(tcp_probe),
            throttle: Throttle::default(),
            logins: Arc::default(),
            restarts,
            access_sync: Arc::default(),
        }
    }

    /// The configuration now in force. A login reads it once and keeps it, so a
    /// reload applies to the logins that start after it.
    pub fn config(&self) -> Arc<Config> {
        Arc::clone(&self.config.read().unwrap_or_else(|e| e.into_inner()))
    }

    /// Puts `config` in force for the logins that start from now on.
    pub fn set_config(&self, config: Config) {
        // Players who wait for a place see the new limit at once.
        self.sessions.queue.set_max_players(config.max_players);
        // An operator of a server that no longer counts, or that has no sidecar any more, stops
        // being one at once.
        self.sessions.operators.follow(&config);
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

/// The evacuation of a backend that is going down, counted once for each restart the
/// detector finds: it calls the evacuation exactly then.
struct Counted<E> {
    metrics: Arc<crate::metrics::Metrics>,
    inner: E,
}

impl<E: Evacuate> Evacuate for Counted<E> {
    fn evacuate(&self, backend: &str, cause: Cause) {
        self.metrics.restart_detected(cause);
        self.inner.evacuate(backend, cause);
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
