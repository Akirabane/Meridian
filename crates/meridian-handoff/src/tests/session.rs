use super::*;

/// Without a switch, a session relays as `meridian-net`'s relay does: both ways,
/// and the session ends when the backend closes.
#[test]
fn a_session_relays_both_ways_and_ends_with_the_backend() {
    let Bench {
        handle,
        run,
        mut player,
        mut source,
    } = bench();
    assert_eq!(handle.phase(), meridian_net::relay::Phase::Play);
    send(&mut source, pc::KEEP_ALIVE, &3u64.to_be_bytes());
    assert_eq!(
        expect(&mut player, pc::KEEP_ALIVE).unwrap(),
        3u64.to_be_bytes()
    );
    send(&mut player, ps::KEEP_ALIVE, &3u64.to_be_bytes());
    assert_eq!(
        expect(&mut source, ps::KEEP_ALIVE).unwrap(),
        3u64.to_be_bytes()
    );
    drop(source);
    let report = run.join().unwrap().unwrap();
    assert!(player.recv().unwrap_err().is_end_of_stream());
    // The configuration and play packets of the bench, and the keep-alive.
    assert_eq!(
        report.to_client.packets,
        3 + source_world().len() as u64 + 1
    );
    assert!(handle.is_over());
}

/// The player leaving closes the backend.
#[test]
fn the_player_leaving_closes_the_backend() {
    let Bench {
        handle,
        run,
        player,
        mut source,
    } = bench();
    drop(player);
    run.join().unwrap().unwrap();
    assert!(source.recv().unwrap_err().is_end_of_stream());
    assert!(handle.is_over());
}

/// The commands the hook knows are answered in the chat and never reach the
/// backend.
#[test]
fn the_proxy_answers_its_commands() {
    let (proxy_client, mut player) = pipe();
    let (proxy_backend, mut source) = pipe();
    player.reader.set_read_timeout(Some(WAIT)).unwrap();
    source.reader.set_read_timeout(Some(WAIT)).unwrap();
    let (session, _handle) = Session::new(
        ClientSession {
            profile: profile(),
            auth: AuthKind::Offline,
            virtual_host: None,
            connection: proxy_client,
        },
        BackendLink {
            connection: proxy_backend,
        },
        "source",
    )
    .unwrap();
    let session = session.with_commands(Arc::new(|command: &str| {
        (command == "server").then(|| "on source".to_owned())
    }));
    let run = std::thread::spawn(move || session.run());
    send(&mut source, cfg::clientbound::FINISH_CONFIGURATION, &[]);
    expect(&mut player, cfg::clientbound::FINISH_CONFIGURATION).unwrap();
    send(&mut player, cfg::serverbound::FINISH_CONFIGURATION, &[]);
    expect(&mut source, cfg::serverbound::FINISH_CONFIGURATION).unwrap();
    let command = |text: &str| {
        play::Serverbound::ChatCommand(play::ChatCommand {
            command: text.into(),
        })
        .encode()
    };
    player.send(&command("server")).unwrap();
    let (id, payload) = player.recv().unwrap();
    match decode(id, &payload) {
        C::SystemChat(chat) => assert_eq!(
            horizon_protocol::text::plain(&chat.content, &Default::default()),
            "on source"
        ),
        other => panic!("{other:?}"),
    }
    player.send(&command("list")).unwrap();
    expect(&mut source, ps::CHAT_COMMAND).unwrap();
    drop(source);
    run.join().unwrap().unwrap();
}

