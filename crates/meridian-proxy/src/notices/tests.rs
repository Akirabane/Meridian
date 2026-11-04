use super::*;
use crate::balance::Balancer;
use crate::restart::{Cause, Evacuate, Settings};
use meridian_config::{BalancedServer, Role};
use meridian_sidecar::management::Notices;
use meridian_sidecar::request::NoticeHeard;
use std::collections::VecDeque;
use std::net::{Ipv4Addr, TcpListener};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const STOPPING: &str = r#"{"jsonrpc":"2.0","method":"minecraft:notification/server/stopping"}"#;
const STARTED: &str = r#"{"jsonrpc":"2.0","method":"minecraft:notification/server/started"}"#;
const SECRET: &[u8] = b"the secret the proxy and the sidecar share";

#[derive(Default)]
struct Recorded(Mutex<Vec<(String, Cause)>>);

impl Evacuate for Recorded {
    fn evacuate(&self, backend: &str, cause: Cause) {
        self.0.lock().unwrap().push((backend.to_owned(), cause));
    }
}

fn detector() -> (Detector, Arc<Balancer>, Arc<Recorded>) {
    let balancer = Arc::new(Balancer::default());
    let recorded = Arc::new(Recorded::default());
    let detector = Detector::new(Arc::clone(&balancer), recorded.clone(), Settings::default());
    (detector, balancer, recorded)
}

fn sidecar(backend: &str, port: u16, notices: bool) -> Sidecar {
    Sidecar {
        backend: backend.into(),
        address: format!("127.0.0.1:{port}"),
        secret_file: PathBuf::from("unused"),
        notices,
    }
}

/// `east` gives notices; `lobby` has a role and a sidecar but is not set to; `loose` is
/// set to but has no role.
fn config() -> Config {
    let server = |backend: &str| BalancedServer {
        backend: backend.into(),
        role: Role::Lobby,
        soft_cap: 5,
        hard_cap: 5,
    };
    Config {
        sidecars: vec![
            sidecar("east", 25581, true),
            sidecar("lobby", 25580, false),
            sidecar("loose", 25582, true),
        ],
        balanced: vec![server("east"), server("lobby")],
        ..Config::default()
    }
}

fn heard(seq: u64, line: &str, age_secs: u64) -> NoticeHeard {
    NoticeHeard {
        seq,
        line: line.into(),
        age: Duration::from_secs(age_secs),
    }
}

fn report(last: u64, notices: Vec<NoticeHeard>) -> NoticeReport {
    NoticeReport {
        enabled: true,
        watching: true,
        last,
        notices,
    }
}

/// What a sidecar answers, in order, and what it was asked.
#[derive(Default)]
struct Script {
    answers: Mutex<VecDeque<Result<NoticeReport, String>>>,
    asked: Mutex<Vec<Option<u64>>>,
    opened: AtomicUsize,
}

impl Script {
    fn with(answers: Vec<Result<NoticeReport, String>>) -> Arc<Self> {
        Arc::new(Script {
            answers: Mutex::new(answers.into()),
            ..Script::default()
        })
    }

    fn asked(&self) -> Vec<Option<u64>> {
        self.asked.lock().unwrap().clone()
    }

    /// How the proxy reaches it: one script for every sidecar asked.
    fn open(
        self: &Arc<Self>,
    ) -> impl Fn(&Sidecar, Duration) -> Result<Box<dyn NoticeSource>, String> + use<> {
        let script = Arc::clone(self);
        move |sidecar: &Sidecar, _| {
            assert_eq!(sidecar.backend, "east", "only east is asked");
            script.opened.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(Scripted(Arc::clone(&script))))
        }
    }
}

struct Scripted(Arc<Script>);

impl NoticeSource for Scripted {
    fn notices(&mut self, after: Option<u64>) -> Result<NoticeReport, String> {
        self.0.asked.lock().unwrap().push(after);
        self.0
            .answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Ok(report(0, vec![])))
    }
}

fn run(config: &Config, script: &Arc<Script>, listeners: &Listeners, detector: &Detector) {
    round(config, &script.open(), listeners, detector, Instant::now());
}

