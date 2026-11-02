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
    let got = configure(&mut proxy, &client).unwrap();
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
