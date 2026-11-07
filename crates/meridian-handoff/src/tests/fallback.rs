//! A player whose backend is lost without the proxy having wanted it is moved to a
//! replacement, and is not otherwise.
use super::*;
use crate::Notice;
use horizon_protocol::{packets::configuration::Disconnect, text};
use horizon_world::nbt::Tag;
use std::sync::atomic::{AtomicUsize, Ordering};
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
    /// How many times the replacement said that the player's game followed.
    followed: Arc<AtomicUsize>,
}

fn moved(fallback: Fallback, joined: bool) -> Moved {
    moved_noticing(fallback, joined, |_| SAYING.into())
}

/// As [`moved`], with the notice the replacement gives for a loss.
fn moved_noticing(fallback: Fallback, joined: bool, notice: fn(&Loss) -> Notice) -> Moved {
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
    let followed = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&followed);
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
                    notice: Some(notice(loss)),
                    keep: None,
                    on_active: Some(Box::new({
                        let counted = Arc::clone(&counted);
                        move || {
                            counted.fetch_add(1, Ordering::SeqCst);
                        }
                    })),
                }),
                None => Err("No other server could take you".into()),
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
        followed,
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

fn decode_disconnect(id: i32, payload: &[u8]) -> Disconnect {
    match decode(id, payload) {
        C::Disconnect(d) => d,
        other => panic!("{other:?}"),
    }
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
        followed,
    } = moved(BOTH, true);
    assert_eq!(followed.load(Ordering::SeqCst), 0);
    drop(source);
    arrive(&mut player, &mut second);
    assert_eq!(says(&mut player), SAYING);
    // The replacement said once that the player's game followed.
    assert_eq!(followed.load(Ordering::SeqCst), 1);
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

/// The reason of a kick that is a translation key reaches the client as a translation, in the
/// chat line that says why it was moved: the client has the words, the proxy does not.
#[test]
fn a_translated_reason_reaches_the_client_to_translate() {
    let Moved {
        run,
        mut player,
        mut source,
        mut second,
        losses,
        ..
    } = moved_noticing(BOTH, true, |loss| match loss {
        Loss::Kicked(reason) => Notice::around(
            "You were disconnected from source: ",
            reason,
            ". You were moved to fallback.",
        ),
        Loss::Crashed(_) => SAYING.into(),
    });
    let shutdown = "multiplayer.disconnect.server_shutdown";
    // The server's reason carries a click that would run a command as the player.
    let reason = Tag::compound([
        ("translate", text::literal(shutdown)),
        (
            "clickEvent",
            Tag::compound([
                ("action", text::literal("run_command")),
                ("value", text::literal("/op Mallory")),
            ]),
        ),
    ]);
    source
        .send(&play::Clientbound::Disconnect(Disconnect { reason }).encode())
        .unwrap();
    arrive(&mut player, &mut second);
    let (id, payload) = player.recv().unwrap();
    let language = text::Language::from([(shutdown.to_owned(), "Server closed".to_owned())]);
    match decode(id, &payload) {
        C::SystemChat(chat) => {
            // The client's own words, not the key.
            assert_eq!(
                text::plain(&chat.content, &language),
                "You were disconnected from source: Server closed. You were moved to fallback."
            );
            // The click did not follow the player to the server they were moved to.
            let keys = crate::reason::keys_of(&chat.content);
            assert!(!keys.contains("clickEvent"), "{keys:?}");
            // And a client without the key shows the key, as any client would.
            assert_eq!(
                text::plain(&chat.content, &Default::default()),
                format!(
                    "You were disconnected from source: {shutdown}. You were moved to fallback."
                )
            );
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(losses.recv().unwrap(), Loss::Kicked(_)));
    drop(second);
    drop(source);
    let _ = run.join().unwrap();
}

/// A kick that is held back (the proxy ended the session itself) is shown to the player as the
/// server sent it, a translation key still a key for the game to translate.
#[test]
fn a_kick_held_back_is_shown_as_the_server_sent_it() {
    use crate::session::{End, Lost};
    use std::sync::atomic::Ordering;
    let Moved {
        handle,
        run,
        mut player,
        source,
        second,
        ..
    } = moved(BOTH, true);
    handle.0.closing.store(true, Ordering::SeqCst);
    let shutdown = "multiplayer.disconnect.server_shutdown";
    let reason = crate::Reason::of(&Disconnect {
        reason: text::translatable(shutdown, &[]),
    });
    let mut never = |_: &Loss| Err(Notice::from("not asked"));
    let lost = crate::session::lost(&handle.0, End::Kicked(reason), &mut never);
    assert!(matches!(lost, Lost::Over(End::Closed)));
    let (id, payload) = player.recv().unwrap();
    assert_eq!(id, pc::DISCONNECT);
    let language = text::Language::from([(shutdown.to_owned(), "Server closed".to_owned())]);
    let Disconnect { reason } = decode_disconnect(id, &payload);
    assert_eq!(text::plain(&reason, &language), "Server closed");
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
        followed,
        ..
    } = moved(BOTH, true);
    drop(source);
    expect(&mut player, pc::START_CONFIGURATION).unwrap();
    // The player is in the configuration phase by now: its disconnect is that phase's.
    let (id, _) = player.recv().unwrap();
    assert_eq!(id, cfg::clientbound::DISCONNECT);
    assert!(player.recv().unwrap_err().is_end_of_stream());
    run.join().unwrap().unwrap();
    // A game that did not follow is not said to have.
    assert_eq!(followed.load(Ordering::SeqCst), 0);
    assert!(matches!(losses.recv().unwrap(), Loss::Crashed(_)));
    drop(second);
}

