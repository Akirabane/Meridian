//! The player's side: the proxy answers the handshake, the status request and the
//! login itself, and the rest of the session goes through the connection it
//! leaves behind.
//!
//! The steps follow vanilla's server (`ServerHandshakePacketListenerImpl`,
//! `ServerStatusPacketListenerImpl`, `ServerLoginPacketListenerImpl`), as Horizon's
//! server does in `horizon-client/src/lan/conn.rs`.
use crate::login_policy::{LoginPolicy, NameLookup, ProfileLookup, Refusal};
use crate::{Error, Profile, Result};
use horizon_protocol::{
    buf::Reader,
    connection::Connection,
    crypto, frame,
    packets::{handshake, login, status},
    rsa::RsaPrivateKey,
};
use meridian_config::Config;
use std::{
    collections::BTreeMap,
    net::IpAddr,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

/// How long the handshake, a status request and the login may wait for the
/// client's next packet. Vanilla waits 30 s, but a client answers these at once,
/// and a silent socket should not hold a thread for that long.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(10);

/// The bounds of what a connection may take before it is a session: whatever
/// sends the bytes, the handshake, the status and the login end within them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    /// The longest wait for the client's next byte.
    pub silence: Duration,
    /// The longest a handshake and a login may take in all, as vanilla's
    /// `ServerLoginPacketListenerImpl.tick` gives a login 600 ticks.
    pub login: Duration,
    /// The longest a handshake, a status request and its ping may take in all.
    pub status: Duration,
    /// The longest frame read before the login is done, in bytes.
    pub max_frame: usize,
}

/// The bounds of every connection the proxy accepts.
pub const BOUNDS: Bounds = Bounds {
    silence: LOGIN_TIMEOUT,
    login: Duration::from_secs(30),
    status: Duration::from_secs(5),
    max_frame: 2048,
};

/// The read timeout left on the connection for the rest of the session:
/// vanilla's 30 s.
pub const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// `ServerHandshakePacketListenerImpl`: a client older than this protocol is told
/// it is outdated, any other mismatch that it is incompatible.
const OUTDATED_BELOW: i32 = 754;

/// A player who has logged in to the proxy and is ready to be relayed.
pub struct ClientSession {
    pub profile: Profile,
    /// How the player proved who they are, which decides the servers they may
    /// be sent to.
    pub auth: AuthKind,
    /// The host name the client wrote in its handshake, in lower case and
    /// without anything a mod loader appends, or `None` when it was empty. A
    /// forced host is looked up with it.
    pub virtual_host: Option<String>,
    pub connection: Connection,
}

/// The host name of a handshake as the proxy compares it: up to the first NUL
/// (Forge and others append data after one), without a final dot, in lower case.
/// The port travels in its own field, so the host has none to remove.
pub fn virtual_host(raw: &str) -> Option<String> {
    let host = raw.split('\0').next().unwrap_or_default().trim();
    let host = host.strip_suffix('.').unwrap_or(host);
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// What became of the players other servers sent here with a vanilla transfer (the handshake's
/// TRANSFER intention), for the metrics page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Transfers {
    /// Refused, as `accept-transfers` is off.
    pub refused: u64,
    /// Taken, and logged in as any player.
    pub logged_in: u64,
    /// Taken, but their login did not finish (a check refused it, or it failed).
    pub failed: u64,
}

static TRANSFERS_REFUSED: AtomicU64 = AtomicU64::new(0);
static TRANSFERS_LOGGED_IN: AtomicU64 = AtomicU64::new(0);
static TRANSFERS_FAILED: AtomicU64 = AtomicU64::new(0);

/// The count a login ends in: none for a player who came on their own; for a transferred one,
/// logged in or failed.
fn transfer_count(transferred: bool, joined: bool) -> Option<&'static AtomicU64> {
    if !transferred {
        return None;
    }
    Some(if joined {
        &TRANSFERS_LOGGED_IN
    } else {
        &TRANSFERS_FAILED
    })
}

/// The transfers counted since the proxy started.
pub fn transfers() -> Transfers {
    Transfers {
        refused: TRANSFERS_REFUSED.load(Ordering::Relaxed),
        logged_in: TRANSFERS_LOGGED_IN.load(Ordering::Relaxed),
        failed: TRANSFERS_FAILED.load(Ordering::Relaxed),
    }
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

/// How the proxy checks the login of a player called `name`, decided from the
/// name alone before anything is asked of the client ([`login_policy`]): the
/// online mode verifies everyone, the offline mode nobody, and the hybrid mode
/// looks the name up.
fn auth_for(
    config: &Config,
    name: &str,
    checks: &Checks<'_>,
) -> std::result::Result<AuthKind, Refusal> {
    checks
        .policy
        .auth_for(config, name, checks.lookup, checks.peer)
}

/// What a login asks besides the connection: the session service, the name
/// lookup of the hybrid mode and its memory, and where the player connects from.
struct Checks<'a> {
    sessions: &'a dyn session::Sessions,
    lookup: &'a dyn NameLookup,
    policy: &'a LoginPolicy,
    peer: Option<IpAddr>,
}

impl Checks<'static> {
    /// The real services and the proxy's policy.
    fn real(peer: Option<IpAddr>) -> Self {
        Checks {
            sessions: &session::SessionService,
            lookup: &ProfileLookup,
            policy: LoginPolicy::global(),
            peer,
        }
    }
}

/// The players the server list counts, one per live [`Seat`], in the order they
/// took it: the name and unique id the list may show.
static SEATS: Mutex<BTreeMap<u64, (String, [u8; 16])>> = Mutex::new(BTreeMap::new());
/// The number of the next seat, which orders them.
static NEXT_SEAT: AtomicU64 = AtomicU64::new(0);

