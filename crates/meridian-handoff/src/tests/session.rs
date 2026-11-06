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
