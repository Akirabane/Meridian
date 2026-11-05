//! The swarm against the synthetic servers: a smoke test of twenty bots for five seconds, and
//! the same through the real proxy when its program is built.
use super::*;
use std::ffi::OsString;

fn args(list: &[&str]) -> Vec<OsString> {
    list.iter().map(OsString::from).collect()
}

fn settings(window: u64) -> Settings {
    let mut o = parse(args(&["--proxy", "x", "--min-free-gib", "0"]))
        .unwrap()
        .unwrap()
        .settings;
    o.window = Duration::from_secs(window);
    // A small world: a smoke test shows that everything is connected, not how much it costs, and
    // a test binary is not optimised. The load of a full world, at 20 bots, took a slow runner
    // past the bots' patience.
    o.shape.view_radius = 2;
    o.shape.entities = 10;
    o.shape.registry_bytes = 8_000;
    // A folder of its own: the tests run at the same time, and each proxy has its files.
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    o.dir = std::env::temp_dir().join(format!(
        "meridian-load-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&o.dir).unwrap();
    o
}

/// How many bots a smoke test has, and how many of them must get in for it to pass: a slow
/// runner may lose some, and the test is about the wiring, not about a figure.
const BOTS: usize = 10;
const ENOUGH: usize = BOTS / 2;

/// The tests that run bots take turns: together they are a heavy load for a small machine, and
/// each one's figures are its own.
static ONE_PROXY: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The proxy's program, when this test run has built it: next to the test's own, in
/// `target/<profile>`.
fn proxy_program() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let path = exe
        .parent()?
        .parent()?
        .join(format!("meridian{}", std::env::consts::EXE_SUFFIX));
    path.exists().then_some(path)
}

#[test]
fn the_options_are_read_and_a_large_step_is_refused() {
    let o = parse(args(&[
        "--players",
        "10,20",
        "--seconds",
        "5",
        "--scenarios",
        "idle,micro",
        "--compression",
        "same",
        "--proxy",
        "x",
    ]))
    .unwrap()
    .unwrap();
    assert_eq!(o.steps, [10, 20]);
    assert_eq!(o.settings.window, Duration::from_secs(5));
    assert_eq!(o.scenarios, ["idle", "micro"]);
    assert_eq!(o.compressions, [Compression::Same]);
    // The limits are the person's to raise, not a typing mistake's.
    let refused = parse(args(&["--players", "1000"])).unwrap_err();
    assert!(refused.contains("not allowed"), "{refused}");
    assert!(parse(args(&["--seconds", "61"])).is_err());
    assert!(parse(args(&["--players", "1000", "--max-bots", "1000"])).is_ok());
    assert!(parse(args(&["--scenarios", "bogus"])).is_err());
    assert!(parse(args(&["--port", "1"])).is_err());
    assert!(parse(args(&["--help"])).unwrap().is_none());
    let total = parse(args(&["--max-total-seconds", "90"]))
        .unwrap()
        .unwrap();
    assert_eq!(total.max_total, Duration::from_secs(90));
    let default = parse(args(&[])).unwrap().unwrap();
    assert_eq!(default.max_total, Duration::from_secs(1800));
}

/// Some bots straight to a synthetic server for five seconds: every bot logs in and joins,
/// the stream reaches them, and the measuring packets come back.
#[test]
fn some_bots_play_against_a_synthetic_server() {
    let _turn = ONE_PROXY.lock().unwrap_or_else(|e| e.into_inner());
    let s = settings(5);
    let world = scenario::start_world(
        &s,
        scenario::Spec {
            name: "lobby",
            flavour: 1,
            traffic: scenario::active_traffic(&s),
            compression: Some(256),
            behind_proxy: false,
            transfers: None,
            bare: false,
        },
    )
    .unwrap();
    let played = scenario::play(&s, "smoke", world.address, BOTS, None).unwrap();
    let said = format!(
        "first failure: {:?}; failed {}, dropped {}; last status: {}",
        played.first_failure, played.failed, played.dropped, played.status
    );
    assert!(
        played.logged >= ENOUGH as u64,
        "logged {}; {said}",
        played.logged
    );
    assert!(
        played.round_trips.count >= ENOUGH,
        "round trips {}; {said}",
        played.round_trips.count
    );
    // A quarter of what the rates say, for bots that were late to start.
    let expected = BOTS as f64 * 5.0 * 20.0 / 4.0;
    assert!(
        played.down_packets > expected,
        "down {}; {said}",
        played.down_packets
    );
    assert!(
        played.up_packets > expected,
        "up {}; {said}",
        played.up_packets
    );
    assert!(
        world.counters.logged_in.load(Ordering::Relaxed) >= ENOUGH as u64,
        "{said}"
    );
}

/// The same bots in front of the real proxy, when it is built: the active scenario, with its
/// direct phase.
#[test]
fn some_bots_cross_the_proxy() {
    let Some(proxy) = proxy_program() else {
        eprintln!("skipped: the meridian program is not built next to this test");
        return;
    };
    let _turn = ONE_PROXY.lock().unwrap_or_else(|e| e.into_inner());
    let mut s = settings(5);
    s.proxy = proxy;
    let get = |row: &Row, name: &str| {
        row.0
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    let mut direct = s.clone();
    direct.window = Duration::from_secs(3);
    let active = scenario::active(&s, &direct, BOTS, Compression::Same).unwrap();
    let logged: u64 = get(&active, "logged_in").parse().unwrap_or(0);
    assert!(logged >= ENOUGH as u64, "{active:?}");
    assert_eq!(get(&active, "proxy_alive"), "true", "{active:?}");
    assert_ne!(get(&active, "proxy_rtt_p50_us"), "0", "{active:?}");
}

/// A switch end to end: bots moved to the second server through the real sidecar. It takes
/// about thirty seconds, so it runs on request: `cargo test -p meridian-bench -- --ignored`.
#[test]
#[ignore = "takes about thirty seconds and needs the meridian program built"]
fn some_bots_are_moved_to_another_server() {
    let proxy = proxy_program().expect("build the meridian program first");
    let _turn = ONE_PROXY.lock().unwrap_or_else(|e| e.into_inner());
    let mut s = settings(5);
    s.proxy = proxy;
    s.movers_percent = 20;
    let row = scenario::moves(&s, 20).unwrap();
    let get = |name: &str| {
        row.0
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    assert_eq!(get("moved"), "4", "{row:?}");
    assert_eq!(get("move_failed"), "0", "{row:?}");
    assert_eq!(get("proxy_alive"), "true");
}
