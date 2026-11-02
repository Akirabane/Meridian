//! The server's side: the proxy connects to a backend as a client and takes the
//! connection through login, leaving it at the start of the configuration phase.
//! The backend runs with online mode off and learns who the player is from the
//! player information forwarding ([`forwarding`]), signed with a secret the proxy
//! and every backend share.
mod forwarding;

use crate::{Profile, Result};
use horizon_protocol::{
    PROTOCOL_VERSION,
    connection::Connection,
    packets::{Empty, handshake, login},
};
use std::{net::IpAddr, time::Duration};

/// A connection to a backend on which the player is logged in.
pub struct BackendLink {
    pub connection: Connection,
}

/// What connecting to a backend needs besides its address and the player.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendOptions {
    /// The secret shared with the backends, which signs the forwarded identity.
    pub forwarding_secret: Vec<u8>,
    /// How long reaching the backend may take.
    pub connect_timeout: Duration,
    /// The player's own address, forwarded to the backend.
    pub player_address: IpAddr,
}

/// Opens a connection to `address` (`host:port`) and logs `profile` in.
pub fn connect(address: &str, profile: &Profile, options: &BackendOptions) -> Result<BackendLink> {
    let (host, port) = split_address(address)?;
    let connection = Connection::connect(host, port, options.connect_timeout)?;
    login(connection, host, port, profile, options)
}

/// Logs `profile` in on a connection that is already open, as [`connect`] does
/// once it has reached the backend. `host` and `port` go into the handshake.
pub fn login(
    mut connection: Connection,
    host: &str,
    port: u16,
    profile: &Profile,
    options: &BackendOptions,
) -> Result<BackendLink> {
    connection.send(
        &handshake::Serverbound::Intention(handshake::Intention {
            protocol: PROTOCOL_VERSION,
            host: host.to_owned(),
            port,
            intent: handshake::LOGIN,
        })
        .encode(),
    )?;
    connection.send(
        &login::Serverbound::Hello(login::ServerHello {
            name: profile.name.clone(),
            uuid: u128::from_be_bytes(profile.uuid),
        })
        .encode(),
    )?;
    loop {
        let (id, payload) = connection.recv()?;
        match login::Clientbound::decode(id, &payload)? {
            login::Clientbound::Compression(p) => connection.set_compression(p.threshold),
            login::Clientbound::CustomQuery(query) => {
                // Only the forwarding is understood; any other channel gets the
                // answer for a query the client does not know.
                let payload = (query.channel == forwarding::CHANNEL).then(|| {
                    forwarding::answer(&options.forwarding_secret, profile, options.player_address)
                });
                connection.send(
                    &login::Serverbound::CustomQueryAnswer(login::CustomQueryAnswer {
                        transaction: query.transaction,
                        payload,
                    })
                    .encode(),
                )?;
            }
            login::Clientbound::CookieRequest(request) => connection.send(
                &login::Serverbound::CookieResponse(login::CookieResponse {
                    key: request.key,
                    payload: None,
                })
                .encode(),
            )?,
            login::Clientbound::LoginFinished(_) => {
                connection.send(&login::Serverbound::LoginAcknowledged(Empty).encode())?;
                return Ok(BackendLink { connection });
            }
            login::Clientbound::Hello(_) => {
                return Err(refused(
                    "the backend asks players to authenticate: it must run with online mode off \
                     and the player information forwarding on",
                ));
            }
            login::Clientbound::Disconnect(p) => {
                return Err(refused(&format!(
                    "the backend refused the login: {}",
                    p.reason_json
                )));
            }
            login::Clientbound::Unknown(id, _) => {
                return Err(refused(&format!(
                    "the backend sent login packet {id}, which is not understood"
                )));
            }
        }
    }
}

fn refused(why: &str) -> crate::Error {
    crate::Error::Protocol(horizon_protocol::Error::new(why))
}

/// `host:port`, with the host of an IPv6 address in brackets (`[::1]:25565`).
fn split_address(address: &str) -> Result<(&str, u16)> {
    let bad = || refused(&format!("{address:?} is not a host:port address"));
    let (host, port) = address.rsplit_once(':').ok_or_else(bad)?;
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    let port = port.parse().map_err(|_| bad())?;
    if host.is_empty() {
        return Err(bad());
    }
    Ok((host, port))
}

#[cfg(test)]
mod tests {
    use super::*;
    use horizon_protocol::{ids, packets::handshake::LOGIN};
    use meridian_testkit::{
        backend::{FakeBackend, Forwarded, REFUSAL, finish_configuration},
        peer::Step,
        pipe,
        trace::{Dir, Recorder},
    };

    fn profile() -> Profile {
        Profile {
            name: "Alex".into(),
            uuid: 0x0123456789abcdef_fedcba9876543210_u128.to_be_bytes(),
            properties: vec![
                ("textures".into(), "e30=".into(), "c2lnbmVk".into()),
                ("other".into(), "dmFsdWU=".into(), String::new()),
            ],
        }
    }

    fn options(secret: &[u8]) -> BackendOptions {
        BackendOptions {
            forwarding_secret: secret.to_vec(),
            connect_timeout: Duration::from_secs(1),
            player_address: "203.0.113.5".parse().unwrap(),
        }
    }

