//! The management listener against a fake management server, in clear and over TLS.
use super::*;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use std::net::TcpListener;
use std::sync::Mutex;
use std::thread::JoinHandle;

const SECRET: &str = "aB3dE5gH7jK9mN1pQ3sT5vX7zA9cD1fG3hJ5kL7m";
const STOPPING: &str = r#"{"jsonrpc":"2.0","method":"minecraft:notification/server/stopping"}"#;
const STARTED: &str = r#"{"jsonrpc":"2.0","method":"minecraft:notification/server/started"}"#;
const JOINED: &str =
    r#"{"jsonrpc":"2.0","method":"minecraft:notification/players/joined","params":[{"name":"x"}]}"#;

/// A throwaway key and a self-signed certificate for `localhost`, as DER in base64; they
/// protect nothing.
const TEST_KEY: &str = "MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgZbGazXfsRYKl4QEIAGzf+IS+brg9B+fGSwHVQDV1+2ChRANCAAQ0sBVYON+MUWbeJm580IwZXyKYgxeCgN3tC6kwAWB8N/r7zbs0+F3/8oWkJI7nUIFIQW8c+UTldbUGdhS6AYhh";
const TEST_CERT: &str = "MIIBfjCCASWgAwIBAgIURfkHKmz/ITbyT0cnth6MjBDiq40wCgYIKoZIzj0EAwIwFDESMBAGA1UEAwwJbG9jYWxob3N0MCAXDTI2MTEwNDE5MTQzMFoYDzIxMjYxMDExMTkxNDMwWjAUMRIwEAYDVQQDDAlsb2NhbGhvc3QwWTATBgcqhkjOPQIBBggqhkjOPQMBBwNCAAQ0sBVYON+MUWbeJm580IwZXyKYgxeCgN3tC6kwAWB8N/r7zbs0+F3/8oWkJI7nUIFIQW8c+UTldbUGdhS6AYhho1MwUTAdBgNVHQ4EFgQUw6aOi4E8+GqQeR4nmCetcnUHeXwwHwYDVR0jBBgwFoAUw6aOi4E8+GqQeR4nmCetcnUHeXwwDwYDVR0TAQH/BAUwAwEB/zAKBggqhkjOPQQDAgNHADBEAiARybvP8824GWQe7SSYfg8lBiFi0SMH4itxh5oIrSImSgIgdYTbB3LSJa5j2HWJ00danAGlKNQd82Xm6ox3heOJfs0=";

fn unbase64(text: &str) -> Vec<u8> {
    let digit = |c: u8| match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        _ => 63,
    };
    let text = text.trim_end_matches('=').as_bytes();
    let mut out = Vec::new();
    for chunk in text.chunks(4) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, c)| n | u32::from(digit(*c)) << (18 - 6 * i));
        for i in 0..chunk.len() - 1 {
            out.push((n >> (16 - 8 * i)) as u8);
        }
    }
    out
}

fn frame(fin: bool, opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![if fin { 0x80 } else { 0 } | opcode];
    if payload.len() < 126 {
        out.push(payload.len() as u8);
    } else {
        out.push(126);
        out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    }
    out.extend_from_slice(payload);
    out
}

fn text(line: &str) -> Vec<u8> {
    frame(true, 0x1, line.as_bytes())
}

/// What a fake management server does with one connection.
struct Script {
    /// The frames it sends after the upgrade.
    frames: Vec<Vec<u8>>,
    /// Whether it then keeps the connection open until the test is over.
    hold: bool,
}

/// A management server on 127.0.0.1 that checks the bearer token, accepts the upgrade
/// and plays one script for each connection.
struct Fake {
    port: u16,
    done: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    heads: Arc<Mutex<Vec<String>>>,
}

