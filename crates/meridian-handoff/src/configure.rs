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

    /// How many packs the client named in its answer to Select Known Packs: none when it
    /// sent no answer, or one that cannot be read. A server sends the entries of a pack the
    /// client knows without their data, so this shapes the target's registry data.
    pub fn known_pack_count(&self) -> usize {
        self.known_packs
            .as_deref()
            .and_then(|answer| Reader::new(answer).varint().ok())
            .map_or(0, |count| usize::try_from(count).unwrap_or(0))
    }

    /// A hash of everything the client sent, which is everything a server's configuration
    /// phase can depend on: two clients with the same key are configured the same way by
    /// the same server. A Horizon server, as vanilla's, sends the entries of a pack the
    /// client knows without their data, so a client that knows the core pack and one that
    /// knows none get different registry data from the same server.
    pub fn key(&self) -> [u8; 32] {
        let mut hash = Sha256::new();
        let mut part = |bytes: Option<&[u8]>| match bytes {
            Some(bytes) => {
                hash.update([1]);
                hash.update((bytes.len() as u64).to_be_bytes());
                hash.update(bytes);
            }
            None => hash.update([0]),
        };
        part(self.information.as_deref());
        part(self.known_packs.as_deref());
        part(Some(&(self.custom.len() as u64).to_be_bytes()));
        for custom in &self.custom {
            part(Some(custom));
        }
        part(Some(&[u8::from(self.accepted_conduct)]));
        hash.finalize().into()
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
        self.first_difference(other).map(|i| KINDS[i].1)
    }

    fn first_difference(&self, other: &Digest) -> Option<usize> {
        (0..KINDS.len()).find(|&i| self.0[i] != other.0[i])
    }

    /// The first 4 bytes of the hash of a kind, in hexadecimal.
    fn short(&self, kind: usize) -> String {
        hex(&self.0[kind][..4])
    }
}

/// A configuration phase as far as the log of a refused switch says it: for each
/// kind of [`KINDS`], its packets and bytes, and for the registry data, the registry
/// each packet carries, with its entries, its bytes and a short hash. It holds no
/// content: registry names are the game's, not a player's.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    kinds: [Sizes; KINDS.len()],
    registries: Vec<Registry>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Sizes {
    packets: u32,
    bytes: u64,
}

/// One Registry Data packet: the registry's name and its number of entries when they
/// can be read, its size and the first bytes of its hash.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Registry {
    name: Option<String>,
    entries: Option<u32>,
    bytes: usize,
    hash: [u8; 4],
}

