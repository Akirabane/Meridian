//! The lists of the management protocol: the notices kept apart, the calls made on the
//! connection, against a fake management server that answers them.
use super::*;
use crate::management::{self, Notices};
use serde_json::json;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

const SECRET: &str = "aB3dE5gH7jK9mN1pQ3sT5vX7zA9cD1fG3hJ5kL7m";
const STOPPING: &str = r#"{"jsonrpc":"2.0","method":"minecraft:notification/server/stopping"}"#;
const BANNED: &str = r#"{"jsonrpc":"2.0","method":"minecraft:notification/bans/added","params":[{"player":{"id":"069a79f4-44e9-4726-a5be-fca90e38aaf5","name":"Alex"},"source":"Rcon","reason":"griefing"}]}"#;
const PARDONED: &str = r#"{"jsonrpc":"2.0","method":"minecraft:notification/bans/removed","params":[{"id":"069a79f4-44e9-4726-a5be-fca90e38aaf5","name":"Alex"}]}"#;
const OPED: &str = r#"{"jsonrpc":"2.0","method":"minecraft:notification/operators/added","params":[{"player":{"id":"069a79f4-44e9-4726-a5be-fca90e38aaf5","name":"Alex"},"permissionLevel":4,"bypassesPlayerLimit":true}]}"#;
const DEOPED: &str = r#"{"jsonrpc":"2.0","method":"minecraft:notification/operators/removed","params":[{"player":{"id":"069a79f4-44e9-4726-a5be-fca90e38aaf5","name":"Alex"},"permissionLevel":4,"bypassesPlayerLimit":true}]}"#;
const JOINED: &str =
    r#"{"jsonrpc":"2.0","method":"minecraft:notification/players/joined","params":[{"name":"x"}]}"#;

