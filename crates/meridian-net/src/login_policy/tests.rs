use super::*;
use std::net::Ipv4Addr;

const PEER: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1));
const OTHER_PEER: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 2));

fn config(mode: AuthMode, auth: AuthSettings) -> Config {
    Config {
        auth_mode: mode,
        online_mode: mode != AuthMode::Offline,
        auth,
        ..Config::default()
    }
}

fn hybrid() -> Config {
    config(AuthMode::Hybrid, AuthSettings::default())
}

/// Asks `policy` at `now` plus `secs` seconds.
fn ask(
    policy: &LoginPolicy,
    config: &Config,
    name: &str,
    lookup: &dyn NameLookup,
    peer: IpAddr,
    start: Instant,
    secs: u64,
) -> Result<AuthKind, Refusal> {
    policy.auth_for_at(
        config,
        name,
        lookup,
        Some(peer),
        start + Duration::from_secs(secs),
    )
}

// The three modes.

#[test]
fn online_mode_verifies_everyone_without_a_lookup() {
    let lookup = FakeLookup::with_accounts(&[]);
    let policy = LoginPolicy::new();
    let config = config(AuthMode::Online, AuthSettings::default());
    assert_eq!(
        policy.auth_for(&config, "NoAccount", &lookup, Some(PEER)),
        Ok(AuthKind::Premium)
    );
    assert_eq!(lookup.calls(), 0);
}

#[test]
fn offline_mode_verifies_nobody_without_a_lookup() {
    let lookup = FakeLookup::with_accounts(&["Notch"]);
    let policy = LoginPolicy::new();
    let config = config(AuthMode::Offline, AuthSettings::default());
    assert_eq!(
        policy.auth_for(&config, "Notch", &lookup, Some(PEER)),
        Ok(AuthKind::Offline)
    );
    assert_eq!(lookup.calls(), 0);
}

#[test]
fn hybrid_mode_verifies_account_names_only() {
    let lookup = FakeLookup::with_accounts(&["Notch"]);
    let policy = LoginPolicy::new();
    let config = hybrid();
    assert_eq!(
        policy.auth_for(&config, "Notch", &lookup, Some(PEER)),
        Ok(AuthKind::Premium)
    );
    assert_eq!(
        policy.auth_for(&config, "NoSuchAccount", &lookup, Some(PEER)),
        Ok(AuthKind::Offline)
    );
}

#[test]
fn a_name_is_an_account_whatever_its_case() {
    // Someone may not take an account's name by changing its case.
    let lookup = FakeLookup::with_accounts(&["Notch"]);
    let policy = LoginPolicy::new();
    assert_eq!(
        policy.auth_for(&hybrid(), "nOTCH", &lookup, Some(PEER)),
        Ok(AuthKind::Premium)
    );
}

// The cache.

#[test]
fn an_answer_is_remembered() {
    let lookup = FakeLookup::with_accounts(&["Notch"]);
    let policy = LoginPolicy::new();
    let config = hybrid();
    let start = Instant::now();
    for secs in [0, 10, 3599] {
        assert_eq!(
            ask(&policy, &config, "Notch", &lookup, PEER, start, secs),
            Ok(AuthKind::Premium)
        );
    }
    assert_eq!(lookup.calls(), 1);
    // Another case of the same name is the same entry.
    ask(&policy, &config, "NOTCH", &lookup, PEER, start, 20).unwrap();
    assert_eq!(lookup.calls(), 1);
}

#[test]
fn an_account_is_remembered_longer_than_no_account() {
    let lookup = FakeLookup::with_accounts(&["Notch"]);
    let policy = LoginPolicy::new();
    let config = hybrid(); // 3600 s for an account, 300 s for none.
    let start = Instant::now();
    ask(&policy, &config, "Notch", &lookup, PEER, start, 0).unwrap();
    ask(&policy, &config, "Free", &lookup, PEER, start, 0).unwrap();
    assert_eq!(lookup.calls(), 2);

    // At 300 s "no account" is looked up again; "account" is still known.
    ask(&policy, &config, "Notch", &lookup, PEER, start, 300).unwrap();
    assert_eq!(lookup.calls(), 2);
    ask(&policy, &config, "Free", &lookup, PEER, start, 300).unwrap();
    assert_eq!(lookup.calls(), 3);

    // At 3600 s the account is looked up again too.
    ask(&policy, &config, "Notch", &lookup, PEER, start, 3600).unwrap();
    assert_eq!(lookup.calls(), 4);
}

#[test]
fn a_name_that_became_an_account_is_verified_once_forgotten() {
    let policy = LoginPolicy::new();
    let config = hybrid();
    let start = Instant::now();
    let before = FakeLookup::with_accounts(&[]);
    assert_eq!(
        ask(&policy, &config, "Newcomer", &before, PEER, start, 0),
        Ok(AuthKind::Offline)
    );
    let after = FakeLookup::with_accounts(&["Newcomer"]);
    assert_eq!(
        ask(&policy, &config, "Newcomer", &after, PEER, start, 300),
        Ok(AuthKind::Premium)
    );
}

#[test]
fn a_zero_cache_remembers_nothing() {
    let lookup = FakeLookup::with_accounts(&["Notch"]);
    let policy = LoginPolicy::new();
    let config = config(
        AuthMode::Hybrid,
        AuthSettings {
            lookup_cache_seconds: 0,
            negative_cache_seconds: 0,
            ..AuthSettings::default()
        },
    );
    let start = Instant::now();
    ask(&policy, &config, "Notch", &lookup, PEER, start, 0).unwrap();
    ask(&policy, &config, "Notch", &lookup, PEER, start, 0).unwrap();
    assert_eq!(lookup.calls(), 2);
}

// Lookup failures.