/// A player counted as online in the server list for as long as it lives. The
/// relay takes one for each session it runs.
#[derive(Debug)]
pub struct Seat(u64);

impl Seat {
    /// Counts the player called `name`, with the unique id `uuid`, as online.
    pub fn take(name: &str, uuid: [u8; 16]) -> Self {
        let id = NEXT_SEAT.fetch_add(1, Ordering::Relaxed);
        seats().insert(id, (name.to_owned(), uuid));
        Seat(id)
    }
}

impl Drop for Seat {
    fn drop(&mut self) {
        seats().remove(&self.0);
    }
}

fn seats() -> std::sync::MutexGuard<'static, BTreeMap<u64, (String, [u8; 16])>> {
    SEATS.lock().unwrap_or_else(|e| e.into_inner())
}

/// How many players the server list shows as online.
pub fn players_online() -> usize {
    seats().len()
}

/// Up to `count` of the players online, the longest there first: the sample the
/// server list shows.
pub fn players_sample(count: usize) -> Vec<(String, [u8; 16])> {
    seats().values().take(count).cloned().collect()
}

/// Takes a new connection through handshake and login. A status request is
/// answered and ends the connection, so it returns `Ok(None)`; so does a login
/// the proxy refuses, after telling the client why.
///
/// A session is returned once the client has acknowledged the login: the
/// connection is then in the configuration phase, as a backend link is, and
/// carries the compression and the encryption the login set up.
///
/// The player's address is not known here, so in the hybrid mode every player
/// accepted this way shares one limit of name lookups; [`accept_from`] gives each
/// address its own.
pub fn accept(connection: Connection, config: &Config) -> Result<Option<ClientSession>> {
    accept_with(connection, config, &Checks::real(None))
}

/// [`accept`] for a player connecting from `peer`, which the hybrid mode's limit
/// of name lookups counts by.
pub fn accept_from(
    connection: Connection,
    config: &Config,
    peer: IpAddr,
) -> Result<Option<ClientSession>> {
    accept_with(connection, config, &Checks::real(Some(peer)))
}

/// [`accept`], with `checks` to ask who a player is.
fn accept_with(
    connection: Connection,
    config: &Config,
    checks: &Checks<'_>,
) -> Result<Option<ClientSession>> {
    accept_bounded(connection, config, checks, &BOUNDS)
}

