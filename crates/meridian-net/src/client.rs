//! The player's side: the proxy answers the handshake, the status request and the
//! login itself, and the rest of the session goes through the connection it
//! leaves behind.
//!
//! The steps follow vanilla's server (`ServerHandshakePacketListenerImpl`,
//! `ServerStatusPacketListenerImpl`, `ServerLoginPacketListenerImpl`), as Horizon's
//! server does in `horizon-client/src/lan/conn.rs`.
use crate::{Error, Profile, Result};
use horizon_protocol::{
    connection::Connection,
    crypto,
    packets::{handshake, login, status},
    rsa::RsaPrivateKey,
};
use meridian_config::Config;
use std::{
    sync::{
        OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

/// How long the handshake, a status request and the login may wait for the
/// client's next packet. Vanilla waits 30 s, but a client answers these at once,
/// and a silent socket should not hold a thread for that long.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(10);

/// The read timeout left on the connection for the rest of the session:
/// vanilla's 30 s.
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// `ServerHandshakePacketListenerImpl`: a client older than this protocol is told
/// it is outdated, any other mismatch that it is incompatible.
const OUTDATED_BELOW: i32 = 754;

/// The player limit shown in the server list. The configuration has no limit
/// yet, so the list shows vanilla's default `max-players`.
const MAX_PLAYERS_SHOWN: usize = 20;

/// A player who has logged in to the proxy and is ready to be relayed.
pub struct ClientSession {
    pub profile: Profile,
    /// How the player proved who they are, which decides the servers they may
    /// be sent to.
    pub auth: AuthKind,
    pub connection: Connection,
}

/// How a player's login was checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthKind {
    /// The session service vouched for the account: the UUID, the name and the
    /// skin are the account's.
    Premium,
    /// Nobody checked the name: the UUID is derived from it, as vanilla does in
    /// offline mode, and there is no skin.
    Offline,
}

/// How the proxy checks the login of a player called `name`. It is decided from
/// the name alone, before anything is asked of the client, so a mode that picks
/// per player (premium names checked, the others let in offline) only has to
/// change this function. For now the configuration decides for everyone.
fn auth_for(config: &Config, _name: &str) -> AuthKind {
    if config.online_mode {
        AuthKind::Premium
    } else {
        AuthKind::Offline
    }
}

/// The players the server list counts: one per live [`Seat`].
static ONLINE: AtomicUsize = AtomicUsize::new(0);

/// A player counted as online in the server list for as long as it lives. The
/// relay takes one for each session it runs.
#[derive(Debug)]
pub struct Seat(());

impl Seat {
    pub fn take() -> Self {
        ONLINE.fetch_add(1, Ordering::Relaxed);
        Seat(())
    }
}

impl Drop for Seat {
    fn drop(&mut self) {
        ONLINE.fetch_sub(1, Ordering::Relaxed);
    }
}

/// How many players the server list shows as online.
pub fn players_online() -> usize {
    ONLINE.load(Ordering::Relaxed)
}

/// Takes a new connection through handshake and login. A status request is
/// answered and ends the connection, so it returns `Ok(None)`; so does a login
/// the proxy refuses, after telling the client why.
///
/// A session is returned once the client has acknowledged the login: the
/// connection is then in the configuration phase, as a backend link is, and
/// carries the compression and the encryption the login set up.
pub fn accept(connection: Connection, config: &Config) -> Result<Option<ClientSession>> {
    accept_with(connection, config, &session::SessionService)
}

/// [`accept`], asking `sessions` who an online player is.
fn accept_with(
    mut connection: Connection,
    config: &Config,
    sessions: &dyn session::Sessions,
) -> Result<Option<ClientSession>> {
    connection.reader.set_read_timeout(Some(LOGIN_TIMEOUT))?;
    let (id, payload) = connection.recv()?;
    let intention = match handshake::Serverbound::decode(id, &payload)? {
        handshake::Serverbound::Intention(intention) => intention,
        handshake::Serverbound::Unknown(id, _) => {
            return Err(protocol_error(format!(
                "expected a handshake, got packet {id}"
            )));
        }
    };
    match intention.intent {
        handshake::STATUS => {
            serve_status(&mut connection, config)?;
            Ok(None)
        }
        handshake::LOGIN => log_in(connection, config, sessions, intention.protocol),
        // The proxy is where players arrive; it takes no transfers from other
        // servers, as a vanilla server does by default (`accepts-transfers`).
        handshake::TRANSFER => {
            disconnect(
                &mut connection,
                &translatable("multiplayer.disconnect.transfers_disabled", &[]),
            );
            Ok(None)
        }
        other => Err(protocol_error(format!("unknown intent {other}"))),
    }
}

/// Who the login says the player is.
#[derive(Debug)]
struct Joined {
    uuid: u128,
    name: String,
    properties: Vec<login::Property>,
}

/// `ServerLoginPacketListenerImpl`: the version, the hello, who the player is,
/// then [`finish`].
fn log_in(
    mut connection: Connection,
    config: &Config,
    sessions: &dyn session::Sessions,
    protocol: i32,
) -> Result<Option<ClientSession>> {
    if protocol != horizon_protocol::PROTOCOL_VERSION {
        let key = if protocol < OUTDATED_BELOW {
            "multiplayer.disconnect.outdated_client"
        } else {
            "multiplayer.disconnect.incompatible"
        };
        disconnect(
            &mut connection,
            &translatable(key, &[horizon_protocol::GAME_VERSION]),
        );
        return Ok(None);
    }
    let (id, payload) = connection.recv()?;
    let hello = match login::Serverbound::decode(id, &payload)? {
        login::Serverbound::Hello(hello) => hello,
        other => {
            return Err(protocol_error(format!(
                "expected a login hello, got packet {}",
                other.id()
            )));
        }
    };
    // handleHello's `validState` throws, and the exception is the disconnect
    // reason the client sees.
    if !valid_name(&hello.name) {
        disconnect(
            &mut connection,
            &translatable(
                "disconnect.genericReason",
                &[
                    "Internal Exception: java.lang.IllegalStateException: Invalid characters in username",
                ],
            ),
        );
        return Ok(None);
    }
    let auth = auth_for(config, &hello.name);
    let joined = match auth {
        AuthKind::Premium => match authenticate(&mut connection, &hello.name, sessions)? {
            Some(joined) => joined,
            None => return Ok(None),
        },
        // UUIDUtil.createOfflineProfile: the UUID is derived from the name, and
        // the UUID the client sent is ignored.
        AuthKind::Offline => Joined {
            uuid: crypto::offline_uuid(&hello.name),
            name: hello.name,
            properties: Vec::new(),
        },
    };
    finish(connection, config, joined, auth).map(Some)
}

/// The proxy's key pair (`MinecraftServer.initializeKeyPair`), made at the first
/// online login and kept for the life of the proxy.
struct ProxyKey {
    key: RsaPrivateKey,
    /// The public key as X.509, sent in the encryption request.
    public_der: Vec<u8>,
}

fn proxy_key() -> Result<&'static ProxyKey> {
    static KEY: OnceLock<ProxyKey> = OnceLock::new();
    if let Some(key) = KEY.get() {
        return Ok(key);
    }
    // Two first logins at once may both make one; the second is dropped.
    let key = RsaPrivateKey::generate(1024)?;
    let public_der = key.public_der();
    Ok(KEY.get_or_init(|| ProxyKey { key, public_der }))
}

