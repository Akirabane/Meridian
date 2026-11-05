//! A player whose server is lost in the game (docs/OPERATING.md, "When a server is lost").
use super::*;
use crate::fallback::{Context, replacer};
use horizon_protocol::{buf::Reader, ids, packets::play as packets};
use meridian_config::{AuthKind, BalancedServer, Balancing, Role};
use meridian_handoff::{Loss, Replacement};
use std::collections::VecDeque;

const LOBBY: &str = "127.0.0.1:25601";
const EAST: &str = "127.0.0.1:25602";
const WEST: &str = "127.0.0.1:25603";

/// Three servers; a player lost by `lobby` goes to `east`, then `west`.
fn listed() -> Config {
    Config {
        try_backends: vec!["east".into(), "west".into()],
        ..routed()
    }
}

/// What the replacer answered a loss of the server `lobby`, and the addresses it tried.
fn ask(
    config: &Config,
    sessions: &Sessions,
    up: Vec<(&str, bool)>,
    loss: Loss,
) -> (Result<Replacement, String>, Vec<String>) {
    let (connect, reachable) = reachable(up);
    let (_relay, handle, _player, _server) = idle_session_on("Alex", "lobby");
    let profile = Profile {
        name: "Alex".into(),
        uuid: offline_uuid("Alex").to_be_bytes(),
        properties: Vec::new(),
    };
    let mut replace = replacer(Context {
        sessions,
        connect: &*connect,
        config,
        handle: &handle,
        profile: &profile,
        virtual_host: None,
        auth: AuthKind::Unverified,
        peer: peer(),
    });
    let answer = replace(&loss);
    let tried = std::mem::take(&mut *reachable.tried.lock().unwrap());
    (answer, tried)
}

fn crash() -> Loss {
    Loss::Crashed("the connection closed".into())
}

#[test]
fn a_crash_sends_the_player_to_the_next_server_of_the_list() {
    let sessions = Sessions::default();
    let (answer, tried) = ask(
        &listed(),
        &sessions,
        vec![(LOBBY, true), (EAST, true), (WEST, true)],
        crash(),
    );
    // The server that was lost is not tried again, though it is the default one.
    assert_eq!(tried, [EAST]);
    let replacement = answer.unwrap();
    assert_eq!(replacement.backend, "east");
    let notice = replacement.notice.unwrap();
    assert!(
        notice.contains("lobby") && notice.contains("east"),
        "{notice}"
    );
}

#[test]
fn a_server_that_does_not_answer_is_skipped() {
    let sessions = Sessions::default();
    let (answer, tried) = ask(
        &listed(),
        &sessions,
        vec![(EAST, false), (WEST, true)],
        crash(),
    );
    assert_eq!(tried, [EAST, WEST]);
    assert_eq!(answer.unwrap().backend, "west");
}

#[test]
fn when_no_server_answers_the_player_is_told_so() {
    let sessions = Sessions::default();
    let (answer, tried) = ask(&listed(), &sessions, vec![], crash());
    assert_eq!(tried, [EAST, WEST]);
    let message = answer.err().unwrap();
    assert!(message.contains("lobby"), "{message}");
    assert!(
        message.contains("No other server could take you"),
        "{message}"
    );
}

#[test]
fn a_server_alone_has_no_other_to_send_the_player_to() {
    let sessions = Sessions::default();
    let (answer, tried) = ask(&routed(), &sessions, vec![(LOBBY, true)], crash());
    // Nothing is tried: the only server there is is the one that was lost.
    assert!(answer.is_err());
    assert!(tried.is_empty(), "{tried:?}");
}

#[test]
fn the_reason_of_a_kick_is_shown_whether_or_not_the_player_is_moved() {
    let sessions = Sessions::default();
    let kick = || Loss::Kicked("You have been kicked".into());
    let (moved, _) = ask(&listed(), &sessions, vec![(EAST, true)], kick());
    let notice = moved.unwrap().notice.unwrap();
    assert!(notice.contains("You have been kicked"), "{notice}");
    assert!(
        notice.contains("lobby") && notice.contains("east"),
        "{notice}"
    );
    let (stranded, _) = ask(&listed(), &sessions, vec![], kick());
    let message = stranded.err().unwrap();
    assert!(message.starts_with("You have been kicked"), "{message}");
}