#[test]
fn a_failed_lookup_refuses_by_default() {
    let lookup = FakeLookup::unavailable();
    let policy = LoginPolicy::new();
    assert_eq!(
        policy.auth_for(&hybrid(), "Anyone", &lookup, Some(PEER)),
        Err(Refusal::LookupFailed)
    );
}

#[test]
fn a_failed_lookup_lets_in_unverified_when_configured() {
    let lookup = FakeLookup::unavailable();
    let policy = LoginPolicy::new();
    let config = config(
        AuthMode::Hybrid,
        AuthSettings {
            on_lookup_failure: OnLookupFailure::Unverified,
            ..AuthSettings::default()
        },
    );
    assert_eq!(
        policy.auth_for(&config, "Anyone", &lookup, Some(PEER)),
        Ok(AuthKind::Offline)
    );
}

#[test]
fn a_failed_lookup_is_not_remembered() {
    let policy = LoginPolicy::new();
    let config = hybrid();
    let start = Instant::now();
    let down = FakeLookup::unavailable();
    assert_eq!(
        ask(&policy, &config, "Notch", &down, PEER, start, 0),
        Err(Refusal::LookupFailed)
    );
    let up = FakeLookup::with_accounts(&["Notch"]);
    assert_eq!(
        ask(&policy, &config, "Notch", &up, PEER, start, 1),
        Ok(AuthKind::Premium)
    );
}

// The limit per address.

fn limited(per_minute: u32) -> Config {
    config(
        AuthMode::Hybrid,
        AuthSettings {
            lookups_per_minute: per_minute,
            ..AuthSettings::default()
        },
    )
}

#[test]
fn an_address_is_refused_after_its_lookups_for_the_minute() {
    let lookup = FakeLookup::with_accounts(&[]);
    let policy = LoginPolicy::new();
    let config = limited(3);
    let start = Instant::now();
    for (i, name) in ["a1", "a2", "a3"].iter().enumerate() {
        assert!(ask(&policy, &config, name, &lookup, PEER, start, i as u64).is_ok());
    }
    assert_eq!(
        ask(&policy, &config, "a4", &lookup, PEER, start, 10),
        Err(Refusal::TooManyLookups)
    );
    // The refused attempt did not reach the service.
    assert_eq!(lookup.calls(), 3);
    // A new minute starts a new count.
    assert!(ask(&policy, &config, "a4", &lookup, PEER, start, 60).is_ok());
}

#[test]
fn remembered_names_do_not_count_against_the_limit() {
    let lookup = FakeLookup::with_accounts(&["Notch"]);
    let policy = LoginPolicy::new();
    let config = limited(1);
    let start = Instant::now();
    ask(&policy, &config, "Notch", &lookup, PEER, start, 0).unwrap();
    for secs in 1..10 {
        assert_eq!(
            ask(&policy, &config, "Notch", &lookup, PEER, start, secs),
            Ok(AuthKind::Premium)
        );
    }
}

#[test]
fn each_address_has_its_own_limit() {
    let lookup = FakeLookup::with_accounts(&[]);
    let policy = LoginPolicy::new();
    let config = limited(1);
    let start = Instant::now();
    assert!(ask(&policy, &config, "a1", &lookup, PEER, start, 0).is_ok());
    assert_eq!(
        ask(&policy, &config, "a2", &lookup, PEER, start, 0),
        Err(Refusal::TooManyLookups)
    );
    assert!(ask(&policy, &config, "a2", &lookup, OTHER_PEER, start, 0).is_ok());
}

#[test]
fn players_whose_address_is_unknown_share_one_limit() {
    let lookup = FakeLookup::with_accounts(&[]);
    let policy = LoginPolicy::new();
    let config = limited(1);
    let now = Instant::now();
    assert!(
        policy
            .auth_for_at(&config, "a1", &lookup, None, now)
            .is_ok()
    );
    assert_eq!(
        policy.auth_for_at(&config, "a2", &lookup, None, now),
        Err(Refusal::TooManyLookups)
    );
}

#[test]
fn failed_lookups_count_against_the_limit_too() {
    // Otherwise an address could keep the service busy for free.
    let lookup = FakeLookup::unavailable();
    let policy = LoginPolicy::new();
    let config = limited(1);
    let start = Instant::now();
    assert_eq!(
        ask(&policy, &config, "a1", &lookup, PEER, start, 0),
        Err(Refusal::LookupFailed)
    );
    assert_eq!(
        ask(&policy, &config, "a2", &lookup, PEER, start, 0),
        Err(Refusal::TooManyLookups)
    );
}

// The HTTP lookup's reading of answers, and the refusal texts.

#[test]
fn profile_answers_are_read_by_status() {
    assert_eq!(ProfileLookup::read_status(200), Ok(true));
    assert_eq!(ProfileLookup::read_status(204), Ok(false));
    assert_eq!(ProfileLookup::read_status(404), Ok(false));
    for status in [400, 403, 429, 500, 503] {
        assert_eq!(
            ProfileLookup::read_status(status),
            Err(LookupError),
            "{status}"
        );
    }
}

#[test]
fn a_name_with_other_characters_is_no_account_without_a_request() {
    // These return before any request is built, so no network is used.
    assert_eq!(ProfileLookup.is_known_account(""), Ok(false));
    assert_eq!(ProfileLookup.is_known_account("a/b"), Ok(false));
    assert_eq!(ProfileLookup.is_known_account("a b"), Ok(false));
}

#[test]
fn refusals_have_readable_reasons() {
    assert_eq!(
        Refusal::LookupFailed.reason_json(),
        r#"{"translate":"multiplayer.disconnect.authservers_down"}"#
    );
    assert!(
        Refusal::TooManyLookups
            .reason_json()
            .contains("Try again in a minute")
    );
}
