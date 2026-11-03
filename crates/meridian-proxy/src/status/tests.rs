use super::*;
use meridian_config::{BalancedServer, Role};
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, TcpListener};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

const AGE: Duration = Duration::from_secs(15);

fn status(players: u64) -> ServerStatus {
    ServerStatus {
        players: Some(players),
        capacity: Some(20),
        accepts_transfers: Some(true),
    }
}

#[test]
fn a_backend_that_never_answered_is_unknown() {
    let statuses = Statuses::default();
    assert_eq!(
        statuses.report("lobby", AGE, Instant::now()),
        Report::Unknown
    );
}

/// A report is believed for the maximum age, counted from when it came, and no
/// longer; a newer one makes the backend believed again.
#[test]
fn a_report_is_believed_for_its_age_and_then_goes_stale() {
    let statuses = Statuses::default();
    let start = Instant::now();
    statuses.record("lobby", status(3), start);
    let at = |secs: u64| start + Duration::from_secs(secs);
    assert_eq!(
        statuses.report("lobby", AGE, at(0)),
        Report::Fresh(status(3))
    );
    assert_eq!(
        statuses.report("lobby", AGE, at(15)),
        Report::Fresh(status(3))
    );
    assert_eq!(statuses.report("lobby", AGE, at(16)), Report::Stale);
    statuses.record("lobby", status(5), at(20));
    assert_eq!(
        statuses.report("lobby", AGE, at(21)),
        Report::Fresh(status(5))
    );
    // Another backend is not touched.
    assert_eq!(statuses.report("east", AGE, at(21)), Report::Unknown);
}

#[test]
fn a_clock_that_runs_back_does_not_make_a_report_stale() {
    let statuses = Statuses::default();
    let now = Instant::now();
    statuses.record("lobby", status(1), now + Duration::from_secs(5));
    assert_eq!(statuses.report("lobby", AGE, now), Report::Fresh(status(1)));
}

/// A configuration with three backends that have sidecars, two of them balanced.
fn config() -> Config {
    let sidecar = |backend: &str, port: u16| Sidecar {
        backend: backend.into(),
        address: format!("127.0.0.1:{port}"),
        secret_file: PathBuf::from("unused"),
    };
    let server = |backend: &str| BalancedServer {
        backend: backend.into(),
        role: Role::Lobby,
        soft_cap: 5,
        hard_cap: 5,
    };
    Config {
        sidecars: vec![
            sidecar("lobby", 25580),
            sidecar("east", 25581),
            sidecar("unbalanced", 25582),
        ],
        balanced: vec![server("lobby"), server("east")],
        sidecar_timeout_ms: 1234,
        ..Config::default()
    }
}

#[test]
fn only_the_sidecars_of_balanced_backends_are_asked() {
    let asked = Mutex::new(Vec::new());
    let statuses = Statuses::default();
    let ask = |sidecar: &Sidecar, timeout: Duration| {
        asked
            .lock()
            .unwrap()
            .push((sidecar.backend.clone(), timeout));
        Ok(status(2))
    };
    poll(&config(), &ask, &statuses);
    let mut asked = asked.into_inner().unwrap();
    asked.sort();
    assert_eq!(
        asked,
        [
            ("east".to_owned(), Duration::from_millis(1234)),
            ("lobby".to_owned(), Duration::from_millis(1234)),
        ]
    );
    let now = Instant::now();
    assert_eq!(statuses.report("lobby", AGE, now), Report::Fresh(status(2)));
    assert_eq!(statuses.report("unbalanced", AGE, now), Report::Unknown);
}

