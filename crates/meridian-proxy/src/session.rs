//! One player's session: the login to the proxy, the backend chosen for them, the
//! login there with the player information forwarding, then the relay until either
//! side leaves. [`Sessions`] keeps every session being relayed: who the player
//! is, where they are, and the handle that ends the session.
use crate::log::{debug, info, warning};
use crate::route::{self, Full};
use horizon_protocol::{connection::Connection, packets::configuration, text};
use meridian_config::Config;
use meridian_net::{
    Profile,
    backend::{BackendLink, BackendOptions},
    client::{self, ClientSession},
    relay::{Handle, Relay},
};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// How the proxy reaches a backend: [`meridian_net::backend::connect`] when it
/// runs, a pipe to a fake backend in tests.
pub type Connect =
    dyn Fn(&str, &Profile, &BackendOptions) -> meridian_net::Result<BackendLink> + Send + Sync;

/// How long a refused player is given to read the reason and close, before the
/// proxy closes the connection itself.
const REFUSAL_GRACE: Duration = Duration::from_secs(2);

/// A player whose session is being relayed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Player {
    pub name: String,
    pub uuid: [u8; 16],
    /// The name of the backend they are on.
    pub backend: String,
    /// Where they connected from.
    pub address: SocketAddr,
    /// When the relay started.
    pub joined: Instant,
}

/// The sessions being relayed.
#[derive(Default)]
pub struct Sessions {
    next: AtomicU64,
    closing: AtomicBool,
    open: Mutex<HashMap<u64, (Player, Handle)>>,
    /// The players let in, from the check of the player limit to the end of
    /// their session, including those still being sent to a backend.
    admitted: AtomicUsize,
}

/// A place taken under the player limit, given back when it is dropped.
pub(crate) struct Admission<'a>(&'a AtomicUsize);

impl Drop for Admission<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

