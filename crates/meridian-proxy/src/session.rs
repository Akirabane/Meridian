//! One player's session: the login to the proxy, the backend chosen for them, the
//! login there with the player information forwarding, then the relay until either
//! side leaves.
use crate::log::{debug, info, warning};
use horizon_protocol::{
    connection::{Closer, Connection},
    packets::configuration,
    text,
};
use meridian_config::Config;
use meridian_net::{
    Profile,
    backend::{BackendLink, BackendOptions},
    client, relay,
};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// How the proxy reaches a backend: [`meridian_net::backend::connect`] when it
/// runs, a pipe to a fake backend in tests.
pub type Connect =
    dyn Fn(&str, &Profile, &BackendOptions) -> meridian_net::Result<BackendLink> + Send + Sync;

/// How long a refused player is given to read the reason and close, before the
/// proxy closes the connection itself.
const REFUSAL_GRACE: Duration = Duration::from_secs(2);

/// The sessions being relayed, so that a stop can close them all.
#[derive(Default)]
pub struct Sessions {
    next: AtomicU64,
    closing: AtomicBool,
    open: Mutex<HashMap<u64, Vec<Closer>>>,
}

impl Sessions {
    /// Counts a session in, with what closes its connections. `None` once the
    /// proxy is stopping: the session must end at once.
    fn open(&self, closers: Vec<Closer>) -> Option<Open<'_>> {
        let mut open = self.lock();
        // Checked under the lock, so a stop cannot miss a session opening now.
        if self.closing.load(Ordering::Relaxed) {
            return None;
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        open.insert(id, closers);
        Some(Open { sessions: self, id })
    }

    /// Closes both connections of every session, and refuses new ones. Returns
    /// how many sessions were closed.
    pub fn close_all(&self) -> usize {
        let open = self.lock();
        self.closing.store(true, Ordering::Relaxed);
        for closer in open.values().flatten() {
            closer.close();
        }
        open.len()
    }

    /// How many sessions are being relayed.
    #[cfg(test)]
    pub fn count(&self) -> usize {
        self.lock().len()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Vec<Closer>>> {
        self.open.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// A session counted in [`Sessions`] until it is dropped.
struct Open<'a> {
    sessions: &'a Sessions,
    id: u64,
}

impl Drop for Open<'_> {
    fn drop(&mut self) {
        self.sessions.lock().remove(&self.id);
    }
}

/// One player's connection, from the handshake to the end of the relay.
pub fn handle(
    connection: Connection,
    peer: SocketAddr,
    config: &Config,
    connect: &Connect,
    sessions: &Sessions,
) {
    let session = match client::accept(connection, config) {
        Ok(Some(session)) => session,
        Ok(None) => {
            debug!("{peer}: status answered, or login refused");
            return;
        }
        Err(e) => {
            warning!("{peer}: {e}; closing the connection");
            return;
        }
    };
    // Counted as online in the server list from now until the session ends.
    let _seat = client::Seat::take();
    let name = session.profile.name.clone();
    let uuid = hyphenated(session.profile.uuid);
    let Some(backend) = config.backend(&config.default_backend) else {
        // The configuration's check makes this impossible; refuse rather than panic.
        warning!(
            "{peer}: {name}: the default backend \"{}\" is not configured",
            config.default_backend
        );
        refuse(session.connection, "There is no server to join right now.");
        return;
    };
    let options = BackendOptions {
        forwarding_secret: config.forwarding_secret.as_bytes().to_vec(),
        connect_timeout: Duration::from_millis(config.connect_timeout_ms),
        player_address: peer.ip(),
    };
    let link = match connect(&backend.address, &session.profile, &options) {
        Ok(link) => link,
        Err(e) => {
            warning!(
                "{peer}: {name} ({uuid}) cannot join backend \"{}\" at {}: {e}",
                backend.name,
                backend.address
            );
            refuse(
                session.connection,
                &format!(
                    "Cannot reach the server \"{}\" right now. Try again in a moment.",
                    backend.name
                ),
            );
            return;
        }
    };
    let closers = match (
        session.connection.reader.closer(),
        link.connection.reader.closer(),
    ) {
        (Ok(a), Ok(b)) => vec![a, b],
        (Err(e), _) | (_, Err(e)) => {
            warning!("{peer}: {name}: cannot set up the session: {e}");
            return;
        }
    };
    let Some(_open) = sessions.open(closers) else {
        debug!("{peer}: {name}: the proxy is stopping; not relaying");
        return;
    };
    info!(
        "{peer}: {name} ({uuid}) joined backend \"{}\"",
        backend.name
    );
    let started = Instant::now();
    match relay::run_counting(session, link) {
        Ok(report) => info!(
            "{peer}: {name} ({uuid}) left backend \"{}\" after {:.1} s: \
             {} packets, {} bytes to the player; {} packets, {} bytes to the backend",
            backend.name,
            started.elapsed().as_secs_f64(),
            report.to_client.packets,
            report.to_client.bytes,
            report.to_backend.packets,
            report.to_backend.bytes
        ),
        Err(e) => warning!(
            "{peer}: {name} ({uuid}) on backend \"{}\": the session broke after {:.1} s: {e}",
            backend.name,
            started.elapsed().as_secs_f64()
        ),
    }
}

/// Tells a player in the configuration phase why the session ends, then gives
/// them a moment to read it and close, so that the reason is not lost to a reset
/// connection.
fn refuse(mut connection: Connection, reason: &str) {
    let packet = configuration::Clientbound::Disconnect(configuration::Disconnect {
        reason: text::literal(reason),
    });
    if connection.send(&packet.encode()).is_err() {
        return;
    }
    if connection
        .reader
        .set_read_timeout(Some(REFUSAL_GRACE))
        .is_err()
    {
        return;
    }
    let until = Instant::now() + REFUSAL_GRACE;
    // Whatever the client still sends is read and dropped until it closes.
    while Instant::now() < until && connection.recv().is_ok() {}
}

/// A unique id as `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`.
fn hyphenated(uuid: [u8; 16]) -> String {
    let hex = format!("{:032x}", u128::from_be_bytes(uuid));
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_unique_id_is_written_with_hyphens() {
        let uuid = 0x0123456789abcdef_fedcba9876543210_u128.to_be_bytes();
        assert_eq!(hyphenated(uuid), "01234567-89ab-cdef-fedc-ba9876543210");
    }

    #[test]
    fn a_stopping_proxy_refuses_new_sessions() {
        let sessions = Sessions::default();
        assert_eq!(sessions.close_all(), 0);
        assert!(sessions.open(Vec::new()).is_none());
        assert_eq!(sessions.count(), 0);
    }
}
