use super::*;
use crate::balance::Server;
use crate::session::{Sessions, tests::idle_session_on};
use std::cell::RefCell;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Instant;

fn config(extra: &str) -> Config {
    Config::from_toml_str(&format!(
        r#"
forwarding-secret = "test-secret"
default-backend = "a"
auth-mode = "offline"
[[backends]]
name = "a"
address = "127.0.0.1:25601"
role = "lobby"
soft-cap = 2
hard-cap = 3
[[backends]]
name = "b"
address = "127.0.0.1:25602"
role = "lobby"
soft-cap = 2
hard-cap = 3
[[backends]]
name = "c"
address = "127.0.0.1:25603"
role = "lobby"
soft-cap = 2
hard-cap = 3
[[backends]]
name = "g1"
address = "127.0.0.1:25604"
role = "game"
soft-cap = 2
hard-cap = 3
[[backends]]
name = "g2"
address = "127.0.0.1:25605"
role = "game"
soft-cap = 2
hard-cap = 3
[balancing]
enabled = true
margin = 0
{extra}
"#
    ))
    .unwrap()
}

fn player(name: &str, backend: &str) -> Player {
    Player {
        name: name.into(),
        uuid: [0; 16],
        backend: backend.into(),
        address: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 50000),
        joined: Instant::now(),
    }
}

/// Keeps a place on `backend`, as a player already there would.
fn hold(sessions: &Sessions, config: &Config, backend: &str) -> Reservation {
    let limits = config.balanced(backend).unwrap();
    sessions
        .balancer
        .reserve_named(
            Server {
                backend,
                soft_cap: limits.soft_cap,
                hard_cap: limits.hard_cap,
                available: true,
                ping: None,
                reported_players: None,
            },
            || sessions.counts(),
        )
        .unwrap()
}

struct Recorded {
    moves: Vec<(String, String)>,
    sleeps: usize,
    outcome: Outcome,
}

/// Evacuates `from` with a mover that records where each player went, and fails for
/// the players named in `failing`.
fn evacuate(
    config: &Config,
    sessions: &Sessions,
    from: &str,
    players: &[Player],
    failing: &[&str],
) -> Recorded {
    evacuate_with(config, sessions, from, players, failing, &[])
}

/// The same, with players the fallback has already moved: `gone` names each with the server
/// it is on now.
fn evacuate_with(
    config: &Config,
    sessions: &Sessions,
    from: &str,
    players: &[Player],
    failing: &[&str],
    gone: &[(&str, &str)],
) -> Recorded {
    let shared = sessions.shared();
    let moves = RefCell::new(Vec::new());
    let sleeps = RefCell::new(0usize);
    let outcome = run(
        config,
        &shared,
        from,
        players,
        &mut |p: &Player, target: &str| {
            // The place reserved on the target is held while the move goes on.
            assert!(sessions.balancer.reserved(target) >= 1, "{target}");
            moves.borrow_mut().push((p.name.clone(), target.to_owned()));
            if let Some((_, now)) = gone.iter().find(|(name, _)| *name == p.name) {
                Err(Unmoved::Elsewhere((*now).to_owned()))
            } else if failing.contains(&p.name.as_str()) {
                Err(Unmoved::Failed(format!("{} could not move", p.name)))
            } else {
                Ok(format!("{} moved to {target}", p.name))
            }
        },
        Duration::from_millis(500),
        &mut |d| {
            assert_eq!(d, Duration::from_millis(500));
            *sleeps.borrow_mut() += 1;
        },
    );
    Recorded {
        moves: moves.into_inner(),
        sleeps: sleeps.into_inner(),
        outcome,
    }
}

fn pairs(moves: &[(String, String)]) -> Vec<(&str, &str)> {
    moves
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect()
}

