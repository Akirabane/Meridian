use super::*;
use crate::session::Player;
use crate::session::tests::idle_session_on;
use horizon_protocol::crypto::offline_uuid;
use meridian_config::{BalancedServer, Config, Role};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::AtomicBool;

fn config() -> Config {
    Config::from_toml_str(
        r#"
forwarding-secret = "test-secret"
default-backend = "lobby"
[[backends]]
name = "lobby"
address = "127.0.0.1:25601"
role = "lobby"
soft-cap = 2
hard-cap = 3
[[backends]]
name = "east"
address = "127.0.0.1:25602"
[balancing]
enabled = true
"#,
    )
    .unwrap()
}

fn proxy() -> Arc<Proxy> {
    Arc::new(Proxy::new(
        config(),
        "meridian.toml".into(),
        Arc::new(AtomicBool::new(false)),
    ))
}

/// The value of the line that starts with `series`.
fn value(page: &str, series: &str) -> String {
    page.lines()
        .find_map(|l| l.strip_prefix(series).and_then(|r| r.strip_prefix(' ')))
        .unwrap_or_else(|| panic!("no line {series} in\n{page}"))
        .to_owned()
}

#[test]
fn a_fresh_proxy_shows_zeroes_and_the_backends() {
    let page = render(&proxy());
    assert_eq!(value(&page, "meridian_players"), "0");
    assert_eq!(
        value(&page, "meridian_backend_players{backend=\"lobby\"}"),
        "0"
    );
    assert_eq!(
        value(&page, "meridian_backend_players{backend=\"east\"}"),
        "0"
    );
    // Only a backend with a role has caps and a state.
    assert_eq!(
        value(&page, "meridian_backend_soft_cap{backend=\"lobby\"}"),
        "2"
    );
    assert_eq!(
        value(&page, "meridian_backend_hard_cap{backend=\"lobby\"}"),
        "3"
    );
    assert!(!page.contains("meridian_backend_soft_cap{backend=\"east\"}"));
    assert_eq!(value(&page, "meridian_connections_total"), "0");
    assert_eq!(value(&page, "meridian_transfers_total"), "0");
    assert_eq!(value(&page, "meridian_transfer_alerts_total"), "0");
    assert_eq!(value(&page, "meridian_evacuations_total"), "0");
    assert!(page.contains("# TYPE meridian_transfer_duration_seconds histogram"));
}

#[test]
fn counters_follow_what_happens() {
    let proxy = proxy();
    let m = proxy.sessions.metrics();
    m.connection();
    m.connection();
    m.throttled();
    m.login();
    m.login_failed(LoginFailure::Full);
    m.login_failed(LoginFailure::Full);
    m.login_failed(LoginFailure::Protocol);
    m.transfer_failed("freeze");
    m.transfer_failed("something else");
    m.alert();
    m.evacuation(3, 1);
    m.restart_detected(Cause::Unreachable);
    m.status_round(Duration::from_millis(250));
    let page = render(&proxy);
    assert_eq!(value(&page, "meridian_connections_total"), "2");
    assert_eq!(value(&page, "meridian_connections_throttled_total"), "1");
    assert_eq!(value(&page, "meridian_logins_total"), "1");
    assert_eq!(
        value(&page, "meridian_login_failures_total{reason=\"full\"}"),
        "2"
    );
    assert_eq!(
        value(&page, "meridian_login_failures_total{reason=\"protocol\"}"),
        "1"
    );
    assert_eq!(
        value(&page, "meridian_transfer_failures_total{step=\"freeze\"}"),
        "1"
    );
    // A step nobody knows counts as the first.
    assert_eq!(
        value(&page, "meridian_transfer_failures_total{step=\"check\"}"),
        "1"
    );
    assert_eq!(value(&page, "meridian_transfer_alerts_total"), "1");
    assert_eq!(value(&page, "meridian_evacuations_total"), "1");
    assert_eq!(
        value(&page, "meridian_evacuated_players_total{result=\"moved\"}"),
        "3"
    );
    assert_eq!(
        value(&page, "meridian_evacuated_players_total{result=\"stayed\"}"),
        "1"
    );
    assert_eq!(
        value(
            &page,
            "meridian_restarts_detected_total{cause=\"unreachable\"}"
        ),
        "1"
    );
    assert_eq!(value(&page, "meridian_status_rounds_total"), "1");
    assert_eq!(
        value(&page, "meridian_status_round_duration_seconds"),
        "0.25"
    );
}