#[test]
fn a_stop_the_server_announced_drains_it_and_is_taken_once() {
    let (detector, balancer, recorded) = detector();
    let script = Script::with(vec![
        // The first answer: where the count stands. Notices from before are not replayed.
        Ok(report(3, vec![])),
        Ok(report(4, vec![heard(4, STOPPING, 0)])),
        // The same notice again, as an answer asked twice would give it.
        Ok(report(4, vec![heard(4, STOPPING, 1)])),
        Ok(report(5, vec![heard(5, STARTED, 0)])),
    ]);
    let (config, listeners) = (config(), Listeners::default());
    run(&config, &script, &listeners, &detector);
    assert!(!balancer.is_drained("east"));
    run(&config, &script, &listeners, &detector);
    assert!(balancer.is_drained("east"));
    assert_eq!(
        recorded.0.lock().unwrap().clone(),
        [("east".to_owned(), Cause::Announced)]
    );
    run(&config, &script, &listeners, &detector);
    assert_eq!(recorded.0.lock().unwrap().len(), 1);
    // The server is up again.
    run(&config, &script, &listeners, &detector);
    assert!(!balancer.is_drained("east"));
    assert!(!detector.is_restarting("east"));
    // Each round asked after the last number taken, over one connection.
    assert_eq!(script.asked(), [None, Some(3), Some(4), Some(4)]);
    assert_eq!(script.opened.load(Ordering::SeqCst), 1);
}

#[test]
fn only_the_sidecars_set_to_give_notices_of_backends_with_a_role_are_asked() {
    let (detector, _, _) = detector();
    let script = Script::with(vec![]);
    let (config, listeners) = (config(), Listeners::default());
    // `lobby` and `loose` would panic in the script's open.
    run(&config, &script, &listeners, &detector);
    assert_eq!(script.asked(), [None]);
}

#[test]
fn a_notice_heard_long_ago_is_not_acted_on_but_is_taken() {
    let (detector, balancer, _) = detector();
    let script = Script::with(vec![
        Ok(report(0, vec![])),
        Ok(report(1, vec![heard(1, STOPPING, 600)])),
        Ok(report(1, vec![])),
    ]);
    let (config, listeners) = (config(), Listeners::default());
    for _ in 0..3 {
        run(&config, &script, &listeners, &detector);
    }
    assert!(!balancer.is_drained("east"));
    assert_eq!(script.asked(), [None, Some(0), Some(1)]);
}

#[test]
fn a_sidecar_that_began_its_count_again_is_heard_from_the_start() {
    let (detector, balancer, _) = detector();
    let script = Script::with(vec![
        Ok(report(9, vec![])),
        // Restarted: the count is lower than the number the proxy has.
        Ok(report(0, vec![])),
        Ok(report(1, vec![heard(1, STOPPING, 0)])),
    ]);
    let (config, listeners) = (config(), Listeners::default());
    for _ in 0..3 {
        run(&config, &script, &listeners, &detector);
    }
    assert!(balancer.is_drained("east"));
    assert_eq!(script.asked(), [None, Some(9), Some(0)]);
}

#[test]
fn a_sidecar_that_cannot_be_reached_is_tried_again_after_a_wait_and_said_once() {
    let (detector, _, _) = detector();
    let opened = AtomicUsize::new(0);
    let open = |_: &Sidecar, _: Duration| -> Result<Box<dyn NoticeSource>, String> {
        opened.fetch_add(1, Ordering::SeqCst);
        Err("refused".into())
    };
    let (config, listeners) = (config(), Listeners::default());
    for _ in 0..5 {
        round(&config, &open, &listeners, &detector, Instant::now());
    }
    // Five rounds at once: one try, then the wait.
    assert_eq!(opened.load(Ordering::SeqCst), 1);
    std::thread::sleep(Duration::from_millis(1100));
    round(&config, &open, &listeners, &detector, Instant::now());
    assert_eq!(opened.load(Ordering::SeqCst), 2);
}

#[test]
fn the_wait_grows_to_half_a_minute() {
    let waits: Vec<u64> = (1..=8).map(|n| retry_after(n).as_secs()).collect();
    assert_eq!(waits, [1, 2, 4, 8, 16, 30, 30, 30]);
}

