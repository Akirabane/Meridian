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
    let direct = parse(args(&["--direct-waves", "3"])).unwrap().unwrap();
    assert_eq!(direct.direct_waves, 3);
    assert_eq!(parse(args(&[])).unwrap().unwrap().direct_waves, 2);
    assert!(parse(args(&["--direct-waves", "0"])).is_err());
    assert!(parse(args(&["--direct-waves", "99"])).is_err());
    let default = parse(args(&[])).unwrap().unwrap();
    assert_eq!(default.max_total, Duration::from_secs(1800));
}

/// Some bots straight to a synthetic server for five seconds: every bot logs in and joins,
/// the stream reaches them, and the measuring packets come back.
#[test]
fn the_tick_of_the_servers_is_read() {
    let read = |list: &[&str]| parse(args(list)).unwrap().unwrap().settings.write_per_tick;
    assert_eq!(read(&["--proxy", "x"]), None);
    assert_eq!(
        read(&["--proxy", "x", "--write-per-tick", "50"]),
        Some(Duration::from_millis(50))
    );
    assert_eq!(read(&["--proxy", "x", "--write-per-tick", "0"]), None);
}

/// Every bot writes the host given, or the address it connects to: a proxy's `[[forced-host]]`
/// can then lead the bench's bots to a server of their own behind a proxy other clients load.
#[test]
fn the_bots_write_the_host_they_are_given() {
    let read = |list: &[&str]| parse(args(list)).map(|o| o.unwrap().settings.handshake_host);
    assert_eq!(read(&["--proxy", "x"]), Ok(None));
    assert_eq!(
        read(&["--proxy", "x", "--handshake-host", "probe.l2"]),
        Ok(Some("probe.l2".to_owned()))
    );
    // A forgotten value takes the next option: refused, as a name that starts with a dot.
    for bad in [
        "",
        "probe l2",
        "probe/l2",
        "probé",
        "--players",
        ".probe",
        &"a".repeat(254),
    ] {
        assert!(
            read(&["--proxy", "x", "--handshake-host", bad]).is_err(),
            "{bad:?}"
        );
    }

    // The spectator's host, whatever its case and final dot, would send the bots to its real server.
    let spectator = |host: &str| {
        parse(args(&[
            "--proxy",
            "x",
            "--spectator",
            "127.0.0.1:25566",
            "--handshake-host",
            host,
        ]))
        .map(|_| ())
    };
    assert!(spectator("LOCALHOST.").is_err());
    assert!(spectator("probe.l2").is_ok());

    let address: std::net::SocketAddr = "127.0.0.1:25565".parse().unwrap();
    let mut s = settings(1);
    let hosts = |s: &Settings| {
        scenario::plans(s, address, 3, "P", bot::Role::Active, 0, None)
            .into_iter()
            .map(|p| p.host)
            .collect::<Vec<_>>()
    };
    assert_eq!(hosts(&s), ["127.0.0.1"; 3]);
    s.handshake_host = Some("probe.l2".to_owned());
    assert_eq!(hosts(&s), ["probe.l2"; 3]);
}

/// The moves are asked only when every bot has its world: one short, even one that never got
/// in, and the step says why instead of counting moves as failed.
#[test]
fn moves_wait_for_every_world() {
    let waited = Duration::from_secs(120);
    assert_eq!(scenario::not_ready(1000, 1000, 0, waited), None);
    let short = scenario::not_ready(1000, 736, 264, waited).unwrap();
    assert!(short.starts_with("not ready: 736 of 1000"), "{short}");
    assert!(short.contains("264 never got in"), "{short}");
    assert!(scenario::not_ready(1000, 999, 0, waited).is_some());
}

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

#[test]
fn a_proxy_of_another_machine_needs_its_address_and_its_file() {
    // An address is a proxy elsewhere; anything else is still the path of the program.
    assert!(
        parse(args(&["--proxy", "192.168.1.25:25565"]))
            .unwrap_err()
            .contains("--backend-bind")
    );
    let missing = parse(args(&[
        "--proxy",
        "192.168.1.25:25565",
        "--backend-bind",
        "192.168.1.18",
    ]))
    .unwrap_err();
    assert!(missing.contains("--remote-config"), "{missing}");
    assert!(parse(args(&["--backend-bind", "192.168.1.18"])).is_err());
    assert!(
        parse(args(&[
            "--proxy",
            "192.168.1.25:25565",
            "--backend-bind",
            "nope"
        ]))
        .is_err()
    );
    // Writing the file asks for no file; the run's compression decides the threshold in it.
    let o = parse(args(&[
        "--proxy",
        "192.168.1.25:25565",
        "--backend-bind",
        "192.168.1.18",
        "--backend-ports",
        "25701-25710",
        "--write-remote-config",
        "somewhere",
        "--compression",
        "both-off",
    ]))
    .unwrap()
    .unwrap();
    let remote = o.settings.remote.unwrap();
    assert_eq!(remote.proxy.to_string(), "192.168.1.25:25565");
    assert_eq!(remote.server_address(0).to_string(), "192.168.1.18:25701");
    assert_eq!(remote.server_address(1).to_string(), "192.168.1.18:25702");
    assert_eq!(remote.threshold, -1);
}

