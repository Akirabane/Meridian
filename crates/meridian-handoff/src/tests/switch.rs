use super::*;

/// The target's control, which tells the fake target's connection of the import
/// and of the confirmation.
fn target_control(log: &Log, signal: Signal) -> FakeControl {
    let confirmed = signal.clone();
    FakeControl::new("target", log)
        .on("import", move || {
            let _ = signal.send("import");
        })
        .on("confirm", move || {
            let _ = confirmed.send("confirm");
        })
}

/// The whole switch: the target prepares while the client stays on the source;
/// the source freezes and exports; the target imports; the client gets the delta,
/// the target's extras and then its live packets in its own numbering; the source
/// lets the player go, and nothing of it reaches the client after the hand-over.
#[test]
fn a_player_is_switched_to_the_target() {
    let Bench {
        handle,
        run,
        mut player,
        source,
    } = bench();
    let log = Log::default();
    let (address, imported, target_thread) = target(TargetWorld::default());
    let source = Arc::new(Mutex::new(source));
    let mut source_control = {
        let source = Arc::clone(&source);
        // The source closes the player's connection when it lets them go, with
        // a disconnect the client must never see.
        FakeControl::new("source", &log).on("release", move || {
            send(&mut source.lock().unwrap(), pc::DISCONNECT, &[10, 0]);
        })
    };
    let mut target_control = target_control(&log, imported);
    let digests = Digests::default();
    let steps = Log::default();
    let traced = steps.clone();
    let done = switch(
        &handle,
        TargetBackend {
            name: "target",
            address: &address,
            options: &options(),
        },
        &mut source_control,
        &mut target_control,
        &digests,
        &Settings {
            trace: Some(Arc::new(move |what: &str, _| traced.push(what.to_owned()))),
            ..settings()
        },
    )
    .unwrap();
    // Each step is told, in order, with what the delta goes from and to.
    let lines = steps.lines();
    let steps: Vec<String> = lines
        .iter()
        .map(|line| line.split(':').next().unwrap().to_owned())
        .collect();
    assert_eq!(
        steps,
        [
            "prepare",
            "prepare",
            "prepare",
            "prepare",
            "freeze",
            "hand over",
            "hand over",
            "hand over",
            "hand over",
            "commit"
        ]
    );
    assert!(
        lines.iter().any(|line| line.starts_with(
            "hand over: the client holds inventory none, held slot none, experience none"
        )),
        "{lines:?}"
    );
    assert_eq!(
        log.lines(),
        [
            "target expect Some(10)",
            "source export freeze=true",
            "target import parts=all-but-position announce=Some(true) place=false",
            "target confirm",
            "source release Moved announce=true",
        ]
    );
    assert_eq!(handle.backend(), "target");
    assert_eq!((done.chunks, done.entities, done.extras), (9, 1, 1));
    assert_eq!(done.warning, None);

    let packets = after_switch(&mut player);
    let ids: Vec<i32> = packets.iter().map(|(id, _)| *id).collect();
    assert!(!ids.contains(&pc::DISCONNECT), "{ids:?}");
    assert_eq!(ids[0], pc::BUNDLE_DELIMITER, "{ids:?}");
    let delimiters: Vec<usize> = (0..ids.len())
        .filter(|&i| ids[i] == pc::BUNDLE_DELIMITER)
        .collect();
    assert_eq!(delimiters.len(), 2, "one bundle: {ids:?}");
    // The delta, then the extras, then the live packets.
    assert_eq!(
        &ids[delimiters[1] + 1..],
        [pc::COMMANDS, pc::UPDATE_MOB_EFFECT]
    );
    let delta = &packets[..=delimiters[1]];
    assert_eq!(done.delta_packets, delta.len());
    let typed: Vec<C> = delta
        .iter()
        .filter(|(id, _)| *id != pc::LEVEL_CHUNK_WITH_LIGHT && *id != pc::BUNDLE_DELIMITER)
        .map(|(id, payload)| decode(*id, payload))
        .collect();
    // The source's entity goes; the target's entity 5, which is the client's
    // player id, comes under the target's player id.
    assert!(
        typed
            .iter()
            .any(|p| matches!(p, C::RemoveEntities(r) if r.ids == [30])),
        "{typed:?}"
    );
    assert!(
        typed
            .iter()
            .any(|p| matches!(p, C::AddEntity(a) if a.id == TARGET_ID && a.uuid == 500)),
        "{typed:?}"
    );
    // The position the client confirms is the one the import sent.
    assert!(
        typed
            .iter()
            .any(|p| matches!(p, C::PlayerPosition(p) if p.teleport == 2)),
        "{typed:?}"
    );
    assert_eq!(
        delta
            .iter()
            .filter(|(id, _)| *id == pc::LEVEL_CHUNK_WITH_LIGHT)
            .count(),
        9
    );
    // The target's effect on its player 9 reaches the client on its player 5.
    let (id, payload) = packets.last().unwrap();
    assert_eq!(decode(*id, payload), effect(CLIENT_ID));

    // The client attacks what it knows as 9: the target's entity 5.
    attack(&mut player, TARGET_ID);
    let saw = target_thread.join().unwrap().unwrap();
    assert_eq!(saw.attacked, CLIENT_ID);
    // The target closes: the session ends.
    run.join().unwrap().unwrap();
}