#[test]
fn max_attempts_bounds_the_servers_tried() {
    let sessions = Sessions::default();
    let mut config = listed();
    config.fallback.max_attempts = 1;
    let (answer, tried) = ask(&config, &sessions, vec![(WEST, true)], crash());
    // `east` is the one attempt: it does not answer, and `west` is not tried.
    assert_eq!(tried, [EAST]);
    assert!(answer.is_err());
}

/// The balancer's rules hold for a player who is moved: a drained lobby is not offered.
#[test]
fn a_drained_lobby_is_not_offered() {
    let sessions = Sessions::default();
    let server = |backend: &str| BalancedServer {
        backend: backend.into(),
        role: Role::Lobby,
        soft_cap: 40,
        hard_cap: 50,
    };
    let config = Config {
        balanced: vec![server("lobby"), server("east"), server("west")],
        balancing: Balancing {
            enabled: true,
            ..Balancing::default()
        },
        ..listed()
    };
    assert!(sessions.balancer.drain("east"));
    let (answer, tried) = ask(
        &config,
        &sessions,
        vec![(LOBBY, true), (EAST, true), (WEST, true)],
        crash(),
    );
    assert_eq!(answer.unwrap().backend, "west");
    assert_eq!(tried, [WEST]);
}

// ---- Through the proxy, with a client and backends that run scripts.

/// A packet of the server's, as a step of a script.
fn server_packet(packet: &packets::Clientbound) -> Step {
    let body = packet.encode();
    let mut reader = Reader::new(&body);
    let id = reader.varint().unwrap();
    Step::send(id, reader.rest().to_vec())
}

/// Backends by address, each with the scripts of the connections it will take in turn; an
/// address with none (or none left) does not answer. Keeps the addresses asked for.
type Scripted = (
    Arc<Connect>,
    Arc<Mutex<Vec<String>>>,
    Arc<Mutex<Vec<BackendThread>>>,
);

fn scripted(backends: Vec<(&str, Vec<Vec<Step>>)>) -> Scripted {
    let scripts: Mutex<HashMap<String, VecDeque<Vec<Step>>>> = Mutex::new(
        backends
            .into_iter()
            .map(|(address, scripts)| (address.to_owned(), scripts.into()))
            .collect(),
    );
    let tried = Arc::new(Mutex::new(Vec::new()));
    let threads = Arc::new(Mutex::new(Vec::new()));
    let (asked, kept) = (Arc::clone(&tried), Arc::clone(&threads));
    let connect: Arc<Connect> = Arc::new(move |address, profile, options| {
        asked.lock().unwrap().push(address.to_owned());
        let script = scripts
            .lock()
            .unwrap()
            .get_mut(address)
            .and_then(VecDeque::pop_front);
        let Some(script) = script else {
            return Err(meridian_net::Error::Io(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                "refused",
            )));
        };
        let (proxy_end, server_end) = pipe();
        let fake = FakeBackend {
            forwarding_secret: Some(SECRET.as_bytes().to_vec()),
            after_login: script,
            ..FakeBackend::default()
        };
        kept.lock()
            .unwrap()
            .push(fake.spawn(server_end, Recorder::new()));
        backend::login(proxy_end, "127.0.0.1", 25601, profile, options)
    });
    (connect, tried, threads)
}

const FIRST: [u8; 60] = [6; 60];
const SECOND: [u8; 60] = [8; 60];

