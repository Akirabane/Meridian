use super::*;
use crate::extension::events::told_events;
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
        ..ServerStatus::default()
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
        notices: false,
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

/// A backend that answers "unknown request" is alive: it does not count as failing, so
/// the restart detector does not take it for a backend going down, and it stays unknown.
#[test]
fn a_backend_that_does_not_know_the_request_is_alive_and_stays_unknown() {
    let statuses = Statuses::default();
    let ask = |_: &Sidecar, _: Duration| {
        Err("the sidecar refused: unknown request \"transfer-status\"".to_owned())
    };
    for _ in 0..5 {
        poll(&config(), &ask, &statuses);
        assert!(!statuses.failing("lobby"));
    }
    assert_eq!(
        statuses.report("lobby", AGE, Instant::now()),
        Report::Unknown
    );
    // It is said once, and again only after a real report in between.
    assert!(!statuses.unreported("lobby"));
    statuses.record("lobby", status(1), Instant::now());
    assert!(statuses.unreported("lobby"));
}

/// Any other refusal, such as a sidecar that closes the connection, is still a failure.
#[test]
fn another_refusal_is_still_a_failure() {
    let statuses = Statuses::default();
    let ask = |_: &Sidecar, _: Duration| {
        Err("the sidecar refused: the sidecar closed the connection".to_owned())
    };
    poll(&config(), &ask, &statuses);
    assert!(statuses.failing("lobby"));
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

// ---- The states the extensions are told of ----

/// What `poll` says changed, by backend name (the sidecars are asked at once, so the order is not fixed).
fn changed(
    ask: &(impl Fn(&Sidecar, Duration) -> Result<ServerStatus, String> + Sync),
    statuses: &Statuses,
) -> Vec<(String, bool)> {
    let mut changes: Vec<(String, bool)> = poll(&config(), ask, statuses)
        .into_iter()
        .map(|c| (c.backend, c.answers))
        .collect();
    changes.sort();
    changes
}

/// A state is told only when it **changes**: the first round that reads a backend sets the state it starts from (a backend that is
/// down at that round is not a news, and nor is one that is up), a backend that stays as it is is not told again, and one that answers
/// "unknown request" answers.
#[test]
fn only_a_change_of_state_is_a_change_and_the_first_reading_is_not() {
    let statuses = Statuses::default();
    let lobby_answers = AtomicBool::new(true);
    let east = Mutex::new(Ok(status(1)));
    let ask = |sidecar: &Sidecar, _: Duration| {
        if sidecar.backend == "lobby" {
            if lobby_answers.load(Ordering::Relaxed) {
                Ok(status(3))
            } else {
                Err("no answer".to_owned())
            }
        } else {
            east.lock().unwrap().clone()
        }
    };
    assert!(
        changed(&ask, &statuses).is_empty(),
        "the first reading is the base"
    );
    assert!(changed(&ask, &statuses).is_empty(), "nothing changed");
    lobby_answers.store(false, Ordering::Relaxed);
    assert_eq!(changed(&ask, &statuses), [("lobby".to_owned(), false)]);
    assert!(changed(&ask, &statuses).is_empty(), "down is told once");
    lobby_answers.store(true, Ordering::Relaxed);
    assert_eq!(changed(&ask, &statuses), [("lobby".to_owned(), true)]);
    // A backend that does not know the request is alive: it is not a change, and it stays up.
    *east.lock().unwrap() =
        Err("the sidecar refused: unknown request \"transfer-status\"".to_owned());
    assert!(changed(&ask, &statuses).is_empty());
    // Another refusal is a silence.
    *east.lock().unwrap() =
        Err("the sidecar refused: the sidecar closed the connection".to_owned());
    assert_eq!(changed(&ask, &statuses), [("east".to_owned(), false)]);
}

/// A backend that is down at the first round is the base too: it is told when it comes up, and not before.
#[test]
fn a_backend_that_is_down_at_the_first_reading_is_told_when_it_comes_up() {
    let statuses = Statuses::default();
    let up = AtomicBool::new(false);
    let ask = |_: &Sidecar, _: Duration| {
        if up.load(Ordering::Relaxed) {
            Ok(status(0))
        } else {
            Err("no answer".to_owned())
        }
    };
    assert!(changed(&ask, &statuses).is_empty());
    assert!(changed(&ask, &statuses).is_empty());
    up.store(true, Ordering::Relaxed);
    assert_eq!(
        changed(&ask, &statuses),
        [("east".to_owned(), true), ("lobby".to_owned(), true)]
    );
}

/// A backend a reload removed is forgotten, and when it comes back the first reading is its base again.
#[test]
fn a_backend_a_reload_removed_starts_again_from_its_first_reading() {
    let statuses = Statuses::default();
    assert_eq!(statuses.saw("gone", true), None);
    let ask = |_: &Sidecar, _: Duration| Ok(status(1));
    // `gone` is not in the configuration, so the round forgets it.
    poll(&config(), &ask, &statuses);
    assert_eq!(statuses.saw("gone", false), None, "its base was forgotten");
    assert_eq!(
        statuses.saw("gone", true),
        Some(Change {
            backend: "gone".into(),
            answers: true
        })
    );
}

/// An evacuation that moves nobody: the detector is only asked whether a backend is going down.
struct Nobody;

impl crate::restart::Evacuate for Nobody {
    fn evacuate(&self, _: &str, _: crate::restart::Cause) {}
}

/// The extensions that listen are told `servers/down` for a backend that stopped answering, with `restart` as the cause when the detector
/// believes it is going down for one and `unreachable` when it does not, and `servers/up` (with no cause) for one that answers again.
/// Nothing but the name of the server is in it.
#[test]
fn the_extensions_are_told_a_server_that_went_down_with_why_and_one_that_came_back() {
    let registry = Registry::default();
    let told = registry.bus().recording(false);
    let detector = Detector::new(
        Arc::new(crate::balance::Balancer::default()),
        Arc::new(Nobody),
        crate::restart::Settings::default(),
    );
    let change = |backend: &str, answers| Change {
        backend: backend.to_owned(),
        answers,
    };
    // It was not announced: it does not answer, and that is all that is known.
    tell(&registry, &detector, vec![change("lobby", false)]);
    // It announced its stop, and then stopped answering.
    detector.announce("east", crate::restart::Cause::Announced, Instant::now());
    tell(&registry, &detector, vec![change("east", false)]);
    tell(&registry, &detector, vec![change("lobby", true)]);
    let events = told_events(&told, 3);
    assert_eq!(events[0].0, "meridian:notification/servers/down");
    assert_eq!(
        (
            events[0].1["server"].as_str(),
            events[0].1["cause"].as_str()
        ),
        (Some("lobby"), Some("unreachable"))
    );
    assert_eq!(events[1].0, "meridian:notification/servers/down");
    assert_eq!(
        (
            events[1].1["server"].as_str(),
            events[1].1["cause"].as_str()
        ),
        (Some("east"), Some("restart"))
    );
    assert_eq!(events[2].0, "meridian:notification/servers/up");
    assert_eq!(events[2].1["server"], "lobby");
    assert!(events[2].1.get("cause").is_none(), "an up has no cause");
    let seqs: Vec<_> = events
        .iter()
        .map(|e| e.1["seq"].as_u64().unwrap())
        .collect();
    assert_eq!(seqs, [1, 2, 3]);
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
            notices: None,
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
                notices: false,
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
            ..ServerStatus::default()
        }
    );
}