/// A bucket counts everything at or under its bound, so the buckets only grow, and the
/// last one is the count.
#[test]
fn a_histogram_is_cumulative() {
    let h = Histogram::default();
    for ms in [10, 60, 60, 300, 20_000] {
        h.observe(Duration::from_millis(ms));
    }
    let mut out = String::new();
    h.render(&mut out, "t", "phase=\"x\"");
    let line = |le: &str| {
        out.lines()
            .find(|l| l.contains(&format!("le=\"{le}\"")))
            .and_then(|l| l.rsplit(' ').next())
            .unwrap()
            .to_owned()
    };
    assert_eq!(line("0.05"), "1");
    assert_eq!(line("0.1"), "3");
    assert_eq!(line("0.25"), "3");
    assert_eq!(line("0.5"), "4");
    assert_eq!(line("10"), "4");
    assert_eq!(line("+Inf"), "5");
    assert!(out.contains("t_count{phase=\"x\"} 5"), "{out}");
    assert!(out.contains("t_sum{phase=\"x\"} 20.43"), "{out}");
}

#[test]
fn the_phases_of_a_transfer_are_observed() {
    let proxy = proxy();
    let done = Done {
        delta_packets: 0,
        delta_bytes: 0,
        extras: 0,
        chunks: 0,
        entities: 0,
        times: [
            Duration::from_millis(40),
            Duration::from_millis(80),
            Duration::from_millis(900),
            Duration::from_millis(10),
        ],
        handover: String::new(),
        source_kept: None,
    };
    proxy
        .sessions
        .metrics()
        .transfer_done(&done, Duration::from_millis(1100));
    let page = render(&proxy);
    assert_eq!(value(&page, "meridian_transfers_total"), "1");
    for phase in PHASES {
        assert_eq!(
            value(
                &page,
                &format!("meridian_transfer_duration_seconds_count{{phase=\"{phase}\"}}")
            ),
            "1"
        );
    }
    assert_eq!(
        value(
            &page,
            "meridian_transfer_duration_seconds_bucket{phase=\"freeze\",le=\"0.1\"}"
        ),
        "1"
    );
    assert_eq!(
        value(
            &page,
            "meridian_transfer_duration_seconds_bucket{phase=\"total\",le=\"1\"}"
        ),
        "0"
    );
    assert_eq!(
        value(
            &page,
            "meridian_transfer_duration_seconds_bucket{phase=\"total\",le=\"2.5\"}"
        ),
        "1"
    );
}

#[test]
fn a_backend_is_shown_open_closed_or_draining() {
    let proxy = proxy();
    let state = |page: &str, s: &str| {
        value(
            page,
            &format!("meridian_backend_state{{backend=\"lobby\",state=\"{s}\"}}"),
        )
    };
    // No report from its sidecar yet is not a stale one: the balancer offers it.
    let page = render(&proxy);
    assert_eq!(state(&page, "open"), "1");
    assert_eq!(state(&page, "draining"), "0");
    proxy.sessions.balancer.drain("lobby");
    let page = render(&proxy);
    assert_eq!(state(&page, "open"), "0");
    assert_eq!(state(&page, "draining"), "1");
    proxy.sessions.balancer.undrain("lobby");
    // Two players on a soft cap of two: closed.
    let _a = join(&proxy, "Alex");
    let _b = join(&proxy, "Steve");
    let page = render(&proxy);
    assert_eq!(value(&page, "meridian_players"), "2");
    assert_eq!(
        value(&page, "meridian_backend_players{backend=\"lobby\"}"),
        "2"
    );
    assert_eq!(state(&page, "closed"), "1");
}

/// A player relayed between two idle pipes, counted in the proxy's sessions.
struct Joined<'a> {
    _open: crate::session::Open<'a>,
    _relay: std::thread::JoinHandle<meridian_net::Result<meridian_net::relay::Report>>,
    _ends: (
        horizon_protocol::connection::Connection,
        horizon_protocol::connection::Connection,
    ),
}

fn join<'a>(proxy: &'a Proxy, name: &str) -> Joined<'a> {
    let (relay, handle, player, server) = idle_session_on(name, "lobby");
    let open = proxy
        .sessions
        .open(
            Player {
                name: name.into(),
                uuid: offline_uuid(name).to_be_bytes(),
                backend: "lobby".into(),
                address: "198.51.100.7:50123".parse().unwrap(),
                joined: std::time::Instant::now(),
            },
            handle,
        )
        .unwrap();
    Joined {
        _open: open,
        _relay: std::thread::spawn(move || relay.run()),
        _ends: (player, server),
    }
}