fn target_backend<'a>(address: &'a str, options: &'a BackendOptions) -> TargetBackend<'a> {
    TargetBackend {
        name: "target",
        address,
        options,
    }
}

/// After a failed switch, the session still relays the source to the client.
fn still_on_source(bench: &mut Bench) {
    assert_eq!(bench.handle.backend(), "source");
    send_typed(&mut bench.source, &effect(CLIENT_ID));
    let (id, payload) = bench.player.recv().unwrap();
    assert_eq!(decode(id, &payload), effect(CLIENT_ID));
}

#[test]
fn a_target_that_will_not_expect_the_player_changes_nothing() {
    let mut bench = bench();
    let log = Log::default();
    let failure = switch(
        &bench.handle,
        target_backend("127.0.0.1:1", &options()),
        &mut FakeControl::new("source", &log),
        &mut FakeControl::new("target", &log).refusing("expect", "64 players already expected"),
        &Digests::default(),
        &settings(),
    )
    .unwrap_err();
    assert_eq!(
        (failure.step, failure.after),
        (Step::Prepare, After::Stayed)
    );
    assert!(failure.reason.contains("64 players"), "{failure}");
    assert_eq!(log.lines(), ["target expect Some(10)"]);
    still_on_source(&mut bench);
}

#[test]
fn a_switch_to_the_same_backend_is_refused() {
    let bench = bench();
    let log = Log::default();
    let failure = switch(
        &bench.handle,
        TargetBackend {
            name: "source",
            address: "127.0.0.1:1",
            options: &options(),
        },
        &mut FakeControl::new("source", &log),
        &mut FakeControl::new("target", &log),
        &Digests::default(),
        &settings(),
    )
    .unwrap_err();
    assert_eq!(failure.step, Step::Check);
    assert!(log.lines().is_empty());
}

/// A target with other registries is refused on the prepare connection, before
/// the freeze, and forgets the player; the next switch to it is refused before it
/// is asked anything.
#[test]
fn a_target_with_other_registries_is_refused_and_remembered() {
    let mut bench = bench();
    let log = Log::default();
    let (address, _imported, target_thread) = target(TargetWorld {
        registry: vec![8; 300],
        ..TargetWorld::default()
    });
    let digests = Digests::default();
    let options = options();
    let failure = switch(
        &bench.handle,
        target_backend(&address, &options),
        &mut FakeControl::new("source", &log),
        &mut FakeControl::new("target", &log),
        &digests,
        &settings(),
    )
    .unwrap_err();
    assert_eq!(
        (failure.step, failure.after),
        (Step::Prepare, After::Stayed)
    );
    assert!(failure.reason.contains("registry data"), "{failure}");
    assert_eq!(log.lines(), ["target expect Some(10)", "target drop"]);
    // The proxy closed the target's connection: its script did not get through.
    assert!(!matches!(target_thread.join(), Ok(Ok(_))));
    let again = switch(
        &bench.handle,
        target_backend(&address, &options),
        &mut FakeControl::new("source", &log),
        &mut FakeControl::new("target", &log),
        &digests,
        &settings(),
    )
    .unwrap_err();
    assert_eq!(again.step, Step::Check);
    assert_eq!(log.lines().len(), 2, "nothing more was asked");
    still_on_source(&mut bench);
}

#[test]
fn a_target_in_another_dimension_is_refused_before_the_freeze() {
    let mut bench = bench();
    let log = Log::default();
    let (address, _imported, _target) = target(TargetWorld {
        dimension: "minecraft:the_nether",
        ..TargetWorld::default()
    });
    let failure = switch(
        &bench.handle,
        target_backend(&address, &options()),
        &mut FakeControl::new("source", &log),
        &mut FakeControl::new("target", &log),
        &Digests::default(),
        &settings(),
    )
    .unwrap_err();
    assert_eq!(
        (failure.step, failure.after),
        (Step::Prepare, After::Stayed)
    );
    assert!(failure.reason.contains("minecraft:the_nether"), "{failure}");
    assert_eq!(log.lines(), ["target expect Some(10)", "target drop"]);
    still_on_source(&mut bench);
}

