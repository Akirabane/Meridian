//! The connection against a fake extension: a program in a thread that speaks the protocol with the
//! crates' own server code, and can be told to misbehave. Nothing here starts a proxy.

use super::connection::{loopback_addresses, read_token};
use super::link::{CallError, Timings};
use super::stats::Outcome;
use super::*;
use meridian_extension::handshake::{self, Nonce, Secret};
use meridian_extension::wire::{
    self, CommandSpec, Confirm, Discover, DiscoverMethod, Hello, HelloAnswer, Message, RpcError,
    SUBPROTOCOL,
};
use meridian_websocket::{Next, Socket, Writer, close};
use serde_json::{Value, json};
use std::io::Write as _;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const TOKEN: &[u8] = b"fake-extension-token-0123456789abcdef";
const METHOD: &str = "meridian:test/ping";

/// How the fake answers `meridian:test/ping`.
#[derive(Clone, Debug)]
enum Answer {
    /// `{"pong": <params>}`.
    Echo,
    /// Nothing.
    Silent,
    /// The echo, after a while.
    Slow(Duration),
    /// A JSON-RPC error.
    Error,
    /// A result that is not what the method defines.
    Garbage,
}

#[derive(Clone)]
struct Script {
    token: Vec<u8>,
    versions: Vec<u32>,
    capabilities: Vec<String>,
    bad_proof: bool,
    reject_confirm: bool,
    request_before_confirm: bool,
    big_message: bool,
    /// Sends, once the hello is done, a frame that announces 60000 bytes and then one byte every 20 ms.
    drip: bool,
    /// Reads nothing for this long once the hello is done, as an extension that is stuck would.
    deaf: Option<Duration>,
    /// How many of the connections (after the ones dropped) are deaf; the others read.
    deaf_first: usize,
    /// Connections to drop as soon as they are accepted.
    drop_first: usize,
}

impl Default for Script {
    fn default() -> Self {
        Self {
            token: TOKEN.to_vec(),
            versions: vec![0],
            capabilities: vec!["decide.login".into(), "orders.read".into(), "events".into()],
            bad_proof: false,
            reject_confirm: false,
            request_before_confirm: false,
            big_message: false,
            drip: false,
            deaf: None,
            deaf_first: usize::MAX,
            drop_first: 0,
        }
    }
}

/// What the fake saw.
#[derive(Default)]
struct Seen {
    connected_at: Vec<Instant>,
    hellos: Vec<Hello>,
    confirms: usize,
    /// The methods the proxy asked after the hello.
    requests: Vec<String>,
    /// Every text the proxy sent, as it came.
    texts: Vec<String>,
    close_codes: Vec<Option<u16>>,
}

