use super::*;
use crate::status::poll;
use meridian_config::{BalancedServer, Role, Sidecar};
use meridian_sidecar::request::ServerStatus;
use std::path::PathBuf;

/// An evacuation that only writes down what it was asked.
#[derive(Default)]
struct Recorded(Mutex<Vec<(String, Cause)>>);

impl Evacuate for Recorded {
    fn evacuate(&self, backend: &str, cause: Cause) {
        self.0.lock().unwrap().push((backend.to_owned(), cause));
    }
}

impl Recorded {
    fn calls(&self) -> Vec<(String, Cause)> {
        self.0.lock().unwrap().clone()
    }
}

fn detector() -> (Detector, Arc<Balancer>, Arc<Recorded>) {
    let balancer = Arc::new(Balancer::default());
    let recorded = Arc::new(Recorded::default());
    let detector = Detector::new(
        Arc::clone(&balancer),
        recorded.clone(),
        Settings {
            misses: 3,
            window: Duration::from_secs(60),
        },
    );
    (detector, balancer, recorded)
}

fn at(start: Instant, secs: u64) -> Instant {
    start + Duration::from_secs(secs)
}

const STOPPING: &str = r#"{"jsonrpc":"2.0","method":"minecraft:notification/server/stopping"}"#;
const STARTED: &str = r#"{"jsonrpc":"2.0","method":"minecraft:notification/server/started"}"#;