/// A sidecar that fails keeps its last answer, which grows old; one that never
/// answered stays unknown, so the backend is treated as accepting.
#[test]
fn a_failure_keeps_the_last_answer_and_a_backend_that_never_answered_stays_unknown() {
    let statuses = Statuses::default();
    let answering = std::sync::atomic::AtomicBool::new(true);
    let ask = |sidecar: &Sidecar, _: Duration| {
        if sidecar.backend == "lobby" && answering.load(std::sync::atomic::Ordering::Relaxed) {
            Ok(status(7))
        } else {
            Err("no answer".into())
        }
    };
    poll(&config(), &ask, &statuses);
    answering.store(false, std::sync::atomic::Ordering::Relaxed);
    poll(&config(), &ask, &statuses);
    let later = Instant::now() + Duration::from_secs(60);
    assert_eq!(statuses.report("lobby", AGE, later), Report::Stale);
    assert_eq!(statuses.report("east", AGE, later), Report::Unknown);
    assert_eq!(
        statuses.report("lobby", Duration::from_secs(3600), later),
        Report::Fresh(status(7))
    );
}

#[test]
fn a_failure_is_worth_saying_once_until_the_backend_answers_again() {
    let statuses = Statuses::default();
    assert!(statuses.failed("lobby"));
    assert!(!statuses.failed("lobby"));
    assert!(statuses.recovered("lobby"));
    statuses.record("lobby", status(0), Instant::now());
    assert!(!statuses.recovered("lobby"));
    assert!(statuses.failed("lobby"));
}

#[test]
fn a_backend_removed_by_a_reload_is_forgotten() {
    let statuses = Statuses::default();
    statuses.record("lobby", status(1), Instant::now());
    statuses.record("gone", status(1), Instant::now());
    let ask = |_: &Sidecar, _: Duration| Ok(status(2));
    poll(&config(), &ask, &statuses);
    assert_eq!(
        statuses.report("gone", AGE, Instant::now()),
        Report::Unknown
    );
    assert!(matches!(
        statuses.report("lobby", AGE, Instant::now()),
        Report::Fresh(_)
    ));
}

// ---- The whole way: the proxy, a sidecar over a socket, and a backend's endpoint ----

const SECRET: &[u8] = b"the secret the proxy and the sidecar share";
const TOKEN: &str = "the backend's own token";

/// A stand-in for a Horizon server's control endpoint that answers the status line
/// with `answer`, as the real one is spoken to: `auth TOKEN`, then one command a line.
fn endpoint(answer: &'static str) -> meridian_sidecar::endpoint::Endpoint {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line.trim(), format!("auth {TOKEN}"));
            writeln!(stream, r#"{{"ok":true}}"#).unwrap();
            line.clear();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line.trim(), meridian_sidecar::request::STATUS_LINE);
            writeln!(stream, "{answer}").unwrap();
        }
    });
    meridian_sidecar::endpoint::Endpoint {
        port,
        token: TOKEN.into(),
    }
}

/// A sidecar in front of `endpoint` on a socket of its own, and the secret file the
/// proxy reads for it. The folder is removed when this is dropped.
struct Running {
    sidecar: Sidecar,
    folder: PathBuf,
    stop: Arc<AtomicBool>,
}

