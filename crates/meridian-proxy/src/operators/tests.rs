//! Who counts as an operator: the rule, the cache, and what ages out of it.
use super::*;

const ALEX: [u8; 16] = [1; 16];
const STEVE: [u8; 16] = [2; 16];

fn on() -> OperatorSettings {
    OperatorSettings {
        from_servers: true,
        ..OperatorSettings::default()
    }
}

fn op(name: &str, id: Option<[u8; 16]>, level: u8) -> Op {
    Op {
        id,
        name: name.into(),
        level,
    }
}

/// An `Operators` with `settings`, and `ops` heard from `backend` at `at`.
fn heard(settings: &OperatorSettings, backend: &str, ops: Vec<Op>, at: Instant) -> Operators {
    let operators = Operators::default();
    operators.configure(settings);
    operators.replace(backend, ops, at);
    operators
}

fn groups(
    operators: &Operators,
    backend: &str,
    name: &str,
    uuid: [u8; 16],
    auth: AuthKind,
    now: Instant,
) -> Vec<String> {
    operators.groups_at(backend, name, uuid, auth, now)
}

#[test]
fn nobody_is_an_operator_while_the_table_is_off_whatever_a_server_lists() {
    let now = Instant::now();
    let off = OperatorSettings::default();
    let operators = heard(&off, "east", vec![op("Alex", Some(ALEX), 4)], now);
    assert!(groups(&operators, "east", "Alex", ALEX, AuthKind::Verified, now).is_empty());
    assert_eq!(
        operators.level_of("east", "Alex", ALEX, AuthKind::Verified),
        None
    );
}

#[test]
fn an_operator_of_the_server_they_are_on_is_in_the_group_at_the_level_asked() {
    let now = Instant::now();
    let operators = heard(
        &on(),
        "east",
        vec![op("Alex", Some(ALEX), 4), op("Steve", Some(STEVE), 3)],
        now,
    );
    let ask =
        |backend, name, uuid| groups(&operators, backend, name, uuid, AuthKind::Verified, now);
    assert_eq!(ask("east", "Alex", ALEX), ["operator"]);
    // Level 3 is below the default minimum of 4.
    assert!(ask("east", "Steve", STEVE).is_empty());
    // A player the server does not list.
    assert!(ask("east", "Kim", [3; 16]).is_empty());
    // And a minimum of 3 takes Steve in.
    let lower = OperatorSettings {
        min_level: 3,
        group: "staff".into(),
        ..on()
    };
    operators.configure(&lower);
    // Reconfiguring keeps what was heard of the servers that still count.
    assert_eq!(ask("east", "Steve", STEVE), ["staff"]);
}

/// The rule is the server the player is on, not any server: an operator of one server has
/// nothing from it on another.
#[test]
fn an_operator_of_one_server_is_nothing_on_another() {
    let now = Instant::now();
    let operators = heard(&on(), "east", vec![op("Alex", Some(ALEX), 4)], now);
    operators.replace("lobby", vec![], now);
    assert_eq!(
        groups(&operators, "east", "Alex", ALEX, AuthKind::Verified, now),
        ["operator"]
    );
    assert!(groups(&operators, "lobby", "Alex", ALEX, AuthKind::Verified, now).is_empty());
    // A server never read has no operators.
    assert!(groups(&operators, "game1", "Alex", ALEX, AuthKind::Verified, now).is_empty());
}

#[test]
fn only_the_servers_named_in_from_count() {
    let now = Instant::now();
    let only_lobby = OperatorSettings {
        from: vec!["lobby".into()],
        ..on()
    };
    let operators = Operators::default();
    operators.configure(&only_lobby);
    operators.replace("lobby", vec![op("Alex", Some(ALEX), 4)], now);
    operators.replace("east", vec![op("Alex", Some(ALEX), 4)], now);
    assert_eq!(
        groups(&operators, "lobby", "Alex", ALEX, AuthKind::Verified, now),
        ["operator"]
    );
    assert!(groups(&operators, "east", "Alex", ALEX, AuthKind::Verified, now).is_empty());
}