    /// A Horizon server behind the proxy: compression on, the forwarding asked
    /// for and checked, then the configuration phase starts on a compressed
    /// connection.
    #[test]
    fn a_player_is_logged_in_with_forwarding_and_compression() {
        let (proxy, server) = pipe();
        let registry = vec![9u8; 300];
        let backend = FakeBackend {
            compression: Some(64),
            forwarding_secret: Some(b"shared".to_vec()),
            after_login: vec![
                Step::send(
                    ids::configuration::clientbound::REGISTRY_DATA,
                    registry.clone(),
                ),
                finish_configuration(),
            ],
            ..FakeBackend::default()
        }
        .spawn(server, Recorder::new());
        let mut link = login(proxy, "127.0.0.1", 25601, &profile(), &options(b"shared")).unwrap();
        assert_eq!(
            link.connection.recv().unwrap(),
            (ids::configuration::clientbound::REGISTRY_DATA, registry)
        );
        assert_eq!(
            link.connection.recv().unwrap().0,
            ids::configuration::clientbound::FINISH_CONFIGURATION
        );
        let (seen, _) = backend.join().unwrap().unwrap();
        assert_eq!(
            (seen.protocol, seen.host.as_str(), seen.port, seen.intent),
            (PROTOCOL_VERSION, "127.0.0.1", 25601, LOGIN)
        );
        assert_eq!(seen.name, "Alex");
        assert_eq!(seen.uuid, 0x0123456789abcdef_fedcba9876543210);
        assert_eq!(
            seen.forwarded,
            Some(Forwarded {
                version: 1,
                address: "203.0.113.5".into(),
                uuid: 0x0123456789abcdef_fedcba9876543210,
                name: "Alex".into(),
                properties: vec![
                    ("textures".into(), "e30=".into(), Some("c2lnbmVk".into())),
                    ("other".into(), "dmFsdWU=".into(), None),
                ],
            })
        );
    }

    #[test]
    fn a_backend_with_another_secret_refuses_the_player() {
        let (proxy, server) = pipe();
        let backend = FakeBackend {
            forwarding_secret: Some(b"the backend's".to_vec()),
            ..FakeBackend::default()
        }
        .spawn(server, Recorder::new());
        let error = login(
            proxy,
            "127.0.0.1",
            25601,
            &profile(),
            &options(b"the proxy's"),
        )
        .err()
        .expect("a refusal")
        .to_string();
        assert!(error.contains(REFUSAL), "{error}");
        assert!(backend.join().unwrap().is_err());
    }

    #[test]
    fn an_online_mode_backend_is_refused() {
        let (proxy, server) = pipe();
        let recorder = Recorder::new();
        let backend = FakeBackend {
            online_mode: true,
            ..FakeBackend::default()
        }
        .spawn(server, recorder.clone());
        let error = login(proxy, "127.0.0.1", 25601, &profile(), &options(b"s"))
            .err()
            .expect("a refusal")
            .to_string();
        assert!(error.contains("online mode off"), "{error}");
        backend.join().unwrap().unwrap();
        // Handshake and Login Start, and no answer to the encryption request.
        assert_eq!(recorder.trace().count(Dir::ToServer), 2);
    }

    /// Queries the proxy does not know get an empty answer, and cookies none.
    #[test]
    fn unknown_queries_and_cookies_are_answered_empty() {
        let (proxy, mut server) = pipe();
        let script = std::thread::spawn(move || {
            server.recv().unwrap();
            server.recv().unwrap();
            let send =
                |server: &mut Connection, p: login::Clientbound| server.send(&p.encode()).unwrap();
            send(
                &mut server,
                login::Clientbound::CustomQuery(login::CustomQuery {
                    transaction: 3,
                    channel: "example:other".into(),
                    data: vec![1],
                }),
            );
            let (id, payload) = server.recv().unwrap();
            let answer = login::Serverbound::decode(id, &payload).unwrap();
            send(
                &mut server,
                login::Clientbound::CookieRequest(login::CookieRequest {
                    key: "example:cookie".into(),
                }),
            );
            let (id, payload) = server.recv().unwrap();
            let cookie = login::Serverbound::decode(id, &payload).unwrap();
            send(
                &mut server,
                login::Clientbound::LoginFinished(login::LoginFinished {
                    profile: login::GameProfile {
                        uuid: 1,
                        name: "Alex".into(),
                        properties: Vec::new(),
                    },
                    session: 0,
                }),
            );
            let (acknowledged, _) = server.recv().unwrap();
            (answer, cookie, acknowledged)
        });
        login(proxy, "127.0.0.1", 25601, &profile(), &options(b"s")).unwrap();
        let (answer, cookie, acknowledged) = script.join().unwrap();
        assert_eq!(
            answer,
            login::Serverbound::CustomQueryAnswer(login::CustomQueryAnswer {
                transaction: 3,
                payload: None,
            })
        );
        assert_eq!(
            cookie,
            login::Serverbound::CookieResponse(login::CookieResponse {
                key: "example:cookie".into(),
                payload: None,
            })
        );
        assert_eq!(acknowledged, ids::login::serverbound::LOGIN_ACKNOWLEDGED);
    }

    #[test]
    fn addresses_split_into_host_and_port() {
        assert_eq!(
            split_address("127.0.0.1:25601").unwrap(),
            ("127.0.0.1", 25601)
        );
        assert_eq!(split_address("game-1:25565").unwrap(), ("game-1", 25565));
        assert_eq!(split_address("[::1]:25602").unwrap(), ("::1", 25602));
        for bad in ["game-1", ":25565", "game-1:port", "game-1:70000"] {
            assert!(split_address(bad).is_err(), "{bad}");
        }
    }
}
