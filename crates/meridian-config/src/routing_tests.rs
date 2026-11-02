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
                backend: "hub".into()
            },
            ForcedHost {
                host: "open.example.com".into(),
                backend: "open".into()
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