/// Nothing that names a player is on the page: not the name, the address nor the id.
#[test]
fn the_page_names_no_player() {
    let proxy = proxy();
    let _alex = join(&proxy, "Alexandre");
    let page = render(&proxy);
    assert_eq!(value(&page, "meridian_players"), "1");
    assert!(!page.contains("Alexandre"), "{page}");
    assert!(!page.contains("198.51.100.7"), "{page}");
    assert!(
        !page.contains(&format!("{:032x}", offline_uuid("Alexandre"))),
        "{page}"
    );
    // The labels are fixed words and backend names.
    for line in page.lines().filter(|l| l.contains('{')) {
        let labels = &line[line.find('{').unwrap()..line.find('}').unwrap()];
        for key in labels.trim_matches('{').split(',') {
            let key = key.split('=').next().unwrap();
            assert!(
                [
                    "backend", "state", "reason", "step", "phase", "le", "result", "cause",
                    "version"
                ]
                .contains(&key),
                "{line}"
            );
        }
    }
}

#[test]
fn a_label_value_is_escaped() {
    assert_eq!(escaped("a\"b\\c\nd"), "a\\\"b\\\\c\\nd");
}

#[test]
fn the_health_check_answers_200_then_503_once_the_proxy_stops() {
    let proxy = proxy();
    assert_eq!(
        respond(&proxy, "GET", "/healthz"),
        Answer {
            status: 200,
            content_type: TEXT,
            body: "ok\n".into()
        }
    );
    proxy.stop();
    let stopped = respond(&proxy, "GET", "/healthz");
    assert_eq!(stopped.status, 503);
    assert_eq!(stopped.body, "stopping\n");
    // The counters still answer while it stops.
    assert_eq!(respond(&proxy, "GET", "/metrics").status, 200);
}

#[test]
fn other_paths_and_methods_are_refused() {
    let proxy = proxy();
    assert_eq!(respond(&proxy, "GET", "/").status, 404);
    assert_eq!(respond(&proxy, "GET", "/metrics/extra").status, 404);
    assert_eq!(respond(&proxy, "POST", "/metrics").status, 405);
    assert_eq!(respond(&proxy, "DELETE", "/healthz").status, 405);
    // A query string does not change the page.
    assert_eq!(respond(&proxy, "GET", "/healthz?probe=1").status, 200);
    assert_eq!(respond(&proxy, "HEAD", "/healthz").status, 200);
}

/// One request over a real connection, and the whole answer.
fn request(address: std::net::SocketAddr, raw: &str) -> String {
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream.write_all(raw.as_bytes()).unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    answer
}

#[test]
fn the_server_answers_over_http_and_ends_with_the_proxy() {
    let proxy = proxy();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let thread = serve_in_thread(listener, Arc::clone(&proxy)).unwrap();

    let health = request(address, "GET /healthz HTTP/1.1\r\nHost: x\r\n\r\n");
    assert!(health.starts_with("HTTP/1.1 200 OK\r\n"), "{health}");
    assert!(health.ends_with("\r\n\r\nok\n"), "{health}");

    let page = request(address, "GET /metrics HTTP/1.1\r\nHost: x\r\n\r\n");
    assert!(page.starts_with("HTTP/1.1 200 OK\r\n"), "{page}");
    assert!(
        page.contains("Content-Type: text/plain; version=0.0.4; charset=utf-8\r\n"),
        "{page}"
    );
    assert!(page.contains("\nmeridian_players 0\n"), "{page}");

    let head = request(address, "HEAD /metrics HTTP/1.1\r\nHost: x\r\n\r\n");
    assert!(head.starts_with("HTTP/1.1 200 OK\r\n"), "{head}");
    assert!(head.ends_with("\r\n\r\n"), "no body for HEAD: {head}");

    let post = request(address, "POST /metrics HTTP/1.1\r\nHost: x\r\n\r\n");
    assert!(post.starts_with("HTTP/1.1 405 "), "{post}");
    assert!(post.contains("Allow: GET, HEAD\r\n"), "{post}");

    let missing = request(address, "GET /nothing HTTP/1.1\r\nHost: x\r\n\r\n");
    assert!(missing.starts_with("HTTP/1.1 404 "), "{missing}");

    let garbage = request(address, "\r\n\r\n");
    assert!(garbage.starts_with("HTTP/1.1 400 "), "{garbage}");

    proxy.stop();
    thread.join().unwrap();
}

/// Off by default: nothing listens and nothing is spawned.
#[test]
fn nothing_is_served_unless_the_setting_is_on() {
    let proxy = proxy();
    assert!(!proxy.config().metrics.enabled);
    assert!(spawn(&proxy).is_none());
}

