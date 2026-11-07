//! The admission at login by an extension (docs/EXTENSIONS.md, 4.2), through `serve` and the real
//! connection, against the SDK's end of it. The extension can only add a refusal: what the bans and the
//! whitelist refuse stays refused whatever it says, and it is not even asked.
use super::*;
use crate::extension::RequestOutcome;
use crate::extension::login::DEFAULT_REASON;
use crate::metrics::LoginFailure;
use meridian_config::{Extension as ExtensionConfig, LoginDecision, OnError, Target};
use meridian_extension::decide::{self, Auth, Decision, LoginRequest};
use meridian_extension::handshake::Secret;
use meridian_extension::serve::{Extension as Sdk, Log, Running};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

const TOKEN: &[u8] = b"login-gate-test-token-0123456789abcdef";

static FOLDERS: AtomicUsize = AtomicUsize::new(0);

/// A folder for the token file, removed at the end of the test.
struct Folder(PathBuf);

impl Folder {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "meridian-gate-test-{}-{}",
            std::process::id(),
            FOLDERS.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("token"), TOKEN).unwrap();
        Self(path)
    }

    fn token(&self) -> PathBuf {
        self.0.join("token")
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The lines the extension (the SDK) writes.
#[derive(Default)]
struct Lines(Mutex<Vec<String>>);

impl Log for Lines {
    fn info(&self, line: &str) {
        self.0.lock().unwrap().push(line.to_owned());
    }

    fn warn(&self, line: &str) {
        self.0.lock().unwrap().push(line.to_owned());
    }
}

/// An extension that runs, with what it was asked and what it wrote.
struct Gate {
    running: Running,
    folder: Folder,
    asked: Arc<AtomicUsize>,
    seen: Arc<Mutex<Vec<LoginRequest>>>,
    lines: Arc<Lines>,
}

/// Starts an extension that decides with `decide`, after `delay`, and asks for `wants` besides.
fn gate_with(
    delay: Duration,
    wants: &[&str],
    decide: impl Fn(&LoginRequest) -> Decision + Send + Sync + 'static,
) -> Gate {
    let asked = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let lines = Arc::new(Lines::default());
    let mut extension = Sdk::new("gate", "1.0.0", Secret::new(TOKEN).unwrap())
        .log(Arc::clone(&lines) as Arc<dyn Log>)
        .on_login({
            let (asked, seen) = (Arc::clone(&asked), Arc::clone(&seen));
            move |request| {
                asked.fetch_add(1, Ordering::SeqCst);
                seen.lock().unwrap().push(request.clone());
                std::thread::sleep(delay);
                decide(request)
            }
        });
    for capability in wants {
        extension = extension.wants(capability);
    }
    Gate {
        running: extension.bind("127.0.0.1:0").unwrap(),
        folder: Folder::new(),
        asked,
        seen,
        lines,
    }
}

/// Refuses Alex with a reason, lets Bob in, and has no opinion of anyone else.
fn gate() -> Gate {
    gate_with(Duration::ZERO, &[], |request| {
        match request.player.name.as_str() {
            "Alex" => Decision::deny("The event starts at 20:00."),
            "Bob" => Decision::Allow,
            _ => Decision::Default,
        }
    })
}

fn extension_config(
    address: String,
    token_file: PathBuf,
    grant: &[&str],
    on_error: OnError,
    timeout_ms: u64,
) -> ExtensionConfig {
    ExtensionConfig {
        name: "gate".into(),
        enabled: true,
        address,
        token_file,
        grant: grant.iter().map(|g| (*g).to_owned()).collect(),
        max_in_flight: 64,
        breaker_failures: 5,
        breaker_seconds: 30,
        decide_login: LoginDecision {
            timeout_ms,
            on_error,
        },
        decide_command: Default::default(),
        decide_server: Default::default(),
    }
}

impl Gate {
    fn extension(&self, grant: &[&str], on_error: OnError, timeout_ms: u64) -> ExtensionConfig {
        extension_config(
            self.running.local_addr().to_string(),
            self.folder.token(),
            grant,
            on_error,
            timeout_ms,
        )
    }

    /// The routing configuration with `extension`.
    fn config(extension: ExtensionConfig) -> Config {
        Config {
            extensions: vec![extension],
            ..routed()
        }
    }
}

/// Sessions whose registry runs the extensions of `config`, once the connection to them is up.
fn sessions_for(config: &Config) -> Sessions {
    let sessions = Sessions::default();
    sessions.extensions.follow(config);
    for link in sessions.extensions.links() {
        let limit = Instant::now() + Duration::from_secs(10);
        while !link.is_connected() {
            assert!(Instant::now() < limit, "the extension never connected");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    sessions
}

/// Logs `name` in with `auth` and `host`: the backends tried and what the player was told.
fn login_to(
    config: &Config,
    sessions: &Sessions,
    name: &str,
    auth: client::AuthKind,
    host: Option<&str>,
    script: Vec<Step>,
) -> (Vec<String>, Trace) {
    let (connect, backends) = reachable(vec![("127.0.0.1:25601", true)]);
    route_one(
        config,
        sessions,
        &*connect,
        &backends,
        logged_in(name, auth, host),
        script,
    )
}

fn offline() -> client::AuthKind {
    client::AuthKind::Offline
}

/// Goes to a backend.
fn goes_in(config: &Config, sessions: &Sessions, name: &str) {
    let (tried, _) = login_to(
        config,
        sessions,
        name,
        offline(),
        None,
        vec![Step::ExpectClose],
    );
    assert_eq!(tried, ["127.0.0.1:25601"], "{name} should have gone in");
}

/// Is refused before any backend, with this reason.
fn is_refused(config: &Config, sessions: &Sessions, name: &str, reason: &str) {
    let (tried, trace) = login_to(config, sessions, name, offline(), None, refused());
    assert!(tried.is_empty(), "{name}: a backend was asked");
    assert_eq!(refusal(&trace), reason, "{name}");
}

fn decisions(sessions: &Sessions, decision: &'static str) -> u64 {
    sessions
        .extensions
        .link("gate")
        .unwrap()
        .stats()
        .decisions("login", decision)
}

// ---------------------------------------------------------------------------------------------

#[test]
fn without_an_extension_nothing_is_asked_and_nothing_is_on_the_page() {
    let config = routed();
    assert!(config.extensions.is_empty());
    let sessions = Sessions::default();
    goes_in(&config, &sessions, "Alex");
    assert!(sessions.extensions.links().is_empty());
    let mut page = String::new();
    sessions.extensions.render(&mut page);
    assert!(page.is_empty());
    assert_eq!(sessions.metrics().failures_of(LoginFailure::Extension), 0);
}

#[test]
fn an_extension_that_refuses_turns_the_player_away_with_its_reason_before_any_backend() {
    let gate = gate();
    let config = Gate::config(gate.extension(&["decide.login"], OnError::Default, 1000));
    let sessions = sessions_for(&config);

    is_refused(&config, &sessions, "Alex", "The event starts at 20:00.");
    assert_eq!(sessions.admitted(), 0, "a refused player takes no place");
    assert_eq!(sessions.metrics().failures_of(LoginFailure::Extension), 1);
    assert_eq!(decisions(&sessions, "deny"), 1);

    // The others are not held by it: `allow` and `default` both go on to a backend.
    goes_in(&config, &sessions, "Bob");
    goes_in(&config, &sessions, "Somebody");
    assert_eq!(decisions(&sessions, "allow"), 1);
    assert_eq!(decisions(&sessions, "default"), 1);
    assert_eq!(gate.asked.load(Ordering::SeqCst), 3);
    assert_eq!(sessions.metrics().failures_of(LoginFailure::Extension), 1);

    // The page tells what was decided, with no player in it.
    let mut page = String::new();
    sessions.extensions.render(&mut page);
    for line in [
        "meridian_extension_decisions_total{extension=\"gate\",point=\"login\",decision=\"deny\"} 1",
        "meridian_extension_decisions_total{extension=\"gate\",point=\"login\",decision=\"allow\"} 1",
        "meridian_extension_decisions_total{extension=\"gate\",point=\"login\",decision=\"default\"} 1",
    ] {
        assert!(page.contains(line), "{line}\n{page}");
    }
    assert!(!page.contains("Alex") && !page.contains("Bob"));
}

#[test]
fn a_refusal_without_a_reason_says_what_the_proxy_says_and_a_reason_may_have_two_lines() {
    let gate = gate_with(Duration::ZERO, &[], |request| {
        match request.player.name.as_str() {
            "Silent" => Decision::Deny(None),
            _ => Decision::deny("Come back at 20:00.\nSee the website."),
        }
    });
    let config = Gate::config(gate.extension(&["decide.login"], OnError::Default, 1000));
    let sessions = sessions_for(&config);
    is_refused(&config, &sessions, "Silent", DEFAULT_REASON);
    is_refused(
        &config,
        &sessions,
        "Alex",
        "Come back at 20:00.\nSee the website.",
    );
    assert_eq!(DEFAULT_REASON, "You cannot join this server right now.");
}

/// An `allow` is not a pass: the bans and the whitelist are settled before the extension is asked, so
/// it never lets in a player they turned away, and a banned player costs it no request.
#[test]
fn an_allow_does_not_let_in_a_banned_player_or_one_the_whitelist_keeps_out() {
    let gate = gate_with(Duration::ZERO, &[], |_| Decision::Allow);
    let config = Gate::config(gate.extension(&["decide.login"], OnError::Allow, 1000));
    let sessions = sessions_for(&config);

    let _ = sessions.access.change(|a| {
        (
            (),
            a.ban(Target::parse("Alex").unwrap(), Some("griefing".into())),
        )
    });
    is_refused(
        &config,
        &sessions,
        "Alex",
        "You are banned from this server.\nReason: griefing",
    );
    assert_eq!(sessions.metrics().failures_of(LoginFailure::Banned), 1);
    assert_eq!(sessions.metrics().failures_of(LoginFailure::Extension), 0);

    let _ = sessions.access.change(|a| {
        a.allow(Target::parse("Bob").unwrap()).unwrap();
        a.set_whitelist(true);
        ((), true)
    });
    is_refused(
        &config,
        &sessions,
        "Steve",
        "You are not whitelisted on this server.",
    );
    assert_eq!(
        asked(&gate),
        0,
        "a player they refuse is not put to the extension"
    );

    // Who passes them is asked, and the extension's `allow` takes them on to a backend.
    goes_in(&config, &sessions, "Bob");
    assert_eq!(asked(&gate), 1);
}

fn asked(gate: &Gate) -> usize {
    gate.asked.load(Ordering::SeqCst)
}

#[test]
fn an_allow_does_not_skip_the_second_login_either() {
    let gate = gate_with(Duration::ZERO, &[], |_| Decision::Allow);
    let config = Gate::config(gate.extension(&["decide.login"], OnError::Default, 1000));
    let sessions = sessions_for(&config);
    let (_open, _relay, _player, _server) =
        connected(&sessions, "Alex", Identity::Unverified("alex".into()));
    let (tried, trace) = login_to(&config, &sessions, "ALEX", offline(), None, refused());
    assert!(tried.is_empty());
    assert_eq!(refusal(&trace), ALREADY_CONNECTED);
    // The extension was told that a session of the player is open.
    assert!(gate.seen.lock().unwrap()[0].connected);
}

#[test]
fn a_login_the_extension_refuses_does_not_end_the_session_it_would_have_replaced() {
    let gate = gate();
    let mut config = Gate::config(gate.extension(&["decide.login"], OnError::Default, 1000));
    config.on_duplicate = OnDuplicate::Replace;
    let sessions = sessions_for(&config);
    let (_open, _relay, mut player, _server) =
        connected(&sessions, "Alex", Identity::Unverified("alex".into()));
    is_refused(&config, &sessions, "Alex", "The event starts at 20:00.");
    assert_eq!(
        sessions.count(),
        1,
        "the session already open is not touched"
    );
    // Nothing was sent to it: its player is not told they logged in from elsewhere.
    player
        .reader
        .set_read_timeout(Some(Duration::from_millis(50)))
        .unwrap();
    assert!(player.recv().is_err());
}

#[test]
fn what_the_extension_is_told_of_the_player() {
    let gate = gate_with(Duration::ZERO, &["addresses"], |_| Decision::Default);
    let config =
        Gate::config(gate.extension(&["decide.login", "addresses"], OnError::Default, 1000));
    let sessions = sessions_for(&config);
    let host = Some("play.example.org");
    login_to(
        &config,
        &sessions,
        "Notch",
        client::AuthKind::Premium,
        host,
        vec![Step::ExpectClose],
    );
    login_to(
        &config,
        &sessions,
        "Alex",
        offline(),
        None,
        vec![Step::ExpectClose],
    );
    let seen = gate.seen.lock().unwrap();
    assert_eq!(seen[0].player.name, "Notch");
    assert_eq!(seen[0].player.auth, Auth::Verified);
    assert_eq!(
        seen[0].player.uuid,
        hyphenated(offline_uuid("Notch").to_be_bytes())
    );
    assert_eq!(seen[0].host.as_deref(), host);
    assert_eq!(seen[0].address.as_deref(), Some("198.51.100.7"));
    assert!(!seen[0].connected);
    // An unverified player is told so, and no host is none.
    assert_eq!(seen[1].player.auth, Auth::Unverified);
    assert_eq!(seen[1].host, None);
}

/// The address goes to an extension that asked for it and was granted it, and to no other.
#[test]
fn the_address_is_told_only_with_the_capability_asked_and_granted() {
    for (wants, grant, told) in [
        (vec!["addresses"], vec!["decide.login", "addresses"], true),
        // Asked and not granted.
        (vec!["addresses"], vec!["decide.login"], false),
        // Granted and not asked: "something granted but not declared is not used".
        (vec![], vec!["decide.login", "addresses"], false),
    ] {
        let gate = gate_with(Duration::ZERO, &wants, |_| Decision::Default);
        let config = Gate::config(gate.extension(&grant, OnError::Default, 1000));
        let sessions = sessions_for(&config);
        goes_in(&config, &sessions, "Alex");
        let address = gate.seen.lock().unwrap()[0].address.clone();
        assert_eq!(address.is_some(), told, "wants {wants:?}, grant {grant:?}");
    }
}

#[test]
fn nothing_private_is_in_what_the_extension_writes() {
    let gate = gate_with(Duration::ZERO, &["addresses"], |_| {
        Decision::deny("Not now.")
    });
    let config =
        Gate::config(gate.extension(&["decide.login", "addresses"], OnError::Default, 1000));
    let sessions = sessions_for(&config);
    is_refused(&config, &sessions, "Alex", "Not now.");
    let lines = gate.lines.0.lock().unwrap().join("\n");
    assert!(!lines.is_empty());
    for private in [
        "Alex",
        "198.51.100.7",
        std::str::from_utf8(TOKEN).unwrap(),
        &hyphenated(offline_uuid("Alex").to_be_bytes()),
        "Not now.",
    ] {
        assert!(!lines.contains(private), "{private}\n{lines}");
    }
}

/// With no answer in time, the operator's choice applies: `default` and `allow` go on, `deny` refuses.
#[test]
fn a_slow_extension_takes_the_on_error_policy() {
    let gate = gate_with(Duration::from_millis(600), &[], |_| {
        Decision::deny("Too late.")
    });
    let timeout = 100;
    let by = |policy| Gate::config(gate.extension(&["decide.login"], policy, timeout));
    let default = by(OnError::Default);
    let sessions = sessions_for(&default);

    let began = Instant::now();
    goes_in(&default, &sessions, "Alex");
    assert!(
        began.elapsed() < Duration::from_millis(500),
        "the login waited for the answer: {:?}",
        began.elapsed()
    );
    goes_in(&by(OnError::Allow), &sessions, "Alex");
    is_refused(&by(OnError::Deny), &sessions, "Alex", DEFAULT_REASON);

    assert_eq!(decisions(&sessions, "policy_default"), 1);
    assert_eq!(decisions(&sessions, "policy_allow"), 1);
    assert_eq!(decisions(&sessions, "policy_deny"), 1);
    // The answer that came late was not taken: it said deny, and two of the three went in.
    assert_eq!(sessions.metrics().failures_of(LoginFailure::Extension), 1);
    assert_eq!(sessions.extensions.links()[0].stats().late(), 0);
}

/// An extension that fails decision after decision is not asked any more for a while: its decisions take
/// the policy at once.
#[test]
fn an_extension_that_does_not_answer_trips_the_breaker_and_is_not_asked_again() {
    let gate = gate_with(Duration::from_millis(300), &[], |_| Decision::Allow);
    let mut extension = gate.extension(&["decide.login"], OnError::Deny, 50);
    extension.breaker_failures = 2;
    let config = Gate::config(extension);
    let sessions = sessions_for(&config);

    for _ in 0..2 {
        is_refused(&config, &sessions, "Alex", DEFAULT_REASON);
    }
    let link = sessions.extensions.links().remove(0);
    assert_eq!(
        link.stats().count(decide::LOGIN, RequestOutcome::Timeout),
        2
    );
    // The breaker is open: the next logins are refused by the policy without a request.
    for _ in 0..3 {
        is_refused(&config, &sessions, "Alex", DEFAULT_REASON);
    }
    assert_eq!(
        link.stats().count(decide::LOGIN, RequestOutcome::Breaker),
        3
    );
    assert_eq!(asked(&gate), 2, "the open breaker sent nothing");
    assert_eq!(decisions(&sessions, "policy_deny"), 5);
    assert_eq!(sessions.metrics().failures_of(LoginFailure::Extension), 5);
}

#[test]
fn an_extension_that_is_not_there_takes_the_policy_without_waiting() {
    // A port nothing listens on.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let folder = Folder::new();
    let by = |policy| Config {
        extensions: vec![extension_config(
            format!("127.0.0.1:{port}"),
            folder.token(),
            &["decide.login"],
            policy,
            5000,
        )],
        ..routed()
    };
    let config = by(OnError::Default);
    // It is not connected, so `sessions_for` is not used: the link is only started.
    let sessions = Sessions::default();
    sessions.extensions.follow(&config);

    let began = Instant::now();
    goes_in(&config, &sessions, "Alex");
    goes_in(&by(OnError::Allow), &sessions, "Alex");
    is_refused(&by(OnError::Deny), &sessions, "Alex", DEFAULT_REASON);
    assert!(
        began.elapsed() < Duration::from_secs(4),
        "a login waited for an extension that is not there"
    );
    assert_eq!(sessions.metrics().failures_of(LoginFailure::Extension), 1);
}

#[test]
fn an_extension_that_is_not_started_yet_takes_the_policy_too() {
    // A reload that added the extension and has not started it: the registry does not have it.
    let folder = Folder::new();
    let config = Config {
        extensions: vec![extension_config(
            "127.0.0.1:9".into(),
            folder.token(),
            &["decide.login"],
            OnError::Deny,
            1000,
        )],
        ..routed()
    };
    let sessions = Sessions::default();
    is_refused(&config, &sessions, "Alex", DEFAULT_REASON);
    let config = Config {
        extensions: vec![ExtensionConfig {
            decide_login: LoginDecision {
                timeout_ms: 1000,
                on_error: OnError::Default,
            },
            ..config.extensions[0].clone()
        }],
        ..config
    };
    goes_in(&config, &sessions, "Alex");
}

#[test]
fn what_the_extension_did_not_declare_is_not_asked_and_what_is_not_granted_is_not_either() {
    // The extension serves nothing: it does not declare decide.login.
    let running = Sdk::new("gate", "1.0.0", Secret::new(TOKEN).unwrap())
        .bind("127.0.0.1:0")
        .unwrap();
    let folder = Folder::new();
    let config = Config {
        extensions: vec![extension_config(
            running.local_addr().to_string(),
            folder.token(),
            &["decide.login"],
            OnError::Deny,
            1000,
        )],
        ..routed()
    };
    let sessions = sessions_for(&config);
    // Even with the policy `deny`, nothing is asked and nothing is refused: the point is not served.
    goes_in(&config, &sessions, "Alex");
    let link = sessions.extensions.links().remove(0);
    assert_eq!(
        link.stats().count(decide::LOGIN, RequestOutcome::Answered),
        0
    );
    assert_eq!(sessions.metrics().failures_of(LoginFailure::Extension), 0);

    // An extension that is switched off, or not granted the point, is not asked.
    let gate = gate();
    let mut off = gate.extension(&["decide.login"], OnError::Deny, 1000);
    off.enabled = false;
    let not_granted = gate.extension(&["orders.read"], OnError::Deny, 1000);
    for extension in [off, not_granted] {
        let config = Gate::config(extension);
        let sessions = sessions_for(&config);
        goes_in(&config, &sessions, "Alex");
        assert_eq!(asked(&gate), 0);
    }
}

// ---------------------------------------------------------------------------------------------
// The choice of server (docs/EXTENSIONS.md, 4.4)

mod server {
    use super::*;
    use crate::balance::{Reservation, Server};
    use crate::fallback::{Context, replacer};
    use meridian_config::{
        AuthKind, BalancedServer, Balancing, Role, ServerDecision, ServerOnError,
    };
    use meridian_extension::decide::{Choice, Placing, ServerRequest};
    use meridian_handoff::{Loss, Notice, Replacement};

    const LOBBY: &str = "127.0.0.1:25601";
    const EAST: &str = "127.0.0.1:25602";
    const WEST: &str = "127.0.0.1:25603";

    /// An extension that chooses servers, with what it was asked.
    struct Chooser {
        running: Running,
        folder: Folder,
        seen: Arc<Mutex<Vec<ServerRequest>>>,
    }

    fn chooser(decide: impl Fn(&ServerRequest) -> Choice + Send + Sync + 'static) -> Chooser {
        chooser_after(Duration::ZERO, decide)
    }

    fn chooser_after(
        delay: Duration,
        decide: impl Fn(&ServerRequest) -> Choice + Send + Sync + 'static,
    ) -> Chooser {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let extension = Sdk::new("gate", "1.0.0", Secret::new(TOKEN).unwrap())
            .log(Arc::new(Lines::default()) as Arc<dyn Log>)
            .on_server({
                let seen = Arc::clone(&seen);
                move |request| {
                    seen.lock().unwrap().push(request.clone());
                    std::thread::sleep(delay);
                    decide(request)
                }
            });
        Chooser {
            running: extension.bind("127.0.0.1:0").unwrap(),
            folder: Folder::new(),
            seen,
        }
    }

    impl Chooser {
        fn extension(&self, on_error: ServerOnError, timeout_ms: u64) -> ExtensionConfig {
            let mut config = extension_config(
                self.running.local_addr().to_string(),
                self.folder.token(),
                &["decide.server"],
                OnError::Default,
                1000,
            );
            config.decide_server = ServerDecision {
                timeout_ms,
                on_error,
            };
            config
        }
    }

    /// Three backends, `lobby` then `east` and `west`, and the extension.
    fn config_with(extension: ExtensionConfig) -> Config {
        Config {
            extensions: vec![extension],
            try_backends: vec!["east".into(), "west".into()],
            ..routed()
        }
    }

    /// The same, with balancing on: each backend a lobby with these caps.
    fn balanced_with(extension: ExtensionConfig, soft: u32, hard: u32) -> Config {
        let server = |backend: &str| BalancedServer {
            backend: backend.into(),
            role: Role::Lobby,
            soft_cap: soft,
            hard_cap: hard,
        };
        Config {
            balanced: vec![server("lobby"), server("east"), server("west")],
            balancing: Balancing {
                enabled: true,
                ..Balancing::default()
            },
            ..config_with(extension)
        }
    }

    /// Keeps a place on `backend`, as a player already there would.
    fn hold(sessions: &Sessions, config: &Config, backend: &str) -> Reservation {
        let limits = config.balanced(backend).unwrap();
        sessions
            .balancer
            .reserve_named(
                Server {
                    backend,
                    soft_cap: limits.soft_cap,
                    hard_cap: limits.hard_cap,
                    available: true,
                    ping: None,
                    reported_players: None,
                },
                || sessions.counts(),
            )
            .unwrap_or_else(|e| panic!("cannot hold a place on {backend}: {e}"))
    }

    /// Logs Alex in; `up` says which backends answer. The addresses tried and what the player got.
    fn login_alex(
        config: &Config,
        sessions: &Sessions,
        up: &[(&str, bool)],
        host: Option<&str>,
        script: Vec<Step>,
    ) -> (Vec<String>, Trace) {
        let (connect, backends) = reachable(up.to_vec());
        route_one(
            config,
            sessions,
            &*connect,
            &backends,
            logged_in("Alex", client::AuthKind::Offline, host),
            script,
        )
    }

    const ALL_UP: [(&str, bool); 3] = [(LOBBY, true), (EAST, true), (WEST, true)];

    /// Goes in, to the backends `tried`.
    fn goes_to(config: &Config, sessions: &Sessions, up: &[(&str, bool)], tried: &[&str]) {
        let (got, _) = login_alex(config, sessions, up, None, vec![Step::ExpectClose]);
        assert_eq!(got, tried);
    }

    fn refused_with(
        config: &Config,
        sessions: &Sessions,
        up: &[(&str, bool)],
        tried: &[&str],
    ) -> String {
        let (got, trace) = login_alex(config, sessions, up, None, refused());
        assert_eq!(got, tried);
        refusal(&trace)
    }

    fn decisions(sessions: &Sessions, decision: &'static str) -> u64 {
        sessions
            .extensions
            .link("gate")
            .unwrap()
            .stats()
            .decisions("server", decision)
    }

    #[test]
    fn without_a_server_extension_nothing_is_asked() {
        // An extension that holds only the login does not hold this point.
        let gate = gate_with(Duration::ZERO, &[], |_| Decision::Default);
        let config = Gate::config(gate.extension(&["decide.login"], OnError::Default, 1000));
        let sessions = sessions_for(&config);
        goes_to(&config, &sessions, &ALL_UP, &[LOBBY]);
        assert_eq!(decisions(&sessions, "default"), 0);
        assert_eq!(decisions(&sessions, "order"), 0);
    }

    #[test]
    fn default_leaves_the_routes_as_they_are_and_the_extension_is_told_who_and_what() {
        let chooser = chooser(|_| Choice::Default);
        let config = config_with(chooser.extension(ServerOnError::Default, 1000));
        let sessions = sessions_for(&config);
        let (tried, _) = login_alex(
            &config,
            &sessions,
            &ALL_UP,
            Some("play.example.org"),
            vec![Step::ExpectClose],
        );
        assert_eq!(tried, [LOBBY]);
        assert_eq!(decisions(&sessions, "default"), 1);
        let seen = chooser.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        let request = &seen[0];
        assert_eq!(request.reason, Placing::Login);
        assert_eq!(request.lost, None);
        assert_eq!(request.host.as_deref(), Some("play.example.org"));
        assert_eq!(request.player.name, "Alex");
        assert_eq!(request.player.auth, decide::Auth::Unverified);
        // What the routes allow, in the order the proxy would try them; with balancing off, the proxy
        // knows nothing more of them than their names.
        let names: Vec<&str> = request.candidates.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["lobby", "east", "west"]);
        assert!(request.candidates.iter().all(|c| c.role.is_none()
            && c.players.is_none()
            && c.soft_cap.is_none()
            && c.hard_cap.is_none()
            && c.state.is_none()));
    }

    #[test]
    fn an_order_is_tried_in_its_order_and_only_those() {
        let chooser = chooser(|_| Choice::order(["west", "east"]));
        let config = config_with(chooser.extension(ServerOnError::Default, 1000));
        let sessions = sessions_for(&config);
        // West first, as asked: not the default backend.
        goes_to(&config, &sessions, &ALL_UP, &[WEST]);
        // A server that does not answer is passed over for the next of the order, and the lobby, which
        // the order leaves out, is not tried however many fail.
        goes_to(
            &config,
            &sessions,
            &[(LOBBY, true), (EAST, true), (WEST, false)],
            &[WEST, EAST],
        );
        let said = refused_with(
            &config,
            &sessions,
            &[(LOBBY, true), (EAST, false), (WEST, false)],
            &[WEST, EAST],
        );
        assert!(said.starts_with("Cannot reach the servers"), "{said}");
        assert!(!said.contains("lobby"), "{said}");
        assert_eq!(decisions(&sessions, "order"), 3);
        assert_eq!(sessions.metrics().failures_of(LoginFailure::Unreachable), 1);
    }

    #[test]
    fn a_refusal_turns_the_player_away_with_its_reason_before_any_backend() {
        let chooser = chooser(|_| Choice::refuse("No game is running."));
        let config = config_with(chooser.extension(ServerOnError::Default, 1000));
        let sessions = sessions_for(&config);
        let said = refused_with(&config, &sessions, &ALL_UP, &[]);
        assert_eq!(said, "No game is running.");
        assert_eq!(sessions.admitted(), 0, "the place is given back");
        assert_eq!(sessions.metrics().failures_of(LoginFailure::Extension), 1);
        assert_eq!(decisions(&sessions, "refuse"), 1);
    }

    #[test]
    fn a_refusal_without_a_reason_says_what_the_proxy_says() {
        let chooser = chooser(|_| Choice::Refuse(None));
        let config = config_with(chooser.extension(ServerOnError::Default, 1000));
        let sessions = sessions_for(&config);
        assert_eq!(
            refused_with(&config, &sessions, &ALL_UP, &[]),
            DEFAULT_REASON
        );
    }

    /// A name that was not offered is an invalid answer: it counts against the extension and the
    /// operator's on-error policy applies, whatever else the answer says.
    #[test]
    fn a_server_that_was_not_offered_is_an_invalid_answer_and_takes_the_policy() {
        let chooser = chooser(|_| Choice::order(["west", "north"]));
        let config = config_with(chooser.extension(ServerOnError::Default, 1000));
        let sessions = sessions_for(&config);
        // `default`: as if there were no extension.
        goes_to(&config, &sessions, &ALL_UP, &[LOBBY]);
        assert_eq!(decisions(&sessions, "policy_default"), 1);
        assert_eq!(decisions(&sessions, "order"), 0);
        // The extension serves one proxy at a time: this one lets go of it first.
        drop(sessions);

        let config = config_with(chooser.extension(ServerOnError::Refuse, 1000));
        let sessions = sessions_for(&config);
        assert_eq!(
            refused_with(&config, &sessions, &ALL_UP, &[]),
            DEFAULT_REASON
        );
        assert_eq!(decisions(&sessions, "policy_refuse"), 1);
        assert_eq!(sessions.metrics().failures_of(LoginFailure::Extension), 1);
        let mut page = String::new();
        sessions.extensions.render(&mut page);
        assert!(
            page.contains("outcome=\"invalid\"} 1"),
            "an invalid answer is counted\n{page}"
        );
    }

    #[test]
    fn an_extension_that_is_too_slow_or_down_takes_the_policy() {
        let slow = chooser_after(Duration::from_millis(600), |_| Choice::order(["west"]));
        let config = config_with(slow.extension(ServerOnError::Default, 100));
        let sessions = sessions_for(&config);
        goes_to(&config, &sessions, &ALL_UP, &[LOBBY]);
        assert_eq!(decisions(&sessions, "policy_default"), 1);
        drop(sessions);

        let config = config_with(slow.extension(ServerOnError::Refuse, 100));
        let sessions = sessions_for(&config);
        assert_eq!(
            refused_with(&config, &sessions, &ALL_UP, &[]),
            DEFAULT_REASON
        );

        // No connection at all: an extension that never started.
        let mut config = config_with(slow.extension(ServerOnError::Refuse, 100));
        config.extensions[0].address = "127.0.0.1:1".into();
        let sessions = Sessions::default();
        assert_eq!(
            refused_with(&config, &sessions, &ALL_UP, &[]),
            DEFAULT_REASON
        );
    }

    #[test]
    fn an_extension_granted_the_point_that_did_not_declare_it_is_not_asked() {
        // It declares `decide.login` only; the operator granted `decide.server` as well.
        let gate = gate_with(Duration::ZERO, &[], |_| Decision::Default);
        let mut extension =
            gate.extension(&["decide.login", "decide.server"], OnError::Default, 1000);
        extension.decide_server.on_error = ServerOnError::Refuse;
        let config = Gate::config(extension);
        let sessions = sessions_for(&config);
        // Nothing is asked of it at the choice of server, whatever its on-error says.
        goes_to(&config, &sessions, &ALL_UP, &[LOBBY]);
        assert_eq!(decisions(&sessions, "policy_refuse"), 0);
    }

    #[test]
    fn with_balancing_the_extension_is_told_what_the_balancer_knows_of_each_server() {
        let chooser = chooser(|_| Choice::Default);
        let config = balanced_with(chooser.extension(ServerOnError::Default, 1000), 2, 3);
        let sessions = sessions_for(&config);
        let _held = hold(&sessions, &config, "lobby");
        let _more = hold(&sessions, &config, "lobby");
        sessions.balancer.drain("west");
        let (tried, _) = login_alex(&config, &sessions, &ALL_UP, None, vec![Step::ExpectClose]);
        assert_eq!(tried.len(), 1);
        let seen = chooser.seen.lock().unwrap();
        let by_name = |name: &str| {
            seen[0]
                .candidates
                .iter()
                .find(|c| c.name == name)
                .unwrap_or_else(|| panic!("{name} was not offered"))
        };
        let lobby = by_name("lobby");
        assert_eq!(lobby.role.as_deref(), Some("lobby"));
        assert_eq!(
            (lobby.players, lobby.soft_cap, lobby.hard_cap),
            (Some(2), Some(2), Some(3))
        );
        assert_eq!(by_name("east").players, Some(0));
        assert_eq!(by_name("east").state.as_deref(), Some("open"));
        assert_eq!(by_name("west").state.as_deref(), Some("draining"));
    }

    #[test]
    fn with_balancing_an_order_overrides_the_balancer_and_the_hard_cap_still_holds() {
        let chooser = chooser(|_| Choice::order(["west", "lobby"]));
        let config = balanced_with(chooser.extension(ServerOnError::Default, 1000), 2, 2);
        let sessions = sessions_for(&config);
        // The balancer fills the fullest open lobby first, which would be `lobby`; the order says west.
        let _held = hold(&sessions, &config, "lobby");
        goes_to(&config, &sessions, &ALL_UP, &[WEST]);
        // A server at its hard cap is passed over for the next of the order, without a connection.
        let _w1 = hold(&sessions, &config, "west");
        let _w2 = hold(&sessions, &config, "west");
        goes_to(&config, &sessions, &ALL_UP, &[LOBBY]);
    }

    #[test]
    fn a_server_that_is_drained_or_full_is_passed_over_and_when_none_can_the_player_is_told_it_is_full()
     {
        let chooser = chooser(|_| Choice::order(["east", "west"]));
        let config = balanced_with(chooser.extension(ServerOnError::Default, 1000), 1, 1);
        let sessions = sessions_for(&config);
        sessions.balancer.drain("east");
        goes_to(&config, &sessions, &ALL_UP, &[WEST]);
        // West is full now (a place held), east is drained: nowhere to go, and no server was tried.
        let _held = hold(&sessions, &config, "west");
        let said = refused_with(&config, &sessions, &ALL_UP, &[]);
        assert_eq!(said, Full::MESSAGE);
        assert_eq!(sessions.metrics().failures_of(LoginFailure::Full), 1);
        // Not the extension's refusal.
        assert_eq!(sessions.metrics().failures_of(LoginFailure::Extension), 0);
    }

    /// With the queue on, a player whose ordered servers are all at their hard cap waits as they do when
    /// every lobby is full, and the extension is asked again when they are woken.
    #[test]
    fn with_the_queue_a_player_waits_for_an_ordered_server_and_the_extension_is_asked_again() {
        let chooser = chooser(|_| Choice::order(["east"]));
        let mut config = balanced_with(chooser.extension(ServerOnError::Default, 1000), 1, 1);
        config.queue = meridian_config::QueueSettings {
            enabled: true,
            wait_seconds: 5,
            max_length: 5,
        };
        let sessions = sessions_for(&config);
        let held = hold(&sessions, &config, "east");
        let started = Instant::now();
        let (tried, _) = std::thread::scope(|scope| {
            let sessions = &sessions;
            scope.spawn(move || {
                while sessions.queue.len() == 0 {
                    std::thread::sleep(Duration::from_millis(5));
                }
                std::thread::sleep(Duration::from_millis(100));
                drop(held);
            });
            login_alex(&config, sessions, &ALL_UP, None, vec![Step::ExpectClose])
        });
        assert_eq!(tried, [EAST]);
        assert!(started.elapsed() < Duration::from_secs(4));
        assert_eq!(sessions.queue.len(), 0);
        assert_eq!(sessions.admitted(), 0);
        assert!(
            chooser.seen.lock().unwrap().len() >= 2,
            "asked before the wait and again after it"
        );
    }

    #[test]
    fn a_server_without_a_role_is_tried_in_the_order_among_those_that_have_one() {
        let chooser = chooser(|_| Choice::order(["east", "lobby", "west"]));
        let mut config = balanced_with(chooser.extension(ServerOnError::Default, 1000), 2, 2);
        config.balanced.retain(|b| b.backend != "east");
        let sessions = sessions_for(&config);
        // East has no role: no place is held for it, and it is tried first, as the order says.
        goes_to(&config, &sessions, &ALL_UP, &[EAST]);
        goes_to(
            &config,
            &sessions,
            &[(LOBBY, true), (EAST, false), (WEST, true)],
            &[EAST, LOBBY],
        );
    }

    #[test]
    fn the_login_and_the_choice_of_server_are_both_asked_of_one_extension() {
        let asked = Arc::new(Mutex::new(Vec::<&'static str>::new()));
        let extension = Sdk::new("gate", "1.0.0", Secret::new(TOKEN).unwrap())
            .log(Arc::new(Lines::default()) as Arc<dyn Log>)
            .on_login({
                let asked = Arc::clone(&asked);
                move |_| {
                    asked.lock().unwrap().push("login");
                    Decision::Allow
                }
            })
            .on_server({
                let asked = Arc::clone(&asked);
                move |_| {
                    asked.lock().unwrap().push("server");
                    Choice::order(["east"])
                }
            });
        let running = extension.bind("127.0.0.1:0").unwrap();
        let folder = Folder::new();
        let settings = extension_config(
            running.local_addr().to_string(),
            folder.token(),
            &["decide.login", "decide.server"],
            OnError::Default,
            1000,
        );
        let config = config_with(settings);
        let sessions = sessions_for(&config);
        goes_to(&config, &sessions, &ALL_UP, &[EAST]);
        // The login first, then the choice of server, as the order of the checks says.
        assert_eq!(*asked.lock().unwrap(), ["login", "server"]);
    }

    // ---- when a server is lost ------------------------------------------------------------------

    /// The replacer's answer to the loss of `lobby`, and the addresses it tried.
    fn lose_lobby(
        config: &Config,
        sessions: &Sessions,
        up: &[(&str, bool)],
    ) -> (Result<Replacement, Notice>, Vec<String>) {
        let (connect, backends) = reachable(up.to_vec());
        let (_relay, handle, _player, _server) = idle_session_on("Alex", "lobby");
        let profile = Profile {
            name: "Alex".into(),
            uuid: offline_uuid("Alex").to_be_bytes(),
            properties: Vec::new(),
        };
        let mut replace = replacer(Context {
            sessions,
            connect: &*connect,
            config,
            handle: &handle,
            profile: &profile,
            virtual_host: Some("play.example.org"),
            auth: AuthKind::Unverified,
            peer: peer(),
        });
        let answer = replace(&Loss::Crashed("the connection closed".into()));
        let tried = std::mem::take(&mut *backends.tried.lock().unwrap());
        (answer, tried)
    }

    #[test]
    fn at_the_fallback_the_extension_is_told_which_server_was_lost_and_orders_the_others() {
        let chooser = chooser(|_| Choice::order(["west"]));
        let config = config_with(chooser.extension(ServerOnError::Default, 1000));
        let sessions = sessions_for(&config);
        let (answer, tried) = lose_lobby(&config, &sessions, &ALL_UP);
        assert_eq!(tried, [WEST]);
        assert_eq!(answer.unwrap().backend, "west");
        let seen = chooser.seen.lock().unwrap();
        let request = &seen[0];
        assert_eq!(request.reason, Placing::Fallback);
        assert_eq!(request.lost.as_deref(), Some("lobby"));
        assert_eq!(request.host.as_deref(), Some("play.example.org"));
        // The server that lost the player is not offered again.
        let names: Vec<&str> = request.candidates.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["east", "west"]);
    }

    #[test]
    fn at_the_fallback_default_is_the_normal_choice_and_a_refusal_disconnects_with_its_reason() {
        let first = chooser(|_| Choice::Default);
        let config = config_with(first.extension(ServerOnError::Default, 1000));
        let sessions = sessions_for(&config);
        let (answer, tried) = lose_lobby(&config, &sessions, &ALL_UP);
        assert_eq!(tried, [EAST]);
        assert_eq!(answer.unwrap().backend, "east");

        let second = chooser(|_| Choice::refuse("No game is running."));
        let config = config_with(second.extension(ServerOnError::Default, 1000));
        let sessions = sessions_for(&config);
        let (answer, tried) = lose_lobby(&config, &sessions, &ALL_UP);
        assert!(tried.is_empty(), "no server was tried: {tried:?}");
        let notice = answer
            .err()
            .expect("the player is disconnected")
            .to_string();
        assert!(notice.contains("No game is running."), "{notice}");
        assert_eq!(decisions(&sessions, "refuse"), 1);
    }

    /// A player who lost their server is waiting in the configuration phase: the extension has half a
    /// second at most, whatever `timeout-ms` says.
    #[test]
    fn at_the_fallback_the_wait_is_cut_to_half_a_second() {
        let slow = chooser_after(Duration::from_millis(2500), |_| Choice::order(["west"]));
        let config = config_with(slow.extension(ServerOnError::Default, 5000));
        let sessions = sessions_for(&config);
        let began = Instant::now();
        let (answer, tried) = lose_lobby(&config, &sessions, &ALL_UP);
        let took = began.elapsed();
        // Not the 5000 ms of the configuration, nor the 2500 ms the extension takes: 500 ms.
        assert!(
            took >= Duration::from_millis(450) && took < Duration::from_millis(1200),
            "waited {took:?}"
        );
        // No answer in time: the policy, `default`, so the first of the others.
        assert_eq!(tried, [EAST]);
        assert_eq!(answer.unwrap().backend, "east");
        assert_eq!(decisions(&sessions, "policy_default"), 1);
    }

    /// At the fallback the player is already in the game: an extension that is silent, slow or wrong,
    /// with `on-error = "refuse"`, does not disconnect them; they are placed as without it. Only an
    /// explicit `refuse` of the extension disconnects them.
    #[test]
    fn at_the_fallback_a_failure_of_the_extension_never_refuses_but_its_own_refusal_does() {
        // Slow: past the deadline.
        let slow = chooser_after(Duration::from_millis(900), |_| Choice::refuse("No."));
        let config = config_with(slow.extension(ServerOnError::Refuse, 100));
        let sessions = sessions_for(&config);
        let (answer, tried) = lose_lobby(&config, &sessions, &ALL_UP);
        assert_eq!(tried, [EAST]);
        assert_eq!(answer.unwrap().backend, "east");
        assert_eq!(decisions(&sessions, "policy_default"), 1);
        assert_eq!(decisions(&sessions, "policy_refuse"), 0);
        drop(sessions);

        // Invalid: a server that was not offered.
        let wrong = chooser(|_| Choice::order(["north"]));
        let config = config_with(wrong.extension(ServerOnError::Refuse, 1000));
        let sessions = sessions_for(&config);
        let (answer, tried) = lose_lobby(&config, &sessions, &ALL_UP);
        assert_eq!(
            (tried, answer.unwrap().backend),
            (vec![EAST.to_owned()], "east".to_owned())
        );
        drop(sessions);

        // Silent: the extension is not running at all.
        let mut config = config_with(wrong.extension(ServerOnError::Refuse, 1000));
        config.extensions[0].address = "127.0.0.1:1".into();
        let sessions = Sessions::default();
        let (answer, tried) = lose_lobby(&config, &sessions, &ALL_UP);
        assert_eq!(
            (tried, answer.unwrap().backend),
            (vec![EAST.to_owned()], "east".to_owned())
        );

        // An explicit refusal is the extension's decision, and the player is disconnected with it.
        let says_no = chooser(|_| Choice::refuse("No game is running."));
        let config = config_with(says_no.extension(ServerOnError::Refuse, 1000));
        let sessions = sessions_for(&config);
        let (answer, tried) = lose_lobby(&config, &sessions, &ALL_UP);
        assert!(tried.is_empty());
        let notice = answer.err().expect("disconnected").to_string();
        assert!(notice.contains("No game is running."), "{notice}");
    }

    /// An order may send a player to a server at its soft cap, up to the hard cap: the extension decides
    /// where the players go, and the balancer's soft cap and margin are its own rule.
    #[test]
    fn an_order_may_fill_a_server_past_its_soft_cap_up_to_the_hard_cap() {
        let chooser = chooser(|_| Choice::order(["east"]));
        let config = balanced_with(chooser.extension(ServerOnError::Default, 1000), 1, 2);
        let sessions = sessions_for(&config);
        // East has one player: at its soft cap, so the balancer would call it closed.
        let _held = hold(&sessions, &config, "east");
        goes_to(&config, &sessions, &ALL_UP, &[EAST]);
        // The extension was told so, and went on.
        assert_eq!(
            chooser.seen.lock().unwrap()[0]
                .candidates
                .iter()
                .find(|c| c.name == "east")
                .and_then(|c| c.state.clone())
                .as_deref(),
            Some("closed")
        );
        // At the hard cap it is full, whatever the order says.
        let _second = hold(&sessions, &config, "east");
        let said = refused_with(&config, &sessions, &ALL_UP, &[]);
        assert_eq!(said, Full::MESSAGE);
    }
}
