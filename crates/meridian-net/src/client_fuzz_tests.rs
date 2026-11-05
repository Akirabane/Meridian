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