/// `ServerLoginPacketListenerImpl.handleKey` and its authenticator: the
/// encryption request, the client's answer, encryption on, and the session
/// service's word on who the player is. `None` when the player is refused, after
/// they were told why.
fn authenticate(
    connection: &mut Connection,
    name: &str,
    sessions: &dyn session::Sessions,
) -> Result<Option<Joined>> {
    let key = proxy_key()?;
    let mut challenge = [0u8; 4];
    crypto::random_bytes(&mut challenge)?;
    let request = login::Clientbound::Hello(login::Hello {
        server_id: String::new(),
        public_key: key.public_der.clone(),
        challenge: challenge.to_vec(),
        should_authenticate: true,
    });
    connection.send(&request.encode())?;
    let (id, payload) = connection.recv()?;
    let answer = match login::Serverbound::decode(id, &payload)? {
        login::Serverbound::Key(answer) => answer,
        other => {
            return Err(protocol_error(format!(
                "expected the encryption answer, got packet {}",
                other.id()
            )));
        }
    };
    // ServerboundKeyPacket.isChallengeValid and getSecretKey.
    if key.key.decrypt(&answer.challenge).ok().as_deref() != Some(&challenge[..]) {
        return Err(protocol_error(
            "the encryption answer does not match the challenge".into(),
        ));
    }
    let secret: [u8; 16] = key
        .key
        .decrypt(&answer.key)
        .ok()
        .and_then(|secret| secret.try_into().ok())
        .ok_or_else(|| protocol_error("the shared secret is not 16 bytes".into()))?;
    let server_hash = crypto::server_hash("", &secret, &key.public_der);
    connection.enable_encryption(&secret);
    let refusal = match sessions.has_joined(name, &server_hash) {
        Ok(Some(joined)) => return Ok(Some(joined)),
        Ok(None) => "multiplayer.disconnect.unverified_username",
        Err(session::Unavailable) => "multiplayer.disconnect.authservers_down",
    };
    disconnect(connection, &translatable(refusal, &[]));
    Ok(None)
}

