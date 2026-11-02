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
    client.expect(UUID, "Alex").unwrap();
    let exported = client.export(UUID).unwrap();
    assert_eq!(
        exported,
        json!({ "echo": format!(r#"player-export {{"uuid":"{UUID}"}}"#) })
    );
    client.import(UUID, json!({ "health": 20.0 })).unwrap();
    client.release(UUID, Outcome::Moved).unwrap();
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
    let error = client.export(UUID).unwrap_err();
    assert!(
        matches!(&error, Error::Refused(why) if why.contains("signature")),
        "{error}"
    );
    assert!(running.seen.lock().unwrap().is_empty());
    // The connection is not used again.
    assert!(client.export(UUID).is_err());
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
    let error = client.export(UUID).unwrap_err();
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
    assert!(matches!(client.export(UUID), Err(Error::Timeout)));
    mute.join().unwrap();
}

#[test]
fn a_backend_answer_that_is_not_json_is_an_error() {
    let running = Running::start(Arc::new(|_| Some("not json".to_owned())), WAIT);
    let mut client = running.client(SECRET);
    let error = client.export(UUID).unwrap_err().to_string();
    assert!(error.contains("did not answer with JSON"), "{error}");
}

#[test]
fn a_backend_error_is_passed_on() {
    let running = Running::start(
        Arc::new(|_| Some(r#"{"error":"no such player"}"#.to_owned())),
        WAIT,
    );
    let mut client = running.client(SECRET);
    let error = client.export(UUID).unwrap_err();
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
    let error = client.export(UUID).unwrap_err();
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