/// A message is shown only in the play phase, and `try_tell` says whether it was: before the configuration is finished the client
/// reads no chat, nothing is written, and it says so; after, it is written and read.
#[test]
fn a_message_is_shown_only_in_the_play_phase_and_try_tell_says_whether_it_was() {
    let (proxy_client, mut player) = pipe();
    let (proxy_backend, mut source) = pipe();
    player.reader.set_read_timeout(Some(WAIT)).unwrap();
    source.reader.set_read_timeout(Some(WAIT)).unwrap();
    let (session, handle) = Session::new(
        ClientSession {
            profile: profile(),
            auth: AuthKind::Offline,
            virtual_host: None,
            connection: proxy_client,
        },
        BackendLink {
            connection: proxy_backend,
        },
        "source",
    )
    .unwrap();
    let run = std::thread::spawn(move || session.run());
    // Not in play yet: nothing is written, and it says so.
    assert!(!handle.try_tell("Too early."));
    send(&mut source, cfg::clientbound::FINISH_CONFIGURATION, &[]);
    expect(&mut player, cfg::clientbound::FINISH_CONFIGURATION).unwrap();
    // In play: written, and said.
    assert!(handle.try_tell("Hello."));
    let (id, payload) = player.recv().unwrap();
    match decode(id, &payload) {
        C::SystemChat(chat) => assert_eq!(
            horizon_protocol::text::plain(&chat.content, &Default::default()),
            "Hello."
        ),
        other => panic!("{other:?}"),
    }
    drop(source);
    run.join().unwrap().unwrap();
}

/// A command the hook takes (`TAKEN`) never reaches the backend, nothing is written for it at
/// once, and what the hook's threads tell the player afterwards is read, in order: the one that
/// ran while the command was being taken (kept until the reader is done with it) and the one
/// that comes later. The next command, which the hook leaves alone, reaches the backend first.
#[test]
fn a_command_the_proxy_takes_is_answered_later_and_never_reaches_the_backend() {
    let (proxy_client, mut player) = pipe();
    let (proxy_backend, mut source) = pipe();
    player.reader.set_read_timeout(Some(WAIT)).unwrap();
    source.reader.set_read_timeout(Some(WAIT)).unwrap();
    let (session, handle) = Session::new(
        ClientSession {
            profile: profile(),
            auth: AuthKind::Offline,
            virtual_host: None,
            connection: proxy_client,
        },
        BackendLink {
            connection: proxy_backend,
        },
        "source",
    )
    .unwrap();
    let hooked = handle.clone();
    let session = session.with_commands(Arc::new(move |command: &str| match command {
        // A thread of the hook that tells while the command is still being taken.
        "party" => {
            let told = hooked.clone();
            std::thread::spawn(move || told.tell("Party: quick."))
                .join()
                .unwrap();
            Some(meridian_net::relay::TAKEN.to_owned())
        }
        // A thread that tells after the reader has finished with the command.
        "slow" => {
            let told = hooked.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(50));
                told.tell("Slow: done.");
            });
            Some(meridian_net::relay::TAKEN.to_owned())
        }
        _ => None,
    }));
    let run = std::thread::spawn(move || session.run());
    send(&mut source, cfg::clientbound::FINISH_CONFIGURATION, &[]);
    expect(&mut player, cfg::clientbound::FINISH_CONFIGURATION).unwrap();
    send(&mut player, cfg::serverbound::FINISH_CONFIGURATION, &[]);
    expect(&mut source, cfg::serverbound::FINISH_CONFIGURATION).unwrap();
    let command = |text: &str| {
        play::Serverbound::ChatCommand(play::ChatCommand {
            command: text.into(),
        })
        .encode()
    };
    let said = |player: &mut Connection| {
        let (id, payload) = player.recv().unwrap();
        match decode(id, &payload) {
            C::SystemChat(chat) => {
                horizon_protocol::text::plain(&chat.content, &Default::default())
            }
            other => panic!("{other:?}"),
        }
    };
    player.send(&command("party")).unwrap();
    player.send(&command("slow")).unwrap();
    // The lines the hook's threads told, and no other: nothing was written for the commands.
    assert_eq!(said(&mut player), "Party: quick.");
    assert_eq!(said(&mut player), "Slow: done.");
    player.send(&command("list")).unwrap();
    let payload = expect(&mut source, ps::CHAT_COMMAND).unwrap();
    match play::Serverbound::decode(ps::CHAT_COMMAND, &payload) {
        Ok(play::Serverbound::ChatCommand(c)) => assert_eq!(c.command, "list"),
        other => panic!("{other:?}"),
    }
    drop(source);
    run.join().unwrap().unwrap();
}