/// Asking the session service whether a player joined.
mod session {
    use super::Joined;
    use horizon_protocol::packets::login;
    use std::{sync::Mutex, time::Duration};

    /// The session service did not answer.
    #[derive(Debug)]
    pub(super) struct Unavailable;

    /// Whether the player who says they are `name` told the session service they
    /// were joining the server whose hash is `server_hash`, and who they are.
    pub(super) trait Sessions {
        fn has_joined(&self, name: &str, server_hash: &str) -> Result<Option<Joined>, Unavailable>;
    }

    /// Where the services list their endpoints. Vanilla's session service finds
    /// `hasJoined` through it, and so does Horizon's server.
    const DISCOVERY: &str = "https://discovery.minecraftservices.com/minecraft/client";

    /// The session service on the internet.
    pub(super) struct SessionService;

    impl Sessions for SessionService {
        fn has_joined(&self, name: &str, server_hash: &str) -> Result<Option<Joined>, Unavailable> {
            let url = verify_url()?;
            let separator = if url.contains('?') { '&' } else { '?' };
            let url = format!(
                "{url}{separator}username={}&serverId={}",
                url_encode(name),
                url_encode(server_hash)
            );
            match http_client().get(&url).call() {
                // No content: the player did not join.
                Ok(response) => Ok(response
                    .into_string()
                    .ok()
                    .and_then(|body| parse_joined(&body, name))),
                Err(ureq::Error::Status(status, _)) if status >= 500 => Err(Unavailable),
                Err(ureq::Error::Status(..)) => Ok(None),
                Err(ureq::Error::Transport(_)) => Err(Unavailable),
            }
        }
    }

    fn http_client() -> ureq::Agent {
        ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(15))
            .user_agent(concat!("Meridian/", env!("CARGO_PKG_VERSION")))
            .build()
    }

    /// The `hasJoined` endpoint, read from the discovery document once; a failed
    /// read is tried again at the next login.
    fn verify_url() -> Result<String, Unavailable> {
        static URL: Mutex<Option<String>> = Mutex::new(None);
        if let Some(url) = URL.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            return Ok(url);
        }
        let document: serde_json::Value = http_client()
            .get(DISCOVERY)
            .call()
            .ok()
            .and_then(|response| response.into_string().ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .ok_or(Unavailable)?;
        let url = verify_endpoint(&document).ok_or(Unavailable)?;
        *URL.lock().unwrap_or_else(|e| e.into_inner()) = Some(url.clone());
        Ok(url)
    }

    pub(super) fn verify_endpoint(document: &serde_json::Value) -> Option<String> {
        document
            .pointer("/discovery/session/endpoints/verify/uri")?
            .as_str()
            .map(str::to_owned)
    }

    /// `HasJoinedMinecraftServerResponse`: the id, the name and the properties.
    /// No id is no profile. The name is the account's, in its own case; `asked`
    /// stands in when the answer has none.
    pub(super) fn parse_joined(body: &str, asked: &str) -> Option<Joined> {
        let value: serde_json::Value = serde_json::from_str(body).ok()?;
        let uuid = parse_uuid(value.get("id")?.as_str()?)?;
        let name = value
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or(asked)
            .to_owned();
        let properties = value
            .get("properties")
            .and_then(|p| p.as_array())
            .into_iter()
            .flatten()
            .filter_map(|p| {
                Some(login::Property {
                    name: p.get("name")?.as_str()?.to_owned(),
                    value: p.get("value")?.as_str()?.to_owned(),
                    signature: p
                        .get("signature")
                        .and_then(|s| s.as_str())
                        .map(str::to_owned),
                })
            })
            .collect();
        Some(Joined {
            uuid,
            name,
            properties,
        })
    }

    /// A UUID as 32 hex digits, with or without dashes.
    pub(super) fn parse_uuid(text: &str) -> Option<u128> {
        let hex: String = text.chars().filter(|&c| c != '-').collect();
        if hex.len() != 32 {
            return None;
        }
        u128::from_str_radix(&hex, 16).ok()
    }

    /// `URLEncoder.encode(value, UTF-8)`.
    pub(super) fn url_encode(text: &str) -> String {
        let mut out = String::new();
        for b in text.bytes() {
            match b {
                b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'.' | b'-' | b'*' | b'_' => {
                    out.push(char::from(b));
                }
                b' ' => out.push('+'),
                _ => out.push_str(&format!("%{b:02X}")),
            }
        }
        out
    }
}