#[test]
fn players_go_one_at_a_time_to_the_servers_of_the_same_role_with_a_pause_between() {
    let config = config("");
    let sessions = Sessions::default();
    let players = [
        player("Alice", "a"),
        player("Bob", "a"),
        player("Carol", "a"),
    ];
    let done = evacuate(&config, &sessions, "a", &players, &[]);
    // Never to `a`, never to a game server. The mover does not change the sessions here,
    // so each move gives its place back and the first server listed is found again.
    assert_eq!(
        pairs(&done.moves),
        [("Alice", "b"), ("Bob", "b"), ("Carol", "b")]
    );
    assert_eq!(
        done.sleeps, 2,
        "a pause between two moves, none before the first"
    );
    assert_eq!(done.outcome.moved, 3);
    assert!(done.outcome.stayed.is_empty());
    assert_eq!(
        sessions.balancer.reserved("b"),
        0,
        "the places are given back"
    );
}

#[test]
fn a_game_server_sends_its_players_to_the_other_game_servers() {
    let config = config("");
    let sessions = Sessions::default();
    let done = evacuate(&config, &sessions, "g1", &[player("Dave", "g1")], &[]);
    assert_eq!(pairs(&done.moves), [("Dave", "g2")]);
}

#[test]
fn players_with_nowhere_to_go_stay_and_are_listed() {
    let config = config("");
    let sessions = Sessions::default();
    // b and c are at their hard cap (3): nobody can take a player of a.
    let _held: Vec<Reservation> = ["b", "b", "b", "c", "c", "c"]
        .iter()
        .map(|b| hold(&sessions, &config, b))
        .collect();
    let players = [player("Alice", "a"), player("Bob", "a")];
    let done = evacuate(&config, &sessions, "a", &players, &[]);
    assert!(done.moves.is_empty());
    assert_eq!(done.sleeps, 0);
    assert_eq!(done.outcome.moved, 0);
    assert_eq!(
        done.outcome.report("a"),
        [
            "a: 0 moved, 2 stayed",
            "  Alice stays on a: no other server of role lobby has room",
            "  Bob stays on a: no other server of role lobby has room",
        ]
    );
}

#[test]
fn a_lone_server_of_its_role_has_nowhere_to_send_anyone() {
    let mut config = config("");
    config.balanced.retain(|b| b.backend != "g2");
    let sessions = Sessions::default();
    let done = evacuate(&config, &sessions, "g1", &[player("Dave", "g1")], &[]);
    assert!(done.moves.is_empty());
    assert_eq!(done.outcome.stayed.len(), 1);
}

#[test]
fn a_move_that_fails_leaves_the_player_and_the_others_go_on() {
    let config = config("");
    let sessions = Sessions::default();
    let players = [player("Alice", "a"), player("Bob", "a")];
    let done = evacuate(&config, &sessions, "a", &players, &["Alice"]);
    assert_eq!(pairs(&done.moves), [("Alice", "b"), ("Bob", "b")]);
    assert_eq!(done.outcome.moved, 1);
    assert_eq!(
        done.outcome.report("a"),
        [
            "Alice could not move",
            "Bob moved to b",
            "a: 1 moved, 1 stayed",
            "  Alice stays on a: the move failed",
        ]
    );
    assert_eq!(sessions.balancer.reserved("b"), 0);
}

/// The server stopped while the evacuation went on: the fallback moved Bob and Carol before
/// their turn came. They did not stay and nothing failed, so they are counted apart, with the
/// server they are on, and the counts keep their old words when nobody was moved by it.
#[test]
fn players_the_fallback_already_moved_are_counted_apart_and_do_not_stay() {
    let config = config("");
    let sessions = Sessions::default();
    let players = [
        player("Alice", "a"),
        player("Bob", "a"),
        player("Carol", "a"),
    ];
    let done = evacuate_with(
        &config,
        &sessions,
        "a",
        &players,
        &[],
        &[("Bob", "b"), ("Carol", "c")],
    );
    assert_eq!(done.outcome.moved, 1);
    assert!(done.outcome.stayed.is_empty());
    assert_eq!(
        done.outcome.already,
        [
            ("Bob".to_owned(), "b".to_owned()),
            ("Carol".to_owned(), "c".to_owned()),
        ]
    );
    assert_eq!(
        done.outcome.counts(),
        "1 moved, 0 stayed, 2 already moved by the fallback"
    );
    assert_eq!(
        done.outcome.report("a"),
        [
            "Alice moved to b",
            "a: 1 moved, 0 stayed, 2 already moved by the fallback",
            "  Bob is already on b (moved by the fallback)",
            "  Carol is already on c (moved by the fallback)",
        ]
    );
    assert_eq!(sessions.balancer.reserved("b"), 0);
}

