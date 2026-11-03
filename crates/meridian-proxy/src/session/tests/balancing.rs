//! Balancing at login (docs/LOAD-BALANCING.md).
use super::*;
use crate::balance::{NoRoom, Refused, Reservation, Server};
use meridian_config::{BalancedServer, Balancing, ForcedHost, Role, TieBreak};

const LOBBY: &str = "127.0.0.1:25601";
const EAST: &str = "127.0.0.1:25602";
const WEST: &str = "127.0.0.1:25603";

/// Three lobbies, `lobby`, `east` and `west`, each with the same caps. Players come
/// to `lobby` first and fall back to the others in `try`.
fn balanced(enabled: bool, soft: u32, hard: u32) -> Config {
    let server = |backend: &str| BalancedServer {
        backend: backend.into(),
        role: Role::Lobby,
        soft_cap: soft,
        hard_cap: hard,
    };
    Config {
        try_backends: vec!["east".into(), "west".into()],
        balanced: vec![server("lobby"), server("east"), server("west")],
        balancing: Balancing {
            enabled,
            ..Balancing::default()
        },
        ..routed()
    }
}

fn server<'a>(config: &Config, backend: &'a str) -> Server<'a> {
    let limits = config.balanced(backend).unwrap();
    Server {
        backend,
        soft_cap: limits.soft_cap,
        hard_cap: limits.hard_cap,
        available: true,
        ping: None,
    }
}

/// Keeps a place on `backend`, as a player already there would.
fn hold<'a>(sessions: &'a Sessions, config: &Config, backend: &str) -> Reservation<'a> {
    sessions
        .balancer
        .reserve_named(server(config, backend), || sessions.counts())
        .unwrap_or_else(|e| panic!("cannot hold a place on {backend}: {e}"))
}

fn login_one(
    config: &Config,
    sessions: &Sessions,
    up: Vec<(&str, bool)>,
    host: Option<&str>,
    script: Vec<Step>,
) -> (Vec<String>, Trace) {
    let (connect, backends) = reachable(up);
    route_one(
        config,
        sessions,
        &*connect,
        &backends,
        logged_in("Alex", client::AuthKind::Offline, host),
        script,
    )
}

fn forced_east(config: &mut Config) {
    config.forced_hosts = vec![ForcedHost {
        host: "east.example.com".into(),
        backend: "east".into(),
    }];
}

/// With balancing off the roles change nothing: the default backend is tried, and
/// only it, however many places the balancer would count on it.
#[test]
fn balancing_off_keeps_the_way_logins_were_routed() {
    let config = balanced(false, 1, 1);
    let sessions = Sessions::default();
    let _held = hold(&sessions, &config, "lobby");
    let (tried, _) = login_one(
        &config,
        &sessions,
        vec![(LOBBY, true), (EAST, true)],
        None,
        vec![Step::ExpectClose],
    );
    assert_eq!(tried, [LOBBY]);
}

/// A configuration with no roles at all tries the candidates as before when the
/// first is down.
#[test]
fn a_configuration_without_roles_is_routed_as_before() {
    let config = Config {
        try_backends: vec!["east".into()],
        ..routed()
    };
    let sessions = Sessions::default();
    let (tried, _) = login_one(
        &config,
        &sessions,
        vec![(LOBBY, false), (EAST, true)],
        None,
        vec![Step::ExpectClose],
    );
    assert_eq!(tried, [LOBBY, EAST]);
}

/// An empty lobby takes the player: the first listed, as nothing tells them apart.
#[test]
fn a_login_goes_to_a_lobby_with_room() {
    let config = balanced(true, 2, 3);
    let sessions = Sessions::default();
    let (tried, _) = login_one(
        &config,
        &sessions,
        vec![(LOBBY, true)],
        None,
        vec![Step::ExpectClose],
    );
    assert_eq!(tried, [LOBBY]);
    assert_eq!(sessions.admitted(), 0);
}

/// The first lobby is at its soft cap: the player goes to the next one, which is
/// opened.
#[test]
fn the_next_lobby_is_opened_at_the_soft_cap() {
    let config = balanced(true, 1, 2);
    let sessions = Sessions::default();
    let _held = hold(&sessions, &config, "lobby");
    let (tried, _) = login_one(
        &config,
        &sessions,
        vec![(LOBBY, true), (EAST, true)],
        None,
        vec![Step::ExpectClose],
    );
    assert_eq!(tried, [EAST]);
}

