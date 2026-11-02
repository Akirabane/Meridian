//! The `[auth]` table.
use super::*;

fn with(auth: &str) -> Result<Config, ConfigError> {
    Config::from_toml_str(&format!(
        r#"
forwarding-secret = "test-secret"
[[backends]]
name = "lobby"
address = "127.0.0.1:25566"
{auth}"#
    ))
}

#[test]
fn the_defaults_remember_accounts_longer_and_refuse_on_failure() {
    let settings = with("").unwrap().auth;
    assert_eq!(settings, AuthSettings::default());
    assert_eq!(settings.lookup_cache_seconds, 3600);
    assert_eq!(settings.negative_cache_seconds, 300);
    assert_eq!(settings.on_lookup_failure, OnLookupFailure::Refuse);
    assert_eq!(settings.lookups_per_minute, 10);
}

#[test]
fn every_auth_setting_is_read() {
    let settings = with(
        "[auth]\nlookup-cache-seconds = 7200\nnegative-cache-seconds = 60\n\
         on-lookup-failure = \"unverified\"\nlookups-per-minute = 3",
    )
    .unwrap()
    .auth;
    assert_eq!(
        settings,
        AuthSettings {
            lookup_cache_seconds: 7200,
            negative_cache_seconds: 60,
            on_lookup_failure: OnLookupFailure::Unverified,
            lookups_per_minute: 3,
        }
    );
}

#[test]
fn a_partial_auth_table_keeps_the_other_defaults() {
    let settings = with("[auth]\non-lookup-failure = \"refuse\"").unwrap().auth;
    assert_eq!(settings, AuthSettings::default());
}

#[test]
fn no_account_is_never_remembered_longer_than_account() {
    let e = with("[auth]\nlookup-cache-seconds = 60\nnegative-cache-seconds = 61").unwrap_err();
    assert!(
        matches!(
            e,
            ConfigError::NegativeCacheLonger {
                positive: 60,
                negative: 61
            }
        ),
        "{e}"
    );
    // Equal is allowed, and so is no cache at all.
    assert!(with("[auth]\nlookup-cache-seconds = 60\nnegative-cache-seconds = 60").is_ok());
    assert!(with("[auth]\nlookup-cache-seconds = 0\nnegative-cache-seconds = 0").is_ok());
}

#[test]
fn at_least_one_lookup_a_minute() {
    let e = with("[auth]\nlookups-per-minute = 0").unwrap_err();
    assert!(matches!(e, ConfigError::ZeroLookupsPerMinute), "{e}");
}

#[test]
fn bad_auth_values_are_refused() {
    for auth in [
        "[auth]\non-lookup-failure = \"allow\"",
        "[auth]\nlookup-cache-seconds = -1",
        "[auth]\nmode = \"hybrid\"",
    ] {
        assert!(matches!(with(auth), Err(ConfigError::Parse(_))), "{auth}");
    }
}

#[test]
fn debug_output_shows_the_auth_settings() {
    let shown = format!("{:?}", with("").unwrap());
    assert!(shown.contains("lookups_per_minute: 10"), "{shown}");
}