impl Registry {
    fn of(payload: &[u8]) -> Registry {
        let mut reader = Reader::new(payload);
        // A registry's name is an identifier; anything else is not shown.
        let name = reader.string(256).ok().filter(|name| {
            !name.is_empty()
                && name.bytes().all(
                    |b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'.' | b':' | b'/'),
                )
        });
        let entries = name
            .as_ref()
            .and_then(|_| reader.varint().ok())
            .and_then(|count| u32::try_from(count).ok());
        let mut hash = [0; 4];
        hash.copy_from_slice(&Sha256::digest(payload)[..4]);
        Registry {
            name,
            entries,
            bytes: payload.len(),
            hash,
        }
    }

    /// The name a registry is known by: its own, or its place among the packets.
    fn label(&self, index: usize) -> String {
        self.name
            .clone()
            .unwrap_or_else(|| format!("registry packet {} (unreadable name)", index + 1))
    }

    fn describe(&self) -> String {
        match self.entries {
            Some(entries) => format!("{entries} entries, {} bytes", self.bytes),
            None => format!("{} bytes", self.bytes),
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Why two configurations differ, for the log: the kind, its packets, bytes and short
/// hash on each side, and for the registry data, how many registries differ and the
/// first of them. `ours` is the configuration the client holds, `theirs` the target's.
pub fn explain(
    target: &str,
    ours: (&Digest, &Summary),
    theirs: (&Digest, &Summary),
) -> Option<String> {
    let kind = ours.0.first_difference(theirs.0)?;
    let side = |(digest, summary): (&Digest, &Summary)| {
        let sizes = summary.kinds[kind];
        format!(
            "{} packets, {} bytes, hash {}",
            sizes.packets,
            sizes.bytes,
            digest.short(kind)
        )
    };
    let mut text = format!(
        "{}: the client's {}, {target}'s {}",
        KINDS[kind].1,
        side(ours),
        side(theirs)
    );
    if KINDS[kind].0 == c::REGISTRY_DATA {
        text.push_str(&registries(
            target,
            &ours.1.registries,
            &theirs.1.registries,
        ));
    }
    Some(text)
}

/// How the registries of two configurations differ: by name where both can be read,
/// by place otherwise.
fn registries(target: &str, ours: &[Registry], theirs: &[Registry]) -> String {
    let key = |r: &Registry, i: usize| r.name.clone().unwrap_or_else(|| format!("#{i}"));
    let find = |list: &[Registry], wanted: &str| {
        list.iter()
            .enumerate()
            .find(|&(i, r)| key(r, i) == wanted)
            .map(|(_, r)| r.clone())
    };
    let mut differ = 0;
    let mut first = None;
    for (i, r) in ours.iter().enumerate() {
        let other = find(theirs, &key(r, i));
        if other
            .as_ref()
            .is_none_or(|o| o.hash != r.hash || o.bytes != r.bytes)
        {
            differ += 1;
            first.get_or_insert((r.label(i), Some(r.describe()), other.map(|o| o.describe())));
        }
    }
    for (i, r) in theirs.iter().enumerate() {
        if find(ours, &key(r, i)).is_none() {
            differ += 1;
            first.get_or_insert((r.label(i), None, Some(r.describe())));
        }
    }
    let Some((name, mine, other)) = first else {
        return "; every registry is the same, so their order differs".to_owned();
    };
    let show = |side: Option<String>| side.unwrap_or_else(|| "absent".to_owned());
    format!(
        "; {differ} registries differ, the first {name} (the client's {}, {target}'s {})",
        show(mine),
        show(other)
    )
}

/// Builds a [`Digest`] from the clientbound packets of a configuration phase.
#[derive(Clone, Debug)]
pub struct DigestBuilder {
    hashes: [Sha256; KINDS.len()],
    summary: Summary,
}

impl Default for DigestBuilder {
    fn default() -> Self {
        DigestBuilder {
            hashes: std::array::from_fn(|_| Sha256::new()),
            summary: Summary::default(),
        }
    }
}

impl DigestBuilder {
    /// Adds a clientbound configuration packet; packets of other kinds change
    /// nothing.
    pub fn add(&mut self, id: i32, payload: &[u8]) {
        if let Some(i) = KINDS.iter().position(|(kind, _)| *kind == id) {
            // The length first, so that two packets never hash as one.
            self.hashes[i].update((payload.len() as u64).to_be_bytes());
            self.hashes[i].update(payload);
            let sizes = &mut self.summary.kinds[i];
            sizes.packets = sizes.packets.saturating_add(1);
            sizes.bytes = sizes.bytes.saturating_add(payload.len() as u64);
            if id == c::REGISTRY_DATA {
                self.summary.registries.push(Registry::of(payload));
            }
        }
    }

    pub fn finish(&self) -> Digest {
        Digest(std::array::from_fn(|i| {
            self.hashes[i].clone().finalize().into()
        }))
    }

    /// What the log of a refused switch says of this configuration.
    pub fn summary(&self) -> Summary {
        self.summary.clone()
    }
}

/// Takes a target connection, just logged in, through its configuration phase as
/// the client went through its own, and returns the target's digest. On success
/// the proxy has acknowledged Finish Configuration: the connection is in the play
/// phase.
pub fn configure(
    connection: &mut Connection,
    client: &ClientConfiguration,
) -> Result<(Digest, Summary), String> {
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
                return Ok((digest.finish(), digest.summary()));
            }
            _ => {}
        }
    }
}