#[test]
fn a_connection_that_failed_is_not_used_again() {
    let (detector, balancer, _) = detector();
    let script = Script::with(vec![
        Ok(report(0, vec![])),
        Err("the sidecar closed the connection".into()),
        Ok(report(0, vec![])),
        Ok(report(1, vec![heard(1, STOPPING, 0)])),
    ]);
    let (config, listeners) = (config(), Listeners::default());
    run(&config, &script, &listeners, &detector);
    run(&config, &script, &listeners, &detector);
    assert_eq!(script.opened.load(Ordering::SeqCst), 1);
    std::thread::sleep(Duration::from_millis(1100));
    // A new connection, and the number the proxy has is asked after again.
    run(&config, &script, &listeners, &detector);
    run(&config, &script, &listeners, &detector);
    assert_eq!(script.opened.load(Ordering::SeqCst), 2);
    assert!(balancer.is_drained("east"));
    assert_eq!(script.asked(), [None, Some(0), Some(0), Some(0)]);
}

#[test]
fn a_sidecar_that_does_not_listen_is_left_alone_for_a_while() {
    let (detector, balancer, _) = detector();
    let script = Script::with(vec![
        Ok(NoticeReport::default()),
        Ok(report(1, vec![heard(1, STOPPING, 0)])),
    ]);
    let (config, listeners) = (config(), Listeners::default());
    run(&config, &script, &listeners, &detector);
    run(&config, &script, &listeners, &detector);
    run(&config, &script, &listeners, &detector);
    assert_eq!(script.asked().len(), 1);
    assert!(!balancer.is_drained("east"));
}

#[test]
fn a_backend_a_reload_removed_is_forgotten() {
    let (detector, _, _) = detector();
    let script = Script::with(vec![Ok(report(7, vec![]))]);
    let (mut config, listeners) = (config(), Listeners::default());
    run(&config, &script, &listeners, &detector);
    config.sidecars.retain(|s| s.backend != "east");
    run(&config, &script, &listeners, &detector);
    config.sidecars.push(sidecar("east", 25581, true));
    run(&config, &script, &listeners, &detector);
    // Asked from the start again, not after 7.
    assert_eq!(script.asked(), [None, None]);
}

/// The two programs together: a sidecar that heard `server/stopping` on its side, and the
/// proxy asking it over the signed exchange, with the secret in its file.
#[test]
fn a_notice_the_sidecar_heard_reaches_the_detector_over_the_signed_exchange() {
    let notices = Arc::new(Notices::default());
    notices.set_watching(true);
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let settings = Arc::new(meridian_sidecar::server::Settings {
        secret: SECRET.to_vec(),
        endpoint: Box::new(|| Err(meridian_sidecar::Error::Endpoint("no backend".into()))),
        endpoint_timeout: Duration::from_secs(1),
        idle_timeout: Duration::from_secs(10),
        insecure_remote: false,
        notices: Some(Arc::clone(&notices)),
    });
    let stop = Arc::new(AtomicBool::new(false));
    let serving = {
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            meridian_sidecar::server::serve(listener, settings, &stop).unwrap()
        })
    };
    let folder = std::env::temp_dir().join(format!("meridian-notices-test-{}", std::process::id()));
    std::fs::create_dir_all(&folder).unwrap();
    let secret_file = folder.join("east.key");
    std::fs::write(&secret_file, SECRET).unwrap();
    let mut config = config();
    config.sidecars = vec![Sidecar {
        backend: "east".into(),
        address,
        secret_file,
        notices: true,
    }];
    config.sidecar_timeout_ms = 3_000;

    let (detector, balancer, recorded) = detector();
    let listeners = Listeners::default();
    let open = |s: &Sidecar, t: Duration| open_sidecar(s, t);
    // The first round learns where the count stands.
    round(&config, &open, &listeners, &detector, Instant::now());
    assert!(!balancer.is_drained("east"));
    // The server announces its stop; the next round, which waits for it, takes it.
    let hearing = {
        let notices = Arc::clone(&notices);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            notices.offer(STOPPING);
        })
    };
    let begun = Instant::now();
    round(&config, &open, &listeners, &detector, Instant::now());
    hearing.join().unwrap();
    assert!(
        begun.elapsed() < Duration::from_millis(1500),
        "{:?}",
        begun.elapsed()
    );
    assert!(balancer.is_drained("east"));
    assert_eq!(
        recorded.0.lock().unwrap().clone(),
        [("east".to_owned(), Cause::Announced)]
    );
    stop.store(true, Ordering::Relaxed);
    serving.join().unwrap();
    let _ = std::fs::remove_dir_all(&folder);
}
