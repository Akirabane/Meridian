//! The `[fallback]` table.
use super::*;

fn parse(tables: &str) -> Result<Config, ConfigError> {
    Config::from_toml_str(&format!(
        r#"forwarding-secret = "test-secret"
[[backends]]
name = "lobby"
address = "127.0.0.1:25566"
{tables}"#
    ))
}

/// Without the table both kinds of loss are made good, three servers at most.
#[test]
fn the_defaults_move_the_player_after_a_kick_and_after_a_crash() {
    let fallback = parse("").unwrap().fallback;
    assert_eq!(fallback, Fallback::default());
    assert!(fallback.on_kick);
    assert!(fallback.on_crash);
    assert_eq!(fallback.max_attempts, 3);
    assert_eq!(parse("[fallback]").unwrap().fallback, Fallback::default());
}

#[test]
fn each_setting_is_read() {
    let fallback = parse("[fallback]\non-kick = false\nmax-attempts = 5")
        .unwrap()
        .fallback;
    assert!(!fallback.on_kick);
    assert!(fallback.on_crash);
    assert_eq!(fallback.max_attempts, 5);
    let fallback = parse("[fallback]\non-crash = false").unwrap().fallback;
    assert!(fallback.on_kick);
    assert!(!fallback.on_crash);
}

#[test]
fn no_attempt_at_all_is_refused() {
    let e = parse("[fallback]\nmax-attempts = 0").unwrap_err();
    assert!(matches!(e, ConfigError::ZeroFallbackAttempts), "{e}");
    assert!(e.to_string().contains("max-attempts"), "{e}");
}

#[test]
fn a_wrong_key_or_type_is_refused() {
    for bad in [
        "[fallback]\non-kick = \"yes\"",
        "[fallback]\non_kick = true",
        "[fallback]\nmax-attempts = -1",
        "[fallback]\nunknown = 1",
    ] {
        assert!(matches!(parse(bad), Err(ConfigError::Parse(_))), "{bad}");
    }
}