impl Sessions {
    /// Takes a place for one more player under `max_players` (0: no limit). The
    /// check and the count are one step, so two players logging in at once cannot
    /// both take the last place.
    pub(crate) fn admit(&self, max_players: u32) -> Result<Admission<'_>, Full> {
        let mut now = self.admitted.load(Ordering::Relaxed);
        loop {
            route::admit(max_players, now)?;
            match self.admitted.compare_exchange_weak(
                now,
                now + 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Ok(Admission(&self.admitted)),
                // Another login took or gave back a place: check again.
                Err(actual) => now = actual,
            }
        }
    }

    /// How many players hold a place under the limit.
    #[cfg(test)]
    pub fn admitted(&self) -> usize {
        self.admitted.load(Ordering::Relaxed)
    }

    /// Counts a session in. `None` once the proxy is stopping: the session must
    /// end at once.
    pub(crate) fn open(&self, player: Player, handle: Handle) -> Option<Open<'_>> {
        let mut open = self.lock();
        // Checked under the lock, so a stop cannot miss a session opening now.
        if self.closing.load(Ordering::Relaxed) {
            return None;
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        open.insert(id, (player, handle));
        Some(Open { sessions: self, id })
    }

    /// The players being relayed, in the order they joined.
    pub fn players(&self) -> Vec<Player> {
        let mut players: Vec<Player> = self.lock().values().map(|(p, _)| p.clone()).collect();
        players.sort_by_key(|p| p.joined);
        players
    }

    /// How many sessions are being relayed.
    #[cfg(test)]
    pub fn count(&self) -> usize {
        self.lock().len()
    }

    /// The player called `name`, whatever its case.
    pub fn find(&self, name: &str) -> Option<Player> {
        self.lock()
            .values()
            .find(|(p, _)| p.name.eq_ignore_ascii_case(name))
            .map(|(p, _)| p.clone())
    }

    /// Ends the session of the player called `name`, telling them `reason`.
    /// Returns the player, or `None` if no one has that name.
    pub fn kick(&self, name: &str, reason: &str) -> Option<Player> {
        let (player, handle) = self
            .lock()
            .values()
            .find(|(p, _)| p.name.eq_ignore_ascii_case(name))
            .cloned()?;
        handle.disconnect(configuration::Disconnect {
            reason: text::literal(reason),
        });
        Some(player)
    }

    /// Ends every session, telling each player that the server closed, and refuses
    /// new ones. Returns how many sessions were ended.
    pub fn close_all(&self) -> usize {
        let open = self.lock();
        self.closing.store(true, Ordering::Relaxed);
        for (_, handle) in open.values() {
            // `multiplayer.disconnect.server_shutdown`: the "Server closed" of the
            // player's own language.
            handle.disconnect(configuration::Disconnect {
                reason: text::translatable("multiplayer.disconnect.server_shutdown", &[]),
            });
        }
        open.len()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<u64, (Player, Handle)>> {
        self.open.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// A session counted in [`Sessions`] until it is dropped.
pub(crate) struct Open<'a> {
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
    // The address lets the hybrid mode limit name lookups per address.
    match client::accept_from(connection, config, peer.ip()) {
        Ok(Some(session)) => serve(session, peer, config, connect, sessions),
        Ok(None) => debug!("{peer}: status answered, or login refused"),
        Err(e) => warning!("{peer}: {e}; closing the connection"),
    }
}

/// The kind of player the routes know, from how the login was checked.
pub fn route_auth(auth: client::AuthKind) -> meridian_config::AuthKind {
    match auth {
        client::AuthKind::Premium => meridian_config::AuthKind::Verified,
        client::AuthKind::Offline => meridian_config::AuthKind::Unverified,
    }
}

/// A player who has logged in to the proxy: a place under the player limit, the
/// first backend that answers among the ones the routes allow, then the relay.
pub(crate) fn serve(
    session: ClientSession,
    peer: SocketAddr,
    config: &Config,
    connect: &Connect,
    sessions: &Sessions,
) {
    let name = session.profile.name.clone();
    let uuid = session.profile.uuid;
    let Ok(_admission) = sessions.admit(config.max_players) else {
        info!(
            "{peer}: {name} ({}) refused: the proxy has its {} players",
            hyphenated(uuid),
            config.max_players
        );
        refuse(session.connection, Full::MESSAGE);
        return;
    };
    // Counted as online in the server list from now until the session ends.
    let _seat = client::Seat::take();
    let auth = route_auth(session.auth);
    let candidates = match route::candidates(config, session.virtual_host.as_deref(), auth) {
        Ok(candidates) => candidates,
        Err(e) => {
            warning!(
                "{peer}: {name} ({}): no backend to send them to: {e}",
                hyphenated(uuid)
            );
            refuse(session.connection, "There is no server to join right now.");
            return;
        }
    };
    let options = BackendOptions {
        forwarding_secret: config.forwarding_secret.as_bytes().to_vec(),
        connect_timeout: Duration::from_millis(config.connect_timeout_ms),
        player_address: peer.ip(),
    };
    // Each candidate in turn, with the connect timeout for each.
    let mut joined = None;
    for candidate in &candidates {
        let Some(backend) = config.backend(candidate) else {
            continue;
        };
        match connect(&backend.address, &session.profile, &options) {
            Ok(link) => {
                joined = Some((backend, link));
                break;
            }
            Err(e) => warning!(
                "{peer}: {name} ({}) cannot join backend \"{}\" at {}: {e}",
                hyphenated(uuid),
                backend.name,
                backend.address
            ),
        }
    }
    let Some((backend, link)) = joined else {
        refuse(session.connection, &cannot_reach(&candidates));
        return;
    };
    let (relay, handle) = match Relay::new(session, link) {
        Ok(relay) => relay,
        Err(e) => {
            warning!("{peer}: {name}: cannot set up the session: {e}");
            return;
        }
    };
    let player = Player {
        name: name.clone(),
        uuid,
        backend: backend.name.clone(),
        address: peer,
        joined: Instant::now(),
    };
    let Some(_open) = sessions.open(player, handle.clone()) else {
        debug!("{peer}: {name}: the proxy is stopping; not relaying");
        handle.disconnect(configuration::Disconnect {
            reason: text::translatable("multiplayer.disconnect.server_shutdown", &[]),
        });
        return;
    };
    info!(
        "{peer}: {name} ({}) joined backend \"{}\"",
        hyphenated(uuid),
        backend.name
    );
    let started = Instant::now();
    match relay.run() {
        Ok(report) => info!(
            "{peer}: {name} ({}) left backend \"{}\" after {:.1} s: \
             {} packets, {} bytes to the player; {} packets, {} bytes to the backend",
            hyphenated(uuid),
            backend.name,
            started.elapsed().as_secs_f64(),
            report.to_client.packets,
            report.to_client.bytes,
            report.to_backend.packets,
            report.to_backend.bytes
        ),
        Err(e) => warning!(
            "{peer}: {name} ({}) on backend \"{}\": the session broke after {:.1} s: {e}",
            hyphenated(uuid),
            backend.name,
            started.elapsed().as_secs_f64()
        ),
    }
}

/// What a player is told when none of the backends tried answers.
fn cannot_reach(tried: &[String]) -> String {
    let names: Vec<String> = tried.iter().map(|n| format!("\"{n}\"")).collect();
    match names.as_slice() {
        [one] => format!("Cannot reach the server {one} right now. Try again in a moment."),
        many => format!(
            "Cannot reach the servers {} right now. Try again in a moment.",
            many.join(", ")
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
pub fn hyphenated(uuid: [u8; 16]) -> String {
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
pub(crate) mod tests {
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
        let player_trace = Recorder::new();
        let client = FakeClient {
            after_login: vec![
                Step::Expect {
                    id: ids::configuration::clientbound::DISCONNECT,
                },
                Step::ExpectClose,
            ],
            ..FakeClient::named("Alex")
        }
        .spawn(player_end, player_trace.clone());
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
        // The player is told the server closed, in their own language.
        let trace = player_trace.trace();
        let configuration::Clientbound::Disconnect(disconnect) =
            configuration::Clientbound::decode(trace.packets[0].id, &trace.packets[0].payload)
                .unwrap()
        else {
            panic!("not a disconnect");
        };
        assert_eq!(
            disconnect.reason,
            text::translatable("multiplayer.disconnect.server_shutdown", &[])
        );
    }

    /// A session relayed between two pipes whose far ends do nothing, with the
    /// player's and the backend's ends handed back.
    pub fn idle_session(name: &str) -> (Relay, Handle, Connection, Connection) {
        let (proxy_client, player) = pipe();
        let (proxy_backend, server) = pipe();
        let client = client::ClientSession {
            profile: Profile {
                name: name.to_owned(),
                uuid: offline_uuid(name).to_be_bytes(),
                properties: Vec::new(),
            },
            auth: client::AuthKind::Offline,
            virtual_host: None,
            connection: proxy_client,
        };
        let backend = BackendLink {
            connection: proxy_backend,
        };
        let (relay, handle) = Relay::new(client, backend).unwrap();
        (relay, handle, player, server)
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
        let (_relay, handle, _player, _server) = idle_session("Alex");
        let player = Player {
            name: "Alex".into(),
            uuid: [0; 16],
            backend: "lobby".into(),
            address: peer(),
            joined: Instant::now(),
        };
        assert!(sessions.open(player, handle).is_none());
        assert_eq!(sessions.count(), 0);
    }

    /// The registry lists players in the order they joined, finds them whatever
    /// the case of the name, and a kick ends only that player's session.
    #[test]
    fn the_registry_lists_finds_and_kicks_players() {
        let sessions = Sessions::default();
        let mut ends = Vec::new();
        let mut opened = Vec::new();
        let start = Instant::now();
        for (i, name) in ["Alex", "Steve"].into_iter().enumerate() {
            let (relay, handle, player, server) = idle_session(name);
            let player_info = Player {
                name: name.into(),
                uuid: offline_uuid(name).to_be_bytes(),
                backend: "lobby".into(),
                address: peer(),
                joined: start + Duration::from_millis(i as u64),
            };
            opened.push(sessions.open(player_info, handle).unwrap());
            ends.push((std::thread::spawn(move || relay.run()), player, server));
        }
        let names: Vec<String> = sessions.players().into_iter().map(|p| p.name).collect();
        assert_eq!(names, ["Alex", "Steve"]);
        assert_eq!(sessions.find("steve").unwrap().name, "Steve");
        assert!(sessions.find("Herobrine").is_none());
        assert!(sessions.kick("Herobrine", "bye").is_none());
        assert_eq!(sessions.kick("ALEX", "bye").unwrap().name, "Alex");
        let (relay, mut player, _server) = ends.remove(0);
        assert!(relay.join().unwrap().is_ok());
        let (id, payload) = player.recv().unwrap();
        let configuration::Clientbound::Disconnect(d) =
            configuration::Clientbound::decode(id, &payload).unwrap()
        else {
            panic!("not a disconnect");
        };
        assert_eq!(text::plain(&d.reason, &Default::default()), "bye");
        // Steve is still relayed.
        drop(opened.remove(0));
        assert_eq!(sessions.count(), 1);
        drop(opened);
        for (relay, player, server) in ends {
            drop((player, server));
            assert!(relay.join().unwrap().is_ok());
        }
    }

    /// A player already logged in to the proxy: the session the proxy holds, and
    /// the player's own end, in the configuration phase.
    fn logged_in(
        name: &str,
        auth: client::AuthKind,
        host: Option<&str>,
    ) -> (ClientSession, Connection) {
        let (proxy_end, player) = pipe();
        let session = ClientSession {
            profile: Profile {
                name: name.to_owned(),
                uuid: offline_uuid(name).to_be_bytes(),
                properties: Vec::new(),
            },
            auth,
            virtual_host: host.map(str::to_owned),
            connection: proxy_end,
        };
        (session, player)
    }

    /// The backends of the routing tests: lobby, east and west, an offline proxy.
    fn routed() -> Config {
        let backend = |name: &str, port: u16| Backend {
            name: name.into(),
            address: format!("127.0.0.1:{port}"),
        };
        Config {
            backends: vec![
                backend("lobby", 25601),
                backend("east", 25602),
                backend("west", 25603),
            ],
            default_backend: "lobby".into(),
            online_mode: false,
            auth_mode: meridian_config::AuthMode::Offline,
            forwarding_secret: SECRET.into(),
            ..Config::default()
        }
    }

    /// Backends reached through pipes, by address: `None` is one that does not
    /// answer. Keeps the addresses tried, in order, and the backends' threads.
    struct Reachable {
        tried: Arc<Mutex<Vec<String>>>,
        threads: Arc<Mutex<Vec<BackendThread>>>,
    }

    fn reachable(backends: Vec<(&str, bool)>) -> (Arc<Connect>, Reachable) {
        let answering: HashMap<String, Option<FakeBackend>> = backends
            .into_iter()
            .map(|(address, answers)| {
                let backend = answers.then(|| FakeBackend {
                    forwarding_secret: Some(SECRET.as_bytes().to_vec()),
                    after_login: vec![Step::Close],
                    ..FakeBackend::default()
                });
                (address.to_owned(), backend)
            })
            .collect();
        let answering = Mutex::new(answering);
        let kept = Reachable {
            tried: Arc::new(Mutex::new(Vec::new())),
            threads: Arc::new(Mutex::new(Vec::new())),
        };
        let (tried, threads) = (Arc::clone(&kept.tried), Arc::clone(&kept.threads));
        let connect: Arc<Connect> = Arc::new(move |address, profile, options| {
            tried.lock().unwrap().push(address.to_owned());
            let fake = answering
                .lock()
                .unwrap()
                .get_mut(address)
                .and_then(Option::take);
            let Some(fake) = fake else {
                return Err(meridian_net::Error::Io(std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    "refused",
                )));
            };
            let (proxy_end, server_end) = pipe();
            threads
                .lock()
                .unwrap()
                .push(fake.spawn(server_end, Recorder::new()));
            backend::login(proxy_end, "127.0.0.1", 25601, profile, options)
        });
        (connect, kept)
    }

    /// Runs one logged-in player through the routing. The player's end runs
    /// `script`; returns the addresses tried and what the player received.
    fn route_one(
        config: &Config,
        sessions: &Sessions,
        connect: &Connect,
        reachable: &Reachable,
        session: (ClientSession, Connection),
        script: Vec<Step>,
    ) -> (Vec<String>, Trace) {
        let (session, player) = session;
        let recorder = Recorder::new();
        let player = meridian_testkit::peer::spawn(player, script, Dir::ToClient, recorder.clone());
        serve(session, peer(), config, connect, sessions);
        player.join().unwrap().unwrap();
        for thread in reachable.threads.lock().unwrap().drain(..) {
            thread.join().unwrap().unwrap();
        }
        let tried = std::mem::take(&mut *reachable.tried.lock().unwrap());
        (tried, recorder.trace())
    }

    /// The reason of the configuration-phase Disconnect a player received.
    fn refusal(trace: &Trace) -> String {
        let packet = &trace.packets[0];
        let configuration::Clientbound::Disconnect(d) =
            configuration::Clientbound::decode(packet.id, &packet.payload).unwrap()
        else {
            panic!("not a disconnect");
        };
        text::plain(&d.reason, &Default::default())
    }

    fn refused() -> Vec<Step> {
        vec![
            Step::Expect {
                id: ids::configuration::clientbound::DISCONNECT,
            },
            Step::Close,
        ]
    }

    #[test]
    fn the_route_knows_both_kinds_of_login() {
        use meridian_config::AuthKind;
        assert_eq!(route_auth(client::AuthKind::Premium), AuthKind::Verified);
        assert_eq!(route_auth(client::AuthKind::Offline), AuthKind::Unverified);
    }

    /// A host name forced to east sends the player there, not to the default.
    #[test]
    fn a_forced_host_chooses_the_backend() {
        let config = Config {
            forced_hosts: vec![meridian_config::ForcedHost {
                host: "east.example.com".into(),
                backend: "east".into(),
            }],
            ..routed()
        };
        let sessions = Sessions::default();
        let (connect, backends) =
            reachable(vec![("127.0.0.1:25601", true), ("127.0.0.1:25602", true)]);
        let (tried, _) = route_one(
            &config,
            &sessions,
            &*connect,
            &backends,
            logged_in("Alex", client::AuthKind::Offline, Some("east.example.com")),
            vec![Step::ExpectClose],
        );
        assert_eq!(tried, ["127.0.0.1:25602"]);
        assert_eq!(sessions.admitted(), 0, "the place is given back at the end");
    }

    /// The default backend is down: the player goes to the next in `try`.
    #[test]
    fn a_player_falls_back_when_the_first_backend_is_down() {
        let config = Config {
            try_backends: vec!["east".into()],
            ..routed()
        };
        let sessions = Sessions::default();
        let (connect, backends) =
            reachable(vec![("127.0.0.1:25601", false), ("127.0.0.1:25602", true)]);
        let (tried, _) = route_one(
            &config,
            &sessions,
            &*connect,
            &backends,
            logged_in("Alex", client::AuthKind::Offline, None),
            vec![Step::ExpectClose],
        );
        assert_eq!(tried, ["127.0.0.1:25601", "127.0.0.1:25602"]);
    }

    /// None of the candidates answers: the player is told which were tried.
    #[test]
    fn a_player_is_told_when_no_candidate_answers() {
        let config = Config {
            try_backends: vec!["east".into()],
            ..routed()
        };
        let sessions = Sessions::default();
        let (connect, backends) =
            reachable(vec![("127.0.0.1:25601", false), ("127.0.0.1:25602", false)]);
        let (tried, trace) = route_one(
            &config,
            &sessions,
            &*connect,
            &backends,
            logged_in("Alex", client::AuthKind::Offline, None),
            refused(),
        );
        assert_eq!(tried, ["127.0.0.1:25601", "127.0.0.1:25602"]);
        assert_eq!(
            refusal(&trace),
            "Cannot reach the servers \"lobby\", \"east\" right now. Try again in a moment."
        );
        assert_eq!(sessions.admitted(), 0);
    }

    /// At the player limit, a player is refused before any backend is tried.
    #[test]
    fn a_full_proxy_refuses_the_player() {
        let config = Config {
            max_players: 1,
            ..routed()
        };
        let sessions = Sessions::default();
        let held = sessions.admit(1).unwrap();
        let (connect, backends) = reachable(vec![("127.0.0.1:25601", true)]);
        let (tried, trace) = route_one(
            &config,
            &sessions,
            &*connect,
            &backends,
            logged_in("Steve", client::AuthKind::Offline, None),
            refused(),
        );
        assert!(tried.is_empty());
        assert_eq!(refusal(&trace), "The server is full");
        assert_eq!(sessions.admitted(), 1, "only the place already held");
        drop(held);
        assert_eq!(sessions.admitted(), 0);
    }

    /// The limit counts places as they are taken, not sessions as they open.
    #[test]
    fn the_last_place_is_taken_once() {
        let sessions = Sessions::default();
        let first = sessions.admit(2).unwrap();
        let second = sessions.admit(2).unwrap();
        assert!(sessions.admit(2).is_err());
        drop(first);
        let third = sessions.admit(2).unwrap();
        assert!(sessions.admit(0).is_ok(), "0 is no limit");
        drop((second, third));
    }

    /// The hybrid mode: a verified player goes to the verified group's backend,
    /// an unverified one to the other group's, each never to the other's.
    #[test]
    fn the_hybrid_mode_sends_each_kind_to_its_group() {
        use meridian_config::{AuthKind, AuthMode, Group, Route};
        let config = Config {
            online_mode: true,
            auth_mode: AuthMode::Hybrid,
            groups: vec![
                Group {
                    name: "premium".into(),
                    auth: AuthKind::Verified,
                    backends: vec!["lobby".into()],
                },
                Group {
                    name: "cracked".into(),
                    auth: AuthKind::Unverified,
                    backends: vec!["east".into()],
                },
            ],
            routes: vec![
                Route {
                    auth: AuthKind::Verified,
                    group: "premium".into(),
                },
                Route {
                    auth: AuthKind::Unverified,
                    group: "cracked".into(),
                },
            ],
            ..routed()
        };
        config.validate().unwrap();
        let sessions = Sessions::default();
        let (connect, backends) =
            reachable(vec![("127.0.0.1:25601", true), ("127.0.0.1:25602", true)]);
        let (tried, _) = route_one(
            &config,
            &sessions,
            &*connect,
            &backends,
            logged_in("Notch", client::AuthKind::Premium, None),
            vec![Step::ExpectClose],
        );
        assert_eq!(tried, ["127.0.0.1:25601"]);
        let (tried, _) = route_one(
            &config,
            &sessions,
            &*connect,
            &backends,
            logged_in("guest", client::AuthKind::Offline, None),
            vec![Step::ExpectClose],
        );
        assert_eq!(tried, ["127.0.0.1:25602"]);
    }
}
