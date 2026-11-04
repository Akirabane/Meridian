//! The sidecar and its client together, over sockets on 127.0.0.1, in front of a
//! stand-in for a Horizon server's control endpoint.
use crate::Error;
use crate::client::SidecarClient;
use crate::endpoint::{Endpoint, fake};
use crate::management::Notices;
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
/// A hand-over id, as an export with freeze gives it.
const HANDOVER: &str = "9f2c4be0a1d34e6b8c0f1e2d3c4b5a69";
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
        Self::start_with(answer, endpoint_timeout, None)
    }

    fn start_with(
        answer: fake::Answer,
        endpoint_timeout: Duration,
        notices: Option<Arc<Notices>>,
    ) -> Self {
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
            notices,
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
        .import(
            UUID,
            json!({ "version": 1, "handover": HANDOVER }),
            None,
            None,
            None,
            None,
        )
        .unwrap();
    client
        .release(UUID, HANDOVER, Outcome::Moved, false)
        .unwrap();
    client.drop_player(UUID, None).unwrap();
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
    // The backend never said no: the request may be asked again, on this connection.
    assert!(
        matches!(&error, Error::Unreached(why) if why.contains("no answer in time")),
        "{error}"
    );
    assert!(error.is_lost());
}

/// An answer for the first command that comes too late (600 ms), and echoes at once
/// after. The endpoint answers one command at a time.
fn late_once() -> fake::Answer {
    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    Arc::new(move |line: &str| {
        if asked.fetch_add(1, Ordering::SeqCst) == 0 {
            std::thread::sleep(Duration::from_millis(600));
        }
        Some(json!({ "echo": line }).to_string())
    })
}

#[test]
fn a_request_the_backend_never_answered_can_be_asked_again_on_the_same_connection() {
    let running = Running::start(late_once(), Duration::from_millis(400));
    let mut client = running.client(SECRET);
    // Nothing was lost yet.
    assert!(!client.recover().unwrap());
    assert!(!client.was_lost());
    assert!(client.export(UUID, true).unwrap_err().is_lost());
    assert!(client.was_lost());
    assert!(client.recover().unwrap());
    let again = client.export(UUID, true).unwrap();
    assert_eq!(
        again,
        json!({ "echo": format!("player-export {DASHED} freeze") })
    );
    // An answer needs no second try.
    assert!(!client.was_lost());
    assert!(!client.recover().unwrap());
}

#[test]
fn a_connection_that_timed_out_is_reopened_to_ask_again() {
    // The backend takes 750 ms to answer the first command; the proxy's side gives up
    // after 500 ms. The endpoint answers one command at a time, so the second waits
    // for the first to end, which is less than the 500 ms it has.
    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let answer: fake::Answer = Arc::new(move |line: &str| {
        if asked.fetch_add(1, Ordering::SeqCst) == 0 {
            std::thread::sleep(Duration::from_millis(750));
        }
        Some(json!({ "echo": line }).to_string())
    });
    let running = Running::start(answer, Duration::from_secs(5));
    let mut client =
        SidecarClient::connect(&running.address, SECRET, Duration::from_millis(500)).unwrap();
    assert!(matches!(client.export(UUID, true), Err(Error::Timeout)));
    // The connection is not used again as it is.
    assert!(client.export(UUID, true).is_err());
    // Recovering opens a new one, with its own challenge and numbers.
    assert!(client.recover().unwrap());
    assert_eq!(
        client.export(UUID, true).unwrap(),
        json!({ "echo": format!("player-export {DASHED} freeze") })
    );
}

