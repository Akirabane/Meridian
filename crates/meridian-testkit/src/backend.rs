//! A fake backend: it takes one player through the login as a Horizon server
//! does, then follows a script. It can turn compression on, ask for the player
//! information forwarding and refuse an answer that its secret did not sign, or
//! behave like an online-mode server.
//!
//! The forwarding check is written here on its own, from the exchange described in
//! Horizon's `docs/velocity-forwarding.md`, so that it does not share code with
//! the proxy it tests.
use crate::{
    peer::{self, Step, WAIT},
    trace::{Dir, Recorder},
};
use horizon_protocol::{
    buf::Reader,
    connection::Connection,
    ids,
    packets::{handshake, login},
};
use sha2::{Digest, Sha256};
use std::thread::JoinHandle;

/// The login plugin channel the forwarding is asked on.
pub const FORWARDING_CHANNEL: &str = "velocity:player_info";

/// The transaction id of the fake backend's forwarding request.
pub const FORWARDING_TRANSACTION: i32 = 7;

/// What the backend sends when it refuses the forwarded data.
pub const REFUSAL: &str = r#"{"text":"This server is behind a proxy: join through the proxy."}"#;

/// What the backend learned from the login.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Login {
    pub protocol: i32,
    pub host: String,
    pub port: u16,
    pub intent: i32,
    pub name: String,
    pub uuid: u128,
    /// What the proxy forwarded, when the backend asked.
    pub forwarded: Option<Forwarded>,
}

/// The player information a proxy forwards.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Forwarded {
    pub version: i32,
    pub address: String,
    pub uuid: u128,
    pub name: String,
    /// `(name, value, signature)`.
    pub properties: Vec<(String, String, Option<String>)>,
}

/// A backend scripted for one login.
#[derive(Clone, Debug, Default)]
pub struct FakeBackend {
    /// Turns compression on with this threshold before anything else.
    pub compression: Option<i32>,
    /// Asks for the forwarding and refuses an answer this secret did not sign.
    pub forwarding_secret: Option<Vec<u8>>,
    /// Asks the player to authenticate, as an online-mode server does.
    pub online_mode: bool,
    /// What it does once the login is acknowledged, from the configuration phase on.
    pub after_login: Vec<Step>,
}

impl FakeBackend {
    /// Runs the login and the script on `connection`. Every packet received is
    /// recorded as going to the server.
    pub fn run(&self, connection: &mut Connection, recorder: &Recorder) -> Result<Login, String> {
        connection
            .reader
            .set_read_timeout(Some(WAIT))
            .map_err(|e| e.to_string())?;
        let recv = |connection: &mut Connection| {
            let (id, payload) = connection.recv().map_err(|e| e.to_string())?;
            recorder.record(Dir::ToServer, id, &payload);
            Ok::<_, String>((id, payload))
        };
        let (id, payload) = recv(connection)?;
        let handshake::Serverbound::Intention(intention) =
            handshake::Serverbound::decode(id, &payload).map_err(|e| e.to_string())?
        else {
            return Err(format!("expected the handshake, received packet {id}"));
        };
        let (id, payload) = recv(connection)?;
        let login::Serverbound::Hello(hello) =
            login::Serverbound::decode(id, &payload).map_err(|e| e.to_string())?
        else {
            return Err(format!("expected Login Start, received packet {id}"));
        };
        let mut login = Login {
            protocol: intention.protocol,
            host: intention.host,
            port: intention.port,
            intent: intention.intent,
            name: hello.name,
            uuid: hello.uuid,
            forwarded: None,
        };
        let send = |connection: &mut Connection, packet: login::Clientbound| {
            connection.send(&packet.encode()).map_err(|e| e.to_string())
        };
        if self.online_mode {
            send(
                connection,
                login::Clientbound::Hello(login::Hello {
                    server_id: String::new(),
                    public_key: vec![1; 16],
                    challenge: vec![2; 4],
                    should_authenticate: true,
                }),
            )?;
            return Ok(login);
        }
        if let Some(threshold) = self.compression {
            send(
                connection,
                login::Clientbound::Compression(login::Compression { threshold }),
            )?;
            connection.set_compression(threshold);
        }
        if let Some(secret) = &self.forwarding_secret {
            send(
                connection,
                login::Clientbound::CustomQuery(login::CustomQuery {
                    transaction: FORWARDING_TRANSACTION,
                    channel: FORWARDING_CHANNEL.into(),
                    data: vec![1],
                }),
            )?;
            let (id, payload) = recv(connection)?;
            let answer = match login::Serverbound::decode(id, &payload) {
                Ok(login::Serverbound::CustomQueryAnswer(answer))
                    if answer.transaction == FORWARDING_TRANSACTION =>
                {
                    answer.payload
                }
                _ => None,
            };
            match answer
                .ok_or_else(|| "no forwarding answer".to_owned())
                .and_then(|payload| verify(secret, &payload))
            {
                Ok(forwarded) => login.forwarded = Some(forwarded),
                Err(why) => {
                    send(
                        connection,
                        login::Clientbound::Disconnect(login::Disconnect {
                            reason_json: REFUSAL.into(),
                        }),
                    )?;
                    return Err(why);
                }
            }
        }
        send(
            connection,
            login::Clientbound::LoginFinished(login::LoginFinished {
                profile: login::GameProfile {
                    uuid: login.uuid,
                    name: login.name.clone(),
                    properties: Vec::new(),
                },
                session: 0,
            }),
        )?;
        let (id, _) = recv(connection)?;
        if id != ids::login::serverbound::LOGIN_ACKNOWLEDGED {
            return Err(format!("expected Login Acknowledged, received packet {id}"));
        }
        peer::run(connection, &self.after_login, Dir::ToServer, recorder)?;
        Ok(login)
    }

