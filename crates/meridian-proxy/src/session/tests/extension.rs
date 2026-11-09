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
        own_commands: Default::default(),
        orders: Default::default(),
        events: Default::default(),
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
// The SDK's example `allow-all`, run as a separate process (docs/EXTENSIONS.md, 8.2)

mod allow_all;

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

    pub(super) const LOBBY: &str = "127.0.0.1:25601";
    pub(super) const EAST: &str = "127.0.0.1:25602";
    pub(super) const WEST: &str = "127.0.0.1:25603";

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
    pub(super) fn login_alex(
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

    pub(super) const ALL_UP: [(&str, bool); 3] = [(LOBBY, true), (EAST, true), (WEST, true)];

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
        // What east last said of itself, as a round of readings keeps it.
        let east_said = meridian_sidecar::request::ServerStatus {
            players: Some(0),
            capacity: Some(40),
            stopping: Some(false),
            ..Default::default()
        };
        sessions
            .statuses
            .record("east", east_said, std::time::Instant::now());
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
        // Why west is kept from players; nothing for the open ones.
        assert_eq!(
            by_name("west").why,
            Some(meridian_extension::decide::Why::Drained)
        );
        assert_eq!((by_name("east").why, lobby.why), (None, None));
        // East's report, as the round kept it; none for a server no round read, and no `up` without a round.
        let report = by_name("east").report.expect("east's reading");
        assert_eq!((report.capacity, report.stopping), (Some(40), Some(false)));
        assert_eq!(
            (lobby.report, lobby.up, by_name("east").up),
            (None, None, None)
        );
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
// ---------------------------------------------------------------------------------------------
// The permission of a command (docs/EXTENSIONS.md, 4.3)

mod command {
    use super::*;
    use crate::commands::{self, Answer, Typist};
    use crate::extension::command::Gate;
    use meridian_config::{
        AuthKind, CommandDecision, CommandSettings, Permissions, ProxyCommand, Role,
    };
    use meridian_extension::decide::{CommandRequest, NodeAnswers, NodeVerdict};
    use meridian_net::relay::CommandHook;

    /// An extension that decides permissions, with what it was asked.
    struct Ranker {
        running: Running,
        folder: Folder,
        seen: Arc<Mutex<Vec<CommandRequest>>>,
    }

    fn ranker(decide: impl Fn(&CommandRequest) -> NodeAnswers + Send + Sync + 'static) -> Ranker {
        ranker_after(Duration::ZERO, decide)
    }

    fn ranker_after(
        delay: Duration,
        decide: impl Fn(&CommandRequest) -> NodeAnswers + Send + Sync + 'static,
    ) -> Ranker {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let extension = Sdk::new("gate", "1.0.0", Secret::new(TOKEN).unwrap())
            .log(Arc::new(Lines::default()) as Arc<dyn Log>)
            .on_command({
                let seen = Arc::clone(&seen);
                move |request| {
                    seen.lock().unwrap().push(request.clone());
                    std::thread::sleep(delay);
                    decide(request)
                }
            });
        Ranker {
            running: extension.bind("127.0.0.1:0").unwrap(),
            folder: Folder::new(),
            seen,
        }
    }

    impl Ranker {
        fn extension(&self, on_error: OnError, timeout_ms: u64, cache_max: u64) -> ExtensionConfig {
            let mut config = extension_config(
                self.running.local_addr().to_string(),
                self.folder.token(),
                &["decide.command"],
                OnError::Default,
                1000,
            );
            config.decide_command = CommandDecision {
                timeout_ms,
                on_error,
                cache_seconds_max: cache_max,
            };
            config
        }

        fn asked(&self) -> usize {
            self.seen.lock().unwrap().len()
        }
    }

    /// All six commands on, and no extension.
    pub(super) fn six_commands() -> Config {
        Config {
            commands: CommandSettings {
                enabled: vec![
                    ProxyCommand::Server,
                    ProxyCommand::Hub,
                    ProxyCommand::Play,
                    ProxyCommand::Send,
                    ProxyCommand::Glist,
                    ProxyCommand::Alert,
                ],
                ..CommandSettings::default()
            },
            ..routed()
        }
    }

    /// All six commands on, and the extension.
    fn config_with(extension: ExtensionConfig) -> Config {
        Config {
            extensions: vec![extension],
            ..six_commands()
        }
    }

    /// Gives Alex the staff nodes, and nobody else.
    pub(super) const FILE: &str = r#"
[group.staff]
allow = ["meridian.send", "meridian.glist", "meridian.alert"]
[[player]]
name = "Alex"
groups = ["staff"]
"#;

    /// Denies `/server` to everyone, and gives nothing.
    const FILE_CLOSED: &str = r#"
[group.nobody]
deny = ["meridian.server", "meridian.hub"]
[[player]]
name = "Alex"
groups = ["nobody"]
"#;

    /// The hook of Alex's session, with the extension of `config` in `sessions`; what the staff closure
    /// was asked is kept.
    pub(super) fn hook_of(
        config: &Config,
        sessions: &Sessions,
        file: &str,
    ) -> (Arc<CommandHook>, Arc<Mutex<Vec<String>>>) {
        let ran = Arc::new(Mutex::new(Vec::new()));
        let permissions = Arc::new(Permissions::parse(file).unwrap());
        let hook = {
            let ran = Arc::clone(&ran);
            let live = Arc::new(config.clone());
            commands::hook_with(
                Arc::new(config.clone()),
                move || Typist {
                    auth: AuthKind::Unverified,
                    backend: "lobby".into(),
                    name: "Alex".into(),
                    uuid: [1; 16],
                    permissions: Arc::clone(&permissions),
                    operator_groups: Vec::new(),
                },
                |target: &str| format!("Moving you to {target}..."),
                |_: Role, _: &Typist| "Moving you to the lobby...".to_owned(),
                move |_: &Typist, answer: Answer| {
                    ran.lock().unwrap().push(format!("{answer:?}"));
                    "done".to_owned()
                },
                Some(Arc::new(Gate::new(
                    Arc::clone(&sessions.extensions),
                    move || Arc::clone(&live),
                ))),
                None,
            )
        };
        (hook, ran)
    }

    fn decisions(sessions: &Sessions, decision: &'static str) -> u64 {
        sessions
            .extensions
            .link("gate")
            .unwrap()
            .stats()
            .decisions("command", decision)
    }

    pub(super) const DENIED_SEND: &str = "You do not have permission to use /send.";
    const DENIED_SERVER: &str = "You do not have permission to use /server.";

    // ---- the rule, and who is asked --------------------------------------------------------------

    #[test]
    fn with_no_extension_the_file_decides_and_nothing_is_asked() {
        let config = six_commands();
        assert!(config.extensions.is_empty());
        let sessions = Sessions::default();
        let (hook, ran) = hook_of(&config, &sessions, FILE);
        assert_eq!(hook("send all east").as_deref(), Some("done"));
        assert_eq!(ran.lock().unwrap().len(), 1);
        let (hook, _) = hook_of(&config, &sessions, "");
        assert_eq!(hook("send all east").as_deref(), Some(DENIED_SEND));
        assert!(sessions.extensions.links().is_empty());
    }

    #[test]
    fn an_extension_that_is_disabled_or_not_granted_or_did_not_declare_is_not_asked() {
        let ranker = ranker(|request| NodeAnswers::all(request, NodeVerdict::Deny));
        // Switched off.
        let mut off = ranker.extension(OnError::Deny, 1000, 0);
        off.enabled = false;
        let config = config_with(off);
        let sessions = sessions_for(&config);
        let (hook, _) = hook_of(&config, &sessions, FILE);
        assert_eq!(hook("send all east").as_deref(), Some("done"));
        // Not granted `decide.command`: it holds another point.
        let mut other = ranker.extension(OnError::Deny, 1000, 0);
        other.grant = vec!["decide.login".into()];
        let config = config_with(other);
        let sessions = sessions_for(&config);
        let (hook, _) = hook_of(&config, &sessions, FILE);
        assert_eq!(hook("send all east").as_deref(), Some("done"));
        assert_eq!(ranker.asked(), 0);
        drop(sessions);
        // Granted, and the extension never declared it (it holds the login only).
        let gate = gate_with(Duration::ZERO, &[], |_| Decision::Default);
        let mut undeclared = extension_config(
            gate.running.local_addr().to_string(),
            gate.folder.token(),
            &["decide.login", "decide.command"],
            OnError::Default,
            1000,
        );
        undeclared.decide_command.on_error = OnError::Deny;
        let config = config_with(undeclared);
        let sessions = sessions_for(&config);
        let (hook, _) = hook_of(&config, &sessions, FILE);
        assert_eq!(
            hook("send all east").as_deref(),
            Some("done"),
            "nothing is asked of it, whatever its on-error says"
        );
    }

    #[test]
    fn default_leaves_the_question_to_the_file_and_the_operators() {
        let ranker = ranker(|_| NodeAnswers::new());
        let config = config_with(ranker.extension(OnError::Default, 1000, 0));
        let sessions = sessions_for(&config);
        // The file gives Alex /send, and does not give it to others.
        let (hook, _) = hook_of(&config, &sessions, FILE);
        assert_eq!(hook("send all east").as_deref(), Some("done"));
        let (hook, _) = hook_of(&config, &sessions, "");
        assert_eq!(hook("send all east").as_deref(), Some(DENIED_SEND));
        assert_eq!(decisions(&sessions, "default"), 2);
    }

    /// U2: the extension is the source of rights. Its `allow` gives a node the file refuses.
    #[test]
    fn an_allow_gives_what_the_file_refuses() {
        let ranker = ranker(|request| NodeAnswers::all(request, NodeVerdict::Allow));
        let config = config_with(ranker.extension(OnError::Default, 1000, 0));
        let sessions = sessions_for(&config);
        // The file gives Alex nothing, and the staff closure is reached all the same.
        let (hook, ran) = hook_of(&config, &sessions, "");
        assert_eq!(hook("send all east").as_deref(), Some("done"));
        assert_eq!(hook("alert hello there").as_deref(), Some("done"));
        assert_eq!(ran.lock().unwrap().len(), 2);
        // Even a /server the file denies to everyone.
        let (hook, _) = hook_of(&config, &sessions, FILE_CLOSED);
        assert_eq!(
            hook("server east").as_deref(),
            Some("Moving you to east...")
        );
        assert_eq!(decisions(&sessions, "allow"), 3);
    }

    /// U2: its `deny` takes a node the file gives.
    #[test]
    fn a_deny_takes_what_the_file_gives() {
        let ranker = ranker(|request| NodeAnswers::all(request, NodeVerdict::Deny));
        let config = config_with(ranker.extension(OnError::Default, 1000, 0));
        let sessions = sessions_for(&config);
        let (hook, ran) = hook_of(&config, &sessions, FILE);
        assert_eq!(hook("send all east").as_deref(), Some(DENIED_SEND));
        assert!(ran.lock().unwrap().is_empty(), "the command did not run");
        // /server is open to everyone until a group denies it: the extension denies it.
        let (hook, _) = hook_of(&config, &sessions, "");
        assert_eq!(hook("server east").as_deref(), Some(DENIED_SERVER));
        assert_eq!(decisions(&sessions, "deny"), 2);
    }

    #[test]
    fn a_verdict_is_for_one_node_and_the_others_stay_the_files() {
        // It takes the way to `east` and says nothing of the rest.
        let ranker = ranker(|_| NodeAnswers::new().deny("meridian.server.east"));
        let config = config_with(ranker.extension(OnError::Default, 1000, 0));
        let sessions = sessions_for(&config);
        let (hook, _) = hook_of(&config, &sessions, "");
        assert_eq!(
            hook("server east").as_deref(),
            Some("You do not have permission to go to east.")
        );
        assert_eq!(
            hook("server west").as_deref(),
            Some("Moving you to west...")
        );
        // The list is made of the servers whose node is not denied.
        assert_eq!(
            hook("server").as_deref(),
            Some("Servers: lobby (default), west. You are on lobby.")
        );
    }

    #[test]
    fn a_command_that_checks_several_nodes_is_one_request_that_names_them_all() {
        let ranker = ranker(|_| NodeAnswers::new());
        let config = config_with(ranker.extension(OnError::Default, 1000, 0));
        let sessions = sessions_for(&config);
        let (hook, _) = hook_of(&config, &sessions, "");
        let _ = hook("server");
        let _ = hook("server east");
        let _ = hook("hub");
        let seen = ranker.seen.lock().unwrap();
        assert_eq!(seen.len(), 3, "one request for each command");
        assert_eq!(
            seen[0].nodes,
            [
                "meridian.server",
                "meridian.server.lobby",
                "meridian.server.east",
                "meridian.server.west"
            ]
        );
        assert_eq!(seen[1].nodes, ["meridian.server", "meridian.server.east"]);
        assert_eq!(seen[2].nodes, ["meridian.hub", "meridian.server.lobby"]);
        assert_eq!(
            (seen[1].command.as_str(), seen[1].args.as_slice()),
            ("server", ["east".to_owned()].as_slice())
        );
        assert_eq!(seen[1].server, "lobby");
        assert_eq!(seen[1].player.name, "Alex");
        assert_eq!(seen[1].player.auth, decide::Auth::Unverified);
    }

    #[test]
    fn a_command_the_proxy_does_not_answer_never_reaches_the_extension() {
        let ranker = ranker(|request| NodeAnswers::all(request, NodeVerdict::Deny));
        let config = config_with(ranker.extension(OnError::Deny, 1000, 0));
        let sessions = sessions_for(&config);
        let (hook, _) = hook_of(&config, &sessions, FILE);
        for command in [
            "tell Bob hi",
            "gamemode creative",
            "",
            "   ",
            "servers",
            "help",
        ] {
            assert_eq!(hook(command), None, "{command:?}");
        }
        assert_eq!(ranker.asked(), 0);
        // A command the proxy answers is turned off: it is the server's, and is not asked either.
        let mut config = config_with(ranker.extension(OnError::Deny, 1000, 0));
        config.commands.enabled.retain(|c| *c != ProxyCommand::Send);
        let sessions = Sessions::default();
        let (hook, _) = hook_of(&config, &sessions, FILE);
        assert_eq!(hook("send all east"), None);
    }

    #[test]
    fn what_the_extension_is_told_of_the_arguments_is_bounded() {
        let ranker = ranker(|_| NodeAnswers::new());
        let config = config_with(ranker.extension(OnError::Default, 1000, 0));
        let sessions = sessions_for(&config);
        let (hook, _) = hook_of(&config, &sessions, FILE);
        let long = "x".repeat(300);
        let many = (0..40)
            .map(|i| format!("w{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let _ = hook(&format!("alert {long} {many}"));
        let seen = ranker.seen.lock().unwrap();
        assert_eq!(seen[0].args.len(), decide::MAX_COMMAND_ARGS);
        assert_eq!(seen[0].args[0].chars().count(), decide::MAX_ARG_CHARS);
    }

    // ---- when there is no usable answer ----------------------------------------------------------

    #[test]
    fn with_no_usable_answer_the_on_error_policy_decides() {
        // Slow: past the deadline.
        let slow = ranker_after(Duration::from_millis(600), |request| {
            NodeAnswers::all(request, NodeVerdict::Deny)
        });
        for (policy, label, expected_without_file, expected_with_file) in [
            // default: the file.
            (OnError::Default, "policy_default", DENIED_SEND, "done"),
            // allow cannot be configured; if it got here it is `default`, and never a permission.
            (OnError::Allow, "policy_default", DENIED_SEND, "done"),
            // deny: refused.
            (OnError::Deny, "policy_deny", DENIED_SEND, DENIED_SEND),
        ] {
            let config = config_with(slow.extension(policy, 100, 0));
            let sessions = sessions_for(&config);
            let (hook, _) = hook_of(&config, &sessions, "");
            let began = Instant::now();
            assert_eq!(
                hook("send all east").as_deref(),
                Some(expected_without_file),
                "{label}"
            );
            // The player waited the deadline, not the extension's 600 ms.
            assert!(
                began.elapsed() < Duration::from_millis(450),
                "{label}: {:?}",
                began.elapsed()
            );
            let (hook, _) = hook_of(&config, &sessions, FILE);
            assert_eq!(
                hook("send all east").as_deref(),
                Some(expected_with_file),
                "{label}"
            );
            assert_eq!(decisions(&sessions, label), 2, "{label}");
            drop(sessions);
        }
    }

    #[test]
    fn an_extension_that_is_not_running_takes_the_policy_too() {
        let ranker = ranker(|_| NodeAnswers::new());
        for (policy, expected) in [
            (OnError::Default, DENIED_SEND),
            (OnError::Allow, DENIED_SEND),
            (OnError::Deny, DENIED_SEND),
        ] {
            let mut extension = ranker.extension(policy, 100, 0);
            extension.address = "127.0.0.1:1".into();
            let config = config_with(extension);
            // The registry never started it.
            let sessions = Sessions::default();
            let (hook, _) = hook_of(&config, &sessions, "");
            assert_eq!(hook("send all east").as_deref(), Some(expected));
        }
        assert_eq!(ranker.asked(), 0);
    }

    #[test]
    fn an_open_breaker_spares_the_wait_and_takes_the_policy_at_once() {
        let slow = ranker_after(Duration::from_millis(800), |_| NodeAnswers::new());
        let mut settings = slow.extension(OnError::Deny, 100, 0);
        settings.breaker_failures = 2;
        let config = config_with(settings);
        let sessions = sessions_for(&config);
        let (hook, _) = hook_of(&config, &sessions, FILE);
        // Two commands that miss their deadline open the breaker.
        assert_eq!(hook("send all east").as_deref(), Some(DENIED_SEND));
        assert_eq!(hook("send all east").as_deref(), Some(DENIED_SEND));
        assert_eq!(slow.asked(), 2);
        // Then the next ones take the policy at once, with no request and no wait.
        let began = Instant::now();
        for _ in 0..5 {
            assert_eq!(hook("send all east").as_deref(), Some(DENIED_SEND));
        }
        assert!(
            began.elapsed() < Duration::from_millis(80),
            "{:?}",
            began.elapsed()
        );
        assert_eq!(
            slow.asked(),
            2,
            "nothing was sent while the breaker is open"
        );
        assert_eq!(decisions(&sessions, "policy_deny"), 7);
    }

    // ---- what is kept ----------------------------------------------------------------------------

    #[test]
    fn what_the_extension_lets_the_proxy_keep_is_not_asked_again() {
        let ranker =
            ranker(|request| NodeAnswers::all(request, NodeVerdict::Allow).cache_seconds(60));
        let config = config_with(ranker.extension(OnError::Default, 1000, 300));
        let sessions = sessions_for(&config);
        let (hook, _) = hook_of(&config, &sessions, "");
        for _ in 0..10 {
            assert_eq!(hook("send all east").as_deref(), Some("done"));
        }
        assert_eq!(ranker.asked(), 1, "ten commands, one request");
        assert_eq!(decisions(&sessions, "allow"), 1);
        assert_eq!(decisions(&sessions, "cached"), 9);
        // Another node is another question; the first stays answered.
        assert_eq!(hook("alert hi").as_deref(), Some("done"));
        assert_eq!(hook("send all east").as_deref(), Some("done"));
        assert_eq!(ranker.asked(), 2);
        // Only the nodes that were not kept are asked: `/server east` has two, one is kept by now.
        let _ = hook("server east");
        let _ = hook("server east");
        assert_eq!(ranker.asked(), 3);
        assert_eq!(ranker.seen.lock().unwrap()[2].nodes.len(), 2);
    }

    #[test]
    fn nothing_is_kept_unless_the_extension_says_so_and_never_longer_than_the_maximum() {
        // No `cache_seconds`: asked each time.
        let none = ranker(|request| NodeAnswers::all(request, NodeVerdict::Allow));
        let config = config_with(none.extension(OnError::Default, 1000, 300));
        let sessions = sessions_for(&config);
        let (hook, _) = hook_of(&config, &sessions, "");
        for _ in 0..3 {
            let _ = hook("send all east");
        }
        assert_eq!(none.asked(), 3);
        // The hook holds the registry, which holds the connection: both go before the next test of it.
        drop(hook);
        drop(sessions);
        // A maximum of 0: the extension asks for a minute and the operator will not have it.
        let greedy =
            ranker(|request| NodeAnswers::all(request, NodeVerdict::Allow).cache_seconds(3600));
        let config = config_with(greedy.extension(OnError::Default, 1000, 0));
        let sessions = sessions_for(&config);
        let (hook, _) = hook_of(&config, &sessions, "");
        for _ in 0..3 {
            let _ = hook("send all east");
        }
        assert_eq!(greedy.asked(), 3);
        drop(hook);
        drop(sessions);
        // A maximum of 1 second: kept for a second, though the extension asked for an hour.
        let config = config_with(greedy.extension(OnError::Default, 1000, 1));
        let sessions = sessions_for(&config);
        let (hook, _) = hook_of(&config, &sessions, "");
        let before = greedy.asked();
        let _ = hook("send all east");
        let _ = hook("send all east");
        assert_eq!(greedy.asked(), before + 1);
        std::thread::sleep(Duration::from_millis(1100));
        let _ = hook("send all east");
        assert_eq!(greedy.asked(), before + 2, "asked again after the maximum");
    }

    // ---- the extension asks the proxy to forget what it kept ------------------------------------

    const ALEX: &str = "01010101-0101-0101-0101-010101010101";

    /// The three tests below: an extension whose answers are kept for a minute, Alex's hook, the link.
    fn kept_for_a_minute() -> (
        Ranker,
        Sessions,
        Arc<CommandHook>,
        Arc<crate::extension::Link>,
    ) {
        let ranker =
            ranker(|request| NodeAnswers::all(request, NodeVerdict::Allow).cache_seconds(60));
        let config = config_with(ranker.extension(OnError::Default, 1000, 300));
        let sessions = sessions_for(&config);
        let (hook, _) = hook_of(&config, &sessions, "");
        let link = sessions.extensions.link("gate").unwrap();
        (ranker, sessions, hook, link)
    }

    #[test]
    fn when_the_extension_asks_to_forget_everyone_what_was_kept_is_asked_again() {
        let (ranker, _sessions, hook, link) = kept_for_a_minute();
        for _ in 0..3 {
            let _ = hook("send all east");
        }
        assert_eq!(ranker.asked(), 1);
        link.forget_all();
        let _ = hook("send all east");
        let _ = hook("send all east");
        assert_eq!(ranker.asked(), 2, "asked again once, then kept again");
    }

    #[test]
    fn when_the_extension_asks_to_forget_one_player_only_that_players_answers_go() {
        let (ranker, _sessions, hook, link) = kept_for_a_minute();
        let _ = hook("send all east");
        assert_eq!(ranker.asked(), 1);
        // Someone else: Alex keeps what was kept.
        link.forget_player("02020202-0202-0202-0202-020202020202".into());
        let _ = hook("send all east");
        assert_eq!(ranker.asked(), 1);
        // Alex, in capitals as a program may write it: asked again, then kept again.
        link.forget_player(ALEX.to_ascii_uppercase());
        let _ = hook("send all east");
        let _ = hook("send all east");
        assert_eq!(ranker.asked(), 2);
        // More requests than the proxy keeps count of, none of them for Alex: it cannot tell, so it forgets.
        for n in 0..300 {
            link.forget_player(format!("{n:08x}-0000-0000-0000-000000000000"));
        }
        let _ = hook("send all east");
        let _ = hook("send all east");
        assert_eq!(ranker.asked(), 3);
    }

    /// One extension's word is not another's: the orders of an extension that does not decide the permissions
    /// are refused, and what an extension that has no say forgets is not what the one that has kept.
    #[test]
    fn what_another_extension_forgets_is_not_what_the_one_that_decides_has_kept() {
        let ranker =
            ranker(|request| NodeAnswers::all(request, NodeVerdict::Allow).cache_seconds(60));
        let other = gate();
        let mut other_config = other.extension(&["decide.login"], OnError::Default, 1000);
        other_config.name = "other".into();
        let mut config = config_with(ranker.extension(OnError::Default, 1000, 300));
        config.extensions.push(other_config);
        let sessions = sessions_for(&config);
        let (hook, _) = hook_of(&config, &sessions, "");
        let _ = hook("send all east");
        assert_eq!(ranker.asked(), 1);
        // The other extension forgets everyone, and Alex: what the first kept is kept.
        let theirs = sessions.extensions.link("other").unwrap();
        theirs.forget_all();
        theirs.forget_player(ALEX.into());
        for _ in 0..300 {
            theirs.forget_player("02020202-0202-0202-0202-020202020202".into());
        }
        let _ = hook("send all east");
        assert_eq!(
            ranker.asked(),
            1,
            "the verdicts of the extension that decides were kept"
        );
        // And the one that decides forgetting is what makes the proxy ask again.
        sessions.extensions.link("gate").unwrap().forget_all();
        let _ = hook("send all east");
        assert_eq!(ranker.asked(), 2);
    }

    /// The whole way, with the SDK: the extension gives the order with its `Proxy` handle, over the real
    /// connection, and the proxy forgets.
    #[test]
    fn an_extension_that_gives_the_order_with_its_sdk_makes_the_proxy_forget() {
        let (ranker, _sessions, hook, _link) = kept_for_a_minute();
        let _ = hook("send all east");
        let _ = hook("send all east");
        assert_eq!(ranker.asked(), 1);
        let proxy = ranker.running.proxy();
        // One player, then everyone; each time the proxy asks again, once.
        assert_eq!(proxy.invalidate_player(ALEX), Ok(()));
        let _ = hook("send all east");
        let _ = hook("send all east");
        assert_eq!(ranker.asked(), 2);
        assert_eq!(proxy.invalidate_all(), Ok(()));
        let _ = hook("send all east");
        let _ = hook("send all east");
        assert_eq!(ranker.asked(), 3);
        // Someone else: what is kept for Alex stays.
        assert_eq!(
            proxy.invalidate_player("02020202-0202-0202-0202-020202020202"),
            Ok(())
        );
        let _ = hook("send all east");
        assert_eq!(ranker.asked(), 3);
    }

    /// The refusals of the real dispatcher, as the SDK of the extension tells them: the capability the operator did not
    /// grant is the real `-32601`, and a spent budget is the real `-32001`, which a moment later is not any more.
    #[test]
    fn what_the_dispatcher_refuses_reaches_the_sdk_as_what_it_is() {
        // The extension asks for `decide.command` (it has a handler for it) and the operator grants it `orders.read`
        // only: its order is not one it may give.
        let ranker = ranker(|request| NodeAnswers::all(request, NodeVerdict::Allow));
        let mut ungranted = ranker.extension(OnError::Default, 1000, 300);
        ungranted.grant = vec!["orders.read".into()];
        let config = config_with(ungranted);
        let sessions = sessions_for(&config);
        let proxy = ranker.running.proxy();
        let limit = Instant::now() + Duration::from_secs(10);
        while !proxy.is_connected() {
            assert!(Instant::now() < limit, "the extension never connected");
            std::thread::sleep(Duration::from_millis(5));
        }
        let refused = proxy.invalidate_all().unwrap_err();
        assert!(refused.is_not_granted(), "{refused}");
        assert!(
            !refused.retry_later() && !refused.may_have_run(),
            "{refused}"
        );
        drop(sessions);

        // Granted, with a budget of one order and a rate of one a second: the second is over it.
        let ranker = self::ranker(|request| NodeAnswers::all(request, NodeVerdict::Allow));
        let mut tight = ranker.extension(OnError::Default, 1000, 300);
        tight.orders = meridian_config::Orders {
            rate_per_second: 1,
            burst: 1,
            max_running: 8,
            max_kicks_per_minute: 0,
        };
        let config = config_with(tight);
        let sessions = sessions_for(&config);
        let proxy = ranker.running.proxy();
        let limit = Instant::now() + Duration::from_secs(10);
        while !proxy.is_connected() {
            assert!(Instant::now() < limit, "the extension never connected");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(proxy.invalidate_all(), Ok(()));
        let over = proxy.invalidate_all().unwrap_err();
        assert!(over.retry_later() && !over.may_have_run(), "{over}");
        assert!(over.to_string().contains("-32001"), "{over}");
        // A moment later the budget has a token again.
        std::thread::sleep(Duration::from_millis(1200));
        assert_eq!(proxy.invalidate_all(), Ok(()));
        drop(sessions);
    }

    #[test]
    fn an_answer_that_was_on_its_way_when_the_extension_asked_to_forget_the_player_is_not_kept() {
        let ranker = ranker_after(Duration::from_millis(300), |request| {
            NodeAnswers::all(request, NodeVerdict::Allow).cache_seconds(60)
        });
        let config = config_with(ranker.extension(OnError::Default, 2000, 300));
        let sessions = sessions_for(&config);
        let (hook, _) = hook_of(&config, &sessions, "");
        let link = sessions.extensions.link("gate").unwrap();
        let asking = {
            let hook = Arc::clone(&hook);
            std::thread::spawn(move || hook("send all east"))
        };
        let limit = Instant::now() + Duration::from_secs(10);
        while ranker.asked() == 0 {
            assert!(Instant::now() < limit, "the extension was never asked");
            std::thread::sleep(Duration::from_millis(5));
        }
        // The extension is deciding; it asks to forget the player before its answer comes.
        link.forget_player(ALEX.into());
        assert_eq!(asking.join().unwrap().as_deref(), Some("done"));
        let _ = hook("send all east");
        assert_eq!(ranker.asked(), 2, "the answer given before was not kept");
    }

    #[test]
    fn a_failure_is_never_kept_and_a_denial_is_kept_like_an_allow() {
        // Slow once: the policy, not kept; then it answers and the answer is kept.
        let calls = Arc::new(AtomicUsize::new(0));
        let ranker = ranker_after(Duration::ZERO, {
            let calls = Arc::clone(&calls);
            move |request| {
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    std::thread::sleep(Duration::from_millis(400));
                }
                NodeAnswers::all(request, NodeVerdict::Deny).cache_seconds(60)
            }
        });
        let config = config_with(ranker.extension(OnError::Default, 100, 300));
        let sessions = sessions_for(&config);
        let (hook, _) = hook_of(&config, &sessions, FILE);
        // The first misses its deadline: the policy is `default`, the file gives Alex /send, and nothing
        // is kept.
        assert_eq!(hook("send all east").as_deref(), Some("done"));
        std::thread::sleep(Duration::from_millis(400));
        // The second is answered `deny` and kept; the third is not asked.
        assert_eq!(hook("send all east").as_deref(), Some(DENIED_SEND));
        assert_eq!(hook("send all east").as_deref(), Some(DENIED_SEND));
        assert_eq!(ranker.asked(), 2);
    }

    #[test]
    fn what_is_kept_is_dropped_at_a_reload_and_when_the_extension_says_hello_again() {
        let asked = Arc::new(AtomicUsize::new(0));
        let make = |asked: &Arc<AtomicUsize>| {
            let asked = Arc::clone(asked);
            Sdk::new("gate", "1.0.0", Secret::new(TOKEN).unwrap())
                .log(Arc::new(Lines::default()) as Arc<dyn Log>)
                .on_command(move |request| {
                    asked.fetch_add(1, Ordering::SeqCst);
                    NodeAnswers::all(request, NodeVerdict::Allow).cache_seconds(60)
                })
        };
        let first = make(&asked).bind("127.0.0.1:0").unwrap();
        let address = first.local_addr();
        let folder = Folder::new();
        let mut settings = extension_config(
            address.to_string(),
            folder.token(),
            &["decide.command"],
            OnError::Default,
            1000,
        );
        settings.decide_command.cache_seconds_max = 300;
        let config = config_with(settings);
        let sessions = sessions_for(&config);
        let (hook, _) = hook_of(&config, &sessions, "");
        let _ = hook("send all east");
        let _ = hook("send all east");
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        // A reload: the registry follows the configuration again.
        sessions.extensions.follow(&config);
        let _ = hook("send all east");
        assert_eq!(
            asked.load(Ordering::SeqCst),
            2,
            "asked again after a reload"
        );
        let _ = hook("send all east");
        assert_eq!(asked.load(Ordering::SeqCst), 2);
        // The extension goes and comes back on the same port: a new hello, and what it said before is
        // not what it says now.
        let link = sessions.extensions.link("gate").unwrap();
        let hellos = link.epoch();
        first.stop();
        let second = loop {
            match make(&asked).bind(address) {
                Ok(running) => break running,
                Err(_) => std::thread::sleep(Duration::from_millis(20)),
            }
        };
        let limit = Instant::now() + Duration::from_secs(15);
        while link.epoch() == hellos || !link.is_connected() {
            assert!(Instant::now() < limit, "no new hello");
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = hook("send all east");
        assert_eq!(
            asked.load(Ordering::SeqCst),
            3,
            "asked again after a new hello"
        );
        drop(second);
    }

    // ---- what a kept answer is kept for ----------------------------------------------------------

    /// The hook of Alex's session as `hook_of` makes it, with the server Alex is on in a cell the test
    /// changes, and the gate to look at.
    fn hook_moving(
        config: &Config,
        sessions: &Sessions,
        file: &str,
    ) -> (Arc<CommandHook>, Arc<Mutex<String>>, Arc<Gate>) {
        let backend = Arc::new(Mutex::new("lobby".to_owned()));
        let permissions = Arc::new(Permissions::parse(file).unwrap());
        let live = Arc::new(config.clone());
        let gate = Arc::new(Gate::new(Arc::clone(&sessions.extensions), move || {
            Arc::clone(&live)
        }));
        let hook = {
            let backend = Arc::clone(&backend);
            commands::hook_with(
                Arc::new(config.clone()),
                move || Typist {
                    auth: AuthKind::Unverified,
                    backend: backend.lock().unwrap().clone(),
                    name: "Alex".into(),
                    uuid: [1; 16],
                    permissions: Arc::clone(&permissions),
                    operator_groups: Vec::new(),
                },
                |target: &str| format!("Moving you to {target}..."),
                |_: Role, _: &Typist| "Moving you to the lobby...".to_owned(),
                |_: &Typist, _: Answer| "done".to_owned(),
                Some(Arc::clone(&gate)),
                None,
            )
        };
        (hook, backend, gate)
    }

    /// The extension decides with the server the player is on, so what it said on one server is not
    /// reused on another, and the cache is emptied when the player's server changes.
    #[test]
    fn an_answer_kept_on_one_server_is_not_used_on_another() {
        let ranker = ranker(|request| {
            let verdict = if request.server == "lobby" {
                NodeVerdict::Allow
            } else {
                NodeVerdict::Deny
            };
            NodeAnswers::all(request, verdict).cache_seconds(60)
        });
        let config = config_with(ranker.extension(OnError::Default, 1000, 300));
        let sessions = sessions_for(&config);
        let (hook, backend, _) = hook_moving(&config, &sessions, "");
        assert_eq!(hook("send all east").as_deref(), Some("done"));
        assert_eq!(hook("send all east").as_deref(), Some("done"));
        assert_eq!(ranker.asked(), 1, "kept on the lobby");
        // The player moves: the extension is asked again, and says no.
        *backend.lock().unwrap() = "east".into();
        assert_eq!(hook("send all east").as_deref(), Some(DENIED_SEND));
        assert_eq!(ranker.asked(), 2);
        assert_eq!(ranker.seen.lock().unwrap()[1].server, "east");
        assert_eq!(hook("send all east").as_deref(), Some(DENIED_SEND));
        assert_eq!(ranker.asked(), 2, "kept on east");
        // And back: what was kept for the lobby went with the move.
        *backend.lock().unwrap() = "lobby".into();
        assert_eq!(hook("send all east").as_deref(), Some("done"));
        assert_eq!(ranker.asked(), 3);
    }

    /// The extension decides with the arguments too: `/send Bob lobby` and `/send all east` are two questions,
    /// and the same command with the same arguments is one.
    #[test]
    fn an_answer_kept_for_some_arguments_is_not_used_for_others() {
        let ranker = ranker(|request| {
            // Everyone may be sent, but nobody may send everyone.
            let verdict = if request.args.first().is_some_and(|who| who == "all") {
                NodeVerdict::Deny
            } else {
                NodeVerdict::Allow
            };
            NodeAnswers::all(request, verdict).cache_seconds(60)
        });
        let config = config_with(ranker.extension(OnError::Default, 1000, 300));
        let sessions = sessions_for(&config);
        let (hook, _, _) = hook_moving(&config, &sessions, "");
        assert_eq!(hook("send Bob lobby").as_deref(), Some("done"));
        assert_eq!(hook("send all east").as_deref(), Some(DENIED_SEND));
        assert_eq!(ranker.asked(), 2, "two questions, two requests");
        // The same ones again are kept.
        assert_eq!(hook("send Bob lobby").as_deref(), Some("done"));
        assert_eq!(hook("send all east").as_deref(), Some(DENIED_SEND));
        assert_eq!(hook("send Bob lobby").as_deref(), Some("done"));
        assert_eq!(ranker.asked(), 2);
        // Another word is another question, and so is the same word of another command.
        assert_eq!(hook("send Bob east").as_deref(), Some("done"));
        assert_eq!(ranker.asked(), 3);
        let _ = hook("alert Bob lobby");
        assert_eq!(ranker.asked(), 4);
    }

    /// What ran out is not worth a place: the answers whose time is over go before a new one is refused,
    /// so that a player who types commands for a long time keeps getting their answers kept.
    #[test]
    fn the_answers_that_ran_out_are_dropped_before_a_new_one_is_refused() {
        let ranker =
            ranker(|request| NodeAnswers::all(request, NodeVerdict::Allow).cache_seconds(60));
        // Kept for a second at most.
        let config = config_with(ranker.extension(OnError::Default, 1000, 1));
        let sessions = sessions_for(&config);
        let (hook, _, gate) = hook_moving(&config, &sessions, "");
        // Each message is another question, so each is kept on its own, up to the most there are.
        for i in 0..512 {
            let _ = hook(&format!("alert message {i}"));
        }
        assert_eq!(gate.kept_len(), 512);
        // Full: the next ones are asked and not kept.
        let before = ranker.asked();
        let _ = hook("alert one more");
        let _ = hook("alert one more");
        assert_eq!(ranker.asked(), before + 2);
        assert_eq!(gate.kept_len(), 512);
        // A second later they have run out: the first new answer makes room, and is kept.
        std::thread::sleep(Duration::from_millis(1100));
        let _ = hook("alert after the wait");
        assert_eq!(gate.kept_len(), 1);
        let again = ranker.asked();
        let _ = hook("alert after the wait");
        assert_eq!(ranker.asked(), again, "kept");
    }

    #[test]
    fn each_session_keeps_its_own_answers() {
        let ranker =
            ranker(|request| NodeAnswers::all(request, NodeVerdict::Allow).cache_seconds(60));
        let config = config_with(ranker.extension(OnError::Default, 1000, 300));
        let sessions = sessions_for(&config);
        let (first, _) = hook_of(&config, &sessions, "");
        let (second, _) = hook_of(&config, &sessions, "");
        let _ = first("send all east");
        let _ = first("send all east");
        let _ = second("send all east");
        assert_eq!(ranker.asked(), 2, "one for each player");
    }

    // ---- the proxy does not wait for ever --------------------------------------------------------

    #[test]
    fn a_slow_extension_costs_a_command_its_deadline_and_no_more() {
        let slow = ranker_after(Duration::from_millis(1500), |_| NodeAnswers::new());
        let config = config_with(slow.extension(OnError::Default, 150, 0));
        let sessions = sessions_for(&config);
        let (hook, _) = hook_of(&config, &sessions, FILE);
        let began = Instant::now();
        assert_eq!(hook("glist").as_deref(), Some("done"));
        let took = began.elapsed();
        assert!(
            took >= Duration::from_millis(140) && took < Duration::from_millis(600),
            "waited {took:?}"
        );
    }
}

/// The commands an extension declares of its own (docs/EXTENSIONS.md, 4.3): taken from the player, run by the
/// extension, their lines told when they come.
mod own {
    use super::*;
    use crate::commands::{self, Answer, Typist};
    use crate::extension::command::Gate;
    use crate::extension::own::{BUSY, NO_ANSWER, Own};
    use meridian_config::{AuthKind, CommandSettings, Permissions, ProxyCommand, Role};
    use meridian_extension::decide::{CommandRequest, NodeAnswers, NodeVerdict};
    use meridian_extension::run::{self, Lines as Said, RunRequest};
    use meridian_extension::wire::CommandSpec;
    use meridian_net::relay::{CommandHook, TAKEN};

    /// An extension that declares commands and answers each with a line of its own, and what it was asked.
    struct Parties {
        running: Running,
        folder: Folder,
        seen: Arc<Mutex<Vec<RunRequest>>>,
    }

    fn spec(name: &str, aliases: &[&str]) -> CommandSpec {
        CommandSpec {
            name: name.into(),
            aliases: aliases.iter().map(|a| (*a).to_owned()).collect(),
            usage: String::new(),
        }
    }

    /// `commands`: the declared names with their aliases. Each answers `<tag> ran <name>` after `delay`.
    fn parties(tag: &str, delay: Duration, commands: &[(&str, &[&str])]) -> Parties {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut extension = Sdk::new(tag, "1.0.0", Secret::new(TOKEN).unwrap())
            .log(Arc::new(Lines::default()) as Arc<dyn Log>);
        for (name, aliases) in commands {
            let (seen, tag) = (Arc::clone(&seen), tag.to_owned());
            extension = extension.command(spec(name, aliases), move |request: &RunRequest| {
                seen.lock().unwrap().push(request.clone());
                std::thread::sleep(delay);
                Said::one(&format!("{tag} ran {}", request.command))
            });
        }
        Parties {
            running: extension.bind("127.0.0.1:0").unwrap(),
            folder: Folder::new(),
            seen,
        }
    }

    fn party() -> Parties {
        parties("gate", Duration::ZERO, &[("party", &["p"])])
    }

    impl Parties {
        /// The `[[extension]]` called `name`, granted `grant`, whose commands wait `timeout_ms`.
        fn extension(&self, name: &str, grant: &[&str], timeout_ms: u64) -> ExtensionConfig {
            let mut config = extension_config(
                self.running.local_addr().to_string(),
                self.folder.token(),
                grant,
                OnError::Default,
                1000,
            );
            config.name = name.into();
            config.own_commands.timeout_ms = timeout_ms;
            config
        }

        fn asked(&self) -> usize {
            self.seen.lock().unwrap().len()
        }
    }

    /// The six commands of the proxy, minus `without`, and the extensions.
    fn config_of(extensions: Vec<ExtensionConfig>, without: &[ProxyCommand]) -> Config {
        let enabled = [
            ProxyCommand::Server,
            ProxyCommand::Hub,
            ProxyCommand::Play,
            ProxyCommand::Send,
            ProxyCommand::Glist,
            ProxyCommand::Alert,
        ]
        .into_iter()
        .filter(|c| !without.contains(c))
        .collect();
        Config {
            extensions,
            commands: CommandSettings {
                enabled,
                ..CommandSettings::default()
            },
            ..routed()
        }
    }

    /// Alex's hook, and the lines told to Alex.
    fn hook_of(
        config: &Config,
        sessions: &Sessions,
        file: &str,
    ) -> (Arc<CommandHook>, Arc<Mutex<Vec<String>>>) {
        let told = Arc::new(Mutex::new(Vec::new()));
        let permissions = Arc::new(Permissions::parse(file).unwrap());
        let live = Arc::new(config.clone());
        let current = move || Arc::clone(&live);
        let hook = commands::hook_with(
            Arc::new(config.clone()),
            move || Typist {
                auth: AuthKind::Unverified,
                backend: "lobby".into(),
                name: "Alex".into(),
                uuid: [1; 16],
                permissions: Arc::clone(&permissions),
                operator_groups: Vec::new(),
            },
            |target: &str| format!("Moving you to {target}..."),
            |_: Role, _: &Typist| "Moving you to the lobby...".to_owned(),
            |_: &Typist, _: Answer| "done".to_owned(),
            Some(Arc::new(Gate::new(
                Arc::clone(&sessions.extensions),
                current.clone(),
            ))),
            Some(Arc::new(Own::new(
                Arc::clone(&sessions.extensions),
                current,
                {
                    let told = Arc::clone(&told);
                    move |line: &str| told.lock().unwrap().push(line.to_owned())
                },
            ))),
        );
        (hook, told)
    }

    /// Waits until `count` lines have been told, and gives them.
    fn told_by_now(told: &Arc<Mutex<Vec<String>>>, count: usize) -> Vec<String> {
        let limit = Instant::now() + Duration::from_secs(5);
        while told.lock().unwrap().len() < count {
            assert!(Instant::now() < limit, "told {:?}", told.lock().unwrap());
            std::thread::sleep(Duration::from_millis(5));
        }
        told.lock().unwrap().clone()
    }

    const TAKEN_AT_ONCE: Option<&str> = Some(TAKEN);

    #[test]
    fn without_an_extension_every_word_goes_to_the_server() {
        let config = config_of(Vec::new(), &[]);
        let sessions = Sessions::default();
        let (hook, told) = hook_of(&config, &sessions, "");
        assert_eq!(hook("party invite Bob"), None);
        assert_eq!(hook("tp Bob"), None);
        // The proxy's own commands answer as ever: `/send` is closed to a player nothing opens it for.
        assert_eq!(
            hook("send all east").as_deref(),
            Some("You do not have permission to use /send.")
        );
        assert!(told.lock().unwrap().is_empty());
    }

    #[test]
    fn a_declared_command_is_taken_its_lines_come_later_and_the_extension_is_told_who_and_what() {
        let party = party();
        let config = config_of(vec![party.extension("gate", &["commands"], 2000)], &[]);
        let sessions = sessions_for(&config);
        let (hook, told) = hook_of(&config, &sessions, "");
        // Taken at once: the hook says nothing now, and the command goes nowhere else.
        assert_eq!(hook("party invite Bob").as_deref(), TAKEN_AT_ONCE);
        assert_eq!(told_by_now(&told, 1), ["gate ran party"]);
        let seen = party.seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].player.name, "Alex");
        assert_eq!(seen[0].player.uuid, "01010101-0101-0101-0101-010101010101");
        assert_eq!(
            seen[0].player.auth,
            meridian_extension::decide::Auth::Unverified
        );
        assert_eq!(seen[0].server, "lobby");
        assert_eq!(seen[0].command, "party");
        assert_eq!(seen[0].args, ["invite", "Bob"]);
        // A word that is not the extension's is not taken.
        assert_eq!(hook("tp Bob"), None);
        assert_eq!(party.asked(), 1);
        let link = sessions.extensions.link("gate").unwrap();
        assert_eq!(link.stats().count(run::RUN, RequestOutcome::Answered), 1);
    }

    #[test]
    fn an_alias_runs_the_command_it_stands_for_and_the_proxys_own_commands_are_not_touched() {
        let party = party();
        let config = config_of(vec![party.extension("gate", &["commands"], 2000)], &[]);
        let sessions = sessions_for(&config);
        let (hook, told) = hook_of(&config, &sessions, FILE_STAFF);
        assert_eq!(hook("p").as_deref(), TAKEN_AT_ONCE);
        assert_eq!(told_by_now(&told, 1), ["gate ran party"]);
        assert_eq!(party.seen.lock().unwrap()[0].command, "party");
        // The proxy's own commands answer as ever, and the extension is not asked about them.
        assert_eq!(hook("send all east").as_deref(), Some("done"));
        assert_eq!(
            hook("hub").as_deref(),
            Some("You are already on lobby."),
            "hub"
        );
        assert_eq!(party.asked(), 1);
    }

    /// Gives Alex `/send`, and closes the extension's `party` to everyone but the group `vip`.
    const FILE_STAFF: &str = r#"
[group.staff]
allow = ["meridian.send"]
[[player]]
name = "Alex"
groups = ["staff"]
"#;

    #[test]
    fn the_node_of_the_command_decides_before_the_extension_is_asked() {
        let party = party();
        let config = config_of(vec![party.extension("gate", &["commands"], 2000)], &[]);
        let sessions = sessions_for(&config);
        // Open to everyone when nothing is written.
        let (hook, told) = hook_of(&config, &sessions, "");
        assert_eq!(hook("party").as_deref(), TAKEN_AT_ONCE);
        told_by_now(&told, 1);
        // Closed by the file: said at once, for the alias too, and the extension is not asked.
        let (hook, told) = hook_of(
            &config,
            &sessions,
            "[group.default]\ndeny = [\"ext.gate.party\"]\n",
        );
        let asked = party.asked();
        assert_eq!(
            hook("party").as_deref(),
            Some("You do not have permission to use /party.")
        );
        assert_eq!(
            hook("p").as_deref(),
            Some("You do not have permission to use /p.")
        );
        assert_eq!(party.asked(), asked);
        assert!(told.lock().unwrap().is_empty());
    }

    #[test]
    fn only_the_extension_that_owns_a_command_is_asked_for_its_node() {
        // One extension rules the permissions of the proxy's commands (`decide.command`), another has the
        // command `party`: the first is not told what Alex typed after it.
        let ranks = Sdk::new("ranks", "1.0.0", Secret::new(TOKEN).unwrap())
            .log(Arc::new(Lines::default()) as Arc<dyn Log>)
            .on_command(|request: &CommandRequest| NodeAnswers::all(request, NodeVerdict::Deny));
        let ranks = Parties {
            running: ranks.bind("127.0.0.1:0").unwrap(),
            folder: Folder::new(),
            seen: Arc::default(),
        };
        let owner = parties("parties", Duration::ZERO, &[("party", &[])]);
        let config = config_of(
            vec![
                ranks.extension("ranks", &["decide.command"], 1000),
                owner.extension("parties", &["commands"], 2000),
            ],
            &[],
        );
        let sessions = sessions_for(&config);
        let (hook, told) = hook_of(&config, &sessions, "");
        // The rank system denies every node it is asked about; it is not asked about this one.
        assert_eq!(hook("party").as_deref(), TAKEN_AT_ONCE);
        assert_eq!(told_by_now(&told, 1), ["parties ran party"]);
        // The rank system is asked about the proxy's own commands, as ever.
        assert_eq!(
            hook("send all east").as_deref(),
            Some("You do not have permission to use /send.")
        );
        // The same extension, holding both points, rules its own command's node.
        let both = Sdk::new("both", "1.0.0", Secret::new(TOKEN).unwrap())
            .log(Arc::new(Lines::default()) as Arc<dyn Log>)
            .on_command(|request: &CommandRequest| NodeAnswers::all(request, NodeVerdict::Deny))
            .command(spec("party", &[]), |_: &RunRequest| Said::one("ran"));
        let both = Parties {
            running: both.bind("127.0.0.1:0").unwrap(),
            folder: Folder::new(),
            seen: Arc::default(),
        };
        let config = config_of(
            vec![both.extension("both", &["decide.command", "commands"], 1000)],
            &[],
        );
        let sessions = sessions_for(&config);
        let (hook, _) = hook_of(&config, &sessions, "");
        assert_eq!(
            hook("party").as_deref(),
            Some("You do not have permission to use /party.")
        );
    }

    #[test]
    fn a_command_that_does_not_answer_in_time_says_so_and_a_late_answer_is_dropped() {
        let slow = parties("gate", Duration::from_millis(500), &[("party", &[])]);
        let config = config_of(vec![slow.extension("gate", &["commands"], 100)], &[]);
        let sessions = sessions_for(&config);
        let (hook, told) = hook_of(&config, &sessions, "");
        assert_eq!(hook("party").as_deref(), TAKEN_AT_ONCE);
        assert_eq!(told_by_now(&told, 1), [NO_ANSWER]);
        let link = sessions.extensions.link("gate").unwrap();
        assert_eq!(link.stats().count(run::RUN, RequestOutcome::Timeout), 1);
        // The answer comes after the deadline: dropped and counted, and the player reads nothing more.
        let limit = Instant::now() + Duration::from_secs(5);
        while link.stats().late() == 0 {
            assert!(Instant::now() < limit, "the late answer never came");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(told.lock().unwrap().len(), 1);
    }

    #[test]
    fn the_word_stays_the_extensions_while_it_is_down() {
        let party = party();
        let config = config_of(vec![party.extension("gate", &["commands"], 2000)], &[]);
        let sessions = sessions_for(&config);
        let (hook, told) = hook_of(&config, &sessions, "");
        assert_eq!(hook("party").as_deref(), TAKEN_AT_ONCE);
        told_by_now(&told, 1);
        let Parties { running, .. } = party;
        drop(running);
        let link = sessions.extensions.link("gate").unwrap();
        let limit = Instant::now() + Duration::from_secs(10);
        while link.is_connected() {
            assert!(Instant::now() < limit, "the connection never ended");
            std::thread::sleep(Duration::from_millis(10));
        }
        // The word stays reserved while the extension is down: not the server's, and told so.
        assert_eq!(hook("party").as_deref(), TAKEN_AT_ONCE);
        assert_eq!(told_by_now(&told, 2)[1], NO_ANSWER);
        assert_eq!(
            link.stats().count(run::RUN, RequestOutcome::Disconnected),
            1
        );
    }

    #[test]
    fn a_player_has_one_command_running_at_a_time() {
        // Long enough that a loaded machine types the second command while the first still runs.
        let slow = parties("gate", Duration::from_millis(1500), &[("party", &[])]);
        let config = config_of(vec![slow.extension("gate", &["commands"], 5000)], &[]);
        let sessions = sessions_for(&config);
        let (hook, told) = hook_of(&config, &sessions, "");
        assert_eq!(hook("party").as_deref(), TAKEN_AT_ONCE);
        assert_eq!(hook("party").as_deref(), Some(BUSY));
        assert!(slow.asked() <= 1, "the second was not sent");
        assert_eq!(told_by_now(&told, 1), ["gate ran party"]);
        // Done, the player may run another.
        let limit = Instant::now() + Duration::from_secs(5);
        while hook("party").as_deref() == Some(BUSY) {
            assert!(Instant::now() < limit, "still busy");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(told_by_now(&told, 2).len(), 2);
    }

    #[test]
    fn a_command_that_fails_never_opens_the_breaker_of_the_extension() {
        let slow = parties("gate", Duration::from_millis(300), &[("party", &[])]);
        let mut extension = slow.extension("gate", &["commands"], 30);
        extension.breaker_failures = 2;
        let config = config_of(vec![extension], &[]);
        let sessions = sessions_for(&config);
        let (hook, told) = hook_of(&config, &sessions, "");
        for round in 1..=4 {
            // The player waits for the one before: they have one running at a time.
            let limit = Instant::now() + Duration::from_secs(5);
            while hook("party").as_deref() == Some(BUSY) {
                assert!(Instant::now() < limit, "still busy");
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(told_by_now(&told, round).last().unwrap(), NO_ANSWER);
        }
        let link = sessions.extensions.link("gate").unwrap();
        assert_eq!(link.stats().count(run::RUN, RequestOutcome::Timeout), 4);
        // Four failures in a row, with two enough for a decision: the breaker is closed.
        let mut page = String::new();
        sessions.extensions.render(&mut page);
        assert!(
            page.contains("meridian_extension_breaker_open{extension=\"gate\"} 0"),
            "{page}"
        );
        assert_eq!(link.stats().count(run::RUN, RequestOutcome::Breaker), 0);
    }

    #[test]
    fn the_words_of_the_proxy_and_of_an_earlier_extension_are_not_taken() {
        // Declares `send` (the proxy's, enabled), `p` as an alias of its own, and `party`.
        let first = parties("first", Duration::ZERO, &[("send", &[]), ("party", &["p"])]);
        let second = parties(
            "second",
            Duration::ZERO,
            &[("party", &[]), ("other", &["p"])],
        );
        let config = config_of(
            vec![
                first.extension("first", &["commands"], 2000),
                second.extension("second", &["commands"], 2000),
            ],
            &[],
        );
        let sessions = sessions_for(&config);
        let (hook, told) = hook_of(&config, &sessions, FILE_STAFF);
        // `send` is still the proxy's.
        assert_eq!(hook("send all east").as_deref(), Some("done"));
        assert_eq!(first.asked() + second.asked(), 0);
        // `party` is the first extension's, in the order of the configuration.
        assert_eq!(hook("party").as_deref(), TAKEN_AT_ONCE);
        assert_eq!(told_by_now(&told, 1), ["first ran party"]);
        // `p` is the first's alias, so the second's `other` goes without it, and still has its name.
        assert_eq!(hook("p").as_deref(), TAKEN_AT_ONCE);
        let limit = Instant::now() + Duration::from_secs(5);
        while told.lock().unwrap().len() < 2 {
            assert!(Instant::now() < limit);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(told.lock().unwrap()[1], "first ran party");
        let limit = Instant::now() + Duration::from_secs(5);
        while hook("other").as_deref() == Some(BUSY) {
            assert!(Instant::now() < limit);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(told_by_now(&told, 3)[2], "second ran other");
        // The same extensions in the other order: the other one has `party`.
        let swapped = config_of(
            vec![
                second.extension("second", &["commands"], 2000),
                first.extension("first", &["commands"], 2000),
            ],
            &[],
        );
        let (hook, told) = hook_of(&swapped, &sessions, "");
        assert_eq!(hook("party").as_deref(), TAKEN_AT_ONCE);
        assert_eq!(told_by_now(&told, 1), ["second ran party"]);
    }

    #[test]
    fn a_word_of_the_proxy_that_is_not_enabled_may_be_the_extensions() {
        let party = parties("gate", Duration::ZERO, &[("send", &[])]);
        let config = config_of(
            vec![party.extension("gate", &["commands"], 2000)],
            &[ProxyCommand::Send],
        );
        let sessions = sessions_for(&config);
        let (hook, told) = hook_of(&config, &sessions, "");
        assert_eq!(hook("send all east").as_deref(), TAKEN_AT_ONCE);
        assert_eq!(told_by_now(&told, 1), ["gate ran send"]);
    }

    #[test]
    fn a_command_is_the_extensions_only_if_it_is_enabled_and_granted() {
        let party = party();
        let hooked = |extension: ExtensionConfig| {
            let config = config_of(vec![extension], &[]);
            let sessions = sessions_for(&config);
            let (hook, _) = hook_of(&config, &sessions, "");
            (hook("party"), sessions)
        };
        // Granted another point: the declaration is not used.
        let (said, _) = hooked(party.extension("gate", &["decide.login"], 2000));
        assert_eq!(said, None);
        // Switched off.
        let mut off = party.extension("gate", &["commands"], 2000);
        off.enabled = false;
        let (said, sessions) = hooked(off);
        assert_eq!(said, None);
        assert!(sessions.extensions.links().is_empty());
        assert_eq!(party.asked(), 0);
    }

    #[test]
    fn what_the_player_typed_is_bounded_before_the_extension_reads_it() {
        let party = party();
        let config = config_of(vec![party.extension("gate", &["commands"], 2000)], &[]);
        let sessions = sessions_for(&config);
        let (hook, told) = hook_of(&config, &sessions, "");
        let line = format!("party {}", vec!["x".repeat(300); 40].join(" "));
        assert_eq!(hook(&line).as_deref(), TAKEN_AT_ONCE);
        told_by_now(&told, 1);
        let seen = party.seen.lock().unwrap().clone();
        assert_eq!(seen[0].args.len(), 32);
        assert!(seen[0].args.iter().all(|a| a.chars().count() == 256));
    }

    #[test]
    fn an_extension_has_at_most_half_of_its_places_running_commands() {
        let slow = parties("gate", Duration::from_millis(1500), &[("party", &[])]);
        let mut extension = slow.extension("gate", &["commands"], 5000);
        // Four places: two for commands, two left for the extension's decisions.
        extension.max_in_flight = 4;
        let config = config_of(vec![extension], &[]);
        let sessions = sessions_for(&config);
        let players: Vec<_> = (0..3).map(|_| hook_of(&config, &sessions, "")).collect();
        assert_eq!(players[0].0("party").as_deref(), TAKEN_AT_ONCE);
        assert_eq!(players[1].0("party").as_deref(), TAKEN_AT_ONCE);
        // The third finds half the places taken: told at once, and the extension is not asked.
        assert_eq!(players[2].0("party").as_deref(), Some(NO_ANSWER));
        let link = sessions.extensions.link("gate").unwrap();
        assert_eq!(link.stats().count(run::RUN, RequestOutcome::Busy), 1);
        // The first two are answered, and the places are free again for the third.
        told_by_now(&players[0].1, 1);
        told_by_now(&players[1].1, 1);
        let limit = Instant::now() + Duration::from_secs(5);
        while players[2].0("party").as_deref() != TAKEN_AT_ONCE {
            assert!(Instant::now() < limit, "the places never came back");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(told_by_now(&players[2].1, 1), ["gate ran party"]);
    }

    #[test]
    fn typing_again_while_a_command_runs_does_not_ask_the_extension_for_the_permission() {
        let permissions = Arc::new(AtomicUsize::new(0));
        let both = Sdk::new("gate", "1.0.0", Secret::new(TOKEN).unwrap())
            .log(Arc::new(Lines::default()) as Arc<dyn Log>)
            .on_command({
                let permissions = Arc::clone(&permissions);
                move |_: &CommandRequest| {
                    permissions.fetch_add(1, Ordering::SeqCst);
                    NodeAnswers::new()
                }
            })
            .command(spec("party", &[]), |_: &RunRequest| {
                std::thread::sleep(Duration::from_millis(1500));
                Said::one("ran")
            });
        let both = Parties {
            running: both.bind("127.0.0.1:0").unwrap(),
            folder: Folder::new(),
            seen: Arc::default(),
        };
        let config = config_of(
            vec![both.extension("gate", &["decide.command", "commands"], 5000)],
            &[],
        );
        let sessions = sessions_for(&config);
        let (hook, told) = hook_of(&config, &sessions, "");
        assert_eq!(hook("party").as_deref(), TAKEN_AT_ONCE);
        assert_eq!(permissions.load(Ordering::SeqCst), 1);
        // Typed again while it runs: told at once, and the extension is not asked about the node again.
        for _ in 0..5 {
            assert_eq!(hook("party").as_deref(), Some(BUSY));
        }
        assert_eq!(permissions.load(Ordering::SeqCst), 1);
        assert_eq!(told_by_now(&told, 1), ["ran"]);
    }

    #[test]
    fn what_a_command_says_never_holds_a_formatting_code_or_a_reversing_mark() {
        let extension = Sdk::new("gate", "1.0.0", Secret::new(TOKEN).unwrap())
            .log(Arc::new(Lines::default()) as Arc<dyn Log>)
            .command(spec("party", &[]), |_: &RunRequest| {
                Said::new(["\u{a7}4Server\u{a7}r says hi", "x\u{202e}evil\u{200b}y"])
            });
        let party = Parties {
            running: extension.bind("127.0.0.1:0").unwrap(),
            folder: Folder::new(),
            seen: Arc::default(),
        };
        let config = config_of(vec![party.extension("gate", &["commands"], 2000)], &[]);
        let sessions = sessions_for(&config);
        let (hook, told) = hook_of(&config, &sessions, "");
        assert_eq!(hook("party").as_deref(), TAKEN_AT_ONCE);
        assert_eq!(told_by_now(&told, 2), ["4Serverr says hi", "xevily"]);
    }

    /// The extension that holds `word` in `config`, from the words the registry resolved.
    fn holder(
        registry: &crate::extension::Registry,
        config: &Config,
        word: &str,
    ) -> Option<String> {
        registry.claim(config, word).map(|claim| claim.extension)
    }

    #[test]
    fn the_words_are_resolved_again_when_what_they_depend_on_changes() {
        let first = parties("gate", Duration::ZERO, &[("party", &["p"])]);
        let second = parties("other", Duration::ZERO, &[("duel", &[]), ("party", &[])]);
        let third = parties("third", Duration::ZERO, &[("send", &[])]);
        let ext = |name: &'static str, grant: &[&str]| match name {
            "gate" => first.extension("gate", grant, 2000),
            "other" => second.extension("other", grant, 2000),
            _ => third.extension("third", grant, 2000),
        };
        let config = config_of(
            vec![
                ext("gate", &["commands"]),
                ext("other", &["commands"]),
                ext("third", &["commands"]),
            ],
            &[],
        );
        let sessions = sessions_for(&config);
        let registry = &sessions.extensions;
        let who = |config: &Config, word: &str| holder(registry, config, word);
        let some = |name: &str| Some(name.to_owned());
        assert_eq!(who(&config, "party"), some("gate"));
        assert_eq!(who(&config, "p"), some("gate"));
        assert_eq!(who(&config, "duel"), some("other"));
        // `send` is the proxy's, enabled: refused. Not enabled, it is the third's: the commands of the proxy
        // that are enabled are part of what the words were resolved for.
        assert_eq!(who(&config, "send"), None);
        let without_send = config_of(
            vec![
                ext("gate", &["commands"]),
                ext("other", &["commands"]),
                ext("third", &["commands"]),
            ],
            &[ProxyCommand::Send],
        );
        assert_eq!(who(&without_send, "send"), some("third"));
        assert_eq!(who(&config, "send"), None);
        // The order of the extensions: the first keeps `party`.
        let swapped = config_of(
            vec![
                ext("other", &["commands"]),
                ext("gate", &["commands"]),
                ext("third", &["commands"]),
            ],
            &[],
        );
        assert_eq!(who(&swapped, "party"), some("other"));
        // A name that is refused refuses its command: the first extension's `party` loses its alias `p` too.
        assert_eq!(who(&swapped, "p"), None);
        assert_eq!(who(&config, "party"), some("gate"));
        // The grant of `commands`: taken from the first, its words go, and the second has `party`.
        let ungranted = config_of(
            vec![
                ext("gate", &["decide.login"]),
                ext("other", &["commands"]),
                ext("third", &["commands"]),
            ],
            &[],
        );
        assert_eq!(who(&ungranted, "party"), some("other"));
        assert_eq!(who(&ungranted, "p"), None);
        assert_eq!(who(&config, "p"), some("gate"));
        // Switched off, it is as if it was not there.
        let mut off = ext("gate", &["commands"]);
        off.enabled = false;
        let disabled = config_of(vec![off, ext("other", &["commands"])], &[]);
        assert_eq!(who(&disabled, "party"), some("other"));
        assert_eq!(who(&disabled, "p"), None);
        // No extension has commands: nothing is resolved, and nothing is held.
        let none = config_of(Vec::new(), &[]);
        assert_eq!(who(&none, "party"), None);
        assert_eq!(who(&config, "party"), some("gate"));
    }

    #[test]
    fn a_new_hello_with_other_commands_is_read_and_the_old_words_are_let_go() {
        let first = parties("gate", Duration::ZERO, &[("party", &["p"])]);
        let config = config_of(vec![first.extension("gate", &["commands"], 2000)], &[]);
        let sessions = sessions_for(&config);
        let registry = &sessions.extensions;
        let link = registry.link("gate").unwrap();
        assert_eq!(holder(registry, &config, "party"), Some("gate".to_owned()));
        let hellos = link.epoch();
        // The extension is restarted with other commands, on the same address.
        let address = first.running.local_addr();
        let Parties { running, .. } = first;
        drop(running);
        let mut second = None;
        let limit = Instant::now() + Duration::from_secs(10);
        while second.is_none() {
            assert!(Instant::now() < limit, "the address never came back");
            second = Sdk::new("gate", "1.0.0", Secret::new(TOKEN).unwrap())
                .log(Arc::new(Lines::default()) as Arc<dyn Log>)
                .command(spec("duel", &[]), |_: &RunRequest| Said::one("a duel"))
                .bind(address)
                .ok();
            std::thread::sleep(Duration::from_millis(50));
        }
        let limit = Instant::now() + Duration::from_secs(20);
        while !(link.is_connected() && link.epoch() > hellos) {
            assert!(Instant::now() < limit, "the proxy never said hello again");
            std::thread::sleep(Duration::from_millis(20));
        }
        // What the new hello declares is what is read: `duel` is the extension's, `party` goes to the server.
        assert_eq!(holder(registry, &config, "duel"), Some("gate".to_owned()));
        assert_eq!(holder(registry, &config, "party"), None);
        assert_eq!(holder(registry, &config, "p"), None);
        drop(second);
    }

    #[test]
    fn a_link_that_a_reload_replaces_is_not_the_one_the_words_were_resolved_with() {
        let party = party();
        let config = config_of(vec![party.extension("gate", &["commands"], 2000)], &[]);
        let sessions = sessions_for(&config);
        let before = sessions.extensions.link("gate").unwrap();
        let claim = sessions.extensions.claim(&config, "party").unwrap();
        assert!(Arc::ptr_eq(&claim.link, &before));
        // A reload that changes a setting of the connection replaces the link by a new one, whose first
        // hello makes it as many hellos old as the old one was: the count alone would not tell them apart.
        let mut changed = party.extension("gate", &["commands"], 2000);
        changed.max_in_flight = 32;
        let reloaded = config_of(vec![changed], &[]);
        sessions.extensions.follow(&reloaded);
        let after = sessions.extensions.link("gate").unwrap();
        assert!(!Arc::ptr_eq(&before, &after), "the link was replaced");
        let limit = Instant::now() + Duration::from_secs(20);
        while !(after.is_connected() && after.epoch() >= 1) {
            assert!(Instant::now() < limit, "the new link never connected");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(after.epoch(), before.epoch(), "as many hellos as before");
        let claim = sessions.extensions.claim(&reloaded, "party").unwrap();
        assert!(
            Arc::ptr_eq(&claim.link, &after),
            "the new link, not the old"
        );
        // And the command goes through it.
        let (hook, told) = hook_of(&reloaded, &sessions, "");
        assert_eq!(hook("party").as_deref(), TAKEN_AT_ONCE);
        assert_eq!(told_by_now(&told, 1), ["gate ran party"]);
    }

    #[test]
    fn when_no_extension_has_commands_any_more_the_resolved_words_and_their_links_are_let_go() {
        let party = party();
        let config = config_of(vec![party.extension("gate", &["commands"], 2000)], &[]);
        let sessions = sessions_for(&config);
        let registry = &sessions.extensions;
        let link = registry.link("gate").unwrap();
        let base = Arc::strong_count(&link);
        drop(registry.claim(&config, "party").unwrap());
        assert!(
            Arc::strong_count(&link) > base,
            "the resolved words hold the link"
        );
        // Every extension without commands, or none: the next command a player types finds nothing to look
        // up, and what was resolved is let go with the link it held.
        let none = config_of(Vec::new(), &[]);
        assert!(registry.claim(&none, "party").is_none());
        assert_eq!(Arc::strong_count(&link), base, "nothing is held any more");
        // And they are resolved again when an extension has commands again.
        assert!(registry.claim(&config, "party").is_some());
    }

    #[test]
    fn a_link_that_a_reload_stopped_is_not_kept_alive_by_the_resolved_words() {
        let party = party();
        let config = config_of(vec![party.extension("gate", &["commands"], 2000)], &[]);
        let sessions = sessions_for(&config);
        let registry = &sessions.extensions;
        let weak = {
            let link = registry.link("gate").unwrap();
            drop(registry.claim(&config, "party").unwrap());
            Arc::downgrade(&link)
        };
        // The reload removes the extension: its link is stopped, and the words it held are let go.
        registry.follow(&config_of(Vec::new(), &[]));
        assert!(weak.upgrade().is_none(), "the stopped link is still held");
    }

    #[test]
    fn stopping_the_registry_lets_go_of_the_resolved_words_and_their_links() {
        let party = party();
        let config = config_of(vec![party.extension("gate", &["commands"], 2000)], &[]);
        let sessions = sessions_for(&config);
        let registry = &sessions.extensions;
        let weak = {
            let link = registry.link("gate").unwrap();
            drop(registry.claim(&config, "party").unwrap());
            Arc::downgrade(&link)
        };
        registry.stop();
        assert!(weak.upgrade().is_none(), "the stopped link is still held");
    }

    // The word of an extension, typed by a player, through `serve` and the real connection: what
    // `docs/EXTENSIONS.md` (4.3, decision Q1) says in one line, that **the server never sees it**.
    // The fake backend below fails the test on any packet it gets after the configuration phase, so
    // a command that was passed on would end the test; the control at the end shows that it does
    // see a command that is not the extension's.
    mod wire {
        use super::*;
        use horizon_protocol::packets::play;
        use ids::{configuration as cfg, play as ids_play};

        fn payload(body: Vec<u8>) -> Vec<u8> {
            body[1..].to_vec()
        }

        fn typed(line: &str) -> Step {
            Step::send(
                ids_play::serverbound::CHAT_COMMAND,
                payload(
                    play::Serverbound::ChatCommand(play::ChatCommand {
                        command: line.into(),
                    })
                    .encode(),
                ),
            )
        }

        fn said(line: &str) -> Step {
            Step::expect_exactly(
                ids_play::clientbound::SYSTEM_CHAT,
                payload(
                    play::Clientbound::SystemChat(play::SystemChat {
                        content: text::literal(line),
                        overlay: false,
                    })
                    .encode(),
                ),
            )
        }

        /// Alex logs in, types `line`, reads `replies`, and leaves. The backend sees a command only
        /// when `backend_gets_it`; otherwise it fails on anything but the player's close.
        fn alex_types(
            config: &Config,
            sessions: &Sessions,
            line: &str,
            replies: &[&str],
            backend_gets_it: bool,
        ) {
            let mut after_login = vec![
                finish_configuration(),
                Step::Expect {
                    id: cfg::serverbound::FINISH_CONFIGURATION,
                },
            ];
            if backend_gets_it {
                after_login.push(Step::Expect {
                    id: ids_play::serverbound::CHAT_COMMAND,
                });
            }
            after_login.push(Step::ExpectClose);
            let backend = FakeBackend {
                forwarding_secret: Some(SECRET.as_bytes().to_vec()),
                after_login,
                ..FakeBackend::default()
            };
            let (connect, seen) = connector(backend, Recorder::new());
            let (session, player) = logged_in("Alex", client::AuthKind::Offline, None);
            let mut script = vec![
                Step::Expect {
                    id: cfg::clientbound::FINISH_CONFIGURATION,
                },
                acknowledge_configuration(),
                typed(line),
            ];
            script.extend(replies.iter().map(|r| said(r)));
            script.push(Step::Close);
            let player =
                meridian_testkit::peer::spawn(player, script, Dir::ToClient, Recorder::new());
            serve(session, peer(), config, &*connect, sessions);
            player.join().unwrap().unwrap();
            let (_, _, backend) = seen.lock().unwrap().take().unwrap();
            backend.join().unwrap().unwrap();
        }

        fn parties_with_time() -> Parties {
            parties("gate", Duration::ZERO, &[("party", &["p"]), ("time", &[])])
        }

        #[test]
        fn what_a_player_types_of_an_extensions_words_is_answered_by_it_and_never_reaches_the_server()
         {
            let party = parties_with_time();
            let config = config_of(vec![party.extension("gate", &["commands"], 2000)], &[]);
            let sessions = sessions_for(&config);
            // One session for each word, as a player's next word may come before the guard of the
            // last one is let go (the lines are told first).
            alex_types(
                &config,
                &sessions,
                "party invite Bob",
                &["gate ran party"],
                false,
            );
            alex_types(&config, &sessions, "p", &["gate ran party"], false);
            // `time` is a word the server has too: it is the extension's as long as it declares it.
            alex_types(
                &config,
                &sessions,
                "time set 18000",
                &["gate ran time"],
                false,
            );
            let seen = party.seen.lock().unwrap().clone();
            let asked: Vec<(&str, Vec<&str>)> = seen
                .iter()
                .map(|r| {
                    (
                        r.command.as_str(),
                        r.args.iter().map(String::as_str).collect(),
                    )
                })
                .collect();
            assert_eq!(
                asked,
                [
                    ("party", vec!["invite", "Bob"]),
                    ("party", vec![]),
                    ("time", vec!["set", "18000"]),
                ]
            );
            let link = sessions.extensions.link("gate").unwrap();
            assert_eq!(link.stats().count(run::RUN, RequestOutcome::Answered), 3);
        }

        #[test]
        fn while_the_extension_is_down_its_words_say_so_and_still_never_reach_the_server() {
            let party = parties_with_time();
            let config = config_of(vec![party.extension("gate", &["commands"], 2000)], &[]);
            let sessions = sessions_for(&config);
            alex_types(&config, &sessions, "party", &["gate ran party"], false);
            let Parties { running, .. } = party;
            drop(running);
            let link = sessions.extensions.link("gate").unwrap();
            let limit = Instant::now() + Duration::from_secs(10);
            while link.is_connected() {
                assert!(Instant::now() < limit, "the connection never ended");
                std::thread::sleep(Duration::from_millis(10));
            }
            alex_types(&config, &sessions, "party", &[NO_ANSWER], false);
            alex_types(&config, &sessions, "time set 18000", &[NO_ANSWER], false);
            assert_eq!(
                link.stats().count(run::RUN, RequestOutcome::Disconnected),
                2
            );
        }

        #[test]
        fn control_a_command_that_is_not_the_extensions_does_reach_the_server() {
            let party = parties_with_time();
            let config = config_of(vec![party.extension("gate", &["commands"], 2000)], &[]);
            let sessions = sessions_for(&config);
            // The backend of this session waits for the command and fails without it, as the two tests
            // above fail with it: this is what makes their "never" worth something.
            alex_types(&config, &sessions, "tp Bob", &[], true);
            assert_eq!(party.asked(), 0, "the extension was not asked");
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The orders that read (docs/EXTENSIONS.md, section 5), from the real sessions and over the real connection

mod reading {
    use super::*;
    use crate::extension::Facts;
    use crate::extension::facts::ProxyFacts;
    use meridian_config::{BalancedServer, Balancing, Role};
    use meridian_extension::decide::Auth;
    use std::sync::RwLock;

    fn facts_of(sessions: &Arc<Sessions>, config: &Config) -> ProxyFacts {
        ProxyFacts::new(sessions, &Arc::new(RwLock::new(Arc::new(config.clone()))))
    }

    /// Whether a player was let in as a verified account is what the events of a move say of them: true only for a session that the session
    /// service confirmed, and false for one that did not, and for nobody who is online.
    #[test]
    fn the_facts_say_whether_a_player_was_let_in_as_a_verified_account() {
        let sessions = Arc::new(Sessions::default());
        let facts = facts_of(&sessions, &balanced());
        let notch = offline_uuid("Notch").to_be_bytes();
        let alex = offline_uuid("Alex").to_be_bytes();
        assert!(!facts.verified(notch), "nobody is online");
        let _notch = connected(&sessions, "Notch", Identity::Verified(notch));
        let _alex = connected(&sessions, "Alex", Identity::Unverified("alex".into()));
        assert!(facts.verified(notch));
        assert!(!facts.verified(alex));
        assert!(!facts.verified([9; 16]));
    }

    /// The routing config, with `lobby` balanced (soft cap 40, hard cap 50).
    fn balanced() -> Config {
        Config {
            balanced: vec![BalancedServer {
                backend: "lobby".into(),
                role: Role::Lobby,
                soft_cap: 40,
                hard_cap: 50,
            }],
            balancing: Balancing {
                enabled: true,
                ..Balancing::default()
            },
            ..routed()
        }
    }

    #[test]
    fn the_facts_of_the_proxy_are_its_sessions_its_servers_and_its_queue() {
        let sessions = Arc::new(Sessions::default());
        let config = balanced();
        let facts = facts_of(&sessions, &config);
        assert!(facts.players(true).is_empty());
        let _alex = connected(&sessions, "Alex", Identity::Unverified("alex".into()));
        let _notch = connected(
            &sessions,
            "Notch",
            Identity::Verified(offline_uuid("Notch").to_be_bytes()),
        );
        // The players, with how each was proven, where they are, and their address (the order takes it out).
        let mut players = facts.players(true);
        players.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(players.len(), 2);
        assert_eq!(
            (players[0].name.as_str(), players[0].auth),
            ("Alex", Auth::Unverified)
        );
        assert_eq!(
            (players[1].name.as_str(), players[1].auth),
            ("Notch", Auth::Verified)
        );
        assert_eq!(
            players[0].uuid,
            hyphenated(offline_uuid("Alex").to_be_bytes()).to_ascii_lowercase()
        );
        assert!(
            players
                .iter()
                .all(|p| p.server == "lobby" && p.address.is_some())
        );
        // A caller that did not ask for the addresses is not given them: the facts do not hand them out.
        let without = facts.players(false);
        assert_eq!(without.len(), 2);
        assert!(without.iter().all(|p| p.address.is_none()), "{without:?}");
        // The servers: the three of the configuration, `lobby` the default and balanced, the others with no role.
        let servers = facts.servers();
        let names: Vec<_> = servers.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["lobby", "east", "west"]);
        assert_eq!(
            servers.iter().map(|s| s.default).collect::<Vec<_>>(),
            [true, false, false]
        );
        assert_eq!(
            servers.iter().map(|s| s.players).collect::<Vec<_>>(),
            [2, 0, 0]
        );
        assert_eq!(servers[0].role.as_deref(), Some("lobby"));
        assert_eq!(
            (servers[0].soft_cap, servers[0].hard_cap),
            (Some(40), Some(50))
        );
        assert!(servers[0].state.is_some());
        assert_eq!(
            (&servers[1].role, &servers[1].state, servers[1].soft_cap),
            (&None, &None, None)
        );
        // The queue: nobody waits.
        let queue = facts.queue();
        assert_eq!(
            (queue.enabled, queue.total, queue.waiting.len()),
            (config.queue.enabled, 0, 0)
        );
    }

    /// `servers/list` from the real sessions: a server the rounds read gives whether it answered and what it last said of itself;
    /// one that is drained says why; one the rounds do not read gives none of it.
    #[test]
    fn the_list_of_servers_says_what_each_reported_whether_it_answered_and_why_it_is_kept() {
        let sessions = Arc::new(Sessions::default());
        let mut config = balanced();
        config.sidecars = vec![meridian_config::Sidecar {
            backend: "lobby".into(),
            address: "127.0.0.1:25580".into(),
            secret_file: PathBuf::from("unused"),
            notices: false,
        }];
        let report = meridian_sidecar::request::ServerStatus {
            players: Some(7),
            capacity: Some(60),
            held: Some(1),
            stopping: Some(false),
            ..Default::default()
        };
        crate::status::poll(&config, &|_, _| Ok(report), &sessions.statuses);
        let facts = facts_of(&sessions, &config);
        let servers = facts.servers();
        let lobby = &servers[0];
        assert_eq!((lobby.up, lobby.why), (Some(true), None));
        let read = lobby
            .report
            .expect("a server the rounds read has its report");
        assert_eq!(
            (read.players, read.capacity, read.held, read.stopping),
            (Some(7), Some(60), Some(1), Some(false))
        );
        assert!(read.age_ms < 60_000, "{read:?}");
        assert_eq!(
            (servers[1].up, servers[1].why, servers[1].report),
            (None, None, None)
        );
        sessions.balancer.drain("lobby");
        assert_eq!(
            facts.servers()[0].why,
            Some(meridian_extension::decide::Why::Drained)
        );
    }

    #[test]
    fn the_facts_hold_the_sessions_weakly_and_say_nothing_once_they_are_gone() {
        let sessions = Arc::new(Sessions::default());
        let facts = facts_of(&sessions, &balanced());
        drop(sessions);
        assert!(facts.players(true).is_empty() && facts.servers().is_empty());
        assert_eq!(facts.queue().total, 0);
    }

    /// An extension that asks for `orders.read` (and `addresses`), granted by the operator, reading the proxy's own
    /// sessions with its SDK over the real connection.
    fn read_with(addresses: bool) -> Option<String> {
        let folder = Folder::new();
        let mut extension = Sdk::new("reader", "1.0.0", Secret::new(TOKEN).unwrap())
            .log(Arc::new(Lines::default()) as Arc<dyn Log>)
            .wants("orders.read");
        let mut grant = vec!["orders.read"];
        if addresses {
            extension = extension.wants("addresses");
            grant.push("addresses");
        }
        let running = extension.bind("127.0.0.1:0").unwrap();
        let config = Config {
            extensions: vec![extension_config(
                running.local_addr().to_string(),
                folder.token(),
                &grant,
                OnError::Default,
                1000,
            )],
            ..routed()
        };
        let sessions = Arc::new(sessions_for(&config));
        sessions
            .extensions
            .serve_facts(Arc::new(facts_of(&sessions, &config)));
        let _alex = connected(&sessions, "Alex", Identity::Unverified("alex".into()));
        let proxy = running.proxy();
        let players = proxy.players().unwrap();
        assert_eq!(players.len(), 1);
        assert_eq!(
            (players[0].name.as_str(), players[0].server.as_str()),
            ("Alex", "lobby")
        );
        let found = proxy.find_player("ALEX").unwrap().expect("Alex is online");
        assert_eq!(found.uuid, players[0].uuid);
        assert_eq!(
            proxy
                .find_player(&players[0].uuid.replace('-', ""))
                .unwrap()
                .map(|p| p.name),
            Some("Alex".into())
        );
        assert_eq!(proxy.find_player("Nobody").unwrap(), None);
        assert_eq!(proxy.servers().unwrap().len(), 3);
        assert_eq!(proxy.queue().unwrap().total, 0);
        // A page of one, from the beginning: the only player, and no cursor.
        let page = proxy.players_page(Some(1), None).unwrap();
        assert_eq!((page.players.len(), page.total, page.next), (1, 1, None));
        players[0].address.clone()
    }

    #[test]
    fn an_sdk_reads_the_proxys_sessions_servers_and_queue_and_gets_an_address_only_with_addresses()
    {
        assert_eq!(read_with(false), None);
        assert!(read_with(true).is_some());
    }

    /// An extension that did not ask for `orders.read` in its hello is refused, though the operator granted it: only
    /// what both sides name is used.
    #[test]
    fn an_extension_that_did_not_ask_for_orders_read_is_refused_the_reads_the_operator_granted() {
        let folder = Folder::new();
        let running = Sdk::new("quiet", "1.0.0", Secret::new(TOKEN).unwrap())
            .log(Arc::new(Lines::default()) as Arc<dyn Log>)
            .on_login(|_| meridian_extension::decide::Decision::Default)
            .bind("127.0.0.1:0")
            .unwrap();
        let config = Config {
            extensions: vec![extension_config(
                running.local_addr().to_string(),
                folder.token(),
                &["decide.login", "orders.read"],
                OnError::Default,
                1000,
            )],
            ..routed()
        };
        let sessions = Arc::new(sessions_for(&config));
        sessions
            .extensions
            .serve_facts(Arc::new(facts_of(&sessions, &config)));
        let refused = running.proxy().players().unwrap_err();
        assert!(refused.is_not_granted(), "{refused}");
    }

    // ---- the orders that message ----------------------------------------------------------------------

    use crate::extension::actions::ProxyActions;
    use crate::extension::{Actions, KickRefusal, Paid, Refusal, SendLot};
    use meridian_extension::orders::{Target, Who};

    fn actions_of(sessions: &Arc<Sessions>, config: &Config) -> ProxyActions {
        ProxyActions::new(sessions, &Arc::new(RwLock::new(Arc::new(config.clone()))))
    }

    #[test]
    fn the_actions_of_the_proxy_tell_the_players_that_are_there_and_count_those_a_line_reaches() {
        let sessions = Arc::new(Sessions::default());
        let config = balanced();
        let actions = actions_of(&sessions, &config);
        // Nobody is online.
        assert_eq!(actions.tell(&Who::Name("Alex".into()), "hi"), 0);
        assert_eq!(actions.audience(None), Ok(0));
        let _alex = connected(&sessions, "Alex", Identity::Unverified("alex".into()));
        let _notch = connected(
            &sessions,
            "Notch",
            Identity::Verified(offline_uuid("Notch").to_be_bytes()),
        );
        // One player, by name in any case and by unique id; nobody else.
        // The sessions of this test have not finished their configuration: they read no chat, so nobody is shown the line
        // (the positive case is `try_tell`'s, tested where a session can be put in play).
        assert_eq!(actions.tell(&Who::Name("ALEX".into()), "hi"), 0);
        let notch = hyphenated(offline_uuid("Notch").to_be_bytes()).to_ascii_lowercase();
        assert_eq!(actions.tell(&Who::Id(notch), "hi"), 0);
        assert_eq!(actions.tell(&Who::Name("Nobody".into()), "hi"), 0);
        assert_eq!(
            actions.tell(
                &Who::Id("99999999-0000-0000-0000-000000000000".into()),
                "hi"
            ),
            0
        );
        // The players a line reaches: all of them, those of a server (both are on `lobby`), none of `east`'s, and a
        // server that is not one is an error whose words do not matter (the order gives its own).
        assert_eq!(actions.audience(None), Ok(2));
        assert_eq!(actions.audience(Some("lobby")), Ok(2));
        assert_eq!(actions.audience(Some("east")), Ok(0));
        assert!(actions.audience(Some("nowhere")).is_err());
        // The alert of the proxy, counted as it counts.
        // Two found, none shown the line.
        assert_eq!(actions.audience(None), Ok(2));
        assert_eq!(actions.broadcast(None, "hello"), Ok(0));
        assert_eq!(actions.broadcast(Some("east"), "hello"), Ok(0));
        assert!(actions.broadcast(Some("nowhere"), "hello").is_err());
    }

    #[test]
    fn the_actions_hold_the_sessions_weakly_and_reach_nobody_once_they_are_gone() {
        let sessions = Arc::new(Sessions::default());
        let actions = actions_of(&sessions, &balanced());
        drop(sessions);
        assert_eq!(actions.tell(&Who::Name("Alex".into()), "hi"), 0);
        assert_eq!(actions.audience(None), Ok(0));
        assert_eq!(actions.broadcast(None, "hi"), Ok(0));
    }

    /// An extension that asks for `orders.message`, granted by the operator, giving its orders with its SDK over the real
    /// connection to the real sessions.
    #[test]
    fn an_sdk_tells_a_player_and_broadcasts_over_the_real_connection() {
        let folder = Folder::new();
        let running = Sdk::new("speaker", "1.0.0", Secret::new(TOKEN).unwrap())
            .log(Arc::new(Lines::default()) as Arc<dyn Log>)
            .wants("orders.message")
            .bind("127.0.0.1:0")
            .unwrap();
        let config = Config {
            extensions: vec![extension_config(
                running.local_addr().to_string(),
                folder.token(),
                &["orders.message"],
                OnError::Default,
                1000,
            )],
            ..routed()
        };
        let sessions = Arc::new(sessions_for(&config));
        sessions
            .extensions
            .serve_actions(Arc::new(actions_of(&sessions, &config)));
        let _alex = connected(&sessions, "Alex", Identity::Unverified("alex".into()));
        let proxy = running.proxy();
        // Found, but not in the play phase: nobody is shown the line, and the answer says so.
        assert_eq!(proxy.tell("alex", "Hello."), Ok(0));
        assert_eq!(proxy.tell("Nobody", "Hello?"), Ok(0));
        assert_eq!(proxy.broadcast(None, "Restart in five minutes."), Ok(0));
        assert_eq!(proxy.broadcast(Some("east"), "Hello east."), Ok(0));
        // A server the proxy does not have: refused by the proxy (-32602), and told in fixed words.
        let refused = proxy.broadcast(Some("nowhere"), "hi").unwrap_err();
        assert!(
            matches!(&refused, meridian_extension::serve::OrderError::Refused(e) if e.code == -32602),
            "{refused}"
        );
        assert!(!refused.to_string().contains("nowhere"), "{refused}");
        // A line that is not valid does not leave the SDK.
        assert!(matches!(
            proxy.tell("Alex", "two\nlines"),
            Err(meridian_extension::serve::OrderError::Invalid(_))
        ));
    }

    // ---- the order that moves ---------------------------------------------------------------------------

    #[test]
    fn the_actions_of_the_proxy_plan_a_move_of_those_who_are_not_on_the_server_yet() {
        let sessions = Arc::new(Sessions::default());
        // No server of the balancer: a server the balancer knows only takes players it has a recent report of.
        let config = routed();
        let actions = actions_of(&sessions, &config);
        // No one online: a plan with nobody, which starts nothing.
        let plan = actions.plan_send(&Target::All, "east").unwrap();
        assert_eq!(plan.count(), 0);
        assert_eq!(plan.start(Paid::for_tests(), SendLot::free()), Ok(0));
        let _alex = connected(&sessions, "Alex", Identity::Unverified("alex".into()));
        let _notch = connected(
            &sessions,
            "Notch",
            Identity::Verified(offline_uuid("Notch").to_be_bytes()),
        );
        // Both are on `lobby`: all of them would move to `east`, none to `lobby`.
        assert_eq!(actions.plan_send(&Target::All, "east").unwrap().count(), 2);
        assert_eq!(actions.plan_send(&Target::All, "lobby").unwrap().count(), 0);
        assert_eq!(
            actions
                .plan_send(&Target::Server("lobby".into()), "east")
                .unwrap()
                .count(),
            2
        );
        assert_eq!(
            actions
                .plan_send(&Target::Server("west".into()), "east")
                .unwrap()
                .count(),
            0
        );
        // One player, by name in any case or by id; one who is not there is nobody.
        assert_eq!(
            actions
                .plan_send(&Target::Player(Who::Name("ALEX".into())), "east")
                .unwrap()
                .count(),
            1
        );
        let notch = hyphenated(offline_uuid("Notch").to_be_bytes()).to_ascii_lowercase();
        assert_eq!(
            actions
                .plan_send(&Target::Player(Who::Id(notch)), "east")
                .unwrap()
                .count(),
            1
        );
        assert_eq!(
            actions
                .plan_send(&Target::Player(Who::Name("Nobody".into())), "east")
                .unwrap()
                .count(),
            0
        );
        // A server the proxy does not have, as destination or as source, is refused.
        assert_eq!(
            actions.plan_send(&Target::All, "nowhere").err(),
            Some(Refusal::UnknownDestination)
        );
        assert_eq!(
            actions
                .plan_send(&Target::Server("nowhere".into()), "east")
                .err(),
            Some(Refusal::UnknownSource)
        );
    }

    #[test]
    fn an_sdk_asks_for_a_move_over_the_real_connection_and_is_refused_a_server_the_proxy_does_not_have()
     {
        let folder = Folder::new();
        let running = Sdk::new("mover", "1.0.0", Secret::new(TOKEN).unwrap())
            .log(Arc::new(Lines::default()) as Arc<dyn Log>)
            .wants("orders.move")
            .bind("127.0.0.1:0")
            .unwrap();
        let config = Config {
            extensions: vec![extension_config(
                running.local_addr().to_string(),
                folder.token(),
                &["orders.move"],
                OnError::Default,
                1000,
            )],
            ..routed()
        };
        let sessions = Arc::new(sessions_for(&config));
        sessions
            .extensions
            .serve_actions(Arc::new(actions_of(&sessions, &config)));
        let _alex = connected(&sessions, "Alex", Identity::Unverified("alex".into()));
        let proxy = running.proxy();
        // Nobody to move (Alex is on `lobby` already, and nobody is called Nobody): `0`, and no move is started.
        assert_eq!(proxy.send_all("lobby"), Ok(0));
        assert_eq!(proxy.send_player("Nobody", "east"), Ok(0));
        assert_eq!(proxy.send_server("west", "east"), Ok(0));
        for refused in [
            proxy.send_all("nowhere"),
            proxy.send_server("nowhere", "east"),
        ] {
            let refused = refused.unwrap_err();
            assert!(
                matches!(&refused, meridian_extension::serve::OrderError::Refused(e) if e.code == -32602),
                "{refused}"
            );
        }
    }

    // ---- the order that expels ---------------------------------------------------------------------------

    #[test]
    fn the_actions_of_the_proxy_kick_the_player_who_is_there_by_name_or_id_and_name_who_was_kicked()
    {
        let sessions = Arc::new(Sessions::default());
        let actions = actions_of(&sessions, &balanced());
        assert_eq!(actions.kick(&Who::Name("Alex".into()), "bye"), Ok(None));
        let _alex = connected(&sessions, "Alex", Identity::Unverified("alex".into()));
        let _notch = connected(
            &sessions,
            "Notch",
            Identity::Verified(offline_uuid("Notch").to_be_bytes()),
        );
        // Nobody of that name or id.
        assert_eq!(actions.kick(&Who::Name("Nobody".into()), "bye"), Ok(None));
        assert_eq!(
            actions.kick(
                &Who::Id("99999999-0000-0000-0000-000000000000".into()),
                "bye"
            ),
            Ok(None)
        );
        // By name in any case, and by unique id: the player kicked is named as the proxy knows them.
        assert_eq!(
            actions.kick(&Who::Name("ALEX".into()), "Spam."),
            Ok(Some("Alex".to_owned()))
        );
        let notch = hyphenated(offline_uuid("Notch").to_be_bytes()).to_ascii_lowercase();
        assert_eq!(
            actions.kick(&Who::Id(notch), "Spam."),
            Ok(Some("Notch".to_owned()))
        );
    }

    /// The reason a kicked player reads, in the phase their client is in.
    fn kick_reason(player: &mut Connection) -> String {
        let (id, payload) = player.recv().unwrap();
        let configuration::Clientbound::Disconnect(d) =
            configuration::Clientbound::decode(id, &payload).unwrap()
        else {
            panic!("not a disconnect");
        };
        text::plain(&d.reason, &Default::default())
    }

    /// Two accounts whose names differ only by case (a verified `alex` beside an unverified `Alex`, which `on-duplicate =
    /// "refuse"` does not tell apart: it refuses a second one of the same sort) are never kicked by name, since the first of
    /// the two would be the one a map happens to list first. Nobody is kicked; the unique id names one of them.
    #[test]
    fn a_name_that_two_accounts_have_by_case_kicks_nobody_and_the_unique_id_kicks_the_one_it_names()
    {
        let sessions = Arc::new(Sessions::default());
        let actions = actions_of(&sessions, &balanced());
        let upper = connected(&sessions, "Alex", Identity::Unverified("alex".into()));
        let lower = connected(
            &sessions,
            "alex",
            Identity::Verified(offline_uuid("alex").to_be_bytes()),
        );
        for spelling in ["Alex", "alex", "ALEX"] {
            assert_eq!(
                actions.kick(&Who::Name(spelling.into()), "Spam."),
                Err(KickRefusal::Several),
                "{spelling}"
            );
        }
        // Neither was told anything: their relays still run.
        assert!(!upper.1.is_finished() && !lower.1.is_finished());
        // The unique id names the account: only that one is kicked, whatever the case of the name it has.
        let id = hyphenated(offline_uuid("alex").to_be_bytes()).to_ascii_lowercase();
        assert_eq!(
            actions.kick(&Who::Id(id), "Spam."),
            Ok(Some("alex".to_owned()))
        );
        let (_, relay, mut kicked, _server) = lower;
        assert_eq!(kick_reason(&mut kicked), "Spam.");
        assert!(relay.join().unwrap().is_ok());
        assert!(!upper.1.is_finished(), "the other account was not kicked");
    }

    /// A player who has `meridian.kick.exempt` (here by a rank of their own) cannot be kicked by an extension, by name or by id;
    /// the others still can. The permission is given to nobody by default.
    #[test]
    fn a_player_with_the_kick_exempt_permission_is_not_kicked_by_an_extension() {
        let sessions = Arc::new(Sessions::default());
        let actions = actions_of(&sessions, &balanced());
        let _boss = connected(&sessions, "Boss", Identity::Unverified("boss".into()));
        let _alex = connected(&sessions, "Alex", Identity::Unverified("alex".into()));
        let boss = hyphenated(offline_uuid("Boss").to_be_bytes()).to_ascii_lowercase();
        sessions.permissions.install(
            meridian_config::Permissions::parse(
                "[group.guard]\nallow = [\"meridian.kick.exempt\"]\n[[player]]\nname = \"Boss\"\ngroups = [\"guard\"]\n",
            )
            .unwrap(),
            None,
        );
        assert_eq!(
            actions.kick(&Who::Name("boss".into()), "Spam."),
            Err(KickRefusal::Exempt)
        );
        assert_eq!(
            actions.kick(&Who::Id(boss), "Spam."),
            Err(KickRefusal::Exempt)
        );
        assert_eq!(
            actions.kick(&Who::Name("Alex".into()), "Spam."),
            Ok(Some("Alex".to_owned()))
        );
        // Taking the rank away, the player can be kicked again: it is read at each kick, not when they joined.
        sessions
            .permissions
            .install(meridian_config::Permissions::default(), None);
        assert_eq!(
            actions.kick(&Who::Name("Boss".into()), "Spam."),
            Ok(Some("Boss".to_owned()))
        );
    }

    /// A player who is still in the configuration phase reads the reason, a kick of a session already ended says `1` again while
    /// the session is listed (a player who was leaving counts), and once it is gone nobody is there to be kicked.
    #[test]
    fn a_player_in_configuration_reads_the_reason_and_kicking_twice_says_one_while_the_session_is_listed()
     {
        let sessions = Arc::new(Sessions::default());
        let actions = actions_of(&sessions, &balanced());
        let (open, relay, mut player, _server) =
            connected(&sessions, "Alex", Identity::Unverified("alex".into()));
        assert_eq!(
            actions.kick(&Who::Name("alex".into()), "First."),
            Ok(Some("Alex".to_owned()))
        );
        assert_eq!(kick_reason(&mut player), "First.");
        assert!(relay.join().unwrap().is_ok());
        // Its session is still listed until the proxy has let it go: a second kick finds it and counts.
        assert_eq!(
            actions.kick(&Who::Name("alex".into()), "Second."),
            Ok(Some("Alex".to_owned()))
        );
        drop(open);
        assert_eq!(actions.kick(&Who::Name("alex".into()), "Third."), Ok(None));
    }

    #[test]
    fn an_sdk_kicks_over_the_real_connection_and_a_player_who_is_not_online_is_zero() {
        let folder = Folder::new();
        let running = Sdk::new("warden", "1.0.0", Secret::new(TOKEN).unwrap())
            .log(Arc::new(Lines::default()) as Arc<dyn Log>)
            .wants("orders.moderate")
            .bind("127.0.0.1:0")
            .unwrap();
        let config = Config {
            extensions: vec![extension_config(
                running.local_addr().to_string(),
                folder.token(),
                &["orders.moderate"],
                OnError::Default,
                1000,
            )],
            ..routed()
        };
        let sessions = Arc::new(sessions_for(&config));
        sessions
            .extensions
            .serve_actions(Arc::new(actions_of(&sessions, &config)));
        let _alex = connected(&sessions, "Alex", Identity::Unverified("alex".into()));
        let proxy = running.proxy();
        assert_eq!(proxy.kick("Nobody", Some("hello?")), Ok(0));
        assert_eq!(proxy.kick("alex", Some("Spam.")), Ok(1));
        // A reason that is not valid does not leave the SDK.
        assert!(matches!(
            proxy.kick("Alex", Some("two\nlines")),
            Err(meridian_extension::serve::OrderError::Invalid(_))
        ));
    }

    /// A server the balancer drained takes no players: a plan to it is refused, whoever is to be moved.
    #[test]
    fn a_plan_to_a_server_that_takes_no_players_is_refused() {
        let sessions = Arc::new(Sessions::default());
        let config = Config {
            balanced: vec![
                BalancedServer {
                    backend: "lobby".into(),
                    role: Role::Lobby,
                    soft_cap: 40,
                    hard_cap: 50,
                },
                BalancedServer {
                    backend: "east".into(),
                    role: Role::Game,
                    soft_cap: 20,
                    hard_cap: 24,
                },
            ],
            balancing: Balancing {
                enabled: true,
                ..Balancing::default()
            },
            ..routed()
        };
        let actions = actions_of(&sessions, &config);
        // `east` is offered while nothing says otherwise (a server that never reported is not one that stopped reporting)...
        assert!(actions.plan_send(&Target::All, "east").is_ok());
        // ...and drained, it takes no player.
        assert!(sessions.balancer.drain("east"));
        assert_eq!(
            actions
                .plan_send(&Target::Player(Who::Name("Alex".into())), "east")
                .err(),
            Some(Refusal::DestinationNotTakingPlayers)
        );
        // A server that has no role is not the balancer's: it is taken, and so is the one whose destination is unknown refused first.
        assert!(actions.plan_send(&Target::All, "west").is_ok());
        assert_eq!(
            actions.plan_send(&Target::All, "nowhere").err(),
            Some(Refusal::UnknownDestination)
        );
    }
}

// ---------------------------------------------------------------------------------------------
// The events of real sessions (docs/EXTENSIONS.md, section 6): through `serve`, the real connection, and the SDK's handler

mod events {
    use super::*;
    use meridian_extension::events::{Event, LeftReason};

    /// An extension that collects the events it is told.
    struct Watcher {
        running: Running,
        folder: Folder,
        seen: Arc<Mutex<Vec<Event>>>,
    }

    /// An extension that collects the events, and asks for `wants` besides `events`.
    fn watcher(wants: &[&str]) -> Watcher {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut extension = Sdk::new("gate", "1.0.0", Secret::new(TOKEN).unwrap())
            .log(Arc::new(Lines::default()) as Arc<dyn Log>)
            .on_event({
                let seen = Arc::clone(&seen);
                move |event| seen.lock().unwrap().push(event.clone())
            });
        for capability in wants {
            extension = extension.wants(capability);
        }
        let running = extension.bind("127.0.0.1:0").unwrap();
        Watcher {
            running,
            folder: Folder::new(),
            seen,
        }
    }

    impl Watcher {
        fn config(&self, grant: &[&str]) -> Config {
            Gate::config(extension_config(
                self.running.local_addr().to_string(),
                self.folder.token(),
                grant,
                OnError::Default,
                1000,
            ))
        }

        fn wait_for(&self, count: usize) -> Vec<Event> {
            let limit = Instant::now() + Duration::from_secs(10);
            loop {
                let seen = self.seen.lock().unwrap().clone();
                if seen.len() >= count {
                    return seen;
                }
                assert!(
                    Instant::now() < limit,
                    "gave up waiting for {count} events, got {seen:?}"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    /// A player who goes in and stays until the proxy ends their session: the backend waits to be closed, and the player reads the
    /// disconnect.
    fn stays_until_ended(config: &Config, sessions: &Sessions, name: &str) {
        let (connect, backends) =
            reachable_after(vec![("127.0.0.1:25601", true)], vec![Step::ExpectClose]);
        route_one(
            config,
            sessions,
            &*connect,
            &backends,
            logged_in(name, offline(), None),
            refused(),
        );
    }

    /// A real login and a real departure are told, with who, where and why; the address only to an extension granted `addresses`.
    #[test]
    fn a_real_login_and_departure_are_told_with_who_where_and_why_and_the_address_only_with_addresses()
     {
        for (grant, address) in [
            (&["events"][..], false),
            (&["events", "addresses"][..], true),
        ] {
            let watcher = watcher(&["addresses"]);
            let config = watcher.config(grant);
            let sessions = sessions_for(&config);
            goes_in(&config, &sessions, "Notch");
            let seen = watcher.wait_for(2);
            let Event::Joined(joined) = &seen[0] else {
                panic!("{:?}", seen[0]);
            };
            assert_eq!(
                (joined.player.name.as_str(), joined.server.as_str()),
                ("Notch", "lobby")
            );
            assert_eq!(joined.player.auth, Auth::Unverified);
            assert_eq!(joined.seq, 1);
            assert_eq!(
                joined.address,
                address.then(|| peer().ip().to_string()),
                "{grant:?}"
            );
            let Event::Left(left) = &seen[1] else {
                panic!("{:?}", seen[1]);
            };
            assert_eq!(
                (
                    left.player.name.as_str(),
                    left.server.as_str(),
                    left.reason,
                    left.seq
                ),
                ("Notch", "lobby", LeftReason::Left, 2)
            );
            // The player is gone from the list by the time the extension is told.
            assert!(sessions.find("Notch").is_none());
        }
    }

    /// Why a session ended, when the proxy ended it: a kick, a stop.
    #[test]
    fn a_session_the_proxy_ended_says_why_a_kick_and_a_stop() {
        for (n, why) in [LeftReason::Kicked, LeftReason::Shutdown]
            .into_iter()
            .enumerate()
        {
            let watcher = watcher(&[]);
            let config = watcher.config(&["events"]);
            let sessions = sessions_for(&config);
            std::thread::scope(|scope| {
                scope.spawn(|| stays_until_ended(&config, &sessions, "Notch"));
                let limit = Instant::now() + Duration::from_secs(10);
                while sessions.find("Notch").is_none() {
                    assert!(Instant::now() < limit, "the session never opened");
                    std::thread::sleep(Duration::from_millis(5));
                }
                if n == 0 {
                    assert!(sessions.kick("notch", "bye").is_some());
                } else {
                    assert_eq!(sessions.close_all(), 1);
                }
            });
            let seen = watcher.wait_for(2);
            let Event::Left(left) = &seen[1] else {
                panic!("{:?}", seen[1]);
            };
            assert_eq!(left.reason, why);
        }
    }

    /// A second login that replaces the first ends it with `replaced`.
    #[test]
    fn a_session_that_a_second_login_replaced_says_replaced() {
        let watcher = watcher(&[]);
        let mut config = watcher.config(&["events"]);
        config.on_duplicate = OnDuplicate::Replace;
        let sessions = sessions_for(&config);
        std::thread::scope(|scope| {
            scope.spawn(|| stays_until_ended(&config, &sessions, "Notch"));
            let limit = Instant::now() + Duration::from_secs(10);
            while sessions.find("Notch").is_none() {
                assert!(Instant::now() < limit, "the session never opened");
                std::thread::sleep(Duration::from_millis(5));
            }
            scope.spawn(|| stays_until_ended(&config, &sessions, "Notch"));
            // The first ends because the second replaced it; then the second is ended, as an operator would.
            let seen = watcher.wait_for(3);
            assert!(
                seen.iter()
                    .any(|e| matches!(e, Event::Left(l) if l.reason == LeftReason::Replaced)),
                "{seen:?}"
            );
            assert!(sessions.kick("notch", "bye").is_some());
        });
        let seen = watcher.wait_for(4);
        let kinds = |wanted: LeftReason| {
            seen.iter()
                .filter(|e| matches!(e, Event::Left(l) if l.reason == wanted))
                .count()
        };
        assert_eq!(
            (kinds(LeftReason::Replaced), kinds(LeftReason::Kicked)),
            (1, 1),
            "{seen:?}"
        );
    }
}