/// A session whose command hook holds a handle to it, as the proxy's hook does, with the
/// player's and the backend's ends of its connections.
fn hooked() -> (Session, SessionHandle, Connection, Connection) {
    let (proxy_client, player) = pipe();
    let (proxy_backend, source) = pipe();
    player.reader.set_read_timeout(Some(WAIT)).unwrap();
    source.reader.set_read_timeout(Some(WAIT)).unwrap();
    let (session, handle) = Session::new(
        ClientSession {
            profile: profile(),
            auth: AuthKind::Offline,
            virtual_host: None,
            connection: proxy_client,
        },
        BackendLink {
            connection: proxy_backend,
        },
        "source",
    )
    .unwrap();
    let hooked = handle.clone();
    let session = session.with_commands(Arc::new(move |_: &str| {
        hooked.tell("Hooked.");
        None
    }));
    (session, handle, player, source)
}

/// Once a session is over and its handles are dropped, nothing of it is left, even when its
/// command hook holds a handle to it.
#[test]
fn a_session_over_is_freed_though_its_hook_holds_a_handle() {
    let (session, handle, mut player, mut source) = hooked();
    let left = Arc::downgrade(&handle.0);
    let run = std::thread::spawn(move || session.run());
    send(&mut source, cfg::clientbound::FINISH_CONFIGURATION, &[]);
    expect(&mut player, cfg::clientbound::FINISH_CONFIGURATION).unwrap();
    send(&mut player, cfg::serverbound::FINISH_CONFIGURATION, &[]);
    expect(&mut source, cfg::serverbound::FINISH_CONFIGURATION).unwrap();
    drop(source);
    run.join().unwrap().unwrap();
    drop(handle);
    wait_for(|| left.upgrade().is_none());
}

/// A session given its hook and never run, as a login refused as a duplicate leaves it, is
/// freed when it is dropped.
#[test]
fn a_session_never_run_is_freed_though_its_hook_holds_a_handle() {
    let (session, handle, _player, _source) = hooked();
    let left = Arc::downgrade(&handle.0);
    drop(session);
    drop(handle);
    assert!(left.upgrade().is_none());
}

/// A message for the player goes to their chat.
#[test]
fn the_player_is_told_in_the_chat() {
    let mut bench = bench();
    bench.handle.tell("Moving you to east");
    let (id, payload) = bench.player.recv().unwrap();
    match decode(id, &payload) {
        C::SystemChat(chat) => assert_eq!(
            horizon_protocol::text::plain(&chat.content, &Default::default()),
            "Moving you to east"
        ),
        other => panic!("{other:?}"),
    }
}