/// [`accept_with`], within `bounds`.
fn accept_bounded(
    mut connection: Connection,
    config: &Config,
    checks: &Checks<'_>,
    bounds: &Bounds,
) -> Result<Option<ClientSession>> {
    let start = Instant::now();
    // The handshake comes first and at once: it has the status's time, the shorter.
    let mut early = Early::new(bounds, start + bounds.status);
    let (id, payload) = early.recv(&mut connection)?.ok_or_else(ended)?;
    let intention = match handshake::Serverbound::decode(id, &payload)? {
        handshake::Serverbound::Intention(intention) => intention,
        handshake::Serverbound::Unknown(id, _) => {
            return Err(protocol_error(format!(
                "expected a handshake, got packet {id}"
            )));
        }
    };
    // A player another server sent here (`/transfer`). With `accept-transfers` they log in
    // exactly as a player who came on their own, by the same code: the same checks, the host they
    // were sent to choosing the server. A transferred client sends nothing more; it gives the
    // cookies the other server left only when asked, and the proxy asks for none (`finish`). Off,
    // they are refused, as a vanilla server does by default (`accepts-transfers`).
    let transferred = intention.intent == handshake::TRANSFER;
    let intent = if transferred && config.accept_transfers {
        handshake::LOGIN
    } else {
        intention.intent
    };
    match intent {
        handshake::STATUS => {
            serve_status(
                &mut connection,
                &mut early,
                config,
                virtual_host(&intention.host).as_deref(),
            )?;
            Ok(None)
        }
        handshake::LOGIN => {
            early.until = start + bounds.login;
            early.login = true;
            let joined = log_in(
                connection,
                &mut early,
                config,
                checks,
                intention.protocol,
                virtual_host(&intention.host),
            );
            if let Some(counter) = transfer_count(transferred, matches!(joined, Ok(Some(_)))) {
                counter.fetch_add(1, Ordering::Relaxed);
            }
            joined
        }
        handshake::TRANSFER => {
            TRANSFERS_REFUSED.fetch_add(1, Ordering::Relaxed);
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
    early: &mut Early,
    config: &Config,
    checks: &Checks<'_>,
    protocol: i32,
    virtual_host: Option<String>,
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
    let (id, payload) = early.recv(&mut connection)?.ok_or_else(ended)?;
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
    let auth = match auth_for(config, &hello.name, checks) {
        Ok(auth) => auth,
        Err(refusal) => {
            disconnect(&mut connection, &refusal.reason_json());
            return Ok(None);
        }
    };
    let joined = match auth {
        AuthKind::Premium => {
            match authenticate(&mut connection, early, &hello.name, checks.sessions)? {
                Some(joined) => joined,
                None => return Ok(None),
            }
        }
        // UUIDUtil.createOfflineProfile: the UUID is derived from the name, and
        // the UUID the client sent is ignored.
        AuthKind::Offline => Joined {
            uuid: crypto::offline_uuid(&hello.name),
            name: hello.name,
            properties: Vec::new(),
        },
    };
    finish(connection, early, config, joined, auth, virtual_host)
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
    early: &mut Early,
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
    let (id, payload) = early.recv(connection)?.ok_or_else(ended)?;
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
    early.encrypted = true;
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
    early: &mut Early,
    config: &Config,
    joined: Joined,
    auth: AuthKind,
    virtual_host: Option<String>,
) -> Result<Option<ClientSession>> {
    // Unlike vanilla, a connection over memory is compressed too: the proxy only
    // has those in tests, which should go through what players get.
    let threshold = config.compression_threshold;
    if threshold >= 0 {
        let compression = login::Clientbound::Compression(login::Compression { threshold });
        connection.send(&compression.encode())?;
        connection.set_compression(threshold);
        early.threshold = usize::try_from(threshold).ok();
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
    // The proxy sends no query and no cookie request, so an answer to one is
    // refused, as vanilla's handleCustomQueryPacket and handleCookieResponse do: a
    // client cannot keep the login open with packets that lead nowhere.
    let (id, payload) = early.recv(&mut connection)?.ok_or_else(ended)?;
    match login::Serverbound::decode(id, &payload)? {
        login::Serverbound::LoginAcknowledged(_) => {}
        login::Serverbound::CustomQueryAnswer(_) | login::Serverbound::CookieResponse(_) => {
            disconnect(
                &mut connection,
                &translatable("multiplayer.disconnect.unexpected_query_response", &[]),
            );
            return Ok(None);
        }
        other => {
            return Err(protocol_error(format!(
                "unexpected packet {} before the login was acknowledged",
                other.id()
            )));
        }
    }
    connection.reader.set_read_timeout(Some(READ_TIMEOUT))?;
    let properties = joined
        .properties
        .into_iter()
        .map(|p| (p.name, p.value, p.signature.unwrap_or_default()))
        .collect();
    Ok(Some(ClientSession {
        profile: Profile {
            name: joined.name,
            uuid: joined.uuid.to_be_bytes(),
            properties,
        },
        auth,
        virtual_host,
        connection,
    }))
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
fn serve_status(
    connection: &mut Connection,
    early: &mut Early,
    config: &Config,
    host: Option<&str>,
) -> Result<()> {
    let mut answered = false;
    loop {
        // A client that closes, or says nothing more, is done.
        let Some((id, payload)) = early.recv(connection)? else {
            return Ok(());
        };
        match status::Serverbound::decode(id, &payload)? {
            status::Serverbound::StatusRequest(_) if answered => return Ok(()),
            status::Serverbound::StatusRequest(_) => {
                answered = true;
                let sample = players_sample(config.status_sample as usize);
                let json = status_json(config, host, players_online(), &sample);
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
/// speaks, which is Horizon's, the players are those the proxy relays, with a
/// sample of their names, and the description is the proxy's own, or the one of
/// the forced host the client connected through (`host`, from its handshake). The
/// favicon is the proxy's, when there is one.
fn status_json(
    config: &Config,
    host: Option<&str>,
    online: usize,
    sample: &[(String, [u8; 16])],
) -> String {
    let sample: Vec<serde_json::Value> = sample
        .iter()
        .map(|(name, uuid)| serde_json::json!({ "name": name, "id": hyphenated(*uuid) }))
        .collect();
    let mut status = serde_json::json!({
        "version": {
            "name": horizon_protocol::GAME_VERSION,
            "protocol": horizon_protocol::PROTOCOL_VERSION,
        },
        "players": { "max": max_shown(config, online), "online": online, "sample": sample },
        "description": config.motd_for(host),
        "enforcesSecureChat": false,
    });
    // One picture for the whole proxy, whatever host the client typed.
    if let Some(favicon) = &config.favicon {
        status["favicon"] = favicon.data_uri.clone().into();
    }
    status.to_string()
}

/// The player limit the server list shows: `max-players`, or one more than the
/// players online when there is no limit, since the list needs a number.
fn max_shown(config: &Config, online: usize) -> usize {
    match config.max_players {
        0 => online + 1,
        max => max as usize,
    }
}

/// A unique id as the server list writes it, with hyphens.
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

fn protocol_error(message: String) -> Error {
    Error::Protocol(horizon_protocol::Error::new(message))
}

/// What a login or a status request that took too long is told, as vanilla's
/// `ServerLoginPacketListenerImpl.tick` says it after 600 ticks.
const TOO_SLOW: &str = "the client took too long to log in";

/// A connection that ended, or went silent, before a packet began.
fn ended() -> Error {
    protocol_error("the client closed the connection or stopped answering".into())
}

/// Reads the client's packets before its session, within [`Bounds`].
///
/// Before encryption the frames are read here a byte at a time, so that each read
/// waits at most until the deadline, whatever pace the bytes come at, and a frame
/// longer than [`Bounds::max_frame`] is refused from its length, before any of it is
/// kept. Once the login turned encryption on, the connection reads; the deadline is
/// still checked before each packet, and the player has been checked by the session
/// service by then.
struct Early {
    until: Instant,
    silence: Duration,
    max_frame: usize,
    /// The compression the login set up: the frames then start with a data length.
    threshold: Option<usize>,
    /// Encryption is on: the connection reads from then on.
    encrypted: bool,
    /// The connection is logging in, and is told when it takes too long.
    login: bool,
}

impl Early {
    fn new(bounds: &Bounds, until: Instant) -> Self {
        Early {
            until,
            silence: bounds.silence,
            max_frame: bounds.max_frame,
            threshold: None,
            encrypted: false,
            login: false,
        }
    }

    /// The deadline has passed: a login is told so, and the connection ends.
    fn too_slow(&self, connection: &mut Connection) -> Error {
        if self.login {
            disconnect(
                connection,
                &translatable("multiplayer.disconnect.slow_login", &[]),
            );
        }
        protocol_error(TOO_SLOW.into())
    }

    /// Sets the read timeout to what is left before the deadline, at most the
    /// silence; an error once nothing is left.
    fn arm(&self, connection: &mut Connection) -> Result<()> {
        let left = self.until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(self.too_slow(connection));
        }
        connection
            .reader
            .set_read_timeout(Some(left.min(self.silence)))?;
        Ok(())
    }

    /// One byte; `None` when none came, because the client closed or said nothing.
    fn byte(&self, connection: &mut Connection) -> Result<Option<u8>> {
        self.arm(connection)?;
        match connection.reader.read_raw(1) {
            Ok(byte) => Ok(byte.first().copied()),
            Err(_) if Instant::now() >= self.until => Err(self.too_slow(connection)),
            Err(_) => Ok(None),
        }
    }

    /// The next packet, as its id and payload; `None` when the connection ended or
    /// went silent before a frame began.
    fn recv(&mut self, connection: &mut Connection) -> Result<Option<(i32, Vec<u8>)>> {
        if self.encrypted {
            self.arm(connection)?;
            return match connection.recv() {
                Ok(packet) => Ok(Some(packet)),
                Err(_) if Instant::now() >= self.until => Err(self.too_slow(connection)),
                Err(e) if e.is_end_of_stream() || e.is_timed_out() => Ok(None),
                Err(e) => Err(e.into()),
            };
        }
        // Varint21FrameDecoder: a length of at most three bytes, never zero.
        let mut length = 0usize;
        let mut complete = false;
        for i in 0..3 {
            let Some(b) = self.byte(connection)? else {
                return if i == 0 { Ok(None) } else { Err(ended()) };
            };
            length |= usize::from(b & 0x7f) << (7 * i);
            if b & 0x80 == 0 {
                complete = true;
                break;
            }
        }
        if !complete {
            return Err(protocol_error("length wider than 21 bits".into()));
        }
        if length == 0 {
            return Err(protocol_error("Frame length cannot be zero".into()));
        }
        if length > self.max_frame {
            return Err(protocol_error(format!(
                "a frame of {length} bytes before the login is done; at most {} are read",
                self.max_frame
            )));
        }
        let mut frame = Vec::with_capacity(length);
        while frame.len() < length {
            frame.push(self.byte(connection)?.ok_or_else(ended)?);
        }
        let body = match self.threshold {
            None => frame,
            Some(threshold) => {
                // A data length larger than the frames read here is refused before
                // anything is inflated.
                let mut r = Reader::new(&frame);
                let size = r.varint()?;
                if usize::try_from(size).is_ok_and(|size| size > self.max_frame) {
                    return Err(protocol_error(format!(
                        "a packet of {size} bytes before the login is done; at most {} are read",
                        self.max_frame
                    )));
                }
                match frame::body(&frame, Some(threshold), &mut frame::Inflater::default())? {
                    frame::Body::Plain(at) => frame[at..].to_vec(),
                    frame::Body::Inflated(body) => body,
                }
            }
        };
        let mut r = Reader::new(&body);
        let id = r.varint()?;
        Ok(Some((id, body[r.position()..].to_vec())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::login_policy::FakeLookup;

    #[test]
    fn the_virtual_host_is_the_bare_host_in_lower_case() {
        assert_eq!(
            virtual_host("Play.Example.COM").as_deref(),
            Some("play.example.com")
        );
        assert_eq!(
            virtual_host("play.example.com.").as_deref(),
            Some("play.example.com")
        );
        // Forge appends a marker after a NUL; other loaders do the same.
        assert_eq!(
            virtual_host("play.example.com\0FML3\0").as_deref(),
            Some("play.example.com")
        );
        assert_eq!(virtual_host("127.0.0.1").as_deref(), Some("127.0.0.1"));
        assert_eq!(virtual_host(""), None);
        assert_eq!(virtual_host("\0FML\0"), None);
    }
    use horizon_protocol::connection::Pipe;
    use std::thread::JoinHandle;

    /// The proxy's side of a pipe, running `accept` on its own thread.
    fn proxy(config: Config) -> (Connection, JoinHandle<Result<Option<ClientSession>>>) {
        let (client, server) = Pipe::pair();
        let accepted = std::thread::spawn(move || accept(Connection::local(server), &config));
        (Connection::local(client), accepted)
    }

    fn handshake(client: &mut Connection, protocol: i32, intent: i32) {
        handshake_to(client, "localhost", protocol, intent);
    }

    fn handshake_to(client: &mut Connection, host: &str, protocol: i32, intent: i32) {
        let intention = handshake::Serverbound::Intention(handshake::Intention {
            protocol,
            host: host.into(),
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
        assert_eq!(json["players"]["max"], Config::default().max_players);

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

    /// The whole status exchange of a client that typed `host`, as the JSON it gets.
    fn status_for(config: &Config, host: &str) -> serde_json::Value {
        let (mut client, accepted) = proxy(config.clone());
        handshake_to(
            &mut client,
            host,
            horizon_protocol::PROTOCOL_VERSION,
            handshake::STATUS,
        );
        client
            .send(&status::Serverbound::StatusRequest(Default::default()).encode())
            .unwrap();
        let status::Clientbound::StatusResponse(response) = status_reply(&mut client) else {
            panic!("expected a status response");
        };
        drop(client);
        assert!(accepted.join().unwrap().unwrap().is_none());
        serde_json::from_str(&response.json).unwrap()
    }

    fn with_motds() -> Config {
        use meridian_config::ForcedHost;
        let forced = |host: &str, motd: Option<&str>| ForcedHost {
            host: host.into(),
            backend: "east".into(),
            motd: motd.map(str::to_owned),
        };
        Config {
            motd: "Global".into(),
            forced_hosts: vec![
                forced("a.example.com", Some("Motd A")),
                forced("plain.example.com", None),
            ],
            ..Config::default()
        }
    }

    #[test]
    fn a_forced_host_shows_its_own_motd_and_any_other_host_the_global_one() {
        let config = with_motds();
        assert_eq!(
            status_for(&config, "a.example.com")["description"],
            "Motd A"
        );
        // Matched as a login is: case, a final dot, what a mod loader appends.
        assert_eq!(
            status_for(&config, "A.Example.COM.")["description"],
            "Motd A"
        );
        assert_eq!(
            status_for(&config, "a.example.com\u{0}FML3\u{0}")["description"],
            "Motd A"
        );
        // A forced host with no motd, an unknown host and the address all show the global one.
        assert_eq!(
            status_for(&config, "plain.example.com")["description"],
            "Global"
        );
        assert_eq!(
            status_for(&config, "other.example.com")["description"],
            "Global"
        );
        assert_eq!(status_for(&config, "127.0.0.1")["description"], "Global");
        assert_eq!(status_for(&config, "")["description"], "Global");
    }

    #[test]
    fn the_status_carries_the_favicon_when_there_is_one_whatever_the_host() {
        let mut config = with_motds();
        assert!(
            status_for(&config, "a.example.com")
                .get("favicon")
                .is_none()
        );
        config.favicon = Some(meridian_config::Favicon {
            path: "icon.png".into(),
            data_uri: "data:image/png;base64,AAAA".into(),
        });
        for host in ["a.example.com", "other.example.com", "localhost"] {
            assert_eq!(
                status_for(&config, host)["favicon"],
                "data:image/png;base64,AAAA",
                "{host}"
            );
        }
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
            serde_json::from_str(&status_json(&Config::default(), None, 3, &[])).unwrap();
        assert_eq!(json["players"]["online"], 3);
        assert_eq!(json["enforcesSecureChat"], false);
    }

    #[test]
    fn the_status_shows_the_player_limit_of_the_configuration() {
        let limited = Config {
            max_players: 50,
            ..Config::default()
        };
        let json: serde_json::Value =
            serde_json::from_str(&status_json(&limited, None, 3, &[])).unwrap();
        assert_eq!(json["players"]["max"], 50);
        // Without a limit, the list still needs a number: one more than online.
        let unlimited = Config {
            max_players: 0,
            ..Config::default()
        };
        let json: serde_json::Value =
            serde_json::from_str(&status_json(&unlimited, None, 3, &[])).unwrap();
        assert_eq!(json["players"]["max"], 4);
    }

    #[test]
    fn the_status_shows_a_sample_of_names() {
        let sample = vec![
            ("Alex".to_owned(), [0x11; 16]),
            ("Steve".to_owned(), [0x22; 16]),
        ];
        let json: serde_json::Value =
            serde_json::from_str(&status_json(&Config::default(), None, 2, &sample)).unwrap();
        assert_eq!(json["players"]["sample"][0]["name"], "Alex");
        assert_eq!(
            json["players"]["sample"][0]["id"],
            "11111111-1111-1111-1111-111111111111"
        );
        assert_eq!(json["players"]["sample"][1]["name"], "Steve");
        let json: serde_json::Value =
            serde_json::from_str(&status_json(&Config::default(), None, 0, &[])).unwrap();
        assert_eq!(json["players"]["sample"], serde_json::json!([]));
    }

    #[test]
    fn the_status_announces_horizons_protocol() {
        let json: serde_json::Value =
            serde_json::from_str(&status_json(&Config::default(), None, 0, &[])).unwrap();
        assert_eq!(
            json["version"]["protocol"],
            horizon_protocol::PROTOCOL_VERSION
        );
        assert_eq!(json["version"]["name"], horizon_protocol::GAME_VERSION);
    }

    #[test]
    fn a_seat_counts_while_it_lives_and_names_its_player() {
        // Other tests may hold seats at the same time, so this one looks for its
        // own unique id rather than at the totals alone.
        let uuid = [0x5e; 16];
        let mine = |sample: &[(String, [u8; 16])]| sample.iter().any(|(_, id)| *id == uuid);
        let seat = Seat::take("SeatTest", uuid);
        assert!(players_online() >= 1);
        assert!(mine(&players_sample(usize::MAX)));
        drop(seat);
        assert!(!mine(&players_sample(usize::MAX)));
    }

    #[test]
    fn the_sample_is_the_longest_online_first_and_limited() {
        let first = Seat::take("Early", [0x61; 16]);
        let second = Seat::take("Late", [0x62; 16]);
        let sample = players_sample(usize::MAX);
        let early = sample.iter().position(|(_, id)| *id == [0x61; 16]).unwrap();
        let late = sample.iter().position(|(_, id)| *id == [0x62; 16]).unwrap();
        assert!(early < late);
        assert!(players_sample(1).len() <= 1);
        assert!(players_sample(0).is_empty());
        drop((first, second));
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
        // The handshake of these tests names "localhost".
        assert_eq!(session.virtual_host.as_deref(), Some("localhost"));
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
    fn an_answer_to_a_query_never_sent_ends_the_login() {
        let unasked = [
            login::Serverbound::CustomQueryAnswer(login::CustomQueryAnswer {
                transaction: 1,
                payload: None,
            }),
            login::Serverbound::CookieResponse(login::CookieResponse {
                key: "minecraft:a".into(),
                payload: Some(vec![1, 2, 3]),
            }),
        ];
        for packet in unasked {
            let (mut client, accepted) = proxy(offline(-1));
            start_login(&mut client, "Alex");
            login_reply(&mut client);
            client.send(&packet.encode()).unwrap();
            // Vanilla's handleCustomQueryPacket and handleCookieResponse.
            assert_eq!(
                reason(login_reply(&mut client))["translate"],
                "multiplayer.disconnect.unexpected_query_response"
            );
            assert!(accepted.join().unwrap().unwrap().is_none());
        }
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
        let before = transfers();
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
        // Other tests may count at the same time: the count grew by this one at least.
        assert!(transfers().refused > before.refused);
    }

    /// Off, a transfer is refused before its version is read: a client of another version is told that transfers are
    /// disabled, not that its version is wrong, and is counted `refused`, never `failed` (vanilla's order:
    /// `ServerHandshakePacketListenerImpl` checks `acceptsTransfers` first, and the version only in `beginLogin`).
    #[test]
    fn a_transfer_of_another_version_is_refused_as_a_transfer_when_they_are_off() {
        let before = transfers();
        let (mut client, accepted) = proxy(offline(-1));
        handshake(
            &mut client,
            horizon_protocol::PROTOCOL_VERSION + 1,
            handshake::TRANSFER,
        );
        let json = reason(login_reply(&mut client));
        assert_eq!(
            json["translate"],
            "multiplayer.disconnect.transfers_disabled"
        );
        assert!(accepted.join().unwrap().unwrap().is_none());
        // Other tests may count at the same time: the refusals grew by this one at least.
        assert!(transfers().refused > before.refused);
    }

    /// Which count a login ends in, apart from the other tests' logins (the counts are shared).
    #[test]
    fn only_a_transferred_player_is_counted_and_in_the_count_of_how_it_ended() {
        assert!(transfer_count(false, true).is_none());
        assert!(transfer_count(false, false).is_none());
        assert!(std::ptr::eq(
            transfer_count(true, true).unwrap(),
            &TRANSFERS_LOGGED_IN
        ));
        assert!(std::ptr::eq(
            transfer_count(true, false).unwrap(),
            &TRANSFERS_FAILED
        ));
    }

    /// The same as `start_login`, for a player another server sent to `host`.
    fn start_transfer(client: &mut Connection, host: &str, name: &str) {
        handshake_to(
            client,
            host,
            horizon_protocol::PROTOCOL_VERSION,
            handshake::TRANSFER,
        );
        let hello = login::Serverbound::Hello(login::ServerHello {
            name: name.into(),
            uuid: 42,
        });
        client.send(&hello.encode()).unwrap();
    }

    fn accepting(config: Config) -> Config {
        Config {
            accept_transfers: true,
            ..config
        }
    }

    #[test]
    fn with_accept_transfers_a_transferred_player_logs_in_as_any_other() {
        let before = transfers();
        let (mut client, accepted) = proxy(accepting(offline(64)));
        start_transfer(&mut client, "Survival.Example.", "Steve");
        // The same login as one that came on its own: the compression, then the finished
        // login. Nothing comes between: the proxy asks the client for no cookie.
        assert_eq!(
            login_reply(&mut client),
            login::Clientbound::Compression(login::Compression { threshold: 64 })
        );
        client.set_compression(64);
        let login::Clientbound::LoginFinished(finished) = login_reply(&mut client) else {
            panic!("expected the finished login");
        };
        assert_eq!(finished.profile.uuid, crypto::offline_uuid("Steve"));
        acknowledge(&mut client);
        let session = accepted.join().unwrap().unwrap().expect("a session");
        assert_eq!(session.auth, AuthKind::Offline);
        // The host it was sent to chooses the server, as for any login (case and the
        // final dot do not matter), and an unknown one is kept as it is.
        assert_eq!(session.virtual_host.as_deref(), Some("survival.example"));
        assert!(transfers().logged_in > before.logged_in);
    }

    #[test]
    fn a_transferred_player_meets_the_same_checks() {
        let before = transfers();
        let (mut client, accepted) = proxy(accepting(offline(-1)));
        // A name a login refuses is refused after a transfer too.
        start_transfer(&mut client, "lobby.example", "no spaces");
        let json = reason(login_reply(&mut client));
        assert_eq!(json["translate"], "disconnect.genericReason");
        assert!(accepted.join().unwrap().unwrap().is_none());
        assert!(transfers().failed > before.failed);
    }

    #[test]
    fn an_unasked_cookie_after_a_transfer_still_ends_the_login() {
        let (mut client, accepted) = proxy(accepting(offline(-1)));
        start_transfer(&mut client, "lobby.example", "Alex");
        assert!(matches!(
            login_reply(&mut client),
            login::Clientbound::LoginFinished(_)
        ));
        // A cookie the other server left, offered unasked: refused as vanilla refuses it.
        let cookie = login::Serverbound::CookieResponse(login::CookieResponse {
            key: "lobby:ticket".into(),
            payload: Some(b"vip".to_vec()),
        });
        client.send(&cookie.encode()).unwrap();
        let json = reason(login_reply(&mut client));
        assert_eq!(
            json["translate"],
            "multiplayer.disconnect.unexpected_query_response"
        );
        assert!(accepted.join().unwrap().unwrap().is_none());
    }

    #[test]
    fn the_configuration_decides_how_every_login_is_checked() {
        let sessions = FakeSessions {
            answer: Answer::NotJoined,
            asked: Asked::default(),
        };
        let lookup = FakeLookup::with_accounts(&["Notch"]);
        let policy = LoginPolicy::new();
        let checks = Checks {
            sessions: &sessions,
            lookup: &lookup,
            policy: &policy,
            peer: None,
        };
        for name in ["Steve", "Notch"] {
            assert_eq!(auth_for(&offline(-1), name, &checks), Ok(AuthKind::Offline));
            assert_eq!(
                auth_for(&Config::default(), name, &checks),
                Ok(AuthKind::Premium)
            );
        }
        // Only the hybrid mode looks names up.
        assert_eq!(lookup.calls(), 0);
        let hybrid = Config {
            auth_mode: meridian_config::AuthMode::Hybrid,
            ..Config::default()
        };
        assert_eq!(auth_for(&hybrid, "Notch", &checks), Ok(AuthKind::Premium));
        assert_eq!(auth_for(&hybrid, "Steve", &checks), Ok(AuthKind::Offline));
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
        let (client, accepted) = checked_proxy(config, sessions, FakeLookup::with_accounts(&[]));
        (client, accepted, asked)
    }

    /// A proxy that asks `sessions` and `lookup`, with a policy of its own.
    fn checked_proxy(
        config: Config,
        sessions: FakeSessions,
        lookup: FakeLookup,
    ) -> (Connection, JoinHandle<Result<Option<ClientSession>>>) {
        let (client, server) = Pipe::pair();
        let accepted = std::thread::spawn(move || {
            let policy = LoginPolicy::new();
            let checks = Checks {
                sessions: &sessions,
                lookup: &lookup,
                policy: &policy,
                peer: None,
            };
            accept_with(Connection::local(server), &config, &checks)
        });
        (Connection::local(client), accepted)
    }

    /// A hybrid proxy that knows `accounts`, or whose lookups fail when `None`.
    fn hybrid_proxy(
        accounts: Option<&[&str]>,
    ) -> (Connection, JoinHandle<Result<Option<ClientSession>>>) {
        let config = Config {
            auth_mode: meridian_config::AuthMode::Hybrid,
            compression_threshold: -1,
            ..Config::default()
        };
        let sessions = FakeSessions {
            answer: Answer::NotJoined,
            asked: Asked::default(),
        };
        let lookup = match accounts {
            Some(accounts) => FakeLookup::with_accounts(accounts),
            None => FakeLookup::unavailable(),
        };
        checked_proxy(config, sessions, lookup)
    }

    #[test]
    fn hybrid_mode_lets_a_name_without_an_account_in_unverified() {
        let (mut client, accepted) = hybrid_proxy(Some(&["Notch"]));
        start_login(&mut client, "Steve");
        let login::Clientbound::LoginFinished(finished) = login_reply(&mut client) else {
            panic!("expected the finished login, without encryption");
        };
        assert_eq!(finished.profile.uuid, crypto::offline_uuid("Steve"));
        acknowledge(&mut client);
        let session = accepted.join().unwrap().unwrap().expect("a session");
        assert_eq!(session.auth, AuthKind::Offline);
    }

    #[test]
    fn hybrid_mode_asks_an_account_name_to_authenticate() {
        let (mut client, accepted) = hybrid_proxy(Some(&["Notch"]));
        start_login(&mut client, "notch");
        // The encryption request: nobody gets an account's name without the account.
        assert!(matches!(
            login_reply(&mut client),
            login::Clientbound::Hello(_)
        ));
        drop(client);
        assert!(accepted.join().unwrap().is_err());
    }

    #[test]
    fn hybrid_mode_refuses_when_the_lookup_fails() {
        let (mut client, accepted) = hybrid_proxy(None);
        start_login(&mut client, "Steve");
        let json = reason(login_reply(&mut client));
        assert_eq!(json["translate"], "multiplayer.disconnect.authservers_down");
        assert!(accepted.join().unwrap().unwrap().is_none());
    }

    /// A hybrid login of `name` from `peer`, through a policy and a lookup shared
    /// between logins as the proxy shares them. Returns the proxy's first reply.
    fn hybrid_login_from(
        policy: &std::sync::Arc<LoginPolicy>,
        lookup: &std::sync::Arc<FakeLookup>,
        peer: IpAddr,
        name: &str,
    ) -> login::Clientbound {
        let config = Config {
            auth_mode: meridian_config::AuthMode::Hybrid,
            compression_threshold: -1,
            auth: meridian_config::AuthSettings {
                lookups_per_minute: 1,
                ..Default::default()
            },
            ..Config::default()
        };
        let (client, server) = Pipe::pair();
        let (policy, lookup) = (policy.clone(), lookup.clone());
        let accepted = std::thread::spawn(move || {
            let sessions = FakeSessions {
                answer: Answer::NotJoined,
                asked: Asked::default(),
            };
            let checks = Checks {
                sessions: &sessions,
                lookup: &*lookup,
                policy: &policy,
                peer: Some(peer),
            };
            accept_with(Connection::local(server), &config, &checks)
        });
        let mut client = Connection::local(client);
        start_login(&mut client, name);
        let reply = login_reply(&mut client);
        if matches!(reply, login::Clientbound::LoginFinished(_)) {
            acknowledge(&mut client);
        }
        accepted.join().unwrap().unwrap();
        reply
    }

    #[test]
    fn each_address_has_its_own_limit_of_lookups() {
        let policy = std::sync::Arc::new(LoginPolicy::new());
        let lookup = std::sync::Arc::new(FakeLookup::with_accounts(&[]));
        let first: IpAddr = "192.0.2.1".parse().unwrap();
        let second: IpAddr = "192.0.2.2".parse().unwrap();
        let finished =
            |reply: &login::Clientbound| matches!(reply, login::Clientbound::LoginFinished(_));

        // One lookup a minute: the first address uses its own.
        assert!(finished(&hybrid_login_from(
            &policy, &lookup, first, "Alpha"
        )));
        // A second new name from the same address is refused before any lookup.
        let refused = reason(hybrid_login_from(&policy, &lookup, first, "Bravo"));
        assert!(
            refused["text"]
                .as_str()
                .unwrap()
                .contains("Too many logins"),
            "{refused}"
        );
        // Another address still has its lookup.
        assert!(finished(&hybrid_login_from(
            &policy, &lookup, second, "Charlie"
        )));
        assert_eq!(lookup.calls(), 2);
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

    /// Short bounds, so that the tests of the deadlines take a moment.
    const SHORT: Bounds = Bounds {
        silence: Duration::from_millis(400),
        login: Duration::from_millis(800),
        status: Duration::from_millis(800),
        max_frame: 2048,
    };

    /// A proxy that runs `accept` within `bounds`; the client's two halves.
    fn bounded_proxy(
        config: Config,
        bounds: Bounds,
    ) -> (
        horizon_protocol::connection::ConnReader,
        horizon_protocol::connection::ConnWriter,
        JoinHandle<Result<Option<ClientSession>>>,
    ) {
        let (client, server) = Pipe::pair();
        let accepted = std::thread::spawn(move || {
            accept_bounded(
                Connection::local(server),
                &config,
                &Checks::real(None),
                &bounds,
            )
        });
        let (reader, writer) = Connection::local(client).split();
        (reader, writer, accepted)
    }

    /// Whether `accepted` has ended within `limit`.
    fn ends_within<T>(accepted: &JoinHandle<T>, limit: Duration) -> bool {
        let until = Instant::now() + limit;
        while Instant::now() < until {
            if accepted.is_finished() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        accepted.is_finished()
    }

    /// Writes `bytes` one at a time, `every` apart, until `stop` is set.
    fn trickle(
        mut writer: horizon_protocol::connection::ConnWriter,
        bytes: Vec<u8>,
        every: Duration,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> JoinHandle<()> {
        std::thread::spawn(move || {
            for b in bytes {
                if stop.load(std::sync::atomic::Ordering::Relaxed)
                    || writer.write_raw(&[b]).is_err()
                {
                    return;
                }
                std::thread::sleep(every);
            }
        })
    }

    fn intention(intent: i32) -> Vec<u8> {
        handshake::Serverbound::Intention(handshake::Intention {
            protocol: horizon_protocol::PROTOCOL_VERSION,
            host: "localhost".into(),
            port: 25565,
            intent,
        })
        .encode()
    }

    /// A frame of `length` bytes, of which only the length means anything: what a
    /// client that sends a byte at a time, never quite silent, starts with.
    fn announced(length: usize) -> Vec<u8> {
        let mut bytes = Vec::new();
        horizon_protocol::buf::write_varint(&mut bytes, length as i32);
        bytes.extend(std::iter::repeat_n(0u8, length));
        bytes
    }

    #[test]
    fn a_login_that_comes_a_byte_at_a_time_ends_at_its_deadline() {
        let (mut reader, mut writer, accepted) = bounded_proxy(offline(-1), SHORT);
        writer.send(&intention(handshake::LOGIN)).unwrap();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        // A byte every 100 ms: never silent for the 400 ms of `silence`.
        let sender = trickle(
            writer,
            announced(1000),
            Duration::from_millis(100),
            stop.clone(),
        );
        let ended = ends_within(&accepted, Duration::from_secs(3));
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(ended, "the login is still open past its deadline");
        let (id, payload) = reader.recv().unwrap();
        let login::Clientbound::Disconnect(disconnect) =
            login::Clientbound::decode(id, &payload).unwrap()
        else {
            panic!("expected a disconnect");
        };
        assert!(
            disconnect
                .reason_json
                .contains("multiplayer.disconnect.slow_login")
        );
        assert!(accepted.join().unwrap().is_err());
        sender.join().unwrap();
    }

    #[test]
    fn a_status_request_that_comes_a_byte_at_a_time_ends_at_its_deadline() {
        let (_reader, mut writer, accepted) = bounded_proxy(Config::default(), SHORT);
        writer.send(&intention(handshake::STATUS)).unwrap();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let sender = trickle(
            writer,
            announced(1000),
            Duration::from_millis(100),
            stop.clone(),
        );
        let ended = ends_within(&accepted, Duration::from_secs(3));
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(ended, "the status is still open past its deadline");
        assert!(accepted.join().unwrap().is_err());
        sender.join().unwrap();
    }

    #[test]
    fn a_frame_too_long_for_a_login_is_refused_from_its_length() {
        let bounds = Bounds {
            silence: Duration::from_secs(5),
            login: Duration::from_secs(10),
            status: Duration::from_secs(10),
            max_frame: 2048,
        };
        // As the first frame, and after the handshake: a length of 2 MiB and nothing
        // after it. The refusal comes from the length, without waiting for the bytes.
        for handshake_first in [false, true] {
            let (_reader, mut writer, accepted) = bounded_proxy(offline(-1), bounds);
            if handshake_first {
                writer.send(&intention(handshake::LOGIN)).unwrap();
            }
            writer.write_raw(&[0xff, 0xff, 0x7f]).unwrap();
            assert!(
                ends_within(&accepted, Duration::from_secs(2)),
                "the proxy waits for a frame it should refuse"
            );
            let Err(error) = accepted.join().unwrap() else {
                panic!("expected a refusal");
            };
            let error = error.to_string();
            assert!(error.contains("before the login is done"), "{error}");
        }
    }

    #[test]
    fn a_compressed_acknowledgement_is_read_and_a_huge_one_refused() {
        // A threshold of 0: the client compresses every packet, the acknowledgement too.
        let (mut client, accepted) = proxy(offline(0));
        start_login(&mut client, "Alex");
        login_reply(&mut client);
        client.set_compression(0);
        login_reply(&mut client);
        acknowledge(&mut client);
        assert!(accepted.join().unwrap().unwrap().is_some());

        // A data length of 8 MiB in a frame of a few bytes is refused before it is
        // inflated.
        let (mut client, accepted) = proxy(offline(0));
        start_login(&mut client, "Alex");
        login_reply(&mut client);
        client.set_compression(0);
        login_reply(&mut client);
        let mut frame = Vec::new();
        horizon_protocol::buf::write_varint(&mut frame, 8 << 20);
        frame.extend([0x78, 0x9c, 0x03, 0x00]);
        let mut bytes = Vec::new();
        horizon_protocol::buf::write_varint(&mut bytes, frame.len() as i32);
        bytes.extend(frame);
        client.writer.write_raw(&bytes).unwrap();
        let Err(error) = accepted.join().unwrap() else {
            panic!("expected a refusal");
        };
        let error = error.to_string();
        assert!(error.contains("before the login is done"), "{error}");
    }
}

#[cfg(test)]
#[path = "client_fuzz_tests.rs"]
mod fuzz_tests;