/// Ten ports in a row for the synthetic servers of a test: free now, **outside the range the system
/// draws the ports of outgoing connections from**, and **never given twice in this process**.
///
/// The bots' sockets and the proxy's take their ports from the system's range (32768 and up on Linux by
/// default, 49152 and up on Windows; the Linux value is read when it can be), one after the other, so a port found
/// there by binding to port 0 and giving it back can be taken by one of them before the server asks for it. The
/// tests of a process also run at the same time: every range handed out is kept in a list for the life of the
/// process, and a caller takes the next one that is not on it, so two tests never choose ranges that overlap,
/// whatever the order they ask in (the list is locked while a range is probed, so they ask one at a time).
fn free_ports() -> (u16, u16) {
    use std::sync::Mutex;
    use std::sync::atomic::AtomicU16;
    static NEXT: AtomicU16 = AtomicU16::new(0);
    static HANDED_OUT: Mutex<Vec<u16>> = Mutex::new(Vec::new());
    let slots = (outgoing_ports_start().saturating_sub(FREE_PORTS_FROM) / 10).min(900);
    assert!(
        slots >= 20,
        "the system draws the ports of outgoing connections from below {}",
        FREE_PORTS_FROM + 200
    );
    // Processes and calls start at different places.
    let start = (std::process::id() as u16)
        .wrapping_mul(7)
        .wrapping_add(NEXT.fetch_add(1, Ordering::Relaxed).wrapping_mul(13));
    let mut handed_out = HANDED_OUT.lock().unwrap_or_else(|e| e.into_inner());
    for step in 0..slots {
        let first = FREE_PORTS_FROM + (start.wrapping_add(step) % slots) * 10;
        if handed_out.contains(&first) {
            continue;
        }
        let held: Vec<_> = (first..first + 10)
            .map_while(|port| std::net::TcpListener::bind(("127.0.0.1", port)).ok())
            .collect();
        if held.len() == 10 {
            handed_out.push(first);
            return (first, first + 9);
        }
    }
    panic!(
        "no ten free ports in a row between {FREE_PORTS_FROM} and {}",
        FREE_PORTS_FROM + slots * 10
    );
}

/// Where [`free_ports`] begins to look: below every system's range for outgoing connections.
const FREE_PORTS_FROM: u16 = 20_000;

/// The first port of the range the system draws the ports of outgoing connections from.
fn outgoing_ports_start() -> u16 {
    std::fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range")
        .ok()
        .and_then(|text| text.split_whitespace().next()?.parse().ok())
        .unwrap_or(if cfg!(windows) { 49_152 } else { 32_768 })
}

#[test]
fn ports_for_the_servers_of_tests_running_together_never_overlap() {
    // Eight tests of the process asking at once, five times each.
    let threads: Vec<_> = (0..8)
        .map(|_| std::thread::spawn(|| (0..5).map(|_| free_ports()).collect::<Vec<(u16, u16)>>()))
        .collect();
    let mut all: Vec<(u16, u16)> = threads
        .into_iter()
        .flat_map(|t| t.join().unwrap())
        .collect();
    assert_eq!(all.len(), 40);
    all.sort_unstable();
    for pair in all.windows(2) {
        assert!(pair[0].1 < pair[1].0, "{pair:?} overlap");
    }
    let ceiling = outgoing_ports_start();
    for (first, last) in all {
        assert_eq!(last, first + 9);
        // Below the range of outgoing connections, and in our own, from the start of it.
        assert!((FREE_PORTS_FROM..ceiling).contains(&first), "{first}");
        assert!(last < ceiling, "{last}");
    }
}