/// What a server says to a player who joins it: its configuration and the start of the world.
fn joins(registry: u8, world: [u8; 60]) -> Vec<Step> {
    use ids::{configuration as cfg, play};
    vec![
        Step::send(cfg::clientbound::REGISTRY_DATA, vec![registry; 100]),
        finish_configuration(),
        Step::Expect {
            id: cfg::serverbound::FINISH_CONFIGURATION,
        },
        Step::send(play::clientbound::LOGIN, world),
    ]
}

/// What the player sees of a server they join, and says back.
fn arrives(registry: u8, world: [u8; 60]) -> Vec<Step> {
    use ids::{configuration as cfg, play};
    vec![
        Step::expect_exactly(cfg::clientbound::REGISTRY_DATA, vec![registry; 100]),
        Step::Expect {
            id: cfg::clientbound::FINISH_CONFIGURATION,
        },
        acknowledge_configuration(),
        Step::expect_exactly(play::clientbound::LOGIN, world),
    ]
}

/// Runs a player through `serve` with the client script `after_login`.
fn play_through(
    config: &Config,
    sessions: &Sessions,
    connect: &Arc<Connect>,
    threads: &Arc<Mutex<Vec<BackendThread>>>,
    after_login: Vec<Step>,
) {
    let (session, player) = logged_in("Alex", client::AuthKind::Offline, None);
    let player = meridian_testkit::peer::spawn(player, after_login, Dir::ToClient, Recorder::new());
    serve(session, peer(), config, &**connect, sessions);
    player.join().unwrap().unwrap();
    for thread in threads.lock().unwrap().drain(..) {
        thread.join().unwrap().unwrap();
    }
}

fn with_fallback(max_attempts: u32) -> Config {
    Config {
        try_backends: vec!["east".into(), "west".into()],
        fallback: meridian_config::Fallback {
            max_attempts,
            ..meridian_config::Fallback::default()
        },
        ..routed()
    }
}

/// The server of a player crashes: they go back to the configuration phase, join the next
/// server with a loading screen, and are told why in the chat.
#[test]
fn a_player_whose_server_crashes_is_moved_to_the_next() {
    use ids::{configuration as cfg, play};
    let mut lobby = joins(5, FIRST);
    lobby.push(Step::Close);
    let mut east = vec![Step::Expect {
        id: cfg::serverbound::CLIENT_INFORMATION,
    }];
    east.extend(joins(7, SECOND));
    east.push(Step::ExpectClose);
    let (connect, tried, threads) = scripted(vec![(LOBBY, vec![lobby]), (EAST, vec![east])]);
    let mut client = arrives(5, FIRST);
    client.extend([
        Step::Expect {
            id: play::clientbound::START_CONFIGURATION,
        },
        Step::Send {
            id: play::serverbound::CONFIGURATION_ACKNOWLEDGED,
            payload: Vec::new(),
        },
        Step::send(cfg::serverbound::CLIENT_INFORMATION, vec![4; 12]),
    ]);
    client.extend(arrives(7, SECOND));
    client.extend([
        Step::Expect {
            id: play::clientbound::SYSTEM_CHAT,
        },
        Step::Close,
    ]);
    let sessions = Sessions::default();
    play_through(&with_fallback(3), &sessions, &connect, &threads, client);
    assert_eq!(*tried.lock().unwrap(), [LOBBY, EAST]);
    assert_eq!(sessions.count(), 0);
}