#[test]
fn a_player_is_found_by_unique_id_and_a_name_never_beats_a_different_id() {
    let now = Instant::now();
    let operators = heard(&on(), "east", vec![op("Alex", Some(ALEX), 4)], now);
    // The same name with another unique id (an unverified player of the same name, or someone
    // who took the name) is not the operator.
    for auth in [AuthKind::Verified, AuthKind::Unverified] {
        assert!(groups(&operators, "east", "Alex", STEVE, auth, now).is_empty());
    }
    // The id found, whatever the name typed.
    assert_eq!(
        groups(&operators, "east", "alex", ALEX, AuthKind::Unverified, now),
        ["operator"]
    );
}

/// A server may list a name it could not give an id for; then the name counts only for a
/// player whose account was checked.
#[test]
fn an_entry_without_an_id_is_found_by_name_only_for_a_verified_player() {
    let now = Instant::now();
    let operators = heard(&on(), "east", vec![op("Alex", None, 4)], now);
    assert_eq!(
        groups(&operators, "east", "alex", STEVE, AuthKind::Verified, now),
        ["operator"]
    );
    assert!(groups(&operators, "east", "Alex", ALEX, AuthKind::Unverified, now).is_empty());
}

/// The list read again replaces the list: an operator the server dropped is not one any more.
#[test]
fn reading_again_replaces_what_was_known() {
    let now = Instant::now();
    let operators = heard(
        &on(),
        "east",
        vec![op("Alex", Some(ALEX), 4), op("Steve", Some(STEVE), 4)],
        now,
    );
    assert_eq!(
        groups(&operators, "east", "Alex", ALEX, AuthKind::Verified, now),
        ["operator"]
    );
    operators.replace("east", vec![op("Steve", Some(STEVE), 4)], now);
    assert!(groups(&operators, "east", "Alex", ALEX, AuthKind::Verified, now).is_empty());
    assert_eq!(
        groups(&operators, "east", "Steve", STEVE, AuthKind::Verified, now),
        ["operator"]
    );
}

#[test]
fn what_was_heard_is_believed_for_stale_after_seconds_and_hearing_the_server_renews_it() {
    let start = Instant::now();
    let settings = OperatorSettings {
        stale_after_seconds: 600,
        ..on()
    };
    let operators = heard(&settings, "east", vec![op("Alex", Some(ALEX), 4)], start);
    let at = |seconds| start + Duration::from_secs(seconds);
    let is_op = |now| !groups(&operators, "east", "Alex", ALEX, AuthKind::Verified, now).is_empty();
    assert!(is_op(at(0)));
    assert!(is_op(at(600)), "up to the limit");
    assert!(!is_op(at(601)), "past it, the server has no operators");
    // The sidecar heard connected to the server renews the belief.
    operators.heard("east", at(500));
    assert!(is_op(at(1100)));
    assert!(!is_op(at(1101)));
    // Hearing a server that was never read does nothing.
    operators.heard("lobby", at(500));
    assert!(
        groups(
            &operators,
            "lobby",
            "Alex",
            ALEX,
            AuthKind::Verified,
            at(501)
        )
        .is_empty()
    );
}

#[test]
fn turning_the_table_off_or_dropping_a_server_forgets_what_it_said_at_once() {
    let now = Instant::now();
    let operators = heard(&on(), "east", vec![op("Alex", Some(ALEX), 4)], now);
    operators.replace("lobby", vec![op("Alex", Some(ALEX), 4)], now);
    operators.configure(&OperatorSettings {
        from: vec!["lobby".into()],
        ..on()
    });
    assert!(groups(&operators, "east", "Alex", ALEX, AuthKind::Verified, now).is_empty());
    assert_eq!(
        groups(&operators, "lobby", "Alex", ALEX, AuthKind::Verified, now),
        ["operator"]
    );
    operators.configure(&OperatorSettings::default());
    assert!(groups(&operators, "lobby", "Alex", ALEX, AuthKind::Verified, now).is_empty());
    // And on again, it has to be read again.
    operators.configure(&on());
    assert!(groups(&operators, "lobby", "Alex", ALEX, AuthKind::Verified, now).is_empty());
}

#[test]
fn the_level_is_told_for_the_console() {
    let now = Instant::now();
    let operators = heard(&on(), "east", vec![op("Alex", Some(ALEX), 4)], now);
    assert_eq!(
        operators.level_of("east", "Alex", ALEX, AuthKind::Verified),
        Some(4)
    );
    assert_eq!(
        operators.level_of("lobby", "Alex", ALEX, AuthKind::Verified),
        None
    );
}