/// Horizon's `transfer-status` answer, from the backend's endpoint to the proxy's report.
#[test]
fn the_proxy_reads_horizons_transfer_status_through_its_sidecar() {
    let running = Running::start(
        r#"{"ok":true,"players":12,"held":1,"expected":2,"frozen":0,"online":13,"max_players":20,"full":false}"#,
    );
    let status = ask_sidecar(&running.sidecar, Duration::from_secs(3)).unwrap();
    assert_eq!(
        status,
        ServerStatus {
            players: Some(13),
            capacity: Some(20),
            accepts_transfers: Some(true),
            held: Some(1),
            expected: Some(2),
            frozen: Some(0),
            full: Some(false),
            stopping: None,
        }
    );
}

/// A server whose transfer module is off answers an error to `transfer-status`: the proxy
/// reads it as a report that the server takes no transfers, not as a failure.
#[test]
fn a_server_with_transfers_off_reports_that_it_takes_none() {
    let running = Running::start(r#"{"error":"transfers are off on this server"}"#);
    let status = ask_sidecar(&running.sidecar, Duration::from_secs(3)).unwrap();
    assert_eq!(status.accepts_transfers, Some(false));
    let statuses = Statuses::default();
    statuses.record("lobby", status, Instant::now());
    assert!(!statuses.failing("lobby"));
}

#[test]
fn a_sidecar_that_cannot_be_reached_or_read_is_an_error() {
    let running = Running::start("{}");
    let wrong_file = Sidecar {
        secret_file: running.folder.join("missing.key"),
        notices: false,
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
        notices: false,
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

/// A round of readings starts every `probe-interval-seconds`, however long the last one took. A
/// sidecar that takes the connection and never answers holds each round for the whole timeout;
/// the pause must not come on top of it, or each reading of a dead server comes later than the
/// interval says, and the detector of restarts decides later.
#[test]
fn the_rounds_start_at_the_interval_however_long_a_silent_sidecar_holds_them() {
    // Connections wait in the listener's backlog, are taken, and are never answered.
    let silent = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = silent.local_addr().unwrap().to_string();
    let folder = std::env::temp_dir().join(format!(
        "meridian-status-silent-{}-{}",
        std::process::id(),
        address.replace([':', '.'], "-")
    ));
    std::fs::create_dir_all(&folder).unwrap();
    let secret_file = folder.join("lobby.key");
    std::fs::write(&secret_file, SECRET).unwrap();
    let mut config = Config {
        sidecars: vec![Sidecar {
            backend: "lobby".into(),
            address,
            secret_file,
            notices: false,
        }],
        balanced: vec![BalancedServer {
            backend: "lobby".into(),
            role: Role::Lobby,
            soft_cap: 5,
            hard_cap: 5,
        }],
        sidecar_timeout_ms: 500,
        ..Config::default()
    };
    config.balancing.enabled = true;
    config.balancing.probe_interval_seconds = 1;
    config.restart.silent_rounds = 3;
    // When each round reached the sidecar.
    let taking = std::thread::spawn(move || {
        let mut held = Vec::new();
        let mut at = Vec::new();
        while at.len() < 4 {
            held.push(silent.accept().unwrap().0);
            at.push(Instant::now());
        }
        (silent, held, at)
    });
    let stop = Arc::new(AtomicBool::new(false));
    let proxy = Arc::new(Proxy::new(config, PathBuf::from("meridian.toml"), stop));
    let thread = spawn(&proxy).unwrap();
    let (silent, held, at) = taking.join().unwrap();
    // The first round may have started at any moment; two whole periods from the second on.
    // Each round lasts 500 ms: rounds a second apart take 2 s, rounds that wait a second after
    // the last one ended take 3 s.
    let two_periods = at[3] - at[1];
    // The third silent round has ended by the fourth start: the detector decided.
    let decided = proxy.restarts.is_restarting("lobby");
    proxy.stop();
    thread.join().unwrap();
    drop((silent, held));
    let _ = std::fs::remove_dir_all(&folder);
    assert!(
        two_periods < Duration::from_millis(2_500),
        "two rounds took {two_periods:?}, not about 2 s: the pause comes after each round"
    );
    assert!(decided, "three silent rounds make the backend going down");
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

// ---- The ping ----

#[test]
fn a_measure_is_used_for_the_maximum_age_and_then_not() {
    let statuses = Statuses::default();
    let start = Instant::now();
    assert_eq!(statuses.ping("lobby", AGE, start), None);
    statuses.record_ping("lobby", Duration::from_millis(12), start);
    let at = |secs: u64| start + Duration::from_secs(secs);
    assert_eq!(
        statuses.ping("lobby", AGE, at(15)),
        Some(Duration::from_millis(12))
    );
    assert_eq!(statuses.ping("lobby", AGE, at(16)), None);
    assert_eq!(statuses.ping("east", AGE, at(1)), None);
    statuses.retain(|_| false);
    assert_eq!(statuses.ping("lobby", AGE, at(1)), None);
}

/// Every backend that has a role is measured, those with none are not, and one that
/// does not answer keeps no measure.
#[test]
fn only_the_balanced_backends_are_measured_and_a_silent_one_has_no_measure() {
    let mut config = config();
    config.backends = ["lobby", "east", "unbalanced"]
        .iter()
        .enumerate()
        .map(|(i, name)| meridian_config::Backend {
            name: (*name).into(),
            address: format!("127.0.0.1:{}", 25600 + i),
        })
        .collect();
    config.connect_timeout_ms = 777;
    let asked = Mutex::new(Vec::new());
    let probe = |address: &str, timeout: Duration| {
        asked.lock().unwrap().push((address.to_owned(), timeout));
        match address {
            "127.0.0.1:25600" => Ok(Duration::from_millis(4)),
            _ => Err("refused".to_owned()),
        }
    };
    let statuses = Statuses::default();
    measure(&config, &probe, &statuses);
    let mut asked = asked.into_inner().unwrap();
    asked.sort();
    assert_eq!(
        asked,
        [
            ("127.0.0.1:25600".to_owned(), Duration::from_millis(777)),
            ("127.0.0.1:25601".to_owned(), Duration::from_millis(777)),
        ]
    );
    let now = Instant::now();
    assert_eq!(
        statuses.ping("lobby", AGE, now),
        Some(Duration::from_millis(4))
    );
    assert_eq!(statuses.ping("east", AGE, now), None);
    assert_eq!(statuses.ping("unbalanced", AGE, now), None);
}

/// The polling thread measures with the proxy's own probe while the tie is broken by
/// ping, and not otherwise.
#[test]
fn the_polling_thread_measures_only_when_the_tie_is_broken_by_ping() {
    for (tie_break, measured) in [
        (meridian_config::TieBreak::Ping, true),
        (meridian_config::TieBreak::Order, false),
    ] {
        let mut config = config();
        config.backends = vec![meridian_config::Backend {
            name: "lobby".into(),
            address: "127.0.0.1:25600".into(),
        }];
        config.balancing.enabled = true;
        config.balancing.tie_break = tie_break;
        config.balancing.probe_interval_seconds = 1;
        let stop = Arc::new(AtomicBool::new(false));
        let mut proxy = Proxy::new(config, PathBuf::from("meridian.toml"), stop);
        proxy.probe = Arc::new(|_, _| Ok(Duration::from_millis(9)));
        let proxy = Arc::new(proxy);
        let thread = spawn(&proxy).unwrap();
        let started = Instant::now();
        let patience = Duration::from_millis(if measured { 5000 } else { 500 });
        while proxy
            .sessions
            .statuses
            .ping("lobby", AGE, Instant::now())
            .is_none()
            && started.elapsed() < patience
        {
            std::thread::sleep(Duration::from_millis(25));
        }
        let got = proxy.sessions.statuses.ping("lobby", AGE, Instant::now());
        assert_eq!(got.is_some(), measured, "{tie_break:?}");
        proxy.stop();
        thread.join().unwrap();
    }
}