/// A server that kicks the player: they are moved, and are not shown the disconnect.
#[test]
fn a_player_whose_server_kicks_them_is_moved_to_the_next() {
    use ids::{configuration as cfg, play};
    let kick = server_packet(&packets::Clientbound::Disconnect(
        horizon_protocol::packets::configuration::Disconnect {
            reason: text::literal("You have been kicked"),
        },
    ));
    let mut lobby = joins(5, FIRST);
    lobby.push(kick);
    lobby.push(Step::ExpectClose);
    let mut east = vec![Step::Expect {
        id: cfg::serverbound::CLIENT_INFORMATION,
    }];
    east.extend(joins(7, SECOND));
    east.push(Step::ExpectClose);
    let (connect, tried, threads) = scripted(vec![(LOBBY, vec![lobby]), (EAST, vec![east])]);
    let mut client = arrives(5, FIRST);
    // The next thing the player gets is the order to go back to the configuration phase.
    client.extend([
        Step::Expect {
            id: play::clientbound::START_CONFIGURATION,
        },
        Step::Send {
            id: play::serverbound::CONFIGURATION_ACKNOWLEDGED,
            payload: Vec::new(),
        },
        Step::send(cfg::serverbound::CLIENT_INFORMATION, vec![4; 12]),
    ]);
    client.extend(arrives(7, SECOND));
    client.extend([
        Step::Expect {
            id: play::clientbound::SYSTEM_CHAT,
        },
        Step::Close,
    ]);
    let sessions = Sessions::default();
    play_through(&with_fallback(3), &sessions, &connect, &threads, client);
    assert_eq!(*tried.lock().unwrap(), [LOBBY, EAST]);
}

/// Every server fails in turn, and none is tried twice: the player is disconnected with a
/// message, not passed back and forth.
#[test]
fn servers_that_fail_in_turn_do_not_make_a_loop() {
    use ids::{configuration as cfg, play};
    let mut lobby = joins(5, FIRST);
    lobby.push(Step::Close);
    // `east` takes the player and loses them at once. `west` does not answer.
    let mut east = vec![Step::Expect {
        id: cfg::serverbound::CLIENT_INFORMATION,
    }];
    east.extend(joins(7, SECOND));
    east.push(Step::Close);
    let (connect, tried, threads) = scripted(vec![(LOBBY, vec![lobby]), (EAST, vec![east])]);
    let mut client = arrives(5, FIRST);
    client.extend([
        Step::Expect {
            id: play::clientbound::START_CONFIGURATION,
        },
        Step::Send {
            id: play::serverbound::CONFIGURATION_ACKNOWLEDGED,
            payload: Vec::new(),
        },
        Step::send(cfg::serverbound::CLIENT_INFORMATION, vec![4; 12]),
    ]);
    client.extend(arrives(7, SECOND));
    client.extend([
        Step::Expect {
            id: play::clientbound::SYSTEM_CHAT,
        },
        // `east` is lost in its turn; `lobby` and `east` are out, `west` does not answer.
        Step::Expect {
            id: play::clientbound::DISCONNECT,
        },
        Step::ExpectClose,
    ]);
    let sessions = Sessions::default();
    play_through(&with_fallback(5), &sessions, &connect, &threads, client);
    let tried = tried.lock().unwrap().clone();
    assert_eq!(tried, [LOBBY, EAST, WEST], "each server once, none again");
}

/// The proxy ended the session: the player is not moved.
#[test]
fn a_kick_from_the_console_does_not_move_the_player() {
    use ids::play;
    let mut lobby = joins(5, FIRST);
    lobby.push(Step::ExpectClose);
    let (connect, tried, threads) = scripted(vec![(LOBBY, vec![lobby])]);
    let mut client = arrives(5, FIRST);
    client.extend([
        Step::Expect {
            id: play::clientbound::DISCONNECT,
        },
        Step::ExpectClose,
    ]);
    let sessions = Arc::new(Sessions::default());
    let kicker = {
        let sessions = Arc::clone(&sessions);
        std::thread::spawn(move || {
            let until = Instant::now() + Duration::from_secs(5);
            while sessions.find("Alex").is_none() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(5));
            }
            // Let the player reach the play phase.
            std::thread::sleep(Duration::from_millis(300));
            sessions.kick("Alex", "Kicked from the console").is_some()
        })
    };
    play_through(&with_fallback(3), &sessions, &connect, &threads, client);
    assert!(kicker.join().unwrap());
    assert_eq!(*tried.lock().unwrap(), [LOBBY], "no other server was asked");
}

