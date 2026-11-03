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
        reported_players: None,
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

// ---- What the backends report of themselves ----

use meridian_sidecar::request::ServerStatus;

fn report(sessions: &Sessions, backend: &str, status: ServerStatus) {
    sessions.statuses.record(backend, status, Instant::now());
}

fn accepting(players: u64, capacity: u64) -> ServerStatus {
    ServerStatus {
        players: Some(players),
        capacity: Some(capacity),
        accepts_transfers: Some(true),
    }
}

fn up() -> Vec<(&'static str, bool)> {
    vec![(LOBBY, true), (EAST, true), (WEST, true)]
}

/// A lobby that reports it does not accept transfers is not offered, whatever its
/// count; its place goes to the next.
#[test]
fn a_lobby_that_does_not_accept_transfers_is_not_offered() {
    let config = balanced(true, 5, 5);
    let sessions = Sessions::default();
    report(
        &sessions,
        "lobby",
        ServerStatus {
            accepts_transfers: Some(false),
            ..accepting(0, 20)
        },
    );
    let (tried, _) = login_one(&config, &sessions, up(), None, vec![Step::ExpectClose]);
    assert_eq!(tried, [EAST]);
}

/// A report that left `accepts_transfers` out says nothing against the backend.
#[test]
fn a_report_that_leaves_out_accepts_transfers_does_not_refuse() {
    let config = balanced(true, 5, 5);
    let sessions = Sessions::default();
    report(
        &sessions,
        "lobby",
        ServerStatus {
            accepts_transfers: None,
            ..accepting(0, 20)
        },
    );
    let (tried, _) = login_one(&config, &sessions, up(), None, vec![Step::ExpectClose]);
    assert_eq!(tried, [LOBBY]);
}

/// A backend that reported once and then fell silent past the maximum age is not
/// believed, so it is not offered; one that answers again is.
#[test]
fn a_stale_report_takes_the_lobby_out_until_it_answers_again() {
    let config = balanced(true, 5, 5);
    let sessions = Sessions::default();
    let old = Instant::now()
        .checked_sub(Duration::from_secs(
            config.balancing.state_max_age_seconds + 5,
        ))
        .expect("the clock has run long enough");
    sessions.statuses.record("lobby", accepting(0, 20), old);
    let (tried, _) = login_one(&config, &sessions, up(), None, vec![Step::ExpectClose]);
    assert_eq!(tried, [EAST]);
    report(&sessions, "lobby", accepting(0, 20));
    let (tried, _) = login_one(&config, &sessions, up(), None, vec![Step::ExpectClose]);
    assert_eq!(tried, [LOBBY]);
}

/// A backend that never reported is treated as accepting, as it is without balancing.
#[test]
fn a_backend_that_never_reported_is_treated_as_accepting() {
    let config = balanced(true, 5, 5);
    let sessions = Sessions::default();
    let (tried, _) = login_one(&config, &sessions, up(), None, vec![Step::ExpectClose]);
    assert_eq!(tried, [LOBBY]);
}

/// The backend has players the proxy did not send: the larger count is used, so the
/// lobby is full for the balancer even though the proxy counts none on it.
#[test]
fn the_players_a_backend_reports_count_when_there_are_more_than_the_proxy_sent() {
    let config = balanced(true, 5, 5);
    let sessions = Sessions::default();
    report(&sessions, "lobby", accepting(5, 20));
    let (tried, _) = login_one(&config, &sessions, up(), None, vec![Step::ExpectClose]);
    assert_eq!(tried, [EAST]);
    // Fewer than the proxy counts changes nothing: its own count stands.
    let _held = [
        hold(&sessions, &config, "east"),
        hold(&sessions, &config, "east"),
    ];
    report(&sessions, "east", accepting(0, 20));
    report(&sessions, "lobby", accepting(0, 20));
    let (tried, _) = login_one(&config, &sessions, up(), None, vec![Step::ExpectClose]);
    assert_eq!(tried, [EAST], "east has two, the fullest");
}

/// A backend that can take fewer players than the caps written is held to what it
/// says: its capacity lowers its hard cap, and its soft cap with it.
#[test]
fn the_capacity_a_backend_reports_lowers_its_caps() {
    let config = balanced(true, 5, 5);
    let sessions = Sessions::default();
    report(&sessions, "lobby", accepting(0, 1));
    let _held = hold(&sessions, &config, "lobby");
    let (tried, _) = login_one(&config, &sessions, up(), None, vec![Step::ExpectClose]);
    assert_eq!(tried, [EAST], "lobby holds one and can take one");
}

/// A capacity above the caps written raises nothing: the caps of the file are the
/// operator's choice.
#[test]
fn a_larger_capacity_does_not_raise_the_caps() {
    let config = balanced(true, 1, 1);
    let sessions = Sessions::default();
    report(&sessions, "lobby", accepting(0, 100));
    let _held = hold(&sessions, &config, "lobby");
    let (tried, _) = login_one(&config, &sessions, up(), None, vec![Step::ExpectClose]);
    assert_eq!(tried, [EAST]);
}

/// A forced host respects the report too: a backend that does not accept transfers
/// does not take the player, even named on purpose.
#[test]
fn a_forced_host_does_not_reach_a_backend_that_does_not_accept_transfers() {
    let mut config = balanced(true, 5, 5);
    forced_east(&mut config);
    let sessions = Sessions::default();
    report(
        &sessions,
        "east",
        ServerStatus {
            accepts_transfers: Some(false),
            ..accepting(0, 20)
        },
    );
    let (tried, _) = login_one(
        &config,
        &sessions,
        up(),
        Some("east.example.com"),
        vec![Step::ExpectClose],
    );
    assert_eq!(tried, [WEST]);
}