impl Running {
    fn start(answer: &'static str) -> Running {
        let found = endpoint(answer);
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let settings = Arc::new(meridian_sidecar::server::Settings {
            secret: SECRET.to_vec(),
            endpoint: Box::new(move || Ok(found.clone())),
            endpoint_timeout: Duration::from_secs(3),
            idle_timeout: Duration::from_secs(3),
            insecure_remote: false,
        });
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        std::thread::spawn(move || {
            meridian_sidecar::server::serve(listener, settings, &stopping).unwrap()
        });
        let folder = std::env::temp_dir().join(format!(
            "meridian-status-test-{}-{}",
            std::process::id(),
            address.replace([':', '.'], "-")
        ));
        std::fs::create_dir_all(&folder).unwrap();
        let secret_file = folder.join("lobby.key");
        std::fs::write(&secret_file, SECRET).unwrap();
        Running {
            sidecar: Sidecar {
                backend: "lobby".into(),
                address,
                secret_file,
            },
            folder,
            stop,
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = std::fs::remove_dir_all(&self.folder);
    }
}

#[test]
fn the_proxy_reads_a_backends_state_through_its_sidecar() {
    let running = Running::start(r#"{"players":6,"capacity":16,"accepts_transfers":false}"#);
    let status = ask_sidecar(&running.sidecar, Duration::from_secs(3)).unwrap();
    assert_eq!(
        status,
        ServerStatus {
            players: Some(6),
            capacity: Some(16),
            accepts_transfers: Some(false),
        }
    );
}

#[test]
fn a_sidecar_that_cannot_be_reached_or_read_is_an_error() {
    let running = Running::start("{}");
    let wrong_file = Sidecar {
        secret_file: running.folder.join("missing.key"),
        ..running.sidecar.clone()
    };
    assert!(ask_sidecar(&wrong_file, Duration::from_secs(1)).is_err());
    std::fs::write(
        running.folder.join("other.key"),
        b"another secret that is long enough",
    )
    .unwrap();
    let other = Sidecar {
        secret_file: running.folder.join("other.key"),
        ..running.sidecar.clone()
    };
    assert!(ask_sidecar(&other, Duration::from_secs(1)).is_err());
    let closed = Sidecar {
        address: "127.0.0.1:1".into(),
        ..running.sidecar.clone()
    };
    assert!(ask_sidecar(&closed, Duration::from_secs(1)).is_err());
}

/// The thread a running proxy starts asks the sidecar of a balanced backend while
/// balancing is on, and stops with the proxy.
#[test]
fn the_polling_thread_fills_the_statuses_while_balancing_is_on_and_stops_with_the_proxy() {
    let running = Running::start(r#"{"players":2,"capacity":10,"accepts_transfers":true}"#);
    let mut config = Config {
        sidecars: vec![running.sidecar.clone()],
        balanced: vec![BalancedServer {
            backend: "lobby".into(),
            role: Role::Lobby,
            soft_cap: 5,
            hard_cap: 5,
        }],
        ..Config::default()
    };
    config.balancing.enabled = true;
    config.balancing.probe_interval_seconds = 1;
    let stop = Arc::new(AtomicBool::new(false));
    let proxy = Arc::new(Proxy::new(config, PathBuf::from("meridian.toml"), stop));
    let thread = spawn(&proxy).unwrap();
    let started = Instant::now();
    let report = loop {
        let report = proxy.sessions.statuses.report("lobby", AGE, Instant::now());
        if report != Report::Unknown || started.elapsed() > Duration::from_secs(5) {
            break report;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(report, Report::Fresh(status(2).with_capacity(10)));
    proxy.stop();
    thread.join().unwrap();
}

/// With balancing off, the thread asks no one.
#[test]
fn the_polling_thread_asks_no_one_while_balancing_is_off() {
    let running = Running::start(r#"{"players":2}"#);
    let config = Config {
        sidecars: vec![running.sidecar.clone()],
        balanced: vec![BalancedServer {
            backend: "lobby".into(),
            role: Role::Lobby,
            soft_cap: 5,
            hard_cap: 5,
        }],
        ..Config::default()
    };
    assert!(!config.balancing.enabled);
    let stop = Arc::new(AtomicBool::new(false));
    let proxy = Arc::new(Proxy::new(config, PathBuf::from("meridian.toml"), stop));
    let thread = spawn(&proxy).unwrap();
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(
        proxy.sessions.statuses.report("lobby", AGE, Instant::now()),
        Report::Unknown
    );
    proxy.stop();
    thread.join().unwrap();
}

trait WithCapacity {
    fn with_capacity(self, capacity: u64) -> Self;
}

impl WithCapacity for ServerStatus {
    fn with_capacity(self, capacity: u64) -> Self {
        ServerStatus {
            capacity: Some(capacity),
            ..self
        }
    }
}
