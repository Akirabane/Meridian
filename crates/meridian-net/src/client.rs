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
    packets::{handshake, status},
};
use meridian_config::Config;
use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

/// How long the handshake, a status request and the login may wait for the
/// client's next packet. Vanilla waits 30 s, but a client answers these at once,
/// and a silent socket should not hold a thread for that long.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(10);

/// The player limit shown in the server list. The configuration has no limit
/// yet, so the list shows vanilla's default `max-players`.
const MAX_PLAYERS_SHOWN: usize = 20;

/// A player who has logged in to the proxy and is ready to be relayed.
pub struct ClientSession {
    pub profile: Profile,
    pub connection: Connection,
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
/// answered and ends the connection, so it returns `Ok(None)`.
pub fn accept(mut connection: Connection, config: &Config) -> Result<Option<ClientSession>> {
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
        handshake::LOGIN | handshake::TRANSFER => Err(Error::NotImplemented("logging a player in")),
        other => Err(protocol_error(format!("unknown intent {other}"))),
    }
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
}