fn fake(tls: bool, scripts: Vec<Script>) -> Fake {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let done = Arc::new(AtomicBool::new(false));
    let heads = Arc::new(Mutex::new(Vec::new()));
    let config = tls.then(|| {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        Arc::new(
            rustls::ServerConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(
                    vec![CertificateDer::from(unbase64(TEST_CERT))],
                    PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(unbase64(TEST_KEY))),
                )
                .unwrap(),
        )
    });
    let thread = {
        let (done, heads) = (Arc::clone(&done), Arc::clone(&heads));
        std::thread::spawn(move || {
            for script in scripts {
                let Ok((stream, _)) = listener.accept() else {
                    return;
                };
                if done.load(Ordering::Relaxed) {
                    return;
                }
                if let Some(config) = &config {
                    let connection = rustls::ServerConnection::new(Arc::clone(config)).unwrap();
                    play(StreamOwned::new(connection, stream), &script, &done, &heads);
                } else {
                    play(stream, &script, &done, &heads);
                }
            }
        })
    };
    Fake {
        port,
        done,
        thread: Some(thread),
        heads,
    }
}

/// Reads the upgrade, answers it, and plays `script`.
fn play<T: Read + Write>(
    mut stream: T,
    script: &Script,
    done: &AtomicBool,
    heads: &Mutex<Vec<String>>,
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
    let authorised = head.contains(&format!("Authorization: Bearer {SECRET}\r\n"));
    heads.lock().unwrap().push(head);
    if !authorised {
        let _ = stream.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n");
        let _ = stream.flush();
        return;
    }
    stream
        .write_all(
            b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
              Sec-WebSocket-Accept: unchecked\r\n\r\n",
        )
        .unwrap();
    for frame in &script.frames {
        stream.write_all(frame).unwrap();
    }
    let _ = stream.flush();
    while script.hold && !done.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(20));
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Relaxed);
        // A script that was never played leaves the thread in accept.
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A `server.properties` in a folder of its own.
fn properties(port: u16, tls: bool, secret: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "meridian-sidecar-management-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("server.properties");
    std::fs::write(
        &path,
        format!(
            "# a server.properties\nmanagement-server-enabled=true\n\
             management-server-host=localhost\nmanagement-server-port={port}\n\
             management-server-secret={secret}\nmanagement-server-tls-enabled={tls}\n"
        ),
    )
    .unwrap();
    path
}

/// The listener running on `path`.
struct Listening {
    notices: Arc<Notices>,
    stop: Arc<AtomicBool>,
    waits: Arc<Mutex<Vec<Duration>>>,
    thread: Option<JoinHandle<()>>,
}

