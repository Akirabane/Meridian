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