#[test]
fn the_notices_of_the_management_protocol_are_read() {
    assert_eq!(notice(STOPPING), Some(Notice::Stopping));
    assert_eq!(notice(STARTED), Some(Notice::Started));
    // The prefix is optional.
    assert_eq!(
        notice(r#"{"jsonrpc":"2.0","method":"notification/server/stopping"}"#),
        Some(Notice::Stopping)
    );
    // The notifications Horizon also sends are not for the detector.
    for other in [
        r#"{"jsonrpc":"2.0","method":"minecraft:notification/players/joined","params":[{}]}"#,
        r#"{"jsonrpc":"2.0","method":"minecraft:notification/server/saving"}"#,
        // A reply and not a notification.
        r#"{"jsonrpc":"2.0","id":1,"result":true}"#,
        r#"{"method":42}"#,
        "not json",
        "",
    ] {
        assert_eq!(notice(other), None, "{other}");
    }
}

#[test]
fn a_stop_notice_drains_the_backend_and_evacuates_it_once() {
    let (detector, balancer, recorded) = detector();
    let now = Instant::now();
    assert!(detector.handle_notification("east", STOPPING, now));
    assert!(balancer.is_drained("east"));
    assert!(detector.is_restarting("east"));
    assert_eq!(recorded.calls(), [("east".to_owned(), Cause::Announced)]);
    // The panel says it too: it is the same restart.
    assert!(!detector.announce("east", Cause::Panel, at(now, 1)));
    assert_eq!(recorded.calls().len(), 1);
    // Another backend is not touched.
    assert!(!balancer.is_drained("lobby"));
    assert!(!detector.is_restarting("lobby"));
}

#[test]
fn a_message_that_is_not_a_notice_does_nothing() {
    let (detector, balancer, recorded) = detector();
    assert!(!detector.handle_notification("east", r#"{"id":1,"result":true}"#, Instant::now()));
    assert!(!balancer.is_drained("east"));
    assert!(recorded.calls().is_empty());
}

#[test]
fn the_started_notice_offers_the_backend_again() {
    let (detector, balancer, _) = detector();
    let now = Instant::now();
    detector.handle_notification("east", STOPPING, now);
    detector.handle_notification("east", STARTED, at(now, 20));
    assert!(!balancer.is_drained("east"));
    assert!(!detector.is_restarting("east"));
    // A second one changes nothing.
    detector.handle_notification("east", STARTED, at(now, 21));
    assert!(!balancer.is_drained("east"));
}

#[test]
fn a_backend_that_stops_answering_is_going_down_after_the_misses() {
    let (detector, balancer, recorded) = detector();
    let now = Instant::now();
    detector.observe("east", false, at(now, 0));
    detector.observe("east", false, at(now, 5));
    assert!(!balancer.is_drained("east"), "two misses are not enough");
    detector.observe("east", false, at(now, 10));
    assert!(balancer.is_drained("east"));
    assert_eq!(recorded.calls(), [("east".to_owned(), Cause::Unreachable)]);
    // More misses do not evacuate again.
    detector.observe("east", false, at(now, 15));
    assert_eq!(recorded.calls().len(), 1);
    // It answers: it is back, offered again.
    detector.observe("east", true, at(now, 20));
    assert!(!balancer.is_drained("east"));
    assert!(!detector.is_restarting("east"));
}

#[test]
fn an_answer_between_misses_starts_the_count_again() {
    let (detector, balancer, recorded) = detector();
    let now = Instant::now();
    for (i, answered) in [false, false, true, false, false, true]
        .into_iter()
        .enumerate()
    {
        detector.observe("east", answered, at(now, i as u64 * 5));
    }
    assert!(!balancer.is_drained("east"));
    assert!(recorded.calls().is_empty());
}

/// After the notice the backend goes on answering while it shuts down: that is not a
/// return. It is back when it stopped answering and answers again.
#[test]
fn an_answer_while_it_shuts_down_is_not_a_return() {
    let (detector, balancer, _) = detector();
    let now = Instant::now();
    detector.announce("east", Cause::Announced, now);
    detector.observe("east", true, at(now, 5));
    detector.observe("east", true, at(now, 10));
    assert!(balancer.is_drained("east"));
    // It went down...
    detector.observe("east", false, at(now, 15));
    detector.observe("east", false, at(now, 20));
    assert!(balancer.is_drained("east"));
    // ...and it is back.
    detector.observe("east", true, at(now, 25));
    assert!(!balancer.is_drained("east"));
    assert!(!detector.is_restarting("east"));
}

/// A restart faster than a round is never seen as a miss. The window ends it.
#[test]
fn a_backend_that_never_went_quiet_is_offered_again_after_the_window() {
    let (detector, balancer, _) = detector();
    let now = Instant::now();
    detector.announce("east", Cause::Panel, now);
    detector.observe("east", true, at(now, 59));
    assert!(balancer.is_drained("east"));
    detector.observe("east", true, at(now, 60));
    assert!(!balancer.is_drained("east"));
}

#[test]
fn a_backend_the_operator_drained_stays_drained() {
    let (detector, balancer, recorded) = detector();
    let now = Instant::now();
    assert!(balancer.drain("east"));
    // Its players are still evacuated.
    assert!(detector.announce("east", Cause::Announced, now));
    assert_eq!(recorded.calls().len(), 1);
    detector.handle_notification("east", STARTED, at(now, 30));
    assert!(
        balancer.is_drained("east"),
        "the operator's drain is theirs"
    );
    assert!(!detector.is_restarting("east"));
}

#[test]
fn a_backend_can_restart_again() {
    let (detector, balancer, recorded) = detector();
    let now = Instant::now();
    detector.announce("east", Cause::Announced, now);
    detector.back("east");
    assert!(detector.announce("east", Cause::Announced, at(now, 100)));
    assert!(balancer.is_drained("east"));
    assert_eq!(recorded.calls().len(), 2);
}

#[test]
fn a_backend_a_reload_removed_is_forgotten() {
    let (detector, _, _) = detector();
    let now = Instant::now();
    detector.announce("east", Cause::Announced, now);
    detector.observe("lobby", false, now);
    detector.retain(|b| b == "lobby");
    assert!(!detector.is_restarting("east"));
}

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
        ..Config::default()
    }
}

/// The rounds of the poller, end to end: a sidecar that stops answering makes its backend
/// drain, and the others, and the backends without a role, are left alone.
#[test]
fn the_rounds_of_the_poller_drain_a_backend_whose_sidecar_goes_quiet() {
    let (detector, balancer, recorded) = detector();
    let config = config();
    let statuses = Statuses::default();
    let east_down = |sidecar: &Sidecar, _: Duration| {
        if sidecar.backend == "east" || sidecar.backend == "unbalanced" {
            Err("down".to_owned())
        } else {
            Ok(ServerStatus {
                players: Some(1),
                capacity: Some(20),
                accepts_transfers: Some(true),
            })
        }
    };
    let now = Instant::now();
    for round in 0..3 {
        poll(&config, &east_down, &statuses);
        observe_round(&config, &statuses, &detector, at(now, round * 5));
    }
    assert!(balancer.is_drained("east"));
    assert!(!balancer.is_drained("lobby"));
    assert!(!balancer.is_drained("unbalanced"));
    assert_eq!(recorded.calls(), [("east".to_owned(), Cause::Unreachable)]);

    // East answers again.
    let all_up = |_: &Sidecar, _: Duration| {
        Ok(ServerStatus {
            players: Some(0),
            capacity: Some(20),
            accepts_transfers: Some(true),
        })
    };
    poll(&config, &all_up, &statuses);
    observe_round(&config, &statuses, &detector, at(now, 20));
    assert!(!balancer.is_drained("east"));
}