fn listen(path: PathBuf) -> Listening {
    let notices = Arc::new(Notices::default());
    let stop = Arc::new(AtomicBool::new(false));
    let waits = Arc::new(Mutex::new(Vec::new()));
    let thread = {
        let (notices, stop, waits) = (Arc::clone(&notices), Arc::clone(&stop), Arc::clone(&waits));
        std::thread::spawn(move || {
            run(&path, &notices, &stop, &mut |w| {
                waits.lock().unwrap().push(w);
            });
        })
    };
    Listening {
        notices,
        stop,
        waits,
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

/// The first `n` notices, waiting for them.
fn hear(listening: &Listening, n: usize) -> Vec<String> {
    let until = Instant::now() + Duration::from_secs(10);
    let mut lines = Vec::new();
    let mut after = 0;
    while lines.len() < n && Instant::now() < until {
        let heard = listening
            .notices
            .heard(Some(after), Duration::from_millis(200));
        for notice in heard.notices {
            after = notice.seq;
            lines.push(notice.line);
        }
    }
    lines
}

#[test]
fn the_notices_of_a_server_are_heard_in_clear() {
    let server = fake(
        false,
        vec![Script {
            frames: vec![
                frame(true, 0x9, b"ping"),
                text(JOINED),
                frame(true, 0x2, &[0, 1]),
                text(STOPPING),
                // A notice in two fragments.
                frame(false, 0x1, &STARTED.as_bytes()[..20]),
                frame(true, 0x0, &STARTED.as_bytes()[20..]),
            ],
            hold: true,
        }],
    );
    let listening = listen(properties(server.port, false, SECRET));
    // The other notification and the binary frame are not kept.
    assert_eq!(hear(&listening, 2), [STOPPING, STARTED]);
    assert!(listening.notices.heard(None, Duration::ZERO).watching);
    let heads = server.heads.lock().unwrap();
    assert!(heads[0].starts_with("GET / HTTP/1.1\r\n"), "{}", heads[0]);
}

#[test]
fn the_notices_are_heard_over_tls_with_a_certificate_nobody_signed() {
    let server = fake(
        true,
        vec![Script {
            frames: vec![text(STOPPING)],
            hold: true,
        }],
    );
    // TLS is on unless the file says false.
    let listening = listen(properties(server.port, true, SECRET));
    assert_eq!(hear(&listening, 1), [STOPPING]);
}

#[test]
fn a_connection_that_ends_is_made_again_after_a_wait() {
    let server = fake(
        false,
        vec![
            Script {
                frames: vec![text(STOPPING)],
                hold: false,
            },
            Script {
                frames: vec![text(STARTED)],
                hold: true,
            },
        ],
    );
    let listening = listen(properties(server.port, false, SECRET));
    assert_eq!(hear(&listening, 2), [STOPPING, STARTED]);
    // The second connection came after the first wait, not at once.
    assert_eq!(listening.waits.lock().unwrap()[0], FIRST_WAIT);
}

#[test]
fn a_server_that_is_not_there_is_tried_slowly() {
    // A port nothing listens on.
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let listening = listen(properties(port, false, SECRET));
    // A refused connection can take a while to be refused; then the first wait begins.
    let until = Instant::now() + Duration::from_secs(10);
    while listening.waits.lock().unwrap().is_empty() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
    }
    // Half a second into a wait of a second: no second try yet, so never a tight loop.
    std::thread::sleep(Duration::from_millis(500));
    let waits = listening.waits.lock().unwrap().clone();
    assert_eq!(waits, [FIRST_WAIT]);
    assert!(!listening.notices.heard(None, Duration::ZERO).watching);
}

#[test]
fn the_wait_grows_and_stops_at_the_longest() {
    let mut wait = FIRST_WAIT;
    let mut seen = vec![wait];
    for _ in 0..8 {
        wait = (wait * 2).min(LONGEST_WAIT);
        seen.push(wait);
    }
    let secs: Vec<u64> = seen.iter().map(Duration::as_secs).collect();
    assert_eq!(secs, [1, 2, 4, 8, 16, 30, 30, 30, 30]);
}

#[test]
fn a_wrong_secret_is_refused_and_never_shown() {
    let wrong = "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz";
    let server = fake(
        false,
        vec![Script {
            frames: vec![],
            hold: false,
        }],
    );
    let text = std::fs::read_to_string(properties(server.port, false, wrong)).unwrap();
    let settings = Settings::from_properties(&text).unwrap();
    let refused = match connect(&settings) {
        Ok(_) => panic!("a wrong secret was accepted"),
        Err(why) => why,
    };
    assert_eq!(
        refused,
        "the upgrade was refused: HTTP/1.1 401 Unauthorized"
    );
    assert!(!refused.contains(wrong));
    assert!(!format!("{settings:?}").contains(wrong));
    assert!(format!("{settings:?}").contains("<hidden>"));
}

#[test]
fn the_settings_are_read_from_the_properties_of_the_server() {
    let text = "\
# Minecraft server properties
management-server-enabled=true
management-server-port=25585
management-server-secret=abc123
";
    let settings = Settings::from_properties(text).unwrap();
    assert_eq!(
        settings,
        Settings {
            host: "localhost".into(),
            port: 25585,
            secret: Secret::new("abc123"),
            tls: true,
        }
    );
    let plain = Settings::from_properties(&format!(
        "{text}management-server-tls-enabled=false\nmanagement-server-host=127.0.0.1\n"
    ))
    .unwrap();
    assert!(!plain.tls);
    assert_eq!(plain.host, "127.0.0.1");

    let refused = |text: &str| Settings::from_properties(text).unwrap_err();
    assert!(refused("management-server-port=1\nmanagement-server-secret=x").contains("is off"));
    assert!(
        refused("management-server-enabled=false\nmanagement-server-port=1").contains("is off")
    );
    assert!(
        refused("management-server-enabled=true\nmanagement-server-secret=x")
            .contains("management-server-port")
    );
    assert!(
        refused(
            "management-server-enabled=true\nmanagement-server-port=0\n\
             management-server-secret=x"
        )
        .contains("management-server-port")
    );
    assert!(
        refused("management-server-enabled=true\nmanagement-server-port=5")
            .contains("management-server-secret is empty")
    );
    // An error says what is wrong, never a value of the file.
    let odd = refused(
        "management-server-enabled=true\nmanagement-server-port=5\n\
         management-server-secret=a b",
    );
    assert!(!odd.contains("a b"), "{odd}");
}

#[test]
fn an_address_that_is_not_this_machines_is_refused() {
    let settings = |host: &str| Settings {
        host: host.into(),
        port: 25585,
        secret: Secret::new("x"),
        tls: true,
    };
    for host in ["127.0.0.1", "localhost"] {
        let addresses = settings(host).addresses().unwrap();
        assert!(!addresses.is_empty() && addresses.iter().all(|a| a.ip().is_loopback()));
    }
    // A documentation address, and the unspecified one a server may listen on.
    for host in ["192.0.2.1", "0.0.0.0"] {
        let refused = settings(host).addresses().unwrap_err();
        assert!(
            refused.contains("not an address of this machine"),
            "{refused}"
        );
    }
    // The listener says so and does not connect.
    let path = properties(1, false, SECRET);
    let text = std::fs::read_to_string(&path)
        .unwrap()
        .replace("localhost", "192.0.2.1");
    std::fs::write(&path, text).unwrap();
    let listening = listen(path);
    std::thread::sleep(Duration::from_millis(300));
    assert!(!listening.notices.heard(None, Duration::ZERO).watching);
}

#[test]
fn only_the_notices_of_the_servers_life_are_kept_and_few_of_them() {
    let notices = Notices::default();
    assert!(notices.offer(STOPPING));
    assert!(notices.offer(r#"{"method":"notification/server/started"}"#));
    assert!(!notices.offer(JOINED));
    assert!(!notices.offer("not json"));
    assert!(!notices.offer(r#"{"id":1,"result":true}"#));
    for _ in 0..QUEUE_LEN * 2 {
        notices.offer(STOPPING);
    }
    let heard = notices.heard(Some(0), Duration::ZERO);
    assert_eq!(heard.notices.len(), QUEUE_LEN);
    assert_eq!(heard.last, 2 + (QUEUE_LEN * 2) as u64);
    // The oldest went.
    assert_eq!(heard.notices[0].seq, heard.last - QUEUE_LEN as u64 + 1);
}

#[test]
fn a_caller_hears_what_came_after_the_number_it_has() {
    let notices = Notices::default();
    // Without a number: where the count stands, and nothing else.
    notices.offer(STOPPING);
    let heard = notices.heard(None, Duration::from_secs(5));
    assert_eq!((heard.last, heard.notices.len()), (1, 0));
    // Nothing after 1: it waits, then says so.
    let begun = Instant::now();
    let heard = notices.heard(Some(1), Duration::from_millis(200));
    assert!(begun.elapsed() >= Duration::from_millis(190));
    assert_eq!((heard.last, heard.notices.len()), (1, 0));
    // A notice that comes while it waits ends the wait.
    let notices = Arc::new(notices);
    let waiting = {
        let notices = Arc::clone(&notices);
        std::thread::spawn(move || notices.heard(Some(1), Duration::from_secs(10)))
    };
    std::thread::sleep(Duration::from_millis(100));
    notices.offer(STARTED);
    let heard = waiting.join().unwrap();
    assert_eq!(heard.notices.len(), 1);
    assert_eq!(
        (heard.notices[0].seq, heard.notices[0].line.as_str()),
        (2, STARTED)
    );
}
