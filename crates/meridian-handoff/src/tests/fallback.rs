//! A player whose backend is lost without the proxy having wanted it is moved to a
//! replacement, and is not otherwise.
use super::*;
use horizon_protocol::{packets::configuration::Disconnect, text};
use std::sync::mpsc;

const SAYING: &str = "You were moved";

/// A session whose player is on the source, the replacement it gets (once), and what it was
/// asked about the losses.
struct Moved {
    handle: SessionHandle,
    run: JoinHandle<meridian_net::Result<Report>>,
    player: Connection,
    source: Connection,
    second: Connection,
    losses: mpsc::Receiver<Loss>,
}

fn moved(fallback: Fallback, joined: bool) -> Moved {
    let (proxy_client, mut player) = pipe();
    let (proxy_source, mut source) = pipe();
    let (proxy_second, second) = pipe();
    let (session, handle) = Session::new(
        ClientSession {
            profile: profile(),
            auth: AuthKind::Offline,
            virtual_host: None,
            connection: proxy_client,
        },
        BackendLink {
            connection: proxy_source,
        },
        "source",
    )
    .unwrap();
    let (tell, losses) = mpsc::channel();
    let run = std::thread::spawn(move || {
        let mut link = Some(BackendLink {
            connection: proxy_second,
        });
        let mut replace = move |loss: &Loss| {
            tell.send(loss.clone()).unwrap();
            match link.take() {
                Some(link) => Ok(Replacement {
                    link,
                    backend: "fallback".into(),
                    notice: Some(SAYING.into()),
                    keep: None,
                }),
                None => Err("No other server could take you".to_owned()),
            }
        };
        session.run_with(fallback, &mut replace)
    });
    second.reader.set_read_timeout(Some(WAIT)).unwrap();
    if joined {
        join_source(&mut player, &mut source);
    } else {
        player.reader.set_read_timeout(Some(WAIT)).unwrap();
        source.reader.set_read_timeout(Some(WAIT)).unwrap();
    }
    Moved {
        handle,
        run,
        player,
        source,
        second,
        losses,
    }
}

const BOTH: Fallback = Fallback {
    on_kick: true,
    on_crash: true,
};

fn kick(source: &mut Connection, reason: &str) {
    let packet = play::Clientbound::Disconnect(Disconnect {
        reason: text::literal(reason),
    });
    source.send(&packet.encode()).unwrap();
}

fn reason_of(id: i32, payload: &[u8]) -> String {
    match decode(id, payload) {
        C::Disconnect(d) => text::plain(&d.reason, &Default::default()),
        other => panic!("{other:?}"),
    }
}

/// What the client and the replacement go through once the player is told to go back to the
/// configuration phase: the client acknowledges, sends its information, the replacement
/// sends its configuration, and the world begins. A play packet the client sent before it
/// had read the order is dropped.
fn arrive(player: &mut Connection, second: &mut Connection) {
    expect(player, pc::START_CONFIGURATION).unwrap();
    // The client had not read the order yet.
    send(player, ps::KEEP_ALIVE, &1u64.to_be_bytes());
    send(player, ps::CONFIGURATION_ACKNOWLEDGED, &[]);
    send(player, cfg::serverbound::CLIENT_INFORMATION, &INFORMATION);
    // The first thing the replacement hears is the client's configuration: neither the
    // stale play packet nor the acknowledgement.
    expect(second, cfg::serverbound::CLIENT_INFORMATION).unwrap();
    send(second, cfg::clientbound::REGISTRY_DATA, &REGISTRY);
    send(second, cfg::clientbound::FINISH_CONFIGURATION, &[]);
    expect(player, cfg::clientbound::REGISTRY_DATA).unwrap();
    expect(player, cfg::clientbound::FINISH_CONFIGURATION).unwrap();
    send(player, cfg::serverbound::FINISH_CONFIGURATION, &[]);
    expect(second, cfg::serverbound::FINISH_CONFIGURATION).unwrap();
    let (id, payload) = packet(&login(TARGET_ID, OVERWORLD));
    send(second, id, &payload);
    expect(player, pc::LOGIN).unwrap();
}

