//! The panel with stand-in processes: the panel's own binary run as
//! `__fake-process`, which prints, answers its console and opens a port. No real
//! server is started, and nothing leaves the test's own folders.
use meridian_panel::bench::Bench;
use meridian_panel::http::{self, Panel, Request};
use meridian_panel::supervisor::{State, Supervisor};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const FAKE: &str = env!("CARGO_BIN_EXE_meridian-panel");

/// A folder of the test's own, emptied first.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("meridian-panel-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A port nobody listens on right now.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A bench of stand-ins in `dir`: `servers` servers and a proxy, each in its own
/// folder, with a short stop timeout.
fn bench(dir: &Path, servers: &[&str], extra_args: &[&str]) -> (Bench, Vec<u16>) {
    let mut text = format!("panel-port = {}\n", free_port());
    let mut ports = Vec::new();
    // A name ending in "-sidecar" is a sidecar; the others are servers.
    let kind_of = |name: &str| {
        if name.ends_with("-sidecar") {
            "sidecar"
        } else {
            "server"
        }
    };
    for (name, kind) in servers
        .iter()
        .map(|s| (*s, kind_of(s)))
        .chain(std::iter::once(("proxy", "proxy")))
    {
        let port = free_port();
        ports.push(port);
        std::fs::create_dir_all(dir.join(name)).unwrap();
        let mut args = vec![
            "__fake-process".to_owned(),
            "--port".to_owned(),
            port.to_string(),
        ];
        args.extend(extra_args.iter().map(|a| (*a).to_owned()));
        let args: Vec<String> = args.iter().map(|a| format!("\"{a}\"")).collect();
        text.push_str(&format!(
            "\n[[process]]\nname = \"{name}\"\nkind = \"{kind}\"\nprogram = \"{}\"\nargs = [{}]\n\
             dir = \"{name}\"\nport = {port}\nstop-timeout-seconds = 2\n",
            FAKE.replace('\\', "/"),
            args.join(", ")
        ));
    }
    let path = dir.join("bench.toml");
    std::fs::write(&path, text).unwrap();
    (Bench::load(&path).unwrap(), ports)
}