/// The fullest open lobby is chosen: a player joins the lobby that already has
/// people rather than an empty one listed first.
#[test]
fn the_fullest_open_lobby_takes_the_login() {
    let config = balanced(true, 5, 5);
    let sessions = Sessions::default();
    let _held = [
        hold(&sessions, &config, "east"),
        hold(&sessions, &config, "east"),
        hold(&sessions, &config, "west"),
    ];
    let (tried, _) = login_one(
        &config,
        &sessions,
        vec![(LOBBY, true), (EAST, true), (WEST, true)],
        None,
        vec![Step::ExpectClose],
    );
    assert_eq!(tried, [EAST]);
}

/// Every lobby is at its soft cap but not its hard cap: the emptiest takes the
/// overflow.
#[test]
fn the_overflow_goes_past_the_soft_cap_up_to_the_hard_cap() {
    let config = balanced(true, 1, 2);
    let sessions = Sessions::default();
    let _held = [
        hold(&sessions, &config, "lobby"),
        hold(&sessions, &config, "lobby"),
        hold(&sessions, &config, "east"),
        hold(&sessions, &config, "west"),
    ];
    let (tried, _) = login_one(
        &config,
        &sessions,
        vec![(LOBBY, true), (EAST, true), (WEST, true)],
        None,
        vec![Step::ExpectClose],
    );
    // East and west tie on one player: the first listed.
    assert_eq!(tried, [EAST]);
}

/// Every lobby has its hard cap: the player is told the server is full, no backend
/// is connected to, and the ordinary loop does not try them either.
#[test]
fn a_player_is_told_the_server_is_full_when_every_lobby_has_its_hard_cap() {
    let config = balanced(true, 1, 1);
    let sessions = Sessions::default();
    let _held = [
        hold(&sessions, &config, "lobby"),
        hold(&sessions, &config, "east"),
        hold(&sessions, &config, "west"),
    ];
    let (tried, trace) = login_one(
        &config,
        &sessions,
        vec![(LOBBY, true), (EAST, true), (WEST, true)],
        None,
        refused(),
    );
    assert!(tried.is_empty(), "{tried:?}");
    assert_eq!(refusal(&trace), Full::MESSAGE);
    assert_eq!(sessions.admitted(), 0);
}

/// A lobby that does not answer: the player goes to the next, and the place
/// reserved on the first is given back.
#[test]
fn a_lobby_that_does_not_answer_is_left_for_the_next_and_its_place_is_given_back() {
    let config = balanced(true, 2, 2);
    let sessions = Sessions::default();
    let _held = hold(&sessions, &config, "east");
    let (tried, _) = login_one(
        &config,
        &sessions,
        vec![(LOBBY, true), (EAST, false), (WEST, true)],
        None,
        vec![Step::ExpectClose],
    );
    // East is the fullest, so it comes first and fails; lobby is next.
    assert_eq!(tried, [EAST, LOBBY]);
    // Only the held place is left: east has room for one more up to its hard cap.
    assert!(
        sessions
            .balancer
            .reserve_named(server(&config, "east"), || sessions.counts())
            .is_ok()
    );
}

/// None of the lobbies answers: the player is told the servers cannot be reached,
/// not that the server is full, and each lobby is tried once.
#[test]
fn a_player_is_told_the_servers_cannot_be_reached_when_none_answers() {
    let config = balanced(true, 2, 2);
    let sessions = Sessions::default();
    let (tried, trace) = login_one(
        &config,
        &sessions,
        vec![(LOBBY, false), (EAST, false), (WEST, false)],
        None,
        refused(),
    );
    assert_eq!(tried.len(), 3, "each lobby once: {tried:?}");
    let reason = refusal(&trace);
    assert!(reason.starts_with("Cannot reach the servers"), "{reason}");
}

/// A server named by a forced host may pass its soft cap, never its hard cap.
#[test]
fn a_forced_host_may_pass_the_soft_cap_but_not_the_hard_cap() {
    let mut config = balanced(true, 1, 2);
    forced_east(&mut config);
    let sessions = Sessions::default();
    let _held = hold(&sessions, &config, "east");
    let up = || vec![(LOBBY, true), (EAST, true), (WEST, true)];
    // East is at its soft cap: the forced host still takes the player.
    let (tried, _) = login_one(
        &config,
        &sessions,
        up(),
        Some("east.example.com"),
        vec![Step::ExpectClose],
    );
    assert_eq!(tried, [EAST]);
    // East is at its hard cap: the forced host does not take the player, and the
    // next lobby the balancer finds in `try` does.
    let _more = hold(&sessions, &config, "east");
    let (tried, _) = login_one(
        &config,
        &sessions,
        up(),
        Some("east.example.com"),
        vec![Step::ExpectClose],
    );
    assert_eq!(tried, [WEST]);
}

