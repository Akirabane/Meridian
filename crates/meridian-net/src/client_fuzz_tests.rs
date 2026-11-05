//! The player's side fed hostile bytes: valid handshakes, status requests and logins
//! changed at random from a fixed seed. Whatever comes, `accept` must end, without a
//! panic, within its bounds.
use super::*;
use horizon_protocol::connection::Pipe;
use meridian_testkit::mutate::{Rng, mutate};
use std::time::Instant;

/// The rounds of each test: a few seconds in all, on a slow machine.
const ROUNDS: usize = 3000;

fn frame(body: &[u8]) -> Vec<u8> {
    horizon_protocol::frame::encode(body, None).unwrap()
}

fn intention(intent: i32, host: &str) -> Vec<u8> {
    frame(
        &handshake::Serverbound::Intention(handshake::Intention {
            protocol: horizon_protocol::PROTOCOL_VERSION,
            host: host.into(),
            port: 25565,
            intent,
        })
        .encode(),
    )
}

/// What a client sends, whole, for a status request and its ping, and for an offline
/// login with its acknowledgement.
fn streams() -> Vec<Vec<u8>> {
    let mut status = intention(handshake::STATUS, "play.example.org");
    status.extend(frame(
        &status::Serverbound::StatusRequest(Default::default()).encode(),
    ));
    status.extend(frame(
        &status::Serverbound::PingRequest(status::Ping { time: 42 }).encode(),
    ));
    let mut login = intention(handshake::LOGIN, "localhost");
    login.extend(frame(
        &login::Serverbound::Hello(login::ServerHello {
            name: "Alex".into(),
            uuid: 7,
        })
        .encode(),
    ));
    login.extend(frame(
        &login::Serverbound::LoginAcknowledged(Default::default()).encode(),
    ));
    vec![status, login]
}

/// Runs `accept` on `bytes`, sent at once and then closed, and says how it ended.
fn run(
    config: &Config,
    bounds: Bounds,
    bytes: Vec<u8>,
    keep_open: bool,
) -> std::thread::Result<bool> {
    let (client, server) = Pipe::pair();
    let config = config.clone();
    let accepted = std::thread::spawn(move || {
        accept_bounded(
            Connection::local(server),
            &config,
            &Checks::real(None),
            &bounds,
        )
        .map(|session| session.is_some())
        .unwrap_or(false)
    });
    let mut client = Connection::local(client);
    let _ = client.writer.write_raw(&bytes);
    // The end of what the client sends: a parser that waits for more sees it at once.
    // A valid login is kept open, for the proxy's answers to reach it.
    let kept = keep_open.then_some(client);
    let started = Instant::now();
    let ended = accepted.join();
    drop(kept);
    assert!(
        started.elapsed() < bounds.login + Duration::from_secs(1),
        "accept took {:?}",
        started.elapsed()
    );
    ended
}

#[test]
fn hostile_handshakes_statuses_and_logins_end_without_a_panic() {
    let config = Config {
        online_mode: false,
        compression_threshold: -1,
        ..Config::default()
    };
    let bounds = Bounds {
        silence: Duration::from_millis(200),
        login: Duration::from_millis(600),
        status: Duration::from_millis(600),
        max_frame: BOUNDS.max_frame,
    };
    let streams = streams();
    // The valid streams pass, so that the changes start from what a client sends.
    assert!(!run(&config, bounds, streams[0].clone(), false).unwrap());
    assert!(run(&config, bounds, streams[1].clone(), true).unwrap());
    let mut rng = Rng::new(0x6d65_7269_6469_616e);
    for round in 0..ROUNDS {
        let which = rng.below(streams.len());
        let input = mutate(&mut rng, &streams[which]);
        if run(&config, bounds, input.clone(), false).is_err() {
            panic!("round {round} panicked on {input:02x?}");
        }
    }
}

/// The bounds of the tests below: short, so that a test that waits fails quickly.
const QUICK: Bounds = Bounds {
    silence: Duration::from_millis(500),
    login: Duration::from_secs(2),
    status: Duration::from_secs(2),
    max_frame: BOUNDS.max_frame,
};