#[test]
fn a_proxy_with_the_setting_on_listens_where_it_was_told() {
    let probe = TcpListener::bind("127.0.0.1:0").unwrap();
    let bind = probe.local_addr().unwrap();
    drop(probe);
    let proxy = proxy();
    proxy.set_config(Config {
        metrics: meridian_config::Metrics {
            enabled: true,
            bind,
        },
        ..config()
    });
    let thread = spawn(&proxy).expect("on").unwrap();
    let health = request(bind, "GET /healthz HTTP/1.1\r\n\r\n");
    assert!(health.starts_with("HTTP/1.1 200 OK"), "{health}");
    proxy.stop();
    thread.join().unwrap();
}

#[test]
fn balanced_servers_in_the_config_are_the_ones_with_caps() {
    // The page and the console read the same thing: a backend with a role.
    let c = Config {
        balanced: vec![BalancedServer {
            backend: "lobby".into(),
            role: Role::Lobby,
            soft_cap: 2,
            hard_cap: 3,
        }],
        ..config()
    };
    assert!(c.balanced("lobby").is_some());
    assert!(c.balanced("east").is_none());
}

#[test]
fn a_backend_found_to_be_going_down_is_counted_and_shown_draining() {
    let proxy = proxy();
    proxy
        .restarts
        .announce("lobby", Cause::Announced, std::time::Instant::now());
    let page = render(&proxy);
    assert_eq!(
        value(
            &page,
            "meridian_restarts_detected_total{cause=\"announced\"}"
        ),
        "1"
    );
    assert_eq!(
        value(
            &page,
            "meridian_backend_state{backend=\"lobby\",state=\"draining\"}"
        ),
        "1"
    );
    // A second notice for the same restart is not a second restart.
    proxy
        .restarts
        .announce("lobby", Cause::Panel, std::time::Instant::now());
    let page = render(&proxy);
    assert_eq!(
        value(&page, "meridian_restarts_detected_total{cause=\"panel\"}"),
        "0"
    );
}

/// `drain --move` is an evacuation: here the player has a server to go to but no sidecar to
/// do it through, so it stays, and the page counts both.
#[test]
fn an_evacuation_counts_the_players_it_moved_and_the_ones_who_stayed() {
    let proxy = proxy();
    proxy.set_config(Config {
        balanced: ["lobby", "east"]
            .iter()
            .map(|b| BalancedServer {
                backend: (*b).into(),
                role: Role::Lobby,
                soft_cap: 2,
                hard_cap: 3,
            })
            .collect(),
        ..config()
    });
    let _alex = join(&proxy, "Alex");
    // A drain that moves nobody is not an evacuation.
    let said = crate::console::Commands::builtin().execute(&proxy, "drain east --move");
    assert_eq!(said[1], "no player is on east", "{said:?}");
    assert_eq!(value(&render(&proxy), "meridian_evacuations_total"), "0");
    let said = crate::console::Commands::builtin().execute(&proxy, "drain lobby --move");
    assert!(
        said.iter().any(|l| l.contains("0 moved, 1 stayed")),
        "{said:?}"
    );
    let page = render(&proxy);
    assert_eq!(value(&page, "meridian_evacuations_total"), "1");
    assert_eq!(
        value(&page, "meridian_evacuated_players_total{result=\"stayed\"}"),
        "1"
    );
    assert_eq!(
        value(&page, "meridian_evacuated_players_total{result=\"moved\"}"),
        "0"
    );
}

/// Clients that send their request a byte at a time, never quite silent, hold the
/// page's places only until the request's deadline: then a monitor is served again.
#[test]
fn slow_requests_give_their_places_back_at_their_deadline() {
    let proxy = proxy();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let thread = serve_in_thread(listener, Arc::clone(&proxy)).unwrap();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let slow: Vec<_> = (0..MAX_CONNECTIONS)
        .map(|_| {
            let mut stream = TcpStream::connect(address).unwrap();
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed)
                    && stream.write_all(b"G").is_ok()
                {
                    std::thread::sleep(Duration::from_millis(300));
                }
            })
        })
        .collect();
    std::thread::sleep(REQUEST_DEADLINE + Duration::from_millis(1500));
    let health = request(address, "GET /healthz HTTP/1.1\r\nHost: x\r\n\r\n");
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(health.starts_with("HTTP/1.1 200 OK\r\n"), "{health:?}");
    for t in slow {
        t.join().unwrap();
    }
    proxy.stop();
    thread.join().unwrap();
}