/// A failed move and a player the fallback moved are not mixed: the first stays, the second
/// does not.
#[test]
fn a_failed_move_still_stays_next_to_a_player_the_fallback_moved() {
    let config = config("");
    let sessions = Sessions::default();
    let players = [player("Alice", "a"), player("Bob", "a")];
    let done = evacuate_with(
        &config,
        &sessions,
        "a",
        &players,
        &["Alice"],
        &[("Bob", "b")],
    );
    assert_eq!(
        done.outcome.report("a"),
        [
            "Alice could not move",
            "a: 0 moved, 1 stayed, 1 already moved by the fallback",
            "  Alice stays on a: the move failed",
            "  Bob is already on b (moved by the fallback)",
        ]
    );
}

/// The case seen on a real network: the evacuation chose `b` for a player it saw on `a`, but
/// the fallback had put the player on `b` already. The switch refuses ("already on b"), and
/// the evacuation knows from the session that the player is elsewhere, not that the move
/// failed.
#[test]
fn a_player_found_on_the_target_by_the_move_is_one_the_fallback_moved() {
    let config = config("");
    let sessions = Sessions::default();
    // The session is on `b` now; the evacuation of `a` saw it on `a`.
    let (_relay, handle, _player, _server) = idle_session_on("Alice", "b");
    let _open = sessions.open(player("Alice", "b"), handle).unwrap();
    assert_eq!(
        move_one(&sessions, &config, &player("Alice", "a"), "b"),
        Err(Unmoved::Elsewhere("b".to_owned()))
    );
}

/// A move that fails for another reason is still a failed move: the player is where the
/// evacuation saw them.
#[test]
fn a_move_that_fails_for_its_own_reason_is_not_taken_for_the_fallback() {
    let config = config("");
    let sessions = Sessions::default();
    let (_relay, handle, _player, _server) = idle_session_on("Alice", "a");
    let _open = sessions.open(player("Alice", "a"), handle).unwrap();
    // The configuration has no sidecar, so the switch refuses before it asks anything.
    let moved = move_one(&sessions, &config, &player("Alice", "a"), "b");
    let Err(Unmoved::Failed(why)) = moved else {
        panic!("the move should have failed");
    };
    assert!(why.contains("no sidecar"), "{why}");
}

#[test]
fn only_the_players_of_the_drained_backend_are_moved() {
    let config = config("");
    let sessions = Sessions::default();
    let players = [player("Alice", "a"), player("Erin", "b")];
    let done = evacuate(&config, &sessions, "a", &players, &[]);
    assert_eq!(pairs(&done.moves), [("Alice", "b")]);
}

/// A backend for the other kind of player is never a target: the players of `a` are
/// unverified, and `c` is only for verified players, even when `b` is full.
#[test]
fn a_backend_for_the_other_kind_of_player_is_never_the_target() {
    let config = config(
        r#"
[[group]]
name = "guests"
auth = "unverified"
backends = ["a", "b"]
[[group]]
name = "accounts"
auth = "verified"
backends = ["c"]
[[route]]
auth = "unverified"
group = "guests"
[[route]]
auth = "verified"
group = "accounts"
"#,
    );
    let sessions = Sessions::default();
    let _held: Vec<Reservation> = ["b", "b", "b"]
        .iter()
        .map(|b| hold(&sessions, &config, b))
        .collect();
    let done = evacuate(&config, &sessions, "a", &[player("Alice", "a")], &[]);
    assert!(done.moves.is_empty());
    assert_eq!(done.outcome.stayed.len(), 1);
}

/// Nobody on the backend: there is nothing to send, and the console says so.
#[test]
fn send_away_with_nobody_on_the_server_is_nothing() {
    let config = config("");
    let sessions = Sessions::default();
    assert!(send_away(&sessions, &config, "a").is_none());
}