#[test]
fn a_refusal_is_an_answer_and_is_not_asked_again() {
    let running = Running::start(
        Arc::new(|_| Some(r#"{"error":"that player is riding"}"#.to_owned())),
        WAIT,
    );
    let mut client = running.client(SECRET);
    let error = client.export(UUID, true).unwrap_err();
    assert!(!error.is_lost(), "{error}");
    assert!(!client.was_lost());
    assert!(!client.recover().unwrap());
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
    assert!(client.drop_player(UUID, None).is_err());
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

// The contract of Horizon's player transfers (docs/server.md, "A proxy moving a
// player between two servers"), with a fake endpoint that answers as it documents.

/// The record of the contract's `player-export` example, for [`DASHED`], with the
/// hand-over id of an export with freeze (`None` for one without: `null`).
fn record(handover: Option<&str>) -> Value {
    json!({
        "version": 1,
        "data_version": 5023,
        "handover": handover,
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

/// What the fake backend holds of the player: on the source, a hand-over under way;
/// on the target, an arrival whose record was imported without placing it, with the
/// announce asked then.
#[derive(Default)]
struct Transfer {
    frozen: Option<String>,
    imported: Option<(String, bool)>,
    /// A player who was placed, or dropped: there is no arrival any more.
    arrived: bool,
}

/// A backend that answers each command as the contract says, from what the line
/// holds and from the state of the transfer, and refuses the lines the contract
/// refuses. It plays the source and the target both, for one player.
fn contract() -> fake::Answer {
    let transfer = Arc::new(Mutex::new(Transfer::default()));
    Arc::new(move |line: &str| {
        let refuse = |why: &str| Some(json!({ "error": why }).to_string());
        let (command, rest) = line.split_once(' ').unwrap_or((line, ""));
        let mut args = rest.split_whitespace();
        if args.next() != Some(DASHED) {
            return refuse("the sidecar sent a unique id Horizon would not write");
        }
        let mut state = transfer.lock().unwrap();
        let reply = match command {
            "player-expect" => match args.next() {
                None => json!({ "ok": true, "renewed": false, "ttl_seconds": 10 }),
                Some("30") => json!({ "ok": true, "renewed": true, "ttl_seconds": 30 }),
                Some(_) => return refuse("the time must be a number of seconds above zero"),
            },
            "player-export" => match (args.next(), args.next()) {
                (None, None) => json!({ "ok": true, "record": record(None) }),
                // A repeated export is the same record, the same id.
                (Some("freeze"), None) => {
                    let id = state.frozen.get_or_insert_with(|| HANDOVER.to_owned());
                    json!({ "ok": true, "record": record(Some(id)) })
                }
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
                let Some(id) = request["record"]["handover"].as_str() else {
                    return refuse("the record has no hand-over id: export it with freeze");
                };
                if state.imported.as_ref().is_some_and(|(have, _)| have != id) {
                    return refuse("another hand-over is under way for that player");
                }
                let parts = request
                    .get("parts")
                    .cloned()
                    .unwrap_or_else(|| json!(ALL_PARTS));
                let announced = request
                    .get("announce")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                let placed = request
                    .get("place")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                if placed {
                    state.imported = None;
                    state.arrived = true;
                } else {
                    state.imported = Some((id.to_owned(), announced));
                }
                json!({ "ok": true, "handover": id, "parts": parts, "placed": placed, "announced": announced })
            }
            "player-confirm" => {
                let Some(id) = args.next() else {
                    return refuse("player-confirm takes a unique id and a hand-over");
                };
                match state.imported.clone() {
                    Some((have, announce)) if have == id => {
                        state.imported = None;
                        state.arrived = true;
                        json!({ "ok": true, "placed": true, "announced": announce })
                    }
                    Some(_) => return refuse("that record is of another hand-over"),
                    None if state.arrived => {
                        return refuse("no player with that unique id is held");
                    }
                    None => {
                        return refuse("that player is held but its record has not been imported");
                    }
                }
            }
            "player-release" => {
                let Some(id) = args.next() else {
                    return refuse("player-release takes a unique id and a hand-over");
                };
                if state.frozen.as_deref() != Some(id) {
                    return refuse("that player is not in that hand-over");
                }
                match (args.next(), args.next()) {
                    (Some("moved"), None) => {
                        state.frozen = None;
                        json!({ "ok": true, "released": "moved", "announced": false })
                    }
                    (Some("moved"), Some("announce")) => {
                        state.frozen = None;
                        json!({ "ok": true, "released": "moved", "announced": true })
                    }
                    (Some("abort"), None) => {
                        state.frozen = None;
                        json!({ "ok": true, "released": "abort" })
                    }
                    _ => {
                        return refuse(
                            "player-release <uuid> <hand-over> is followed by moved [announce] \
                             or abort",
                        );
                    }
                }
            }
            "player-drop" => {
                // With an id, it must be the imported record's.
                if let Some(id) = args.next()
                    && state.imported.as_ref().map(|(have, _)| have.as_str()) != Some(id)
                {
                    return refuse("that is not the imported record's hand-over");
                }
                state.imported = None;
                state.arrived = true;
                json!({ "ok": true, "held": true, "expected": false })
            }
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
    // back exactly as the backend wrote it, with the hand-over id; asked again, it
    // is the same.
    let exported = client.export(UUID, true).unwrap();
    assert_eq!(
        exported,
        json!({ "ok": true, "record": record(Some(HANDOVER)) })
    );
    assert_eq!(client.export(UUID, true).unwrap(), exported);
    // Without a freeze it is a read: no id.
    let read = client.export(UUID, false).unwrap();
    assert_eq!(read["record"]["handover"], json!(null));

    // The record goes to the held player as it is: some parts, quietly...
    let imported = client
        .import(
            UUID,
            exported["record"].clone(),
            Some(vec!["inventory".into(), "experience".into()]),
            Some(false),
            None,
            None,
        )
        .unwrap();
    assert_eq!(
        imported,
        json!({ "ok": true, "handover": HANDOVER, "parts": ["inventory", "experience"], "placed": true, "announced": false })
    );
    // ...or whole, with the default announce.
    let whole = client
        .import(UUID, record(Some(HANDOVER)), None, None, None, None)
        .unwrap();
    assert_eq!(whole["announced"], json!(true));
    assert_eq!(whole["parts"], json!(ALL_PARTS));

    // The old backend lets the player go, or keeps them.
    assert_eq!(
        client
            .release(UUID, HANDOVER, Outcome::Moved, true)
            .unwrap(),
        json!({ "ok": true, "released": "moved", "announced": true })
    );
    // The hand-over is over: a second release has nothing to end.
    assert!(
        client
            .release(UUID, HANDOVER, Outcome::Abort, false)
            .is_err()
    );
    assert_eq!(client.drop_player(UUID, None).unwrap()["ok"], json!(true));

    // The lines are the contract's, word for word.
    let seen = running.seen.lock().unwrap().clone();
    assert_eq!(seen[0], format!("player-expect {DASHED}"));
    assert_eq!(seen[1], format!("player-expect {DASHED} 30"));
    assert_eq!(seen[2], format!("player-export {DASHED} freeze"));
    assert_eq!(seen[3], format!("player-export {DASHED} freeze"));
    assert_eq!(seen[4], format!("player-export {DASHED}"));
    assert!(seen[5].starts_with(&format!("player-import {DASHED} {{")));
    assert!(seen[6].starts_with(&format!("player-import {DASHED} {{")));
    assert_eq!(
        seen[7],
        format!("player-release {DASHED} {HANDOVER} moved announce")
    );
    assert_eq!(seen[8], format!("player-release {DASHED} {HANDOVER} abort"));
    assert_eq!(seen[9], format!("player-drop {DASHED}"));
}

#[test]
fn a_release_names_the_hand_over_of_the_export() {
    let running = Running::start(contract(), WAIT);
    let mut client = running.client(SECRET);
    // Nothing was exported with a freeze, so there is no hand-over to end.
    let error = client
        .release(UUID, HANDOVER, Outcome::Abort, false)
        .unwrap_err();
    assert!(
        matches!(&error, Error::Refused(why) if why.contains("not in that hand-over")),
        "{error}"
    );
    client.export(UUID, true).unwrap();
    // Another id than the export's is refused, and leaves the freeze as it was.
    let other = "00000000000000000000000000000001";
    assert!(client.release(UUID, other, Outcome::Moved, false).is_err());
    assert_eq!(
        client
            .release(UUID, HANDOVER, Outcome::Abort, false)
            .unwrap(),
        json!({ "ok": true, "released": "abort" })
    );
}

#[test]
fn the_backends_refusals_come_back_as_refusals() {
    let running = Running::start(contract(), WAIT);
    let mut client = running.client(SECRET);
    let mut old = record(Some(HANDOVER));
    old["data_version"] = json!(4000);
    let error = client
        .import(UUID, old, None, None, None, None)
        .unwrap_err();
    assert!(
        matches!(&error, Error::Refused(why) if why.contains("another version")),
        "{error}"
    );
    // The connection still serves the next request.
    assert_eq!(client.drop_player(UUID, None).unwrap()["ok"], json!(true));
}

/// A server that has transfers off answers every player request with an error, and
/// the reason comes back as it is.
#[test]
fn a_server_with_transfers_off_refuses_every_request_and_says_so() {
    let running = Running::start(
        Arc::new(|_| Some(r#"{"error":"transfers are off on this server"}"#.to_owned())),
        WAIT,
    );
    let mut client = running.client(SECRET);
    let off =
        |e: Error| matches!(&e, Error::Refused(why) if why == "transfers are off on this server");
    assert!(off(client.expect(UUID, None).unwrap_err()));
    assert!(off(client.export(UUID, true).unwrap_err()));
    assert!(off(client
        .import(UUID, record(Some(HANDOVER)), None, None, Some(false), None)
        .unwrap_err()));
    assert!(off(client.confirm(UUID, HANDOVER).unwrap_err()));
    assert!(off(client
        .release(UUID, HANDOVER, Outcome::Abort, false)
        .unwrap_err()));
    assert!(off(client.drop_player(UUID, None).unwrap_err()));
    // A refusal from the backend leaves the connection usable.
    assert_eq!(running.seen.lock().unwrap().len(), 6);
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
            client
                .release(UUID, HANDOVER, Outcome::Abort, true)
                .unwrap_err(),
            "announce goes with moved",
        ),
        (
            client
                .release(UUID, "moved", Outcome::Abort, false)
                .unwrap_err(),
            "is not a hand-over id",
        ),
        (
            client.confirm(UUID, "").unwrap_err(),
            "is not a hand-over id",
        ),
        (
            client
                .import(UUID, json!([1, 2]), None, None, None, None)
                .unwrap_err(),
            "JSON object",
        ),
        (
            client
                .import(UUID, record(None), None, None, None, None)
                .unwrap_err(),
            "export it with freeze",
        ),
        (
            client.drop_player("x player-drop", None).unwrap_err(),
            "is not a unique id",
        ),
        (
            client
                .drop_player(UUID, Some(&format!("{HANDOVER} moved")))
                .unwrap_err(),
            "is not a hand-over id",
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

#[test]
fn an_import_without_placing_is_confirmed_later() {
    let running = Running::start(contract(), WAIT);
    let mut client = running.client(SECRET);
    // Before any import, there is nothing to confirm.
    let error = client.confirm(UUID, HANDOVER).unwrap_err();
    assert!(
        matches!(&error, Error::Refused(why) if why.contains("has not been imported")),
        "{error}"
    );
    let imported = client
        .import(
            UUID,
            record(Some(HANDOVER)),
            None,
            Some(true),
            Some(false),
            None,
        )
        .unwrap();
    assert_eq!(imported["placed"], json!(false));
    assert_eq!(imported["announced"], json!(true));
    // The confirm names the record that was imported, and no other.
    let other = "00000000000000000000000000000001";
    let error = client.confirm(UUID, other).unwrap_err();
    assert!(
        matches!(&error, Error::Refused(why) if why.contains("another hand-over")),
        "{error}"
    );
    assert_eq!(
        client.confirm(UUID, HANDOVER).unwrap(),
        json!({ "ok": true, "placed": true, "announced": true })
    );
    // The lines are the contract's.
    let seen = running.seen.lock().unwrap().clone();
    let body: Value = serde_json::from_str(seen[1].splitn(3, ' ').nth(2).unwrap()).unwrap();
    assert_eq!(body["place"], json!(false));
    assert_eq!(body["record"]["handover"], json!(HANDOVER));
    assert_eq!(seen[3], format!("player-confirm {DASHED} {HANDOVER}"));
}

#[test]
fn a_dropped_arrival_cannot_be_confirmed() {
    let running = Running::start(contract(), WAIT);
    let mut client = running.client(SECRET);
    client
        .import(UUID, record(Some(HANDOVER)), None, None, Some(false), None)
        .unwrap();
    // With an id, only the imported record's arrival is dropped.
    let other = "00000000000000000000000000000001";
    assert!(client.drop_player(UUID, Some(other)).is_err());
    assert_eq!(
        client.drop_player(UUID, Some(HANDOVER)).unwrap(),
        json!({ "ok": true, "held": true, "expected": false })
    );
    let error = client.confirm(UUID, HANDOVER).unwrap_err();
    assert!(
        matches!(&error, Error::Refused(why) if why.contains("no player with that unique id is held")),
        "{error}"
    );
    let seen = running.seen.lock().unwrap().clone();
    assert_eq!(seen[2], format!("player-drop {DASHED} {HANDOVER}"));
}

/// The state of the backend goes through the sidecar and the signed exchange: one
/// line to the backend, which names no player, and the answer is read as a status.
#[test]
fn the_state_of_the_backend_comes_back_as_a_status() {
    use crate::request::{STATUS_LINE, ServerStatus};
    let running = Running::start(
        Arc::new(|line| {
            (line == STATUS_LINE).then(|| {
                json!({"players": 4, "capacity": 12, "accepts_transfers": true, "activity": "busy"})
                    .to_string()
            })
        }),
        WAIT,
    );
    let mut client = running.client(SECRET);
    assert_eq!(
        client.status().unwrap(),
        ServerStatus {
            players: Some(4),
            capacity: Some(12),
            accepts_transfers: Some(true),
        }
    );
    // The same connection can ask again, and nothing but the one line was sent.
    client.status().unwrap();
    assert_eq!(*running.seen.lock().unwrap(), [STATUS_LINE, STATUS_LINE]);
}

#[test]
fn a_backend_that_does_not_know_the_status_is_an_error_not_a_status() {
    let running = Running::start(
        Arc::new(|_| Some(json!({"error": "unknown command"}).to_string())),
        WAIT,
    );
    let error = running.client(SECRET).status().unwrap_err();
    assert!(matches!(error, Error::Refused(_)), "{error}");
}

#[test]
fn an_answer_that_is_not_a_status_is_an_error() {
    let running = Running::start(
        Arc::new(|_| Some(json!({"players": "many"}).to_string())),
        WAIT,
    );
    let error = running.client(SECRET).status().unwrap_err();
    assert!(
        error.to_string().contains("the status is not usable"),
        "{error}"
    );
}

#[test]
fn a_status_needs_the_secret_like_every_request() {
    let running = Running::start(Arc::new(|_| Some("{}".to_owned())), WAIT);
    let mut client = running.client(b"another secret that is long enough");
    assert!(client.status().is_err());
    assert!(running.seen.lock().unwrap().is_empty());
}

const STOPPING: &str = r#"{"jsonrpc":"2.0","method":"minecraft:notification/server/stopping"}"#;

/// A sidecar that does not listen to the management protocol says so, and never asks the
/// backend: the proxy can tell a sidecar with no notices from one that heard none.
#[test]
fn a_sidecar_that_does_not_listen_says_so_and_asks_the_backend_nothing() {
    let running = Running::start(echo(), WAIT);
    let mut client = running.client(SECRET);
    let report = client.notices(None, Duration::from_millis(50)).unwrap();
    assert!(!report.enabled && !report.watching);
    assert_eq!((report.last, report.notices.len()), (0, 0));
    assert!(running.seen.lock().unwrap().is_empty());
}

/// A client asks where the count stands, then for what comes after: the notice that the
/// sidecar hears while the request waits is in its answer, on the signed wire.
#[test]
fn the_notices_the_sidecar_hears_reach_the_proxy_after_the_number_it_has() {
    let notices = Arc::new(Notices::default());
    notices.set_watching(true);
    let running = Running::start_with(echo(), WAIT, Some(Arc::clone(&notices)));
    let mut client = running.client(SECRET);
    let start = client.notices(None, Duration::ZERO).unwrap();
    assert!(start.enabled && start.watching);
    assert_eq!((start.last, start.notices.len()), (0, 0));
    // Nothing yet: the answer comes after the wait, empty.
    let begun = std::time::Instant::now();
    let none = client.notices(Some(0), Duration::from_millis(200)).unwrap();
    assert!(begun.elapsed() >= Duration::from_millis(190));
    assert!(none.notices.is_empty());
    // One that comes during the wait ends it.
    let hearing = {
        let notices = Arc::clone(&notices);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            notices.offer(STOPPING);
        })
    };
    let heard = client.notices(Some(0), Duration::from_secs(5)).unwrap();
    hearing.join().unwrap();
    assert_eq!(heard.notices.len(), 1);
    assert_eq!(
        (heard.notices[0].seq, heard.notices[0].line.as_str()),
        (1, STOPPING)
    );
    // Asked again with the number it has, it hears nothing more.
    let again = client.notices(Some(1), Duration::ZERO).unwrap();
    assert_eq!((again.last, again.notices.len()), (1, 0));
    // It never reached the backend.
    assert!(running.seen.lock().unwrap().is_empty());
}

#[test]
fn a_notices_request_takes_no_other_field() {
    use crate::request::Request;
    let ok: Request = serde_json::from_str(r#"{"op":"notices","after":3,"wait_ms":500}"#).unwrap();
    assert_eq!(
        ok,
        Request::Notices {
            after: Some(3),
            wait_ms: Some(500)
        }
    );
    assert!(serde_json::from_str::<Request>(r#"{"op":"notices","uuid":"x"}"#).is_err());
    assert_eq!(
        serde_json::to_string(&Request::Notices {
            after: None,
            wait_ms: None
        })
        .unwrap(),
        r#"{"op":"notices"}"#
    );
}
