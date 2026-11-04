//! The `[queue]` table.
use super::*;

/// `top` is written before the backend, `tables` after it.
fn parse(top: &str, tables: &str) -> Result<Config, ConfigError> {
    Config::from_toml_str(&format!(
        r#"forwarding-secret = "test-secret"
{top}
[[backends]]
name = "lobby"
address = "127.0.0.1:25566"
{tables}"#
    ))
}

#[test]
fn the_queue_is_off_by_default() {
    let queue = parse("", "").unwrap().queue;
    assert_eq!(queue, QueueSettings::default());
    assert!(!queue.enabled);
    assert_eq!(queue.wait_seconds, 15);
    assert_eq!(queue.max_length, 50);
    assert_eq!(
        parse("", "[queue]").unwrap().queue,
        QueueSettings::default()
    );
}

#[test]
fn every_key_can_be_set() {
    let queue = parse(
        "",
        "[queue]\nenabled = true\nwait-seconds = 10\nmax-length = 5",
    )
    .unwrap()
    .queue;
    assert_eq!(
        queue,
        QueueSettings {
            enabled: true,
            wait_seconds: 10,
            max_length: 5,
        }
    );
}

#[test]
fn zero_is_refused_when_the_queue_is_on() {
    let e = parse("", "[queue]\nenabled = true\nwait-seconds = 0").unwrap_err();
    assert!(matches!(e, ConfigError::ZeroQueueWait), "{e}");
    let e = parse("", "[queue]\nenabled = true\nmax-length = 0").unwrap_err();
    assert!(matches!(e, ConfigError::ZeroQueueLength), "{e}");
    // Off, the values are not used.
    assert!(parse("", "[queue]\nwait-seconds = 0\nmax-length = 0").is_ok());
}

/// The client gives up after 30 s of silence: the wait and the time to reach a backend
/// must fit in it.
#[test]
fn a_wait_the_client_would_not_survive_is_refused() {
    let longest = format!("[queue]\nenabled = true\nwait-seconds = {MAX_QUEUE_WAIT_SECONDS}");
    assert!(parse("connect-timeout-ms = 5000", &longest).is_ok());
    let too_long = "[queue]\nenabled = true\nwait-seconds = 21";
    let e = parse("", too_long).unwrap_err();
    assert!(matches!(e, ConfigError::QueueWaitTooLong { .. }), "{e}");
    assert!(e.to_string().contains("30 seconds"), "{e}");
    // A slow connect timeout takes from the wait.
    let e = parse("connect-timeout-ms = 12000", &longest).unwrap_err();
    assert!(matches!(e, ConfigError::QueueWaitTooLong { .. }), "{e}");
    let shorter = "[queue]\nenabled = true\nwait-seconds = 15";
    assert!(parse("connect-timeout-ms = 12000", shorter).is_ok());
    // Off, a long wait does no harm.
    assert!(parse("", "[queue]\nwait-seconds = 300").is_ok());
}

#[test]
fn an_unknown_key_is_refused() {
    assert!(parse("", "[queue]\nsize = 5").is_err());
}