/// What `bytes` sent at once make of a connection: whether a session came, and
/// whether the proxy ended it before its deadline. A session never comes from what
/// these tests send.
fn ends_without_a_session(bytes: Vec<u8>) {
    let config = Config {
        online_mode: false,
        compression_threshold: -1,
        ..Config::default()
    };
    let (client, server) = Pipe::pair();
    let accepted = std::thread::spawn(move || {
        accept_bounded(
            Connection::local(server),
            &config,
            &Checks::real(None),
            &QUICK,
        )
        .map(|session| session.is_some())
    });
    let mut client = Connection::local(client);
    client.writer.write_raw(&bytes).unwrap();
    let started = Instant::now();
    // The client stays: the proxy must end the connection on what it read.
    while !accepted.is_finished() {
        assert!(
            started.elapsed() < QUICK.silence,
            "the proxy waits for more after {bytes:02x?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!matches!(accepted.join().unwrap(), Ok(true)));
}

fn hello(name: &str) -> Vec<u8> {
    frame(
        &login::Serverbound::Hello(login::ServerHello {
            name: name.into(),
            uuid: 7,
        })
        .encode(),
    )
}

#[test]
fn a_frame_length_that_cannot_be_is_refused_at_once() {
    // Zero; past 21 bits; a VarInt that never ends; a length longer than the limit.
    for first in [
        vec![0x00],
        vec![0x80, 0x80, 0x80, 0x01],
        vec![0xff; 6],
        vec![0x81, 0x10],
    ] {
        ends_without_a_session(first);
    }
}

#[test]
fn a_packet_of_another_state_ends_the_connection() {
    // A login's encryption answer where a status request goes, a second handshake in a
    // login, an encryption answer that was never asked for, a play packet before the
    // login, and the acknowledgement before the hello.
    let key = frame(
        &login::Serverbound::Key(login::Key {
            key: vec![1; 128],
            challenge: vec![2; 128],
        })
        .encode(),
    );
    let mut play = Vec::new();
    horizon_protocol::buf::write_varint(&mut play, 0x2c);
    play.extend([1, 2, 3]);
    let acknowledged = frame(&login::Serverbound::LoginAcknowledged(Default::default()).encode());
    let after_login = |next: &[u8]| {
        let mut bytes = intention(handshake::LOGIN, "localhost");
        bytes.extend(next);
        bytes
    };
    let mut status_then_key = intention(handshake::STATUS, "localhost");
    status_then_key.extend(&key);
    ends_without_a_session(status_then_key);
    ends_without_a_session(after_login(&intention(handshake::LOGIN, "localhost")));
    ends_without_a_session(after_login(&key));
    ends_without_a_session(after_login(&frame(&play)));
    ends_without_a_session(after_login(&acknowledged));
    let mut key_after_hello = after_login(&hello("Alex"));
    key_after_hello.extend(&key);
    ends_without_a_session(key_after_hello);
}

#[test]
fn a_name_outside_the_rules_is_refused() {
    for name in [
        "",
        "seventeen_letters",
        "with space",
        "é",
        "a\nb",
        "\u{202e}x",
    ] {
        ends_without_a_session({
            let mut bytes = intention(handshake::LOGIN, "localhost");
            bytes.extend(hello(name));
            bytes
        });
    }
    // A name that is not UTF-8: its length, then bytes no text has.
    let mut bytes = intention(handshake::LOGIN, "localhost");
    let mut body = Vec::new();
    horizon_protocol::buf::write_varint(&mut body, 0);
    body.extend([4, 0xff, 0xfe, 0xc0, 0x80]);
    body.extend([0; 16]);
    bytes.extend(frame(&body));
    ends_without_a_session(bytes);
}

#[test]
fn a_host_longer_than_vanillas_is_refused() {
    let host = "a".repeat(256);
    let mut body = Vec::new();
    horizon_protocol::buf::write_varint(&mut body, 0);
    horizon_protocol::buf::write_varint(&mut body, horizon_protocol::PROTOCOL_VERSION);
    horizon_protocol::buf::write_varint(&mut body, host.len() as i32);
    body.extend(host.as_bytes());
    body.extend(25565u16.to_be_bytes());
    horizon_protocol::buf::write_varint(&mut body, handshake::LOGIN);
    ends_without_a_session(frame(&body));
}
