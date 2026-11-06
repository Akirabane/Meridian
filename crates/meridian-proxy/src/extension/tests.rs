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
                            if dropped.fetch_add(1, Ordering::Relaxed) < script.drop_first {
                                continue;
                            }
                            let (script, seen, answer, current, stop) = (
                                script.clone(),
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
    let Ok((mut reader, writer)) = socket.split() else {
        return;
    };
    *current.lock().unwrap() = Some(writer.clone());
    if script.big_message {
        let _ = writer.send_text(&"x".repeat(300 << 10));
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
    fake.send(r#"{"jsonrpc":"2.0","id":7,"method":"meridian:players/list"}"#);
    fake.send(r#"{"jsonrpc":"2.0","method":"meridian:players/list"}"#);
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
