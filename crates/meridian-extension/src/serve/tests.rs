//! The extension's end, against a stand-in for the proxy that does what the proxy does on the wire.
use super::*;
use crate::handshake::{self, Nonce, Secret};
use crate::wire::{
    self, Confirm, Discover, DiscoverMethod, Hello, HelloAnswer, Id, MAX_MESSAGE, Message,
    RpcError, code, method,
};
use meridian_websocket::{Next, Socket, close, new_key};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::mpsc;
use std::time::Instant;

const TOKEN: &[u8] = b"sdk-test-token-0123456789abcdefghij";
const OTHER_TOKEN: &[u8] = b"another-token-0123456789abcdefghij0";
const ECHO: &str = "meridian:test/echo";

fn token() -> Secret {
    Secret::new(TOKEN).unwrap()
}

/// The lines the extension writes.
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

/// An extension that echoes `params` back, with the capability `decide.login`.
fn echoing(lines: &Arc<Lines>) -> Extension {
    let mut extension = Extension::new("sdk-test", "1.2.3", token())
        .log(Arc::clone(lines) as Arc<dyn Log>)
        .register(
            ECHO,
            "decide.login",
            DiscoverMethod::with_request(ECHO, "object"),
            |params| Ok(params.unwrap_or(Value::Null)),
        );
    extension.timings.look = Duration::from_millis(10);
    extension
}

fn start(extension: Extension) -> Running {
    extension.bind("127.0.0.1:0").unwrap()
}

/// What the proxy is, on the wire.
struct Proxy {
    socket: Socket<TcpStream>,
    next_id: u64,
}

/// Opens a connection as the proxy does. The extension takes one at a time and tells another `503`
/// until the thread of the one before has ended, which a test that connects again right after a close
/// can catch: this waits for the place, a few seconds at most.
fn open(address: SocketAddr) -> Socket<TcpStream> {
    let limit = Instant::now() + Duration::from_secs(5);
    loop {
        let stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let request = wire::upgrade_request("127.0.0.1", address.port(), &new_key().unwrap());
        match Socket::open_checked(stream, &request, 2 * MAX_MESSAGE) {
            Ok(socket) => return socket,
            Err(why) if why.contains("503") && Instant::now() < limit => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(why) => panic!("cannot open the connection: {why}"),
        }
    }
}

/// The next text, or `None` when the connection is closed. Gives up after ten seconds.
fn next(socket: &mut Socket<TcpStream>) -> Option<String> {
    let limit = Instant::now() + Duration::from_secs(10);
    while Instant::now() < limit {
        match socket.read_message() {
            Ok(Next::Text(text)) => return Some(text),
            Ok(Next::Idle) => {}
            Ok(Next::Closed) | Err(_) => return None,
        }
    }
    panic!("nothing came in ten seconds");
}

fn message(socket: &mut Socket<TcpStream>) -> Message {
    Message::parse(&next(socket).expect("the connection was closed")).unwrap()
}

impl Proxy {
    fn connect(address: SocketAddr) -> Self {
        Self {
            socket: open(address),
            next_id: 1,
        }
    }

    fn send(&mut self, text: &str) {
        self.socket.send_text(text).unwrap();
    }

    /// A request, and the answer to it.
    fn call(&mut self, name: &str, params: Option<&Value>) -> Result<Value, RpcError> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&wire::request(id, name, params));
        match message(&mut self.socket) {
            Message::Response { id: got, outcome } => {
                assert_eq!(got, Some(Id::Number(id)));
                outcome
            }
            other => panic!("not an answer: {other:?}"),
        }
    }

    /// The hello with the token `token`: sends the nonce, checks the extension's proof, sends the proxy's.
    fn hello(&mut self, token: &Secret) -> (HelloAnswer, Result<Value, RpcError>) {
        let proxy_nonce = Nonce::random().unwrap();
        let offered = Hello {
            protocol: vec![0],
            proxy: "9.9.9".into(),
            proxy_nonce: proxy_nonce.to_hex(),
        };
        let answer = self
            .call(
                method::HELLO,
                Some(&serde_json::to_value(&offered).unwrap()),
            )
            .unwrap();
        let answer: HelloAnswer = wire::decode(Some(&answer)).unwrap();
        let extension_nonce = Nonce::from_hex(&answer.extension_nonce).unwrap();
        let confirm = Confirm {
            proof: handshake::proxy_proof_hex(token, &proxy_nonce, &extension_nonce),
        };
        let outcome = self.call(
            method::CONFIRM,
            Some(&serde_json::to_value(&confirm).unwrap()),
        );
        (answer, outcome)
    }

    /// The hello with the right token, which must work.
    fn up(address: SocketAddr) -> Self {
        let mut proxy = Self::connect(address);
        let (_, outcome) = proxy.hello(&token());
        assert_eq!(outcome.unwrap(), json!({}));
        proxy
    }
}

