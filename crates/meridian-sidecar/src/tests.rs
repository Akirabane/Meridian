//! The sidecar and its client together, over sockets on 127.0.0.1, in front of a
//! stand-in for a Horizon server's control endpoint.
use crate::Error;
use crate::client::SidecarClient;
use crate::endpoint::{Endpoint, fake};
use crate::request::Outcome;
use crate::server::{Settings, serve};
use crate::wire::{self, Direction};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

const SECRET: &[u8] = b"the secret the proxy and the sidecar share";
const TOKEN: &str = "the backend's own token";
const UUID: &str = "0123456789abcdef0123456789abcdef";
/// [`UUID`] as Horizon writes it.
const DASHED: &str = "01234567-89ab-cdef-0123-456789abcdef";
const WAIT: Duration = Duration::from_secs(3);

/// A sidecar on 127.0.0.1 in front of a fake endpoint that answers with `answer`.
struct Running {
    address: String,
    seen: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    sidecar: Option<JoinHandle<()>>,
    endpoint: Endpoint,
    endpoint_thread: Option<JoinHandle<()>>,
}

impl Running {
    fn start(answer: fake::Answer, endpoint_timeout: Duration) -> Self {
        let (endpoint, seen, endpoint_thread) = fake::start(TOKEN, answer);
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let found = endpoint.clone();
        let settings = Arc::new(Settings {
            secret: SECRET.to_vec(),
            endpoint: Box::new(move || Ok(found.clone())),
            endpoint_timeout,
            idle_timeout: WAIT,
            insecure_remote: false,
        });
        let stop = Arc::new(AtomicBool::new(false));
        let sidecar = {
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || serve(listener, settings, &stop).unwrap())
        };
        Self {
            address,
            seen,
            stop,
            sidecar: Some(sidecar),
            endpoint,
            endpoint_thread: Some(endpoint_thread),
        }
    }

    fn client(&self, secret: &[u8]) -> SidecarClient {
        SidecarClient::connect(&self.address, secret, WAIT).unwrap()
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.sidecar.take() {
            let _ = t.join();
        }
        if let Some(t) = self.endpoint_thread.take() {
            fake::stop(&self.endpoint, t);
        }
    }
}

/// Answers every command with what it was asked, so the test sees the line.
fn echo() -> fake::Answer {
    Arc::new(|line: &str| Some(json!({ "echo": line }).to_string()))
}

#[test]
fn every_request_reaches_the_backend_and_its_answer_comes_back() {
    let running = Running::start(echo(), WAIT);
    let mut client = running.client(SECRET);
    client.expect(UUID, None).unwrap();
    let exported = client.export(UUID, false).unwrap();
    assert_eq!(
        exported,
        json!({ "echo": format!("player-export {DASHED}") })
    );
    client
        .import(UUID, json!({ "version": 1 }), None, None)
        .unwrap();
    client.release(UUID, Outcome::Moved, false).unwrap();
    client.drop_player(UUID).unwrap();
    let commands: Vec<String> = running
        .seen
        .lock()
        .unwrap()
        .iter()
        .map(|l| l.split_once(' ').unwrap().0.to_owned())
        .collect();
    assert_eq!(
        commands,
        [
            "player-expect",
            "player-export",
            "player-import",
            "player-release",
            "player-drop"
        ]
    );
}

#[test]
fn a_client_with_another_secret_is_refused_and_nothing_reaches_the_backend() {
    let running = Running::start(echo(), WAIT);
    let mut client = running.client(b"not the shared secret");
    let error = client.export(UUID, false).unwrap_err();
    assert!(
        matches!(&error, Error::Refused(why) if why.contains("signature")),
        "{error}"
    );
    assert!(running.seen.lock().unwrap().is_empty());
    // The connection is not used again.
    assert!(client.export(UUID, false).is_err());
}

/// A raw connection: its challenge, and a way to send lines and read answers.
struct Raw {
    writer: TcpStream,
    reader: BufReader<TcpStream>,
    challenge: wire::Challenge,
}