    /// Runs the backend on a thread of its own and hands back the connection at
    /// the end with what it learned.
    pub fn spawn(
        self,
        mut connection: Connection,
        recorder: Recorder,
    ) -> JoinHandle<Result<(Login, Connection), String>> {
        std::thread::spawn(move || {
            self.run(&mut connection, &recorder)
                .map(|login| (login, connection))
        })
    }
}

/// The backend's Finish Configuration, which ends the configuration phase.
pub fn finish_configuration() -> Step {
    Step::send(ids::configuration::clientbound::FINISH_CONFIGURATION, [])
}

/// The player's answer to Finish Configuration, which starts the play phase.
pub fn acknowledge_configuration() -> Step {
    Step::Send {
        id: ids::configuration::serverbound::FINISH_CONFIGURATION,
        payload: Vec::new(),
    }
}

/// Checks a forwarding answer against `secret` and reads it.
pub fn verify(secret: &[u8], payload: &[u8]) -> Result<Forwarded, String> {
    if payload.len() < 32 {
        return Err("the forwarded data is too short".into());
    }
    let (signature, data) = payload.split_at(32);
    if hmac_sha256(secret, data) != signature {
        return Err("the forwarded data is not signed with this secret".into());
    }
    let bad = |e: horizon_protocol::Error| format!("the forwarded data is damaged: {e}");
    let mut r = Reader::new(data);
    let version = r.varint().map_err(bad)?;
    let address = r.string(64).map_err(bad)?;
    let uuid = r.uuid().map_err(bad)?;
    let name = r.string(16).map_err(bad)?;
    let count = r.count(64).map_err(bad)?;
    let mut properties = Vec::with_capacity(count);
    for _ in 0..count {
        let name = r.string(64).map_err(bad)?;
        let value = r.string(32767).map_err(bad)?;
        let signature = if r.bool().map_err(bad)? {
            Some(r.string(1024).map_err(bad)?)
        } else {
            None
        };
        properties.push((name, value, signature));
    }
    if r.remaining() != 0 {
        return Err("the forwarded data has bytes left over".into());
    }
    Ok(Forwarded {
        version,
        address,
        uuid,
        name,
        properties,
    })
}

/// HMAC-SHA256, as RFC 2104 defines it.
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut padded = [0u8; BLOCK];
    if key.len() > BLOCK {
        padded[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        padded[..key.len()].copy_from_slice(key);
    }
    let inner = Sha256::new()
        .chain_update(padded.map(|b| b ^ 0x36))
        .chain_update(data)
        .finalize();
    Sha256::new()
        .chain_update(padded.map(|b| b ^ 0x5c))
        .chain_update(inner)
        .finalize()
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// RFC 4231, test cases 1, 2 and 6.
    #[test]
    fn hmac_matches_the_rfc_vectors() {
        assert_eq!(
            hmac_sha256(&[0x0b; 20], b"Hi There").to_vec(),
            hex("b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7")
        );
        assert_eq!(
            hmac_sha256(b"Jefe", b"what do ya want for nothing?").to_vec(),
            hex("5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843")
        );
        assert_eq!(
            hmac_sha256(
                &[0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )
            .to_vec(),
            hex("60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54")
        );
    }

    #[test]
    fn an_answer_signed_with_another_secret_is_refused() {
        let mut payload = hmac_sha256(b"right", &[1, 0]).to_vec();
        payload.extend([1, 0]);
        assert!(verify(b"wrong", &payload).is_err());
    }
}