/// Compression, the finished login, and the client's acknowledgement.
fn finish(
    mut connection: Connection,
    config: &Config,
    joined: Joined,
    auth: AuthKind,
) -> Result<ClientSession> {
    // Unlike vanilla, a connection over memory is compressed too: the proxy only
    // has those in tests, which should go through what players get.
    let threshold = config.compression_threshold;
    if threshold >= 0 {
        let compression = login::Clientbound::Compression(login::Compression { threshold });
        connection.send(&compression.encode())?;
        connection.set_compression(threshold);
    }
    let finished = login::Clientbound::LoginFinished(login::LoginFinished {
        profile: login::GameProfile {
            uuid: joined.uuid,
            name: joined.name.clone(),
            properties: joined.properties.clone(),
        },
        session: session_id()?,
    });
    connection.send(&finished.encode())?;
    // The client may answer a query or a cookie request before it acknowledges.
    loop {
        let (id, payload) = connection.recv()?;
        match login::Serverbound::decode(id, &payload)? {
            login::Serverbound::LoginAcknowledged(_) => break,
            login::Serverbound::CustomQueryAnswer(_) | login::Serverbound::CookieResponse(_) => {}
            other => {
                return Err(protocol_error(format!(
                    "unexpected packet {} before the login was acknowledged",
                    other.id()
                )));
            }
        }
    }
    connection.reader.set_read_timeout(Some(READ_TIMEOUT))?;
    let properties = joined
        .properties
        .into_iter()
        .map(|p| (p.name, p.value, p.signature.unwrap_or_default()))
        .collect();
    Ok(ClientSession {
        profile: Profile {
            name: joined.name,
            uuid: joined.uuid.to_be_bytes(),
            properties,
        },
        auth,
        connection,
    })
}

/// `ServerConnectionListener.getSessionId`: one random id for the life of the
/// proxy, sent with every finished login.
fn session_id() -> Result<u128> {
    static ID: OnceLock<u128> = OnceLock::new();
    if let Some(id) = ID.get() {
        return Ok(*id);
    }
    let mut bytes = [0u8; 16];
    crypto::random_bytes(&mut bytes)?;
    Ok(*ID.get_or_init(|| u128::from_be_bytes(bytes)))
}

/// `StringUtil.isValidPlayerName`.
fn valid_name(name: &str) -> bool {
    (1..=16).contains(&name.len()) && name.chars().all(|c| (33..127).contains(&(c as u32)))
}

/// A translated text component, as JSON.
fn translatable(key: &str, with: &[&str]) -> String {
    if with.is_empty() {
        serde_json::json!({ "translate": key })
    } else {
        serde_json::json!({ "translate": key, "with": with })
    }
    .to_string()
}

/// Tells the client why its login ends. The connection is closed right after,
/// so a failure to send changes nothing.
fn disconnect(connection: &mut Connection, reason_json: &str) {
    let packet = login::Clientbound::Disconnect(login::Disconnect {
        reason_json: reason_json.to_owned(),
    });
    let _ = connection.send(&packet.encode());
}