/// With balancing off a report changes nothing at login.
#[test]
fn with_balancing_off_a_report_changes_nothing() {
    let config = balanced(false, 5, 5);
    let sessions = Sessions::default();
    report(
        &sessions,
        "lobby",
        ServerStatus {
            accepts_transfers: Some(false),
            ..accepting(0, 20)
        },
    );
    let (tried, _) = login_one(&config, &sessions, up(), None, vec![Step::ExpectClose]);
    assert_eq!(tried, [LOBBY]);
}

// ---- Ping and drain ----

fn ping(sessions: &Sessions, backend: &str, ms: u64) {
    sessions
        .statuses
        .record_ping(backend, Duration::from_millis(ms), Instant::now());
}

/// Two lobbies with the same number of players: the one the proxy measured as the
/// quicker takes the login, whatever the order they are listed in.
#[test]
fn the_quicker_lobby_takes_a_tie() {
    let config = balanced(true, 5, 5);
    let sessions = Sessions::default();
    ping(&sessions, "lobby", 40);
    ping(&sessions, "east", 5);
    ping(&sessions, "west", 90);
    let (tried, _) = login_one(&config, &sessions, up(), None, vec![Step::ExpectClose]);
    assert_eq!(tried, [EAST]);
}

/// The ping only breaks a tie: a fuller lobby is still preferred to a quicker one.
#[test]
fn the_ping_does_not_beat_the_fill_rule_at_login() {
    let config = balanced(true, 5, 5);
    let sessions = Sessions::default();
    ping(&sessions, "lobby", 80);
    ping(&sessions, "east", 1);
    let _held = hold(&sessions, &config, "lobby");
    let (tried, _) = login_one(&config, &sessions, up(), None, vec![Step::ExpectClose]);
    assert_eq!(tried, [LOBBY]);
}

/// With `tie-break = "order"` the measures are not used.
#[test]
fn the_order_breaks_the_tie_when_it_is_asked_for() {
    let mut config = balanced(true, 5, 5);
    config.balancing.tie_break = meridian_config::TieBreak::Order;
    let sessions = Sessions::default();
    ping(&sessions, "lobby", 80);
    ping(&sessions, "east", 1);
    let (tried, _) = login_one(&config, &sessions, up(), None, vec![Step::ExpectClose]);
    assert_eq!(tried, [LOBBY]);
}

/// A measure that grew old is not used, as a report that grew old is not believed.
#[test]
fn an_old_measure_is_not_used() {
    let config = balanced(true, 5, 5);
    let sessions = Sessions::default();
    let old = Instant::now()
        .checked_sub(Duration::from_secs(
            config.balancing.state_max_age_seconds + 5,
        ))
        .expect("the clock has run long enough");
    sessions
        .statuses
        .record_ping("east", Duration::from_millis(1), old);
    ping(&sessions, "lobby", 50);
    let (tried, _) = login_one(&config, &sessions, up(), None, vec![Step::ExpectClose]);
    assert_eq!(tried, [LOBBY], "east's measure is too old to rank it first");
}

/// A lobby the operator drained takes no new player, not even through a forced host.
#[test]
fn a_drained_lobby_takes_no_new_player_and_is_offered_again_when_undrained() {
    let mut config = balanced(true, 5, 5);
    forced_east(&mut config);
    let sessions = Sessions::default();
    sessions.balancer.drain("lobby");
    let (tried, _) = login_one(&config, &sessions, up(), None, vec![Step::ExpectClose]);
    assert_eq!(tried, [EAST]);
    sessions.balancer.drain("east");
    let (tried, _) = login_one(
        &config,
        &sessions,
        up(),
        Some("east.example.com"),
        vec![Step::ExpectClose],
    );
    assert_eq!(tried, [WEST], "the forced host's backend is drained too");
    assert!(sessions.balancer.undrain("lobby"));
    let (tried, _) = login_one(&config, &sessions, up(), None, vec![Step::ExpectClose]);
    assert_eq!(tried, [LOBBY]);
}

/// Every lobby drained: the player is refused and no lobby is tried by the ordinary
/// loop either.
#[test]
fn every_lobby_drained_leaves_no_room() {
    let config = balanced(true, 5, 5);
    let sessions = Sessions::default();
    for lobby in ["lobby", "east", "west"] {
        sessions.balancer.drain(lobby);
    }
    let (tried, trace) = login_one(&config, &sessions, up(), None, refused());
    assert!(tried.is_empty(), "{tried:?}");
    assert_eq!(refusal(&trace), Full::MESSAGE);
}

/// The same when every lobby says it does not accept transfers, or has not been heard
/// from for too long: none is offered, and the ordinary loop does not try them.
#[test]
fn lobbies_that_do_not_accept_transfers_or_are_not_heard_from_are_not_tried_at_all() {
    let config = balanced(true, 5, 5);
    let sessions = Sessions::default();
    let old = Instant::now()
        .checked_sub(Duration::from_secs(
            config.balancing.state_max_age_seconds + 5,
        ))
        .expect("the clock has run long enough");
    sessions
        .statuses
        .record("lobby", ServerStatus::default(), old);
    for lobby in ["east", "west"] {
        report(
            &sessions,
            lobby,
            ServerStatus {
                accepts_transfers: Some(false),
                ..accepting(0, 20)
            },
        );
    }
    let (tried, trace) = login_one(&config, &sessions, up(), None, refused());
    assert!(tried.is_empty(), "{tried:?}");
    assert_eq!(refusal(&trace), Full::MESSAGE);
}