#[test]
fn only_the_changes_of_the_lists_are_list_notices() {
    for method in [
        "allowlist/added",
        "allowlist/removed",
        "bans/added",
        "bans/removed",
        "ip_bans/added",
        "ip_bans/removed",
        "operators/added",
        "operators/removed",
    ] {
        let line = format!(r#"{{"jsonrpc":"2.0","method":"minecraft:notification/{method}"}}"#);
        assert!(is_list_notice(&line), "{method}");
        let bare = format!(r#"{{"jsonrpc":"2.0","method":"notification/{method}"}}"#);
        assert!(is_list_notice(&bare), "{method} without the prefix");
    }
    assert!(!is_list_notice(STOPPING));
    assert!(!is_list_notice(JOINED));
    // Other notifications of the protocol are not the proxy's.
    assert!(!is_list_notice(
        r#"{"jsonrpc":"2.0","method":"minecraft:notification/gamerules/updated"}"#
    ));
    assert!(!is_list_notice(
        r#"{"jsonrpc":"2.0","method":"minecraft:notification/server/status"}"#
    ));
    assert!(!is_list_notice("not json"));
    assert!(!is_list_notice(r#"{"jsonrpc":"2.0","id":1,"result":[]}"#));
}

#[test]
fn the_list_notices_have_a_queue_of_their_own() {
    let notices = Notices::default();
    assert!(!notices.offer(BANNED), "the life notices do not keep a ban");
    assert!(notices.lists.offer(BANNED));
    assert!(!notices.lists.offer(STOPPING));
    assert!(notices.offer(STOPPING));
    // A run of bans pushes no stop notice out of its queue.
    for _ in 0..QUEUE_LEN * 2 {
        notices.lists.offer(BANNED);
    }
    let life = notices.heard(Some(0), Duration::ZERO);
    assert_eq!(life.notices.len(), 1);
    let lists = notices.lists.heard(Some(0), Duration::ZERO);
    assert_eq!(lists.last, 1 + QUEUE_LEN as u64 * 2);
    assert_eq!(lists.notices.len(), QUEUE_LEN);
}

#[test]
fn a_caller_that_fell_behind_is_told_it_missed_notices() {
    let lists = Lists::default();
    for _ in 0..QUEUE_LEN + 10 {
        lists.offer(BANNED);
    }
    let behind = lists.heard(Some(2), Duration::ZERO);
    assert!(behind.missed, "10 notices went out of the queue");
    let current = lists.heard(Some(10), Duration::ZERO);
    assert!(!current.missed, "the notice after number 10 is still kept");
    assert_eq!(current.notices.len(), QUEUE_LEN);
    // Without a number nothing is given and nothing is missed: the count is all it learns.
    let first = lists.heard(None, Duration::ZERO);
    assert!(!first.missed && first.notices.is_empty());
    assert_eq!(first.last, QUEUE_LEN as u64 + 10);
    let empty = Lists::default().heard(Some(0), Duration::ZERO);
    assert!(!empty.missed);
}

#[test]
fn a_caller_waits_for_the_next_notice() {
    let lists = Arc::new(Lists::default());
    let waiting = {
        let lists = Arc::clone(&lists);
        std::thread::spawn(move || lists.heard(Some(0), Duration::from_secs(5)))
    };
    std::thread::sleep(Duration::from_millis(100));
    lists.offer(PARDONED);
    let heard = waiting.join().unwrap();
    assert_eq!(heard.notices.len(), 1);
    assert_eq!(heard.notices[0].line, PARDONED);
}

#[test]
fn only_the_calls_on_the_lists_are_allowed() {
    let lists = Lists::default();
    lists.begin_connection();
    for method in [
        "server/stop",
        "players/kick",
        "bans/clear",
        "bans/set",
        "minecraft:bans",
        // The servers own the operators: the proxy reads them and changes nothing.
        "operators/add",
        "operators/remove",
        "operators/set",
        "operators/clear",
    ] {
        assert_eq!(
            lists.call(method, &Value::Null, Duration::from_millis(50)),
            Err(CallError::NotAllowed(method.to_owned())),
            "{method}"
        );
    }
    // Nothing was written for the listener to send.
    assert!(lists.take_calls().is_empty());
}

#[test]
fn a_call_without_a_connection_fails_at_once() {
    let lists = Lists::default();
    let before = Instant::now();
    assert_eq!(
        lists.call("bans", &Value::Null, Duration::from_secs(5)),
        Err(CallError::NotConnected)
    );
    assert!(before.elapsed() < Duration::from_secs(1));
    assert!(CallError::NotConnected.is_lost() && CallError::Timeout.is_lost());
    assert!(CallError::Closed.is_lost());
    assert!(!CallError::Failed("no".into()).is_lost());
    assert!(!CallError::NotAllowed("x".into()).is_lost());
}

#[test]
fn a_call_that_waits_when_the_connection_ends_is_told_so() {
    let lists = Arc::new(Lists::default());
    lists.begin_connection();
    let caller = {
        let lists = Arc::clone(&lists);
        std::thread::spawn(move || lists.call("bans", &Value::Null, Duration::from_secs(5)))
    };
    // Written and waiting, as the listener has not taken it.
    let until = Instant::now() + Duration::from_secs(5);
    while lists.calls().waiting.is_empty() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
    lists.end_connection();
    assert_eq!(caller.join().unwrap(), Err(CallError::Closed));
    assert_eq!(
        lists.call("bans", &Value::Null, Duration::from_secs(1)),
        Err(CallError::NotConnected)
    );
}

#[test]
fn an_answer_is_matched_to_its_call_by_id() {
    let mut waiting: HashMap<u64, Reply> = HashMap::new();
    let (first, first_answer) = mpsc::channel();
    let (second, second_answer) = mpsc::channel();
    waiting.insert(7, first);
    waiting.insert(8, second);
    // The second answers first.
    assert!(answer(
        r#"{"jsonrpc":"2.0","id":8,"result":[{"name":"Alex"}]}"#,
        &mut waiting
    ));
    assert_eq!(
        second_answer.try_recv().unwrap(),
        Ok(json!([{ "name": "Alex" }]))
    );
    assert!(first_answer.try_recv().is_err());
    assert!(answer(
        r#"{"jsonrpc":"2.0","id":7,"error":{"code":-32602,"message":"Invalid params","data":"No key player"}}"#,
        &mut waiting
    ));
    assert_eq!(
        first_answer.try_recv().unwrap(),
        Err(CallError::Failed("Invalid params: No key player".into()))
    );
    // An answer that nobody waits for is taken and dropped.
    assert!(answer(
        r#"{"jsonrpc":"2.0","id":99,"result":true}"#,
        &mut waiting
    ));
    // A notification, an answer with no id and what is not JSON are not answers.
    assert!(!answer(BANNED, &mut waiting));
    assert!(!answer(r#"{"jsonrpc":"2.0","result":true}"#, &mut waiting));
    assert!(!answer("nonsense", &mut waiting));
}

#[test]
fn the_report_survives_the_wire() {
    let report = AccessReport {
        enabled: true,
        watching: true,
        connection: 3,
        last: 12,
        missed: true,
        notices: vec![NoticeHeard {
            seq: 12,
            line: BANNED.to_owned(),
            age: Duration::from_millis(1500),
        }],
    };
    assert_eq!(
        AccessReport::from_value(&report.to_value()).unwrap(),
        report
    );
    assert!(AccessReport::from_value(&json!({ "enabled": true })).is_err());
    assert!(AccessReport::from_value(&json!([])).is_err());
    let mut bad = report.to_value();
    bad["notices"] = json!([{ "seq": 1 }]);
    assert!(AccessReport::from_value(&bad).is_err());
}

// A fake management server that reads the client's requests, as the real one does.

/// What the fake does with one request: the lines it sends back, and whether it then closes.
struct Action {
    replies: Vec<String>,
    close: bool,
}

impl Action {
    fn say(replies: Vec<String>) -> Action {
        Action {
            replies,
            close: false,
        }
    }
}

struct Fake {
    port: u16,
    done: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    /// The requests received, in order.
    received: Arc<Mutex<Vec<Value>>>,
    /// Lines to send to the client on the connection that is open.
    push: mpsc::Sender<String>,
}

type Handler = dyn Fn(&Value) -> Action + Send + Sync;

fn frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![0x80 | opcode];
    match payload.len() {
        n if n < 126 => out.push(n as u8),
        n if n <= 0xffff => {
            out.push(126);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            out.push(127);
            out.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    out.extend_from_slice(payload);
    out
}

/// The text messages in `buf`, unmasked and taken out of it.
fn client_texts(buf: &mut Vec<u8>) -> Vec<String> {
    let mut texts = Vec::new();
    loop {
        if buf.len() < 2 {
            return texts;
        }
        let (len, at) = match buf[1] & 0x7f {
            126 if buf.len() >= 4 => (usize::from(u16::from_be_bytes([buf[2], buf[3]])), 4),
            127 if buf.len() >= 10 => (
                usize::try_from(u64::from_be_bytes(buf[2..10].try_into().unwrap())).unwrap(),
                10,
            ),
            126 | 127 => return texts,
            short => (usize::from(short), 2),
        };
        if buf.len() < at + 4 + len {
            return texts;
        }
        let mask = [buf[at], buf[at + 1], buf[at + 2], buf[at + 3]];
        let payload: Vec<u8> = buf[at + 4..at + 4 + len]
            .iter()
            .zip(mask.iter().cycle())
            .map(|(b, m)| b ^ m)
            .collect();
        let opcode = buf[0] & 0x0f;
        buf.drain(..at + 4 + len);
        if opcode == 0x1 {
            texts.push(String::from_utf8(payload).unwrap());
        }
    }
}

/// A fake that plays `connections` connections, each answering with `handler`.
fn fake(connections: usize, handler: Arc<Handler>) -> Fake {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let done = Arc::new(AtomicBool::new(false));
    let received = Arc::new(Mutex::new(Vec::new()));
    let (push, pushed) = mpsc::channel::<String>();
    let thread = {
        let (done, received) = (Arc::clone(&done), Arc::clone(&received));
        std::thread::spawn(move || {
            for _ in 0..connections {
                let Ok((stream, _)) = listener.accept() else {
                    return;
                };
                if done.load(Ordering::Relaxed) {
                    return;
                }
                play(stream, &*handler, &done, &received, &pushed);
            }
        })
    };
    Fake {
        port,
        done,
        thread: Some(thread),
        received,
        push,
    }
}

fn play(
    mut stream: TcpStream,
    handler: &Handler,
    done: &AtomicBool,
    received: &Mutex<Vec<Value>>,
    pushed: &mpsc::Receiver<String>,
) {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).unwrap_or(0) == 0 {
            return;
        }
        head.push(byte[0]);
    }
    let head = String::from_utf8(head).unwrap();
    if !head.contains(&format!("Authorization: Bearer {SECRET}\r\n")) {
        let _ = stream.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n");
        return;
    }
    stream
        .write_all(
            b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
              Sec-WebSocket-Accept: unchecked\r\n\r\n",
        )
        .unwrap();
    stream
        .set_read_timeout(Some(Duration::from_millis(20)))
        .unwrap();
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    while !done.load(Ordering::Relaxed) {
        while let Ok(line) = pushed.try_recv() {
            if stream.write_all(&frame(0x1, line.as_bytes())).is_err() {
                return;
            }
        }
        match stream.read(&mut chunk) {
            Ok(0) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => return,
        }
        for text in client_texts(&mut buf) {
            let request: Value = serde_json::from_str(&text).unwrap();
            received.lock().unwrap().push(request.clone());
            let action = handler(&request);
            for line in action.replies {
                if stream.write_all(&frame(0x1, line.as_bytes())).is_err() {
                    return;
                }
            }
            if action.close {
                let _ = stream.write_all(&frame(0x8, &[0x03, 0xe8]));
                return;
            }
        }
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Answers a request with `result`, under the id it came with.
fn result_of(request: &Value, result: Value) -> String {
    json!({ "jsonrpc": "2.0", "id": request["id"], "result": result }).to_string()
}

/// A `server.properties` that points at the fake.
fn properties(port: u16) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "meridian-sidecar-lists-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("server.properties");
    std::fs::write(
        &path,
        format!(
            "management-server-enabled=true\nmanagement-server-host=localhost\n\
             management-server-port={port}\nmanagement-server-secret={SECRET}\n\
             management-server-tls-enabled=false\n"
        ),
    )
    .unwrap();
    path
}

/// The listener on a fake, until the test ends.
struct Listening {
    notices: Arc<Notices>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

fn listen(fake: &Fake) -> Listening {
    let notices = Arc::new(Notices::default());
    let stop = Arc::new(AtomicBool::new(false));
    let thread = management::watch(
        properties(fake.port),
        Arc::clone(&notices),
        Arc::clone(&stop),
    )
    .unwrap();
    // Connected, and so able to carry a call.
    let until = Instant::now() + Duration::from_secs(10);
    while !notices.heard(None, Duration::ZERO).watching && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
    Listening {
        notices,
        stop,
        thread: Some(thread),
    }
}

impl Drop for Listening {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

const WAIT: Duration = Duration::from_secs(5);

#[test]
fn a_call_is_written_to_the_server_and_its_answer_comes_back() {
    let server = fake(
        1,
        Arc::new(|request| Action::say(vec![result_of(request, json!([{ "name": "Alex" }]))])),
    );
    let listening = listen(&server);
    let params = json!({ "add": [{ "player": { "name": "Alex" }, "reason": "griefing" }] });
    let result = listening.notices.lists.call("bans/add", &params, WAIT);
    assert_eq!(result, Ok(json!([{ "name": "Alex" }])));
    let received = server.received.lock().unwrap();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0]["jsonrpc"], "2.0");
    assert_eq!(received[0]["method"], "minecraft:bans/add");
    assert_eq!(received[0]["params"], params);
    assert!(received[0]["id"].is_u64(), "{}", received[0]);
}

#[test]
fn a_call_with_no_parameter_sends_no_params() {
    let server = fake(
        1,
        Arc::new(|request| Action::say(vec![result_of(request, json!([]))])),
    );
    let listening = listen(&server);
    assert_eq!(
        listening.notices.lists.call("bans", &Value::Null, WAIT),
        Ok(json!([]))
    );
    let received = server.received.lock().unwrap();
    assert!(received[0].get("params").is_none(), "{}", received[0]);
}

#[test]
fn two_calls_get_their_own_answers() {
    let server = fake(
        1,
        Arc::new(|request| {
            let method = request["method"].as_str().unwrap().to_owned();
            Action::say(vec![result_of(request, json!(method))])
        }),
    );
    let listening = listen(&server);
    let lists = &listening.notices.lists;
    let (a, b) = std::thread::scope(|scope| {
        let a = scope.spawn(|| lists.call("bans", &Value::Null, WAIT));
        let b = scope.spawn(|| lists.call("allowlist", &Value::Null, WAIT));
        (a.join().unwrap(), b.join().unwrap())
    });
    assert_eq!(a, Ok(json!("minecraft:bans")));
    assert_eq!(b, Ok(json!("minecraft:allowlist")));
    let ids: Vec<_> = server
        .received
        .lock()
        .unwrap()
        .iter()
        .map(|r| r["id"].clone())
        .collect();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
}

#[test]
fn an_error_of_the_server_is_a_failure_that_says_why() {
    let server = fake(
        1,
        Arc::new(|request| {
            Action::say(vec![json!({
                "jsonrpc": "2.0",
                "id": request["id"],
                "error": { "code": -32602, "message": "Invalid params", "data": "No key player" },
            })
            .to_string()])
        }),
    );
    let listening = listen(&server);
    let result = listening
        .notices
        .lists
        .call("bans/add", &json!({ "add": [{}] }), WAIT);
    assert_eq!(
        result,
        Err(CallError::Failed("Invalid params: No key player".into()))
    );
}

#[test]
fn a_server_that_does_not_answer_is_a_timeout_and_the_next_call_still_works() {
    let server = fake(
        1,
        Arc::new(|request| {
            if request["method"] == "minecraft:bans" {
                Action::say(vec![])
            } else {
                Action::say(vec![result_of(request, json!([]))])
            }
        }),
    );
    let listening = listen(&server);
    let lists = &listening.notices.lists;
    assert_eq!(
        lists.call("bans", &Value::Null, Duration::from_millis(700)),
        Err(CallError::Timeout)
    );
    assert_eq!(lists.call("allowlist", &Value::Null, WAIT), Ok(json!([])));
}

#[test]
fn the_notices_of_the_lists_are_kept_apart_from_those_of_the_servers_life() {
    let server = fake(1, Arc::new(|_| Action::say(vec![])));
    let listening = listen(&server);
    for line in [JOINED, BANNED, STOPPING, PARDONED] {
        server.push.send(line.to_owned()).unwrap();
    }
    let until = Instant::now() + Duration::from_secs(10);
    let (mut life, mut lists) = (Vec::new(), Vec::new());
    while (life.is_empty() || lists.len() < 2) && Instant::now() < until {
        life = listening
            .notices
            .heard(Some(0), Duration::from_millis(50))
            .notices;
        lists = listening
            .notices
            .lists
            .heard(Some(0), Duration::from_millis(50))
            .notices;
    }
    assert_eq!(
        life.iter().map(|n| n.line.as_str()).collect::<Vec<_>>(),
        [STOPPING]
    );
    assert_eq!(
        lists.iter().map(|n| n.line.as_str()).collect::<Vec<_>>(),
        [BANNED, PARDONED]
    );
}

/// The operators ride in the queue of the lists: the proxy reads them again when one changes.
#[test]
fn the_notices_of_the_operators_come_with_those_of_the_lists() {
    let server = fake(1, Arc::new(|_| Action::say(vec![])));
    let listening = listen(&server);
    for line in [OPED, STOPPING, BANNED, DEOPED] {
        server.push.send(line.to_owned()).unwrap();
    }
    let until = Instant::now() + Duration::from_secs(10);
    let mut lists = Vec::new();
    while lists.len() < 3 && Instant::now() < until {
        lists = listening
            .notices
            .lists
            .heard(Some(0), Duration::from_millis(50))
            .notices;
    }
    assert_eq!(
        lists.iter().map(|n| n.line.as_str()).collect::<Vec<_>>(),
        [OPED, BANNED, DEOPED]
    );
}

/// The proxy reads the operators with a call of no parameter, and gets Horizon's list: for each
/// operator the player, the permission level and whether they pass the limit of players.
#[test]
fn the_operators_are_read_with_a_call_of_no_parameter() {
    let list = json!([{
        "player": { "id": "069a79f4-44e9-4726-a5be-fca90e38aaf5", "name": "Alex" },
        "permissionLevel": 4,
        "bypassesPlayerLimit": false
    }]);
    let answer = list.clone();
    let server = fake(
        1,
        Arc::new(move |request| Action::say(vec![result_of(request, answer.clone())])),
    );
    let listening = listen(&server);
    assert_eq!(
        listening
            .notices
            .lists
            .call("operators", &Value::Null, WAIT),
        Ok(list)
    );
    let received = server.received.lock().unwrap();
    assert_eq!(received[0]["method"], "minecraft:operators");
    assert!(received[0].get("params").is_none(), "{}", received[0]);
}

#[test]
fn a_new_connection_is_counted_and_the_calls_follow_it() {
    // The first connection closes after its first request, which it leaves unanswered.
    let first = Arc::new(Mutex::new(true));
    let server = fake(
        2,
        Arc::new(move |request| {
            let mut first = first.lock().unwrap();
            if std::mem::take(&mut *first) {
                Action {
                    replies: vec![],
                    close: true,
                }
            } else {
                Action::say(vec![result_of(request, json!(["again"]))])
            }
        }),
    );
    let listening = listen(&server);
    let lists = &listening.notices.lists;
    assert_eq!(lists.heard(None, Duration::ZERO).connection, 1);
    // The connection ends under the call: it is told so, not left to wait.
    assert_eq!(
        lists.call("bans", &Value::Null, WAIT),
        Err(CallError::Closed)
    );
    // The listener connects again after its first wait, and the count says so.
    let until = Instant::now() + Duration::from_secs(15);
    while lists.heard(None, Duration::ZERO).connection < 2 && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(lists.heard(None, Duration::ZERO).connection, 2);
    let until = Instant::now() + Duration::from_secs(5);
    while !listening.notices.heard(None, Duration::ZERO).watching && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(lists.call("bans", &Value::Null, WAIT), Ok(json!(["again"])));
}
