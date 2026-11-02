use super::*;

/// The client's packets after a switch, up to the last of the target's extras (its
/// command tree); then the client confirms the position, and the target's first
/// live packet (an effect on the player) follows.
fn after_switch(player: &mut Connection) -> Vec<(i32, Vec<u8>)> {
    let mut packets = Vec::new();
    loop {
        let (id, payload) = player.recv().unwrap();
        packets.push((id, payload));
        if id == pc::COMMANDS {
            break;
        }
    }
    send(player, ps::ACCEPT_TELEPORTATION, &varint(2));
    packets.push(player.recv().unwrap());
    packets
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
    let mut target_control = FakeControl::new("target", &log).on("import", move || {
        imported.send(()).unwrap();
    });
    let digests = Digests::default();
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
        &settings(),
    )
    .unwrap();
    assert_eq!(
        log.lines(),
        [
            "target expect Some(10)",
            "source export freeze=true",
            "target import parts=None announce=Some(true)",
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
    send_typed_serverbound(&mut player, TARGET_ID);
    let saw = target_thread.join().unwrap().unwrap();
    assert_eq!(saw.attacked, CLIENT_ID);
    // The target closes: the session ends.
    run.join().unwrap().unwrap();
}

fn send_typed_serverbound(player: &mut Connection, attacked: i32) {
    player
        .send(&play::Serverbound::Attack(play::Attack { id: attacked }).encode())
        .unwrap();
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
            "target import parts=None announce=Some(true)",
            "source release Abort announce=false",
            "target drop",
        ]
    );
    still_on_source(&mut bench);
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
    let mut target_control = FakeControl::new("target", &log).on("import", move || {
        let _ = imported.send(());
    });
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
            "target import parts=None announce=Some(false)",
            "source release Moved announce=true",
        ]
    );
    run.join().unwrap().unwrap();
}
