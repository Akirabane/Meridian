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
    use horizon_protocol::{crypto::offline_uuid, ids};
    use meridian_config::Backend;
    use meridian_net::backend;
    use meridian_testkit::{
        backend::{FakeBackend, Forwarded, Login, acknowledge_configuration, finish_configuration},
        client::FakeClient,
        peer::Step,
        pipe,
        trace::{Dir, Recorder, Trace},
    };
    use std::sync::Arc;
    use std::thread::JoinHandle;

    const SECRET: &str = "a shared secret";

    fn config() -> Config {
        Config {
            backends: vec![Backend {
                name: "lobby".into(),
                address: "127.0.0.1:25601".into(),
            }],
            default_backend: "lobby".into(),
            online_mode: false,
            compression_threshold: 256,
            forwarding_secret: SECRET.into(),
            ..Config::default()
        }
    }

    fn peer() -> SocketAddr {
        "198.51.100.7:50123".parse().unwrap()
    }

    type BackendThread = JoinHandle<Result<(Login, Connection), String>>;

    /// What the connector was asked for: the address, the options, and the fake
    /// backend's thread.
    type Seen = Arc<Mutex<Option<(String, BackendOptions, BackendThread)>>>;

    /// A connector that joins the proxy to `backend` through a pipe instead of a
    /// socket, keeps what it was asked for, and hands the backend's thread back.
    fn connector(backend: FakeBackend, recorder: Recorder) -> (Arc<Connect>, Seen) {
        let seen = Arc::new(Mutex::new(None));
        let kept = Arc::clone(&seen);
        let backend = Mutex::new(Some(backend));
        let connect: Arc<Connect> = Arc::new(move |address, profile, options| {
            let (proxy_end, server_end) = pipe();
            let fake = backend.lock().unwrap().take().expect("one connection");
            let thread = fake.spawn(server_end, recorder.clone());
            *kept.lock().unwrap() = Some((address.to_owned(), options.clone(), thread));
            backend::login(proxy_end, "127.0.0.1", 25601, profile, options)
        });
        (connect, seen)
    }

    /// The configuration phase, keep-alives both ways, the play phase, and the
    /// backend closing: the two scripts mirror each other.
    fn scripts() -> (Vec<Step>, Vec<Step>) {
        use ids::{configuration as cfg, play};
        let backend = vec![
            Step::send(cfg::clientbound::REGISTRY_DATA, vec![5; 700]),
            Step::send(cfg::clientbound::KEEP_ALIVE, 11u64.to_be_bytes()),
            Step::expect_exactly(cfg::serverbound::KEEP_ALIVE, 11u64.to_be_bytes()),
            finish_configuration(),
            Step::Expect {
                id: cfg::serverbound::FINISH_CONFIGURATION,
            },
            Step::send(play::clientbound::LOGIN, vec![6; 60]),
            Step::send(play::clientbound::KEEP_ALIVE, 12u64.to_be_bytes()),
            Step::expect_exactly(play::serverbound::KEEP_ALIVE, 12u64.to_be_bytes()),
            Step::Close,
        ];
        let client = vec![
            Step::expect_exactly(cfg::clientbound::REGISTRY_DATA, vec![5; 700]),
            Step::expect_exactly(cfg::clientbound::KEEP_ALIVE, 11u64.to_be_bytes()),
            Step::send(cfg::serverbound::KEEP_ALIVE, 11u64.to_be_bytes()),
            Step::Expect {
                id: cfg::clientbound::FINISH_CONFIGURATION,
            },
            acknowledge_configuration(),
            Step::expect_exactly(play::clientbound::LOGIN, vec![6; 60]),
            Step::expect_exactly(play::clientbound::KEEP_ALIVE, 12u64.to_be_bytes()),
            Step::send(play::serverbound::KEEP_ALIVE, 12u64.to_be_bytes()),
            Step::ExpectClose,
        ];
        (backend, client)
    }

    /// The packets after the login, which is where a direct session and a
    /// relayed one must agree: what the client received, then what the backend
    /// received once its `login_packets` login packets are left out.
    fn after_login(trace: &Trace, login_packets: usize) -> Trace {
        let to_client = trace.packets(Dir::ToClient).cloned();
        let to_server = trace.packets(Dir::ToServer).skip(login_packets).cloned();
        Trace {
            packets: to_client.chain(to_server).collect(),
        }
    }

    /// J1's proof in memory: a player logs in to the proxy, which logs them in to
    /// the backend with the forwarding and relays the session. Both sides see
    /// exactly what they see when the player joins the backend directly.
    #[test]
    fn a_player_is_relayed_to_the_backend_as_if_joined_directly() {
        let (backend_script, client_script) = scripts();

        // Directly: the client logs in to the backend itself.
        let direct = Recorder::new();
        let (client_end, server_end) = pipe();
        let server = FakeBackend {
            compression: Some(64),
            after_login: backend_script.clone(),
            ..FakeBackend::default()
        }
        .spawn(server_end, direct.clone());
        let client = FakeClient {
            after_login: client_script.clone(),
            ..FakeClient::named("Alex")
        }
        .spawn(client_end, direct.clone());
        client.join().unwrap().unwrap();
        server.join().unwrap().unwrap();

        // Through the proxy.
        let relayed = Recorder::new();
        let (connect, seen) = connector(
            FakeBackend {
                compression: Some(64),
                forwarding_secret: Some(SECRET.as_bytes().to_vec()),
                after_login: backend_script,
                ..FakeBackend::default()
            },
            relayed.clone(),
        );
        let (player_end, proxy_end) = pipe();
        let sessions = Arc::new(Sessions::default());
        let proxy = {
            let sessions = Arc::clone(&sessions);
            std::thread::spawn(move || handle(proxy_end, peer(), &config(), &*connect, &sessions))
        };
        let client = FakeClient {
            after_login: client_script,
            ..FakeClient::named("Alex")
        }
        .spawn(player_end, relayed.clone());
        let (welcome, _) = client.join().unwrap().unwrap();
        proxy.join().unwrap();
        let (address, options, server) = seen.lock().unwrap().take().unwrap();
        let (login, _) = server.join().unwrap().unwrap();

        // Direct: handshake, Login Start and Login Acknowledged before the script.
        // Relayed: the forwarding answer too.
        assert_eq!(
            after_login(&relayed.trace(), 4),
            after_login(&direct.trace(), 3)
        );
        assert_eq!(welcome.compression, Some(256), "the proxy's own threshold");
        assert_eq!(address, "127.0.0.1:25601");
        assert_eq!(
            options,
            BackendOptions {
                forwarding_secret: SECRET.as_bytes().to_vec(),
                connect_timeout: Duration::from_millis(5_000),
                player_address: peer().ip(),
            }
        );
        assert_eq!(
            login.forwarded,
            Some(Forwarded {
                version: 1,
                address: "198.51.100.7".into(),
                uuid: offline_uuid("Alex"),
                name: "Alex".into(),
                properties: Vec::new(),
            })
        );
        assert_eq!(sessions.count(), 0, "the session is counted out at the end");
    }

    /// A backend that cannot be reached: the player is told so in the
    /// configuration phase, and nothing hangs.
    #[test]
    fn a_player_is_told_when_the_backend_cannot_be_reached() {
        let connect: Arc<Connect> = Arc::new(|_, _, _| {
            Err(meridian_net::Error::Io(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                "refused",
            )))
        });
        let (player_end, proxy_end) = pipe();
        let proxy = std::thread::spawn(move || {
            handle(
                proxy_end,
                peer(),
                &config(),
                &*connect,
                &Sessions::default(),
            )
        });
        let recorder = Recorder::new();
        let client = FakeClient {
            after_login: vec![
                Step::Expect {
                    id: ids::configuration::clientbound::DISCONNECT,
                },
                Step::Close,
            ],
            ..FakeClient::named("Alex")
        }
        .spawn(player_end, recorder.clone());
        client.join().unwrap().unwrap();
        proxy.join().unwrap();
        let trace = recorder.trace();
        let packet = &trace.packets[0];
        let configuration::Clientbound::Disconnect(disconnect) =
            configuration::Clientbound::decode(packet.id, &packet.payload).unwrap()
        else {
            panic!("not a disconnect");
        };
        let reason = text::plain(&disconnect.reason, &Default::default());
        assert!(reason.contains("\"lobby\""), "{reason}");
    }

    /// A stop closes a session being relayed on both sides.
    #[test]
    fn a_stop_closes_the_sessions_being_relayed() {
        let recorder = Recorder::new();
        let (connect, seen) = connector(
            FakeBackend {
                forwarding_secret: Some(SECRET.as_bytes().to_vec()),
                after_login: vec![Step::ExpectClose],
                ..FakeBackend::default()
            },
            recorder.clone(),
        );
        let (player_end, proxy_end) = pipe();
        let sessions = Arc::new(Sessions::default());
        let proxy = {
            let sessions = Arc::clone(&sessions);
            std::thread::spawn(move || handle(proxy_end, peer(), &config(), &*connect, &sessions))
        };
        let client = FakeClient {
            after_login: vec![Step::ExpectClose],
            ..FakeClient::named("Alex")
        }
        .spawn(player_end, recorder);
        let deadline = Instant::now() + Duration::from_secs(5);
        while sessions.count() == 0 {
            assert!(Instant::now() < deadline, "the session never started");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(sessions.close_all(), 1);
        client.join().unwrap().unwrap();
        proxy.join().unwrap();
        let (_, _, server) = seen.lock().unwrap().take().unwrap();
        server.join().unwrap().unwrap();
        assert_eq!(sessions.count(), 0);
    }

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
