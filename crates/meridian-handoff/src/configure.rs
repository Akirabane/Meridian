//! The target's configuration phase, played for the client as the client played its
//! own, and the digest that says whether the two servers configured the client the
//! same way (DESIGN.md decision 4).
//!
//! The client goes through one configuration phase, with the first backend. A
//! switch never sends it another, so the target's registries, tags, features and
//! known packs must be the ones the client already holds. The digest hashes them as
//! the target sends them on the prepare connection itself, before the player is
//! placed anywhere: a held player is not placed until the import (see the crate's
//! documentation).
use horizon_protocol::{
    buf::{Reader, Writer, write_varint},
    connection::Connection,
    ids::configuration::{clientbound as c, serverbound as s},
};
use sha2::{Digest as _, Sha256};

/// What the client sent on its own in its configuration phase, sent again to a
/// target the same way.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClientConfiguration {
    /// Client Information: language, view distance, chat and skin settings.
    pub information: Option<Vec<u8>>,
    /// Custom payloads, in order (the client's brand among them).
    pub custom: Vec<Vec<u8>>,
    /// The client's answer to Select Known Packs.
    pub known_packs: Option<Vec<u8>>,
    /// Whether the client accepted a code of conduct.
    pub accepted_conduct: bool,
}

impl ClientConfiguration {
    /// Notes a packet the client sent in its configuration phase.
    pub fn record(&mut self, id: i32, payload: &[u8]) {
        match id {
            s::CLIENT_INFORMATION => self.information = Some(payload.to_vec()),
            s::CUSTOM_PAYLOAD => self.custom.push(payload.to_vec()),
            s::SELECT_KNOWN_PACKS => self.known_packs = Some(payload.to_vec()),
            s::ACCEPT_CODE_OF_CONDUCT => self.accepted_conduct = true,
            _ => {}
        }
    }
}

/// What must match between two servers' configuration phases, kind by kind.
const KINDS: [(i32, &str); 5] = [
    (c::REGISTRY_DATA, "registry data"),
    (c::UPDATE_TAGS, "tags"),
    (c::UPDATE_ENABLED_FEATURES, "enabled features"),
    (c::SELECT_KNOWN_PACKS, "known packs"),
    (c::CODE_OF_CONDUCT, "code of conduct"),
];

/// The configuration a server gave a client, reduced to a hash of each kind of
/// packet that must match between servers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Digest([[u8; 32]; KINDS.len()]);

impl Digest {
    /// The first kind of packet on which the two differ, or `None` when they match.
    pub fn differs(&self, other: &Digest) -> Option<&'static str> {
        (0..KINDS.len())
            .find(|&i| self.0[i] != other.0[i])
            .map(|i| KINDS[i].1)
    }
}

/// Builds a [`Digest`] from the clientbound packets of a configuration phase.
#[derive(Clone, Debug)]
pub struct DigestBuilder([Sha256; KINDS.len()]);

impl Default for DigestBuilder {
    fn default() -> Self {
        DigestBuilder(std::array::from_fn(|_| Sha256::new()))
    }
}

impl DigestBuilder {
    /// Adds a clientbound configuration packet; packets of other kinds change
    /// nothing.
    pub fn add(&mut self, id: i32, payload: &[u8]) {
        if let Some(i) = KINDS.iter().position(|(kind, _)| *kind == id) {
            // The length first, so that two packets never hash as one.
            self.0[i].update((payload.len() as u64).to_be_bytes());
            self.0[i].update(payload);
        }
    }

    pub fn finish(&self) -> Digest {
        Digest(std::array::from_fn(|i| self.0[i].clone().finalize().into()))
    }
}

/// Takes a target connection, just logged in, through its configuration phase as
/// the client went through its own, and returns the target's digest. On success
/// the proxy has acknowledged Finish Configuration: the connection is in the play
/// phase.
pub fn configure(
    connection: &mut Connection,
    client: &ClientConfiguration,
) -> Result<Digest, String> {
    let send = |connection: &mut Connection, id: i32, payload: &[u8]| {
        let mut body = Vec::with_capacity(payload.len() + 1);
        write_varint(&mut body, id);
        body.extend_from_slice(payload);
        connection
            .send(&body)
            .map_err(|e| format!("the target's connection broke: {e}"))
    };
    if let Some(information) = &client.information {
        send(connection, s::CLIENT_INFORMATION, information)?;
    }
    for custom in &client.custom {
        send(connection, s::CUSTOM_PAYLOAD, custom)?;
    }
    let mut digest = DigestBuilder::default();
    loop {
        let (id, payload) = connection.recv().map_err(|e| {
            if e.is_end_of_stream() {
                "the target closed the connection in its configuration phase".to_owned()
            } else {
                format!("the target's connection broke: {e}")
            }
        })?;
        if let Some((answer, payload)) = meridian_rewrite::answer(true, id, &payload) {
            send(connection, answer, &payload)?;
            continue;
        }
        digest.add(id, &payload);
        match id {
            c::SELECT_KNOWN_PACKS => {
                // No answer recorded: the client knows no pack.
                let known = client.known_packs.as_deref().unwrap_or(&[0]);
                send(connection, s::SELECT_KNOWN_PACKS, known)?;
            }
            c::CODE_OF_CONDUCT => {
                // The digest checks it is the one the client accepted.
                if !client.accepted_conduct {
                    return Err(
                        "the target shows a code of conduct the player never accepted".into(),
                    );
                }
                send(connection, s::ACCEPT_CODE_OF_CONDUCT, &[])?;
            }
            c::COOKIE_REQUEST => {
                let key = Reader::new(&payload)
                    .string(32767)
                    .map_err(|e| format!("the target's cookie request is unreadable: {e}"))?;
                let mut w = Writer::new();
                w.string(&key);
                w.bool(false);
                send(connection, s::COOKIE_RESPONSE, &w.data)?;
            }
            c::RESOURCE_PACK_PUSH => {
                return Err(
                    "the target sends a resource pack in its configuration phase, \
                            which a switch does not take yet"
                        .into(),
                );
            }
            c::DISCONNECT => {
                return Err("the target refused the player in its configuration phase".into());
            }
            c::TRANSFER => {
                return Err("the target sends the player to another address".into());
            }
            c::FINISH_CONFIGURATION => {
                send(connection, s::FINISH_CONFIGURATION, &[])?;
                return Ok(digest.finish());
            }
            _ => {}
        }
    }
}