impl Raw {
    fn open(address: &str) -> Self {
        let stream = TcpStream::connect(address).unwrap();
        stream.set_read_timeout(Some(WAIT)).unwrap();
        let writer = stream.try_clone().unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let challenge = wire::read_challenge(line.trim()).unwrap();
        Self {
            writer,
            reader,
            challenge,
        }
    }

    fn send(&mut self, line: &str) -> String {
        writeln!(self.writer, "{line}").unwrap();
        let mut answer = String::new();
        self.reader.read_line(&mut answer).unwrap();
        answer.trim().to_owned()
    }
}

#[test]
fn a_replayed_request_is_refused_on_its_connection_and_on_another() {
    let running = Running::start(echo(), WAIT);
    let body = json!({ "op": "drop", "uuid": UUID }).to_string();
    let mut first = Raw::open(&running.address);
    let line = wire::seal(SECRET, &first.challenge, Direction::Request, 1, &body);
    let reply = first.send(&line);
    assert!(
        wire::open(
            SECRET,
            &first.challenge,
            Direction::Reply,
            &reply,
            wire::Expected::Exactly(1)
        )
        .is_ok(),
        "{reply}"
    );
    // The same line again on the same connection: its number is used up.
    let again = first.send(&line);
    assert!(again.contains("sequence number 1"), "{again}");
    // The recorded line on a new connection: another challenge.
    let mut second = Raw::open(&running.address);
    let elsewhere = second.send(&line);
    assert!(elsewhere.contains("signature"), "{elsewhere}");
    assert_eq!(
        running.seen.lock().unwrap().len(),
        1,
        "one drop reached the backend"
    );
}

#[test]
fn a_silent_backend_is_reported_in_time() {
    let running = Running::start(Arc::new(|_| None), Duration::from_millis(300));
    let mut client = running.client(SECRET);
    let error = client.export(UUID, false).unwrap_err();
    assert!(
        matches!(&error, Error::Refused(why) if why.contains("no answer in time")),
        "{error}"
    );
}

#[test]
fn a_silent_sidecar_times_out_on_the_client() {
    // Something that sends a challenge and then nothing.
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let mute = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        writeln!(
            stream,
            "{}",
            wire::challenge_line(&[3; wire::CHALLENGE_LEN])
        )
        .unwrap();
        std::thread::sleep(Duration::from_secs(1));
    });
    let mut client = SidecarClient::connect(&address, SECRET, Duration::from_millis(200)).unwrap();
    assert!(matches!(client.export(UUID, false), Err(Error::Timeout)));
    mute.join().unwrap();
}

#[test]
fn a_backend_answer_that_is_not_json_is_an_error() {
    let running = Running::start(Arc::new(|_| Some("not json".to_owned())), WAIT);
    let mut client = running.client(SECRET);
    let error = client.export(UUID, false).unwrap_err().to_string();
    assert!(error.contains("did not answer with JSON"), "{error}");
}

#[test]
fn a_backend_error_is_passed_on() {
    let running = Running::start(
        Arc::new(|_| Some(r#"{"error":"no such player"}"#.to_owned())),
        WAIT,
    );
    let mut client = running.client(SECRET);
    let error = client.export(UUID, false).unwrap_err();
    assert!(
        matches!(&error, Error::Refused(why) if why == "no such player"),
        "{error}"
    );
    // A refusal from the backend leaves the connection usable.
    assert!(client.drop_player(UUID).is_err());
    assert_eq!(running.seen.lock().unwrap().len(), 2);
}

#[test]
fn a_forged_reply_is_refused_by_the_client() {
    // Something between the two that answers in the sidecar's place, without the
    // secret.
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let forger = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut writer = stream.try_clone().unwrap();
        let challenge = [5; wire::CHALLENGE_LEN];
        writeln!(writer, "{}", wire::challenge_line(&challenge)).unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        let body = json!({ "ok": true, "result": { "health": 20 } }).to_string();
        let forged = wire::seal(b"a guessed secret", &challenge, Direction::Reply, 1, &body);
        writeln!(writer, "{forged}").unwrap();
    });
    let mut client = SidecarClient::connect(&address, SECRET, WAIT).unwrap();
    let error = client.export(UUID, false).unwrap_err();
    assert!(
        matches!(error, Error::Wire(wire::WireError::BadMac)),
        "{error}"
    );
    forger.join().unwrap();
}

