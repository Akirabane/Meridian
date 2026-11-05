use super::*;
use crate::configure::DigestBuilder;

fn digest(packets: &[(i32, &[u8])]) -> Digest {
    let mut builder = DigestBuilder::default();
    for (id, payload) in packets {
        builder.add(*id, payload);
    }
    builder.finish()
}

#[test]
fn digests_say_which_kind_differs() {
    use cfg::clientbound as c;
    let a = digest(&[(c::REGISTRY_DATA, &[1]), (c::UPDATE_TAGS, &[2])]);
    assert_eq!(a.differs(&a), None);
    let tags = digest(&[(c::REGISTRY_DATA, &[1]), (c::UPDATE_TAGS, &[3])]);
    assert_eq!(a.differs(&tags), Some("tags"));
    // What does not have to match changes nothing.
    let other = digest(&[
        (c::REGISTRY_DATA, &[1]),
        (c::CUSTOM_PAYLOAD, &[9]),
        (c::UPDATE_TAGS, &[2]),
    ]);
    assert_eq!(a.differs(&other), None);
    // Two packets never hash as one.
    let split = digest(&[(c::REGISTRY_DATA, &[1, 2])]);
    let two = digest(&[(c::REGISTRY_DATA, &[1]), (c::REGISTRY_DATA, &[2])]);
    assert_eq!(split.differs(&two), Some("registry data"));
}

#[test]
fn what_the_client_sent_is_recorded() {
    use cfg::serverbound as s;
    let mut client = ClientConfiguration::default();
    client.record(s::CLIENT_INFORMATION, &[1]);
    client.record(s::CUSTOM_PAYLOAD, &[2]);
    client.record(s::SELECT_KNOWN_PACKS, &[3]);
    client.record(s::KEEP_ALIVE, &[4]);
    client.record(s::ACCEPT_CODE_OF_CONDUCT, &[]);
    assert_eq!(
        client,
        ClientConfiguration {
            information: Some(vec![1]),
            custom: vec![vec![2]],
            known_packs: Some(vec![3]),
            accepted_conduct: true,
        }
    );
}

/// The target's configuration played as the client played its own: what the
/// client sent goes first, keep-alives and cookies are answered, known packs get
/// the client's answer, and the end is acknowledged.
#[test]
fn a_target_is_configured_as_the_client_was() {
    use cfg::{clientbound as c, serverbound as s};
    let (mut proxy, mut server) = pipe();
    let client = ClientConfiguration {
        information: Some(INFORMATION.to_vec()),
        custom: vec![b"\x0fminecraft:brand\x07vanilla".to_vec()],
        known_packs: Some(PACKS_REPLY.to_vec()),
        accepted_conduct: false,
    };
    let script = std::thread::spawn(move || {
        assert_eq!(
            expect(&mut server, s::CLIENT_INFORMATION).unwrap(),
            INFORMATION
        );
        expect(&mut server, s::CUSTOM_PAYLOAD).unwrap();
        send(&mut server, c::REGISTRY_DATA, &REGISTRY);
        send(&mut server, c::SELECT_KNOWN_PACKS, &PACKS);
        assert_eq!(
            expect(&mut server, s::SELECT_KNOWN_PACKS).unwrap(),
            PACKS_REPLY
        );
        send(&mut server, c::PING, &[0, 0, 0, 4]);
        assert_eq!(expect(&mut server, s::PONG).unwrap(), [0, 0, 0, 4]);
        send(&mut server, c::COOKIE_REQUEST, b"\x0aexample:id");
        assert_eq!(
            expect(&mut server, s::COOKIE_RESPONSE).unwrap(),
            b"\x0aexample:id\x00"
        );
        send(&mut server, c::FINISH_CONFIGURATION, &[]);
        expect(&mut server, s::FINISH_CONFIGURATION).unwrap();
    });
    let (got, _) = configure(&mut proxy, &client).unwrap();
    script.join().unwrap();
    use c::{REGISTRY_DATA, SELECT_KNOWN_PACKS};
    assert_eq!(
        got,
        digest(&[(REGISTRY_DATA, &REGISTRY), (SELECT_KNOWN_PACKS, &PACKS)])
    );
}

#[test]
fn a_code_of_conduct_the_player_never_accepted_is_refused() {
    let (mut proxy, mut server) = pipe();
    send(&mut server, cfg::clientbound::CODE_OF_CONDUCT, b"\x02hi");
    let error = configure(&mut proxy, &ClientConfiguration::default()).unwrap_err();
    assert!(error.contains("code of conduct"), "{error}");
}

/// A Registry Data payload: the registry's name, its number of entries, then `rest` as
/// the entries.
fn registry(name: &str, entries: i32, rest: &[u8]) -> Vec<u8> {
    let mut w = horizon_protocol::buf::Writer::new();
    w.string(name);
    let mut payload = w.data;
    horizon_protocol::buf::write_varint(&mut payload, entries);
    payload.extend_from_slice(rest);
    payload
}