fn golden(name: &str) -> String {
    let path = format!("{}/golden/protocol-0/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(path).unwrap()
}

fn until(what: &str, mut ready: impl FnMut() -> bool) {
    let limit = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < limit, "gave up waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// What the server answers to a raw request on a new connection, up to its end or a short wait.
fn raw(address: SocketAddr, request: &str) -> String {
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut answer = Vec::new();
    let _ = stream.read_to_end(&mut answer);
    String::from_utf8_lossy(&answer).into_owned()
}

// ---------------------------------------------------------------------------------------------

#[test]
fn the_hello_is_answered_with_a_proof_the_proxy_checks_and_ends_with_the_proxys() {
    let lines = Arc::new(Lines::default());
    let running = start(echoing(&lines));
    let mut proxy = Proxy::connect(running.local_addr());
    assert_eq!(proxy.socket.protocol(), Some("meridian-extension"));
    let (answer, outcome) = proxy.hello(&token());
    assert_eq!(outcome.unwrap(), json!({}));
    assert_eq!(
        (
            answer.protocol,
            answer.name.as_str(),
            answer.version.as_str()
        ),
        (0, "sdk-test", "1.2.3")
    );
    assert_eq!(answer.capabilities, ["decide.login"]);
    assert!(answer.commands.is_empty());
    until("the line", || {
        lines
            .all()
            .iter()
            .any(|l| l.contains("the proxy 9.9.9 is connected, protocol 0"))
    });
}

#[test]
fn a_capability_the_extension_asks_for_is_declared_once_after_those_of_its_handlers() {
    let extension = echoing(&Arc::default())
        .wants("addresses")
        .wants("decide.login")
        .wants("addresses")
        .wants("events");
    let running = start(extension);
    let mut proxy = Proxy::connect(running.local_addr());
    let (answer, outcome) = proxy.hello(&token());
    assert_eq!(outcome.unwrap(), json!({}));
    assert_eq!(answer.capabilities, ["decide.login", "addresses", "events"]);
}

#[test]
fn the_golden_hello_request_gets_an_answer_the_golden_proxy_nonce_proves() {
    let running = start(echoing(&Arc::default()));
    let mut proxy = Proxy::connect(running.local_addr());
    let Message::Request { params, .. } = Message::parse(&golden("hello.request.json")).unwrap()
    else {
        panic!("the hello is a request");
    };
    proxy.send(&golden("hello.request.json"));
    let Message::Response {
        outcome: Ok(result),
        ..
    } = message(&mut proxy.socket)
    else {
        panic!("the hello is answered");
    };
    let offered: Hello = wire::decode(params.as_ref()).unwrap();
    let answer: HelloAnswer = wire::decode(Some(&result)).unwrap();
    // The golden files use the token of the specification's test vector, not this one: the proof is
    // what this extension's token makes of the nonces, and it is checked as the proxy checks it.
    assert!(handshake::check_extension_proof(
        &token(),
        &Nonce::from_hex(&offered.proxy_nonce).unwrap(),
        &Nonce::from_hex(&answer.extension_nonce).unwrap(),
        &answer.proof
    ));
    assert_eq!(answer.extension_nonce.len(), 64);
}

#[test]
fn a_wrong_token_on_the_proxys_side_is_closed_with_1008_and_nothing_is_said() {
    let lines = Arc::new(Lines::default());
    let running = start(echoing(&lines));
    let mut proxy = Proxy::connect(running.local_addr());
    let (answer, outcome) = {
        let proxy_nonce = Nonce::random().unwrap();
        let offered = Hello {
            protocol: vec![0],
            proxy: "9.9.9".into(),
            proxy_nonce: proxy_nonce.to_hex(),
        };
        let answer = proxy
            .call(
                method::HELLO,
                Some(&serde_json::to_value(&offered).unwrap()),
            )
            .unwrap();
        let answer: HelloAnswer = wire::decode(Some(&answer)).unwrap();
        // The extension's proof does not match the other token either.
        let other = Secret::new(OTHER_TOKEN).unwrap();
        let extension_nonce = Nonce::from_hex(&answer.extension_nonce).unwrap();
        assert!(!handshake::check_extension_proof(
            &other,
            &proxy_nonce,
            &extension_nonce,
            &answer.proof
        ));
        let confirm = Confirm {
            proof: handshake::proxy_proof_hex(&other, &proxy_nonce, &extension_nonce),
        };
        proxy.send(&wire::request(
            2,
            method::CONFIRM,
            Some(&serde_json::to_value(&confirm).unwrap()),
        ));
        (answer, next(&mut proxy.socket))
    };
    assert_eq!(answer.protocol, 0);
    // Not a JSON-RPC answer: the connection is closed, for a policy.
    assert_eq!(outcome, None);
    assert_eq!(proxy.socket.close_code(), Some(close::POLICY));
    until("the refusal", || {
        lines
            .all()
            .iter()
            .any(|l| l.starts_with("WARN refused the proxy: wrong token"))
    });
    // The extension is free for the right proxy.
    let _ = Proxy::up(running.local_addr());
}

#[test]
fn a_confirmation_that_is_not_a_proof_is_refused_the_same_way() {
    let running = start(echoing(&Arc::default()));
    for params in [json!({"proof": "zz"}), json!({}), json!({"proof": 7})] {
        let mut proxy = Proxy::connect(running.local_addr());
        let nonce = Nonce::random().unwrap();
        proxy
            .call(
                method::HELLO,
                Some(&json!({"protocol": [0], "proxy": "9", "proxy_nonce": nonce.to_hex()})),
            )
            .unwrap();
        proxy.send(&wire::request(2, method::CONFIRM, Some(&params)));
        assert_eq!(next(&mut proxy.socket), None, "{params}");
        assert_eq!(proxy.socket.close_code(), Some(close::POLICY));
    }
}

#[test]
fn no_version_in_common_is_an_error_that_lists_the_versions_and_a_close() {
    let running = start(echoing(&Arc::default()));
    let mut proxy = Proxy::connect(running.local_addr());
    let nonce = Nonce::random().unwrap();
    let error = proxy
        .call(
            method::HELLO,
            Some(&json!({"protocol": [7, 8], "proxy": "9", "proxy_nonce": nonce.to_hex()})),
        )
        .unwrap_err();
    assert_eq!(error.code, code::INVALID_REQUEST);
    assert_eq!(error.supported(), Some(vec![0]));
    assert_eq!(next(&mut proxy.socket), None);
    // Several versions: the newest in common is chosen.
    let mut proxy = Proxy::connect(running.local_addr());
    let answer = proxy
        .call(
            method::HELLO,
            Some(&json!({"protocol": [3, 0, 9], "proxy": "9", "proxy_nonce": nonce.to_hex()})),
        )
        .unwrap();
    assert_eq!(answer["protocol"], 0);
}

#[test]
fn a_hello_that_is_not_one_is_an_error_and_a_close() {
    let running = start(echoing(&Arc::default()));
    for params in [
        json!({"protocol": [0], "proxy": "9"}),
        json!({"protocol": [0], "proxy": "9", "proxy_nonce": "abc"}),
        json!({"protocol": "0", "proxy": "9", "proxy_nonce": "00"}),
    ] {
        let mut proxy = Proxy::connect(running.local_addr());
        let error = proxy.call(method::HELLO, Some(&params)).unwrap_err();
        assert_eq!(error.code, code::INVALID_PARAMS, "{params}");
        assert_eq!(next(&mut proxy.socket), None);
    }
}

#[test]
fn nothing_but_the_hello_and_the_list_of_methods_is_served_before_the_hello_is_done() {
    let lines = Arc::new(Lines::default());
    let running = start(echoing(&lines));

    // A request for a decision first: closed, for a policy, with no answer.
    let mut proxy = Proxy::connect(running.local_addr());
    proxy.send(&wire::request(1, ECHO, Some(&json!({"name": "Steve"}))));
    assert_eq!(next(&mut proxy.socket), None);
    assert_eq!(proxy.socket.close_code(), Some(close::POLICY));

    // The confirmation before the hello.
    let mut proxy = Proxy::connect(running.local_addr());
    proxy.send(&wire::request(
        1,
        method::CONFIRM,
        Some(&json!({"proof": "00"})),
    ));
    assert_eq!(next(&mut proxy.socket), None);

    // Not JSON-RPC.
    let mut proxy = Proxy::connect(running.local_addr());
    proxy.send("hello");
    assert_eq!(next(&mut proxy.socket), None);

    // The list of methods is answered, with the methods of the hello and none of the handlers.
    let mut proxy = Proxy::connect(running.local_addr());
    let listed = proxy.call(method::DISCOVER, None).unwrap();
    let listed: Discover = wire::decode(Some(&listed)).unwrap();
    let names: Vec<_> = listed.methods.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, ["meridian:hello", "meridian:hello/confirm"]);
    // And the hello still works after it.
    let (_, outcome) = proxy.hello(&token());
    assert_eq!(outcome.unwrap(), json!({}));
    // The player's data is in no line.
    assert!(lines.all().iter().all(|l| !l.contains("Steve")));
}

#[test]
fn the_list_of_methods_is_made_from_the_handlers_and_starts_with_the_golden_ones() {
    let running = start(echoing(&Arc::default()));
    let mut proxy = Proxy::up(running.local_addr());
    let listed = proxy.call(method::DISCOVER, None).unwrap();
    let document: Discover = wire::decode(Some(&listed)).unwrap();
    let names: Vec<_> = document.methods.iter().map(|m| m.name.as_str()).collect();
    // `rpc.discover` itself is not listed.
    assert_eq!(names, ["meridian:hello", "meridian:hello/confirm", ECHO]);
    assert_eq!(document.openrpc.as_deref(), Some("1.3.2"));
    assert_eq!(document.info.unwrap().version, "0");
    // The first two are the ones of the golden file, byte for byte as JSON.
    let golden: Value = serde_json::from_str(&golden("discover.answer.json")).unwrap();
    assert_eq!(listed["methods"][0], golden["result"]["methods"][0]);
    assert_eq!(listed["methods"][1], golden["result"]["methods"][1]);
    assert_eq!(
        listed["methods"][2],
        json!({"name": ECHO, "params": [{"name": "request", "required": true, "schema": {"type": "object"}}]})
    );
    // The same call under the other name, and with an id that is text, answered as it came.
    assert_eq!(proxy.call(method::DISCOVER_ALIAS, None).unwrap(), listed);
    proxy.send(&json!({"jsonrpc": "2.0", "id": "abc-1", "method": "rpc.discover"}).to_string());
    let Message::Response { id, outcome } = message(&mut proxy.socket) else {
        panic!("an answer");
    };
    assert_eq!(id, Some(Id::Text("abc-1".into())));
    assert_eq!(outcome.unwrap(), listed);
}

#[test]
fn a_handler_gets_the_params_and_its_result_or_error_is_the_answer() {
    let lines = Arc::new(Lines::default());
    let extension = echoing(&lines).register(
        "meridian:test/refuse",
        "decide.command",
        DiscoverMethod::new("meridian:test/refuse"),
        |_| Err(RpcError::new(code::INVALID_PARAMS, "no thanks")),
    );
    let running = start(extension);
    let mut proxy = Proxy::connect(running.local_addr());
    let (answer, _) = proxy.hello(&token());
    // Once each, in the order the handlers were registered.
    assert_eq!(answer.capabilities, ["decide.login", "decide.command"]);
    let sent = json!({"name": "Steve", "n": [1, 2]});
    assert_eq!(proxy.call(ECHO, Some(&sent)).unwrap(), sent);
    let error = proxy.call("meridian:test/refuse", None).unwrap_err();
    assert_eq!(
        (error.code, error.message.as_str()),
        (code::INVALID_PARAMS, "no thanks")
    );
}

#[test]
fn an_unknown_method_a_second_hello_and_a_message_that_is_not_json_rpc_are_errors_and_the_connection_stays()
 {
    let running = start(echoing(&Arc::default()));
    let mut proxy = Proxy::up(running.local_addr());
    assert_eq!(
        proxy.call("meridian:nothing", None).unwrap_err().code,
        code::METHOD_NOT_FOUND
    );
    assert_eq!(
        proxy
            .call(method::HELLO, Some(&json!({"protocol": [0]})))
            .unwrap_err()
            .code,
        code::INVALID_REQUEST
    );
    assert_eq!(
        proxy
            .call(method::CONFIRM, Some(&json!({"proof": "00"})))
            .unwrap_err()
            .code,
        code::INVALID_REQUEST
    );
    // Not JSON-RPC: an error with no id, since none could be read.
    proxy.send("{ not json");
    let Message::Response { id, outcome } = message(&mut proxy.socket) else {
        panic!("an answer");
    };
    assert_eq!(
        (id, outcome.unwrap_err().code),
        (None, code::INVALID_REQUEST)
    );
    // A notification gets no answer, and the next request is answered in order.
    proxy.send(&json!({"jsonrpc": "2.0", "method": ECHO, "params": {}}).to_string());
    assert_eq!(
        proxy.call(ECHO, Some(&json!({"a": 1}))).unwrap(),
        json!({"a": 1})
    );
}

#[test]
fn a_handler_that_panics_is_an_internal_error_and_the_connection_and_the_place_are_kept() {
    let lines = Arc::new(Lines::default());
    let extension = echoing(&lines)
        .register(
            "meridian:test/panic",
            "decide.login",
            DiscoverMethod::new("meridian:test/panic"),
            |_| panic!("the secret in the panic"),
        )
        .max_in_flight(1);
    let running = start(extension);
    let mut proxy = Proxy::up(running.local_addr());
    for _ in 0..3 {
        let error = proxy.call("meridian:test/panic", None).unwrap_err();
        assert_eq!(
            (error.code, error.message.as_str()),
            (code::INTERNAL_ERROR, "internal error")
        );
        // The place was given back: with `max_in_flight(1)` the next request is served.
        assert_eq!(proxy.call(ECHO, Some(&json!(1))).unwrap(), json!(1));
    }
    let lines = lines.all();
    assert!(
        lines
            .iter()
            .any(|l| l.contains("the handler of meridian:test/panic panicked"))
    );
    // What the panic said is not in the extension's own lines.
    assert!(lines.iter().all(|l| !l.contains("the secret in the panic")));
}

#[test]
fn no_more_requests_run_than_max_in_flight_and_one_more_is_an_error_not_a_wait() {
    let (started, started_rx) = mpsc::channel::<()>();
    let (release, release_rx) = mpsc::channel::<()>();
    let release_rx = Mutex::new(release_rx);
    let started = Mutex::new(started);
    let extension = echoing(&Arc::default())
        .register(
            "meridian:test/slow",
            "decide.login",
            DiscoverMethod::new("meridian:test/slow"),
            move |params| {
                started.lock().unwrap().send(()).unwrap();
                release_rx.lock().unwrap().recv().unwrap();
                Ok(params.unwrap_or(Value::Null))
            },
        )
        .max_in_flight(2);
    let running = start(extension);
    let mut proxy = Proxy::up(running.local_addr());
    for id in 10..12 {
        proxy.send(&wire::request(id, "meridian:test/slow", Some(&json!(id))));
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    }
    // A third while two run: refused at once, and the other requests are untouched.
    proxy.send(&wire::request(12, "meridian:test/slow", Some(&json!(12))));
    let Message::Response { id, outcome } = message(&mut proxy.socket) else {
        panic!("an answer");
    };
    assert_eq!(id, Some(Id::Number(12)));
    let error = outcome.unwrap_err();
    assert_eq!(
        (error.code, error.message.as_str()),
        (code::INTERNAL_ERROR, "too many requests")
    );
    // Other methods than the handlers' are not held up by them either.
    assert!(proxy.call(method::DISCOVER, None).is_ok());
    release.send(()).unwrap();
    release.send(()).unwrap();
    let mut answered = Vec::new();
    for _ in 0..2 {
        let Message::Response {
            id: Some(Id::Number(id)),
            outcome,
        } = message(&mut proxy.socket)
        else {
            panic!("an answer");
        };
        assert_eq!(outcome.unwrap(), json!(id));
        answered.push(id);
    }
    answered.sort_unstable();
    assert_eq!(answered, [10, 11]);
    // The places are free again.
    release.send(()).unwrap();
    proxy.send(&wire::request(13, "meridian:test/slow", Some(&json!(13))));
    assert_eq!(
        message(&mut proxy.socket),
        Message::Response {
            id: Some(Id::Number(13)),
            outcome: Ok(json!(13)),
        }
    );
}

#[test]
fn a_message_over_the_limit_is_closed_with_1009() {
    let running = start(echoing(&Arc::default()));
    let mut proxy = Proxy::up(running.local_addr());
    proxy.send(&" ".repeat(MAX_MESSAGE + 1));
    assert_eq!(next(&mut proxy.socket), None);
    assert_eq!(proxy.socket.close_code(), Some(close::TOO_BIG));
    // The extension is free for another proxy once this one has gone.
    drop(proxy);
    until("the slot", || {
        let request = wire::upgrade_request(
            "127.0.0.1",
            running.local_addr().port(),
            &new_key().unwrap(),
        );
        raw(running.local_addr(), &request).starts_with("HTTP/1.1 101")
    });
}

#[test]
fn one_proxy_at_a_time_and_a_second_is_told_503_without_touching_the_first() {
    let lines = Arc::new(Lines::default());
    let running = start(echoing(&lines));
    let mut first = Proxy::up(running.local_addr());
    let request = wire::upgrade_request(
        "127.0.0.1",
        running.local_addr().port(),
        &new_key().unwrap(),
    );
    let answer = raw(running.local_addr(), &request);
    assert!(answer.starts_with("HTTP/1.1 503"), "{answer}");
    assert_eq!(first.call(ECHO, Some(&json!(1))).unwrap(), json!(1));
    assert!(
        lines
            .all()
            .iter()
            .any(|l| l.contains("a second connection was refused"))
    );
    // Once the first has gone, another connects.
    drop(first);
    until("the slot", || {
        let request = wire::upgrade_request(
            "127.0.0.1",
            running.local_addr().port(),
            &new_key().unwrap(),
        );
        raw(running.local_addr(), &request).starts_with("HTTP/1.1 101")
    });
}

#[test]
fn an_upgrade_without_the_subprotocol_or_with_an_origin_is_refused_with_403() {
    let running = start(echoing(&Arc::default()));
    let port = running.local_addr().port();
    let key = new_key().unwrap();
    let plain = format!(
        "GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
    );
    assert!(raw(running.local_addr(), &plain).starts_with("HTTP/1.1 403"));
    let browser = wire::upgrade_request("127.0.0.1", port, &key)
        .replace("\r\n\r\n", "\r\nOrigin: http://evil.example\r\n\r\n");
    assert!(raw(running.local_addr(), &browser).starts_with("HTTP/1.1 403"));
    let other =
        wire::upgrade_request("127.0.0.1", port, &key).replace("meridian-extension", "mqtt");
    assert!(raw(running.local_addr(), &other).starts_with("HTTP/1.1 403"));
    assert!(raw(running.local_addr(), "GET / HTTP/1.0\r\n\r\n").starts_with("HTTP/1.1 400"));
    // Each of them left the extension free.
    let _ = Proxy::up(running.local_addr());
}

#[test]
fn a_proxy_that_does_not_finish_the_hello_in_time_is_let_go() {
    let lines = Arc::new(Lines::default());
    let mut extension = echoing(&lines);
    extension.timings.hello = Duration::from_millis(300);
    let running = start(extension);
    // Connects and says nothing.
    let mut silent = TcpStream::connect(running.local_addr()).unwrap();
    silent
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut buf = [0u8; 8];
    // The extension gives up and closes: end of stream (or a reset), not a hang.
    let read = silent.read(&mut buf);
    assert!(matches!(read, Ok(0) | Err(_)), "{read:?}");
    // One that upgrades and then stays silent is let go as well.
    let mut socket = open(running.local_addr());
    assert_eq!(next(&mut socket), None);
    until("the line", || {
        lines
            .all()
            .iter()
            .any(|l| l.contains("did not finish the hello in time"))
    });
    let _ = Proxy::up(running.local_addr());
}

#[test]
fn stopping_closes_the_connection_with_a_normal_close_waits_for_a_request_and_frees_the_port() {
    let (started, started_rx) = mpsc::channel::<()>();
    let started = Mutex::new(started);
    let finished = Arc::new(AtomicBool::new(false));
    let extension = {
        let finished = Arc::clone(&finished);
        echoing(&Arc::default()).register(
            "meridian:test/slow",
            "decide.login",
            DiscoverMethod::new("meridian:test/slow"),
            move |_| {
                started.lock().unwrap().send(()).unwrap();
                std::thread::sleep(Duration::from_millis(200));
                finished.store(true, Ordering::SeqCst);
                Ok(Value::Null)
            },
        )
    };
    let running = start(extension);
    let address = running.local_addr();
    let mut proxy = Proxy::up(address);
    proxy.send(&wire::request(5, "meridian:test/slow", None::<&Value>));
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();

    let began = Instant::now();
    running.stop();
    assert!(began.elapsed() < Duration::from_secs(5));
    // It waited for the request that was running.
    assert!(finished.load(Ordering::SeqCst));
    // The proxy gets the answer of the request that was running, and only then a normal close: nothing
    // is sent after a close frame.
    let Message::Response { id, outcome } = message(&mut proxy.socket) else {
        panic!("the answer comes before the close");
    };
    assert_eq!((id, outcome), (Some(Id::Number(5)), Ok(Value::Null)));
    assert_eq!(next(&mut proxy.socket), None);
    assert_eq!(proxy.socket.close_code(), Some(close::NORMAL));
    // The port is free.
    drop(std::net::TcpListener::bind(address).unwrap());
    // Stopping again is harmless.
    running.stop();
}

#[test]
fn a_stopper_ends_serve_from_another_thread() {
    let running = start(echoing(&Arc::default()));
    let stopper = running.stopper();
    let thread = std::thread::spawn(move || running.wait());
    std::thread::sleep(Duration::from_millis(50));
    assert!(!thread.is_finished());
    stopper.stop();
    until("the end", || thread.is_finished());
    thread.join().unwrap();
}

#[test]
fn only_the_loopback_can_be_bound() {
    let extension = || Extension::new("x", "1", token());
    for bad in ["0.0.0.0:0", "192.0.2.7:0", "[::]:0"] {
        let error = extension().bind(bad).err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{bad}");
        assert!(error.to_string().contains("loopback"), "{error}");
    }
    for good in ["127.0.0.1:0", "localhost:0"] {
        let running = extension().bind(good).unwrap();
        assert!(running.local_addr().ip().is_loopback());
    }
    let running = extension().bind("[::1]:0");
    if let Ok(running) = running {
        assert!(running.local_addr().ip().is_loopback());
    }
}

#[test]
fn what_the_extension_logs_holds_neither_the_token_nor_a_message_nor_the_proxys_control_characters()
{
    let lines = Arc::new(Lines::default());
    let running = start(echoing(&lines));
    // A proxy that says a version full of control characters.
    let mut proxy = Proxy::connect(running.local_addr());
    let nonce = Nonce::random().unwrap();
    let answer = proxy
        .call(
            method::HELLO,
            Some(&json!({"protocol": [0], "proxy": "1.0\r\nINFO forged\u{1b}[0m", "proxy_nonce": nonce.to_hex()})),
        )
        .unwrap();
    let answer: HelloAnswer = wire::decode(Some(&answer)).unwrap();
    let extension_nonce = Nonce::from_hex(&answer.extension_nonce).unwrap();
    let proof = handshake::proxy_proof_hex(&token(), &nonce, &extension_nonce);
    proxy
        .call(method::CONFIRM, Some(&json!({ "proof": proof })))
        .unwrap();
    proxy
        .call(ECHO, Some(&json!({"name": "PlayerNameThatIsPrivate"})))
        .unwrap();
    proxy.send("PlayerNameThatIsPrivate is not JSON");
    let _ = message(&mut proxy.socket);
    drop(proxy);
    until("the slot", || {
        let request = wire::upgrade_request(
            "127.0.0.1",
            running.local_addr().port(),
            &new_key().unwrap(),
        );
        raw(running.local_addr(), &request).starts_with("HTTP/1.1 101")
    });
    let lines = lines.all();
    let everything = lines.join("\n");
    assert!(
        !everything.contains("PlayerNameThatIsPrivate"),
        "{everything}"
    );
    assert!(!everything.contains(std::str::from_utf8(TOKEN).unwrap()));
    assert!(!everything.contains(&proof));
    assert!(!everything.contains('\u{1b}') && !everything.contains('\r'));
    // The forged version stays on its one line.
    assert!(
        lines
            .iter()
            .any(|l| l == "INFO the proxy 1.0INFO forged[0m is connected, protocol 0"),
        "{lines:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// Time: a peer that sends a byte before each read timeout is let go at the time of the hello or of a
// message, and does not hold the one place there is.

mod slow {
    use super::*;

    /// Sends the head of a frame that announces 60000 bytes, then one byte every 20 ms for up to three
    /// seconds, on a thread of its own, from the stream of `socket`.
    fn drip(socket: &mut Socket<TcpStream>) -> std::thread::JoinHandle<()> {
        let mut raw = socket.stream_mut().try_clone().unwrap();
        std::thread::spawn(move || {
            let mut head = vec![0x81, 0x80 | 126];
            head.extend_from_slice(&60_000u16.to_be_bytes());
            head.extend_from_slice(&[0, 0, 0, 0]);
            if raw.write_all(&head).is_err() {
                return;
            }
            for _ in 0..150 {
                if raw.write_all(b"x").is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        })
    }

    /// An extension whose hello and messages must be whole in 300 ms.
    fn quick(lines: &Arc<Lines>) -> Extension {
        let mut extension = echoing(lines);
        extension.timings.hello = Duration::from_millis(300);
        extension.timings.message = Duration::from_millis(300);
        extension
    }

    /// Waits until a new proxy gets the place, and returns how long that took.
    fn until_free(running: &Running) -> Duration {
        let began = Instant::now();
        until("the place", || {
            let request = wire::upgrade_request(
                "127.0.0.1",
                running.local_addr().port(),
                &new_key().unwrap(),
            );
            raw(running.local_addr(), &request).starts_with("HTTP/1.1 101")
        });
        began.elapsed()
    }

    #[test]
    fn a_hello_that_drips_in_is_let_go_at_the_time_of_the_hello() {
        let lines = Arc::new(Lines::default());
        let running = start(quick(&lines));
        let mut slow = Proxy::connect(running.local_addr());
        let thread = drip(&mut slow.socket);
        let took = until_free(&running);
        assert!(took < Duration::from_millis(1500), "held for {took:?}");
        until("the line", || {
            lines.all().iter().any(|l| l.contains("the hello failed"))
        });
        drop(slow);
        thread.join().unwrap();
    }

    #[test]
    fn a_message_that_drips_in_after_the_hello_is_let_go_too() {
        let lines = Arc::new(Lines::default());
        let running = start(quick(&lines));
        let mut slow = Proxy::up(running.local_addr());
        let thread = drip(&mut slow.socket);
        let took = until_free(&running);
        assert!(took < Duration::from_millis(1500), "held for {took:?}");
        until("the line", || {
            lines
                .all()
                .iter()
                .any(|l| l.contains("the connection ended"))
        });
        drop(slow);
        thread.join().unwrap();
    }

    #[test]
    fn an_upgrade_that_drips_in_is_let_go_at_the_time_of_the_hello() {
        let running = start(quick(&Arc::default()));
        let mut stream = TcpStream::connect(running.local_addr()).unwrap();
        let thread = std::thread::spawn(move || {
            // One byte every 40 ms, and never the blank line.
            let head = b"GET / HTTP/1.1\r\nHost: x\r\nX-Slow: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
            for byte in head {
                if stream.write_all(&[*byte]).is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(40));
            }
        });
        let took = until_free(&running);
        assert!(took < Duration::from_millis(1500), "held for {took:?}");
        thread.join().unwrap();
    }

    #[test]
    fn stopping_does_not_wait_for_a_message_that_drips_in() {
        let running = start(quick(&Arc::default()));
        let mut slow = Proxy::up(running.local_addr());
        let thread = drip(&mut slow.socket);
        std::thread::sleep(Duration::from_millis(50));
        let began = Instant::now();
        running.stop();
        assert!(
            began.elapsed() < Duration::from_millis(1500),
            "stop took {:?}",
            began.elapsed()
        );
        drop(slow);
        thread.join().unwrap();
    }
}

// ---------------------------------------------------------------------------------------------
// The admission at login

mod login {
    use super::*;
    use crate::decide::{self, Decision, LoginRequest};

    /// Refuses Alex with the reason of the specification, lets Bob in, and has no opinion of anyone else.
    fn gate(lines: &Arc<Lines>) -> Extension {
        let mut extension = Extension::new("gate", "1.0.0", token())
            .log(Arc::clone(lines) as Arc<dyn Log>)
            .on_login(
                |request: &LoginRequest| match request.player.name.as_str() {
                    "Alex" => Decision::deny("The event starts at 20:00."),
                    "Bob" => Decision::Allow,
                    "Long" => Decision::deny(&"x".repeat(1000)),
                    "Silent" => Decision::Deny(None),
                    _ => Decision::Default,
                },
            );
        extension.timings.look = Duration::from_millis(10);
        extension
    }

    /// The golden request, for the player `name`.
    fn request_for(name: &str) -> String {
        let mut request: Value = serde_json::from_str(&golden("login.request.json")).unwrap();
        request["params"]["request"]["player"]["name"] = json!(name);
        request.to_string()
    }

    fn answer(proxy: &mut Proxy, name: &str) -> Value {
        proxy.send(&request_for(name));
        let Message::Response { id, outcome } = message(&mut proxy.socket) else {
            panic!("an answer");
        };
        assert_eq!(id, Some(Id::Number(4)));
        outcome.unwrap()
    }

    #[test]
    fn the_hello_declares_the_capability_and_the_list_has_the_method_as_the_golden_file_has_it() {
        let running = start(gate(&Arc::default()));
        let mut proxy = Proxy::connect(running.local_addr());
        let (hello, _) = proxy.hello(&token());
        assert_eq!(hello.capabilities, ["decide.login"]);
        let listed = proxy.call(method::DISCOVER, None).unwrap();
        let golden: Value = serde_json::from_str(&golden("discover.answer.json")).unwrap();
        // `meridian:decide/login` is the third method of the golden list, after the two of the hello.
        assert_eq!(listed["methods"][2], golden["result"]["methods"][2]);
        assert_eq!(listed["methods"][2]["name"], decide::LOGIN);
        assert_eq!(listed["methods"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn the_golden_request_gets_the_golden_answers() {
        let running = start(gate(&Arc::default()));
        let mut proxy = Proxy::up(running.local_addr());
        let golden_result = |name: &str| -> Value {
            let message: Value = serde_json::from_str(&golden(name)).unwrap();
            message["result"].clone()
        };
        assert_eq!(
            answer(&mut proxy, "Alex"),
            golden_result("login.answer.deny.json")
        );
        assert_eq!(
            answer(&mut proxy, "Bob"),
            golden_result("login.answer.allow.json")
        );
        assert_eq!(
            answer(&mut proxy, "Somebody"),
            golden_result("login.answer.default.json")
        );
        // The request of the golden file itself, unchanged, reaches the handler as written.
        proxy.send(&golden("login.request.json"));
        let Message::Response { outcome, .. } = message(&mut proxy.socket) else {
            panic!("an answer");
        };
        assert_eq!(outcome.unwrap(), golden_result("login.answer.deny.json"));
    }

    #[test]
    fn a_reason_is_made_valid_and_a_refusal_may_have_none() {
        let running = start(gate(&Arc::default()));
        let mut proxy = Proxy::up(running.local_addr());
        let cut = answer(&mut proxy, "Long");
        assert_eq!(cut["decision"], "deny");
        // What the proxy would refuse is never sent.
        let reason = cut["reason"].as_str().unwrap();
        assert_eq!(reason.chars().count(), decide::MAX_REASON_CHARS);
        assert!(decide::parse_login_answer(&cut).is_ok());
        assert_eq!(answer(&mut proxy, "Silent"), json!({"decision": "deny"}));
    }

    #[test]
    fn params_that_are_not_a_login_request_are_an_error_that_does_not_quote_them() {
        let lines = Arc::new(Lines::default());
        let running = start(gate(&lines));
        let mut proxy = Proxy::up(running.local_addr());
        let player =
            |auth: &str, uuid: Value| json!({"name": "PrivateName", "uuid": uuid, "auth": auth});
        for params in [
            None,
            Some(json!([])),
            Some(json!({})),
            Some(json!({"request": "PrivateName"})),
            Some(json!({"request": {"player": player("verified", json!(1)), "connected": false}})),
            // The request itself, with no `request` around it: the params are named.
            Some(json!({"player": player("verified", json!("u")), "connected": false})),
        ] {
            let error = proxy.call(decide::LOGIN, params.as_ref()).unwrap_err();
            assert_eq!(error.code, code::INVALID_PARAMS, "{params:?}");
            assert!(!error.message.contains("PrivateName"), "{}", error.message);
        }
        // The connection and the handler are fine after them.
        assert_eq!(answer(&mut proxy, "Bob"), json!({"decision": "allow"}));
        assert!(lines.all().iter().all(|l| !l.contains("PrivateName")));
    }

    #[test]
    fn an_account_kind_the_extension_does_not_know_is_read_as_unverified() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let extension = {
            let seen = Arc::clone(&seen);
            Extension::new("gate", "1.0.0", token()).on_login(move |request| {
                seen.lock().unwrap().push(request.player.auth);
                Decision::Allow
            })
        };
        let running = start(extension);
        let mut proxy = Proxy::up(running.local_addr());
        let mut request: Value = serde_json::from_str(&golden("login.request.json")).unwrap();
        request["params"]["request"]["player"]["auth"] = json!("xbox");
        proxy.send(&request.to_string());
        let Message::Response { outcome, .. } = message(&mut proxy.socket) else {
            panic!("an answer");
        };
        assert_eq!(outcome.unwrap(), json!({"decision": "allow"}));
        assert_eq!(*seen.lock().unwrap(), [decide::Auth::Unverified]);
    }

    #[test]
    fn nothing_of_the_player_is_in_the_extensions_lines() {
        let lines = Arc::new(Lines::default());
        let running = start(gate(&lines));
        let mut proxy = Proxy::up(running.local_addr());
        for name in ["Alex", "Bob", "Somebody"] {
            answer(&mut proxy, name);
        }
        let everything = lines.all().join("\n");
        for private in [
            "Alex",
            "Bob",
            "Somebody",
            "203.0.113.7",
            "0a2ea4ba",
            "20:00",
        ] {
            assert!(!everything.contains(private), "{private}\n{everything}");
        }
    }

    #[test]
    #[should_panic(expected = "registered twice")]
    fn two_login_handlers_are_a_mistake_of_the_program() {
        let _ = gate(&Arc::default()).on_login(|_| Decision::Allow);
    }
}

// ---------------------------------------------------------------------------------------------
// The choice of server

mod server {
    use super::*;
    use crate::decide::{self, Choice, ServerRequest};

    /// Refuses `Banned`, orders the servers the other way round for `Reverse`, gives the lobby named
    /// `x` first to `First`, and has no opinion of anyone else.
    fn chooser(lines: &Arc<Lines>) -> Extension {
        let mut extension = Extension::new("chooser", "1.0.0", token())
            .log(Arc::clone(lines) as Arc<dyn Log>)
            .on_server(
                |request: &ServerRequest| match request.player.name.as_str() {
                    "Banned" => Choice::refuse("No game is running."),
                    "Silent" => Choice::Refuse(None),
                    "Reverse" => {
                        Choice::order(request.candidates.iter().rev().map(|c| c.name.clone()))
                    }
                    "Alex" => Choice::order(["lobby2", "lobby"]),
                    _ => Choice::Default,
                },
            );
        extension.timings.look = Duration::from_millis(10);
        extension
    }

    fn request_for(name: &str) -> String {
        let mut request: Value = serde_json::from_str(&golden("server.request.json")).unwrap();
        request["params"]["request"]["player"]["name"] = json!(name);
        request.to_string()
    }

    fn answer(proxy: &mut Proxy, name: &str) -> Value {
        proxy.send(&request_for(name));
        let Message::Response { id, outcome } = message(&mut proxy.socket) else {
            panic!("an answer");
        };
        assert_eq!(id, Some(Id::Number(5)));
        outcome.unwrap()
    }

    #[test]
    fn the_hello_declares_the_capability_and_the_list_names_the_method() {
        let running = start(chooser(&Arc::default()));
        let mut proxy = Proxy::connect(running.local_addr());
        let (hello, _) = proxy.hello(&token());
        assert_eq!(hello.capabilities, ["decide.server"]);
        let listed = proxy.call(method::DISCOVER, None).unwrap();
        assert_eq!(listed["methods"][2]["name"], decide::SERVER);
        assert_eq!(listed["methods"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn the_golden_request_gets_the_golden_answers() {
        let running = start(chooser(&Arc::default()));
        let mut proxy = Proxy::up(running.local_addr());
        let golden_result = |name: &str| -> Value {
            let message: Value = serde_json::from_str(&golden(name)).unwrap();
            message["result"].clone()
        };
        assert_eq!(
            answer(&mut proxy, "Alex"),
            golden_result("server.answer.order.json")
        );
        assert_eq!(
            answer(&mut proxy, "Banned"),
            golden_result("server.answer.refuse.json")
        );
        assert_eq!(
            answer(&mut proxy, "Somebody"),
            golden_result("server.answer.default.json")
        );
        assert_eq!(answer(&mut proxy, "Silent"), json!({"choice": "refuse"}));
        // The golden request itself reaches the handler as written, candidates and all.
        proxy.send(&golden("server.request.json"));
        let Message::Response { outcome, .. } = message(&mut proxy.socket) else {
            panic!("an answer");
        };
        assert_eq!(outcome.unwrap(), golden_result("server.answer.order.json"));
    }

    #[test]
    fn the_handler_reads_the_candidates_and_what_it_answers_passes_the_proxys_check() {
        let running = start(chooser(&Arc::default()));
        let mut proxy = Proxy::up(running.local_addr());
        let result = answer(&mut proxy, "Reverse");
        assert_eq!(result["servers"], json!(["build", "lobby2", "lobby"]));
        let offered: Vec<String> = ["lobby", "lobby2", "build"]
            .iter()
            .map(|n| (*n).to_owned())
            .collect();
        assert_eq!(
            decide::parse_server_answer(&result, &offered).unwrap(),
            Choice::order(["build", "lobby2", "lobby"])
        );
    }

    #[test]
    fn params_that_are_not_a_server_request_are_an_error_that_does_not_quote_them() {
        let lines = Arc::new(Lines::default());
        let running = start(chooser(&lines));
        let mut proxy = Proxy::up(running.local_addr());
        for params in [
            None,
            Some(json!([])),
            Some(json!({})),
            Some(json!({"request": "PrivateName"})),
            // No candidates: not a request.
            Some(
                json!({"request": {"player": {"name": "PrivateName", "uuid": "u", "auth": "verified"}, "reason": "login"}}),
            ),
        ] {
            let error = proxy.call(decide::SERVER, params.as_ref()).unwrap_err();
            assert_eq!(error.code, code::INVALID_PARAMS, "{params:?}");
            assert!(!error.message.contains("PrivateName"), "{}", error.message);
        }
        assert_eq!(answer(&mut proxy, "Somebody"), json!({"choice": "default"}));
        assert!(lines.all().iter().all(|l| !l.contains("PrivateName")));
    }

    #[test]
    fn nothing_of_the_player_or_the_servers_is_in_the_extensions_lines() {
        let lines = Arc::new(Lines::default());
        let running = start(chooser(&lines));
        let mut proxy = Proxy::up(running.local_addr());
        for name in ["Alex", "Banned", "Somebody"] {
            answer(&mut proxy, name);
        }
        let everything = lines.all().join("\n");
        for private in [
            "Alex", "Banned", "Somebody", "0a2ea4ba", "lobby2", "No game",
        ] {
            assert!(!everything.contains(private), "{private}\n{everything}");
        }
    }

    #[test]
    fn a_login_handler_and_a_server_handler_live_in_one_extension() {
        let extension = Extension::new("both", "1.0.0", token())
            .on_login(|_| decide::Decision::Default)
            .on_server(|_| Choice::Default);
        assert_eq!(extension.capabilities(), ["decide.login", "decide.server"]);
    }

    #[test]
    #[should_panic(expected = "registered twice")]
    fn two_server_handlers_are_a_mistake_of_the_program() {
        let _ = chooser(&Arc::default()).on_server(|_| Choice::Default);
    }
}

// ---------------------------------------------------------------------------------------------
// The permission of a command

mod command {
    use super::*;
    use crate::decide::{self, CommandRequest, NodeAnswers, NodeVerdict};

    /// Gives Alex `/alert` and takes `/send` from everyone, for a minute; says nothing of the rest.
    fn ranks(lines: &Arc<Lines>) -> Extension {
        let mut extension = Extension::new("ranks", "1.0.0", token())
            .log(Arc::clone(lines) as Arc<dyn Log>)
            .on_command(|request: &CommandRequest| {
                let mut answers = NodeAnswers::new().cache_seconds(60);
                for node in &request.nodes {
                    if node == "meridian.send" {
                        answers = answers.deny(node);
                    } else if node == "meridian.alert" && request.player.name == "Alex" {
                        answers = answers.allow(node);
                    }
                }
                answers
            });
        extension.timings.look = Duration::from_millis(10);
        extension
    }

    fn request_for(name: &str, nodes: &[&str]) -> String {
        let mut request: Value = serde_json::from_str(&golden("command.request.json")).unwrap();
        request["params"]["request"]["player"]["name"] = json!(name);
        request["params"]["request"]["nodes"] = json!(nodes);
        request.to_string()
    }

    fn answer(proxy: &mut Proxy, name: &str, nodes: &[&str]) -> Value {
        proxy.send(&request_for(name, nodes));
        let Message::Response { id, outcome } = message(&mut proxy.socket) else {
            panic!("an answer");
        };
        assert_eq!(id, Some(Id::Number(6)));
        outcome.unwrap()
    }

    #[test]
    fn the_hello_declares_the_capability_and_the_list_names_the_method() {
        let running = start(ranks(&Arc::default()));
        let mut proxy = Proxy::connect(running.local_addr());
        let (hello, _) = proxy.hello(&token());
        assert_eq!(hello.capabilities, ["decide.command"]);
        let listed = proxy.call(method::DISCOVER, None).unwrap();
        assert_eq!(listed["methods"][2]["name"], decide::COMMAND);
        // The method as the golden list has it.
        let golden: Value = serde_json::from_str(&golden("discover.answer.json")).unwrap();
        assert_eq!(listed["methods"][2], golden["result"]["methods"][3]);
    }

    #[test]
    fn the_handler_answers_with_what_it_gives_and_takes_and_it_passes_the_proxys_check() {
        let running = start(ranks(&Arc::default()));
        let mut proxy = Proxy::up(running.local_addr());
        let nodes = ["meridian.alert", "meridian.send", "meridian.hub"];
        let result = answer(&mut proxy, "Alex", &nodes);
        assert_eq!(
            result,
            json!({"nodes": {"meridian.alert": "allow", "meridian.send": "deny"}, "cache_seconds": 60})
        );
        let asked: Vec<String> = nodes.iter().map(|n| (*n).to_owned()).collect();
        let parsed = decide::parse_command_answer(&result, &asked).unwrap();
        assert_eq!(parsed.verdict("meridian.alert"), NodeVerdict::Allow);
        assert_eq!(parsed.verdict("meridian.send"), NodeVerdict::Deny);
        assert_eq!(parsed.verdict("meridian.hub"), NodeVerdict::Default);
        // Bob is not given /alert.
        let bob = answer(&mut proxy, "Bob", &["meridian.alert"]);
        assert_eq!(bob, json!({"nodes": {}, "cache_seconds": 60}));
        // The golden request itself reaches the handler as written.
        proxy.send(&golden("command.request.json"));
        let Message::Response { outcome, .. } = message(&mut proxy.socket) else {
            panic!("an answer");
        };
        assert_eq!(outcome.unwrap(), json!({"nodes": {}, "cache_seconds": 60}));
    }

    #[test]
    fn params_that_are_not_a_command_request_are_an_error_that_does_not_quote_them() {
        let lines = Arc::new(Lines::default());
        let running = start(ranks(&lines));
        let mut proxy = Proxy::up(running.local_addr());
        for params in [
            None,
            Some(json!([])),
            Some(json!({})),
            Some(json!({"request": "PrivateName"})),
            // No nodes: not a request.
            Some(
                json!({"request": {"player": {"name": "PrivateName", "uuid": "u", "auth": "verified"}, "server": "lobby", "command": "hub", "args": []}}),
            ),
        ] {
            let error = proxy.call(decide::COMMAND, params.as_ref()).unwrap_err();
            assert_eq!(error.code, code::INVALID_PARAMS, "{params:?}");
            assert!(!error.message.contains("PrivateName"), "{}", error.message);
        }
        assert_eq!(
            answer(&mut proxy, "Bob", &["meridian.hub"]),
            json!({"nodes": {}, "cache_seconds": 60})
        );
        assert!(lines.all().iter().all(|l| !l.contains("PrivateName")));
    }

    #[test]
    fn nothing_of_the_player_or_the_command_is_in_the_extensions_lines() {
        let lines = Arc::new(Lines::default());
        let running = start(ranks(&lines));
        let mut proxy = Proxy::up(running.local_addr());
        for name in ["Alex", "Bob"] {
            answer(&mut proxy, name, &["meridian.alert", "meridian.send"]);
        }
        let everything = lines.all().join("\n");
        for private in ["Alex", "Bob", "0a2ea4ba", "meridian.alert", "meridian.send"] {
            assert!(!everything.contains(private), "{private}\n{everything}");
        }
    }

    #[test]
    #[should_panic(expected = "registered twice")]
    fn two_command_handlers_are_a_mistake_of_the_program() {
        let _ = ranks(&Arc::default()).on_command(|_| NodeAnswers::new());
    }
}

mod own_commands {
    use super::*;
    use crate::run::{self, Lines as Said, RunRequest};

    fn declared(name: &str, aliases: &[&str]) -> CommandSpec {
        CommandSpec {
            name: name.into(),
            aliases: aliases.iter().map(|a| (*a).to_owned()).collect(),
            usage: String::new(),
        }
    }

    /// `/party` (alias `/p`) and `/ping`.
    fn parties(lines: &Arc<Lines>) -> Extension {
        let mut party = declared("party", &["p"]);
        party.usage = "/party <invite|leave> [player]".into();
        let mut extension = Extension::new("parties", "1.0.0", token())
            .log(Arc::clone(lines) as Arc<dyn Log>)
            .command(party, |request: &RunRequest| {
                match request.args.as_slice() {
                    [word, who] if word == "invite" => Said::new([
                        format!("{who} is invited to your party."),
                        format!("Your party: {}, {who}.", request.player.name),
                    ]),
                    _ => Said::one("Usage: /party <invite|leave> [player]"),
                }
            })
            .command(declared("ping", &[]), |request: &RunRequest| {
                Said::one(&format!("pong from {}", request.server))
            });
        extension.timings.look = Duration::from_millis(10);
        extension
    }

    /// The golden request's params, for another command and other arguments.
    fn params_for(command: &str, args: &[&str]) -> Value {
        let mut request: Value = serde_json::from_str(&golden("run.request.json")).unwrap();
        request["params"]["request"]["command"] = json!(command);
        request["params"]["request"]["args"] = json!(args);
        request["params"].clone()
    }

    fn run(proxy: &mut Proxy, command: &str, args: &[&str]) -> Result<Value, RpcError> {
        proxy.call(run::RUN, Some(&params_for(command, args)))
    }

    #[test]
    fn the_hello_declares_the_capability_and_the_commands_and_the_list_names_the_method() {
        let running = start(parties(&Arc::default()));
        let mut proxy = Proxy::connect(running.local_addr());
        let (hello, _) = proxy.hello(&token());
        assert_eq!(hello.capabilities, ["commands"]);
        assert_eq!(hello.commands.len(), 2);
        assert_eq!(hello.commands[0].name, "party");
        assert_eq!(hello.commands[0].aliases, ["p"]);
        assert_eq!(hello.commands[0].usage, "/party <invite|leave> [player]");
        assert_eq!(hello.commands[1].name, "ping");
        let listed = proxy.call(method::DISCOVER, None).unwrap();
        let methods = listed["methods"].as_array().unwrap();
        // One method serves every command of the extension.
        let ours: Vec<&Value> = methods.iter().filter(|m| m["name"] == run::RUN).collect();
        assert_eq!(ours.len(), 1);
        assert_eq!(ours[0]["params"][0]["name"], "request");
        assert_eq!(ours[0]["params"][0]["required"], true);
    }

    #[test]
    fn the_golden_request_gets_the_golden_answer_and_it_passes_the_proxys_check() {
        let running = start(parties(&Arc::default()));
        let mut proxy = Proxy::up(running.local_addr());
        proxy.send(&golden("run.request.json"));
        let Message::Response { id, outcome } = message(&mut proxy.socket) else {
            panic!("an answer");
        };
        assert_eq!(id, Some(Id::Number(7)));
        let result = outcome.unwrap();
        let expected: Value = serde_json::from_str(&golden("run.answer.json")).unwrap();
        assert_eq!(result, expected["result"]);
        assert_eq!(
            run::parse_run_answer(&result).unwrap(),
            ["Bob is invited to your party.", "Your party: Alex, Bob."]
        );
    }

    #[test]
    fn each_command_runs_its_own_handler_and_an_alias_runs_the_one_it_stands_for() {
        let running = start(parties(&Arc::default()));
        let mut proxy = Proxy::up(running.local_addr());
        assert_eq!(
            run(&mut proxy, "ping", &[]).unwrap(),
            json!({"lines": ["pong from lobby"]})
        );
        assert_eq!(
            run(&mut proxy, "party", &["leave"]).unwrap(),
            json!({"lines": ["Usage: /party <invite|leave> [player]"]})
        );
        // The proxy sends the declared name; a proxy that sent the alias would be understood too.
        assert_eq!(
            run(&mut proxy, "p", &["invite", "Cy"]).unwrap()["lines"][0],
            "Cy is invited to your party."
        );
    }

    #[test]
    fn a_command_nobody_declared_and_params_that_are_not_a_request_are_errors_that_do_not_quote() {
        let lines = Arc::new(Lines::default());
        let running = start(parties(&lines));
        let mut proxy = Proxy::up(running.local_addr());
        let error = run(&mut proxy, "PrivateCommand", &["PrivateArg"]).unwrap_err();
        assert_eq!(error.code, code::INVALID_PARAMS);
        assert!(!error.message.contains("Private"), "{}", error.message);
        for params in [
            None,
            Some(json!([])),
            Some(json!({})),
            Some(json!({"request": "PrivateName"})),
            // No arguments: not a request.
            Some(
                json!({"request": {"player": {"name": "PrivateName", "uuid": "u", "auth": "verified"}, "server": "lobby", "command": "ping"}}),
            ),
        ] {
            let error = proxy.call(run::RUN, params.as_ref()).unwrap_err();
            assert_eq!(error.code, code::INVALID_PARAMS, "{params:?}");
            assert!(!error.message.contains("Private"), "{}", error.message);
        }
        assert_eq!(
            run(&mut proxy, "ping", &[]).unwrap(),
            json!({"lines": ["pong from lobby"]})
        );
        assert!(lines.all().iter().all(|l| !l.contains("Private")));
    }

    #[test]
    fn what_a_command_says_is_made_to_pass_the_proxys_check() {
        let running = start(Extension::new("loud", "1.0.0", token()).command(
            declared("loud", &[]),
            |_: &RunRequest| {
                let mut lines: Vec<String> = (0..30).map(|i| format!("line {i}\r\n")).collect();
                lines[0] = "x".repeat(400);
                Said::new(lines)
            },
        ));
        let mut proxy = Proxy::up(running.local_addr());
        let result = run(&mut proxy, "loud", &[]).unwrap();
        let lines = run::parse_run_answer(&result).unwrap();
        assert_eq!(lines.len(), run::MAX_LINES);
        assert_eq!(lines[0].chars().count(), run::MAX_LINE_CHARS);
        assert_eq!(lines[1], "line 1");
    }

    #[test]
    fn a_login_gate_and_commands_live_in_one_extension_and_the_capabilities_are_listed_once() {
        let extension = Extension::new("both", "1.0.0", token())
            .on_login(|_| crate::decide::Decision::Default)
            .command(declared("one", &[]), |_: &RunRequest| Said::none())
            .command(declared("two", &[]), |_: &RunRequest| Said::none());
        assert_eq!(extension.capabilities(), ["decide.login", "commands"]);
        let running = start(extension);
        let mut proxy = Proxy::up(running.local_addr());
        assert_eq!(run(&mut proxy, "two", &[]).unwrap(), json!({"lines": []}));
    }

    #[test]
    fn nothing_of_the_player_or_the_arguments_is_in_the_extensions_lines() {
        let lines = Arc::new(Lines::default());
        let running = start(parties(&lines));
        let mut proxy = Proxy::up(running.local_addr());
        run(&mut proxy, "party", &["invite", "SecretFriend"]).unwrap();
        let everything = lines.all().join("\n");
        for private in ["Alex", "SecretFriend", "0a2ea4ba", "invite"] {
            assert!(!everything.contains(private), "{private}\n{everything}");
        }
    }

    #[test]
    #[should_panic(expected = "is declared twice")]
    fn a_word_declared_by_two_commands_is_a_mistake_of_the_program() {
        let _ = Extension::new("x", "1", token())
            .command(declared("one", &["o"]), |_| Said::none())
            .command(declared("two", &["o"]), |_| Said::none());
    }

    #[test]
    #[should_panic(expected = "is declared twice")]
    fn a_command_declared_twice_is_a_mistake_of_the_program() {
        let _ = Extension::new("x", "1", token())
            .command(declared("one", &[]), |_| Said::none())
            .command(declared("one", &[]), |_| Said::none());
    }

    #[test]
    #[should_panic(expected = "the command is not valid")]
    fn a_word_the_proxy_would_refuse_is_a_mistake_of_the_program() {
        let _ = Extension::new("x", "1", token()).command(declared("Party", &[]), |_| Said::none());
    }

    #[test]
    #[should_panic(expected = "at most 64 commands")]
    fn a_sixty_fifth_command_is_a_mistake_of_the_program() {
        let mut extension = Extension::new("x", "1", token());
        for i in 0..=wire::MAX_COMMANDS {
            extension = extension.command(declared(&format!("c{i}"), &[]), |_| Said::none());
        }
        drop(extension);
    }
}