#[test]
fn a_request_that_is_not_a_player_request_is_refused_by_the_sidecar() {
    let running = Running::start(echo(), WAIT);
    let mut raw = Raw::open(&running.address);
    let line = wire::seal(
        SECRET,
        &raw.challenge,
        Direction::Request,
        1,
        r#"{"op":"teleport"}"#,
    );
    let reply = raw.send(&line);
    let (_, body) = wire::open(
        SECRET,
        &raw.challenge,
        Direction::Reply,
        &reply,
        wire::Expected::Exactly(1),
    )
    .unwrap();
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["ok"], json!(false));
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("not a player request")
    );
    assert!(running.seen.lock().unwrap().is_empty());
}

// The contract of Horizon's held-player work (docs/server.md, "Players arriving
// from another server"), with a fake endpoint that answers as it documents.

/// The record of the contract's `player-export` example, for [`DASHED`].
fn record() -> Value {
    json!({
        "version": 1,
        "data_version": 5023,
        "uuid": DASHED,
        "name": "Alex",
        "dimension": "minecraft:overworld",
        "position": [10.5, 70.0, -3.25],
        "rotation": [90.0, 15.0],
        "velocity": [0.0, 0.0, 0.0],
        "game_mode": 0,
        "player": "CgAAAA==",
        "advancements": {}
    })
}

/// The parts of a record, in the order the contract lists them.
const ALL_PARTS: [&str; 10] = [
    "inventory",
    "ender_chest",
    "experience",
    "health",
    "effects",
    "game_mode",
    "position",
    "spawn_point",
    "recipe_book",
    "advancements",
];

/// A backend that answers each command as the contract says, from what the line
/// holds, and refuses the lines the contract refuses.
fn contract() -> fake::Answer {
    Arc::new(|line: &str| {
        let refuse = |why: &str| Some(json!({ "error": why }).to_string());
        let (command, rest) = line.split_once(' ').unwrap_or((line, ""));
        let mut args = rest.split_whitespace();
        if args.next() != Some(DASHED) {
            return refuse("the sidecar sent a unique id Horizon would not write");
        }
        let reply = match command {
            "player-expect" => match args.next() {
                None => json!({ "ok": true, "renewed": false, "ttl_seconds": 10 }),
                Some("30") => json!({ "ok": true, "renewed": true, "ttl_seconds": 30 }),
                Some(_) => return refuse("the time must be a number of seconds above zero"),
            },
            "player-export" => match (args.next(), args.next()) {
                (None | Some("freeze"), None) => json!({ "ok": true, "record": record() }),
                _ => return refuse("player-export takes a unique id and at most freeze"),
            },
            "player-import" => {
                let body = rest.split_once(' ').map(|(_, b)| b).unwrap_or("");
                let Ok(request) = serde_json::from_str::<Value>(body) else {
                    return refuse("the request is not JSON");
                };
                if request["record"]["data_version"] != json!(5023) {
                    return refuse("the record was made by another version of the game");
                }
                let parts = request
                    .get("parts")
                    .cloned()
                    .unwrap_or_else(|| json!(ALL_PARTS));
                let announced = request.get("announce").cloned().unwrap_or(json!(true));
                json!({ "ok": true, "parts": parts, "announced": announced })
            }
            "player-release" => match (args.next(), args.next()) {
                (Some("moved"), None) => {
                    json!({ "ok": true, "released": "moved", "announced": false })
                }
                (Some("moved"), Some("announce")) => {
                    json!({ "ok": true, "released": "moved", "announced": true })
                }
                (Some("abort"), None) => json!({ "ok": true, "released": "abort" }),
                _ => {
                    return refuse(
                        "player-release <uuid> is followed by moved [announce] or abort",
                    );
                }
            },
            "player-drop" => json!({ "ok": true, "held": false, "expected": true }),
            _ => return refuse("unknown command"),
        };
        Some(reply.to_string())
    })
}