fn says(player: &mut Connection) -> String {
    let (id, payload) = player.recv().unwrap();
    match decode(id, &payload) {
        C::SystemChat(chat) => text::plain(&chat.content, &Default::default()),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_crashed_backend_moves_the_player_to_the_replacement() {
    let Moved {
        handle,
        run,
        mut player,
        source,
        mut second,
        losses,
    } = moved(BOTH, true);
    drop(source);
    arrive(&mut player, &mut second);
    assert_eq!(says(&mut player), SAYING);
    assert!(matches!(losses.recv().unwrap(), Loss::Crashed(_)));
    assert_eq!(handle.backend(), "fallback");
    assert_eq!(handle.phase(), meridian_net::relay::Phase::Play);
    // The session relays to the replacement now, both ways.
    send(&mut second, pc::KEEP_ALIVE, &7u64.to_be_bytes());
    assert_eq!(
        expect(&mut player, pc::KEEP_ALIVE).unwrap(),
        7u64.to_be_bytes()
    );
    send(&mut player, ps::KEEP_ALIVE, &7u64.to_be_bytes());
    assert_eq!(
        expect(&mut second, ps::KEEP_ALIVE).unwrap(),
        7u64.to_be_bytes()
    );
    // The replacement is lost in its turn, and there is no other: the player is told why.
    drop(second);
    let (id, payload) = player.recv().unwrap();
    assert_eq!(id, pc::DISCONNECT);
    assert_eq!(reason_of(id, &payload), "No other server could take you");
    assert!(player.recv().unwrap_err().is_end_of_stream());
    run.join().unwrap().unwrap();
    assert!(matches!(losses.recv().unwrap(), Loss::Crashed(_)));
}

#[test]
fn a_kick_moves_the_player_and_is_not_shown_as_a_disconnect() {
    let Moved {
        run,
        mut player,
        mut source,
        mut second,
        losses,
        ..
    } = moved(BOTH, true);
    kick(&mut source, "You have been kicked");
    // The first thing the client reads is the order to go back to the configuration phase.
    arrive(&mut player, &mut second);
    assert_eq!(says(&mut player), SAYING);
    assert_eq!(
        losses.recv().unwrap(),
        Loss::Kicked("You have been kicked".into())
    );
    drop(second);
    drop(source);
    let _ = run.join().unwrap();
}

#[test]
fn with_the_fallback_off_a_kick_and_a_crash_end_the_session_as_before() {
    for crash in [false, true] {
        let Moved {
            run,
            mut player,
            mut source,
            losses,
            ..
        } = moved(Fallback::default(), true);
        if crash {
            drop(source);
            assert!(player.recv().unwrap_err().is_end_of_stream());
        } else {
            kick(&mut source, "Bye");
            let (id, payload) = player.recv().unwrap();
            assert_eq!(id, pc::DISCONNECT);
            assert_eq!(reason_of(id, &payload), "Bye");
            // The backend closes after it disconnects a player.
            drop(source);
        }
        run.join().unwrap().unwrap();
        assert!(losses.try_recv().is_err(), "crash={crash}");
    }
}

#[test]
fn each_kind_of_loss_has_its_own_switch() {
    // Only crashes: a kick is shown.
    let Moved {
        run,
        mut player,
        mut source,
        losses,
        ..
    } = moved(
        Fallback {
            on_kick: false,
            on_crash: true,
        },
        true,
    );
    kick(&mut source, "Bye");
    let (id, payload) = player.recv().unwrap();
    assert_eq!(id, pc::DISCONNECT);
    assert_eq!(reason_of(id, &payload), "Bye");
    drop(source);
    run.join().unwrap().unwrap();
    assert!(losses.try_recv().is_err());
    // Only kicks: a crash ends the session.
    let Moved {
        run,
        mut player,
        source,
        losses,
        ..
    } = moved(
        Fallback {
            on_kick: true,
            on_crash: false,
        },
        true,
    );
    drop(source);
    assert!(player.recv().unwrap_err().is_end_of_stream());
    run.join().unwrap().unwrap();
    assert!(losses.try_recv().is_err());
}

/// The proxy ended the session: nothing is made good.
#[test]
fn an_end_the_proxy_asked_for_does_not_move_the_player() {
    let Moved {
        handle,
        run,
        mut player,
        source,
        losses,
        ..
    } = moved(BOTH, true);
    handle.disconnect(Disconnect {
        reason: text::literal("Kicked from the console"),
    });
    let (id, payload) = player.recv().unwrap();
    assert_eq!(reason_of(id, &payload), "Kicked from the console");
    assert!(player.recv().unwrap_err().is_end_of_stream());
    run.join().unwrap().unwrap();
    drop(source);
    assert!(losses.try_recv().is_err());
}

/// The player leaving is not a loss either.
#[test]
fn the_player_leaving_does_not_ask_for_a_replacement() {
    let Moved {
        run,
        player,
        mut source,
        losses,
        ..
    } = moved(BOTH, true);
    drop(player);
    run.join().unwrap().unwrap();
    assert!(source.recv().unwrap_err().is_end_of_stream());
    assert!(losses.try_recv().is_err());
}

/// A client still in the configuration phase cannot be taken back to its start.
#[test]
fn a_player_still_configuring_is_not_moved() {
    let Moved {
        run,
        mut player,
        source,
        losses,
        ..
    } = moved(BOTH, false);
    drop(source);
    assert!(player.recv().unwrap_err().is_end_of_stream());
    run.join().unwrap().unwrap();
    assert!(losses.try_recv().is_err());
}

/// A client that never acknowledges does not hold the replacement for ever.
#[test]
fn a_client_that_does_not_acknowledge_is_disconnected() {
    let Moved {
        run,
        mut player,
        source,
        second,
        losses,
        ..
    } = moved(BOTH, true);
    drop(source);
    expect(&mut player, pc::START_CONFIGURATION).unwrap();
    // The player is in the configuration phase by now: its disconnect is that phase's.
    let (id, _) = player.recv().unwrap();
    assert_eq!(id, cfg::clientbound::DISCONNECT);
    assert!(player.recv().unwrap_err().is_end_of_stream());
    run.join().unwrap().unwrap();
    assert!(matches!(losses.recv().unwrap(), Loss::Crashed(_)));
    drop(second);
}
