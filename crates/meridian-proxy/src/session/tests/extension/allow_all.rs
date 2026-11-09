//! The SDK's example `allow-all` (crates/meridian-extension/examples/allow_all.rs) run as a separate
//! process: the program cargo builds for it, started with a token file and a port the system picks. With it
//! the proxy places players and decides commands as it does without it, it is asked each time and in time,
//! and once it is stopped the on-error policies decide, at once.
//!
//! Every wait is for something to happen (a line the program writes, the link going down), bounded, and
//! fails with what was seen; no port is fixed.
use super::command::{DENIED_SEND, FILE, hook_of, six_commands};
use super::server::{ALL_UP, EAST, LOBBY, WEST, login_alex};
use super::*;
use meridian_config::{
    BalancedServer, Balancing, CommandDecision, Role, ServerDecision, ServerOnError,
};
use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::sync::mpsc::{self, Receiver};
use std::time::{SystemTime, UNIX_EPOCH};

/// The longest wait for the program to say where it listens, for a line it writes, or for the proxy to see
/// it gone.
const WAIT: Duration = Duration::from_secs(10);

/// The longest wait for cargo to build the program: nothing to do when `cargo test` built it already.
const BUILD: Duration = Duration::from_secs(300);

/// The deadline of each decision: far above what the program takes, so that a slow machine is not
/// taken for a slow extension.
const DEADLINE_MS: u64 = 5000;

/// The program, built by cargo once for the tests of this module; cargo says where it put it.
fn program() -> &'static PathBuf {
    static PROGRAM: OnceLock<PathBuf> = OnceLock::new();
    PROGRAM.get_or_init(|| {
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| env!("CARGO").into());
        let mut build = Command::new(cargo);
        build
            .args([
                "build",
                "-p",
                "meridian-extension",
                "--example",
                "allow_all",
            ])
            .arg("--message-format=json")
            .current_dir(env!("CARGO_MANIFEST_DIR"));
        let (send, built) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = send.send(build.output());
        });
        let output = built
            .recv_timeout(BUILD)
            .expect("cargo did not build the example allow_all in time")
            .expect("cannot run cargo to build the example allow_all");
        let messages = String::from_utf8_lossy(&output.stdout);
        messages
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|m| m["reason"] == "compiler-artifact" && m["target"]["name"] == "allow_all")
            .find_map(|m| m["executable"].as_str().map(PathBuf::from))
            .unwrap_or_else(|| {
                panic!(
                    "cargo gave no program for the example allow_all ({}):\n{}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr)
                )
            })
    })
}

/// The lines of `stream`, as they come.
fn follow(stream: impl Read + Send + 'static) -> Receiver<String> {
    let (send, lines) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            if send.send(line).is_err() {
                break;
            }
        }
    });
    lines
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
}

/// The program while it runs; killed when dropped.
struct AllowAll {
    child: Child,
    folder: Folder,
    address: String,
    /// What it writes on its standard output: a line for each request.
    lines: Receiver<String>,
    /// What the SDK writes on the standard error stream.
    errors: Receiver<String>,
    /// When it was started, by the system's clock.
    started_ms: u128,
}

