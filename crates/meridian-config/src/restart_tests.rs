//! The `[restart]` table.
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

/// Without the table the proxy only marks a backend draining: nothing moves the
/// players, which is what it did before the table existed.
#[test]
fn the_defaults_do_not_move_anybody() {
    let restart = parse("").unwrap().restart;
    assert_eq!(restart, Restart::default());
    assert!(!restart.evacuate);
    assert_eq!(restart.silent_rounds, 3);
    assert_eq!(restart.grace_seconds, 60);
    // An empty table is the same.
    assert_eq!(parse("[restart]").unwrap().restart, Restart::default());
}

#[test]
fn every_key_can_be_set() {
    let restart = parse("[restart]\nevacuate = true\nsilent-rounds = 5\ngrace-seconds = 90")
        .unwrap()
        .restart;
    assert_eq!(
        restart,
        Restart {
            evacuate: true,
            silent_rounds: 5,
            grace_seconds: 90,
        }
    );
}

#[test]
fn a_key_left_out_keeps_its_default() {
    let restart = parse("[restart]\nevacuate = true").unwrap().restart;
    assert!(restart.evacuate);
    assert_eq!(restart.silent_rounds, 3);
    assert_eq!(restart.grace_seconds, 60);
}

#[test]
fn zero_rounds_and_zero_grace_are_refused() {
    let e = parse("[restart]\nsilent-rounds = 0").unwrap_err();
    assert!(matches!(e, ConfigError::ZeroSilentRounds), "{e}");
    assert!(e.to_string().contains("silent-rounds"), "{e}");
    let e = parse("[restart]\ngrace-seconds = 0").unwrap_err();
    assert!(matches!(e, ConfigError::ZeroRestartGrace), "{e}");
    assert!(e.to_string().contains("grace-seconds"), "{e}");
}

#[test]
fn a_wrong_key_or_type_is_refused() {
    for bad in [
        "[restart]\nevacuate = \"yes\"",
        "[restart]\nsilent_rounds = 3",
        "[restart]\nsilent-rounds = -1",
        "[restart]\nunknown = 1",
    ] {
        assert!(matches!(parse(bad), Err(ConfigError::Parse(_))), "{bad}");
    }
}
