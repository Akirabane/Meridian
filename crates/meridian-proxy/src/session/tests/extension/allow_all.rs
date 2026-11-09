//! The SDK's example `allow-all` (crates/meridian-extension/examples/allow_all.rs) run as a separate
//! process: the program cargo builds for it, started with a token file and a port the system picks. With it
//! the proxy places players and decides commands as it does without it, it is asked each time and in time,
//! and once it is stopped the on-error policies decide, at once.
//!
//! Every wait is for something to happen (a line the program writes, the link going down), bounded, and
//! fails with what was seen; no port is fixed (see `example`).
use super::command::{DENIED_SEND, FILE, hook_of, six_commands};
use super::example::{Example, WAIT};
use super::server::{ALL_UP, EAST, LOBBY, WEST, login_alex, lose_lobby};
use super::*;
use meridian_config::{
    BalancedServer, Balancing, CommandDecision, Role, ServerDecision, ServerOnError,
};

/// The deadline of each decision: far above what the program takes, so that a slow machine is not
/// taken for a slow extension.
const DEADLINE_MS: u64 = 5000;

type AllowAll = Example;

fn start() -> AllowAll {
    Example::start("allow_all", &[])
}

/// The extension's `[[extension]]`, granted the three decisions; each falls back on `default`.
fn extension(allow_all: &AllowAll) -> ExtensionConfig {
    let mut extension = extension_config(
        allow_all.address.clone(),
        allow_all.folder.token(),
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
        extensions: vec![extension(allow_all)],
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
    let allow_all = start();
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

/// The host the player typed to connect, and the address they connect from (the session's `peer`).
const HOST: &str = "play.example.org";
const PLAYER_ADDRESS: &str = "198.51.100.7";

#[test]
fn what_allow_all_writes_holds_neither_the_address_nor_the_host_and_a_fallback_is_written_as_one() {
    let allow_all = start();
    // Granted `addresses` as well: it does not ask for it, so it is not given the address.
    let mut extension = extension(&allow_all);
    extension.grant.push("addresses".into());
    let config = Config {
        extensions: vec![extension],
        ..alone()
    };
    let sessions = sessions_for(&config);
    let info = sessions.extensions.link("gate").unwrap().info().unwrap();
    assert!(
        !info.granted.iter().any(|c| c == "addresses"),
        "{:?}",
        info.granted
    );

    let (without, _) = login_alex(
        &alone(),
        &Sessions::default(),
        &ALL_UP,
        Some(HOST),
        vec![Step::ExpectClose],
    );
    let (tried, _) = login_alex(
        &config,
        &sessions,
        &ALL_UP,
        Some(HOST),
        vec![Step::ExpectClose],
    );
    assert_eq!(tried, without);
    // The lobby is lost: the fallback goes where it goes without allow-all.
    let (answer, without) = lose_lobby(&alone(), &Sessions::default(), &ALL_UP);
    let expected = answer.unwrap().backend;
    let (answer, tried) = lose_lobby(&config, &sessions, &ALL_UP);
    assert_eq!(tried, without);
    assert_eq!(answer.unwrap().backend, expected);
    assert_eq!(decisions(&sessions, "login", "default"), 1);
    assert_eq!(decisions(&sessions, "server", "default"), 2);
    none_late(&sessions);

    let lines = allow_all.wrote(3);
    assert_eq!(
        lines,
        [
            LOGIN,
            SERVER,
            "server player=Alex reason=fallback candidates=2 -> default"
        ]
    );
    for line in &lines {
        assert!(
            !line.contains(HOST) && !line.contains(PLAYER_ADDRESS),
            "{line:?}"
        );
    }
}

#[test]
fn with_balancing_a_player_goes_where_the_balancer_sends_them_without_allow_all() {
    let allow_all = start();
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
    let allow_all = start();
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
    let mut allow_all = start();
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
