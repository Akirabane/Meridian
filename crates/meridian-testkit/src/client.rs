//! A fake client: it logs in offline as a vanilla client does, follows the
//! compression the server asks for, acknowledges the login, then follows a script.
//! It talks to a fake backend directly, or to the proxy, so the two sessions can be
//! compared.
use crate::{
    peer::{self, Step, WAIT},
    trace::{Dir, Recorder},
};
use horizon_protocol::{
    PROTOCOL_VERSION,
    connection::Connection,
    packets::{Empty, handshake, login},
};
use std::thread::JoinHandle;

/// What the client was told at login.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Welcome {
    /// The threshold the server turned compression on with, if it did.
    pub compression: Option<i32>,
    /// The unique id the server gave the player.
    pub uuid: u128,
    /// The name the server gave the player.
    pub name: String,
}

/// A client scripted for one login.
#[derive(Clone, Debug)]
pub struct FakeClient {
    /// The name sent in Login Start.
    pub name: String,
    /// The unique id sent in Login Start.
    pub uuid: u128,
    /// The host and port written in the handshake.
    pub host: String,
    pub port: u16,
    /// What it does once the login is acknowledged, from the configuration phase on.
    pub after_login: Vec<Step>,
}

impl FakeClient {
    /// A client called `name`, with the unique id an offline server derives
    /// from it, that does nothing after the login.
    pub fn named(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            uuid: horizon_protocol::crypto::offline_uuid(name),
            host: "localhost".into(),
            port: 25565,
            after_login: Vec::new(),
        }
    }

    /// Runs the login and the script on `connection`. Only the packets of the
    /// script are recorded, as going to the client, so that a session through the
    /// proxy and a direct one record the same thing.
    pub fn run(&self, connection: &mut Connection, recorder: &Recorder) -> Result<Welcome, String> {
        connection
            .reader
            .set_read_timeout(Some(WAIT))
            .map_err(|e| e.to_string())?;
        let send = |connection: &mut Connection, body: Vec<u8>| {
            connection.send(&body).map_err(|e| e.to_string())
        };
        send(
            connection,
            handshake::Serverbound::Intention(handshake::Intention {
                protocol: PROTOCOL_VERSION,
                host: self.host.clone(),
                port: self.port,
                intent: handshake::LOGIN,
            })
            .encode(),
        )?;
        send(
            connection,
            login::Serverbound::Hello(login::ServerHello {
                name: self.name.clone(),
                uuid: self.uuid,
            })
            .encode(),
        )?;
        let mut compression = None;
        loop {
            let (id, payload) = connection.recv().map_err(|e| e.to_string())?;
            match login::Clientbound::decode(id, &payload).map_err(|e| e.to_string())? {
                login::Clientbound::Compression(p) => {
                    connection.set_compression(p.threshold);
                    compression = Some(p.threshold);
                }
                login::Clientbound::LoginFinished(p) => {
                    send(
                        connection,
                        login::Serverbound::LoginAcknowledged(Empty).encode(),
                    )?;
                    peer::run(connection, &self.after_login, Dir::ToClient, recorder)?;
                    return Ok(Welcome {
                        compression,
                        uuid: p.profile.uuid,
                        name: p.profile.name,
                    });
                }
                login::Clientbound::Disconnect(p) => {
                    return Err(format!("refused at login: {}", p.reason_json));
                }
                other => return Err(format!("unexpected login packet {}", other.id())),
            }
        }
    }

    /// Runs the client on a thread of its own and hands back the connection at
    /// the end with what it was told.
    pub fn spawn(
        self,
        mut connection: Connection,
        recorder: Recorder,
    ) -> JoinHandle<Result<(Welcome, Connection), String>> {
        std::thread::spawn(move || {
            self.run(&mut connection, &recorder)
                .map(|welcome| (welcome, connection))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{backend::FakeBackend, pipe};

    /// The fake client and the fake backend log in to each other directly.
    #[test]
    fn a_client_logs_in_to_a_backend_and_follows_its_script() {
        let (client_end, server_end) = pipe();
        let recorder = Recorder::new();
        let backend = FakeBackend {
            compression: Some(128),
            after_login: vec![Step::send(4, [1, 2]), Step::Expect { id: 9 }],
            ..FakeBackend::default()
        }
        .spawn(server_end, recorder.clone());
        let client = FakeClient {
            after_login: vec![Step::expect_exactly(4, [1, 2]), Step::send(9, [])],
            ..FakeClient::named("Alex")
        };
        let (welcome, _) = client
            .spawn(client_end, recorder.clone())
            .join()
            .unwrap()
            .unwrap();
        let (login, _) = backend.join().unwrap().unwrap();
        assert_eq!(welcome.compression, Some(128));
        assert_eq!(welcome.name, "Alex");
        assert_eq!(login.name, "Alex");
        assert_eq!(login.uuid, horizon_protocol::crypto::offline_uuid("Alex"));
        assert_eq!(recorder.trace().ids(Dir::ToClient), vec![4]);
    }
}