#[test]
fn a_refused_export_leaves_the_player_on_the_source() {
    let mut bench = bench();
    let log = Log::default();
    let (address, _imported, _target) = target(TargetWorld::default());
    let failure = switch(
        &bench.handle,
        target_backend(&address, &options()),
        &mut FakeControl::new("source", &log).refusing("export", "the player is riding"),
        &mut FakeControl::new("target", &log),
        &Digests::default(),
        &settings(),
    )
    .unwrap_err();
    assert_eq!((failure.step, failure.after), (Step::Freeze, After::Stayed));
    assert!(failure.reason.contains("riding"), "{failure}");
    assert_eq!(
        log.lines(),
        [
            "target expect Some(10)",
            "source export freeze=true",
            "target drop"
        ]
    );
    still_on_source(&mut bench);
}

#[test]
fn a_refused_import_lets_the_source_go_on() {
    let mut bench = bench();
    let log = Log::default();
    let (address, _imported, _target) = target(TargetWorld::default());
    let failure = switch(
        &bench.handle,
        target_backend(&address, &options()),
        &mut FakeControl::new("source", &log),
        &mut FakeControl::new("target", &log).refusing("import", "data_version 5022"),
        &Digests::default(),
        &settings(),
    )
    .unwrap_err();
    assert_eq!(
        (failure.step, failure.after),
        (Step::HandOver, After::Stayed)
    );
    assert!(failure.reason.contains("data_version"), "{failure}");
    assert_eq!(
        log.lines(),
        [
            "target expect Some(10)",
            "source export freeze=true",
            "target import parts=all-but-position announce=Some(true) place=false",
            "source release Abort announce=false",
            "target drop",
        ]
    );
    still_on_source(&mut bench);
}

/// The position is carried, the import went through, but the target sends no
/// position: the player is still held, so the target forgets them and the source
/// keeps them.
#[test]
fn a_carried_position_that_never_comes_cancels_the_import() {
    let mut bench = bench();
    let log = Log::default();
    let (address, signal, _target) = target(TargetWorld {
        import_sends_position: false,
        ..TargetWorld::default()
    });
    let failure = switch(
        &bench.handle,
        target_backend(&address, &options()),
        &mut FakeControl::new("source", &log),
        &mut target_control(&log, signal),
        &Digests::default(),
        &Settings {
            import_timeout: Duration::from_millis(300),
            carry_position: true,
            ..settings()
        },
    )
    .unwrap_err();
    assert_eq!(
        (failure.step, failure.after),
        (Step::HandOver, After::Stayed)
    );
    assert!(failure.reason.contains("position"), "{failure}");
    assert_eq!(
        log.lines(),
        [
            "target expect Some(10)",
            "source export freeze=true",
            "target import parts=all announce=Some(true) place=false",
            "source release Abort announce=false",
            "target drop",
        ]
    );
    still_on_source(&mut bench);
}

/// The client has the target's world, but the target will not place the player:
/// the player is told and disconnected, the target forgets them and the source
/// keeps them.
#[test]
fn a_refused_confirmation_disconnects_the_player() {
    let Bench {
        handle,
        run,
        mut player,
        source: _source,
    } = bench();
    let log = Log::default();
    let (address, signal, _target) = target(TargetWorld::default());
    let failure = switch(
        &handle,
        target_backend(&address, &options()),
        &mut FakeControl::new("source", &log),
        &mut target_control(&log, signal).refusing("confirm", "the player is not held"),
        &Digests::default(),
        &settings(),
    )
    .unwrap_err();
    assert_eq!(
        (failure.step, failure.after),
        (Step::HandOver, After::Disconnected)
    );
    assert!(failure.reason.contains("not held"), "{failure}");
    assert_eq!(
        log.lines(),
        [
            "target expect Some(10)",
            "source export freeze=true",
            "target import parts=all-but-position announce=Some(true) place=false",
            "target confirm",
            "source release Abort announce=false",
            "target drop",
        ]
    );
    // The delta, then the disconnect, then the end.
    let mut last = 0;
    while let Ok((id, _)) = player.recv() {
        last = id;
    }
    assert_eq!(last, pc::DISCONNECT);
    run.join().unwrap().unwrap();
}