/// A command that starts a thread which tells the player how it ended (`/send`, `/server`): the
/// thread can end before the reader has written the answer. Here it is made to, every time: the
/// hook waits for the thread's message to be told before it returns its answer. The player must
/// still read the answer first.
#[test]
fn what_a_thread_tells_while_a_command_is_answered_comes_after_the_answer() {
    for round in 0..200 {
        let (proxy_client, mut player) = pipe();
        let (proxy_backend, mut source) = pipe();
        player.reader.set_read_timeout(Some(WAIT)).unwrap();
        source.reader.set_read_timeout(Some(WAIT)).unwrap();
        let (session, handle) = Session::new(
            ClientSession {
                profile: profile(),
                auth: AuthKind::Offline,
                virtual_host: None,
                connection: proxy_client,
            },
            BackendLink {
                connection: proxy_backend,
            },
            "source",
        )
        .unwrap();
        let hooked = handle.clone();
        let session = session.with_commands(Arc::new(move |_: &str| {
            let told = hooked.clone();
            // The thread of a move, which has ended before the answer is returned.
            std::thread::spawn(move || told.tell("Sent."))
                .join()
                .unwrap();
            // The reader thread's own message is written at once, before the answer.
            hooked.tell("Alert.");
            Some("Sending...".to_owned())
        }));
        let run = std::thread::spawn(move || session.run());
        send(&mut source, cfg::clientbound::FINISH_CONFIGURATION, &[]);
        expect(&mut player, cfg::clientbound::FINISH_CONFIGURATION).unwrap();
        send(&mut player, cfg::serverbound::FINISH_CONFIGURATION, &[]);
        expect(&mut source, cfg::serverbound::FINISH_CONFIGURATION).unwrap();
        player
            .send(
                &play::Serverbound::ChatCommand(play::ChatCommand {
                    command: "send".into(),
                })
                .encode(),
            )
            .unwrap();
        let mut said = Vec::new();
        for _ in 0..3 {
            let (id, payload) = player.recv().unwrap();
            match decode(id, &payload) {
                C::SystemChat(chat) => said.push(horizon_protocol::text::plain(
                    &chat.content,
                    &Default::default(),
                )),
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(said, ["Alert.", "Sending...", "Sent."], "round {round}");
        // Told with no command under way, a message is written at once.
        handle.tell("Later.");
        let (id, payload) = player.recv().unwrap();
        match decode(id, &payload) {
            C::SystemChat(chat) => assert_eq!(
                horizon_protocol::text::plain(&chat.content, &Default::default()),
                "Later."
            ),
            other => panic!("{other:?}"),
        }
        drop(source);
        run.join().unwrap().unwrap();
    }
}

/// A session whose transfer hook holds a handle to it: a transfer the hook takes is not relayed,
/// one it passes is, a transfer it gives back with `pass_transfer` reaches the client, and once
/// the session is over, or was never run, nothing of it is left.
#[test]
fn a_transfer_the_hook_takes_is_held_and_the_session_is_freed_after() {
    let transfer = |host: &str, port: i32| {
        play::Clientbound::Transfer(horizon_protocol::packets::configuration::Transfer {
            host: host.into(),
            port,
        })
        .encode()
    };
    let Bench {
        handle,
        run,
        mut player,
        mut source,
    } = bench_with_transfers();
    let left = Arc::downgrade(&handle.0);
    // Taken: the next packet the client gets is the keep-alive sent after it.
    source.send(&transfer("taken.example.org", 25566)).unwrap();
    send(&mut source, pc::KEEP_ALIVE, &7u64.to_be_bytes());
    assert_eq!(
        player.recv().unwrap(),
        (pc::KEEP_ALIVE, 7u64.to_be_bytes().to_vec())
    );
    // Given back by the hook's owner: it reaches the client as it came.
    assert!(handle.pass_transfer("taken.example.org", 25566));
    let (id, payload) = player.recv().unwrap();
    match play::Clientbound::decode(id, &payload) {
        Ok(play::Clientbound::Transfer(t)) => {
            assert_eq!((t.host.as_str(), t.port), ("taken.example.org", 25566))
        }
        other => panic!("{other:?}"),
    }
    // Passed by the hook: relayed at once.
    source.send(&transfer("other.example.org", 25565)).unwrap();
    let (id, payload) = player.recv().unwrap();
    assert!(matches!(
        play::Clientbound::decode(id, &payload),
        Ok(play::Clientbound::Transfer(t)) if t.host == "other.example.org"
    ));
    drop(source);
    run.join().unwrap().unwrap();
    drop(handle);
    wait_for(|| left.upgrade().is_none());

    // Never run: freed as well.
    let (proxy_client, _player) = pipe();
    let (proxy_backend, _source) = pipe();
    let (session, handle) = Session::new(
        ClientSession {
            profile: profile(),
            auth: AuthKind::Offline,
            virtual_host: None,
            connection: proxy_client,
        },
        BackendLink {
            connection: proxy_backend,
        },
        "source",
    )
    .unwrap();
    let kept = handle.clone();
    let session = session.with_transfers(Arc::new(move |_: &SessionHandle, _: &str, _: i32| {
        kept.is_over()
    }));
    let left = Arc::downgrade(&handle.0);
    drop(session);
    drop(handle);
    assert!(left.upgrade().is_none());
}