fn configured(packets: &[(i32, Vec<u8>)]) -> (Digest, crate::configure::Summary) {
    let mut builder = DigestBuilder::default();
    for (id, payload) in packets {
        builder.add(*id, payload);
    }
    (builder.finish(), builder.summary())
}

fn short(packets: &[&[u8]]) -> String {
    use sha2::{Digest as _, Sha256};
    let mut hash = Sha256::new();
    for payload in packets {
        hash.update((payload.len() as u64).to_be_bytes());
        hash.update(payload);
    }
    hash.finalize()[..4]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A refusal says, for the kind that differs, its packets, bytes and a short hash on
/// each side, and which registry differs first, with its entries and bytes: enough to
/// tell entries sent without their data (a pack the client knows) from entries sent
/// with it.
#[test]
fn a_difference_in_the_registries_is_explained() {
    use crate::configure::explain;
    use cfg::clientbound::{REGISTRY_DATA, UPDATE_TAGS};
    let banner = registry("minecraft:banner_pattern", 2, &[0, 0]);
    let biome_known = registry("minecraft:worldgen/biome", 3, &[0, 0, 0]);
    let biome_full = registry("minecraft:worldgen/biome", 3, &[1; 40]);
    let extra = registry("minecraft:damage_type", 1, &[0]);
    let ours = configured(&[
        (REGISTRY_DATA, banner.clone()),
        (REGISTRY_DATA, biome_known.clone()),
        (UPDATE_TAGS, vec![5]),
    ]);
    let theirs = configured(&[
        (REGISTRY_DATA, banner.clone()),
        (REGISTRY_DATA, biome_full.clone()),
        (REGISTRY_DATA, extra.clone()),
        (UPDATE_TAGS, vec![5]),
    ]);
    let text = explain("game1", (&ours.0, &ours.1), (&theirs.0, &theirs.1)).unwrap();
    assert_eq!(
        text,
        format!(
            "registry data: the client's 2 packets, {} bytes, hash {}, game1's 3 packets, {} \
             bytes, hash {}; 2 registries differ, the first minecraft:worldgen/biome (the \
             client's 3 entries, {} bytes, game1's 3 entries, {} bytes)",
            banner.len() + biome_known.len(),
            short(&[&banner, &biome_known]),
            banner.len() + biome_full.len() + extra.len(),
            short(&[&banner, &biome_full, &extra]),
            biome_known.len(),
            biome_full.len(),
        )
    );
    assert_eq!(
        explain("game1", (&ours.0, &ours.1), (&ours.0, &ours.1)),
        None
    );
    // The same registries in another order.
    let swapped = configured(&[
        (REGISTRY_DATA, biome_known.clone()),
        (REGISTRY_DATA, banner.clone()),
        (UPDATE_TAGS, vec![5]),
    ]);
    let text = explain("game1", (&ours.0, &ours.1), (&swapped.0, &swapped.1)).unwrap();
    assert!(
        text.ends_with("every registry is the same, so their order differs"),
        "{text}"
    );
    // Another kind: its sizes and hashes, no registry.
    let tags = configured(&[
        (REGISTRY_DATA, banner.clone()),
        (REGISTRY_DATA, biome_known.clone()),
        (UPDATE_TAGS, vec![6, 6]),
    ]);
    let text = explain("game1", (&ours.0, &ours.1), (&tags.0, &tags.1)).unwrap();
    assert_eq!(
        text,
        format!(
            "tags: the client's 1 packets, 1 bytes, hash {}, game1's 1 packets, 2 bytes, hash {}",
            short(&[&[5]]),
            short(&[&[6, 6]])
        )
    );
}

/// A name that is not an identifier is not written: the packet is named by its place.
#[test]
fn a_registry_whose_name_cannot_be_read_is_named_by_its_place() {
    use crate::configure::explain;
    use cfg::clientbound::REGISTRY_DATA;
    let ours = configured(&[(REGISTRY_DATA, vec![7; 30])]);
    let theirs = configured(&[(REGISTRY_DATA, registry("Not\nA Name", 1, &[0]))]);
    let text = explain("game1", (&ours.0, &ours.1), (&theirs.0, &theirs.1)).unwrap();
    assert!(
        text.ends_with(
            "1 registries differ, the first registry packet 1 (unreadable name) (the client's \
             30 bytes, game1's 13 bytes)"
        ),
        "{text}"
    );
    assert!(!text.contains("Not"), "{text}");
}

#[test]
fn the_known_packs_a_client_named_are_counted() {
    use cfg::serverbound::SELECT_KNOWN_PACKS;
    let mut client = ClientConfiguration::default();
    assert_eq!(client.known_pack_count(), 0);
    client.record(SELECT_KNOWN_PACKS, &[0]);
    assert_eq!(client.known_pack_count(), 0);
    // minecraft:core 26.3
    client.record(SELECT_KNOWN_PACKS, b"\x01\x09minecraft\x04core\x0426.3");
    assert_eq!(client.known_pack_count(), 1);
}