/// The player leaves while the source exports them: their record goes into the
/// target as a normal save, quietly, and the source lets them go (decision 10).
#[test]
fn a_player_who_leaves_during_the_freeze_is_saved_on_the_target() {
    let Bench {
        handle,
        run,
        player,
        source: _source,
    } = bench();
    let log = Log::default();
    let (address, imported, _target) = target(TargetWorld::default());
    let mut player = Some(player);
    let watched = handle.clone();
    let mut source_control = FakeControl::new("source", &log).on("export", move || {
        drop(player.take());
        wait_for(|| watched.is_over());
    });
    let mut target_control = target_control(&log, imported);
    let failure = switch(
        &handle,
        target_backend(&address, &options()),
        &mut source_control,
        &mut target_control,
        &Digests::default(),
        &settings(),
    )
    .unwrap_err();
    assert_eq!((failure.step, failure.after), (Step::Freeze, After::Left));
    assert_eq!(
        log.lines(),
        [
            "target expect Some(10)",
            "source export freeze=true",
            "target import parts=all-but-position announce=Some(false) place=true",
            "source release Moved announce=true",
        ]
    );
    run.join().unwrap().unwrap();
}

/// The source took so long to export that the freeze would run out: the switch
/// gives up while it can still let the source go on.
#[test]
fn a_freeze_that_lasts_too_long_is_given_up() {
    let mut bench = bench();
    let log = Log::default();
    let (address, signal, _target) = target(TargetWorld::default());
    let failure = switch(
        &bench.handle,
        target_backend(&address, &options()),
        &mut FakeControl::new("source", &log).on("export", || {
            std::thread::sleep(Duration::from_millis(300));
        }),
        &mut target_control(&log, signal),
        &Digests::default(),
        &Settings {
            freeze_limit: Duration::from_millis(200),
            ..settings()
        },
    )
    .unwrap_err();
    assert_eq!((failure.step, failure.after), (Step::Freeze, After::Stayed));
    assert!(failure.reason.contains("frozen for 200 ms"), "{failure}");
    assert_eq!(
        log.lines(),
        [
            "target expect Some(10)",
            "source export freeze=true",
            "source release Abort announce=false",
            "target drop",
        ]
    );
    still_on_source(&mut bench);
}

/// The answer to a teleport gives where the player now is: the position as sent,
/// or added to the last one for the parts the flags call relative.
#[test]
fn a_teleport_answer_gives_the_place_it_led_to() {
    use crate::prepare::Pose;
    use horizon_protocol::packets::play::relative;
    let mut pose = Pose::default();
    let at = |position: [f64; 3], yaw: f32, relatives: i32| play::PlayerPosition {
        teleport: 1,
        position,
        delta: [0.0; 3],
        yaw,
        pitch: 10.0,
        relatives,
    };
    assert_eq!(
        pose.placed(&at([8.0, 70.0, 8.0], 90.0, 0)),
        ([8.0, 70.0, 8.0], 90.0, 10.0)
    );
    assert_eq!(
        pose.placed(&at(
            [1.0, 0.0, -2.0],
            45.0,
            relative::X | relative::Z | relative::Y_ROT
        )),
        ([9.0, 0.0, 6.0], 135.0, 10.0)
    );
}

/// A busy target never stops sending after the import (mobs move, time passes):
/// the delta goes as soon as the import's position is there, well before the
/// import timeout, and the player is not kept frozen.
#[test]
fn a_busy_target_does_not_hold_the_hand_over() {
    let Bench {
        handle,
        run,
        mut player,
        source: _source,
    } = bench();
    let log = Log::default();
    let (address, signal, target_thread) = target(TargetWorld {
        chatter: Duration::from_millis(1500),
        ..TargetWorld::default()
    });
    let done = switch(
        &handle,
        target_backend(&address, &options()),
        &mut FakeControl::new("source", &log),
        &mut target_control(&log, signal),
        &Digests::default(),
        &Settings {
            import_timeout: Duration::from_millis(500),
            ..settings()
        },
    )
    .unwrap();
    // Freeze, import and hand-over well inside the import timeout.
    assert!(
        done.times[1] + done.times[2] < Duration::from_millis(500),
        "{:?}",
        done.times
    );
    // The client: the delta and the extras, then the chatter and the live effect.
    loop {
        let (id, _) = player.recv().unwrap();
        if id == pc::COMMANDS {
            break;
        }
    }
    accept(&mut player, 2, [41.0, 70.0, 41.0]);
    loop {
        let (id, payload) = player.recv().unwrap();
        if id == pc::UPDATE_MOB_EFFECT {
            assert_eq!(decode(id, &payload), effect(CLIENT_ID));
            break;
        }
    }
    attack(&mut player, TARGET_ID);
    assert_eq!(target_thread.join().unwrap().unwrap().attacked, CLIENT_ID);
    run.join().unwrap().unwrap();
}
