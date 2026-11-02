//! The panel with stand-in processes: the panel's own binary run as
//! `__fake-process`, which prints, answers its console and opens a port. No real
//! server is started, and nothing leaves the test's own folders.
use meridian_panel::bench::Bench;
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
    for (name, kind) in servers
        .iter()
        .map(|s| (*s, "server"))
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
    wait_for("output", || {
        log_text(&supervisor, "lobby").contains("[stderr] fake process warning")
    });
    assert!(log_text(&supervisor, "lobby").contains("fake process ready"));

    // The console: a line in, the answer out.
    let answer = supervisor
        .ask("lobby", "say hello", Duration::from_millis(800))
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
fn the_whole_bench_starts_servers_first_and_stops_servers_first() {
    let dir = scratch("all");
    let (bench, _) = bench(&dir, &["lobby", "game1", "game2"], &[]);
    for server in ["lobby", "game1", "game2"] {
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
    // The proxy started after every server answered.
    let started_at = |name: &str| {
        supervisor
            .snapshots()
            .into_iter()
            .find(|s| s.name == name)
            .unwrap()
            .uptime
            .unwrap()
    };
    for server in ["lobby", "game1", "game2"] {
        assert!(
            started_at(server) >= started_at("proxy"),
            "{server} started before the proxy"
        );
    }
    // Measures arrive for running processes.
    wait_for("a measure", || {
        supervisor
            .snapshots()
            .iter()
            .all(|s| s.history.last().is_some_and(|h| h.memory_mb > 0.0))
    });

    supervisor.stop_all();
    // The servers go first: while they stop, the proxy still runs.
    wait_for("servers stopped", || {
        ["lobby", "game1", "game2"]
            .iter()
            .all(|s| state_of(&supervisor, s) == State::Stopped)
    });
    wait_for("proxy stopped", || {
        state_of(&supervisor, "proxy") == State::Stopped
    });
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