impl AllowAll {
    fn start() -> Self {
        let folder = Folder::new();
        let started_ms = now_ms();
        let mut child = Command::new(program())
            .arg(folder.token())
            .arg("127.0.0.1:0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("cannot start the example allow_all");
        let lines = follow(child.stdout.take().unwrap());
        let errors = follow(child.stderr.take().unwrap());
        let mut allow_all = Self {
            child,
            folder,
            address: String::new(),
            lines,
            errors,
            started_ms,
        };
        // The SDK says where it listens: the port the system picked.
        let until = Instant::now() + WAIT;
        let mut seen = Vec::new();
        allow_all.address = loop {
            let left = until.saturating_duration_since(Instant::now());
            match allow_all.errors.recv_timeout(left) {
                Ok(line) => match line.split_once(" listens on ") {
                    Some((_, address)) => break address.trim().to_owned(),
                    None => seen.push(line),
                },
                Err(_) => panic!(
                    "allow_all never said where it listens; it wrote:\n{}",
                    seen.join("\n")
                ),
            }
        };
        allow_all
    }

    /// The extension's `[[extension]]`, granted the three decisions; each falls back on `default`.
    fn extension(&self) -> ExtensionConfig {
        let mut extension = extension_config(
            self.address.clone(),
            self.folder.token(),
            &["decide.login", "decide.server", "decide.command"],
            OnError::Default,
            DEADLINE_MS,
        );
        extension.decide_server = ServerDecision {
            timeout_ms: DEADLINE_MS,
            on_error: ServerOnError::Default,
        };
        extension.decide_command = CommandDecision {
            timeout_ms: DEADLINE_MS,
            on_error: OnError::Default,
            cache_seconds_max: 0,
        };
        extension
    }

    /// The next `count` lines it writes, each without its time, which is checked: a number of
    /// milliseconds between its start and now.
    fn wrote(&self, count: usize) -> Vec<String> {
        let until = Instant::now() + WAIT;
        let mut lines = Vec::new();
        while lines.len() < count {
            let left = until.saturating_duration_since(Instant::now());
            let Ok(line) = self.lines.recv_timeout(left) else {
                panic!(
                    "allow_all wrote {} lines of {count}: {lines:?}",
                    lines.len()
                );
            };
            let (time, rest) = line.split_once(' ').unwrap_or_else(|| panic!("{line:?}"));
            let time: u128 = time.parse().unwrap_or_else(|_| panic!("{line:?}"));
            assert!(
                (self.started_ms..=now_ms()).contains(&time),
                "{line:?} is not of this run"
            );
            lines.push(rest.to_owned());
        }
        lines
    }

    /// Kills the program and waits until it is gone.
    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for AllowAll {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Three backends, `lobby` then `east` and `west`, and the six proxy commands: what the proxy does with
/// no extension.
fn alone() -> Config {
    Config {
        try_backends: vec!["east".into(), "west".into()],
        ..six_commands()
    }
}

/// The same with balancing on, each backend a lobby.
fn balanced(config: Config) -> Config {
    let server = |backend: &str| BalancedServer {
        backend: backend.into(),
        role: Role::Lobby,
        soft_cap: 10,
        hard_cap: 20,
    };
    Config {
        balanced: vec![server("lobby"), server("east"), server("west")],
        balancing: Balancing {
            enabled: true,
            ..Balancing::default()
        },
        ..config
    }
}

fn with(allow_all: &AllowAll, config: Config) -> Config {
    Config {
        extensions: vec![allow_all.extension()],
        ..config
    }
}

/// The lines allow-all writes for Alex's login and the choice of their server, without the time.
const LOGIN: &str = "login player=Alex -> default";
const SERVER: &str = "server player=Alex reason=login candidates=3 -> default";

fn decisions(sessions: &Sessions, point: &'static str, decision: &'static str) -> u64 {
    sessions
        .extensions
        .link("gate")
        .unwrap()
        .stats()
        .decisions(point, decision)
}

/// No request of the three methods ran out of time.
fn none_late(sessions: &Sessions) {
    let link = sessions.extensions.link("gate").unwrap();
    for method in [decide::LOGIN, decide::SERVER, decide::COMMAND] {
        assert_eq!(
            link.stats().count(method, RequestOutcome::Timeout),
            0,
            "{method}"
        );
    }
}

#[test]
fn a_player_goes_where_the_routes_send_them_without_allow_all_and_it_is_asked_at_login_and_for_the_server()
 {
    let allow_all = AllowAll::start();
    let downs: [[(&str, bool); 3]; 3] = [
        ALL_UP,
        [(LOBBY, false), (EAST, true), (WEST, true)],
        [(LOBBY, false), (EAST, false), (WEST, true)],
    ];
    // The third time, the lobby that just failed is tried after the others, which are enough.
    let expected: [&[&str]; 3] = [&[LOBBY], &[LOBBY, EAST], &[EAST, WEST]];
    let config = with(&allow_all, alone());
    let sessions = sessions_for(&config);
    let by_itself = Sessions::default();
    for (up, expected) in downs.iter().zip(expected) {
        let (without, _) = login_alex(&alone(), &by_itself, up, None, vec![Step::ExpectClose]);
        assert_eq!(without, expected, "{up:?}");
        let (tried, _) = login_alex(&config, &sessions, up, None, vec![Step::ExpectClose]);
        assert_eq!(tried, without, "{up:?}");
    }
    assert_eq!(decisions(&sessions, "login", "default"), 3);
    assert_eq!(decisions(&sessions, "server", "default"), 3);
    none_late(&sessions);
    assert_eq!(
        allow_all.wrote(6),
        [LOGIN, SERVER, LOGIN, SERVER, LOGIN, SERVER]
    );
}

#[test]
fn with_balancing_a_player_goes_where_the_balancer_sends_them_without_allow_all() {
    let allow_all = AllowAll::start();
    let config = with(&allow_all, balanced(alone()));
    let sessions = sessions_for(&config);
    let by_itself = Sessions::default();
    for up in [ALL_UP, [(LOBBY, false), (EAST, true), (WEST, true)]] {
        let (without, _) = login_alex(
            &balanced(alone()),
            &by_itself,
            &up,
            None,
            vec![Step::ExpectClose],
        );
        let (tried, _) = login_alex(&config, &sessions, &up, None, vec![Step::ExpectClose]);
        assert_eq!(tried, without, "{up:?}");
    }
    assert_eq!(decisions(&sessions, "login", "default"), 2);
    assert_eq!(decisions(&sessions, "server", "default"), 2);
    none_late(&sessions);
    assert_eq!(allow_all.wrote(4), [LOGIN, SERVER, LOGIN, SERVER]);
}

#[test]
fn a_command_is_decided_by_the_file_as_without_allow_all_and_what_was_typed_is_not_written() {
    let allow_all = AllowAll::start();
    let config = with(&allow_all, alone());
    let sessions = sessions_for(&config);
    let by_itself = Sessions::default();
    let typed = [
        (FILE, "send all east", Some("done")),
        ("", "send all east", Some(DENIED_SEND)),
        (FILE, "alert hello there", Some("done")),
        ("", "alert hello there", None),
        (FILE, "server east", None),
    ];
    for (file, typed, expected) in typed {
        let (hook, _) = hook_of(&alone(), &by_itself, file);
        let without = hook(typed);
        if let Some(expected) = expected {
            assert_eq!(without.as_deref(), Some(expected), "{typed:?}");
        }
        let (hook, _) = hook_of(&config, &sessions, file);
        assert_eq!(hook(typed), without, "{typed:?} with the file {file:?}");
    }
    let asked = usize::try_from(decisions(&sessions, "command", "default")).unwrap();
    assert!(asked >= typed.len(), "allow_all was asked {asked} times");
    none_late(&sessions);
    let lines = allow_all.wrote(asked);
    for line in &lines {
        assert!(
            line.starts_with("command player=Alex nodes=meridian.")
                && line.ends_with(" -> default"),
            "{line:?}"
        );
        // What the player typed after the command is theirs, and not written; a node may name a server of
        // the configuration (`meridian.server.east`), never anything else they typed.
        assert!(!line.contains("hello") && !line.contains("all"), "{line:?}");
    }
}

#[test]
fn once_allow_all_is_stopped_the_policies_decide_and_nothing_waits_for_it() {
    let mut allow_all = AllowAll::start();
    let config = with(&allow_all, alone());
    let sessions = sessions_for(&config);
    allow_all.stop();
    let link = sessions.extensions.link("gate").unwrap();
    let until = Instant::now() + WAIT;
    while link.is_connected() {
        assert!(Instant::now() < until, "the proxy never saw allow_all go");
        std::thread::sleep(Duration::from_millis(5));
    }

    let began = Instant::now();
    let (tried, _) = login_alex(&config, &sessions, &ALL_UP, None, vec![Step::ExpectClose]);
    assert_eq!(tried, [LOBBY]);
    let (hook, _) = hook_of(&config, &sessions, FILE);
    assert_eq!(hook("send all east").as_deref(), Some("done"));
    let (hook, _) = hook_of(&config, &sessions, "");
    assert_eq!(hook("send all east").as_deref(), Some(DENIED_SEND));
    assert!(
        began.elapsed() < Duration::from_millis(DEADLINE_MS),
        "a decision waited for an extension that is gone"
    );
    assert_eq!(decisions(&sessions, "login", "policy_default"), 1);
    assert_eq!(decisions(&sessions, "server", "policy_default"), 1);
    assert_eq!(decisions(&sessions, "command", "policy_default"), 2);
    assert_eq!(decisions(&sessions, "login", "default"), 0);
    none_late(&sessions);
}