#[test]
fn a_switch_goes_through_the_contract_line_by_line() {
    let running = Running::start(contract(), WAIT);
    let mut client = running.client(SECRET);

    // On the backend the player goes to: expected for the default time, then 30 s.
    assert_eq!(
        client.expect(UUID, None).unwrap(),
        json!({ "ok": true, "renewed": false, "ttl_seconds": 10 })
    );
    assert_eq!(
        client.expect(UUID, Some(30)).unwrap(),
        json!({ "ok": true, "renewed": true, "ttl_seconds": 30 })
    );

    // On the backend the player leaves: exported with a freeze. The record comes
    // back exactly as the backend wrote it.
    let exported = client.export(UUID, true).unwrap();
    assert_eq!(exported, json!({ "ok": true, "record": record() }));

    // The record goes to the held player as it is: some parts, quietly...
    let imported = client
        .import(
            UUID,
            exported["record"].clone(),
            Some(vec!["inventory".into(), "experience".into()]),
            Some(false),
        )
        .unwrap();
    assert_eq!(
        imported,
        json!({ "ok": true, "parts": ["inventory", "experience"], "announced": false })
    );
    // ...or whole, with the default announce.
    let whole = client.import(UUID, record(), None, None).unwrap();
    assert_eq!(whole["announced"], json!(true));
    assert_eq!(whole["parts"], json!(ALL_PARTS));

    // The old backend lets the player go, or keeps them.
    assert_eq!(
        client.release(UUID, Outcome::Moved, true).unwrap(),
        json!({ "ok": true, "released": "moved", "announced": true })
    );
    assert_eq!(
        client.release(UUID, Outcome::Abort, false).unwrap(),
        json!({ "ok": true, "released": "abort" })
    );
    assert_eq!(client.drop_player(UUID).unwrap()["ok"], json!(true));

    // The lines are the contract's, word for word.
    let seen = running.seen.lock().unwrap().clone();
    assert_eq!(seen[0], format!("player-expect {DASHED}"));
    assert_eq!(seen[1], format!("player-expect {DASHED} 30"));
    assert_eq!(seen[2], format!("player-export {DASHED} freeze"));
    assert!(seen[3].starts_with(&format!("player-import {DASHED} {{")));
    assert_eq!(seen[5], format!("player-release {DASHED} moved announce"));
    assert_eq!(seen[6], format!("player-release {DASHED} abort"));
    assert_eq!(seen[7], format!("player-drop {DASHED}"));
}

#[test]
fn the_backends_refusals_come_back_as_refusals() {
    let running = Running::start(contract(), WAIT);
    let mut client = running.client(SECRET);
    let mut old = record();
    old["data_version"] = json!(4000);
    let error = client.import(UUID, old, None, None).unwrap_err();
    assert!(
        matches!(&error, Error::Refused(why) if why.contains("another version")),
        "{error}"
    );
    // The connection still serves the next request.
    assert_eq!(client.drop_player(UUID).unwrap()["ok"], json!(true));
}

#[test]
fn a_request_that_cannot_be_a_line_never_reaches_the_backend() {
    let running = Running::start(contract(), WAIT);
    let mut client = running.client(SECRET);
    let cases = [
        (
            client.expect("Alex", None).unwrap_err(),
            "is not a unique id",
        ),
        (client.expect(UUID, Some(61)).unwrap_err(), "from 1 to 60"),
        (
            client.release(UUID, Outcome::Abort, true).unwrap_err(),
            "announce goes with moved",
        ),
        (
            client.import(UUID, json!([1, 2]), None, None).unwrap_err(),
            "JSON object",
        ),
        (
            client.drop_player("x player-drop").unwrap_err(),
            "is not a unique id",
        ),
    ];
    for (error, says) in cases {
        assert!(
            matches!(&error, Error::Refused(why) if why.contains(says)),
            "{says}: {error}"
        );
    }
    assert!(running.seen.lock().unwrap().is_empty());
}
