//! Bans and the whitelist at login: a player who is refused is told why before any
//! backend is asked, and takes no place.
use super::*;
use crate::metrics::LoginFailure;
use meridian_config::Target;

/// `peer()` is in this range.
const RANGE: &str = "198.51.100.0/24";

fn ban(sessions: &Sessions, who: &str, reason: Option<&str>) {
    let target = Target::parse(who).unwrap();
    let _ = sessions
        .access
        .change(|a| ((), a.ban(target, reason.map(str::to_owned))));
}

/// Logs `name` in with `auth` against `sessions`: the backends tried and what the
/// player was told, with the player's end running `script`.
fn login(
    sessions: &Sessions,
    name: &str,
    auth: client::AuthKind,
    script: Vec<Step>,
) -> (Vec<String>, Trace) {
    let (connect, backends) = reachable(vec![("127.0.0.1:25601", true)]);
    route_one(
        &routed(),
        sessions,
        &*connect,
        &backends,
        logged_in(name, auth, None),
        script,
    )
}

fn offline() -> client::AuthKind {
    client::AuthKind::Offline
}

#[test]
fn a_player_nobody_banned_goes_to_a_backend_when_nothing_is_set() {
    let sessions = Sessions::default();
    let (tried, _) = login(&sessions, "Alex", offline(), vec![Step::ExpectClose]);
    assert_eq!(tried, ["127.0.0.1:25601"]);
}

#[test]
fn a_banned_name_is_refused_with_its_reason_before_any_backend() {
    let sessions = Sessions::default();
    ban(&sessions, "Alex", Some("griefing the spawn"));
    // Whatever the case of the name, and whatever the account behind it.
    for (name, auth) in [
        ("Alex", offline()),
        ("aLEX", offline()),
        ("ALEX", client::AuthKind::Premium),
    ] {
        let (tried, trace) = login(&sessions, name, auth, refused());
        assert!(tried.is_empty(), "{name}: a backend was asked");
        assert_eq!(
            refusal(&trace),
            "You are banned from this server.\nReason: griefing the spawn"
        );
    }
    assert_eq!(sessions.admitted(), 0, "a refused player takes no place");
    assert_eq!(sessions.metrics().failures_of(LoginFailure::Banned), 3);
    // Another player is not held by it.
    let (tried, _) = login(&sessions, "Steve", offline(), vec![Step::ExpectClose]);
    assert_eq!(tried, ["127.0.0.1:25601"]);
}

#[test]
fn a_ban_without_a_reason_says_only_that_the_player_is_banned() {
    let sessions = Sessions::default();
    ban(&sessions, "Alex", None);
    let (_, trace) = login(&sessions, "Alex", offline(), refused());
    assert_eq!(refusal(&trace), "You are banned from this server.");
}

#[test]
fn a_banned_unique_id_holds_the_account_whatever_it_is_called() {
    let sessions = Sessions::default();
    let notch = hyphenated(offline_uuid("Notch").to_be_bytes());
    ban(&sessions, &notch, Some("the account"));
    let (tried, trace) = login(&sessions, "Notch", client::AuthKind::Premium, refused());
    assert!(tried.is_empty());
    assert!(refusal(&trace).ends_with("Reason: the account"));
    let (tried, _) = login(
        &sessions,
        "Alex",
        client::AuthKind::Premium,
        vec![Step::ExpectClose],
    );
    assert_eq!(tried, ["127.0.0.1:25601"]);
}

#[test]
fn a_banned_address_range_holds_the_players_who_connect_from_it() {
    let sessions = Sessions::default();
    ban(&sessions, RANGE, Some("a bad network"));
    let (tried, trace) = login(&sessions, "Alex", offline(), refused());
    assert!(tried.is_empty());
    assert!(refusal(&trace).ends_with("Reason: a bad network"));
    // The same range elsewhere does not hold them.
    let sessions = Sessions::default();
    ban(&sessions, "203.0.113.0/24", None);
    let (tried, _) = login(&sessions, "Alex", offline(), vec![Step::ExpectClose]);
    assert_eq!(tried, ["127.0.0.1:25601"]);
}

#[test]
fn the_whitelist_refuses_a_player_who_is_not_on_it() {
    let sessions = Sessions::default();
    let _ = sessions.access.change(|a| {
        a.allow(Target::parse("Alex").unwrap()).unwrap();
        a.set_whitelist(true);
        ((), true)
    });
    let (tried, trace) = login(&sessions, "Steve", offline(), refused());
    assert!(tried.is_empty());
    assert_eq!(refusal(&trace), "You are not whitelisted on this server.");
    assert_eq!(sessions.admitted(), 0);
    assert_eq!(
        sessions.metrics().failures_of(LoginFailure::NotWhitelisted),
        1
    );
    // Who is on it goes in, whatever the case of the name.
    let (tried, _) = login(&sessions, "aLEX", offline(), vec![Step::ExpectClose]);
    assert_eq!(tried, ["127.0.0.1:25601"]);
}

#[test]
fn a_pardon_lets_the_player_in_again_and_turning_the_whitelist_off_lets_everyone_in() {
    let sessions = Sessions::default();
    ban(&sessions, "Alex", None);
    let _ = sessions.access.change(|a| {
        a.set_whitelist(true);
        ((), true)
    });
    let (tried, _) = login(&sessions, "Alex", offline(), refused());
    assert!(tried.is_empty());
    let _ = sessions
        .access
        .change(|a| ((), a.pardon(&Target::parse("alex").unwrap())));
    // No longer banned, but the whitelist still keeps them out.
    let (_, trace) = login(&sessions, "Alex", offline(), refused());
    assert_eq!(refusal(&trace), "You are not whitelisted on this server.");
    let _ = sessions.access.change(|a| {
        a.set_whitelist(false);
        ((), true)
    });
    let (tried, _) = login(&sessions, "Alex", offline(), vec![Step::ExpectClose]);
    assert_eq!(tried, ["127.0.0.1:25601"]);
}

#[test]
fn a_ban_comes_before_the_whitelist_and_before_the_duplicate_check() {
    let sessions = Sessions::default();
    let (_open, _relay, _player, _server) =
        connected(&sessions, "Alex", Identity::Unverified("alex".into()));
    ban(&sessions, "Alex", Some("later"));
    let (_, trace) = login(&sessions, "ALEX", offline(), refused());
    assert!(refusal(&trace).contains("banned"), "{}", refusal(&trace));
    assert_eq!(
        sessions.count(),
        1,
        "the session already open is not touched"
    );
}