/// A forced host that leads to a backend with no role is left to the ordinary
/// loop, even when balancing is on.
#[test]
fn a_forced_host_to_a_backend_without_a_role_is_routed_as_before() {
    let mut config = balanced(true, 1, 1);
    config.balanced.retain(|b| b.backend != "east");
    forced_east(&mut config);
    let sessions = Sessions::default();
    let _held = hold(&sessions, &config, "lobby");
    let (tried, _) = login_one(
        &config,
        &sessions,
        vec![(LOBBY, true), (EAST, true)],
        Some("east.example.com"),
        vec![Step::ExpectClose],
    );
    assert_eq!(tried, [EAST]);
}

/// A backend that is not a lobby is not placed at login: the ordinary loop tries
/// it after the lobbies.
#[test]
fn only_lobbies_are_placed_at_login() {
    let mut config = balanced(true, 1, 1);
    for b in &mut config.balanced {
        if b.backend == "lobby" {
            b.role = Role::Game;
        }
    }
    let sessions = Sessions::default();
    let (tried, _) = login_one(
        &config,
        &sessions,
        vec![(LOBBY, true), (EAST, true), (WEST, true)],
        None,
        vec![Step::ExpectClose],
    );
    // East is the first lobby; the game server `lobby` is only a last resort.
    assert_eq!(tried, [EAST]);
}

/// Players logging in at the same time never pass the hard cap: a place is
/// reserved together with the count, so no two see the same free place.
#[test]
fn logins_at_the_same_time_do_not_pass_the_hard_cap() {
    let config = balanced(true, 3, 3);
    let sessions = Sessions::default();
    let names = ["lobby", "east", "west"];
    let results: Vec<Result<Reservation<'_>, NoRoom>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..24)
            .map(|_| {
                scope.spawn(|| {
                    let servers: Vec<Server<'_>> =
                        names.iter().map(|b| server(&config, b)).collect();
                    sessions
                        .balancer
                        .reserve(&servers, || sessions.counts(), 2, TieBreak::Order)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let placed: Vec<&str> = results
        .iter()
        .filter_map(|r| r.as_ref().ok())
        .map(|r| r.backend())
        .collect();
    // Three lobbies of 3: nine places, whatever the interleaving.
    assert_eq!(placed.len(), 9);
    for lobby in names {
        assert_eq!(placed.iter().filter(|p| **p == lobby).count(), 3, "{lobby}");
    }
}

#[test]
fn a_named_place_past_the_hard_cap_is_refused_with_its_reason() {
    let config = balanced(true, 1, 1);
    let sessions = Sessions::default();
    let _held = hold(&sessions, &config, "lobby");
    let refused = sessions
        .balancer
        .reserve_named(server(&config, "lobby"), || sessions.counts());
    assert!(matches!(refused, Err(Refused::AtHardCap)));
}

/// The closed servers are kept between logins: a lobby that reached its soft cap is
/// closed, and stays closed until it falls back by the margin.
#[test]
fn a_lobby_closed_at_its_soft_cap_stays_closed_until_the_margin() {
    let mut config = balanced(true, 2, 4);
    config.balancing.margin = 1;
    let sessions = Sessions::default();
    let first = hold(&sessions, &config, "lobby");
    let second = hold(&sessions, &config, "lobby");
    let up = || vec![(LOBBY, true), (EAST, true), (WEST, true)];
    let (tried, _) = login_one(&config, &sessions, up(), None, vec![Step::ExpectClose]);
    assert_eq!(tried, [EAST]);
    assert!(sessions.balancer.is_closed("lobby"));
    // One player left: 1 is the reopening level (soft cap 2 minus margin 1), so the
    // lobby opens again and, being the fullest, takes the login.
    drop(second);
    let (tried, _) = login_one(&config, &sessions, up(), None, vec![Step::ExpectClose]);
    assert_eq!(tried, [LOBBY]);
    assert!(!sessions.balancer.is_closed("lobby"));
    drop(first);
}