/// What a run against a proxy of another machine does, with the proxy on this one: the servers
/// listen on the address and ports the configuration names, and the figures are the bots'.
#[test]
fn bots_play_through_a_proxy_reached_as_if_it_were_elsewhere() {
    let Some(program) = proxy_program() else {
        eprintln!("skipped: the meridian program is not built next to this test");
        return;
    };
    let _turn = ONE_PROXY.lock().unwrap_or_else(|e| e.into_inner());
    let secret = remote::fresh_secret().unwrap();
    let (first, last) = free_ports();
    let mut s = settings(4);
    s.forwarding = secret.clone();
    let mut remote = remote::Remote {
        proxy: "127.0.0.1:1".parse().unwrap(),
        bind: "127.0.0.1".parse().unwrap(),
        ports: (first, last),
        threshold: 256,
    };
    let mut proxy = crate::proxy::Proxy::start_with(&program, &s.dir, |listen| {
        remote.proxy = listen;
        remote::generate(&remote, 256, &secret)
    })
    .unwrap();
    s.remote = Some(remote);
    let get = |row: &Row, name: &str| {
        row.0
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    let idle = remote::idle(&s, 10).unwrap();
    assert_eq!(get(&idle, "logged_in"), "10", "{idle:?}");
    assert_eq!(get(&idle, "failed"), "0");
    assert_eq!(get(&idle, "proxy_answered"), "true");
    // Two waves of five, a second apart, in both phases.
    s.waves = 2;
    s.wave_gap = Duration::from_secs(1);
    let mut direct = s.clone();
    direct.window = Duration::from_secs(3);
    let active = remote::active(&s, &direct, 10, Compression::Same).unwrap();
    assert_eq!(get(&active, "logged_in"), "10", "{active:?}");
    for wave in ["w1", "w2"] {
        assert_eq!(get(&active, &format!("{wave}_bots")), "5", "{active:?}");
        assert_eq!(
            get(&active, &format!("{wave}_logged_in")),
            "5",
            "{active:?}"
        );
        assert_ne!(
            get(&active, &format!("{wave}_join_start_ms")),
            "",
            "{active:?}"
        );
    }
    // The first wave's bots played while the second came in: the figures are there.
    assert!(
        !get(&active, "w2_rest_rtt_p99_during_us").is_empty(),
        "{active:?}"
    );
    assert_eq!(get(&active, "proxy_answered"), "true");
    assert_ne!(get(&active, "proxy_rtt_p50_us"), "0");
    // The direct phase is told apart, and its figures are there and valid.
    assert_eq!(get(&active, "direct_logged"), "10", "{active:?}");
    assert_eq!(get(&active, "direct_failed"), "0", "{active:?}");
    assert_eq!(get(&active, "added_valid"), "true", "{active:?}");
    assert!(!get(&active, "added_p50_us").is_empty(), "{active:?}");
    assert!(
        !get(&active, "direct_login_p99_ms").is_empty(),
        "{active:?}"
    );
    assert_ne!(get(&active, "raw_connect_p50_us"), "0");
    // Another threshold than the file's is refused rather than measured wrongly.
    assert!(remote::active(&s, &direct, 10, Compression::BothOff).is_err());
    let windows = s.windows.table().csv();
    for phase in ["join", "hold", "measure", "join wave 1", "join wave 2"] {
        assert!(windows.contains(phase), "{windows}");
    }
    // The direct phase arrives in waves as well.
    assert!(windows.contains("R2 direct,10,join wave 2"), "{windows}");
    assert!(proxy.alive());
    proxy.stop().unwrap();
}

#[test]
fn a_run_against_a_proxy_of_another_machine_takes_its_secret_from_the_file() {
    let secret = remote::fresh_secret().unwrap();
    let remote = remote::Remote {
        proxy: "192.168.1.25:25565".parse().unwrap(),
        bind: "192.168.1.18".parse().unwrap(),
        ports: (25701, 25710),
        threshold: 256,
    };
    let dir = std::env::temp_dir().join(format!("meridian-remote-parse-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("meridian.toml");
    std::fs::write(&file, remote::generate(&remote, 256, &secret)).unwrap();
    let file_arg = file.to_str().unwrap();
    let base = [
        "--proxy",
        "192.168.1.25:25565",
        "--backend-bind",
        "192.168.1.18",
        "--remote-config",
        file_arg,
    ];
    let o = parse(args(&base)).unwrap().unwrap();
    // The secret is the file's, the scenarios are the two that can run, the compression the file's.
    assert_eq!(o.settings.forwarding, secret);
    assert_eq!(o.scenarios, ["idle", "active"]);
    assert_eq!(o.compressions, [Compression::Same]);
    assert_eq!(o.settings.remote.unwrap().threshold, 256);
    let mut with = base.to_vec();
    with.extend(["--scenarios", "moves"]);
    assert!(
        parse(args(&with))
            .unwrap_err()
            .contains("only idle and active")
    );
    // The ports of the run are those of the file.
    let mut other = base.to_vec();
    other.extend(["--backend-ports", "25801-25810"]);
    assert!(parse(args(&other)).unwrap_err().contains("25701"));
    let _ = std::fs::remove_dir_all(&dir);
}