/// A player the proxy could not place at login is not in the game: nothing is made good for
/// them, and the servers are asked once each, as a login does.
#[test]
fn a_player_who_never_joined_is_not_moved() {
    let (connect, tried, threads) = scripted(vec![]);
    let client = vec![
        Step::Expect {
            id: ids::configuration::clientbound::DISCONNECT,
        },
        Step::Close,
    ];
    let sessions = Sessions::default();
    play_through(&with_fallback(3), &sessions, &connect, &threads, client);
    assert_eq!(*tried.lock().unwrap(), [LOBBY, EAST, WEST]);
}

// ---- With the scripted player of a real run, over TCP.

/// The scripted player of a real run follows the proxy's order to go back to the
/// configuration phase, joins the second server, and the proxy lists it there. `kick` makes
/// the first server disconnect the player instead of dropping the connection.
fn followed_over_tcp(kick: bool) {
    use crate::state::Proxy;
    use ids::configuration as cfg;
    use meridian_testkit::live::{End, Options, link, session_linked};
    use std::sync::atomic::AtomicBool;

    let mut lobby = vec![Step::Expect {
        id: cfg::serverbound::CLIENT_INFORMATION,
    }];
    lobby.extend(joins(5, FIRST));
    if kick {
        lobby.push(server_packet(&packets::Clientbound::Disconnect(
            horizon_protocol::packets::configuration::Disconnect {
                reason: text::literal("You have been kicked"),
            },
        )));
        lobby.push(Step::ExpectClose);
    } else {
        lobby.push(Step::Close);
    }
    let mut east = vec![Step::Expect {
        id: cfg::serverbound::CLIENT_INFORMATION,
    }];
    east.extend(joins(7, SECOND));
    east.push(Step::ExpectClose);
    let (connect, tried, threads) = scripted(vec![(LOBBY, vec![lobby]), (EAST, vec![east])]);
    let mut proxy = Proxy::new(
        with_fallback(3),
        "meridian.toml".into(),
        Arc::new(AtomicBool::new(false)),
    );
    proxy.connect = connect;
    let proxy = Arc::new(proxy);
    let listener = crate::serve::listen("127.0.0.1:0".parse().unwrap()).unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = {
        let proxy = Arc::clone(&proxy);
        std::thread::spawn(move || crate::serve::serve(listener, &proxy))
    };

    let (handle, link) = link();
    let moves = handle.moves.clone();
    let recorder = Recorder::new();
    let player = {
        let recorder = recorder.clone();
        std::thread::spawn(move || {
            session_linked(
                &Options {
                    host: "127.0.0.1".into(),
                    port,
                    name: "Eve".into(),
                    // Long enough for a slow machine; the test ends the session itself.
                    seconds: 30,
                },
                &recorder,
                &link,
            )
        })
    };
    // The proxy lists the player on the second server.
    let until = Instant::now() + Duration::from_secs(10);
    let on = |backend: &str| {
        proxy
            .sessions
            .players()
            .iter()
            .any(|p| p.name == "Eve" && p.backend == backend)
    };
    while !(on("east") && moves.done() == 1) && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(on("east"), "{:?}", proxy.sessions.players());
    assert_eq!(moves.done(), 1, "the player came back to the world once");
    assert!(!on("lobby"));

    // The player leaves when the test is done, cleanly: it was not disconnected on the way.
    handle.controller.leave().unwrap();
    assert_eq!(player.join().unwrap(), End::Clean);
    // The chat line that says why the player was moved was among what they were sent.
    assert!(
        recorder
            .trace()
            .ids(Dir::ToClient)
            .contains(&ids::play::clientbound::SYSTEM_CHAT),
        "no chat line"
    );
    assert_eq!(*tried.lock().unwrap(), [LOBBY, EAST]);
    proxy.stop();
    server.join().unwrap().unwrap();
    for thread in threads.lock().unwrap().drain(..) {
        let _ = thread.join();
    }
}

#[test]
fn the_scripted_player_follows_the_move_after_a_crash() {
    followed_over_tcp(false);
}

#[test]
fn the_scripted_player_follows_the_move_after_a_kick() {
    followed_over_tcp(true);
}