/// A client may acknowledge the order to go back to the configuration phase before the
/// session has closed the lost backend's connection, on a busy machine or a quick link.
/// The replacement is then already the active lane: the connection closed must still be
/// the lost one, and the player goes on with the replacement.
#[test]
fn an_acknowledgement_before_the_lost_connection_is_closed_keeps_the_replacement() {
    let Moved {
        handle,
        run,
        mut player,
        source,
        mut second,
        losses,
        followed: _,
    } = moved(BOTH, true);
    let (open, waits) = mpsc::channel();
    let (tells, closed) = mpsc::channel();
    *handle.0.moving_gate.lock().unwrap() = Some((waits, tells));
    drop(source);
    expect(&mut player, pc::START_CONFIGURATION).unwrap();
    send(&mut player, ps::CONFIGURATION_ACKNOWLEDGED, &[]);
    send(
        &mut player,
        cfg::serverbound::CLIENT_INFORMATION,
        &INFORMATION,
    );
    // The replacement hears the client: it is the active lane, and the session has not
    // closed the lost connection yet.
    expect(&mut second, cfg::serverbound::CLIENT_INFORMATION).unwrap();
    assert_eq!(handle.backend(), "fallback");
    open.send(()).unwrap();
    closed.recv_timeout(WAIT).unwrap();
    // The replacement is still there, both ways.
    send(&mut second, cfg::clientbound::REGISTRY_DATA, &REGISTRY);
    send(&mut second, cfg::clientbound::FINISH_CONFIGURATION, &[]);
    expect(&mut player, cfg::clientbound::REGISTRY_DATA).unwrap();
    expect(&mut player, cfg::clientbound::FINISH_CONFIGURATION).unwrap();
    send(&mut player, cfg::serverbound::FINISH_CONFIGURATION, &[]);
    expect(&mut second, cfg::serverbound::FINISH_CONFIGURATION).unwrap();
    let (id, payload) = packet(&login(TARGET_ID, OVERWORLD));
    send(&mut second, id, &payload);
    expect(&mut player, pc::LOGIN).unwrap();
    assert_eq!(says(&mut player), SAYING);
    send(&mut second, pc::KEEP_ALIVE, &7u64.to_be_bytes());
    assert_eq!(
        expect(&mut player, pc::KEEP_ALIVE).unwrap(),
        7u64.to_be_bytes()
    );
    assert!(matches!(losses.recv().unwrap(), Loss::Crashed(_)));
    drop(second);
    let _ = run.join();
}

/// A server that stops at once kicks its players while a switch is preparing to move one of
/// them. The switch gives up before the freeze, and the fallback moves that player as it
/// moves the others: the kick is not shown and the session goes on.
#[test]
fn a_kick_while_a_switch_prepares_moves_the_player_by_the_fallback() {
    let Moved {
        handle,
        run,
        mut player,
        source,
        mut second,
        losses,
        ..
    } = moved(BOTH, true);
    let log = Log::default();
    let (address, imported, _target) = target(TargetWorld::default());
    let source = Arc::new(Mutex::new(source));
    let kicker = Arc::clone(&source);
    let mut target_control =
        super::switch::target_control(&log, imported).on("expect", move || {
            kick(&mut kicker.lock().unwrap(), "Server closed");
        });
    let failure = switch(
        &handle,
        super::switch::target_backend(&address, &options()),
        &mut FakeControl::new("source", &log),
        &mut target_control,
        &Digests::default(),
        &settings(),
    )
    .unwrap_err();
    assert_eq!(
        (failure.step, failure.after),
        (Step::Prepare, After::Stayed),
        "{failure}"
    );
    // Nothing was frozen: the target was let go before any export.
    assert_eq!(log.lines(), ["target expect Some(10)", "target drop"]);
    arrive(&mut player, &mut second);
    assert_eq!(says(&mut player), SAYING);
    assert_eq!(losses.recv().unwrap(), Loss::Kicked("Server closed".into()));
    assert_eq!(handle.backend(), "fallback");
    drop(second);
    drop(source);
    let _ = run.join().unwrap();
}

/// The player's server kicks them once it has frozen them and given their record: the switch
/// goes on and hands them over to the target, and the fallback does not move them again.
#[test]
fn a_kick_after_the_freeze_lets_the_switch_hand_the_player_over() {
    let Moved {
        handle,
        run,
        mut player,
        source,
        losses,
        ..
    } = moved(BOTH, true);
    let log = Log::default();
    let (address, imported, _target) = target(TargetWorld::default());
    let source = Arc::new(Mutex::new(source));
    let kicker = Arc::clone(&source);
    let mut source_control = FakeControl::new("source", &log).on("export", move || {
        kick(&mut kicker.lock().unwrap(), "Server closed");
    });
    let mut target_control = super::switch::target_control(&log, imported);
    switch(
        &handle,
        super::switch::target_backend(&address, &options()),
        &mut source_control,
        &mut target_control,
        &Digests::default(),
        &settings(),
    )
    .unwrap();
    assert_eq!(handle.backend(), "target");
    let ids: Vec<i32> = after_switch(&mut player)
        .iter()
        .map(|(id, _)| *id)
        .collect();
    assert!(!ids.contains(&pc::DISCONNECT), "{ids:?}");
    assert!(!ids.contains(&pc::START_CONFIGURATION), "{ids:?}");
    // The fallback was never asked: the player is where the switch put them.
    assert!(losses.try_recv().is_err());
    assert_eq!(handle.backend(), "target");
    drop(player);
    drop(source);
    let _ = run.join().unwrap();
}