/// Writes an accepted EULA, in a test folder only.
fn accept_in(dir: &Path) {
    std::fs::write(dir.join("eula.txt"), "eula=true\n").unwrap();
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(20);
    while Instant::now() < until {
        if done() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("timed out waiting for {what}");
}

/// Waits until the stand-in `name` is online and both its startup lines are in
/// its log, so that nothing it prints at startup can arrive during a question.
fn ready(supervisor: &Supervisor, name: &str) {
    wait_for("online", || state_of(supervisor, name) == State::Online);
    wait_for("the startup lines", || {
        let log = log_text(supervisor, name);
        log.contains("fake process ready") && log.contains("[stderr] fake process warning")
    });
}

fn state_of(supervisor: &Supervisor, name: &str) -> State {
    supervisor
        .snapshots()
        .into_iter()
        .find(|s| s.name == name)
        .unwrap()
        .state
}

fn log_text(supervisor: &Supervisor, name: &str) -> String {
    let (lines, _) = supervisor.log(name, 0).unwrap();
    lines
        .iter()
        .map(|l| l.text.clone())
        .collect::<Vec<_>>()
        .join("\n")
}

/// An answer that comes late, like the proxy's `servers`, is given back whole to
/// its own command, and the next command gets only its own lines.
#[test]
fn a_slow_answer_stays_with_its_command() {
    let dir = scratch("slow");
    let (bench, _) = bench(&dir, &[], &[]);
    let supervisor = Supervisor::new(&bench);
    supervisor.start("proxy").unwrap();
    ready(&supervisor, "proxy");
    let slow = supervisor
        .ask("proxy", "slow", Duration::from_secs(8))
        .unwrap();
    assert_eq!(slow, ["slow: first part", "slow: second part"]);
    let next = supervisor
        .ask("proxy", "hello", Duration::from_secs(8))
        .unwrap();
    assert_eq!(next, ["got: hello"]);
    // A command that prints nothing returns after a short wait, not the longest.
    let started = Instant::now();
    supervisor.send("proxy", "crash").ok();
    assert!(started.elapsed() < Duration::from_secs(1));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_process_starts_answers_its_console_and_stops_with_its_command() {
    let dir = scratch("one");
    let (bench, _) = bench(&dir, &["lobby"], &[]);
    accept_in(&dir.join("lobby"));
    let supervisor = Supervisor::new(&bench);
    assert_eq!(state_of(&supervisor, "lobby"), State::Stopped);

    supervisor.start("lobby").unwrap();
    wait_for("online", || state_of(&supervisor, "lobby") == State::Online);
    let snapshot = supervisor
        .snapshots()
        .into_iter()
        .find(|s| s.name == "lobby")
        .unwrap();
    assert!(snapshot.pid.is_some());
    assert!(
        supervisor
            .start("lobby")
            .unwrap_err()
            .contains("already online")
    );

    // Both outputs are kept, stderr marked.
    ready(&supervisor, "lobby");

    // The console: a line in, the answer out. The limit is the panel's own; the
    // answer is back once the output has been quiet for a moment.
    let answer = supervisor
        .ask("lobby", "say hello", Duration::from_secs(8))
        .unwrap();
    assert_eq!(answer, ["got: say hello"]);
    assert!(supervisor.send("lobby", "two\nlines").is_err());

    // A server stops with "stop", and ends as stopped, not exited.
    supervisor.stop("lobby").unwrap();
    wait_for("stopped", || {
        state_of(&supervisor, "lobby") == State::Stopped
    });
    assert!(log_text(&supervisor, "lobby").contains("stopping with \"stop\""));
    assert!(log_text(&supervisor, "lobby").contains("stopping"));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_server_without_its_eula_is_not_started_and_the_message_says_how() {
    let dir = scratch("eula");
    let (bench, _) = bench(&dir, &["lobby"], &[]);
    let supervisor = Supervisor::new(&bench);
    let e = supervisor.start("lobby").unwrap_err();
    assert!(e.contains("https://aka.ms/MinecraftEULA"), "{e}");
    assert!(
        e.contains(&format!(
            "meridian-panel accept-eula --dir \"{}\" --i-accept-the-minecraft-eula",
            dir.display()
        )),
        "{e}"
    );
    assert_eq!(state_of(&supervisor, "lobby"), State::Stopped);
    // The proxy needs none.
    supervisor.start("proxy").unwrap();
    supervisor.stop("proxy").unwrap();
    wait_for("proxy stopped", || {
        state_of(&supervisor, "proxy") == State::Stopped
    });
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_process_that_ignores_its_stop_command_is_killed_after_the_timeout() {
    let dir = scratch("kill");
    let (bench, _) = bench(&dir, &[], &["--ignore-stop"]);
    let supervisor = Supervisor::new(&bench);
    supervisor.start("proxy").unwrap();
    wait_for("online", || state_of(&supervisor, "proxy") == State::Online);
    supervisor.stop("proxy").unwrap();
    assert_eq!(state_of(&supervisor, "proxy"), State::Stopping);
    wait_for("killed", || {
        state_of(&supervisor, "proxy") == State::Stopped
    });
    assert!(log_text(&supervisor, "proxy").contains("killed"));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_process_that_ends_by_itself_is_shown_with_its_code() {
    let dir = scratch("crash");
    let (bench, _) = bench(&dir, &[], &[]);
    let supervisor = Supervisor::new(&bench);
    supervisor.start("proxy").unwrap();
    supervisor.send("proxy", "crash").unwrap();
    wait_for("exited", || {
        state_of(&supervisor, "proxy") == State::Exited(Some(3))
    });
    // It can be started again.
    supervisor.start("proxy").unwrap();
    supervisor.stop("proxy").unwrap();
    wait_for("stopped", || {
        state_of(&supervisor, "proxy") == State::Stopped
    });
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_whole_bench_starts_servers_sidecars_proxy_and_stops_in_reverse() {
    let dir = scratch("all");
    let servers = ["lobby", "game1", "game2"];
    let sidecars = ["lobby-sidecar", "game1-sidecar", "game2-sidecar"];
    let all: Vec<&str> = servers.iter().chain(sidecars.iter()).copied().collect();
    let (bench, _) = bench(&dir, &all, &[]);
    for server in servers {
        accept_in(&dir.join(server));
    }
    let supervisor = Supervisor::new(&bench);
    supervisor.start_all();
    wait_for("all online", || {
        supervisor
            .snapshots()
            .iter()
            .all(|s| s.state == State::Online)
    });
    // Each stage started once the one before answered: the longer a process has
    // run, the earlier it started.
    let uptime = |name: &str| {
        supervisor
            .snapshots()
            .into_iter()
            .find(|s| s.name == name)
            .unwrap()
            .uptime
            .unwrap()
    };
    for server in servers {
        for sidecar in sidecars {
            assert!(
                uptime(server) >= uptime(sidecar),
                "{sidecar} started before {server}"
            );
        }
    }
    for sidecar in sidecars {
        assert!(
            uptime(sidecar) >= uptime("proxy"),
            "the proxy started before {sidecar}"
        );
    }
    // Measures arrive for running processes.
    wait_for("a measure", || {
        supervisor
            .snapshots()
            .iter()
            .all(|s| s.history.last().is_some_and(|h| h.memory_mb > 0.0))
    });

    // The reverse order: a server is only asked to stop once the proxy and the
    // sidecars have ended. A sidecar has no console and is ended at once.
    supervisor.stop_all();
    wait_for("all stopped", || {
        if servers
            .iter()
            .any(|s| state_of(&supervisor, s) == State::Stopping)
        {
            assert_eq!(state_of(&supervisor, "proxy"), State::Stopped);
            for sidecar in sidecars {
                assert_eq!(state_of(&supervisor, sidecar), State::Stopped, "{sidecar}");
            }
        }
        supervisor
            .snapshots()
            .iter()
            .all(|s| s.state == State::Stopped)
    });
    assert!(log_text(&supervisor, "lobby-sidecar").contains("it has no console"));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn restart_stops_then_starts_again() {
    let dir = scratch("restart");
    let (bench, _) = bench(&dir, &[], &[]);
    let supervisor = Supervisor::new(&bench);
    supervisor.start("proxy").unwrap();
    wait_for("online", || state_of(&supervisor, "proxy") == State::Online);
    let first = supervisor.snapshots()[0].pid.unwrap();
    supervisor.restart("proxy").unwrap();
    wait_for("a new run", || {
        let s = &supervisor.snapshots()[0];
        s.state == State::Online && s.pid.is_some_and(|pid| pid != first)
    });
    supervisor.stop("proxy").unwrap();
    wait_for("stopped", || {
        state_of(&supervisor, "proxy") == State::Stopped
    });
    std::fs::remove_dir_all(&dir).unwrap();
}

// The page and its API.

fn panel(bench: &Bench) -> Panel {
    Panel {
        supervisor: Supervisor::new(bench),
        token: "test-token".into(),
        port: bench.panel_port,
    }
}

fn request(
    panel: &Panel,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: &str,
) -> (u16, serde_json::Value, String) {
    let (path, query) = path.split_once('?').unwrap_or((path, ""));
    let response = http::route(
        panel,
        &Request {
            method: method.into(),
            path: path.into(),
            query: query.into(),
            host: Some(format!("127.0.0.1:{}", panel.port)),
            token: token.map(str::to_owned),
            body: body.as_bytes().to_vec(),
        },
    );
    let text = String::from_utf8(response.body).unwrap();
    let json = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
    (response.status, json, text)
}

#[test]
fn every_api_call_needs_the_token_and_the_host_of_the_panel() {
    let dir = scratch("token");
    let (bench, _) = bench(&dir, &["lobby"], &[]);
    let panel = panel(&bench);
    for (method, path) in [
        ("GET", "/api/state"),
        ("GET", "/api/log?name=lobby"),
        ("POST", "/api/action"),
        ("POST", "/api/command"),
        ("POST", "/api/test/players"),
        ("POST", "/api/test/send"),
    ] {
        assert_eq!(request(&panel, method, path, None, "{}").0, 401, "{path}");
        assert_eq!(
            request(&panel, method, path, Some("wrong"), "{}").0,
            401,
            "{path}"
        );
    }
    assert_eq!(
        request(&panel, "GET", "/api/state", Some("test-token"), "").0,
        200
    );

    // A name that is not the panel's, as a rebinding site would send.
    for host in [
        Some("evil.example:8140".to_owned()),
        None,
        Some(format!("127.0.0.1:{}", panel.port + 1)),
    ] {
        let response = http::route(
            &panel,
            &Request {
                method: "GET".into(),
                path: "/api/state".into(),
                host,
                token: Some("test-token".into()),
                ..Request::default()
            },
        );
        assert_eq!(response.status, 403);
    }
    // The page carries the token for the script, and works with localhost too.
    let page = http::route(
        &panel,
        &Request {
            method: "GET".into(),
            path: "/".into(),
            host: Some(format!("localhost:{}", panel.port)),
            ..Request::default()
        },
    );
    assert_eq!(page.status, 200);
    assert!(
        String::from_utf8(page.body)
            .unwrap()
            .contains("content=\"test-token\"")
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_api_drives_the_bench_by_name_only() {
    let dir = scratch("api");
    let (bench, _) = bench(&dir, &["game1"], &[]);
    accept_in(&dir.join("game1"));
    let panel = panel(&bench);
    let t = Some("test-token");

    let (status, state, _) = request(&panel, "GET", "/api/state", t, "");
    assert_eq!(status, 200);
    assert_eq!(state["processes"][0]["name"], "game1");
    assert_eq!(state["processes"][0]["state"], "stopped");

    // No program or path can be given: only names of the bench.
    let (status, error, _) = request(
        &panel,
        "POST",
        "/api/action",
        t,
        r#"{"name":"cmd.exe","action":"start"}"#,
    );
    assert_eq!(status, 400);
    assert!(
        error["error"]
            .as_str()
            .unwrap()
            .contains("no process called")
    );
    assert_eq!(
        request(
            &panel,
            "POST",
            "/api/action",
            t,
            r#"{"name":"game1","action":"delete"}"#
        )
        .0,
        400
    );

    assert_eq!(
        request(
            &panel,
            "POST",
            "/api/action",
            t,
            r#"{"name":"proxy","action":"start"}"#
        )
        .0,
        200
    );
    ready(&panel.supervisor, "proxy");

    // The test buttons go through the proxy's console.
    let (_, players, _) = request(&panel, "POST", "/api/test/players", t, "{}");
    assert!(
        players["lines"].to_string().contains("Alex on lobby"),
        "{players}"
    );
    let (status, sent, _) = request(
        &panel,
        "POST",
        "/api/test/send",
        t,
        r#"{"player":"Alex","target":"game1"}"#,
    );
    assert_eq!(status, 200);
    assert_eq!(
        sent["lines"][0],
        "transfer not available yet: Alex would move to game1"
    );
    // Only a player name and a server of the bench are taken.
    for body in [
        r#"{"player":"Alex; end","target":"game1"}"#,
        r#"{"player":"","target":"game1"}"#,
        r#"{"player":"Alex","target":"proxy"}"#,
        r#"{"player":"Alex","target":"nether"}"#,
    ] {
        assert_eq!(
            request(&panel, "POST", "/api/test/send", t, body).0,
            400,
            "{body}"
        );
    }

    // A console line, and the log since a number.
    let (_, answer, _) = request(
        &panel,
        "POST",
        "/api/command",
        t,
        r#"{"name":"proxy","line":"servers"}"#,
    );
    assert_eq!(answer["lines"][0], "got: servers");
    let (_, log, _) = request(&panel, "GET", "/api/log?name=proxy&since=0", t, "");
    assert!(log["lines"].to_string().contains("> servers"));
    let next = log["next"].as_u64().unwrap();
    let (_, later, _) = request(
        &panel,
        "GET",
        &format!("/api/log?name=proxy&since={next}"),
        t,
        "",
    );
    assert_eq!(later["lines"].as_array().unwrap().len(), 0);

    assert_eq!(
        request(
            &panel,
            "POST",
            "/api/action",
            t,
            r#"{"name":"proxy","action":"stop"}"#
        )
        .0,
        200
    );
    wait_for("stopped", || {
        state_of(&panel.supervisor, "proxy") == State::Stopped
    });
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_panel_listens_on_127_0_0_1_only() {
    let listener = http::bind(0).unwrap();
    assert!(listener.local_addr().unwrap().ip().is_loopback());
}
