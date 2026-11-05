//! The routing settings: fallbacks, the player limit, forced hosts, the login
//! mode, groups and routes.
use super::*;

/// Three backends and a secret; `extra` is added at the top of the file.
fn with(extra: &str) -> String {
    format!(
        r#"{extra}
forwarding-secret = "test-secret"
default-backend = "lobby"
[[backends]]
name = "lobby"
address = "127.0.0.1:25566"
[[backends]]
name = "hub"
address = "127.0.0.1:25567"
[[backends]]
name = "open"
address = "127.0.0.1:25568"
"#
    )
}

fn load(extra: &str) -> Config {
    Config::from_toml_str(&with(extra)).unwrap()
}

fn refused(extra: &str) -> ConfigError {
    Config::from_toml_str(&with(extra)).expect_err("the configuration should be refused")
}

#[test]
fn try_is_empty_and_max_players_is_twenty_by_default() {
    let config = load("");
    assert!(config.try_backends.is_empty());
    assert_eq!(config.max_players, 20);
    assert_eq!(Config::default().max_players, 20);
}

#[test]
fn try_keeps_its_order() {
    assert_eq!(
        load(r#"try = ["open", "hub"]"#).try_backends,
        ["open", "hub"]
    );
}

#[test]
fn try_refuses_an_unknown_backend() {
    let e = refused(r#"try = ["hub", "nether"]"#);
    assert!(
        matches!(&e, ConfigError::UnknownBackend { setting, name } if setting == "try" && name == "nether"),
        "{e}"
    );
}

#[test]
fn try_refuses_the_default_backend_and_repeats() {
    assert!(matches!(refused(r#"try = ["lobby"]"#), ConfigError::DefaultInTry(n) if n == "lobby"));
    assert!(
        matches!(refused(r#"try = ["hub", "hub"]"#), ConfigError::DuplicateTry(n) if n == "hub")
    );
}

#[test]
fn max_players_zero_means_no_limit() {
    assert_eq!(load("max-players = 0").max_players, 0);
    assert_eq!(load("max-players = 500").max_players, 500);
}

#[test]
fn max_players_must_fit_the_server_list() {
    assert_eq!(
        load("max-players = 2147483647").max_players,
        i32::MAX as u32
    );
    let e = refused("max-players = 2147483648");
    assert!(
        matches!(e, ConfigError::MaxPlayersTooLarge(2_147_483_648)),
        "{e}"
    );
    // A negative number is not a player count at all.
    assert!(matches!(refused("max-players = -1"), ConfigError::Parse(_)));
}

const FORCED: &str = r#"
[[forced-host]]
host = "Play.Example.com"
backend = "hub"
[[forced-host]]
host = "open.example.com"
backend = "open"
"#;

/// `[[forced-host]]` tables must come after the top-level keys of [`with`].
fn load_forced(tables: &str) -> Result<Config, ConfigError> {
    Config::from_toml_str(&format!("{}{tables}", with("")))
}

#[test]
fn forced_hosts_are_read_in_lower_case() {
    let config = load_forced(FORCED).unwrap();
    assert_eq!(
        config.forced_hosts,
        [
            ForcedHost {
                host: "play.example.com".into(),
                backend: "hub".into(),
                motd: None,
            },
            ForcedHost {
                host: "open.example.com".into(),
                backend: "open".into(),
                motd: None,
            },
        ]
    );
}

#[test]
fn a_forced_host_is_found_whatever_its_case() {
    let config = load_forced(FORCED).unwrap();
    assert_eq!(config.forced_backend("PLAY.example.COM"), Some("hub"));
    assert_eq!(config.forced_backend("open.example.com."), Some("open"));
    assert_eq!(config.forced_backend("example.com"), None);
}

#[test]
fn a_forced_host_must_lead_to_a_configured_backend() {
    let e =
        load_forced("[[forced-host]]\nhost = \"a.example.com\"\nbackend = \"nether\"").unwrap_err();
    assert!(
        matches!(&e, ConfigError::UnknownBackend { name, .. } if name == "nether"),
        "{e}"
    );
    assert!(e.to_string().contains("a.example.com"), "{e}");
}

#[test]
fn a_forced_host_must_be_a_bare_host_name() {
    for host in [
        "",
        "play.example.com:25565",
        "play .example.com",
        "example.com/x",
        "example.com.",
    ] {
        let e = load_forced(&format!(
            "[[forced-host]]\nhost = \"{host}\"\nbackend = \"hub\""
        ))
        .unwrap_err();
        assert!(
            matches!(e, ConfigError::InvalidForcedHost { .. }),
            "{host}: {e}"
        );
    }
}

#[test]
fn a_host_cannot_be_forced_twice() {
    let e = load_forced(
        "[[forced-host]]\nhost = \"a.example.com\"\nbackend = \"hub\"\n\
         [[forced-host]]\nhost = \"A.example.com\"\nbackend = \"open\"",
    )
    .unwrap_err();
    assert!(
        matches!(&e, ConfigError::DuplicateForcedHost(h) if h == "a.example.com"),
        "{e}"
    );
}

#[test]
fn the_login_mode_is_online_by_default() {
    let config = load("");
    assert_eq!(config.auth_mode, AuthMode::Online);
    assert!(config.online_mode);
    assert_eq!(Config::default().auth_mode, AuthMode::Online);
}

#[test]
fn auth_mode_is_read_and_keeps_online_mode_in_step() {
    for (text, mode, online) in [
        ("online", AuthMode::Online, true),
        ("offline", AuthMode::Offline, false),
    ] {
        let config = load(&format!("auth-mode = \"{text}\""));
        assert_eq!(config.auth_mode, mode, "{text}");
        assert_eq!(config.online_mode, online, "{text}");
    }
}

#[test]
fn online_mode_alone_still_works() {
    let on = load("online-mode = true");
    assert_eq!((on.auth_mode, on.online_mode), (AuthMode::Online, true));
    let off = load("online-mode = false");
    assert_eq!((off.auth_mode, off.online_mode), (AuthMode::Offline, false));
}

#[test]
fn online_mode_and_auth_mode_may_agree() {
    assert_eq!(
        load("online-mode = true\nauth-mode = \"online\"").auth_mode,
        AuthMode::Online
    );
    assert_eq!(
        load("online-mode = false\nauth-mode = \"offline\"").auth_mode,
        AuthMode::Offline
    );
}

#[test]
fn online_mode_and_auth_mode_may_not_disagree() {
    for extra in [
        "online-mode = true\nauth-mode = \"offline\"",
        "online-mode = false\nauth-mode = \"online\"",
        "online-mode = true\nauth-mode = \"hybrid\"",
        "online-mode = false\nauth-mode = \"hybrid\"",
    ] {
        let e = refused(extra);
        assert!(
            matches!(e, ConfigError::AuthModeConflict { .. }),
            "{extra}: {e}"
        );
        assert!(
            e.to_string().contains("remove it and keep auth-mode"),
            "{e}"
        );
    }
}

#[test]
fn an_unknown_auth_mode_is_refused() {
    assert!(matches!(
        refused("auth-mode = \"cracked\""),
        ConfigError::Parse(_)
    ));
    assert!(matches!(
        refused("auth-mode = \"Online\""),
        ConfigError::Parse(_)
    ));
}

#[test]
fn a_config_built_in_code_must_keep_the_two_settings_in_step() {
    let mut config = load("");
    config.online_mode = false;
    assert!(matches!(
        config.validate(),
        Err(ConfigError::AuthModeConflict { .. })
    ));
    config.auth_mode = AuthMode::Offline;
    assert!(config.validate().is_ok());
}

/// Two groups, one of each kind. `extra` holds top-level keys, `tables` more
/// tables after the groups.
fn grouped(extra: &str, tables: &str) -> Result<Config, ConfigError> {
    Config::from_toml_str(&format!(
        r#"{}
[[group]]
name = "accounts"
auth = "verified"
backends = ["lobby", "hub"]

[[group]]
name = "guests"
auth = "unverified"
backends = ["open"]
{tables}"#,
        with(extra)
    ))
}

const BOTH_ROUTES: &str = r#"
[[route]]
auth = "verified"
group = "accounts"
[[route]]
auth = "unverified"
group = "guests"
"#;

#[test]
fn groups_and_routes_are_read() {
    let config = grouped("auth-mode = \"hybrid\"", BOTH_ROUTES).unwrap();
    assert_eq!(config.auth_mode, AuthMode::Hybrid);
    // Hybrid asks accounts to authenticate, so code that only knows online-mode
    // keeps encrypting until hybrid logins are written.
    assert!(config.online_mode);
    assert_eq!(
        config.groups[0],
        Group {
            name: "accounts".into(),
            auth: AuthKind::Verified,
            backends: vec!["lobby".into(), "hub".into()],
        }
    );
    assert_eq!(config.routes.len(), 2);
    assert_eq!(
        config.route_group(AuthKind::Verified).unwrap().name,
        "accounts"
    );
    assert_eq!(
        config.route_group(AuthKind::Unverified).unwrap().name,
        "guests"
    );
}

#[test]
fn groups_without_routes_are_allowed_and_change_nothing() {
    let config = grouped("", "").unwrap();
    assert_eq!(config.groups.len(), 2);
    assert!(config.routes.is_empty());
    assert!(config.route_group(AuthKind::Verified).is_none());
}

#[test]
fn online_mode_needs_only_the_verified_route() {
    let config = grouped(
        "auth-mode = \"online\"",
        "[[route]]\nauth = \"verified\"\ngroup = \"accounts\"",
    )
    .unwrap();
    assert_eq!(config.routes.len(), 1);
}

#[test]
fn a_route_missing_for_a_kind_the_mode_lets_in_is_refused() {
    let e = grouped(
        "auth-mode = \"offline\"",
        "[[route]]\nauth = \"verified\"\ngroup = \"accounts\"",
    )
    .unwrap_err();
    assert!(
        matches!(
            e,
            ConfigError::MissingRoute {
                auth: AuthKind::Unverified,
                mode: AuthMode::Offline
            }
        ),
        "{e}"
    );
    let e = grouped(
        "auth-mode = \"hybrid\"",
        "[[route]]\nauth = \"verified\"\ngroup = \"accounts\"",
    )
    .unwrap_err();
    assert!(
        matches!(
            e,
            ConfigError::MissingRoute {
                auth: AuthKind::Unverified,
                ..
            }
        ),
        "{e}"
    );
}

#[test]
fn hybrid_mode_without_routes_is_refused() {
    // Both kinds would meet on the default backend.
    let e = refused("auth-mode = \"hybrid\"");
    assert!(
        matches!(
            e,
            ConfigError::MissingRoute {
                auth: AuthKind::Verified,
                ..
            }
        ),
        "{e}"
    );
    assert!(e.to_string().contains("add a route"), "{e}");
}

#[test]
fn a_backend_in_groups_of_both_kinds_is_refused() {
    let e = grouped(
        "",
        "[[group]]\nname = \"mixed\"\nauth = \"unverified\"\nbackends = [\"hub\"]",
    )
    .unwrap_err();
    assert!(
        matches!(&e, ConfigError::MixedAuthBackend { backend, verified_group, unverified_group }
            if backend == "hub" && verified_group == "accounts" && unverified_group == "mixed"),
        "{e}"
    );
    let message = e.to_string();
    assert!(
        message.contains("\"hub\"") && message.contains("two files"),
        "{message}"
    );
}

#[test]
fn a_backend_in_two_groups_of_the_same_kind_is_allowed() {
    let config = grouped(
        "",
        "[[group]]\nname = \"more-accounts\"\nauth = \"verified\"\nbackends = [\"hub\"]",
    )
    .unwrap();
    assert_eq!(config.groups.len(), 3);
}

#[test]
fn bad_groups_are_refused() {
    let e = grouped(
        "",
        "[[group]]\nname = \"accounts\"\nauth = \"verified\"\nbackends = [\"hub\"]",
    )
    .unwrap_err();
    assert!(
        matches!(&e, ConfigError::DuplicateGroup(n) if n == "accounts"),
        "{e}"
    );

    let e = grouped(
        "",
        "[[group]]\nname = \"empty\"\nauth = \"verified\"\nbackends = []",
    )
    .unwrap_err();
    assert!(
        matches!(&e, ConfigError::EmptyGroup(n) if n == "empty"),
        "{e}"
    );

    let e = grouped(
        "",
        "[[group]]\nname = \" \"\nauth = \"verified\"\nbackends = [\"hub\"]",
    )
    .unwrap_err();
    assert!(matches!(e, ConfigError::EmptyGroupName), "{e}");

    let e = grouped(
        "",
        "[[group]]\nname = \"x\"\nauth = \"verified\"\nbackends = [\"nether\"]",
    )
    .unwrap_err();
    assert!(
        matches!(&e, ConfigError::UnknownBackend { setting, name } if setting == "group \"x\"" && name == "nether"),
        "{e}"
    );

    let e = grouped(
        "",
        "[[group]]\nname = \"x\"\nauth = \"verified\"\nbackends = [\"hub\", \"hub\"]",
    )
    .unwrap_err();
    assert!(matches!(e, ConfigError::DuplicateInGroup { .. }), "{e}");

    let e = grouped(
        "",
        "[[group]]\nname = \"x\"\nauth = \"guest\"\nbackends = [\"hub\"]",
    )
    .unwrap_err();
    assert!(matches!(e, ConfigError::Parse(_)), "{e}");
}

#[test]
fn bad_routes_are_refused() {
    let e = grouped("", "[[route]]\nauth = \"verified\"\ngroup = \"nowhere\"").unwrap_err();
    assert!(
        matches!(&e, ConfigError::UnknownRouteGroup { group, .. } if group == "nowhere"),
        "{e}"
    );

    let e = grouped("", "[[route]]\nauth = \"verified\"\ngroup = \"guests\"").unwrap_err();
    assert!(
        matches!(
            e,
            ConfigError::RouteAuthMismatch {
                auth: AuthKind::Verified,
                group_auth: AuthKind::Unverified,
                ..
            }
        ),
        "{e}"
    );

    let e = grouped(
        "",
        "[[route]]\nauth = \"verified\"\ngroup = \"accounts\"\n[[route]]\nauth = \"verified\"\ngroup = \"accounts\"",
    )
    .unwrap_err();
    assert!(
        matches!(e, ConfigError::DuplicateRoute(AuthKind::Verified)),
        "{e}"
    );
}

// The core settings: duplicates, the connection limit, backend retries and the
// server list's sample.

#[test]
fn core_settings_have_their_defaults() {
    let config = load("");
    assert_eq!(config.on_duplicate, OnDuplicate::Refuse);
    assert_eq!(config.connections_per_minute, 20);
    assert_eq!(config.max_pending_logins, 2048);
    assert_eq!(config.max_pending_logins_per_address, 0);
    assert_eq!(config.backend_retry_seconds, 10);
    assert_eq!(config.status_sample, 12);
}

#[test]
fn core_settings_are_read() {
    let config = load(
        "on-duplicate = \"replace\"\nconnections-per-minute = 0\n\
         max-pending-logins = 0\nmax-pending-logins-per-address = 4\n\
         backend-retry-seconds = 0\nstatus-sample = 0",
    );
    assert_eq!(config.on_duplicate, OnDuplicate::Replace);
    assert_eq!(config.connections_per_minute, 0);
    assert_eq!(config.max_pending_logins, 0);
    assert_eq!(config.max_pending_logins_per_address, 4);
    assert_eq!(config.backend_retry_seconds, 0);
    assert_eq!(config.status_sample, 0);
}

#[test]
fn the_status_sample_has_a_ceiling() {
    assert_eq!(load("status-sample = 100").status_sample, 100);
    let e = refused("status-sample = 101");
    assert!(matches!(e, ConfigError::StatusSampleTooLarge(101)), "{e}");
}

#[test]
fn bad_core_values_are_refused() {
    for extra in [
        "on-duplicate = \"kick\"",
        "connections-per-minute = -1",
        "backend-retry-seconds = -5",
        "status-sample = \"12\"",
    ] {
        assert!(matches!(refused(extra), ConfigError::Parse(_)), "{extra}");
    }
}