/// `ServerStatusPacketListenerImpl`: one status answer, then the pong, and the
/// connection ends. A second status request ends it too, and so does a client
/// that closes without pinging.
fn serve_status(connection: &mut Connection, config: &Config) -> Result<()> {
    let mut answered = false;
    loop {
        let (id, payload) = match connection.recv() {
            Ok(packet) => packet,
            Err(e) if e.is_end_of_stream() => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        match status::Serverbound::decode(id, &payload)? {
            status::Serverbound::StatusRequest(_) if answered => return Ok(()),
            status::Serverbound::StatusRequest(_) => {
                answered = true;
                let json = status_json(config, players_online());
                let response = status::Clientbound::StatusResponse(status::StatusResponse { json });
                connection.send(&response.encode())?;
            }
            status::Serverbound::PingRequest(ping) => {
                connection.send(&status::Clientbound::PongResponse(ping).encode())?;
                return Ok(());
            }
            status::Serverbound::Unknown(..) => return Ok(()),
        }
    }
}

/// The status the server list shows. The proxy answers it alone, without asking
/// a backend: a status request is the cheapest and most frequent connection there
/// is, and it must not open a second one. So the version is the one the proxy
/// speaks, the players are those the proxy relays, and the description is the
/// proxy's own.
fn status_json(config: &Config, online: usize) -> String {
    serde_json::json!({
        "version": {
            "name": horizon_protocol::GAME_VERSION,
            "protocol": horizon_protocol::PROTOCOL_VERSION,
        },
        "players": { "max": MAX_PLAYERS_SHOWN, "online": online },
        "description": config.motd,
        "enforcesSecureChat": false,
    })
    .to_string()
}

fn protocol_error(message: String) -> Error {
    Error::Protocol(horizon_protocol::Error::new(message))
}

#[cfg(test)]
mod tests {
    use super::*;
    use horizon_protocol::connection::Pipe;
    use std::thread::JoinHandle;

    /// The proxy's side of a pipe, running `accept` on its own thread.
    fn proxy(config: Config) -> (Connection, JoinHandle<Result<Option<ClientSession>>>) {
        let (client, server) = Pipe::pair();
        let accepted = std::thread::spawn(move || accept(Connection::local(server), &config));
        (Connection::local(client), accepted)
    }

    fn handshake(client: &mut Connection, protocol: i32, intent: i32) {
        let intention = handshake::Serverbound::Intention(handshake::Intention {
            protocol,
            host: "localhost".into(),
            port: 25565,
            intent,
        });
        client.send(&intention.encode()).unwrap();
    }

    fn status_reply(client: &mut Connection) -> status::Clientbound {
        let (id, payload) = client.recv().unwrap();
        status::Clientbound::decode(id, &payload).unwrap()
    }

    #[test]
    fn the_status_shows_the_proxy_and_the_ping_is_answered() {
        let config = Config {
            motd: "Meridian test".into(),
            ..Config::default()
        };
        let (mut client, accepted) = proxy(config);
        handshake(
            &mut client,
            horizon_protocol::PROTOCOL_VERSION,
            handshake::STATUS,
        );
        client
            .send(&status::Serverbound::StatusRequest(Default::default()).encode())
            .unwrap();
        let status::Clientbound::StatusResponse(response) = status_reply(&mut client) else {
            panic!("expected a status response");
        };
        let json: serde_json::Value = serde_json::from_str(&response.json).unwrap();
        assert_eq!(json["description"], "Meridian test");
        assert_eq!(
            json["version"]["protocol"],
            horizon_protocol::PROTOCOL_VERSION
        );
        assert_eq!(json["version"]["name"], horizon_protocol::GAME_VERSION);
        assert_eq!(json["players"]["max"], MAX_PLAYERS_SHOWN);

        let ping = status::Ping { time: 1234 };
        client
            .send(&status::Serverbound::PingRequest(ping.clone()).encode())
            .unwrap();
        assert_eq!(
            status_reply(&mut client),
            status::Clientbound::PongResponse(ping)
        );
        assert!(accepted.join().unwrap().unwrap().is_none());
    }

    #[test]
    fn a_status_answered_without_a_ping_ends_when_the_client_leaves() {
        let (mut client, accepted) = proxy(Config::default());
        // An old client's version: the status is still answered.
        handshake(&mut client, 47, handshake::STATUS);
        client
            .send(&status::Serverbound::StatusRequest(Default::default()).encode())
            .unwrap();
        assert!(matches!(
            status_reply(&mut client),
            status::Clientbound::StatusResponse(_)
        ));
        drop(client);
        assert!(accepted.join().unwrap().unwrap().is_none());
    }

    #[test]
    fn a_second_status_request_ends_the_connection() {
        let (mut client, accepted) = proxy(Config::default());
        handshake(
            &mut client,
            horizon_protocol::PROTOCOL_VERSION,
            handshake::STATUS,
        );
        let request = status::Serverbound::StatusRequest(Default::default()).encode();
        client.send(&request).unwrap();
        status_reply(&mut client);
        client.send(&request).unwrap();
        assert!(accepted.join().unwrap().unwrap().is_none());
    }

    #[test]
    fn the_status_counts_the_players_online() {
        let json: serde_json::Value =
            serde_json::from_str(&status_json(&Config::default(), 3)).unwrap();
        assert_eq!(json["players"]["online"], 3);
        assert_eq!(json["enforcesSecureChat"], false);
    }

    #[test]
    fn a_seat_counts_while_it_lives() {
        let before = players_online();
        let seat = Seat::take();
        assert_eq!(players_online(), before + 1);
        drop(seat);
        assert_eq!(players_online(), before);
    }

    #[test]
    fn a_connection_that_does_not_start_with_a_handshake_is_refused() {
        let (mut client, accepted) = proxy(Config::default());
        client.send(&[0x7f, 1, 2, 3]).unwrap();
        assert!(matches!(accepted.join().unwrap(), Err(Error::Protocol(_))));
    }

    fn offline(compression_threshold: i32) -> Config {
        Config {
            online_mode: false,
            compression_threshold,
            ..Config::default()
        }
    }

    fn start_login(client: &mut Connection, name: &str) {
        handshake(client, horizon_protocol::PROTOCOL_VERSION, handshake::LOGIN);
        let hello = login::Serverbound::Hello(login::ServerHello {
            name: name.into(),
            uuid: 42,
        });
        client.send(&hello.encode()).unwrap();
    }

    fn login_reply(client: &mut Connection) -> login::Clientbound {
        let (id, payload) = client.recv().unwrap();
        login::Clientbound::decode(id, &payload).unwrap()
    }

    fn acknowledge(client: &mut Connection) {
        let acknowledged = login::Serverbound::LoginAcknowledged(Default::default());
        client.send(&acknowledged.encode()).unwrap();
    }

    /// The reason of a login disconnect, as JSON.
    fn reason(packet: login::Clientbound) -> serde_json::Value {
        let login::Clientbound::Disconnect(disconnect) = packet else {
            panic!("expected a disconnect, got {packet:?}");
        };
        serde_json::from_str(&disconnect.reason_json).unwrap()
    }

    #[test]
    fn an_offline_login_derives_the_uuid_and_compresses() {
        let (mut client, accepted) = proxy(offline(64));
        start_login(&mut client, "Steve");
        assert_eq!(
            login_reply(&mut client),
            login::Clientbound::Compression(login::Compression { threshold: 64 })
        );
        client.set_compression(64);
        let login::Clientbound::LoginFinished(finished) = login_reply(&mut client) else {
            panic!("expected the finished login");
        };
        let uuid = crypto::offline_uuid("Steve");
        assert_eq!(finished.profile.name, "Steve");
        assert_eq!(finished.profile.uuid, uuid);
        assert!(finished.profile.properties.is_empty());
        assert_eq!(finished.session, session_id().unwrap());
        acknowledge(&mut client);

        let session = accepted.join().unwrap().unwrap().expect("a session");
        assert_eq!(session.auth, AuthKind::Offline);
        assert_eq!(
            session.profile,
            Profile {
                name: "Steve".into(),
                uuid: uuid.to_be_bytes(),
                properties: Vec::new(),
            }
        );
        // Both ends compress now: a packet past the threshold crosses whole.
        let mut proxy_side = session.connection;
        let big: Vec<u8> = std::iter::once(5)
            .chain(std::iter::repeat_n(7, 1000))
            .collect();
        client.send(&big).unwrap();
        assert_eq!(proxy_side.recv().unwrap(), (5, vec![7; 1000]));
    }

    #[test]
    fn without_compression_the_login_finishes_at_once() {
        let (mut client, accepted) = proxy(offline(-1));
        start_login(&mut client, "Alex");
        assert!(matches!(
            login_reply(&mut client),
            login::Clientbound::LoginFinished(_)
        ));
        acknowledge(&mut client);
        let session = accepted.join().unwrap().unwrap().expect("a session");
        assert_eq!(session.profile.name, "Alex");
    }

    #[test]
    fn the_login_waits_for_the_acknowledgement_past_a_query_answer() {
        let (mut client, accepted) = proxy(offline(-1));
        start_login(&mut client, "Alex");
        login_reply(&mut client);
        let answer = login::Serverbound::CustomQueryAnswer(login::CustomQueryAnswer {
            transaction: 1,
            payload: None,
        });
        client.send(&answer.encode()).unwrap();
        acknowledge(&mut client);
        assert!(accepted.join().unwrap().unwrap().is_some());
    }

    #[test]
    fn another_packet_before_the_acknowledgement_is_an_error() {
        let (mut client, accepted) = proxy(offline(-1));
        start_login(&mut client, "Alex");
        login_reply(&mut client);
        let hello = login::Serverbound::Hello(login::ServerHello {
            name: "Alex".into(),
            uuid: 0,
        });
        client.send(&hello.encode()).unwrap();
        assert!(matches!(accepted.join().unwrap(), Err(Error::Protocol(_))));
    }

    #[test]
    fn a_client_of_another_version_is_told_which_one_the_proxy_speaks() {
        for (protocol, key) in [
            (
                horizon_protocol::PROTOCOL_VERSION + 1,
                "multiplayer.disconnect.incompatible",
            ),
            (47, "multiplayer.disconnect.outdated_client"),
        ] {
            let (mut client, accepted) = proxy(offline(-1));
            handshake(&mut client, protocol, handshake::LOGIN);
            let json = reason(login_reply(&mut client));
            assert_eq!(json["translate"], key);
            assert_eq!(json["with"][0], horizon_protocol::GAME_VERSION);
            assert!(accepted.join().unwrap().unwrap().is_none());
        }
    }

    #[test]
    fn a_name_with_invalid_characters_is_refused() {
        let (mut client, accepted) = proxy(offline(-1));
        start_login(&mut client, "no spaces");
        let json = reason(login_reply(&mut client));
        assert_eq!(json["translate"], "disconnect.genericReason");
        assert!(accepted.join().unwrap().unwrap().is_none());
    }

    #[test]
    fn a_transfer_is_refused() {
        let (mut client, accepted) = proxy(offline(-1));
        handshake(
            &mut client,
            horizon_protocol::PROTOCOL_VERSION,
            handshake::TRANSFER,
        );
        let json = reason(login_reply(&mut client));
        assert_eq!(
            json["translate"],
            "multiplayer.disconnect.transfers_disabled"
        );
        assert!(accepted.join().unwrap().unwrap().is_none());
    }

    #[test]
    fn the_configuration_decides_how_every_login_is_checked() {
        for name in ["Steve", "Notch"] {
            assert_eq!(auth_for(&offline(-1), name), AuthKind::Offline);
            assert_eq!(auth_for(&Config::default(), name), AuthKind::Premium);
        }
    }

    #[test]
    fn player_names_follow_vanilla() {
        assert!(valid_name("Steve"));
        assert!(valid_name("a_b-c.1!"));
        assert!(valid_name(&"x".repeat(16)));
        assert!(!valid_name(""));
        assert!(!valid_name(&"x".repeat(17)));
        assert!(!valid_name("no spaces"));
        assert!(!valid_name("é"));
    }

    /// What the fake session service says.
    #[derive(Clone)]
    enum Answer {
        Joined,
        NotJoined,
        Down,
    }

    const ACCOUNT_UUID: u128 = 0x069a79f444e94726a5befca90e38aaf5;

    /// A session service that answers as told and records what it was asked.
    struct FakeSessions {
        answer: Answer,
        asked: std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>,
    }

    impl session::Sessions for FakeSessions {
        fn has_joined(
            &self,
            name: &str,
            server_hash: &str,
        ) -> std::result::Result<Option<Joined>, session::Unavailable> {
            self.asked
                .lock()
                .unwrap()
                .push((name.to_owned(), server_hash.to_owned()));
            match self.answer {
                Answer::Joined => Ok(Some(Joined {
                    uuid: ACCOUNT_UUID,
                    // The account's own case, not the one the client typed.
                    name: "Notch".into(),
                    properties: vec![login::Property {
                        name: "textures".into(),
                        value: "e30=".into(),
                        signature: Some("c2lnbmVk".into()),
                    }],
                })),
                Answer::NotJoined => Ok(None),
                Answer::Down => Err(session::Unavailable),
            }
        }
    }

    type Asked = std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>;

    /// An online proxy whose session service answers `answer`.
    fn online_proxy(
        answer: Answer,
    ) -> (Connection, JoinHandle<Result<Option<ClientSession>>>, Asked) {
        let asked = Asked::default();
        let sessions = FakeSessions {
            answer,
            asked: asked.clone(),
        };
        let config = Config {
            online_mode: true,
            compression_threshold: 256,
            ..Config::default()
        };
        let (client, server) = Pipe::pair();
        let accepted =
            std::thread::spawn(move || accept_with(Connection::local(server), &config, &sessions));
        (Connection::local(client), accepted, asked)
    }

    /// The client's half of the encryption, as vanilla's
    /// `ClientHandshakePacketListenerImpl.handleHello` does it: the server hash
    /// it would send to the session service.
    fn answer_encryption(client: &mut Connection) -> String {
        let login::Clientbound::Hello(hello) = login_reply(client) else {
            panic!("expected the encryption request");
        };
        assert!(hello.should_authenticate);
        assert_eq!(hello.server_id, "");
        let public = horizon_protocol::rsa::RsaPublicKey::from_der(&hello.public_key).unwrap();
        let secret = [9u8; 16];
        let key = login::Serverbound::Key(login::Key {
            key: public.encrypt(&secret).unwrap(),
            challenge: public.encrypt(&hello.challenge).unwrap(),
        });
        client.send(&key.encode()).unwrap();
        client.enable_encryption(&secret);
        crypto::server_hash("", &secret, &hello.public_key)
    }

    #[test]
    fn an_online_login_is_encrypted_and_takes_the_account_profile() {
        let (mut client, accepted, asked) = online_proxy(Answer::Joined);
        start_login(&mut client, "notch");
        let server_hash = answer_encryption(&mut client);
        assert_eq!(
            login_reply(&mut client),
            login::Clientbound::Compression(login::Compression { threshold: 256 })
        );
        client.set_compression(256);
        let login::Clientbound::LoginFinished(finished) = login_reply(&mut client) else {
            panic!("expected the finished login");
        };
        assert_eq!(finished.profile.uuid, ACCOUNT_UUID);
        assert_eq!(finished.profile.name, "Notch");
        assert_eq!(finished.profile.properties.len(), 1);
        acknowledge(&mut client);

        let session = accepted.join().unwrap().unwrap().expect("a session");
        assert_eq!(session.auth, AuthKind::Premium);
        assert_eq!(
            asked.lock().unwrap().as_slice(),
            [("notch".into(), server_hash)]
        );
        assert_eq!(
            session.profile,
            Profile {
                name: "Notch".into(),
                uuid: ACCOUNT_UUID.to_be_bytes(),
                properties: vec![("textures".into(), "e30=".into(), "c2lnbmVk".into())],
            }
        );
        // The session's connection is encrypted and compressed both ways.
        let mut proxy_side = session.connection;
        proxy_side.send(&[3, 1, 2]).unwrap();
        assert_eq!(client.recv().unwrap(), (3, vec![1, 2]));
        client.send(&[4, 5]).unwrap();
        assert_eq!(proxy_side.recv().unwrap(), (4, vec![5]));
    }

    #[test]
    fn a_player_the_session_service_does_not_know_is_refused() {
        for (answer, key) in [
            (
                Answer::NotJoined,
                "multiplayer.disconnect.unverified_username",
            ),
            (Answer::Down, "multiplayer.disconnect.authservers_down"),
        ] {
            let (mut client, accepted, _) = online_proxy(answer);
            start_login(&mut client, "Steve");
            answer_encryption(&mut client);
            // The refusal is encrypted, as everything after the answer.
            assert_eq!(reason(login_reply(&mut client))["translate"], key);
            assert!(accepted.join().unwrap().unwrap().is_none());
        }
    }

    #[test]
    fn a_wrong_challenge_is_an_error() {
        let (mut client, accepted, asked) = online_proxy(Answer::Joined);
        start_login(&mut client, "Steve");
        let login::Clientbound::Hello(hello) = login_reply(&mut client) else {
            panic!("expected the encryption request");
        };
        let public = horizon_protocol::rsa::RsaPublicKey::from_der(&hello.public_key).unwrap();
        let key = login::Serverbound::Key(login::Key {
            key: public.encrypt(&[9u8; 16]).unwrap(),
            challenge: public.encrypt(b"nope").unwrap(),
        });
        client.send(&key.encode()).unwrap();
        assert!(matches!(accepted.join().unwrap(), Err(Error::Protocol(_))));
        assert!(asked.lock().unwrap().is_empty());
    }

    #[test]
    fn the_session_service_answer_is_read_as_vanilla_reads_it() {
        let body = r#"{"id":"069a79f444e94726a5befca90e38aaf5","name":"Notch",
            "properties":[{"name":"textures","value":"e30=","signature":"c2ln"},
                          {"name":"plain","value":"v"}]}"#;
        let joined = session::parse_joined(body, "notch").unwrap();
        assert_eq!(joined.uuid, ACCOUNT_UUID);
        assert_eq!(joined.name, "Notch");
        assert_eq!(joined.properties.len(), 2);
        assert_eq!(joined.properties[0].signature.as_deref(), Some("c2ln"));
        assert_eq!(joined.properties[1].signature, None);
        // No id, or no body at all: the player did not join.
        assert!(session::parse_joined(r#"{"name":"Notch"}"#, "notch").is_none());
        assert!(session::parse_joined("", "notch").is_none());
        // A name missing from the answer falls back to the one asked.
        let bare = session::parse_joined(r#"{"id":"069a79f4-44e9-4726-a5be-fca90e38aaf5"}"#, "x");
        assert_eq!(bare.map(|j| j.name).as_deref(), Some("x"));
    }

    /// The real service, through the discovery document and TLS: nobody joined
    /// with this hash, so it answers no rather than failing.
    #[test]
    #[ignore = "asks the session service on the internet"]
    fn the_real_session_service_answers() {
        use session::Sessions;
        let answer = session::SessionService.has_joined("MeridianProbe", "-1a2b3c");
        assert!(matches!(answer, Ok(None)), "{answer:?}");
    }

    #[test]
    fn uuids_and_queries_are_written_as_the_services_expect() {
        assert_eq!(
            session::parse_uuid("069a79f4-44e9-4726-a5be-fca90e38aaf5"),
            Some(ACCOUNT_UUID)
        );
        assert_eq!(session::parse_uuid("069a79f4"), None);
        assert_eq!(session::url_encode("-1a2b_c d.*"), "-1a2b_c+d.*");
        assert_eq!(session::url_encode("a&b=é"), "a%26b%3D%C3%A9");
        let document = serde_json::json!({
            "discovery": {"session": {"endpoints": {"verify": {"uri": "https://s/hasJoined"}}}}
        });
        assert_eq!(
            session::verify_endpoint(&document).as_deref(),
            Some("https://s/hasJoined")
        );
        assert_eq!(session::verify_endpoint(&serde_json::json!({})), None);
    }
}
