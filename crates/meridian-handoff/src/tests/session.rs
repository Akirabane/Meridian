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