/// A request line far longer than any monitor's is read up to the limit and answered
/// from what was read, here a path the page does not serve: the rest is never read
/// nor kept.
#[test]
fn a_giant_request_line_is_answered_from_its_first_bytes() {
    let proxy = proxy();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let thread = serve_in_thread(listener, Arc::clone(&proxy)).unwrap();
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    // 64 KiB of a path and no end of line: the proxy answers after MAX_REQUEST.
    let mut line = b"GET /".to_vec();
    line.extend(std::iter::repeat_n(b'a', 64 << 10));
    let _ = stream.write_all(&line);
    let mut answer = Vec::new();
    let _ = stream.read_to_end(&mut answer);
    let answer = String::from_utf8_lossy(&answer);
    assert!(answer.starts_with("HTTP/1.1 404 "), "{answer}");
    proxy.stop();
    thread.join().unwrap();
}

/// A player whose client stopped reading while their server still sends, as a client
/// does on a saturated link while it receives its world: the relay is stuck writing to it.
struct Stalled<'a> {
    _open: crate::session::Open<'a>,
    /// The client's socket, never read; dropping it ends the relay.
    _client: TcpStream,
    _server: horizon_protocol::connection::Connection,
}

fn stalled<'a>(proxy: &'a Proxy, name: &str) -> Stalled<'a> {
    use horizon_protocol::connection::Connection;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (accepted, _) = listener.accept().unwrap();
    let (proxy_backend, mut server) = meridian_testkit::pipe();
    let session = meridian_net::client::ClientSession {
        profile: meridian_net::Profile {
            name: name.to_owned(),
            uuid: offline_uuid(name).to_be_bytes(),
            properties: Vec::new(),
        },
        auth: meridian_net::client::AuthKind::Offline,
        virtual_host: None,
        connection: Connection::from_stream(accepted).unwrap(),
    };
    let link = meridian_net::backend::BackendLink {
        connection: proxy_backend,
    };
    let (relay, handle) = meridian_handoff::Session::new(session, link, "lobby").unwrap();
    std::thread::spawn(move || relay.run());
    let open = proxy
        .sessions
        .open(
            Player {
                name: name.into(),
                uuid: offline_uuid(name).to_be_bytes(),
                backend: "lobby".into(),
                address: "198.51.100.7:50124".parse().unwrap(),
                joined: std::time::Instant::now(),
            },
            handle,
        )
        .unwrap();
    // The server sends its world, 64 MiB of it in packets of 1 MiB, far more than the
    // client's socket holds: the relay is soon stuck writing the next packet.
    let chunk = vec![7u8; 1 << 20];
    for _ in 0..64 {
        server
            .send(&meridian_testkit::body(
                horizon_protocol::ids::configuration::clientbound::CUSTOM_PAYLOAD,
                &chunk,
            ))
            .unwrap();
    }
    std::thread::sleep(Duration::from_millis(500));
    Stalled {
        _open: open,
        _client: client,
        _server: server,
    }
}

/// The page is answered at once while a player's client does not read: what it counts
/// never waits for a relay that is writing to a client (a burst of arrivals on a busy
/// link), and neither does a login, which counts the same way.
#[test]
fn the_page_does_not_wait_for_a_client_that_does_not_read() {
    let proxy = proxy();
    let _alex = join(&proxy, "Alex");
    let _stuck = stalled(&proxy, "Steve");
    let (done, answered) = std::sync::mpsc::channel();
    let started = std::time::Instant::now();
    {
        let proxy = Arc::clone(&proxy);
        std::thread::spawn(move || {
            let answer = respond(&proxy, "GET", "/metrics");
            let counts = proxy.sessions.counts();
            let _ = done.send((answer, counts));
        });
    }
    let Ok((answer, counts)) = answered.recv_timeout(Duration::from_secs(3)) else {
        // The sessions cannot be closed while the page holds them: they are left as they
        // are, so that the failure is told rather than waited on.
        let waited = started.elapsed();
        std::mem::forget((_alex, _stuck));
        panic!("the page was not answered within {waited:?}");
    };
    let took = started.elapsed();
    eprintln!("the page was answered in {took:?}");
    assert!(took < Duration::from_millis(500), "{took:?}");
    assert_eq!(answer.status, 200);
    assert_eq!(value(&answer.body, "meridian_players"), "2");
    assert_eq!(
        value(&answer.body, "meridian_backend_players{backend=\"lobby\"}"),
        "2"
    );
    assert_eq!(counts.get("lobby"), Some(&2));
}
