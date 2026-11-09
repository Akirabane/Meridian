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
        events: Default::default(),
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
        events: Default::default(),
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
        assert_eq!(
            methods(&["orders.message"]),
            ["meridian:players/tell", "meridian:broadcast"]
        );
        assert_eq!(methods(&["orders.move"]), ["meridian:players/send"]);
        assert_eq!(methods(&["orders.moderate"]), ["meridian:players/kick"]);
        assert_eq!(
            methods(&["orders.operate"]),
            [
                "meridian:servers/drain",
                "meridian:servers/undrain",
                "meridian:reload"
            ]
        );
        assert_eq!(methods(&["orders.console"]), ["meridian:console/execute"]);
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
        fn players(&self, with_addresses: bool) -> Vec<PlayerInfo> {
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
                    address: with_addresses.then(|| format!("203.0.113.{n}")),
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

    /// The two orders that take no parameters refuse any: a program that thinks it is filtering finds out.
    #[test]
    fn the_orders_that_take_no_parameters_refuse_any_and_take_none_as_none() {
        let (fake, _registry, link, _folder) = reading(&["orders.read"], &Stub::new(), |_| {});
        for (n, method) in [shapes::SERVERS_LIST, shapes::QUEUE_LIST]
            .into_iter()
            .enumerate()
        {
            let n = n as u64 * 10;
            for (k, params) in [r#"{"x":1}"#, "[1]", r#""all""#, "7"]
                .into_iter()
                .enumerate()
            {
                let id = json!(5000 + n + k as u64);
                order(&fake, &id, method, params);
                let reply = answer(&fake, &id);
                assert_eq!(code_of(&reply), -32602, "{method} {params}");
                assert_eq!(reply["error"]["message"], "this order takes no parameters");
            }
            for (k, params) in ["{}", "null"].into_iter().enumerate() {
                let id = json!(5100 + n + k as u64);
                order(&fake, &id, method, params);
                assert!(
                    answer(&fake, &id).get("result").is_some(),
                    "{method} {params}"
                );
            }
            // And none at all.
            let id = json!(5200 + n);
            fake.send(&format!(
                r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}"}}"#
            ));
            assert!(answer(&fake, &id).get("result").is_some(), "{method}");
            assert_eq!(link.stats().orders(method, Ended::Invalid), 4, "{method}");
        }
    }

    /// A limit and a cursor that are null are the first page, as if they were left out.
    #[test]
    fn a_page_with_a_null_limit_and_cursor_is_the_first_page() {
        let (fake, _registry, _link, _folder) = reading(&["orders.read"], &Stub::new(), |_| {});
        order(
            &fake,
            &json!(5300),
            shapes::PLAYERS_LIST,
            r#"{"limit":null,"cursor":null}"#,
        );
        assert_eq!(page(&answer(&fake, &json!(5300))).players.len(), 7);
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

    // ---- the orders that message ----------------------------------------------------------------------

    use crate::extension::link::SendLot;
    use crate::extension::orders::Actions;
    use crate::extension::orders::{KickRefusal, MovePlan, Paid, Refusal};
    use meridian_extension::orders::{Started, Target, Told, Who};
    use std::sync::Mutex as StdMutex;

    /// Actions that are made up: they record what they were asked, `Nobody` is not online, `east` has two players, any
    /// other server is not one, and "every server" reaches `everyone` players.
    struct Acting {
        told: StdMutex<Vec<(Who, String)>>,
        cast: StdMutex<Vec<(Option<String>, String)>>,
        /// The plans that were started: whom, and where to.
        started: StdMutex<Vec<(Target, String)>>,
        /// Who was kicked, and the reason they were given.
        kicked: StdMutex<Vec<(Who, String)>>,
        /// Whether the plans that are started keep their place for a send, as a thread that is still moving players does.
        hold_lots: AtomicBool,
        /// The places for a send that the started plans hold.
        lots: StdMutex<Vec<SendLot>>,
        everyone: AtomicUsize,
    }

    /// A plan of the stand-in: it records itself when it is started.
    struct Plan {
        count: usize,
        entry: (Target, String),
        log: Arc<Acting>,
    }

    impl MovePlan for Plan {
        fn count(&self) -> usize {
            self.count
        }

        fn start(self: Box<Self>, _paid: Paid, lot: SendLot) -> Result<usize, Refusal> {
            self.log.started.lock().unwrap().push(self.entry.clone());
            if self.log.hold_lots.load(Ordering::SeqCst) {
                self.log.lots.lock().unwrap().push(lot);
            }
            Ok(self.count)
        }
    }

    /// The stand-in as the registry holds it: an `Arc`, so that the plans it makes can record themselves in it.
    struct ActingArc(Arc<Acting>);

    impl std::ops::Deref for ActingArc {
        type Target = Acting;
        fn deref(&self) -> &Acting {
            &self.0
        }
    }

    impl Acting {
        fn new(everyone: usize) -> Arc<Self> {
            Arc::new(Self {
                told: StdMutex::new(Vec::new()),
                cast: StdMutex::new(Vec::new()),
                started: StdMutex::new(Vec::new()),
                kicked: StdMutex::new(Vec::new()),
                hold_lots: AtomicBool::new(false),
                lots: StdMutex::new(Vec::new()),
                everyone: AtomicUsize::new(everyone),
            })
        }

        fn kicked(&self) -> Vec<(Who, String)> {
            self.kicked.lock().unwrap().clone()
        }

        /// The moves end: the places for a send are let go.
        fn finish_the_moves(&self) {
            self.lots.lock().unwrap().clear();
        }

        fn started(&self) -> Vec<(Target, String)> {
            self.started.lock().unwrap().clone()
        }

        fn told(&self) -> Vec<(Who, String)> {
            self.told.lock().unwrap().clone()
        }

        fn cast(&self) -> Vec<(Option<String>, String)> {
            self.cast.lock().unwrap().clone()
        }
    }

    impl Actions for ActingArc {
        /// `Nobody` is not online, `Twin` is two accounts and `Boss` cannot be kicked by an extension: the others are
        /// kicked, and named as they were asked for.
        fn kick(&self, who: &Who, reason: &str) -> Result<Option<String>, KickRefusal> {
            self.kicked
                .lock()
                .unwrap()
                .push((who.clone(), reason.to_owned()));
            match who {
                Who::Name(name) if name.eq_ignore_ascii_case("Nobody") => Ok(None),
                Who::Name(name) if name.eq_ignore_ascii_case("Twin") => Err(KickRefusal::Several),
                Who::Name(name) if name.eq_ignore_ascii_case("Boss") => Err(KickRefusal::Exempt),
                Who::Name(name) => Ok(Some(name.clone())),
                Who::Id(_) => Ok(Some("ById".to_owned())),
            }
        }

        /// `nowhere` is a server nobody has; `Nobody` is not online; a server's players are two; all are `everyone`.
        fn plan_send(&self, target: &Target, to: &str) -> Result<Box<dyn MovePlan>, Refusal> {
            if to == "nowhere" {
                return Err(Refusal::UnknownDestination);
            }
            if to == "drained" {
                return Err(Refusal::DestinationNotTakingPlayers);
            }
            let count = match target {
                Target::Server(from) if from == "nowhere" => return Err(Refusal::UnknownSource),
                Target::Server(_) => 2,
                Target::All => self.everyone.load(Ordering::SeqCst),
                Target::Player(Who::Name(name)) if name.eq_ignore_ascii_case("Nobody") => 0,
                Target::Player(_) => 1,
            };
            Ok(Box::new(Plan {
                count,
                entry: (target.clone(), to.to_owned()),
                log: Arc::clone(&self.0),
            }))
        }

        fn tell(&self, who: &Who, text: &str) -> usize {
            self.told
                .lock()
                .unwrap()
                .push((who.clone(), text.to_owned()));
            usize::from(!matches!(who, Who::Name(name) if name.eq_ignore_ascii_case("Nobody")))
        }

        fn audience(&self, server: Option<&str>) -> Result<usize, String> {
            match server {
                None => Ok(self.everyone.load(Ordering::SeqCst)),
                Some("east") => Ok(2),
                Some(other) => Err(format!("there is no server called {other}")),
            }
        }

        fn broadcast(&self, server: Option<&str>, text: &str) -> Result<usize, String> {
            let reached = self.audience(server)?;
            self.cast
                .lock()
                .unwrap()
                .push((server.map(str::to_owned), text.to_owned()));
            Ok(reached)
        }
    }

    /// `orderly`, with the actions of `acting` served.
    fn acting_with(
        grant: &[&str],
        acting: &Arc<Acting>,
        change: impl FnOnce(&mut Settings),
    ) -> (Fake, Registry, Arc<Link>, Folder) {
        let (fake, registry, link, folder) = orderly(grant, change);
        registry.serve_actions(Arc::new(ActingArc(Arc::clone(acting))) as Arc<dyn Actions>);
        (fake, registry, link, folder)
    }

    fn told_of(reply: &Value) -> usize {
        serde_json::from_value::<Told>(reply["result"].clone())
            .unwrap_or_else(|e| panic!("{e}: {reply}"))
            .told
    }

    #[test]
    fn the_orders_that_message_need_orders_message_and_are_refused_as_if_they_did_not_exist_without_it()
     {
        let acting = Acting::new(5);
        // `orders.read` is granted, `orders.message` is not.
        let (fake, _registry, link, _folder) = acting_with(&["orders.read"], &acting, |_| {});
        order(
            &fake,
            &json!(6000),
            shapes::PLAYERS_TELL,
            r#"{"player":"Alex","text":"hi"}"#,
        );
        order(&fake, &json!(6001), shapes::BROADCAST, r#"{"text":"hi"}"#);
        for id in [6000, 6001] {
            assert_eq!(code_of(&answer(&fake, &json!(id))), -32601);
        }
        assert!(acting.told().is_empty() && acting.cast().is_empty());
        assert_eq!(link.stats().orders(shapes::PLAYERS_TELL, Ended::Refused), 1);
        assert_eq!(link.stats().orders(shapes::BROADCAST, Ended::Refused), 1);
        // Granted, they act.
        let (fake, _registry, _link, _folder) = acting_with(&["orders.message"], &acting, |_| {});
        order(
            &fake,
            &json!(6100),
            shapes::PLAYERS_TELL,
            r#"{"player":"Alex","text":"hi"}"#,
        );
        assert_eq!(told_of(&answer(&fake, &json!(6100))), 1);
        order(&fake, &json!(6101), shapes::BROADCAST, r#"{"text":"hi"}"#);
        assert_eq!(told_of(&answer(&fake, &json!(6101))), 5);
    }

    #[test]
    fn a_player_is_told_the_line_exactly_as_written_and_one_who_is_not_online_is_told_nothing() {
        let acting = Acting::new(5);
        let (fake, _registry, _link, _folder) = acting_with(&["orders.message"], &acting, |_| {});
        // Characters that would mean something in a JSON text component or in HTML are only characters.
        let line = r#"Click <b>here</b> & {"text":"x","clickEvent":1} 'quoted'"#;
        let params = serde_json::to_string(&json!({"player": "alex", "text": line})).unwrap();
        order(&fake, &json!(6200), shapes::PLAYERS_TELL, &params);
        assert_eq!(told_of(&answer(&fake, &json!(6200))), 1);
        order(
            &fake,
            &json!(6201),
            shapes::PLAYERS_TELL,
            r#"{"player":"0A2EA4BABCED39D280E11AD51A0E925C","text":"by id"}"#,
        );
        assert_eq!(told_of(&answer(&fake, &json!(6201))), 1);
        order(
            &fake,
            &json!(6202),
            shapes::PLAYERS_TELL,
            r#"{"player":"Nobody","text":"hello?"}"#,
        );
        assert_eq!(told_of(&answer(&fake, &json!(6202))), 0);
        let told = acting.told();
        assert_eq!(told[0], (Who::Name("alex".into()), line.to_owned()));
        assert_eq!(
            told[1].0,
            Who::Id("0a2ea4ba-bced-39d2-80e1-1ad51a0e925c".into())
        );
        assert_eq!(told.len(), 3);
    }

    #[test]
    fn a_line_that_is_not_valid_is_refused_with_32602_and_nobody_is_told() {
        let acting = Acting::new(5);
        let (fake, _registry, link, _folder) = acting_with(&["orders.message"], &acting, |_| {});
        let secret = "an-id-that-must-not-come-back";
        let long = "x".repeat(shapes::MAX_TEXT + 1);
        let cases = [
            serde_json::to_string(&json!({"player": "Alex", "text": ""})).unwrap(),
            serde_json::to_string(&json!({"player": "Alex", "text": long})).unwrap(),
            serde_json::to_string(&json!({"player": "Alex", "text": format!("{secret}\n")}))
                .unwrap(),
            serde_json::to_string(&json!({"player": "Alex", "text": format!("{secret}\u{a7}4")}))
                .unwrap(),
            serde_json::to_string(&json!({"player": "Alex", "text": format!("{secret}\u{202e}")}))
                .unwrap(),
            serde_json::to_string(&json!({"player": secret.repeat(3), "text": "hi"})).unwrap(),
            r#"{"player":"Alex"}"#.to_owned(),
            r#"["Alex","hi"]"#.to_owned(),
        ];
        for (n, params) in cases.iter().enumerate() {
            let id = json!(6300 + n);
            order(&fake, &id, shapes::PLAYERS_TELL, params);
            let reply = answer(&fake, &id);
            assert_eq!(code_of(&reply), -32602, "{params}");
            assert!(!reply.to_string().contains(secret), "{reply}");
        }
        assert!(acting.told().is_empty());
        assert_eq!(
            link.stats().orders(shapes::PLAYERS_TELL, Ended::Invalid),
            cases.len() as u64
        );
    }

    #[test]
    fn a_broadcast_reaches_every_player_or_those_of_one_server_and_a_server_nobody_has_is_refused()
    {
        let acting = Acting::new(5);
        let (fake, _registry, _link, _folder) = acting_with(&["orders.message"], &acting, |_| {});
        order(
            &fake,
            &json!(6400),
            shapes::BROADCAST,
            r#"{"text":"Restart in 5 minutes."}"#,
        );
        assert_eq!(told_of(&answer(&fake, &json!(6400))), 5);
        order(
            &fake,
            &json!(6401),
            shapes::BROADCAST,
            r#"{"server":"east","text":"Hello east."}"#,
        );
        assert_eq!(told_of(&answer(&fake, &json!(6401))), 2);
        let secret = "server-name-that-must-not-come-back";
        order(
            &fake,
            &json!(6402),
            shapes::BROADCAST,
            &format!(r#"{{"server":"{secret}","text":"hi"}}"#),
        );
        let reply = answer(&fake, &json!(6402));
        assert_eq!(code_of(&reply), -32602);
        assert!(!reply.to_string().contains(secret), "{reply}");
        assert_eq!(
            acting.cast(),
            [
                (None, "Restart in 5 minutes.".to_owned()),
                (Some("east".to_owned()), "Hello east.".to_owned())
            ]
        );
    }

    /// A broadcast costs the budget the number of players it reaches, and at most the burst (a `send all` of 500 players
    /// could never run if it cost 500), and one the budget cannot pay is refused before anyone is told.
    #[test]
    fn a_broadcast_costs_the_players_it_reaches_up_to_the_burst_and_is_refused_before_anyone_is_told_if_it_cannot_be_paid()
     {
        // Two tokens of three are spent by two tells: a broadcast to five (cost: the three of the burst) cannot be paid.
        let acting = Acting::new(5);
        let (fake, _registry, link, _folder) =
            acting_with(&["orders.message"], &acting, |settings| {
                settings.order_burst = 3;
                settings.order_rate = 1;
            });
        for n in 0..2 {
            order(
                &fake,
                &json!(6500 + n),
                shapes::PLAYERS_TELL,
                r#"{"player":"Alex","text":"hi"}"#,
            );
            assert_eq!(told_of(&answer(&fake, &json!(6500 + n))), 1);
        }
        order(
            &fake,
            &json!(6510),
            shapes::BROADCAST,
            r#"{"text":"to everyone"}"#,
        );
        let reply = answer(&fake, &json!(6510));
        assert_eq!(code_of(&reply), -32001);
        assert!(acting.cast().is_empty(), "nobody was told");
        assert_eq!(link.stats().orders(shapes::BROADCAST, Ended::OverBudget), 1);

        // A rested budget of four, and a broadcast to 500: it costs four (the burst), and it runs.
        let acting = Acting::new(500);
        let (fake, _registry, _link, _folder) =
            acting_with(&["orders.message"], &acting, |settings| {
                settings.order_burst = 4;
                settings.order_rate = 1;
            });
        order(
            &fake,
            &json!(6600),
            shapes::BROADCAST,
            r#"{"text":"to 500"}"#,
        );
        assert_eq!(told_of(&answer(&fake, &json!(6600))), 500);
        // It took the whole burst: the next order, at once, is over the budget.
        order(
            &fake,
            &json!(6601),
            shapes::PLAYERS_TELL,
            r#"{"player":"Alex","text":"hi"}"#,
        );
        assert_eq!(code_of(&answer(&fake, &json!(6601))), -32001);
        assert_eq!(acting.told().len(), 0);
    }

    #[test]
    fn with_no_actions_given_an_order_that_messages_says_it_is_not_available_and_nothing_breaks() {
        // `orderly` serves none.
        let (fake, _registry, link, _folder) = orderly(&["orders.message"], |_| {});
        order(
            &fake,
            &json!(6700),
            shapes::PLAYERS_TELL,
            r#"{"player":"Alex","text":"hi"}"#,
        );
        order(&fake, &json!(6701), shapes::BROADCAST, r#"{"text":"hi"}"#);
        for id in [6700, 6701] {
            let reply = answer(&fake, &json!(id));
            assert_eq!(
                (code_of(&reply), reply["error"]["message"].as_str()),
                (-32603, Some("not available"))
            );
        }
        assert_eq!(link.stats().orders(shapes::BROADCAST, Ended::Failed), 1);
        assert!(link.is_connected() && ping(&link, Duration::from_secs(2)).is_ok());
    }

    /// Two broadcasts that each need the whole burst, sent together: one is paid, the other is refused (not both
    /// half-charged and both refused), and nobody is told by the one that is refused.
    #[test]
    fn two_broadcasts_that_each_need_the_whole_burst_cannot_both_take_part_of_it() {
        let acting = Acting::new(50);
        let (fake, _registry, link, _folder) =
            acting_with(&["orders.message"], &acting, |settings| {
                settings.order_burst = 50;
                settings.order_rate = 1;
                settings.orders_running = 8;
            });
        order(
            &fake,
            &json!(7000),
            shapes::BROADCAST,
            r#"{"text":"first"}"#,
        );
        order(
            &fake,
            &json!(7001),
            shapes::BROADCAST,
            r#"{"text":"second"}"#,
        );
        let replies = [answer(&fake, &json!(7000)), answer(&fake, &json!(7001))];
        let paid = replies.iter().filter(|r| r.get("result").is_some()).count();
        let refused = replies.iter().filter(|r| r.get("error").is_some()).count();
        assert_eq!((paid, refused), (1, 1), "{replies:?}");
        assert!(
            replies
                .iter()
                .filter_map(|r| r.get("error"))
                .all(|e| e["code"] == -32001)
        );
        assert_eq!(
            acting.cast().len(),
            1,
            "only the one that was paid told anyone"
        );
        assert_eq!(link.stats().orders(shapes::BROADCAST, Ended::Done), 1);
        assert_eq!(link.stats().orders(shapes::BROADCAST, Ended::OverBudget), 1);
    }

    /// A broadcast the budget cannot pay is refused, shows nothing, and costs one token anyway: with one token left, the
    /// order after it finds none.
    #[test]
    fn a_refused_broadcast_costs_one_token_and_shows_nothing() {
        let acting = Acting::new(5);
        let (fake, _registry, _link, _folder) =
            acting_with(&["orders.message"], &acting, |settings| {
                settings.order_burst = 6;
                settings.order_rate = 1;
            });
        // 5 of 6: a broadcast to five players is paid, and one token is left.
        order(
            &fake,
            &json!(7100),
            shapes::BROADCAST,
            r#"{"text":"to five"}"#,
        );
        assert_eq!(told_of(&answer(&fake, &json!(7100))), 5);
        // Another to five needs five: it is refused, shows nothing, and takes the one token that is left.
        order(
            &fake,
            &json!(7101),
            shapes::BROADCAST,
            r#"{"text":"to five again"}"#,
        );
        assert_eq!(code_of(&answer(&fake, &json!(7101))), -32001);
        assert_eq!(acting.cast().len(), 1);
        // So the next order, which would have cost one, finds nothing at the door.
        order(
            &fake,
            &json!(7102),
            shapes::PLAYERS_TELL,
            r#"{"player":"Alex","text":"hi"}"#,
        );
        assert_eq!(code_of(&answer(&fake, &json!(7102))), -32001);
        assert!(acting.told().is_empty());
    }

    // ---- the order that moves --------------------------------------------------------------------------

    fn started_of(reply: &Value) -> usize {
        serde_json::from_value::<Started>(reply["result"].clone())
            .unwrap_or_else(|e| panic!("{e}: {reply}"))
            .started
    }

    #[test]
    fn the_order_that_moves_needs_orders_move_and_is_refused_as_if_it_did_not_exist_without_it() {
        let acting = Acting::new(5);
        // `orders.message` and `orders.read` are granted, `orders.move` is not.
        let (fake, _registry, link, _folder) =
            acting_with(&["orders.message", "orders.read"], &acting, |_| {});
        order(
            &fake,
            &json!(8000),
            shapes::PLAYERS_SEND,
            r#"{"to":"east","all":true}"#,
        );
        assert_eq!(code_of(&answer(&fake, &json!(8000))), -32601);
        assert!(acting.started().is_empty());
        assert_eq!(link.stats().orders(shapes::PLAYERS_SEND, Ended::Refused), 1);
        // Granted, it starts.
        let (fake, _registry, _link, _folder) = acting_with(&["orders.move"], &acting, |_| {});
        order(
            &fake,
            &json!(8100),
            shapes::PLAYERS_SEND,
            r#"{"to":"east","all":true}"#,
        );
        assert_eq!(started_of(&answer(&fake, &json!(8100))), 5);
    }

    #[test]
    fn one_player_every_player_or_those_of_a_server_are_sent_and_the_answer_is_how_many_started() {
        let acting = Acting::new(5);
        let (fake, _registry, _link, _folder) = acting_with(&["orders.move"], &acting, |_| {});
        order(
            &fake,
            &json!(8200),
            shapes::PLAYERS_SEND,
            r#"{"to":"east","player":"Alex"}"#,
        );
        assert_eq!(started_of(&answer(&fake, &json!(8200))), 1);
        order(
            &fake,
            &json!(8201),
            shapes::PLAYERS_SEND,
            r#"{"to":"east","player":"0A2EA4BABCED39D280E11AD51A0E925C"}"#,
        );
        assert_eq!(started_of(&answer(&fake, &json!(8201))), 1);
        order(
            &fake,
            &json!(8202),
            shapes::PLAYERS_SEND,
            r#"{"to":"east","all":true}"#,
        );
        assert_eq!(started_of(&answer(&fake, &json!(8202))), 5);
        order(
            &fake,
            &json!(8203),
            shapes::PLAYERS_SEND,
            r#"{"to":"east","from":"lobby"}"#,
        );
        assert_eq!(started_of(&answer(&fake, &json!(8203))), 2);
        // Nobody to move: not an error, `0`.
        order(
            &fake,
            &json!(8204),
            shapes::PLAYERS_SEND,
            r#"{"to":"east","player":"Nobody"}"#,
        );
        assert_eq!(started_of(&answer(&fake, &json!(8204))), 0);
        assert_eq!(
            acting.started(),
            [
                (Target::Player(Who::Name("Alex".into())), "east".to_owned()),
                (
                    Target::Player(Who::Id("0a2ea4ba-bced-39d2-80e1-1ad51a0e925c".into())),
                    "east".to_owned()
                ),
                (Target::All, "east".to_owned()),
                (Target::Server("lobby".into()), "east".to_owned()),
                (
                    Target::Player(Who::Name("Nobody".into())),
                    "east".to_owned()
                ),
            ]
        );
    }

    #[test]
    fn a_server_the_proxy_does_not_have_is_refused_in_fixed_words_and_nobody_is_moved() {
        let acting = Acting::new(5);
        let (fake, _registry, link, _folder) = acting_with(&["orders.move"], &acting, |_| {});
        let secret = "name-that-must-not-come-back";
        order(
            &fake,
            &json!(8300),
            shapes::PLAYERS_SEND,
            "{\"to\":\"nowhere\",\"all\":true}",
        );
        order(
            &fake,
            &json!(8301),
            shapes::PLAYERS_SEND,
            "{\"to\":\"east\",\"from\":\"nowhere\"}",
        );
        for id in [8300, 8301] {
            let reply = answer(&fake, &json!(id));
            assert_eq!(code_of(&reply), -32602, "{reply}");
            assert!(
                reply["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("is not a server of the proxy")
            );
        }
        // What was written is not given back.
        order(
            &fake,
            &json!(8302),
            shapes::PLAYERS_SEND,
            &format!("{{\"to\":\"{secret}\\n\",\"all\":true}}"),
        );
        let reply = answer(&fake, &json!(8302));
        assert_eq!(code_of(&reply), -32602);
        assert!(!reply.to_string().contains(secret), "{reply}");
        // Parameters that are not one of the three ways to say whom.
        for (n, params) in [
            r#"{"to":"east"}"#,
            r#"{"to":"east","player":"Alex","all":true}"#,
            r#"{"to":"east","all":false}"#,
            r#"{"all":true}"#,
            r#"["east"]"#,
        ]
        .into_iter()
        .enumerate()
        {
            let id = json!(8310 + n);
            order(&fake, &id, shapes::PLAYERS_SEND, params);
            assert_eq!(code_of(&answer(&fake, &id)), -32602, "{params}");
        }
        assert!(acting.started().is_empty());
        assert_eq!(link.stats().orders(shapes::PLAYERS_SEND, Ended::Invalid), 8);
    }

    /// The order costs the players it moves, at most the burst, charged all at once: a `send all` of 500 players runs on a
    /// rested budget, and one that the budget cannot pay is refused before anybody is moved.
    #[test]
    fn the_order_that_moves_costs_the_players_it_moves_up_to_the_burst_and_moves_nobody_if_it_cannot_be_paid()
     {
        let acting = Acting::new(500);
        let (fake, _registry, link, _folder) = acting_with(&["orders.move"], &acting, |settings| {
            settings.order_burst = 4;
            settings.order_rate = 1;
        });
        order(
            &fake,
            &json!(8400),
            shapes::PLAYERS_SEND,
            r#"{"to":"east","all":true}"#,
        );
        assert_eq!(started_of(&answer(&fake, &json!(8400))), 500);
        // It took the whole burst: the next, at once, is over the budget and moves nobody.
        order(
            &fake,
            &json!(8401),
            shapes::PLAYERS_SEND,
            r#"{"to":"east","from":"lobby"}"#,
        );
        assert_eq!(code_of(&answer(&fake, &json!(8401))), -32001);
        assert_eq!(acting.started().len(), 1);
        assert_eq!(
            link.stats().orders(shapes::PLAYERS_SEND, Ended::OverBudget),
            1
        );
    }

    #[test]
    fn with_no_actions_given_the_order_that_moves_says_it_is_not_available() {
        let (fake, _registry, link, _folder) = orderly(&["orders.move"], |_| {});
        order(
            &fake,
            &json!(8500),
            shapes::PLAYERS_SEND,
            r#"{"to":"east","all":true}"#,
        );
        let reply = answer(&fake, &json!(8500));
        assert_eq!(
            (code_of(&reply), reply["error"]["message"].as_str()),
            (-32603, Some("not available"))
        );
        assert_eq!(link.stats().orders(shapes::PLAYERS_SEND, Ended::Failed), 1);
        assert!(link.is_connected() && ping(&link, Duration::from_secs(2)).is_ok());
    }

    // ---- the order that expels -------------------------------------------------------------------------

    fn kicked_of(reply: &Value) -> usize {
        serde_json::from_value::<meridian_extension::orders::Kicked>(reply["result"].clone())
            .unwrap_or_else(|e| panic!("{e}: {reply}"))
            .kicked
    }

    #[test]
    fn the_order_that_expels_needs_orders_moderate_and_is_refused_as_if_it_did_not_exist_without_it()
     {
        let acting = Acting::new(5);
        // The two other powers over players are granted, this one is not.
        let (fake, _registry, link, _folder) =
            acting_with(&["orders.move", "orders.message"], &acting, |_| {});
        order(
            &fake,
            &json!(9000),
            shapes::PLAYERS_KICK,
            r#"{"player":"Alex"}"#,
        );
        assert_eq!(code_of(&answer(&fake, &json!(9000))), -32601);
        assert!(acting.kicked().is_empty());
        assert_eq!(link.stats().orders(shapes::PLAYERS_KICK, Ended::Refused), 1);
        let (fake, _registry, _link, _folder) = acting_with(&["orders.moderate"], &acting, |_| {});
        order(
            &fake,
            &json!(9100),
            shapes::PLAYERS_KICK,
            r#"{"player":"Alex"}"#,
        );
        assert_eq!(kicked_of(&answer(&fake, &json!(9100))), 1);
    }

    #[test]
    fn a_player_is_kicked_by_name_or_id_with_a_reason_as_written_or_the_default_and_nobody_is_zero()
    {
        let acting = Acting::new(5);
        let (fake, _registry, _link, _folder) = acting_with(&["orders.moderate"], &acting, |_| {});
        let line = r#"Spam <b>again</b> & {"clickEvent":1}"#;
        let params = serde_json::to_string(&json!({"player": "alex", "reason": line})).unwrap();
        order(&fake, &json!(9200), shapes::PLAYERS_KICK, &params);
        assert_eq!(kicked_of(&answer(&fake, &json!(9200))), 1);
        order(
            &fake,
            &json!(9201),
            shapes::PLAYERS_KICK,
            r#"{"player":"0A2EA4BABCED39D280E11AD51A0E925C"}"#,
        );
        assert_eq!(kicked_of(&answer(&fake, &json!(9201))), 1);
        order(
            &fake,
            &json!(9202),
            shapes::PLAYERS_KICK,
            r#"{"player":"Nobody","reason":"hello?"}"#,
        );
        assert_eq!(kicked_of(&answer(&fake, &json!(9202))), 0);
        let kicked = acting.kicked();
        assert_eq!(kicked[0], (Who::Name("alex".into()), line.to_owned()));
        assert_eq!(
            kicked[1],
            (
                Who::Id("0a2ea4ba-bced-39d2-80e1-1ad51a0e925c".into()),
                shapes::DEFAULT_KICK_REASON.to_owned()
            )
        );
        assert_eq!(kicked.len(), 3);
    }

    #[test]
    fn a_reason_that_is_not_valid_is_refused_with_32602_and_nobody_is_kicked() {
        let acting = Acting::new(5);
        let (fake, _registry, link, _folder) = acting_with(&["orders.moderate"], &acting, |_| {});
        let secret = "reason-that-must-not-come-back";
        let long = "x".repeat(shapes::MAX_TEXT + 1);
        let cases = [
            serde_json::to_string(&json!({"player": "Alex", "reason": ""})).unwrap(),
            serde_json::to_string(&json!({"player": "Alex", "reason": long})).unwrap(),
            serde_json::to_string(&json!({"player": "Alex", "reason": format!("{secret}\n")}))
                .unwrap(),
            serde_json::to_string(&json!({"player": "Alex", "reason": format!("{secret}\u{a7}4")}))
                .unwrap(),
            r#"{"reason":"Spam."}"#.to_owned(),
            r#"["Alex","Spam."]"#.to_owned(),
        ];
        for (n, params) in cases.iter().enumerate() {
            let id = json!(9300 + n);
            order(&fake, &id, shapes::PLAYERS_KICK, params);
            let reply = answer(&fake, &id);
            assert_eq!(code_of(&reply), -32602, "{params}");
            assert!(!reply.to_string().contains(secret), "{reply}");
        }
        assert!(acting.kicked().is_empty());
        assert_eq!(
            link.stats().orders(shapes::PLAYERS_KICK, Ended::Invalid),
            cases.len() as u64
        );
    }

    /// A kick costs one order, like any: there is no other limit, and a budget that is spent refuses it.
    #[test]
    fn a_kick_costs_one_order_and_a_spent_budget_refuses_it() {
        let acting = Acting::new(5);
        let (fake, _registry, _link, _folder) =
            acting_with(&["orders.moderate"], &acting, |settings| {
                settings.order_burst = 2;
                settings.order_rate = 1;
            });
        for n in 0..2 {
            order(
                &fake,
                &json!(9400 + n),
                shapes::PLAYERS_KICK,
                r#"{"player":"Alex"}"#,
            );
            assert_eq!(kicked_of(&answer(&fake, &json!(9400 + n))), 1);
        }
        order(
            &fake,
            &json!(9410),
            shapes::PLAYERS_KICK,
            r#"{"player":"Alex"}"#,
        );
        assert_eq!(code_of(&answer(&fake, &json!(9410))), -32001);
        assert_eq!(acting.kicked().len(), 2, "the refused one kicked nobody");
    }

    /// A name that two accounts have, and a player who may not be kicked by an extension, are refused in words that are
    /// fixed: nobody is kicked, and the place among the kicks of the minute is not used.
    #[test]
    fn a_kick_of_a_name_two_accounts_have_or_of_an_exempt_player_is_refused_in_fixed_words_and_costs_no_place()
     {
        let acting = Acting::new(5);
        let (fake, _registry, link, _folder) =
            acting_with(&["orders.moderate"], &acting, |settings| {
                settings.kicks_per_minute = 1;
            });
        let cases = [
            (
                "Twin",
                "more than one player has that name: use the unique id",
            ),
            (
                "twin",
                "more than one player has that name: use the unique id",
            ),
            ("Boss", "that player cannot be kicked by an extension"),
        ];
        for (n, (name, words)) in cases.iter().enumerate() {
            let id = json!(9600 + n);
            let params =
                serde_json::to_string(&json!({"player": name, "reason": "secret-reason"})).unwrap();
            order(&fake, &id, shapes::PLAYERS_KICK, &params);
            let reply = answer(&fake, &id);
            assert_eq!(code_of(&reply), -32602, "{name}");
            assert_eq!(reply["error"]["message"].as_str(), Some(*words), "{name}");
            assert!(!reply.to_string().contains("secret-reason"), "{reply}");
        }
        assert_eq!(link.stats().orders(shapes::PLAYERS_KICK, Ended::Invalid), 3);
        // None of the three kept the one place of the minute: Alex can still be kicked.
        order(
            &fake,
            &json!(9610),
            shapes::PLAYERS_KICK,
            r#"{"player":"Alex"}"#,
        );
        assert_eq!(kicked_of(&answer(&fake, &json!(9610))), 1);
    }

    /// `max-kicks-per-minute`: past it a kick is `-32002 too many kicks` and kicks nobody; only a kick that ended a session
    /// counts, so asking after players who are not there does not spend it.
    #[test]
    fn the_kicks_of_a_minute_are_limited_when_the_operator_says_so_and_only_a_kick_that_kicked_counts()
     {
        let acting = Acting::new(5);
        let (fake, _registry, _link, _folder) =
            acting_with(&["orders.moderate"], &acting, |settings| {
                settings.kicks_per_minute = 2;
            });
        for n in 0..5 {
            order(
                &fake,
                &json!(9700 + n),
                shapes::PLAYERS_KICK,
                r#"{"player":"Nobody"}"#,
            );
            assert_eq!(kicked_of(&answer(&fake, &json!(9700 + n))), 0);
        }
        for n in 0..2 {
            order(
                &fake,
                &json!(9710 + n),
                shapes::PLAYERS_KICK,
                r#"{"player":"Alex"}"#,
            );
            assert_eq!(kicked_of(&answer(&fake, &json!(9710 + n))), 1);
        }
        order(
            &fake,
            &json!(9720),
            shapes::PLAYERS_KICK,
            r#"{"player":"Steve"}"#,
        );
        let reply = answer(&fake, &json!(9720));
        assert_eq!(code_of(&reply), -32002);
        assert_eq!(reply["error"]["message"].as_str(), Some("too many kicks"));
        let kicked = acting.kicked();
        assert_eq!(
            kicked
                .iter()
                .filter(|(who, _)| *who == Who::Name("Steve".into()))
                .count(),
            0,
            "the refused one kicked nobody and did not even look for the player"
        );
    }

    /// The window of the limit is a minute, kept per extension, and a place that kicked nobody is given back.
    #[test]
    fn the_place_of_a_kick_is_free_again_after_a_minute_or_when_nobody_was_kicked_and_no_limit_counts_nothing()
     {
        let acting = Acting::new(5);
        let (_fake, _registry, link, _folder) =
            acting_with(&["orders.moderate"], &acting, |settings| {
                settings.kicks_per_minute = 2;
            });
        let t0 = Instant::now();
        let first = link.reserve_kick(t0).expect("the first");
        let second = link
            .reserve_kick(t0 + Duration::from_secs(10))
            .expect("the second");
        assert!(
            link.reserve_kick(t0 + Duration::from_secs(59)).is_none(),
            "two in the minute"
        );
        // A minute after the first, its place is free; the second still counts.
        let third = link
            .reserve_kick(t0 + Duration::from_secs(61))
            .expect("the first one left the window");
        assert!(link.reserve_kick(t0 + Duration::from_secs(62)).is_none());
        // A place that kicked nobody is given back, and only that one.
        link.give_back_kick(second);
        let fourth = link
            .reserve_kick(t0 + Duration::from_secs(63))
            .expect("the second was given back");
        assert!(link.reserve_kick(t0 + Duration::from_secs(64)).is_none());
        let _kept = (first, third, fourth);
        // Without a limit (0, the default) every kick has its place and nothing is counted.
        let (_fake, _registry, free, _folder) = acting_with(&["orders.moderate"], &acting, |_| {});
        for n in 0..1_000 {
            assert!(free.reserve_kick(t0 + Duration::from_millis(n)).is_some());
        }
    }

    #[test]
    fn with_no_actions_given_the_order_that_expels_says_it_is_not_available() {
        let (fake, _registry, link, _folder) = orderly(&["orders.moderate"], |_| {});
        order(
            &fake,
            &json!(9500),
            shapes::PLAYERS_KICK,
            r#"{"player":"Alex"}"#,
        );
        let reply = answer(&fake, &json!(9500));
        assert_eq!(
            (code_of(&reply), reply["error"]["message"].as_str()),
            (-32603, Some("not available"))
        );
        assert_eq!(link.stats().orders(shapes::PLAYERS_KICK, Ended::Failed), 1);
    }

    /// An extension sends one lot at a time: while the players of one are being moved, another is refused with -32002 and a fixed
    /// text, and costs one token; when the moves end, the next is taken.
    #[test]
    fn a_second_send_while_one_is_running_is_refused_and_costs_a_token_and_the_next_one_is_taken_when_it_ends()
     {
        let acting = Acting::new(1);
        acting.hold_lots.store(true, Ordering::SeqCst);
        let (fake, _registry, link, _folder) = acting_with(&["orders.move"], &acting, |settings| {
            settings.order_burst = 3;
            settings.order_rate = 1;
        });
        let send = r#"{"to":"east","player":"Alex"}"#;
        order(&fake, &json!(8600), shapes::PLAYERS_SEND, send);
        assert_eq!(started_of(&answer(&fake, &json!(8600))), 1);
        // The moves of the first are still going: the second is refused, in fixed words.
        order(&fake, &json!(8601), shapes::PLAYERS_SEND, send);
        let busy = answer(&fake, &json!(8601));
        assert_eq!(code_of(&busy), -32002, "{busy}");
        assert_eq!(busy["error"]["message"], "a send is already running");
        assert_eq!(link.stats().orders(shapes::PLAYERS_SEND, Ended::Busy), 1);
        assert_eq!(acting.started().len(), 1, "the refused one moved nobody");
        // The moves end. The third is taken (the one that was refused cost a token: 3 - 1 - 1 = 1 is left, and it is enough).
        acting.finish_the_moves();
        order(&fake, &json!(8602), shapes::PLAYERS_SEND, send);
        assert_eq!(started_of(&answer(&fake, &json!(8602))), 1);
        // The budget is spent: one for each of the three. The next order finds none at the door.
        order(&fake, &json!(8603), shapes::PLAYERS_SEND, send);
        assert_eq!(code_of(&answer(&fake, &json!(8603))), -32001);
    }

    /// A server that takes no players now is refused when the send is planned, with its own words: a `send all` to it would tell
    /// each player that they could not be moved.
    #[test]
    fn a_server_that_takes_no_players_is_refused_at_once_with_fixed_words() {
        let acting = Acting::new(5);
        let (fake, _registry, link, _folder) = acting_with(&["orders.move"], &acting, |_| {});
        order(
            &fake,
            &json!(8700),
            shapes::PLAYERS_SEND,
            r#"{"to":"drained","all":true}"#,
        );
        let reply = answer(&fake, &json!(8700));
        assert_eq!(code_of(&reply), -32602, "{reply}");
        assert_eq!(
            reply["error"]["message"],
            "\"to\" is a server that takes no players now"
        );
        assert!(acting.started().is_empty());
        assert_eq!(link.stats().orders(shapes::PLAYERS_SEND, Ended::Invalid), 1);
        // And the place for a send was not kept by the refusal: the next send, to a server that takes them, is taken.
        order(
            &fake,
            &json!(8701),
            shapes::PLAYERS_SEND,
            r#"{"to":"east","all":true}"#,
        );
        assert_eq!(started_of(&answer(&fake, &json!(8701))), 5);
    }

    // ---- the orders that operate ----------------------------------------------------------------------

    use crate::extension::orders::Operate;
    use meridian_extension::orders::{Drained, Undrained};

    /// An `Operate` that is made up: `nowhere` has no role, `lobby` and `east` have; `on` players are on any server with a role;
    /// a drain, an undrain, an evacuation and a reload are recorded. `reload` waits while `hold_reload` is set, to keep a reload
    /// running.
    struct Operating {
        on: usize,
        drained: StdMutex<Vec<String>>,
        evacuated: StdMutex<Vec<String>>,
        hold_lots: AtomicBool,
        lots: StdMutex<Vec<SendLot>>,
        hold_reload: AtomicBool,
        reloads_begun: AtomicUsize,
        reloads_done: AtomicUsize,
    }

    impl Operating {
        fn new(on: usize) -> Arc<Self> {
            Arc::new(Self {
                on,
                drained: StdMutex::new(Vec::new()),
                evacuated: StdMutex::new(Vec::new()),
                hold_lots: AtomicBool::new(false),
                lots: StdMutex::new(Vec::new()),
                hold_reload: AtomicBool::new(false),
                reloads_begun: AtomicUsize::new(0),
                reloads_done: AtomicUsize::new(0),
            })
        }

        fn drained(&self) -> Vec<String> {
            self.drained.lock().unwrap().clone()
        }
    }

    struct Evacuation {
        count: usize,
        server: String,
        log: Arc<Operating>,
    }

    impl MovePlan for Evacuation {
        fn count(&self) -> usize {
            self.count
        }

        fn start(self: Box<Self>, _paid: Paid, lot: SendLot) -> Result<usize, Refusal> {
            // `stuck` is a server whose moves cannot be started: the lot is dropped with the plan.
            if self.server == "stuck" {
                return Err(Refusal::CannotStart);
            }
            self.log.evacuated.lock().unwrap().push(self.server.clone());
            if self.log.hold_lots.load(Ordering::SeqCst) {
                self.log.lots.lock().unwrap().push(lot);
            }
            Ok(self.count)
        }
    }

    struct OperatingArc(Arc<Operating>);

    impl std::ops::Deref for OperatingArc {
        type Target = Operating;
        fn deref(&self) -> &Operating {
            &self.0
        }
    }

    impl Operate for OperatingArc {
        fn players_on(&self, server: &str) -> Result<usize, Refusal> {
            if server == "nowhere" {
                Err(Refusal::NoRole)
            } else {
                Ok(self.on)
            }
        }

        fn drain(&self, server: &str) -> Result<bool, Refusal> {
            if server == "nowhere" {
                return Err(Refusal::NoRole);
            }
            let mut drained = self.drained.lock().unwrap();
            if drained.iter().any(|d| d == server) {
                return Ok(false);
            }
            drained.push(server.to_owned());
            Ok(true)
        }

        fn undrain(&self, server: &str) -> Result<bool, Refusal> {
            if server == "restarting" {
                return Err(Refusal::Restarting);
            }
            let mut drained = self.drained.lock().unwrap();
            let before = drained.len();
            drained.retain(|d| d != server);
            Ok(drained.len() != before)
        }

        fn plan_evacuation(&self, server: &str) -> Box<dyn MovePlan> {
            Box::new(Evacuation {
                count: self.on,
                server: server.to_owned(),
                log: Arc::clone(&self.0),
            })
        }

        fn reload(&self) {
            self.reloads_begun.fetch_add(1, Ordering::SeqCst);
            let limit = Instant::now() + Duration::from_secs(20);
            while self.hold_reload.load(Ordering::SeqCst) && Instant::now() < limit {
                std::thread::sleep(Duration::from_millis(2));
            }
            self.reloads_done.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// `orderly`, with the operations of `operating` served.
    fn operating_with(
        grant: &[&str],
        operating: &Arc<Operating>,
        change: impl FnOnce(&mut Settings),
    ) -> (Fake, Registry, Arc<Link>, Folder) {
        let (fake, registry, link, folder) = orderly(grant, change);
        registry.serve_operate(Arc::new(OperatingArc(Arc::clone(operating))) as Arc<dyn Operate>);
        (fake, registry, link, folder)
    }

    fn drained_of(reply: &Value) -> Drained {
        serde_json::from_value(reply["result"].clone()).unwrap_or_else(|e| panic!("{e}: {reply}"))
    }

    #[test]
    fn the_orders_that_operate_need_orders_operate_and_are_refused_as_if_they_did_not_exist_without_it()
     {
        let operating = Operating::new(5);
        // The other powers over the network are granted, this one is not.
        let (fake, _registry, link, _folder) =
            operating_with(&["orders.move", "orders.moderate"], &operating, |_| {});
        order(
            &fake,
            &json!(10_000),
            shapes::SERVERS_DRAIN,
            r#"{"server":"lobby"}"#,
        );
        order(
            &fake,
            &json!(10_001),
            shapes::SERVERS_UNDRAIN,
            r#"{"server":"lobby"}"#,
        );
        order(&fake, &json!(10_002), shapes::RELOAD, "{}");
        for id in [10_000, 10_001, 10_002] {
            assert_eq!(code_of(&answer(&fake, &json!(id))), -32601);
        }
        assert!(operating.drained().is_empty());
        assert_eq!(operating.reloads_begun.load(Ordering::SeqCst), 0);
        assert_eq!(link.stats().orders(shapes::RELOAD, Ended::Refused), 1);
    }

    #[test]
    fn a_server_is_drained_and_offered_again_and_each_answer_says_whether_it_changed() {
        let operating = Operating::new(5);
        let (fake, _registry, _link, _folder) =
            operating_with(&["orders.operate"], &operating, |_| {});
        order(
            &fake,
            &json!(10_100),
            shapes::SERVERS_DRAIN,
            r#"{"server":"lobby"}"#,
        );
        assert_eq!(
            drained_of(&answer(&fake, &json!(10_100))),
            Drained {
                drained: true,
                moving: 0
            }
        );
        // Again: it was drained already.
        order(
            &fake,
            &json!(10_101),
            shapes::SERVERS_DRAIN,
            r#"{"server":"lobby"}"#,
        );
        assert_eq!(
            drained_of(&answer(&fake, &json!(10_101))),
            Drained {
                drained: false,
                moving: 0
            }
        );
        assert!(
            operating.evacuated.lock().unwrap().is_empty(),
            "no `move`: nobody was sent"
        );
        order(
            &fake,
            &json!(10_102),
            shapes::SERVERS_UNDRAIN,
            r#"{"server":"lobby"}"#,
        );
        let undrained: Undrained =
            serde_json::from_value(answer(&fake, &json!(10_102))["result"].clone()).unwrap();
        assert!(undrained.undrained);
        // Not drained: `false`, not an error.
        order(
            &fake,
            &json!(10_103),
            shapes::SERVERS_UNDRAIN,
            r#"{"server":"lobby"}"#,
        );
        let undrained: Undrained =
            serde_json::from_value(answer(&fake, &json!(10_103))["result"].clone()).unwrap();
        assert!(!undrained.undrained);
        assert!(operating.drained().is_empty());
    }

    #[test]
    fn a_drain_with_move_sends_the_players_away_and_costs_the_players_it_moves_up_to_the_burst() {
        let operating = Operating::new(500);
        let (fake, _registry, _link, _folder) =
            operating_with(&["orders.operate"], &operating, |settings| {
                settings.order_burst = 4;
                settings.order_rate = 1;
            });
        order(
            &fake,
            &json!(10_200),
            shapes::SERVERS_DRAIN,
            r#"{"server":"lobby","move":true}"#,
        );
        assert_eq!(
            drained_of(&answer(&fake, &json!(10_200))),
            Drained {
                drained: true,
                moving: 500
            }
        );
        assert_eq!(*operating.evacuated.lock().unwrap(), ["lobby"]);
        // It took the whole burst: the next order, at once, is over the budget.
        order(
            &fake,
            &json!(10_201),
            shapes::SERVERS_UNDRAIN,
            r#"{"server":"lobby"}"#,
        );
        assert_eq!(code_of(&answer(&fake, &json!(10_201))), -32001);
    }

    /// A drain whose evacuation the budget cannot pay does not drain: the server is not left drained with its players on it.
    #[test]
    fn a_drain_with_move_that_the_budget_cannot_pay_drains_nothing() {
        let operating = Operating::new(5);
        let (fake, _registry, link, _folder) =
            operating_with(&["orders.operate"], &operating, |settings| {
                settings.order_burst = 3;
                settings.order_rate = 1;
            });
        // Two undrains: two tokens of three. A drain that sends five players needs the three of the burst.
        for n in 0..2 {
            order(
                &fake,
                &json!(10_300 + n),
                shapes::SERVERS_UNDRAIN,
                r#"{"server":"east"}"#,
            );
            let _ = answer(&fake, &json!(10_300 + n));
        }
        order(
            &fake,
            &json!(10_310),
            shapes::SERVERS_DRAIN,
            r#"{"server":"lobby","move":true}"#,
        );
        assert_eq!(code_of(&answer(&fake, &json!(10_310))), -32001);
        assert!(
            operating.drained().is_empty(),
            "the server was drained although the budget could not pay"
        );
        assert!(operating.evacuated.lock().unwrap().is_empty());
        assert_eq!(
            link.stats()
                .orders(shapes::SERVERS_DRAIN, Ended::OverBudget),
            1
        );
    }

    #[test]
    fn a_server_with_no_role_is_refused_in_fixed_words_and_a_second_evacuation_waits_for_the_first()
    {
        let operating = Operating::new(5);
        let (fake, _registry, link, _folder) =
            operating_with(&["orders.operate"], &operating, |_| {});
        let secret = "server-name-that-must-not-come-back";
        order(
            &fake,
            &json!(10_400),
            shapes::SERVERS_DRAIN,
            r#"{"server":"nowhere","move":true}"#,
        );
        let reply = answer(&fake, &json!(10_400));
        assert_eq!(code_of(&reply), -32602);
        assert_eq!(
            reply["error"]["message"],
            "\"server\" is not a server with a role"
        );
        order(
            &fake,
            &json!(10_401),
            shapes::SERVERS_DRAIN,
            &format!("{{\"server\":\"{secret}\\n\"}}"),
        );
        let reply = answer(&fake, &json!(10_401));
        assert_eq!(code_of(&reply), -32602);
        assert!(!reply.to_string().contains(secret), "{reply}");
        // One lot of moves at a time, as for `players/send`.
        operating.hold_lots.store(true, Ordering::SeqCst);
        order(
            &fake,
            &json!(10_410),
            shapes::SERVERS_DRAIN,
            r#"{"server":"lobby","move":true}"#,
        );
        assert_eq!(drained_of(&answer(&fake, &json!(10_410))).moving, 5);
        order(
            &fake,
            &json!(10_411),
            shapes::SERVERS_DRAIN,
            r#"{"server":"east","move":true}"#,
        );
        let busy = answer(&fake, &json!(10_411));
        assert_eq!(code_of(&busy), -32002);
        assert_eq!(busy["error"]["message"], "a send is already running");
        assert_eq!(
            operating.drained(),
            ["lobby"],
            "the refused drain drained nothing"
        );
        assert_eq!(link.stats().orders(shapes::SERVERS_DRAIN, Ended::Busy), 1);
    }

    /// `reload` answers `{"ok": true}` **first** and applies afterwards: the reload here waits to be let go, and the answer
    /// arrives anyway (were the answer written after, it would never come). While it runs, another is refused.
    #[test]
    fn a_reload_is_answered_first_and_applied_after_and_one_runs_at_a_time() {
        let operating = Operating::new(0);
        operating.hold_reload.store(true, Ordering::SeqCst);
        // A reload costs half the burst and the retries cost a token each: a large budget keeps this about the places.
        let (fake, _registry, link, _folder) =
            operating_with(&["orders.operate"], &operating, |settings| {
                settings.order_burst = 10_000;
                settings.order_rate = 1_000;
            });
        order(&fake, &json!(10_500), shapes::RELOAD, "{}");
        assert_eq!(answer(&fake, &json!(10_500))["result"], json!({"ok": true}));
        until("the reload to begin", || {
            operating.reloads_begun.load(Ordering::SeqCst) == 1
        });
        assert_eq!(
            operating.reloads_done.load(Ordering::SeqCst),
            0,
            "the reload was still running when it was answered"
        );
        // A second, while the first runs.
        order(&fake, &json!(10_501), shapes::RELOAD, "{}");
        let busy = answer(&fake, &json!(10_501));
        assert_eq!(code_of(&busy), -32002);
        assert_eq!(busy["error"]["message"], "a reload is already running");
        assert_eq!(link.stats().orders(shapes::RELOAD, Ended::Busy), 1);
        assert_eq!(operating.reloads_begun.load(Ordering::SeqCst), 1);
        // The first ends; the next is taken.
        operating.hold_reload.store(false, Ordering::SeqCst);
        until("the reload to end", || {
            operating.reloads_done.load(Ordering::SeqCst) == 1
        });
        let mut again = 0;
        until("the place for a reload to be let go", || {
            again += 1;
            let id = json!(10_700 + again);
            order(&fake, &id, shapes::RELOAD, "{}");
            answer(&fake, &id).get("result").is_some()
        });
        // Parameters it does not take are refused, and nothing is reloaded for them.
        order(
            &fake,
            &json!(10_510),
            shapes::RELOAD,
            r#"{"file":"other.toml"}"#,
        );
        assert_eq!(code_of(&answer(&fake, &json!(10_510))), -32602);
    }

    #[test]
    fn with_nothing_to_operate_given_the_orders_that_operate_say_they_are_not_available() {
        let (fake, _registry, link, _folder) = orderly(&["orders.operate"], |_| {});
        order(
            &fake,
            &json!(10_600),
            shapes::SERVERS_DRAIN,
            r#"{"server":"lobby"}"#,
        );
        order(
            &fake,
            &json!(10_601),
            shapes::SERVERS_UNDRAIN,
            r#"{"server":"lobby"}"#,
        );
        order(&fake, &json!(10_602), shapes::RELOAD, "{}");
        for id in [10_600, 10_601, 10_602] {
            let reply = answer(&fake, &json!(id));
            assert_eq!(
                (code_of(&reply), reply["error"]["message"].as_str()),
                (-32603, Some("not available"))
            );
        }
        assert_eq!(link.stats().orders(shapes::RELOAD, Ended::Failed), 1);
        assert!(link.is_connected() && ping(&link, Duration::from_secs(2)).is_ok());
    }

    // ---- the review of the orders that operate --------------------------------------------------------------

    /// A reload reads, checks and applies everything: it costs half the burst, charged at once, and one the budget cannot pay is
    /// refused for budget, before anything is read.
    #[test]
    fn a_reload_costs_half_the_burst_and_a_second_close_one_is_refused_for_budget() {
        let operating = Operating::new(0);
        let (fake, _registry, _link, _folder) =
            operating_with(&["orders.operate"], &operating, |settings| {
                settings.order_burst = 3;
                settings.order_rate = 1;
            });
        order(&fake, &json!(12_000), shapes::RELOAD, "{}");
        assert_eq!(answer(&fake, &json!(12_000))["result"], json!({"ok": true}));
        until("the reload to end", || {
            operating.reloads_done.load(Ordering::SeqCst) == 1
        });
        // Two tokens of three went to the first (half the burst, rounded up): one is left, and the second needs two.
        order(&fake, &json!(12_001), shapes::RELOAD, "{}");
        let reply = answer(&fake, &json!(12_001));
        assert_eq!(code_of(&reply), -32001, "{reply}");
        assert_eq!(
            operating.reloads_begun.load(Ordering::SeqCst),
            1,
            "nothing was read for it"
        );
    }

    /// Once the server is drained the order has done what matters: if the moves cannot be started it answers `moving: 0`, not an
    /// error, and the place for a send is let go.
    #[test]
    fn a_drain_whose_moves_cannot_start_is_still_a_drain_and_says_zero_moving() {
        let operating = Operating::new(5);
        let (fake, _registry, _link, _folder) =
            operating_with(&["orders.operate"], &operating, |_| {});
        order(
            &fake,
            &json!(12_100),
            shapes::SERVERS_DRAIN,
            r#"{"server":"stuck","move":true}"#,
        );
        assert_eq!(
            drained_of(&answer(&fake, &json!(12_100))),
            Drained {
                drained: true,
                moving: 0
            }
        );
        assert_eq!(operating.drained(), ["stuck"]);
        // The place for a send was let go: another drain moves.
        order(
            &fake,
            &json!(12_101),
            shapes::SERVERS_DRAIN,
            r#"{"server":"lobby","move":true}"#,
        );
        assert_eq!(drained_of(&answer(&fake, &json!(12_101))).moving, 5);
    }

    /// A server that is going down for a restart is not offered again by an extension: fixed words, and nothing changes.
    #[test]
    fn an_undrain_of_a_server_that_is_restarting_is_refused_in_fixed_words() {
        let operating = Operating::new(0);
        let (fake, _registry, link, _folder) =
            operating_with(&["orders.operate"], &operating, |_| {});
        order(
            &fake,
            &json!(12_200),
            shapes::SERVERS_UNDRAIN,
            r#"{"server":"restarting"}"#,
        );
        let reply = answer(&fake, &json!(12_200));
        assert_eq!(code_of(&reply), -32602);
        assert_eq!(
            reply["error"]["message"],
            "\"server\" is draining for a restart"
        );
        assert_eq!(
            link.stats().orders(shapes::SERVERS_UNDRAIN, Ended::Invalid),
            1
        );
    }

    /// A reload that removes this very extension: the answer is written first, then the connection is closed by the reload itself,
    /// and nothing is left blocked: the thread of the order ends, and the place for a reload is let go.
    #[test]
    fn a_reload_that_removes_the_extension_that_asked_for_it_answers_first_and_leaves_nothing_blocked()
     {
        struct Removing(std::sync::Weak<Registry>, Arc<AtomicBool>);
        impl Operate for Removing {
            fn players_on(&self, _: &str) -> Result<usize, Refusal> {
                Ok(0)
            }
            fn drain(&self, _: &str) -> Result<bool, Refusal> {
                Ok(true)
            }
            fn undrain(&self, _: &str) -> Result<bool, Refusal> {
                Ok(false)
            }
            fn plan_evacuation(&self, _: &str) -> Box<dyn MovePlan> {
                unreachable!("no drain here")
            }
            /// What a reload of a file without the extension does to the connections: `follow` with no extension left.
            fn reload(&self) {
                if let Some(registry) = self.0.upgrade() {
                    registry.follow(&meridian_config::Config::default());
                }
                self.1.store(true, Ordering::SeqCst);
            }
        }
        let fake = Fake::start(Script {
            capabilities: vec!["orders.operate".to_owned()],
            ..Script::default()
        });
        let folder = Folder::new();
        let mut settings = settings(&fake, &folder);
        settings.grant = vec!["orders.operate".to_owned()];
        let registry = Arc::new(Registry::default());
        let link = registry.start(settings).unwrap();
        until("the connection", || link.is_connected());
        let applied = Arc::new(AtomicBool::new(false));
        registry.serve_operate(Arc::new(Removing(
            Arc::downgrade(&registry),
            Arc::clone(&applied),
        )));
        order(&fake, &json!(12_300), shapes::RELOAD, "{}");
        // Answered first, with the connection still the one that asked.
        assert_eq!(answer(&fake, &json!(12_300))["result"], json!({"ok": true}));
        until("the reload to be applied", || {
            applied.load(Ordering::SeqCst)
        });
        // The extension is gone from the registry, its connection is closed, and nothing waits: no thread of an order is alive and
        // the place for a reload is free.
        assert!(registry.links().is_empty());
        until("the connection to close", || !link.is_connected());
        until("the thread of the order to end", || {
            link.order_threads_alive() == 0
        });
        assert!(
            link.reload_lot().is_some(),
            "the place for a reload was let go"
        );
    }

    // ---- the order that runs a console line -----------------------------------------------------------------

    use crate::extension::orders::Console;
    use meridian_extension::orders::Executed;

    /// A `Console` that is made up. It records what happens, in order (`journal: <entry>`, `execute: <line>`); a line says what it
    /// does by its first word: `slow` and `reload` wait while `hold` is set, `loud` says much, `dirty` says what a line may not
    /// hold, `quiet` says nothing, anything else says `ran <word>`.
    struct Consoled {
        events: StdMutex<Vec<String>>,
        hold: AtomicBool,
        begun: AtomicUsize,
        ended: AtomicUsize,
    }

    impl Consoled {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                events: StdMutex::new(Vec::new()),
                hold: AtomicBool::new(false),
                begun: AtomicUsize::new(0),
                ended: AtomicUsize::new(0),
            })
        }

        fn events(&self) -> Vec<String> {
            self.events.lock().unwrap().clone()
        }

        fn executed(&self) -> Vec<String> {
            self.events()
                .into_iter()
                .filter_map(|e| e.strip_prefix("execute: ").map(str::to_owned))
                .collect()
        }
    }

    struct ConsoledArc(Arc<Consoled>);

    impl Console for ConsoledArc {
        fn execute(&self, line: &str) -> Vec<String> {
            self.0
                .events
                .lock()
                .unwrap()
                .push(format!("execute: {line}"));
            let word = line
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            match word.as_str() {
                "slow" | "reload" => {
                    self.0.begun.fetch_add(1, Ordering::SeqCst);
                    let limit = Instant::now() + Duration::from_secs(20);
                    while self.0.hold.load(Ordering::SeqCst) && Instant::now() < limit {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    self.0.ended.fetch_add(1, Ordering::SeqCst);
                    vec![format!("{word} done")]
                }
                "loud" => (0..2_000)
                    .map(|n| format!("line {n} {}", "x".repeat(20)))
                    .collect(),
                "dirty" => vec!["a\u{a7}4b\u{1b}[0m".to_owned(), "c\nd".to_owned()],
                "quiet" => Vec::new(),
                "boom" => panic!("a command that fails"),
                other => vec![format!("ran {other}")],
            }
        }

        fn journal(&self, entry: &str) {
            self.0
                .events
                .lock()
                .unwrap()
                .push(format!("journal: {entry}"));
        }
    }

    /// `orderly`, with the console served.
    fn consoling_with(
        grant: &[&str],
        consoled: &Arc<Consoled>,
        change: impl FnOnce(&mut Settings),
    ) -> (Fake, Registry, Arc<Link>, Folder) {
        // A line costs half the burst: a burst of two makes a line cost one token, refilled fast, so that the tests that are not about
        // the price are about what they test.
        let (fake, registry, link, folder) = orderly(grant, |settings| {
            settings.order_burst = 2;
            settings.order_rate = 1_000;
            change(settings);
        });
        registry.serve_console(Arc::new(ConsoledArc(Arc::clone(consoled))) as Arc<dyn Console>);
        (fake, registry, link, folder)
    }

    fn executed_of(reply: &Value) -> Executed {
        serde_json::from_value(reply["result"].clone()).unwrap_or_else(|e| panic!("{e}: {reply}"))
    }

    fn run_line(fake: &Fake, id: u64, line: &str) -> Value {
        let params = serde_json::to_string(&json!({ "line": line })).unwrap();
        order(fake, &json!(id), shapes::CONSOLE_EXECUTE, &params);
        answer(fake, &json!(id))
    }

    /// The operator's shell is granted by its name: every other power over the network, and the right to be asked for permissions,
    /// do not give it; it is refused as if the method did not exist, and nothing runs.
    #[test]
    fn the_order_that_runs_a_console_line_needs_orders_console_by_its_name_and_nothing_else_gives_it()
     {
        let consoled = Consoled::new();
        let everything_else = [
            "orders.read",
            "orders.message",
            "orders.move",
            "orders.moderate",
            "orders.operate",
            "decide.login",
            "decide.command",
            "decide.server",
            "commands",
            "events",
            "addresses",
        ];
        let (fake, _registry, link, _folder) = consoling_with(&everything_else, &consoled, |_| {});
        let reply = run_line(&fake, 11_000, "list");
        assert_eq!(code_of(&reply), -32601);
        assert_eq!(reply["error"]["message"], "method not found");
        assert!(consoled.events().is_empty(), "nothing was journaled or run");
        assert_eq!(
            link.stats().orders(shapes::CONSOLE_EXECUTE, Ended::Refused),
            1
        );
        let (fake, _registry, _link, _folder) =
            consoling_with(&["orders.console"], &consoled, |_| {});
        assert_eq!(
            executed_of(&run_line(&fake, 11_001, "list")),
            Executed {
                lines: vec!["ran list".into()],
                truncated: false
            }
        );
    }

    /// The line is written in the log **before** it runs, with the extension and the whole line, and what it said in one line after.
    #[test]
    fn a_line_is_journaled_before_it_runs_and_what_it_said_after() {
        let consoled = Consoled::new();
        let (fake, _registry, link, _folder) =
            consoling_with(&["orders.console"], &consoled, |_| {});
        let name = link.name().to_owned();
        run_line(&fake, 11_100, "kick  Steve   Spam, again");
        assert_eq!(
            consoled.events(),
            [
                format!("journal: extension \"{name}\" runs: kick  Steve   Spam, again"),
                "execute: kick  Steve   Spam, again".to_owned(),
                format!(
                    "journal: extension \"{name}\" ran: kick  Steve   Spam, again: said: ran kick"
                ),
            ]
        );
        // Many lines, and none, in one line.
        run_line(&fake, 11_101, "loud");
        run_line(&fake, 11_102, "quiet");
        let events = consoled.events();
        assert!(
            events[events.len() - 4..]
                .iter()
                .any(|e| e.ends_with("said: line 0 xxxxxxxxxxxxxxxxxxxx (and 1 more lines)"))
                || events.iter().any(|e| e.contains("loud: said: line 0 ")),
            "{events:?}"
        );
        assert!(events.last().unwrap().ends_with("quiet: no output"));
    }

    /// What it says is plain text and bounded: what a line may not hold is dropped, and past the limit it is cut and says so.
    #[test]
    fn the_output_is_plain_lines_and_is_cut_at_the_limit_with_a_mark() {
        let consoled = Consoled::new();
        let (fake, _registry, _link, _folder) =
            consoling_with(&["orders.console"], &consoled, |_| {});
        assert_eq!(
            executed_of(&run_line(&fake, 11_200, "dirty")),
            Executed {
                lines: vec!["a4b[0m".into(), "cd".into()],
                truncated: false
            }
        );
        assert_eq!(
            executed_of(&run_line(&fake, 11_201, "quiet")),
            Executed {
                lines: vec![],
                truncated: false
            }
        );
        let loud = executed_of(&run_line(&fake, 11_202, "loud"));
        assert!(loud.truncated);
        assert_eq!(
            loud.lines.last().map(String::as_str),
            Some(shapes::OUTPUT_TRUNCATED)
        );
        let chars: usize = loud.lines.iter().map(|l| l.chars().count()).sum();
        assert!(
            chars <= shapes::MAX_CONSOLE_OUTPUT + shapes::OUTPUT_TRUNCATED.len(),
            "{chars}"
        );
    }

    /// A line that is not one line of plain text is refused with fixed words, and nothing is journaled or run.
    #[test]
    fn a_line_that_is_not_one_plain_line_is_refused_and_nothing_is_run() {
        let consoled = Consoled::new();
        let (fake, _registry, link, _folder) =
            consoling_with(&["orders.console"], &consoled, |_| {});
        let secret = "secret-line";
        let cases = [
            serde_json::to_string(&json!({"line": ""})).unwrap(),
            serde_json::to_string(&json!({"line": "   "})).unwrap(),
            serde_json::to_string(&json!({"line": format!("{secret}\nend")})).unwrap(),
            serde_json::to_string(&json!({"line": format!("{secret}\u{a7}4")})).unwrap(),
            serde_json::to_string(&json!({"line": "x".repeat(shapes::MAX_CONSOLE_LINE + 1)}))
                .unwrap(),
            r#"{"command":"list"}"#.to_owned(),
            r#"["list"]"#.to_owned(),
        ];
        for (n, params) in cases.iter().enumerate() {
            let id = json!(11_300 + n);
            order(&fake, &id, shapes::CONSOLE_EXECUTE, params);
            let reply = answer(&fake, &id);
            assert_eq!(code_of(&reply), -32602, "{params}");
            assert!(!reply.to_string().contains(secret), "{reply}");
        }
        assert!(consoled.events().is_empty());
        assert_eq!(
            link.stats().orders(shapes::CONSOLE_EXECUTE, Ended::Invalid),
            cases.len() as u64
        );
    }

    /// One line at a time for an extension. A line that does not end in time is answered `did not finish`, **goes on running**, and
    /// holds the place until it ends.
    #[test]
    fn one_line_runs_at_a_time_and_one_that_does_not_end_in_time_goes_on_and_holds_the_place() {
        let consoled = Consoled::new();
        consoled.hold.store(true, Ordering::SeqCst);
        let (fake, _registry, _link, _folder) =
            consoling_with(&["orders.console"], &consoled, |settings| {
                settings.console_timeout = Duration::from_millis(150);
            });
        let reply = run_line(&fake, 11_400, "slow");
        assert_eq!(code_of(&reply), -32603);
        assert_eq!(reply["error"]["message"], "did not finish");
        assert_eq!(consoled.begun.load(Ordering::SeqCst), 1);
        assert_eq!(consoled.ended.load(Ordering::SeqCst), 0, "it goes on");
        // The place is held until the line ends: another is refused, and runs nothing.
        let busy = run_line(&fake, 11_401, "list");
        assert_eq!(code_of(&busy), -32002);
        assert_eq!(busy["error"]["message"], "a command is already running");
        assert_eq!(consoled.executed(), ["slow"]);
        consoled.hold.store(false, Ordering::SeqCst);
        until("the line to end", || {
            consoled.ended.load(Ordering::SeqCst) == 1
        });
        let mut again = 0;
        until("the place to be let go", || {
            again += 1;
            run_line(&fake, 11_410 + again, "list")
                .get("result")
                .is_some()
        });
    }

    /// A line that panics is `-32603` and lets the place go.
    #[test]
    fn a_line_that_fails_is_an_error_and_lets_the_place_go() {
        let consoled = Consoled::new();
        let (fake, _registry, link, _folder) =
            consoling_with(&["orders.console"], &consoled, |_| {});
        let reply = run_line(&fake, 11_500, "boom");
        assert_eq!(code_of(&reply), -32603);
        assert_eq!(
            link.stats().orders(shapes::CONSOLE_EXECUTE, Ended::Failed),
            1
        );
        let mut again = 0;
        until("the place to be let go", || {
            again += 1;
            run_line(&fake, 11_510 + again, "list")
                .get("result")
                .is_some()
        });
    }

    /// `reload` (the whole line) answers first and runs after, as the order `reload` does: the answer comes while it still runs,
    /// the line is journaled before, and nothing else runs meanwhile. A `reload` with words after it is not that: it runs now.
    #[test]
    fn a_reload_line_is_answered_first_and_run_after_and_one_with_arguments_runs_now() {
        let consoled = Consoled::new();
        consoled.hold.store(true, Ordering::SeqCst);
        let (fake, _registry, link, _folder) =
            consoling_with(&["orders.console"], &consoled, |_| {});
        let name = link.name().to_owned();
        let reply = run_line(&fake, 11_600, "RELOAD");
        let said = executed_of(&reply);
        assert_eq!(said.lines.len(), 1);
        assert!(said.lines[0].starts_with("reload asked:"), "{said:?}");
        until("the reload to begin", || {
            consoled.begun.load(Ordering::SeqCst) == 1
        });
        assert_eq!(
            consoled.ended.load(Ordering::SeqCst),
            0,
            "the reload was still running when it was answered"
        );
        assert_eq!(
            consoled.events()[0],
            format!("journal: extension \"{name}\" runs: RELOAD")
        );
        // While it runs, no other line runs.
        let busy = run_line(&fake, 11_601, "list");
        assert_eq!(code_of(&busy), -32002);
        assert_eq!(busy["error"]["message"], "a command is already running");
        consoled.hold.store(false, Ordering::SeqCst);
        until("the reload to end", || {
            consoled.ended.load(Ordering::SeqCst) == 1
        });
        let mut again = 0;
        until("the places to be let go", || {
            again += 1;
            run_line(&fake, 11_610 + again, "list")
                .get("result")
                .is_some()
        });
        // With words after it, the line is the console's to refuse, and it runs before the answer: nothing is deferred.
        let reply = run_line(&fake, 11_700, "reload now");
        assert_eq!(executed_of(&reply).lines, ["reload done"]);
        assert_eq!(consoled.ended.load(Ordering::SeqCst), 2);
    }

    /// Every line costs half the burst, whatever it says (the budget limits how often, not what): two close lines are too many for a
    /// burst of three, the second is refused for budget and nothing is journaled or run for it.
    #[test]
    fn every_line_costs_half_the_burst_and_a_second_close_one_is_refused_for_budget() {
        let consoled = Consoled::new();
        let (fake, _registry, link, _folder) =
            consoling_with(&["orders.console"], &consoled, |settings| {
                settings.order_burst = 3;
                settings.order_rate = 1;
            });
        assert!(run_line(&fake, 11_800, "list").get("result").is_some());
        until("the place to be let go", || link.console_lot().is_some());
        // Whatever the line: `send all` is not cheaper than `list`.
        let refused = run_line(&fake, 11_801, "send all east");
        assert_eq!(code_of(&refused), -32001, "{refused}");
        assert_eq!(consoled.executed(), ["list"], "the refused one ran nothing");
        assert_eq!(
            consoled.events().len(),
            3,
            "and was not even journaled: {:?}",
            consoled.events()
        );
    }

    #[test]
    fn with_no_console_given_the_order_says_it_is_not_available() {
        let (fake, _registry, _link, _folder) = orderly(&["orders.console"], |_| {});
        let reply = run_line(&fake, 11_900, "list");
        assert_eq!(
            (code_of(&reply), reply["error"]["message"].as_str()),
            (-32603, Some("not available"))
        );
    }

    /// A `reload` line pays the price of a line once, which is the price of the order `reload`: it does not pay twice.
    #[test]
    fn a_reload_line_pays_the_half_burst_once_and_not_twice() {
        let consoled = Consoled::new();
        let (fake, _registry, link, _folder) =
            consoling_with(&["orders.console"], &consoled, |settings| {
                settings.order_burst = 4;
                settings.order_rate = 1;
            });
        let reply = run_line(&fake, 11_950, "reload");
        assert!(
            executed_of(&reply).lines[0].starts_with("reload asked:"),
            "{reply}"
        );
        until("the reload to end", || {
            consoled.ended.load(Ordering::SeqCst) == 1
        });
        until("the places to be let go", || link.console_lot().is_some());
        // Two of the four went to the reload line (half the burst): two are left, which pay for exactly one more line. Had the
        // reload paid twice, this one would be refused.
        assert!(run_line(&fake, 11_960, "list").get("result").is_some());
        until("the place to be let go", || link.console_lot().is_some());
        assert_eq!(code_of(&run_line(&fake, 11_961, "list")), -32001);
        assert_eq!(consoled.executed(), ["reload", "list"]);
    }
}

// ---------------------------------------------------------------------------------------------
// The events the proxy tells an extension (docs/EXTENSIONS.md, section 6)

mod the_events {
    use super::events::Kind;
    use super::*;
    use meridian_extension::events::LeftReason;

    const ASKED: &[&str] = &["events"];

    /// A fake that asks for `asked` in its hello and a link that is granted `granted`, with `change` applied to the link's settings.
    fn listening(
        asked: &[&str],
        granted: &[&str],
        change: impl FnOnce(&mut Settings),
    ) -> (Fake, Registry, Arc<Link>, Folder) {
        let fake = Fake::start(Script {
            capabilities: asked.iter().map(|c| (*c).to_owned()).collect(),
            ..Script::default()
        });
        let (registry, link, folder) = connect_with(&fake, |settings| {
            settings.grant = granted.iter().map(|c| (*c).to_owned()).collect();
            change(settings);
        });
        until("the connection", || link.is_connected());
        (fake, registry, link, folder)
    }

    fn joined(name: &str) -> Kind {
        Kind::Joined {
            player: Kind::player(name, [1; 16], true),
            server: "lobby".into(),
            address: "203.0.113.9".into(),
        }
    }

    fn left(name: &str, reason: LeftReason) -> Kind {
        Kind::Left {
            player: Kind::player(name, [1; 16], false),
            server: "east".into(),
            reason,
        }
    }

    /// What the fake was told as notifications, in the order it came: the method and the params.
    fn told(fake: &Fake) -> Vec<(String, Value)> {
        fake.seen(|seen| {
            seen.texts
                .iter()
                .filter_map(|text| {
                    let message: Value = serde_json::from_str(text).ok()?;
                    if message.get("id").is_some() {
                        return None;
                    }
                    let method = message["method"].as_str()?;
                    method
                        .starts_with("meridian:notification/")
                        .then(|| (method.to_owned(), message["params"].clone()))
                })
                .collect()
        })
    }

    fn wait_for(fake: &Fake, count: usize) -> Vec<(String, Value)> {
        let mut found = Vec::new();
        until("the events", || {
            found = told(fake);
            found.len() >= count
        });
        found
    }

    /// An extension that is not granted `events`, or did not ask for it, is told nothing, and nothing is even made for it.
    #[test]
    fn an_extension_that_does_not_have_events_by_both_its_hello_and_the_grant_is_told_nothing() {
        for (asked, granted) in [
            (ASKED, &["orders.read"][..]),
            (&["orders.read"][..], &["events"][..]),
            (&["orders.read"][..], &["orders.read"][..]),
        ] {
            let (fake, registry, _link, _folder) = listening(asked, granted, |_| {});
            let made = AtomicUsize::new(0);
            registry.emit(|| {
                made.fetch_add(1, Ordering::SeqCst);
                joined("Alex")
            });
            assert_eq!(made.load(Ordering::SeqCst), 0, "{asked:?} {granted:?}");
            assert_eq!(registry.services.bus.listeners(), 0);
            std::thread::sleep(Duration::from_millis(80));
            assert!(told(&fake).is_empty(), "{asked:?} {granted:?}");
        }
        // Both: it is made, and told.
        let (fake, registry, _link, _folder) = listening(ASKED, ASKED, |_| {});
        until("a listener", || registry.services.bus.listeners() == 1);
        registry.emit(|| joined("Alex"));
        assert_eq!(wait_for(&fake, 1).len(), 1);
    }

    /// An event says who, where, `seq` and `at`, and nothing else; the address only to a connection granted `addresses`.
    #[test]
    fn an_event_has_its_number_and_time_and_the_address_only_with_addresses() {
        let (fake, registry, _link, _folder) = listening(ASKED, ASKED, |_| {});
        until("a listener", || registry.services.bus.listeners() == 1);
        registry.emit(|| joined("Alex"));
        registry.emit(|| left("Alex", LeftReason::Kicked));
        let told = wait_for(&fake, 2);
        assert_eq!(told[0].0, "meridian:notification/players/joined");
        let keys = |params: &Value| {
            let mut keys: Vec<&str> = params
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort_unstable();
            keys.join(",")
        };
        assert_eq!(
            keys(&told[0].1),
            "at,player,seq,server",
            "no address, and nothing else"
        );
        assert_eq!(told[0].1["seq"], 1);
        assert!(
            told[0].1["at"].as_u64().unwrap() > 1_700_000_000_000,
            "milliseconds since the epoch"
        );
        assert_eq!(
            told[0].1["player"],
            json!({"name": "Alex", "uuid": "01010101-0101-0101-0101-010101010101", "auth": "verified"})
        );
        assert_eq!(told[0].1["server"], "lobby");
        assert_eq!(told[1].0, "meridian:notification/players/left");
        assert_eq!(told[1].1["seq"], 2);
        assert_eq!(told[1].1["reason"], "kicked");
        assert_eq!(told[1].1["server"], "east");
        assert_eq!(told[1].1["player"]["auth"], "unverified");
        assert_eq!(keys(&told[1].1), "at,player,reason,seq,server");
        // With `addresses`: in `joined`, and only there.
        let both = ["events", "addresses"];
        let (fake, registry, _link, _folder) = listening(&both, &both, |_| {});
        until("a listener", || registry.services.bus.listeners() == 1);
        registry.emit(|| joined("Alex"));
        registry.emit(|| left("Alex", LeftReason::Left));
        let told = wait_for(&fake, 2);
        assert_eq!(told[0].1["address"], "203.0.113.9");
        assert!(told[1].1.get("address").is_none());
        // Asked for and not granted: no address either.
        let (fake, registry, _link, _folder) = listening(&both, ASKED, |_| {});
        until("a listener", || registry.services.bus.listeners() == 1);
        registry.emit(|| joined("Alex"));
        assert!(wait_for(&fake, 1)[0].1.get("address").is_none());
    }

    /// A move and a fallback say who, from where to where, and one fixed word each; a success has no reason and a fallback with nowhere to go has no
    /// destination.
    #[test]
    fn a_move_and_a_fallback_are_told_with_fixed_words_and_nothing_else() {
        use meridian_extension::events::{FailedStep, Loss, MoveOutcome};
        let (fake, registry, _link, _folder) = listening(ASKED, ASKED, |_| {});
        until("a listener", || registry.services.bus.listeners() == 1);
        let player = || Kind::player("Alex", [1; 16], true);
        registry.emit(|| Kind::Moved {
            player: player(),
            from: "lobby".into(),
            to: "east".into(),
            outcome: MoveOutcome::Seamless,
            reason: None,
        });
        registry.emit(|| Kind::Moved {
            player: player(),
            from: "lobby".into(),
            to: "east".into(),
            outcome: MoveOutcome::Failed,
            reason: Some(FailedStep::Prepare),
        });
        registry.emit(|| Kind::Fallback {
            player: player(),
            from: "lobby".into(),
            to: Some("east".into()),
            loss: Loss::Crashed,
        });
        registry.emit(|| Kind::Fallback {
            player: player(),
            from: "lobby".into(),
            to: None,
            loss: Loss::Kicked,
        });
        let told = wait_for(&fake, 4);
        let keys = |params: &Value| {
            let mut keys: Vec<&str> = params
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort_unstable();
            keys.join(",")
        };
        assert_eq!(told[0].0, "meridian:notification/players/moved");
        assert_eq!(keys(&told[0].1), "at,from,outcome,player,seq,to");
        assert_eq!(told[0].1["outcome"], "seamless");
        assert_eq!(keys(&told[1].1), "at,from,outcome,player,reason,seq,to");
        assert_eq!(
            (told[1].1["outcome"].as_str(), told[1].1["reason"].as_str()),
            (Some("failed"), Some("prepare"))
        );
        assert_eq!(told[2].0, "meridian:notification/players/fallback");
        assert_eq!(keys(&told[2].1), "at,from,loss,player,seq,to");
        assert_eq!(
            (told[2].1["to"].as_str(), told[2].1["loss"].as_str()),
            (Some("east"), Some("crashed"))
        );
        assert_eq!(keys(&told[3].1), "at,from,loss,player,seq");
        assert_eq!(told[3].1["loss"], "kicked");
        // The address is never in these two, granted or not.
        let both = ["events", "addresses"];
        let (fake, registry, _link, _folder) = listening(&both, &both, |_| {});
        until("a listener", || registry.services.bus.listeners() == 1);
        registry.emit(|| Kind::Fallback {
            player: player(),
            from: "lobby".into(),
            to: None,
            loss: Loss::Crashed,
        });
        assert!(!wait_for(&fake, 1)[0].1.to_string().contains("address"));
    }

    /// In order, and the numbers go up by one.
    #[test]
    fn the_events_come_in_the_order_they_were_made_and_numbered_one_by_one() {
        let (fake, registry, _link, _folder) = listening(ASKED, ASKED, |_| {});
        until("a listener", || registry.services.bus.listeners() == 1);
        for n in 0..200 {
            registry.emit(|| joined(&format!("p{n}")));
        }
        let told = wait_for(&fake, 200);
        for (n, (_, params)) in told.iter().enumerate() {
            assert_eq!(params["seq"], n as u64 + 1);
            assert_eq!(params["player"]["name"], format!("p{n}"));
        }
    }

    /// A queue that is full drops the **newest**, counts it, the number is taken all the same (a gap is a loss), and the next event out is
    /// preceded by `events/dropped`.
    #[test]
    fn a_full_queue_drops_the_newest_counts_them_and_tells_the_gap_where_it_is() {
        let (fake, registry, link, _folder) = listening(ASKED, ASKED, |settings| {
            settings.events_queue = 2;
        });
        until("a listener", || registry.services.bus.listeners() == 1);
        link.event_hold.on.store(true, Ordering::SeqCst);
        // The first is taken by the writer, which waits; the room is then two, and the rest is lost.
        registry.emit(|| joined("p1"));
        until("the writer to hold", || {
            link.event_hold.waiting.load(Ordering::SeqCst) == 1
        });
        for n in 2..=10 {
            registry.emit(|| joined(&format!("p{n}")));
        }
        assert_eq!(link.stats().events_dropped(), 7);
        link.event_hold.on.store(false, Ordering::SeqCst);
        let told = wait_for(&fake, 3);
        // What was kept is written as it was, with no notice among them: the oldest are kept and the newest dropped (4 to 10 are lost).
        let seqs: Vec<u64> = told
            .iter()
            .map(|(_, p)| p["seq"].as_u64().unwrap())
            .collect();
        assert_eq!(seqs, [1, 2, 3], "{told:?}");
        assert!(told.iter().all(|(m, _)| m.ends_with("players/joined")));
        // The next event is the first after the gap: the notice comes **exactly between** the last event kept and it, with the size of the
        // gap, and the event after it has the number that follows the lost ones.
        registry.emit(|| joined("later"));
        let told = wait_for(&fake, 5);
        assert_eq!(
            told[3].0, "meridian:notification/events/dropped",
            "{told:?}"
        );
        assert_eq!(told[3].1["count"], 7);
        assert!(
            told[3].1.get("seq").is_none(),
            "it has no number: the next event's shows the gap"
        );
        assert_eq!(
            (told[4].0.as_str(), told[4].1["seq"].as_u64()),
            ("meridian:notification/players/joined", Some(11))
        );
        assert_eq!(told.len(), 5, "and the notice was told once");
        // Told once: more events with no gap tell nothing more.
        registry.emit(|| joined("again"));
        let told = wait_for(&fake, 6);
        assert_eq!(told[5].1["seq"], 12);
        assert_eq!(link.stats().events_sent("players/joined"), 5);
        let mut page = String::new();
        registry.render(&mut page);
        for line in [
            "meridian_extension_events_total{extension=\"perms\",event=\"players/joined\"} 5",
            "meridian_extension_events_dropped_total{extension=\"perms\"} 7",
        ] {
            assert!(page.contains(line), "{line}\n{page}");
        }
    }

    /// With nobody listening an emission does not even look at the list of listeners: a thread that holds that list does not hold the one
    /// that emits (the flag is what spares it the lock).
    #[test]
    fn with_nobody_listening_an_emission_does_not_wait_for_the_list_of_listeners() {
        let registry = Arc::new(Registry::default());
        let held = registry.services.bus.hold_the_list();
        let done = Arc::new(AtomicBool::new(false));
        let emitter = {
            let (registry, done) = (Arc::clone(&registry), Arc::clone(&done));
            std::thread::spawn(move || {
                registry.emit(|| joined("Alex"));
                done.store(true, Ordering::SeqCst);
            })
        };
        let limit = Instant::now() + Duration::from_secs(2);
        while !done.load(Ordering::SeqCst) && Instant::now() < limit {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            done.load(Ordering::SeqCst),
            "the emission waited for the list while nobody listened"
        );
        drop(held);
        emitter.join().unwrap();
    }

    /// An extension that does not read never holds the thread that makes the events: a hundred thousand of them are made, and
    /// handed over, while its writer is stuck.
    #[test]
    fn an_extension_that_does_not_read_never_holds_the_thread_that_emits() {
        let (_fake, registry, link, _folder) = listening(ASKED, ASKED, |settings| {
            settings.events_queue = 4;
        });
        until("a listener", || registry.services.bus.listeners() == 1);
        link.event_hold.on.store(true, Ordering::SeqCst);
        let registry = Arc::new(registry);
        let done = Arc::new(AtomicBool::new(false));
        let emitter = {
            let (registry, done) = (Arc::clone(&registry), Arc::clone(&done));
            std::thread::spawn(move || {
                for n in 0..100_000 {
                    registry.emit(|| joined(&format!("p{n}")));
                }
                done.store(true, Ordering::SeqCst);
            })
        };
        until("the emitter to finish while the writer is stuck", || {
            done.load(Ordering::SeqCst)
        });
        emitter.join().unwrap();
        assert!(link.stats().events_dropped() >= 99_990);
        link.event_hold.on.store(false, Ordering::SeqCst);
    }

    /// The queue is emptied when the connection ends: nothing is replayed to the next one, and its numbers start again at 1.
    #[test]
    fn what_is_queued_when_the_connection_ends_is_not_replayed_and_the_numbers_start_again() {
        let (fake, registry, link, _folder) = listening(ASKED, ASKED, |_| {});
        until("a listener", || registry.services.bus.listeners() == 1);
        link.event_hold.on.store(true, Ordering::SeqCst);
        for n in 0..3 {
            registry.emit(|| joined(&format!("before{n}")));
        }
        fake.drop_connection();
        until("the connection to come back", || {
            fake.seen(|seen| seen.hellos.len()) == 2 && link.is_connected()
        });
        until("the new listener", || {
            registry.services.bus.listeners() == 1
        });
        link.event_hold.on.store(false, Ordering::SeqCst);
        registry.emit(|| joined("after"));
        let told = wait_for(&fake, 1);
        std::thread::sleep(Duration::from_millis(100));
        let told_later = told_of_the_second_connection(&fake);
        assert_eq!(told_later.len(), 1, "{told:?} {told_later:?}");
        assert_eq!(told_later[0]["player"]["name"], "after");
        assert_eq!(
            told_later[0]["seq"], 1,
            "a new connection starts again at 1"
        );
        assert!(
            !fake.seen(|seen| seen.texts.iter().any(|t| t.contains("before"))),
            "what was queued was thrown away with its connection"
        );
    }

    /// What the fake was told on its latest connection: the events that carry `after`.
    fn told_of_the_second_connection(fake: &Fake) -> Vec<Value> {
        told(fake)
            .into_iter()
            .filter(|(_, params)| params["player"]["name"] == "after")
            .map(|(_, params)| params)
            .collect()
    }

    /// With nobody listening an event costs one load and builds nothing; this measures what it costs, and is run by hand:
    /// `cargo test -p meridian-proxy -- --ignored --nocapture the_cost_of_an_event`.
    #[test]
    #[ignore = "a measure, run by hand"]
    fn the_cost_of_an_event_with_and_without_a_listener() {
        const N: u32 = 200_000;
        let (_fake, registry, link, _folder) = listening(ASKED, &["orders.read"], |_| {});
        until("the connection", || link.is_connected());
        let start = Instant::now();
        for n in 0..N {
            registry.emit(|| joined(&format!("p{n}")));
        }
        let nobody = start.elapsed();
        let (fake, registry, link, _folder) = listening(ASKED, ASKED, |settings| {
            settings.events_queue = 10_000;
        });
        until("a listener", || registry.services.bus.listeners() == 1);
        let start = Instant::now();
        for n in 0..N {
            registry.emit(|| joined(&format!("p{n}")));
        }
        let listener = start.elapsed();
        eprintln!(
            "emit: {} ns each with nobody listening, {} ns each with one listener ({} dropped, {} written so far)",
            nobody.as_nanos() / u128::from(N),
            listener.as_nanos() / u128::from(N),
            link.stats().events_dropped(),
            told(&fake).len()
        );
    }
}