struct Fake {
    address: SocketAddr,
    seen: Arc<Mutex<Seen>>,
    answer: Arc<Mutex<Answer>>,
    current: Arc<Mutex<Option<Writer>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Fake {
    fn start(script: Script) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Seen::default()));
        let answer = Arc::new(Mutex::new(Answer::Echo));
        let current = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let dropped = Arc::new(AtomicUsize::new(0));
        let thread = {
            let (seen, answer, current, stop) = (
                Arc::clone(&seen),
                Arc::clone(&answer),
                Arc::clone(&current),
                Arc::clone(&stop),
            );
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let _ = stream.set_nonblocking(false);
                            seen.lock().unwrap().connected_at.push(Instant::now());
                            let index = dropped.fetch_add(1, Ordering::Relaxed);
                            if index < script.drop_first {
                                continue;
                            }
                            let mut script = script.clone();
                            if index - script.drop_first >= script.deaf_first {
                                script.deaf = None;
                            }
                            let (script, seen, answer, current, stop) = (
                                script,
                                Arc::clone(&seen),
                                Arc::clone(&answer),
                                Arc::clone(&current),
                                Arc::clone(&stop),
                            );
                            std::thread::spawn(move || {
                                handle(stream, &script, &seen, &answer, &current, &stop);
                            });
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(2)),
                    }
                }
            })
        };
        Self {
            address,
            seen,
            answer,
            current,
            stop,
            thread: Some(thread),
        }
    }

    fn answer_with(&self, answer: Answer) {
        *self.answer.lock().unwrap() = answer;
    }

    fn seen<T>(&self, look: impl FnOnce(&Seen) -> T) -> T {
        look(&self.seen.lock().unwrap())
    }

    /// Sends a text to the proxy on the connection that is up.
    fn send(&self, text: &str) {
        let writer = self.current.lock().unwrap().clone().expect("a connection");
        writer.send_text(text).unwrap();
    }

    /// Ends the connection that is up, as a crash of the extension would.
    fn drop_connection(&self) {
        if let Some(writer) = self.current.lock().unwrap().take() {
            let _ = writer.shutdown();
        }
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.drop_connection();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The next text the proxy sent, or `None` when the connection ended or the fake is stopped.
fn next_text(
    socket: &mut Socket<TcpStream>,
    seen: &Mutex<Seen>,
    stop: &AtomicBool,
) -> Option<String> {
    loop {
        if stop.load(Ordering::Relaxed) {
            return None;
        }
        match socket.read_message() {
            Ok(Next::Text(text)) => {
                seen.lock().unwrap().texts.push(text.clone());
                return Some(text);
            }
            Ok(Next::Idle) => {}
            Ok(Next::Closed) => {
                seen.lock().unwrap().close_codes.push(socket.close_code());
                return None;
            }
            Err(_) => return None,
        }
    }
}

fn handle(
    stream: TcpStream,
    script: &Script,
    seen: &Mutex<Seen>,
    answer: &Mutex<Answer>,
    current: &Mutex<Option<Writer>>,
    stop: &AtomicBool,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(50)));
    // A larger limit than the proxy's, so that the fake can send what the proxy must refuse.
    let Ok(mut socket) = Socket::accept(stream, 1 << 20, |upgrade| {
        assert_eq!(upgrade.protocols(), [SUBPROTOCOL]);
        Ok(Some(SUBPROTOCOL.to_owned()))
    }) else {
        return;
    };
    let Some(text) = next_text(&mut socket, seen, stop) else {
        return;
    };
    let Ok(Message::Request {
        id: Some(id),
        params,
        ..
    }) = Message::parse(&text)
    else {
        return;
    };
    let hello: Hello = wire::decode(params.as_ref()).unwrap();
    seen.lock().unwrap().hellos.push(hello.clone());
    let Some(version) = hello
        .protocol
        .iter()
        .copied()
        .filter(|v| script.versions.contains(v))
        .max()
    else {
        let _ = socket.send_text(&wire::error(
            Some(&id),
            &RpcError::version_mismatch(&script.versions),
        ));
        while let Some(_text) = next_text(&mut socket, seen, stop) {}
        return;
    };
    let proxy_nonce = Nonce::from_hex(&hello.proxy_nonce).unwrap();
    let extension_nonce = Nonce::random().unwrap();
    let proving = if script.bad_proof {
        Secret::new(b"another token, which is long enough too".to_vec()).unwrap()
    } else {
        Secret::new(script.token.clone()).unwrap()
    };
    let reply = HelloAnswer {
        protocol: version,
        extension_nonce: extension_nonce.to_hex(),
        proof: handshake::extension_proof_hex(&proving, &proxy_nonce, &extension_nonce),
        name: "fake".into(),
        version: "1.2.3".into(),
        capabilities: script.capabilities.clone(),
        commands: vec![CommandSpec {
            name: "party".into(),
            aliases: vec![],
            usage: "/party".into(),
        }],
    };
    let _ = socket.send_text(&wire::result(&id, &reply));
    if script.request_before_confirm {
        let _ = socket.send_text(&wire::request(900, "meridian:players/list", None::<&Value>));
    }
    let Some(text) = next_text(&mut socket, seen, stop) else {
        return;
    };
    let Ok(Message::Request {
        id: Some(confirm_id),
        params,
        ..
    }) = Message::parse(&text)
    else {
        return;
    };
    let confirm: Confirm = wire::decode(params.as_ref()).unwrap();
    let checking = Secret::new(script.token.clone()).unwrap();
    if script.reject_confirm
        || !handshake::check_proxy_proof(&checking, &proxy_nonce, &extension_nonce, &confirm.proof)
    {
        let _ = socket.close(close::POLICY, "");
        while let Some(_text) = next_text(&mut socket, seen, stop) {}
        return;
    }
    seen.lock().unwrap().confirms += 1;
    let _ = socket.send_text(&wire::result(&confirm_id, &json!({})));
    let raw = socket.stream_mut().try_clone();
    let Ok((mut reader, writer)) = socket.split() else {
        return;
    };
    *current.lock().unwrap() = Some(writer.clone());
    if script.drip
        && let Ok(mut raw) = raw
    {
        std::thread::spawn(move || {
            // A frame of a server: not masked.
            let mut head = vec![0x81, 126];
            head.extend_from_slice(&60_000u16.to_be_bytes());
            if raw.write_all(&head).is_err() {
                return;
            }
            for _ in 0..150 {
                if raw.write_all(b"x").is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
    }
    if script.big_message {
        // Over the proxy's limit (256 KiB) by far, so that most of it is still unread when the proxy
        // closes: a close over unread bytes is a reset, which can take the close frame with it.
        let _ = writer.send_text(&"x".repeat(900 << 10));
        // An extension that is busy for a moment before it reads again: the close frame waits unread
        // in its socket, where a reset from the proxy would take it away.
        std::thread::sleep(Duration::from_millis(200));
    }
    if let Some(deaf) = script.deaf {
        let until = Instant::now() + deaf;
        while Instant::now() < until && !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    while !stop.load(Ordering::Relaxed) {
        let text = match reader.read_message() {
            Ok(Next::Text(text)) => text,
            Ok(Next::Idle) => continue,
            Ok(Next::Closed) => {
                seen.lock().unwrap().close_codes.push(reader.close_code());
                return;
            }
            Err(_) => return,
        };
        seen.lock().unwrap().texts.push(text.clone());
        let Ok(Message::Request {
            id: Some(id),
            method,
            params,
        }) = Message::parse(&text)
        else {
            continue;
        };
        seen.lock().unwrap().requests.push(method.clone());
        if wire::method::is_discover(&method) {
            let document = Discover::new(
                0,
                vec![DiscoverMethod::with_request(
                    "meridian:decide/login",
                    "object",
                )],
            );
            let _ = writer.send_text(&wire::result(&id, &document));
            continue;
        }
        let how = answer.lock().unwrap().clone();
        match how {
            Answer::Echo => {
                let _ = writer.send_text(&wire::result(&id, &json!({ "pong": params })));
            }
            Answer::Silent => {}
            Answer::Slow(wait) => {
                let writer = writer.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(wait);
                    let _ = writer.send_text(&wire::result(&id, &json!({ "pong": params })));
                });
            }
            Answer::Error => {
                let _ = writer.send_text(&wire::error(
                    Some(&id),
                    &RpcError::new(wire::code::INVALID_PARAMS, "bad request"),
                ));
            }
            Answer::Garbage => {
                let _ = writer.send_text(&wire::result(&id, &json!({ "unexpected": true })));
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------

static FOLDERS: AtomicUsize = AtomicUsize::new(0);

/// A folder for the token file, removed at the end of the test.
struct Folder(PathBuf);

impl Folder {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "meridian-extension-test-{}-{}",
            std::process::id(),
            FOLDERS.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn token(&self, token: &[u8]) -> PathBuf {
        let path = self.0.join("token");
        std::fs::write(&path, token).unwrap();
        path
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn timings() -> Timings {
    Timings {
        connect: Duration::from_secs(1),
        hello: Duration::from_secs(2),
        write: Duration::from_secs(2),
        message: Duration::from_millis(300),
        look: Duration::from_millis(20),
        first_wait: Duration::from_millis(20),
        longest_wait: Duration::from_millis(160),
    }
}

fn settings(fake: &Fake, folder: &Folder) -> Settings {
    let mut settings = Settings::new("perms", fake.address.to_string(), folder.token(TOKEN));
    settings.grant = vec!["decide.login".into(), "orders.read".into()];
    settings.timings = timings();
    settings
}

fn until(what: &str, mut ready: impl FnMut() -> bool) {
    let limit = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < limit, "gave up waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn ping(link: &Link, timeout: Duration) -> Result<Value, CallError> {
    link.call(METHOD, Some(&json!({ "n": 1 })), timeout, |result| {
        result
            .get("pong")
            .cloned()
            .ok_or_else(|| "no pong in the result".to_owned())
    })
}

/// A link to the fake with `change` applied to its settings, and the registry that keeps it.
fn connect_with(fake: &Fake, change: impl FnOnce(&mut Settings)) -> (Registry, Arc<Link>, Folder) {
    let folder = Folder::new();
    let mut settings = settings(fake, &folder);
    change(&mut settings);
    let registry = Registry::default();
    let link = registry.start(settings).unwrap();
    (registry, link, folder)
}

fn connect(fake: &Fake) -> (Registry, Arc<Link>, Folder) {
    let (registry, link, folder) = connect_with(fake, |_| {});
    until("the connection", || link.is_connected());
    (registry, link, folder)
}

// ---------------------------------------------------------------------------------------------

#[test]
fn the_hello_is_done_and_what_the_extension_declared_is_split_by_the_grant() {
    let fake = Fake::start(Script::default());
    let (_registry, link, _folder) = connect(&fake);
    let info = link.info().unwrap();
    assert_eq!(
        (info.protocol, info.name.as_str(), info.version.as_str()),
        (0, "fake", "1.2.3")
    );
    assert_eq!(info.granted, ["decide.login", "orders.read"]);
    assert_eq!(info.refused, ["events"]);
    assert_eq!(info.commands[0].name, "party");
    assert!(link.stats().is_connected());
    fake.seen(|seen| {
        let hello = &seen.hellos[0];
        assert_eq!(hello.protocol, [0]);
        assert_eq!(hello.proxy, env!("CARGO_PKG_VERSION"));
        assert_eq!(hello.proxy_nonce.len(), 64);
        assert_eq!(seen.confirms, 1);
    });
}

#[test]
fn a_call_gets_its_answer_and_is_counted() {
    let fake = Fake::start(Script::default());
    let (_registry, link, _folder) = connect(&fake);
    assert_eq!(
        ping(&link, Duration::from_secs(2)).unwrap(),
        json!({ "n": 1 })
    );
    assert_eq!(link.stats().count(METHOD, Outcome::Answered), 1);
    fake.seen(|seen| assert_eq!(seen.requests, [METHOD]));
}

#[test]
fn a_wrong_token_on_the_extensions_side_is_refused_and_nothing_follows() {
    let fake = Fake::start(Script {
        bad_proof: true,
        ..Script::default()
    });
    let (_registry, link, _folder) = connect_with(&fake, |_| {});
    // The proxy closes with 1008, and says no more.
    until("the close", || {
        fake.seen(|s| s.close_codes.contains(&Some(close::POLICY)))
    });
    fake.seen(|seen| {
        assert_eq!(seen.confirms, 0);
        // The hello is the only text the proxy sent.
        assert_eq!(seen.texts.len(), 1, "{:?}", seen.texts);
    });
    assert!(!link.is_connected());
    // It will not try again before the longest wait.
    until("the wait", || link.last_wait() == timings().longest_wait);
    assert_eq!(
        ping(&link, Duration::from_millis(50)),
        Err(CallError::Disconnected)
    );
}

#[test]
fn a_wrong_token_on_the_proxys_side_is_a_token_file_the_extension_does_not_share() {
    // The proxy holds another token than the extension: the extension's proof is the one that fails.
    let fake = Fake::start(Script::default());
    let (_registry, link, _folder) = connect_with(&fake, |settings| {
        std::fs::write(
            &settings.token_file,
            b"a different token, long enough: 0123456789",
        )
        .unwrap();
    });
    until("the close", || {
        fake.seen(|s| s.close_codes.contains(&Some(close::POLICY)))
    });
    assert!(!link.is_connected());
    fake.seen(|seen| assert_eq!(seen.confirms, 0));
}

#[test]
fn an_extension_that_does_not_accept_the_proxys_proof_is_refused_the_same_way() {
    let fake = Fake::start(Script {
        reject_confirm: true,
        ..Script::default()
    });
    let (_registry, link, _folder) = connect_with(&fake, |_| {});
    until("the longest wait", || {
        link.last_wait() == timings().longest_wait
    });
    assert!(!link.is_connected());
}

#[test]
fn no_version_in_common_is_refused_and_waits_the_longest() {
    let fake = Fake::start(Script {
        versions: vec![1, 2],
        ..Script::default()
    });
    let (_registry, link, _folder) = connect_with(&fake, |_| {});
    until("the longest wait", || {
        link.last_wait() == timings().longest_wait
    });
    assert!(!link.is_connected());
    fake.seen(|seen| {
        assert_eq!(seen.hellos[0].protocol, [0]);
        // No confirmation, no proof: nothing but the hello.
        assert_eq!(seen.confirms, 0);
        assert_eq!(seen.texts.len(), 1);
    });
}

#[test]
fn a_request_from_the_extension_before_the_hello_is_done_ends_the_hello() {
    let fake = Fake::start(Script {
        request_before_confirm: true,
        ..Script::default()
    });
    let (_registry, link, _folder) = connect_with(&fake, |_| {});
    until("the close", || {
        fake.seen(|s| s.close_codes.contains(&Some(close::POLICY)))
    });
    assert!(!link.is_connected());
}

#[test]
fn the_connection_is_opened_again_after_a_wait_that_doubles_up_to_the_longest() {
    let fake = Fake::start(Script {
        drop_first: 6,
        ..Script::default()
    });
    let (_registry, link, _folder) = connect_with(&fake, |_| {});
    until("the connection", || link.is_connected());
    let times = fake.seen(|s| s.connected_at.clone());
    assert_eq!(times.len(), 7);
    let gaps: Vec<Duration> = times.windows(2).map(|w| w[1] - w[0]).collect();
    // 20, 40, 80, 160, 160, 160 ms: never less than the wait, and never much more than the longest.
    for (gap, wait) in gaps.iter().zip([20, 40, 80, 160, 160, 160]) {
        assert!(*gap >= Duration::from_millis(wait), "{gaps:?}");
        assert!(
            *gap < Duration::from_millis(wait) + Duration::from_secs(1),
            "{gaps:?}"
        );
    }
    // After it was up, a failure starts again from the first wait.
    fake.drop_connection();
    until("the end", || !link.is_connected());
    until("the connection again", || {
        link.is_connected() && fake.seen(|s| s.connected_at.len()) == 8
    });
    assert_eq!(link.last_wait(), timings().first_wait);
}

#[test]
fn a_deadline_that_passes_is_a_timeout_and_the_late_answer_is_dropped_and_counted() {
    let fake = Fake::start(Script::default());
    fake.answer_with(Answer::Slow(Duration::from_millis(300)));
    let (_registry, link, _folder) = connect(&fake);
    assert_eq!(
        ping(&link, Duration::from_millis(100)),
        Err(CallError::Timeout)
    );
    assert_eq!(link.stats().count(METHOD, Outcome::Timeout), 1);
    until("the late answer", || link.stats().late() == 1);
}

#[test]
fn an_error_and_an_answer_of_the_wrong_shape_are_invalid() {
    let fake = Fake::start(Script::default());
    let (_registry, link, _folder) = connect(&fake);
    fake.answer_with(Answer::Error);
    let Err(CallError::Invalid(why)) = ping(&link, Duration::from_secs(2)) else {
        panic!("an error answer is invalid");
    };
    assert!(
        why.contains("-32602") && !why.contains("bad request"),
        "{why}"
    );
    fake.answer_with(Answer::Garbage);
    let Err(CallError::Invalid(why)) = ping(&link, Duration::from_secs(2)) else {
        panic!("an answer of the wrong shape is invalid");
    };
    assert!(!why.contains("unexpected"), "{why}");
    assert_eq!(link.stats().count(METHOD, Outcome::Invalid), 2);
}

#[test]
fn no_more_requests_wait_than_max_in_flight_and_the_next_does_not_wait_for_a_place() {
    let fake = Fake::start(Script::default());
    fake.answer_with(Answer::Silent);
    let (_registry, link, _folder) = connect_with(&fake, |settings| settings.max_in_flight = 2);
    until("the connection", || link.is_connected());
    let waiting: Vec<_> = (0..2)
        .map(|_| {
            let link = Arc::clone(&link);
            std::thread::spawn(move || ping(&link, Duration::from_millis(600)))
        })
        .collect();
    until("both requests", || fake.seen(|s| s.requests.len()) == 2);
    let started = Instant::now();
    assert_eq!(ping(&link, Duration::from_secs(5)), Err(CallError::Busy));
    assert!(started.elapsed() < Duration::from_millis(300));
    assert_eq!(link.stats().count(METHOD, Outcome::Busy), 1);
    // Busy is not a failure: the two that wait time out, and only they count.
    for thread in waiting {
        assert_eq!(thread.join().unwrap(), Err(CallError::Timeout));
    }
    // The places are free again.
    fake.answer_with(Answer::Echo);
    assert!(ping(&link, Duration::from_secs(2)).is_ok());
}

#[test]
fn the_breaker_opens_after_failures_in_a_row_sends_nothing_while_open_and_closes_on_an_answer() {
    let fake = Fake::start(Script::default());
    fake.answer_with(Answer::Silent);
    let (_registry, link, _folder) = connect_with(&fake, |settings| {
        settings.breaker_failures = 2;
        settings.breaker_open = Duration::from_millis(300);
    });
    until("the connection", || link.is_connected());
    for _ in 0..2 {
        assert_eq!(
            ping(&link, Duration::from_millis(30)),
            Err(CallError::Timeout)
        );
    }
    let mut page = String::new();
    link.stats().set_breaker_open(true);
    stats::render(&[("perms".into(), Arc::clone(link.stats()))], &mut page);
    assert!(page.contains("meridian_extension_breaker_open{extension=\"perms\"} 1"));
    // Open: no request goes.
    assert_eq!(
        ping(&link, Duration::from_secs(1)),
        Err(CallError::BreakerOpen)
    );
    assert_eq!(fake.seen(|s| s.requests.len()), 2);
    assert_eq!(link.stats().count(METHOD, Outcome::Breaker), 1);
    // After its time one request is the trial, and an answer closes it.
    fake.answer_with(Answer::Echo);
    std::thread::sleep(Duration::from_millis(350));
    assert!(ping(&link, Duration::from_secs(2)).is_ok());
    assert!(ping(&link, Duration::from_secs(2)).is_ok());
    assert_eq!(fake.seen(|s| s.requests.len()), 4);
}

/// A call of the kind a command of the extension's own makes: it counts for nothing.
fn uncounted(link: &Link, timeout: Duration) -> Result<Value, CallError> {
    link.call_counting(
        METHOD,
        Some(&json!({ "n": 1 })),
        timeout,
        |result| {
            result
                .get("pong")
                .cloned()
                .ok_or_else(|| "no pong in the result".to_owned())
        },
        false,
    )
}

#[test]
fn an_answer_to_a_call_that_counts_for_nothing_does_not_hide_the_failures_of_the_others() {
    let fake = Fake::start(Script::default());
    let (_registry, link, _folder) = connect_with(&fake, |settings| {
        settings.breaker_failures = 4;
        settings.breaker_open = Duration::from_secs(30);
    });
    until("the connection", || link.is_connected());
    // Four decisions that time out, and between them commands that answer: the answers do not reset the
    // count of failures in a row, so the fourth opens the breaker (an extension whose logins time out and
    // whose commands answer must not keep its breaker closed for ever).
    for round in 0..4 {
        fake.answer_with(Answer::Silent);
        assert_eq!(
            ping(&link, Duration::from_millis(30)),
            Err(CallError::Timeout),
            "decision {round}"
        );
        if round < 3 {
            fake.answer_with(Answer::Echo);
            assert!(uncounted(&link, Duration::from_secs(2)).is_ok());
        }
    }
    let asked = fake.seen(|s| s.requests.len());
    assert_eq!(
        ping(&link, Duration::from_secs(1)),
        Err(CallError::BreakerOpen)
    );
    // Open, it refuses a command too, with no request; and never takes its turn as the trial.
    assert_eq!(
        uncounted(&link, Duration::from_secs(1)),
        Err(CallError::BreakerOpen)
    );
    assert_eq!(fake.seen(|s| s.requests.len()), asked);
    assert_eq!(link.stats().count(METHOD, Outcome::Breaker), 2);
}

#[test]
fn the_failures_of_a_call_that_counts_for_nothing_are_not_the_breakers() {
    let fake = Fake::start(Script::default());
    let (_registry, link, _folder) = connect_with(&fake, |settings| settings.breaker_failures = 2);
    until("the connection", || link.is_connected());
    // Deadlines that pass, then answers that are not what the method defines (a command's lines that are
    // too many or hold a control character are such answers): more than the breaker allows.
    fake.answer_with(Answer::Silent);
    for _ in 0..3 {
        assert_eq!(
            uncounted(&link, Duration::from_millis(30)),
            Err(CallError::Timeout)
        );
    }
    fake.answer_with(Answer::Garbage);
    for _ in 0..3 {
        assert!(matches!(
            uncounted(&link, Duration::from_secs(2)),
            Err(CallError::Invalid(_))
        ));
    }
    // The breaker is closed: a decision goes, and is answered.
    fake.answer_with(Answer::Echo);
    assert!(ping(&link, Duration::from_secs(2)).is_ok());
    assert_eq!(link.stats().count(METHOD, Outcome::Timeout), 3);
    assert_eq!(link.stats().count(METHOD, Outcome::Invalid), 3);
    assert_eq!(link.stats().count(METHOD, Outcome::Breaker), 0);
}

#[test]
fn a_broken_connection_ends_the_call_that_waits_opens_the_breaker_and_the_next_hello_closes_it() {
    let fake = Fake::start(Script::default());
    fake.answer_with(Answer::Silent);
    let (_registry, link, _folder) = connect(&fake);
    let waiting = {
        let link = Arc::clone(&link);
        std::thread::spawn(move || ping(&link, Duration::from_secs(30)))
    };
    until("the request", || fake.seen(|s| s.requests.len()) == 1);
    fake.drop_connection();
    // It does not wait for its 30 seconds.
    assert_eq!(waiting.join().unwrap(), Err(CallError::Disconnected));
    assert_eq!(link.stats().count(METHOD, Outcome::Disconnected), 1);
    // The next hello: connected again.
    fake.answer_with(Answer::Echo);
    until("the connection again", || {
        link.is_connected() && fake.seen(|s| s.confirms) == 2
    });
    assert!(ping(&link, Duration::from_secs(2)).is_ok());
}

#[test]
fn the_extensions_own_requests_are_answered_with_the_id_they_came_with() {
    let fake = Fake::start(Script::default());
    let (_registry, _link, _folder) = connect(&fake);
    fake.send(r#"{"jsonrpc":"2.0","id":"abc-1","method":"meridian:rpc.discover"}"#);
    fake.send(r#"{"jsonrpc":"2.0","id":7,"method":"meridian:nothing/here"}"#);
    fake.send(r#"{"jsonrpc":"2.0","method":"meridian:nothing/here"}"#);
    until("both answers", || {
        fake.seen(|s| {
            s.texts
                .iter()
                .any(|t| t.contains(r#""id":"abc-1""#) && t.contains("methods"))
                && s.texts
                    .iter()
                    .any(|t| t.contains(r#""id":7"#) && t.contains("-32601"))
        })
    });
    // The notification, with no id, got no answer: two answers, no more.
    std::thread::sleep(Duration::from_millis(100));
    let answers = fake.seen(|s| {
        s.texts
            .iter()
            .filter(|t| t.contains(r#""result""#) || t.contains(r#""error""#))
            .count()
    });
    assert_eq!(answers, 2);
}

#[test]
fn a_message_over_the_limit_is_refused_with_the_code_for_a_message_too_big() {
    let fake = Fake::start(Script {
        big_message: true,
        ..Script::default()
    });
    let (_registry, _link, _folder) = connect_with(&fake, |_| {});
    until("the close", || {
        fake.seen(|s| s.close_codes.contains(&Some(close::TOO_BIG)))
    });
}

/// The reader of the connection has a time for a message, from its first byte to its last: an extension
/// that sends one a byte at a time is let go at that time and the proxy connects again, instead of
/// waiting on it for as long as it keeps sending.
#[test]
fn an_extension_that_sends_a_message_a_byte_at_a_time_is_let_go_and_the_proxy_connects_again() {
    let fake = Fake::start(Script {
        drip: true,
        ..Script::default()
    });
    let (_registry, _link, _folder) = connect_with(&fake, |_| {});
    until("the second connection", || {
        fake.seen(|s| s.connected_at.len() >= 2)
    });
    let gap = fake.seen(|s| s.connected_at[1] - s.connected_at[0]);
    // The drip lasts three seconds or more; the message time is 300 ms.
    assert!(gap < Duration::from_millis(1500), "{gap:?}");
}

/// An extension that has stopped reading must not hold the threads of the players who ask it, one
/// after the other, each for the time of a write: the requests go through one thread that writes, and
/// the calls wait for their answers at their own deadlines.
#[test]
fn an_extension_that_does_not_read_holds_no_player_for_longer_than_the_deadline() {
    let fake = Fake::start(Script {
        deaf: Some(Duration::from_secs(60)),
        ..Script::default()
    });
    // A write that blocks may take 1.5 s before it gives up, which the old way paid one thread after
    // the other.
    let (_registry, link, _folder) = connect_with(&fake, |s| {
        s.timings.write = Duration::from_millis(1500);
        s.max_in_flight = 64;
    });
    until("the connection", || link.is_connected());
    let big = json!({ "filler": "x".repeat(200 << 10) });
    let began = Instant::now();
    let players: Vec<_> = (0..8)
        .map(|_| {
            let (link, big) = (Arc::clone(&link), big.clone());
            std::thread::spawn(move || {
                let began = Instant::now();
                let mut longest = Duration::ZERO;
                for _ in 0..6 {
                    let asked = Instant::now();
                    let _ = link.call(METHOD, Some(&big), Duration::from_millis(300), |_| Ok(()));
                    longest = longest.max(asked.elapsed());
                }
                (began.elapsed(), longest)
            })
        })
        .collect();
    for player in players {
        let (_, longest) = player.join().unwrap();
        // One call waits for its deadline (300 ms) and a margin, not for a write.
        assert!(
            longest < Duration::from_millis(1200),
            "a call took {longest:?}"
        );
    }
    let took = began.elapsed();
    assert!(
        took < Duration::from_secs(3),
        "the players took {took:?} in all"
    );
}

/// A write that gives up closes the connection and the proxy connects again. How much it takes to
/// block a write is the system's to say (Linux sizes the buffers of a socket by itself, to several
/// MiB, and Windows to much less), so this sends until the connection is closed, and fails, saying so, if
/// 64 MiB did not do it.
#[test]
fn a_write_that_gives_up_closes_the_connection_and_the_proxy_connects_again() {
    let fake = Fake::start(Script {
        // The first connection reads nothing for as long as the test needs; the next ones read.
        deaf: Some(Duration::from_secs(60)),
        deaf_first: 1,
        ..Script::default()
    });
    let (_registry, link, _folder) = connect_with(&fake, |s| {
        s.timings.write = Duration::from_millis(200);
        // The deadlines of the calls must not open the breaker before the writes block.
        s.breaker_failures = u32::MAX;
        s.max_in_flight = 64;
    });
    until("the connection", || link.is_connected());
    let big = json!({ "filler": "x".repeat(200 << 10) });
    let done = Arc::new(AtomicBool::new(false));
    let sent = Arc::new(AtomicUsize::new(0));
    let players: Vec<_> = (0..16)
        .map(|_| {
            let (link, big, done, sent) = (
                Arc::clone(&link),
                big.clone(),
                Arc::clone(&done),
                Arc::clone(&sent),
            );
            std::thread::spawn(move || {
                while !done.load(Ordering::Relaxed) {
                    let asked =
                        link.call(METHOD, Some(&big), Duration::from_millis(100), |_| Ok(()));
                    // Only what was handed over counts: once the connection is closed the calls fail at
                    // once, and they are not what blocked it.
                    if asked == Err(CallError::Disconnected) {
                        std::thread::sleep(Duration::from_millis(5));
                    } else {
                        sent.fetch_add(1, Ordering::Relaxed);
                    }
                }
            })
        })
        .collect();
    let limit = Instant::now() + Duration::from_secs(60);
    let outcome = loop {
        if fake.seen(|s| s.hellos.len() >= 2) {
            break Ok(());
        }
        // 200 KiB a request: 328 of them are 64 MiB.
        if sent.load(Ordering::Relaxed) > 328 || Instant::now() > limit {
            break Err(format!(
                "{} requests of 200 KiB did not block a write, or the proxy did not connect again",
                sent.load(Ordering::Relaxed)
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    done.store(true, Ordering::Relaxed);
    for player in players {
        player.join().unwrap();
    }
    outcome.unwrap();
    // The extension of the second connection reads, and answers.
    until("an answer", || ping(&link, Duration::from_secs(1)).is_ok());
}

/// A request that cannot be handed over because the thread that writes is stuck is a failure of the
/// extension, the trial's included: the breaker must not stay waiting for the trial's result. The link
/// is not run by a connection thread here, so that nothing closes the breaker or opens it behind the
/// test's back, on a system that wakes a reader at a shutdown and on one that does not.
#[test]
fn a_trial_that_cannot_be_sent_ends_the_trial() {
    use super::breaker::Permit;
    use super::link::Info;
    // The writing half of a connection, and the other end kept open.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        Socket::accept(stream, wire::MAX_MESSAGE, |_| {
            Ok(Some(wire::SUBPROTOCOL.to_owned()))
        })
        .unwrap()
    });
    let request = wire::upgrade_request(
        "127.0.0.1",
        address.port(),
        &meridian_websocket::new_key().unwrap(),
    );
    let _peer = Socket::open_checked(
        TcpStream::connect(address).unwrap(),
        &request,
        wire::MAX_MESSAGE,
    )
    .unwrap();
    let (_reader, writer) = server.join().unwrap().split().unwrap();

    let mut settings = Settings::new("x", "127.0.0.1:1", "x.token");
    settings.breaker_failures = 1;
    settings.breaker_open = Duration::from_millis(30);
    let link = Link::new(settings, Arc::default());
    // An outbox that is full: the request cannot be handed over.
    let (outbox, _inbox) = std::sync::mpsc::sync_channel::<String>(1);
    outbox.send(String::new()).unwrap();
    let info = Info {
        protocol: 0,
        name: "x".into(),
        version: "1".into(),
        granted: Vec::new(),
        refused: Vec::new(),
        commands: Vec::new(),
    };
    link.connected(writer, outbox, info);

    // Not the trial: it fails, and opens the breaker.
    assert_eq!(
        ping(&link, Duration::from_secs(1)),
        Err(CallError::Disconnected)
    );
    std::thread::sleep(Duration::from_millis(60));
    // The trial: it fails too, and the breaker is open again, not left waiting for the trial.
    assert_eq!(
        ping(&link, Duration::from_secs(1)),
        Err(CallError::Disconnected)
    );
    let later = Instant::now() + Duration::from_secs(60);
    assert_eq!(link.breaker().permit(later), Permit::Trial);
}

#[test]
fn a_decision_point_that_is_granted_and_not_declared_is_named() {
    use super::connection::undeclared_points;
    use super::link::Info;
    let info = |granted: &[&str]| Info {
        protocol: 0,
        name: "x".into(),
        version: "1".into(),
        granted: granted.iter().map(|g| (*g).to_owned()).collect(),
        refused: Vec::new(),
        commands: Vec::new(),
    };
    let mut settings = Settings::new("x", "127.0.0.1:1", "x.token");
    settings.grant = vec![
        "decide.login".into(),
        "decide.command".into(),
        "addresses".into(),
    ];
    // It declared the login only: the command is granted and not declared. `addresses` is not a
    // decision point, and an extension may not need it.
    assert_eq!(
        undeclared_points(&settings, &info(&["decide.login"])),
        ["decide.command"]
    );
    assert!(undeclared_points(&settings, &info(&["decide.login", "decide.command"])).is_empty());
    assert_eq!(
        undeclared_points(&settings, &info(&[])),
        ["decide.login", "decide.command"]
    );
    // Nothing granted, nothing to say.
    settings.grant.clear();
    assert!(undeclared_points(&settings, &info(&[])).is_empty());
}

/// The close must reach the extension whatever the size of the message it sent: the proxy reads the
/// rest of it before it closes, so that its socket is not reset with the close frame unread.
#[test]
fn the_close_for_a_message_too_big_reaches_the_extension_every_time() {
    for round in 0..20 {
        let fake = Fake::start(Script {
            big_message: true,
            ..Script::default()
        });
        let (_registry, _link, _folder) = connect_with(&fake, |_| {});
        let limit = Instant::now() + Duration::from_secs(10);
        while !fake.seen(|s| s.close_codes.contains(&Some(close::TOO_BIG))) {
            assert!(
                Instant::now() < limit,
                "round {round}: the extension never read the close 1009; it read {:?}",
                fake.seen(|s| s.close_codes.clone())
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[test]
fn the_token_is_in_nothing_the_proxy_sent() {
    let fake = Fake::start(Script::default());
    let (_registry, link, _folder) = connect(&fake);
    let _ = ping(&link, Duration::from_secs(2));
    let token = String::from_utf8(TOKEN.to_vec()).unwrap();
    fake.seen(|seen| {
        assert!(
            seen.texts.iter().all(|t| !t.contains(&token)),
            "{:?}",
            seen.texts
        );
    });
}

#[test]
fn only_the_loopback_is_connected_to() {
    for fine in ["127.0.0.1:25590", "[::1]:25590", "localhost:25590"] {
        assert!(loopback_addresses(fine).is_ok(), "{fine}");
    }
    for refused in [
        "8.8.8.8:53",
        "192.168.1.5:25590",
        "0.0.0.0:1",
        "[2001:db8::1]:1",
    ] {
        assert!(
            loopback_addresses(refused)
                .unwrap_err()
                .contains("loopback"),
            "{refused}"
        );
    }
    assert!(loopback_addresses("no port").is_err());
}

#[test]
fn a_token_file_that_cannot_be_used_is_named_and_its_content_is_not() {
    let folder = Folder::new();
    let error = read_token(&folder.0.join("missing")).err().unwrap();
    assert!(error.contains("missing"), "{error}");
    let short = folder.token(b"short-secret-value");
    let error = read_token(&short).err().unwrap();
    assert!(
        error.contains("token") && !error.contains("short-secret-value"),
        "{error}"
    );
    let good = folder.token(&[TOKEN, b"\n"].concat());
    assert!(read_token(&good).is_ok());
}

#[test]
fn an_address_that_is_not_on_the_loopback_is_never_connected_to() {
    let folder = Folder::new();
    let mut settings = Settings::new("away", "192.0.2.1:25590", folder.token(TOKEN));
    settings.timings = timings();
    let registry = Registry::default();
    let link = registry.start(settings).unwrap();
    std::thread::sleep(Duration::from_millis(150));
    assert!(!link.is_connected());
    assert_eq!(
        ping(&link, Duration::from_millis(50)),
        Err(CallError::Disconnected)
    );
}

#[test]
fn stopping_ends_the_connection_and_the_thread_quickly() {
    let fake = Fake::start(Script::default());
    let (registry, link, _folder) = connect(&fake);
    let started = Instant::now();
    registry.stop();
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(!link.is_connected());
    until("the fake to see the end", || {
        !fake.seen(|s| s.close_codes.is_empty())
    });
    assert_eq!(registry.links().len(), 0);
}

#[test]
fn the_metrics_page_has_the_extensions_and_none_without_one() {
    assert_eq!(Registry::default().links().len(), 0);
    let mut page = String::new();
    Registry::default().render(&mut page);
    assert!(page.is_empty());

    let fake = Fake::start(Script::default());
    let (registry, link, _folder) = connect(&fake);
    ping(&link, Duration::from_secs(2)).unwrap();
    registry.render(&mut page);
    for line in [
        "meridian_extension_connected{extension=\"perms\"} 1",
        "meridian_extension_breaker_open{extension=\"perms\"} 0",
        "meridian_extension_requests_total{extension=\"perms\",method=\"meridian:test/ping\",outcome=\"answered\"} 1",
        "meridian_extension_late_answers_total{extension=\"perms\"} 0",
    ] {
        assert!(page.contains(line), "{line}\n{page}");
    }
}

// ---------------------------------------------------------------------------------------------

/// The configuration with one `[[extension]]` that leads to `fake`, the token in `folder`.
fn configured(
    fake: &Fake,
    folder: &Folder,
    change: impl FnOnce(&mut meridian_config::Extension),
) -> meridian_config::Config {
    let mut extension = meridian_config::Extension {
        name: "perms".into(),
        enabled: true,
        address: fake.address.to_string(),
        token_file: folder.token(TOKEN),
        grant: vec!["decide.login".into()],
        max_in_flight: 64,
        breaker_failures: 5,
        breaker_seconds: 30,
        decide_login: Default::default(),
        decide_command: Default::default(),
        decide_server: Default::default(),
        own_commands: Default::default(),
        orders: Default::default(),
    };
    change(&mut extension);
    meridian_config::Config {
        extensions: vec![extension],
        ..meridian_config::Config::default()
    }
}

#[test]
fn the_defaults_of_the_configuration_are_the_defaults_of_the_connection() {
    let defaults = Settings::new("x", "127.0.0.1:1", "x.token");
    let configured = Settings::from(&meridian_config::Extension {
        name: "x".into(),
        enabled: true,
        address: "127.0.0.1:1".into(),
        token_file: "x.token".into(),
        grant: Vec::new(),
        max_in_flight: 64,
        breaker_failures: 5,
        breaker_seconds: 30,
        decide_login: Default::default(),
        decide_command: Default::default(),
        decide_server: Default::default(),
        own_commands: Default::default(),
        orders: Default::default(),
    });
    assert_eq!(configured, defaults);
    // The shortest token the configuration accepts is the shortest the handshake accepts.
    assert_eq!(
        meridian_config::MIN_EXTENSION_TOKEN,
        handshake::MIN_TOKEN_LEN
    );
}

#[test]
fn without_an_extension_in_the_configuration_nothing_starts() {
    let registry = Registry::default();
    assert!(
        registry
            .follow(&meridian_config::Config::default())
            .is_empty()
    );
    assert!(registry.links().is_empty());
    let mut page = String::new();
    registry.render(&mut page);
    assert!(page.is_empty());
}

#[test]
fn the_registry_starts_what_the_configuration_enables_and_leaves_alone_what_did_not_change() {
    let fake = Fake::start(Script::default());
    let folder = Folder::new();
    let registry = Registry::default();
    let config = configured(&fake, &folder, |_| {});
    let lines = registry.follow(&config);
    assert_eq!(
        lines,
        [format!(
            "extension \"perms\" added: connecting to {}",
            fake.address
        )]
    );
    let link = registry.links().remove(0);
    until("the connection", || link.is_connected());
    assert_eq!(link.info().unwrap().granted, ["decide.login"]);
    assert!(ping(&link, Duration::from_secs(2)).is_ok());

    // The same configuration: the same connection, and its counters are kept.
    assert!(registry.follow(&config).is_empty());
    assert!(Arc::ptr_eq(&link, &registry.links()[0]));
    assert!(link.is_connected());
    // What only a decision point reads does not touch the connection either.
    let slower = configured(&fake, &folder, |e| e.decide_login.timeout_ms = 3000);
    assert!(registry.follow(&slower).is_empty());
    assert!(Arc::ptr_eq(&link, &registry.links()[0]));
}

#[test]
fn a_changed_extension_gets_a_new_connection_and_the_old_one_is_closed() {
    let fake = Fake::start(Script::default());
    let folder = Folder::new();
    let registry = Registry::default();
    registry.follow(&configured(&fake, &folder, |_| {}));
    let old = registry.links().remove(0);
    until("the connection", || old.is_connected());

    let lines = registry.follow(&configured(&fake, &folder, |e| {
        e.max_in_flight = 2;
        e.grant = vec!["decide.login".into(), "orders.read".into()];
    }));
    assert_eq!(
        lines,
        ["extension \"perms\" changed: its connection is closed and opened again"]
    );
    assert!(!old.is_connected());
    assert_eq!(
        ping(&old, Duration::from_millis(50)),
        Err(CallError::Disconnected)
    );
    let new = registry.links().remove(0);
    assert!(!Arc::ptr_eq(&old, &new));
    until("the new connection", || new.is_connected());
    assert_eq!(new.info().unwrap().granted, ["decide.login", "orders.read"]);
    // The fake saw the first connection end and a second hello.
    until("the second hello", || fake.seen(|s| s.hellos.len() == 2));
    assert!(fake.seen(|s| !s.close_codes.is_empty()));
}

#[test]
fn a_removed_or_disabled_extension_is_stopped_and_its_thread_ends() {
    for (disabled, word) in [(false, "removed"), (true, "disabled")] {
        let fake = Fake::start(Script::default());
        let folder = Folder::new();
        let registry = Registry::default();
        registry.follow(&configured(&fake, &folder, |_| {}));
        let link = registry.links().remove(0);
        until("the connection", || link.is_connected());

        let config = if disabled {
            configured(&fake, &folder, |e| e.enabled = false)
        } else {
            meridian_config::Config::default()
        };
        let started = Instant::now();
        let lines = registry.follow(&config);
        assert!(started.elapsed() < Duration::from_secs(3));
        assert_eq!(
            lines,
            [format!(
                "extension \"perms\" {word}: its connection is closed"
            )]
        );
        assert!(registry.links().is_empty());
        assert!(!link.is_connected());
        until("the fake to see the end", || {
            !fake.seen(|s| s.close_codes.is_empty())
        });
        // Nothing connects again.
        let hellos = fake.seen(|s| s.hellos.len());
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(fake.seen(|s| s.hellos.len()), hellos);
        // It can be enabled again: a new connection.
        let lines = registry.follow(&configured(&fake, &folder, |_| {}));
        assert_eq!(lines.len(), 1);
        until("the connection", || registry.links()[0].is_connected());
    }
}

// The real connection against the SDK's end of it (`meridian_extension::serve`): the two sides of the
// protocol that Meridian ships, one against the other.

mod sdk {
    use super::*;
    use meridian_extension::serve::{Extension as SdkExtension, Log, Running as SdkRunning};

    #[derive(Default)]
    struct Lines(Mutex<Vec<String>>);

    impl Lines {
        fn all(&self) -> Vec<String> {
            self.0.lock().unwrap().clone()
        }
    }

    impl Log for Lines {
        fn info(&self, line: &str) {
            self.0.lock().unwrap().push(format!("INFO {line}"));
        }

        fn warn(&self, line: &str) {
            self.0.lock().unwrap().push(format!("WARN {line}"));
        }
    }

    fn sdk(token: &[u8], lines: &Arc<Lines>) -> SdkRunning {
        SdkExtension::new("sdk-extension", "0.3.1", Secret::new(token).unwrap())
            .log(Arc::clone(lines) as Arc<dyn Log>)
            .bind("127.0.0.1:0")
            .unwrap()
    }

    fn link_to(running: &SdkRunning, folder: &Folder) -> (Registry, Arc<Link>) {
        let mut settings = Settings::new(
            "perms",
            running.local_addr().to_string(),
            folder.token(TOKEN),
        );
        settings.grant = vec!["decide.login".into()];
        settings.timings = timings();
        let registry = Registry::default();
        let link = registry.start(settings).unwrap();
        (registry, link)
    }

    #[test]
    fn the_proxys_connection_and_the_sdk_do_the_hello_and_the_proxy_reads_what_the_sdk_declares() {
        let lines = Arc::new(Lines::default());
        let running = sdk(TOKEN, &lines);
        let folder = Folder::new();
        let (_registry, link) = link_to(&running, &folder);
        until("the connection", || link.is_connected());
        let info = link.info().unwrap();
        assert_eq!(
            (info.protocol, info.name.as_str(), info.version.as_str()),
            (0, "sdk-extension", "0.3.1")
        );
        // It declares nothing yet (no handler is registered), so nothing is granted or refused.
        assert!(info.granted.is_empty() && info.refused.is_empty());
        until("the line", || {
            lines.all().iter().any(|l| {
                l.starts_with("INFO the proxy ") && l.ends_with("is connected, protocol 0")
            })
        });
    }

    #[test]
    fn an_error_of_the_sdk_reaches_the_proxy_as_an_invalid_answer() {
        let running = sdk(TOKEN, &Arc::default());
        let folder = Folder::new();
        let (_registry, link) = link_to(&running, &folder);
        until("the connection", || link.is_connected());
        // No handler serves this method: the SDK answers -32601.
        let result = link.call(
            "meridian:decide/login",
            Some(&json!({"name": "Steve"})),
            Duration::from_secs(2),
            |_| Ok(()),
        );
        let Err(CallError::Invalid(why)) = result else {
            panic!("{result:?}");
        };
        assert!(why.contains("-32601"), "{why}");
        assert!(link.is_connected());
    }

    #[test]
    fn a_wrong_token_is_refused_by_the_proxy_before_it_confirms_and_it_waits_the_longest() {
        let lines = Arc::new(Lines::default());
        // The SDK has another token than the file the proxy reads.
        let running = sdk(b"another-token-0123456789abcdefghij0", &lines);
        let folder = Folder::new();
        let (_registry, link) = link_to(&running, &folder);
        // The proxy checks the extension's proof first and closes for a policy; it never confirms.
        until("the wait", || link.last_wait() == timings().longest_wait);
        assert!(!link.is_connected());
        assert!(link.info().is_none());
        // The SDK saw a proxy that closed after the hello, and said nothing of the token.
        until("the extension's line", || {
            lines.all().iter().any(|l| l.contains("the hello failed"))
        });
        assert!(lines.all().iter().all(|l| !l.contains("another-token")));
        assert!(
            ping(&link, Duration::from_millis(50)).is_err(),
            "no request goes out"
        );
    }

    #[test]
    fn stopping_the_sdk_ends_the_proxys_connection_and_the_proxy_keeps_trying() {
        let running = sdk(TOKEN, &Arc::default());
        let folder = Folder::new();
        let (_registry, link) = link_to(&running, &folder);
        until("the connection", || link.is_connected());
        running.stop();
        until("the end", || !link.is_connected());
        // Nothing listens any more: the wait grows to the longest.
        until("the wait", || link.last_wait() == timings().longest_wait);
    }
}

mod login_with_the_sdk {
    use super::*;
    use meridian_extension::decide::{self, Auth, Decision, LoginParams, LoginRequest, Player};
    use meridian_extension::serve::{Extension as SdkExtension, Running as SdkRunning};

    fn request(name: &str) -> LoginParams {
        LoginParams {
            request: LoginRequest {
                player: Player {
                    name: name.into(),
                    uuid: "0a2ea4ba-bced-39d2-80e1-1ad51a0e925c".into(),
                    auth: Auth::Unverified,
                },
                host: Some("play.example.org".into()),
                address: None,
                connected: false,
            },
        }
    }

    fn ask(link: &Link, name: &str) -> Result<Decision, CallError> {
        let params = serde_json::to_value(request(name)).unwrap();
        link.call(
            decide::LOGIN,
            Some(&params),
            Duration::from_secs(2),
            decide::parse_login_answer,
        )
    }

    fn gate() -> SdkExtension {
        SdkExtension::new("gate", "1.0.0", Secret::new(TOKEN).unwrap()).on_login(|request| {
            // The extension is told what the proxy knows of the account.
            assert_eq!(request.player.auth, Auth::Unverified);
            match request.player.name.as_str() {
                "Alex" => Decision::deny("The event starts at 20:00."),
                "Bob" => Decision::Allow,
                _ => Decision::Default,
            }
        })
    }

    fn connect_to(running: &SdkRunning, grant: &[&str]) -> (Registry, Arc<Link>, Folder) {
        let folder = Folder::new();
        let mut settings = Settings::new(
            "perms",
            running.local_addr().to_string(),
            folder.token(TOKEN),
        );
        settings.grant = grant.iter().map(|g| (*g).to_owned()).collect();
        settings.timings = timings();
        let registry = Registry::default();
        let link = registry.start(settings).unwrap();
        until("the connection", || link.is_connected());
        (registry, link, folder)
    }

    #[test]
    fn the_proxys_connection_asks_the_sdk_for_a_login_and_reads_the_decision() {
        let running = gate().bind("127.0.0.1:0").unwrap();
        let (registry, link, _folder) = connect_to(&running, &["decide.login"]);
        let info = link.info().unwrap();
        assert_eq!(info.granted, ["decide.login"]);
        assert!(info.refused.is_empty());
        assert_eq!(
            ask(&link, "Alex").unwrap(),
            Decision::deny("The event starts at 20:00.")
        );
        assert_eq!(ask(&link, "Bob").unwrap(), Decision::Allow);
        assert_eq!(ask(&link, "Somebody").unwrap(), Decision::Default);
        let mut page = String::new();
        registry.render(&mut page);
        assert!(
            page.contains("meridian_extension_requests_total{extension=\"perms\",method=\"meridian:decide/login\",outcome=\"answered\"} 3"),
            "{page}"
        );
    }

    #[test]
    fn a_capability_the_operator_did_not_grant_is_refused_in_the_hello() {
        let running = gate().bind("127.0.0.1:0").unwrap();
        let (_registry, link, _folder) = connect_to(&running, &[]);
        let info = link.info().unwrap();
        assert!(info.granted.is_empty());
        assert_eq!(info.refused, ["decide.login"]);
    }
}

// ---------------------------------------------------------------------------------------------
// The orders of the extension (docs/EXTENSIONS.md, section 5)

mod the_orders {
    use super::*;
    use crate::extension::orders::{Outcome as Ended, test_support};

    const INVALIDATE: &str = "meridian:permissions/invalidate";
    const BLOCK: &str = "meridian:test/block";
    const PANIC: &str = "meridian:test/panic";
    const ALEX: &str = "0a2ea4ba-bced-39d2-80e1-1ad51a0e925c";
    const BOB: &str = "1b3fb5cb-cdfe-4ae3-91f2-2be62b1fa36d";

    /// A fake that says hello asking for `grant`, and a link that is granted it, with `change` applied to
    /// the link's settings.
    fn orderly(
        grant: &[&str],
        change: impl FnOnce(&mut Settings),
    ) -> (Fake, Registry, Arc<Link>, Folder) {
        let grant: Vec<String> = grant.iter().map(|g| (*g).to_owned()).collect();
        let fake = Fake::start(Script {
            capabilities: grant.clone(),
            ..Script::default()
        });
        let (registry, link, folder) = connect_with(&fake, |settings| {
            settings.grant = grant;
            change(settings);
        });
        until("the connection", || link.is_connected());
        (fake, registry, link, folder)
    }

    /// Sends the request `method` with `params` (as JSON text) and the id `id`.
    fn order(fake: &Fake, id: &Value, method: &str, params: &str) {
        fake.send(&format!(
            r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}","params":{params}}}"#
        ));
    }

    /// The answer the proxy gave to the request with this id, waited for.
    fn answer(fake: &Fake, id: &Value) -> Value {
        let mut found = None;
        until("the answer", || {
            found = fake.seen(|seen| {
                seen.texts.iter().find_map(|text| {
                    let message: Value = serde_json::from_str(text).ok()?;
                    (message["id"] == *id
                        && (message.get("result").is_some() || message.get("error").is_some()))
                    .then_some(message)
                })
            });
            found.is_some()
        });
        found.unwrap()
    }

    fn code_of(answer: &Value) -> i64 {
        answer["error"]["code"].as_i64().expect("an error")
    }

    #[test]
    fn an_order_with_its_capability_runs_and_is_answered_with_the_id_it_came_with() {
        let (fake, _registry, link, _folder) = orderly(&["decide.command"], |_| {});
        order(&fake, &json!(100), INVALIDATE, r#"{"all":true}"#);
        assert_eq!(answer(&fake, &json!(100))["result"], json!({"ok": true}));
        assert_eq!(link.forgotten_all(), 1);
        // One player, the id written in capitals and with no hyphens, as a program may write it.
        let written = ALEX.replace('-', "").to_ascii_uppercase();
        order(
            &fake,
            &json!("a-string-id"),
            INVALIDATE,
            &format!(r#"{{"player":"{written}"}}"#),
        );
        assert_eq!(
            answer(&fake, &json!("a-string-id"))["result"],
            json!({"ok": true})
        );
        assert!(link.forgotten_player(0, ALEX).1);
        assert!(!link.forgotten_player(0, BOB).1);
        assert_eq!(link.stats().orders(INVALIDATE, Ended::Done), 2);
    }

    #[test]
    fn parameters_that_are_not_what_the_order_takes_are_answered_32602_and_run_nothing() {
        let (fake, _registry, link, _folder) = orderly(&["decide.command"], |_| {});
        let secret = "an-id-that-must-not-come-back";
        for (n, params) in [
            "{}".to_owned(),
            r#"{"all":false}"#.to_owned(),
            format!(r#"{{"player":"{secret}"}}"#),
            format!(r#"{{"player":"{ALEX}","all":true}}"#),
            r#"{"player":7}"#.to_owned(),
            r#"{"all":true,"more":1}"#.to_owned(),
            "[1]".to_owned(),
            "null".to_owned(),
        ]
        .iter()
        .enumerate()
        {
            let id = json!(200 + n);
            order(&fake, &id, INVALIDATE, params);
            let reply = answer(&fake, &id);
            assert_eq!(code_of(&reply), -32602, "{params}");
            assert!(!reply.to_string().contains(secret), "{reply}");
        }
        // No `params` at all.
        fake.send(&format!(
            r#"{{"jsonrpc":"2.0","id":299,"method":"{INVALIDATE}"}}"#
        ));
        assert_eq!(code_of(&answer(&fake, &json!(299))), -32602);
        assert_eq!(
            (link.forgotten_all(), link.forgotten_player(0, ALEX)),
            (0, (0, false))
        );
        assert_eq!(link.stats().orders(INVALIDATE, Ended::Invalid), 9);
        assert_eq!(link.stats().orders(INVALIDATE, Ended::Done), 0);
    }

    #[test]
    fn an_order_without_its_capability_and_a_method_nobody_has_are_answered_as_if_they_did_not_exist()
     {
        // `decide.command` is not granted: `orders.read` is.
        let (fake, registry, link, _folder) = orderly(&["orders.read"], |_| {});
        order(&fake, &json!(300), INVALIDATE, r#"{"all":true}"#);
        order(&fake, &json!(301), "meridian:nothing/here", "{}");
        order(&fake, &json!(302), "meridian:made/up", "{}");
        let (ungranted, unbuilt, made_up) = (
            answer(&fake, &json!(300)),
            answer(&fake, &json!(301)),
            answer(&fake, &json!(302)),
        );
        // The same answer for all three: the extension cannot tell a method it may not use from one that is not there.
        assert_eq!(ungranted["error"], unbuilt["error"]);
        assert_eq!(ungranted["error"], made_up["error"]);
        assert_eq!(code_of(&ungranted), -32601);
        assert_eq!(link.forgotten_all(), 0);
        assert_eq!(link.stats().orders(INVALIDATE, Ended::Refused), 1);
        // The two methods the proxy does not have are counted under one name, not under what the extension said.
        assert_eq!(link.stats().orders("unknown", Ended::Refused), 2);
        let mut page = String::new();
        registry.render(&mut page);
        assert!(
            !page.contains("made/up") && !page.contains("nothing/here"),
            "{page}"
        );
    }

    #[test]
    fn a_request_with_no_id_is_a_notification_and_nothing_is_run_for_it() {
        let (fake, _registry, link, _folder) = orderly(&["decide.command"], |_| {});
        fake.send(&format!(
            r#"{{"jsonrpc":"2.0","method":"{INVALIDATE}","params":{{"all":true}}}}"#
        ));
        // A request after it: once this is answered, the notification has been read.
        order(&fake, &json!(400), INVALIDATE, r#"{"all":true}"#);
        let _ = answer(&fake, &json!(400));
        assert_eq!(link.forgotten_all(), 1, "only the request ran");
        assert_eq!(link.stats().orders(INVALIDATE, Ended::Done), 1);
    }

    #[test]
    fn an_order_over_the_budget_is_answered_with_its_own_code_and_is_not_run() {
        let (fake, _registry, link, _folder) = orderly(&["decide.command"], |settings| {
            settings.order_burst = 3;
            settings.order_rate = 1;
        });
        for n in 0..6 {
            order(&fake, &json!(500 + n), INVALIDATE, r#"{"all":true}"#);
        }
        let answers: Vec<Value> = (0..6).map(|n| answer(&fake, &json!(500 + n))).collect();
        let over = answers.iter().filter(|a| a.get("error").is_some()).count();
        let done = answers.len() - over;
        // The burst is three. A second may have passed on a loaded machine and given one more back.
        assert!((3..=4).contains(&done), "{done} ran");
        assert!(
            answers
                .iter()
                .filter(|a| a.get("error").is_some())
                .all(|a| code_of(a) == -32001
                    && a["error"]["message"] == "over the budget of orders")
        );
        assert_eq!(
            usize::try_from(link.forgotten_all()).unwrap(),
            done,
            "an order that was refused did not run"
        );
        assert_eq!(
            link.stats().orders(INVALIDATE, Ended::OverBudget),
            over as u64
        );
        assert_eq!(link.stats().orders(INVALIDATE, Ended::Done), done as u64);
    }

    /// A slow order holds neither the thread that reads the connection nor the places of the others: the proxy's
    /// own request is answered while it waits, a second order finds no place and is told so at once, and the
    /// place is given back when the first ends.
    #[test]
    fn a_slow_order_does_not_hold_the_reader_and_at_most_the_places_there_are_run() {
        test_support::RELEASE.store(false, Ordering::SeqCst);
        test_support::ENTERED.store(0, Ordering::SeqCst);
        let (fake, _registry, link, _folder) = orderly(&["orders.read"], |settings| {
            settings.orders_running = 1;
        });
        order(&fake, &json!(600), BLOCK, "{}");
        until("the order to start", || {
            test_support::ENTERED.load(Ordering::SeqCst) == 1
        });
        // No place left.
        order(&fake, &json!(601), BLOCK, "{}");
        let busy = answer(&fake, &json!(601));
        assert_eq!(code_of(&busy), -32002);
        assert_eq!(busy["error"]["message"], "too many orders running");
        assert_eq!(test_support::ENTERED.load(Ordering::SeqCst), 1);
        // The reader is free: a request of the proxy to the extension is answered, and so is `rpc.discover`.
        assert!(ping(&link, Duration::from_secs(2)).is_ok());
        fake.send(r#"{"jsonrpc":"2.0","id":"d-1","method":"meridian:rpc.discover"}"#);
        assert!(answer(&fake, &json!("d-1")).get("result").is_some());
        // Let it go: it answers, and a place is free again.
        test_support::RELEASE.store(true, Ordering::SeqCst);
        assert_eq!(answer(&fake, &json!(600))["result"], json!({"ok": true}));
        order(&fake, &json!(602), BLOCK, "{}");
        assert_eq!(answer(&fake, &json!(602))["result"], json!({"ok": true}));
        assert_eq!(link.stats().orders(BLOCK, Ended::Done), 2);
        assert_eq!(link.stats().orders(BLOCK, Ended::Busy), 1);
        test_support::RELEASE.store(false, Ordering::SeqCst);
    }

    #[test]
    fn an_order_that_fails_is_answered_with_an_error_and_gives_its_place_back() {
        let (fake, _registry, link, _folder) = orderly(&["orders.read"], |settings| {
            settings.orders_running = 1;
        });
        for n in 0..3 {
            order(&fake, &json!(700 + n), PANIC, "{}");
            let reply = answer(&fake, &json!(700 + n));
            // Each finds its place free: the one before it ended and gave it back.
            assert_eq!(code_of(&reply), -32603, "{reply}");
            assert_eq!(reply["error"]["message"], "the order failed");
        }
        assert_eq!(link.stats().orders(PANIC, Ended::Failed), 3);
        assert_eq!(link.stats().orders(PANIC, Ended::Busy), 0);
        // And the connection is up, and answers.
        assert!(link.is_connected() && ping(&link, Duration::from_secs(2)).is_ok());
    }

    #[test]
    fn rpc_discover_lists_the_orders_the_capabilities_that_were_granted_allow() {
        let methods = |grant: &[&str]| -> Vec<String> {
            let (fake, _registry, _link, _folder) = orderly(grant, |_| {});
            fake.send(r#"{"jsonrpc":"2.0","id":800,"method":"rpc.discover"}"#);
            let reply = answer(&fake, &json!(800));
            let document: Discover = serde_json::from_value(reply["result"].clone()).unwrap();
            document.methods.into_iter().map(|m| m.name).collect()
        };
        assert_eq!(methods(&["decide.command"]), [INVALIDATE]);
        assert_eq!(
            methods(&["orders.read"]),
            [
                "meridian:players/list",
                "meridian:players/find",
                "meridian:servers/list",
                "meridian:queue/list",
                BLOCK,
                PANIC,
                "meridian:test/slow-write"
            ]
        );
        assert!(methods(&["decide.login", "events"]).is_empty());
    }

    #[test]
    fn the_orders_are_on_the_page_of_metrics_by_extension_method_and_outcome() {
        let (fake, registry, _link, _folder) = orderly(&["decide.command"], |_| {});
        order(&fake, &json!(900), INVALIDATE, r#"{"all":true}"#);
        let _ = answer(&fake, &json!(900));
        order(&fake, &json!(901), INVALIDATE, "{}");
        let _ = answer(&fake, &json!(901));
        let mut page = String::new();
        registry.render(&mut page);
        for line in [
            "meridian_extension_orders_total{extension=\"perms\",method=\"meridian:permissions/invalidate\",outcome=\"done\"} 1",
            "meridian_extension_orders_total{extension=\"perms\",method=\"meridian:permissions/invalidate\",outcome=\"invalid\"} 1",
            "meridian_extension_orders_total{extension=\"perms\",method=\"meridian:permissions/invalidate\",outcome=\"budget\"} 0",
            "meridian_extension_orders_total{extension=\"perms\",method=\"meridian:permissions/invalidate\",outcome=\"busy\"} 0",
        ] {
            assert!(page.contains(line), "{line}\n{page}");
        }
        assert!(
            page.contains("# TYPE meridian_extension_orders_total counter"),
            "{page}"
        );
    }

    /// An extension that does not read its answers holds the threads of the orders it gave, each for the time of a
    /// write, and no place: the places count the orders that run, so they cannot bound those threads; the threads
    /// themselves are bounded, to twice `max-running`. The writes that block are the test's own (`SLOW_WRITE` waits
    /// before it writes, until it is let go), so that nothing here depends on how much a socket takes before a
    /// write blocks, which is not the same on every system.
    #[test]
    fn an_order_that_waits_to_write_holds_no_place_and_the_threads_that_wait_are_bounded() {
        const SLOW: &str = "meridian:test/slow-write";
        test_support::WRITE_OPEN.store(false, Ordering::SeqCst);
        let (fake, _registry, link, _folder) = orderly(&["orders.read"], |settings| {
            settings.orders_running = 4;
            settings.order_rate = 1000;
            settings.order_burst = 1000;
        });
        // Eight orders, one after the other: each runs, gives its place back, and waits to write. The next is sent
        // when the one before waits, so that no place is ever the limit: only the threads are.
        for n in 0..8 {
            order(&fake, &json!(1000 + n), SLOW, "{}");
            until("an order to wait to write", || {
                test_support::WAITING_TO_WRITE.load(Ordering::SeqCst) == n + 1
            });
        }
        assert_eq!(link.order_threads_alive(), 8);
        // Their places are free: they gave them back before they waited. (Kept until the write ended, they would
        // have refused the fifth.) All four can be taken, and no fifth.
        let places: Vec<_> = (0..4)
            .map(|_| link.order_place().expect("a place"))
            .collect();
        assert!(link.order_place().is_none());
        drop(places);
        // The ninth, and the ones after it, find no thread: told so at once, and none runs. The number of threads
        // never goes above twice the places.
        for n in 8..12 {
            order(&fake, &json!(1000 + n), SLOW, "{}");
            let refused = answer(&fake, &json!(1000 + n));
            assert_eq!(code_of(&refused), -32002, "{refused}");
            assert_eq!(link.order_threads_alive(), 8);
        }
        assert_eq!(link.stats().orders(SLOW, Ended::Busy), 4);
        assert_eq!(link.stats().orders(SLOW, Ended::Done), 8);
        // The writes can go: all eight answer, the threads end, and orders are served again.
        test_support::WRITE_OPEN.store(true, Ordering::SeqCst);
        for n in 0..8 {
            assert_eq!(
                answer(&fake, &json!(1000 + n))["result"],
                json!({"ok": true})
            );
        }
        until("the threads to end", || link.order_threads_alive() == 0);
        order(&fake, &json!(2000), SLOW, "{}");
        assert_eq!(answer(&fake, &json!(2000))["result"], json!({"ok": true}));
        test_support::WRITE_OPEN.store(false, Ordering::SeqCst);
    }

    // ---- the orders that read ------------------------------------------------------------------------

    use crate::extension::orders::Facts;
    use meridian_extension::orders::{
        self as shapes, Found, PlayerInfo, PlayersPage, QueueState, ServerInfo, ServersList,
        Waiting,
    };

    /// Facts that are made up: seven players (P1 to P7, ids that sort as their numbers, all with an address), three
    /// servers and a queue of two. `players` waits while `hold` is set, to keep a read running.
    struct Stub {
        hold: AtomicBool,
        entered: AtomicUsize,
    }

    impl Stub {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                hold: AtomicBool::new(false),
                entered: AtomicUsize::new(0),
            })
        }
    }

    impl Facts for Stub {
        fn players(&self) -> Vec<PlayerInfo> {
            self.entered.fetch_add(1, Ordering::SeqCst);
            let limit = Instant::now() + Duration::from_secs(20);
            while self.hold.load(Ordering::SeqCst) && Instant::now() < limit {
                std::thread::sleep(Duration::from_millis(2));
            }
            (1..=7)
                .rev()
                .map(|n| PlayerInfo {
                    name: format!("P{n}"),
                    uuid: format!("{n}0000000-0000-0000-0000-000000000000"),
                    auth: if n % 2 == 0 {
                        meridian_extension::decide::Auth::Verified
                    } else {
                        meridian_extension::decide::Auth::Unverified
                    },
                    server: "lobby".into(),
                    seconds: 60 * n,
                    address: Some(format!("203.0.113.{n}")),
                })
                .collect()
        }

        fn servers(&self) -> Vec<ServerInfo> {
            vec![
                ServerInfo {
                    name: "lobby".into(),
                    default: true,
                    players: 7,
                    role: Some("lobby".into()),
                    soft_cap: Some(40),
                    hard_cap: Some(50),
                    state: Some("open".into()),
                    ping_ms: Some(4),
                },
                ServerInfo {
                    name: "east".into(),
                    default: false,
                    players: 0,
                    role: Some("game".into()),
                    soft_cap: Some(20),
                    hard_cap: Some(24),
                    state: Some("draining".into()),
                    ping_ms: None,
                },
                ServerInfo {
                    name: "tools".into(),
                    default: false,
                    players: 0,
                    role: None,
                    soft_cap: None,
                    hard_cap: None,
                    state: None,
                    ping_ms: None,
                },
            ]
        }

        fn queue(&self) -> QueueState {
            QueueState {
                enabled: true,
                total: 2,
                waiting: vec![
                    Waiting {
                        position: 1,
                        name: "Dave".into(),
                        seconds: 41,
                    },
                    Waiting {
                        position: 2,
                        name: "Erin".into(),
                        seconds: 9,
                    },
                ],
            }
        }
    }

    /// `orderly`, with the facts of `stub` served.
    fn reading(
        grant: &[&str],
        stub: &Arc<Stub>,
        change: impl FnOnce(&mut Settings),
    ) -> (Fake, Registry, Arc<Link>, Folder) {
        let (fake, registry, link, folder) = orderly(grant, change);
        registry.serve_facts(Arc::clone(stub) as Arc<dyn Facts>);
        (fake, registry, link, folder)
    }

    fn page(reply: &Value) -> PlayersPage {
        serde_json::from_value(reply["result"].clone()).unwrap_or_else(|e| panic!("{e}: {reply}"))
    }

    #[test]
    fn the_orders_that_read_need_orders_read_and_are_refused_as_if_they_did_not_exist_without_it() {
        let (fake, _registry, link, _folder) = reading(&["decide.command"], &Stub::new(), |_| {});
        for (n, method) in [
            shapes::PLAYERS_LIST,
            shapes::PLAYERS_FIND,
            shapes::SERVERS_LIST,
            shapes::QUEUE_LIST,
        ]
        .into_iter()
        .enumerate()
        {
            let id = json!(3000 + n);
            order(&fake, &id, method, r#"{"player":"Steve"}"#);
            let reply = answer(&fake, &id);
            assert_eq!(code_of(&reply), -32601, "{method}");
            assert_eq!(link.stats().orders(method, Ended::Refused), 1, "{method}");
        }
        // Granted, they answer.
        let (fake, _registry, _link, _folder) = reading(&["orders.read"], &Stub::new(), |_| {});
        for (n, (method, params)) in [
            (shapes::PLAYERS_LIST, "{}"),
            (shapes::PLAYERS_FIND, r#"{"player":"P1"}"#),
            (shapes::SERVERS_LIST, "{}"),
            (shapes::QUEUE_LIST, "{}"),
        ]
        .into_iter()
        .enumerate()
        {
            let id = json!(3100 + n);
            order(&fake, &id, method, params);
            assert!(answer(&fake, &id).get("result").is_some(), "{method}");
        }
    }

    #[test]
    fn the_players_come_a_page_at_a_time_in_the_order_of_their_ids_and_the_cursor_goes_on() {
        let (fake, _registry, link, _folder) = reading(&["orders.read"], &Stub::new(), |_| {});
        let mut told = Vec::new();
        let mut cursor: Option<String> = None;
        for n in 0..10 {
            let id = json!(3200 + n);
            let params = match &cursor {
                Some(c) => format!(r#"{{"limit":3,"cursor":"{c}"}}"#),
                None => r#"{"limit":3}"#.to_owned(),
            };
            order(&fake, &id, shapes::PLAYERS_LIST, &params);
            let got = page(&answer(&fake, &id));
            assert_eq!(got.total, 7);
            told.extend(got.players.into_iter().map(|p| p.name));
            match got.next {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        assert_eq!(told, ["P1", "P2", "P3", "P4", "P5", "P6", "P7"]);
        // No parameter at all: the first hundred, which is all seven.
        fake.send(&format!(
            r#"{{"jsonrpc":"2.0","id":3300,"method":"{}"}}"#,
            shapes::PLAYERS_LIST
        ));
        assert_eq!(page(&answer(&fake, &json!(3300))).players.len(), 7);
        // More than a page may hold, or none, or a cursor nobody gave: told so, and nothing is read.
        for (n, params) in [
            r#"{"limit":501}"#,
            r#"{"limit":0}"#,
            r#"{"cursor":"nonsense"}"#,
            r#"{"more":1}"#,
        ]
        .into_iter()
        .enumerate()
        {
            let id = json!(3400 + n);
            order(&fake, &id, shapes::PLAYERS_LIST, params);
            assert_eq!(code_of(&answer(&fake, &id)), -32602, "{params}");
        }
        assert_eq!(link.stats().orders(shapes::PLAYERS_LIST, Ended::Invalid), 4);
    }

    #[test]
    fn a_players_address_is_told_only_to_an_extension_that_was_granted_addresses() {
        let addresses = |grant: &[&str]| -> (Vec<Option<String>>, Option<String>) {
            let (fake, _registry, _link, _folder) = reading(grant, &Stub::new(), |_| {});
            order(&fake, &json!(3500), shapes::PLAYERS_LIST, "{}");
            let listed = page(&answer(&fake, &json!(3500)));
            order(
                &fake,
                &json!(3501),
                shapes::PLAYERS_FIND,
                r#"{"player":"p3"}"#,
            );
            let found: Found =
                serde_json::from_value(answer(&fake, &json!(3501))["result"].clone()).unwrap();
            (
                listed.players.into_iter().map(|p| p.address).collect(),
                found.player.unwrap().address,
            )
        };
        let (listed, found) = addresses(&["orders.read"]);
        assert!(
            listed.iter().all(Option::is_none) && found.is_none(),
            "{listed:?} {found:?}"
        );
        let (listed, found) = addresses(&["orders.read", "addresses"]);
        assert_eq!(listed.len(), 7);
        assert!(listed.iter().all(Option::is_some), "{listed:?}");
        assert_eq!(found.as_deref(), Some("203.0.113.3"));
    }

    #[test]
    fn a_player_is_found_by_name_in_any_case_or_by_id_and_nobody_is_null() {
        let (fake, _registry, _link, _folder) = reading(&["orders.read"], &Stub::new(), |_| {});
        let find = |n: u64, who: &str| -> Option<String> {
            order(
                &fake,
                &json!(3600 + n),
                shapes::PLAYERS_FIND,
                &format!(r#"{{"player":"{who}"}}"#),
            );
            let reply = answer(&fake, &json!(3600 + n));
            let found: Found = serde_json::from_value(reply["result"].clone())
                .unwrap_or_else(|e| panic!("{e}: {reply}"));
            found.player.map(|p| p.name)
        };
        assert_eq!(find(0, "P2").as_deref(), Some("P2"));
        assert_eq!(find(1, "p5").as_deref(), Some("P5"));
        assert_eq!(
            find(2, "40000000-0000-0000-0000-000000000000").as_deref(),
            Some("P4")
        );
        assert_eq!(
            find(3, "60000000000000000000000000000000").as_deref(),
            Some("P6")
        );
        assert_eq!(find(4, "Nobody"), None);
        assert_eq!(find(5, "99000000-0000-0000-0000-000000000000"), None);
        // Something that is neither a name nor an id, or no parameter: told so.
        for (n, params) in [r#"{"player":""}"#, r#"{"player":7}"#, "{}"]
            .into_iter()
            .enumerate()
        {
            let id = json!(3700 + n);
            order(&fake, &id, shapes::PLAYERS_FIND, params);
            assert_eq!(code_of(&answer(&fake, &id)), -32602, "{params}");
        }
    }

    #[test]
    fn the_servers_and_the_queue_are_told_as_facts_and_no_server_address_is_told() {
        let (fake, _registry, _link, _folder) =
            reading(&["orders.read", "addresses"], &Stub::new(), |_| {});
        order(&fake, &json!(3800), shapes::SERVERS_LIST, "{}");
        let reply = answer(&fake, &json!(3800));
        let servers: ServersList = serde_json::from_value(reply["result"].clone()).unwrap();
        assert_eq!(servers.servers.len(), 3);
        assert!(servers.servers[0].default && !servers.servers[1].default);
        assert_eq!(servers.servers[0].state.as_deref(), Some("open"));
        assert_eq!(servers.servers[2].role, None);
        assert!(!reply.to_string().contains("address"), "{reply}");
        order(&fake, &json!(3801), shapes::QUEUE_LIST, "{}");
        let queue: QueueState =
            serde_json::from_value(answer(&fake, &json!(3801))["result"].clone()).unwrap();
        assert_eq!(
            (queue.enabled, queue.total, queue.waiting.len()),
            (true, 2, 2)
        );
        assert_eq!(queue.waiting[0].name, "Dave");
    }

    #[test]
    fn with_no_facts_given_a_read_says_it_is_not_available_and_nothing_breaks() {
        // `orderly` serves none: the registry was never given the proxy's.
        let (fake, _registry, link, _folder) = orderly(&["orders.read"], |_| {});
        order(&fake, &json!(3900), shapes::PLAYERS_LIST, "{}");
        let reply = answer(&fake, &json!(3900));
        assert_eq!(
            (code_of(&reply), reply["error"]["message"].as_str()),
            (-32603, Some("not available"))
        );
        assert_eq!(link.stats().orders(shapes::PLAYERS_LIST, Ended::Failed), 1);
        assert!(link.is_connected() && ping(&link, Duration::from_secs(2)).is_ok());
    }

    /// A read costs one order of the budget and holds a place while it runs, like any order: over the budget it is
    /// refused with -32001, and with the places taken with -32002, and neither reads anything.
    #[test]
    fn a_read_is_refused_over_the_budget_and_when_the_places_are_taken() {
        let stub = Stub::new();
        let (fake, _registry, link, _folder) = reading(&["orders.read"], &stub, |settings| {
            settings.order_burst = 3;
            settings.order_rate = 1;
            settings.orders_running = 1;
        });
        // The place: a read that is held takes it, and the next is refused at once.
        stub.hold.store(true, Ordering::SeqCst);
        order(&fake, &json!(4000), shapes::PLAYERS_LIST, "{}");
        until("the read to start", || {
            stub.entered.load(Ordering::SeqCst) == 1
        });
        order(&fake, &json!(4001), shapes::SERVERS_LIST, "{}");
        assert_eq!(code_of(&answer(&fake, &json!(4001))), -32002);
        assert_eq!(stub.entered.load(Ordering::SeqCst), 1);
        stub.hold.store(false, Ordering::SeqCst);
        assert_eq!(page(&answer(&fake, &json!(4000))).total, 7);
        assert_eq!(link.stats().orders(shapes::SERVERS_LIST, Ended::Busy), 1);
        // The budget: three tokens, one spent on the held read (the refused one spent none): two more, then over.
        let mut over = 0u64;
        for n in 0..4 {
            let id = json!(4100 + n);
            order(&fake, &id, shapes::QUEUE_LIST, "{}");
            let reply = answer(&fake, &id);
            if reply.get("error").is_some() {
                assert_eq!(code_of(&reply), -32001);
                over += 1;
            }
        }
        assert!((1..=3).contains(&over), "{over} were over the budget");
        assert_eq!(
            link.stats().orders(shapes::QUEUE_LIST, Ended::OverBudget),
            over
        );
    }
}
