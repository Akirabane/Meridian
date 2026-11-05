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
            "target confirm 9f2c4be0a1d34e6b8c0f1e2d3c4b5a69",
            "source release 9f2c4be0a1d34e6b8c0f1e2d3c4b5a69 Moved announce=true",
        ]
    );
    assert_eq!(handle.backend(), "target");
    assert_eq!((done.chunks, done.entities, done.extras), (9, 1, 1));

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

/// What a backend gave one client says nothing of what it gives a client that sent
/// something else: a Horizon server sends the entries of a pack the client knows
/// without their data. A backend that configured one client differently is still asked
/// for a client that configured itself another way, and the switch goes through when
/// that client's configuration matches.
#[test]
fn a_backend_refused_for_one_client_is_still_asked_for_another() {
    let digests = Digests::default();
    let options = options();
    let log = Log::default();
    // The first client: the backend named "target" configures it otherwise, and that
    // is remembered.
    let mut first = bench();
    let (address, _imported, _target) = target(TargetWorld {
        registry: vec![8; 300],
        ..TargetWorld::default()
    });
    let failure = switch(
        &first.handle,
        target_backend(&address, &options),
        &mut FakeControl::new("source", &log),
        &mut FakeControl::new("target", &log),
        &digests,
        &settings(),
    )
    .unwrap_err();
    assert_eq!(failure.step, Step::Prepare);
    still_on_source(&mut first);
    // Another client, which sent another Client Information, and which the same
    // backend configures as its first server did.
    let other = [6; 12];
    let second = bench_as(&other);
    let (address, imported, target_thread) = target(TargetWorld {
        information: other.to_vec(),
        ..TargetWorld::default()
    });
    let done = switch(
        &second.handle,
        target_backend(&address, &options),
        &mut FakeControl::new("source", &log),
        &mut target_control(&log, imported),
        &digests,
        &settings(),
    );
    assert!(done.is_ok(), "{:?}", done.err().map(|f| f.to_string()));
    assert_eq!(second.handle.backend(), "target");
    drop(target_thread);
}

/// A refusal on the registries says, in its one line, how the two configurations
/// differ: sizes and short hashes on each side, and the first registry that differs.
/// Refused again from what was remembered, it says so, with the known packs the client
/// named, since a server sends the entries of a known pack without their data.
#[test]
fn a_refusal_on_the_registries_says_how_they_differ() {
    let mut bench = bench();
    let log = Log::default();
    let other = vec![8; 300];
    let (address, _imported, _target) = target(TargetWorld {
        registry: other.clone(),
        ..TargetWorld::default()
    });
    let digests = Digests::default();
    let options = options();
    let short = |payload: &[u8]| -> String {
        use sha2::{Digest as _, Sha256};
        let mut hash = Sha256::new();
        hash.update((payload.len() as u64).to_be_bytes());
        hash.update(payload);
        hash.finalize()[..4]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    };
    let detail = format!(
        "registry data: the client's 1 packets, 300 bytes, hash {}, target's 1 packets, 300 \
         bytes, hash {}; 1 registries differ, the first registry packet 1 (unreadable name) \
         (the client's 300 bytes, target's 300 bytes)",
        short(&REGISTRY),
        short(&other)
    );
    let failure = switch(
        &bench.handle,
        target_backend(&address, &options),
        &mut FakeControl::new("source", &log),
        &mut FakeControl::new("target", &log),
        &digests,
        &settings(),
    )
    .unwrap_err();
    assert_eq!(failure.step, Step::Prepare);
    assert_eq!(
        failure.reason,
        format!(
            "target configures clients differently (registry data): the player would need a \
             new configuration phase ({detail})"
        )
    );
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
    assert_eq!(
        again.reason,
        format!(
            "target configures clients differently (registry data): the player would need a \
             new configuration phase ({detail}; target's was seen in an earlier switch of a \
             client that sent what this player sent, which named 0 known packs)"
        )
    );
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
            "source release 9f2c4be0a1d34e6b8c0f1e2d3c4b5a69 Abort announce=false",
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
            "target import parts=all announce=Some(true) place=false at=[8.0, 70.0, 8.0] look=[0.0, 0.0] velocity=None ground=None",
            "source release 9f2c4be0a1d34e6b8c0f1e2d3c4b5a69 Abort announce=false",
            "target drop 9f2c4be0a1d34e6b8c0f1e2d3c4b5a69",
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
            "target confirm 9f2c4be0a1d34e6b8c0f1e2d3c4b5a69",
            "source release 9f2c4be0a1d34e6b8c0f1e2d3c4b5a69 Abort announce=false",
            "target drop 9f2c4be0a1d34e6b8c0f1e2d3c4b5a69",
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

/// The rest of a switch that went through: the client has the target's world, and
/// what it does reaches the target.
fn finish(
    mut player: Connection,
    target_thread: JoinHandle<Result<TargetSaw, String>>,
    run: JoinHandle<meridian_net::Result<Report>>,
) {
    let packets = after_switch(&mut player);
    assert!(!packets.iter().any(|(id, _)| *id == pc::DISCONNECT));
    attack(&mut player, TARGET_ID);
    let saw = target_thread.join().unwrap().unwrap();
    assert_eq!(saw.attacked, CLIENT_ID);
    run.join().unwrap().unwrap();
}

/// The target has placed the player and the file is on its disk, but the source
/// refuses to let them go: its freeze ran out, or a command killed the frozen player.
/// Nothing undoes the placing, so the player stays on the target, the source's
/// connection closes, and the switch tells the caller that the source kept its copy.
#[test]
fn a_source_that_refuses_moved_leaves_the_player_on_the_target() {
    let Bench {
        handle,
        run,
        player,
        source: _source,
    } = bench();
    let log = Log::default();
    let (address, signal, target_thread) = target(TargetWorld::default());
    let done = switch(
        &handle,
        target_backend(&address, &options()),
        &mut FakeControl::new("source", &log)
            .refusing("release", "the hand-over is over")
            .with_ticks_left(None),
        &mut target_control(&log, signal),
        &Digests::default(),
        &settings(),
    )
    .unwrap();
    assert_eq!(done.handover, HANDOVER);
    assert_eq!(done.source_kept.as_deref(), Some("the hand-over is over"));
    // The release is asked once, as `moved`, with the id of the export. There is no
    // abort to follow (a refused hand-over has nothing left to end) and no drop of
    // the target, which has the player placed.
    assert_eq!(
        log.lines(),
        [
            "target expect Some(10)",
            "source export freeze=true",
            "target import parts=all-but-position announce=Some(true) place=false",
            "target confirm 9f2c4be0a1d34e6b8c0f1e2d3c4b5a69",
            "source release 9f2c4be0a1d34e6b8c0f1e2d3c4b5a69 Moved announce=true",
        ]
    );
    assert_eq!(handle.backend(), "target");
    finish(player, target_thread, run);
}

/// A switch that went through leaves nothing for an operator to look at.
#[test]
fn a_source_that_lets_the_player_go_keeps_nothing() {
    let Bench {
        handle,
        run,
        player,
        source: _source,
    } = bench();
    let log = Log::default();
    let (address, signal, target_thread) = target(TargetWorld::default());
    let done = switch(
        &handle,
        target_backend(&address, &options()),
        &mut FakeControl::new("source", &log),
        &mut target_control(&log, signal),
        &Digests::default(),
        &settings(),
    )
    .unwrap();
    assert_eq!(done.handover, HANDOVER);
    assert_eq!(done.source_kept, None);
    finish(player, target_thread, run);
}

/// The answer to the export is lost on the way: the export is asked again on a
/// reopened connection, and the switch goes on with the record that comes back.
#[test]
fn a_lost_export_is_asked_again() {
    let Bench {
        handle,
        run,
        player,
        source: _source,
    } = bench();
    let log = Log::default();
    let (address, signal, target_thread) = target(TargetWorld::default());
    let steps = Log::default();
    let traced = steps.clone();
    let done = switch(
        &handle,
        target_backend(&address, &options()),
        &mut FakeControl::new("source", &log).losing_exports(2),
        &mut target_control(&log, signal),
        &Digests::default(),
        &Settings {
            trace: Some(Arc::new(move |what: &str, _| traced.push(what.to_owned()))),
            ..settings()
        },
    )
    .unwrap();
    assert_eq!(done.handover, HANDOVER);
    assert_eq!(
        log.lines(),
        [
            "target expect Some(10)",
            "source export freeze=true (lost)",
            "source reopen",
            "source export freeze=true (lost)",
            "source reopen",
            "source export freeze=true",
            "target import parts=all-but-position announce=Some(true) place=false",
            "target confirm 9f2c4be0a1d34e6b8c0f1e2d3c4b5a69",
            "source release 9f2c4be0a1d34e6b8c0f1e2d3c4b5a69 Moved announce=true",
        ]
    );
    let told = steps.lines();
    assert!(
        told.iter()
            .any(|line| line.contains("asking again (2 of 3)")),
        "{told:?}"
    );
    assert!(
        told.iter()
            .any(|line| line.contains("asking again (3 of 3)")),
        "{told:?}"
    );
    finish(player, target_thread, run);
}

/// An export whose answer never comes is asked a few times, no more: the target
/// forgets the player, and the source, which may have frozen them, lets the freeze go
/// by itself.
#[test]
fn an_export_that_is_always_lost_is_given_up() {
    let mut bench = bench();
    let log = Log::default();
    let (address, _imported, _target) = target(TargetWorld::default());
    let failure = switch(
        &bench.handle,
        target_backend(&address, &options()),
        &mut FakeControl::new("source", &log).losing_exports(10),
        &mut FakeControl::new("target", &log),
        &Digests::default(),
        &settings(),
    )
    .unwrap_err();
    assert_eq!((failure.step, failure.after), (Step::Freeze, After::Stayed));
    assert!(failure.reason.contains("asked 3 times"), "{failure}");
    assert!(failure.reason.contains("no answer in time"), "{failure}");
    assert_eq!(
        log.lines(),
        [
            "target expect Some(10)",
            "source export freeze=true (lost)",
            "source reopen",
            "source export freeze=true (lost)",
            "source reopen",
            "source export freeze=true (lost)",
            "target drop",
        ]
    );
    still_on_source(&mut bench);
}

/// A sidecar that cannot be reached again ends the asking, and says so.
#[test]
fn an_export_is_not_asked_again_when_the_sidecar_is_gone() {
    let mut bench = bench();
    let log = Log::default();
    let (address, _imported, _target) = target(TargetWorld::default());
    let mut source = FakeControl::new("source", &log).losing_exports(1);
    source.cannot_reopen = true;
    let failure = switch(
        &bench.handle,
        target_backend(&address, &options()),
        &mut source,
        &mut FakeControl::new("target", &log),
        &Digests::default(),
        &settings(),
    )
    .unwrap_err();
    assert_eq!((failure.step, failure.after), (Step::Freeze, After::Stayed));
    assert!(
        failure.reason.contains("cannot reach the sidecar again"),
        "{failure}"
    );
    assert_eq!(
        log.lines(),
        [
            "target expect Some(10)",
            "source export freeze=true (lost)",
            "source reopen",
            "target drop",
        ]
    );
    still_on_source(&mut bench);
}

/// A refusal is an answer: the export is not asked again.
#[test]
fn a_refused_export_is_not_asked_again() {
    let mut bench = bench();
    let log = Log::default();
    let (address, _imported, _target) = target(TargetWorld::default());
    let failure = switch(
        &bench.handle,
        target_backend(&address, &options()),
        &mut FakeControl::new("source", &log).refusing("export", "the player is asleep"),
        &mut FakeControl::new("target", &log),
        &Digests::default(),
        &settings(),
    )
    .unwrap_err();
    assert_eq!((failure.step, failure.after), (Step::Freeze, After::Stayed));
    assert!(!failure.reason.contains("asked"), "{failure}");
    assert_eq!(log.lines().len(), 3, "{:?}", log.lines());
    still_on_source(&mut bench);
}

/// The source says its freeze ends before the rest of the transfer could be done:
/// the switch gives up at once, while it can still let the source go on. The
/// freeze limit being above what the server allows is told.
#[test]
fn a_freeze_that_ends_too_soon_is_given_up_before_the_import() {
    let mut bench = bench();
    let log = Log::default();
    let (address, signal, _target) = target(TargetWorld::default());
    let steps = Log::default();
    let traced = steps.clone();
    let failure = switch(
        &bench.handle,
        target_backend(&address, &options()),
        // One second of freeze left, and two seconds of margin kept.
        &mut FakeControl::new("source", &log).with_ticks_left(Some(20)),
        &mut target_control(&log, signal),
        &Digests::default(),
        &Settings {
            trace: Some(Arc::new(move |what: &str, _| traced.push(what.to_owned()))),
            ..settings()
        },
    )
    .unwrap_err();
    assert_eq!((failure.step, failure.after), (Step::Freeze, After::Stayed));
    assert!(
        failure.reason.contains("left on the old server"),
        "{failure}"
    );
    assert_eq!(
        log.lines(),
        [
            "target expect Some(10)",
            "source export freeze=true",
            "source release 9f2c4be0a1d34e6b8c0f1e2d3c4b5a69 Abort announce=false",
            "target drop",
        ]
    );
    let told = steps.lines();
    assert!(
        told.iter().any(|line| line.contains("max_freeze_seconds")),
        "{told:?}"
    );
    still_on_source(&mut bench);
}

/// The margin is the one asked for: with a small one, the same freeze is enough.
#[test]
fn the_margin_kept_against_the_freeze_is_the_one_asked_for() {
    let Bench {
        handle,
        run,
        player,
        source: _source,
    } = bench();
    let log = Log::default();
    let (address, signal, target_thread) = target(TargetWorld::default());
    let done = switch(
        &handle,
        target_backend(&address, &options()),
        // Three seconds of freeze left; the margin keeps one.
        &mut FakeControl::new("source", &log).with_ticks_left(Some(60)),
        &mut target_control(&log, signal),
        &Digests::default(),
        &Settings {
            commit_margin: Duration::from_secs(1),
            ..settings()
        },
    )
    .unwrap();
    assert_eq!(done.source_kept, None);
    finish(player, target_thread, run);
}

/// The client has the target's world, but the delta took so long that the freeze left
/// no longer outlasts the confirm and the release: the arrival is dropped and the
/// source goes on, rather than placing a player whose release would be refused.
#[test]
fn a_freeze_that_runs_short_before_the_confirm_drops_the_arrival() {
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
        // Four seconds of freeze, a margin of one and a half: the confirm may start
        // until about two and a half seconds after the export.
        &mut FakeControl::new("source", &log).with_ticks_left(Some(80)),
        &mut target_control(&log, signal),
        &Digests::default(),
        &Settings {
            commit_margin: Duration::from_millis(1500),
            // Slow once the client holds the target's world, as a client that reads
            // the delta slowly would be.
            trace: Some(Arc::new(|what: &str, _| {
                if what.starts_with("hand over: the client has the target's world") {
                    std::thread::sleep(Duration::from_millis(2800));
                }
            })),
            ..settings()
        },
    )
    .unwrap_err();
    assert_eq!(
        (failure.step, failure.after),
        (Step::HandOver, After::Disconnected)
    );
    assert!(failure.reason.contains("was dropped"), "{failure}");
    assert!(
        failure.reason.contains("left on the old server"),
        "{failure}"
    );
    assert_eq!(
        log.lines(),
        [
            "target expect Some(10)",
            "source export freeze=true",
            "target import parts=all-but-position announce=Some(true) place=false",
            "source release 9f2c4be0a1d34e6b8c0f1e2d3c4b5a69 Abort announce=false",
            "target drop 9f2c4be0a1d34e6b8c0f1e2d3c4b5a69",
        ]
    );
    let mut last = 0;
    while let Ok((id, _)) = player.recv() {
        last = id;
    }
    assert_eq!(last, pc::DISCONNECT);
    run.join().unwrap().unwrap();
}

/// A server that freezes the player and gives no hand-over id cannot be asked to let
/// them go, nor give the target a record it takes: the switch is given up before
/// anything is imported.
#[test]
fn an_export_without_a_hand_over_id_is_given_up() {
    for record in [
        json!({ "version": 1, "dimension": OVERWORLD, "player": "AAAA" }),
        json!({ "version": 1, "handover": null, "dimension": OVERWORLD, "player": "AAAA" }),
        json!({ "version": 1, "handover": "not an id", "dimension": OVERWORLD, "player": "AAAA" }),
    ] {
        let mut bench = bench();
        let log = Log::default();
        let (address, _imported, _target) = target(TargetWorld::default());
        let mut source = FakeControl::new("source", &log);
        source.record = record;
        let failure = switch(
            &bench.handle,
            target_backend(&address, &options()),
            &mut source,
            &mut FakeControl::new("target", &log),
            &Digests::default(),
            &settings(),
        )
        .unwrap_err();
        assert_eq!((failure.step, failure.after), (Step::Freeze, After::Stayed));
        assert!(failure.reason.contains("no hand-over id"), "{failure}");
        // No release is asked (there is no id to name) and nothing was imported.
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
            "source release 9f2c4be0a1d34e6b8c0f1e2d3c4b5a69 Moved announce=true",
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
            "source release 9f2c4be0a1d34e6b8c0f1e2d3c4b5a69 Abort announce=false",
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

/// With the position carried, the record goes to the target with the place and
/// look the client last gave, not those of the export: nothing the client did is
/// pulled back. The target moves the player, so the delta waits for the chunks
/// around the new place, which the target view then holds.
#[test]
fn a_carried_position_is_where_the_client_stands() {
    for recentered in [None, Some(true)] {
        let Bench {
            handle,
            run,
            mut player,
            mut source,
        } = bench();
        // The client walks and turns after joining.
        let moved = play::Serverbound::MovePlayerPosRot(play::MovePlayerPosRot {
            x: 12.5,
            y: 71.0,
            z: -3.5,
            yaw: 45.0,
            pitch: 10.0,
            flags: 1,
        });
        player.send(&moved.encode()).unwrap();
        expect(&mut source, ps::MOVE_PLAYER_POS_ROT).unwrap();
        let log = Log::default();
        let (address, signal, target_thread) = target(TargetWorld {
            recenter: true,
            ..TargetWorld::default()
        });
        let done = switch(
            &handle,
            target_backend(&address, &options()),
            &mut FakeControl::new("source", &log),
            &mut FakeControl {
                recentered,
                ..target_control(&log, signal)
            },
            &Digests::default(),
            &Settings {
                carry_position: true,
                ..settings()
            },
        )
        .unwrap();
        assert_eq!(
            log.lines()[2],
            "target import parts=all announce=Some(true) place=false \
             at=[12.5, 71.0, -3.5] look=[45.0, 10.0] velocity=None \
             ground=Some(true)",
            "{recentered:?}"
        );
        // The nine columns of the join, and the two around the new place.
        assert_eq!(done.chunks, 11, "{recentered:?}");
        after_switch(&mut player);
        attack(&mut player, TARGET_ID);
        target_thread.join().unwrap().unwrap();
        run.join().unwrap().unwrap();
    }
}

/// The target says the import kept the player in the same chunk: the delta does
/// not wait for chunks that will not come.
#[test]
fn a_carried_position_in_the_same_chunk_waits_for_no_chunk() {
    let Bench {
        handle,
        run,
        mut player,
        source: _source,
    } = bench();
    let log = Log::default();
    let (address, signal, target_thread) = target(TargetWorld::default());
    switch(
        &handle,
        target_backend(&address, &options()),
        &mut FakeControl::new("source", &log),
        &mut FakeControl {
            recentered: Some(false),
            ..target_control(&log, signal)
        },
        &Digests::default(),
        &Settings {
            carry_position: true,
            import_timeout: Duration::from_millis(500),
            ..settings()
        },
    )
    .unwrap();
    after_switch(&mut player);
    attack(&mut player, TARGET_ID);
    target_thread.join().unwrap().unwrap();
    run.join().unwrap().unwrap();
}

/// The target tells the player their operator level at login, as an entity event
/// (24 plus the level) the views do not keep: the client must still get it, in its
/// own numbering, after the delta and before the command tree, or it keeps the level
/// of its first server. A target that gives a lower level lowers it, so level 0 is
/// sent as well. A target that sends none sends the client none.
#[test]
fn the_permission_level_of_the_target_reaches_the_client() {
    for (permission, event) in [(Some(4), Some(28)), (Some(0), Some(24)), (None, None)] {
        let Bench {
            handle,
            run,
            mut player,
            source: _source,
        } = bench();
        let log = Log::default();
        let (address, signal, target_thread) = target(TargetWorld {
            permission,
            ..TargetWorld::default()
        });
        let done = switch(
            &handle,
            target_backend(&address, &options()),
            &mut FakeControl::new("source", &log),
            &mut target_control(&log, signal),
            &Digests::default(),
            &settings(),
        )
        .unwrap();
        let packets = after_switch(&mut player);
        let ids: Vec<i32> = packets.iter().map(|(id, _)| *id).collect();
        let last_delimiter = ids
            .iter()
            .rposition(|&id| id == pc::BUNDLE_DELIMITER)
            .unwrap();
        let sent: Vec<&(i32, Vec<u8>)> = packets
            .iter()
            .filter(|(id, _)| *id == pc::ENTITY_EVENT)
            .collect();
        match event {
            Some(event) => {
                // One event, with the client's id, between the delta and the tree.
                assert_eq!(sent.len(), 1, "{ids:?}");
                let (id, payload) = sent[0];
                assert_eq!(
                    decode(*id, payload),
                    C::EntityEvent(play::EntityEvent {
                        id: CLIENT_ID,
                        event
                    })
                );
                let at = ids.iter().position(|&id| id == pc::ENTITY_EVENT).unwrap();
                assert!(at > last_delimiter, "{ids:?}");
                assert_eq!(ids[at + 1], pc::COMMANDS, "{ids:?}");
                assert_eq!(done.extras, 2);
            }
            None => {
                assert!(sent.is_empty(), "{ids:?}");
                assert_eq!(done.extras, 1);
            }
        }
        attack(&mut player, TARGET_ID);
        assert_eq!(target_thread.join().unwrap().unwrap().attacked, CLIENT_ID);
        run.join().unwrap().unwrap();
    }
}

/// The answer to the confirm is lost after the target placed the player: the confirm
/// is asked again on a reopened connection, the target answers it as it answered the
/// first, and the switch ends as a success. Without the retry, the proxy would take
/// the placing for a failure, release the source with `abort`, and the player would
/// be on both servers.
#[test]
fn a_lost_confirm_is_asked_again_and_the_switch_succeeds() {
    let Bench {
        handle,
        run,
        player,
        source: _source,
    } = bench();
    let log = Log::default();
    let (address, signal, target_thread) = target(TargetWorld::default());
    let steps = Log::default();
    let traced = steps.clone();
    let done = switch(
        &handle,
        target_backend(&address, &options()),
        &mut FakeControl::new("source", &log),
        &mut target_control(&log, signal).losing("confirm", 1),
        &Digests::default(),
        &Settings {
            trace: Some(Arc::new(move |what: &str, _| traced.push(what.to_owned()))),
            ..settings()
        },
    )
    .unwrap();
    assert_eq!(done.source_kept, None);
    assert_eq!(
        log.lines(),
        [
            "target expect Some(10)",
            "source export freeze=true",
            "target import parts=all-but-position announce=Some(true) place=false",
            "target confirm 9f2c4be0a1d34e6b8c0f1e2d3c4b5a69 (answer lost)",
            "target reopen",
            "target confirm 9f2c4be0a1d34e6b8c0f1e2d3c4b5a69",
            "source release 9f2c4be0a1d34e6b8c0f1e2d3c4b5a69 Moved announce=true",
        ]
    );
    let told = steps.lines();
    assert!(
        told.iter()
            .any(|line| line.contains("the placing: no answer") && line.contains("(2 of 3)")),
        "{told:?}"
    );
    finish(player, target_thread, run);
}

/// The same for the import: the target holds the record already, and answers the
/// repeat with `ok`.
#[test]
fn a_lost_import_is_asked_again_and_the_switch_succeeds() {
    let Bench {
        handle,
        run,
        player,
        source: _source,
    } = bench();
    let log = Log::default();
    let (address, signal, target_thread) = target(TargetWorld::default());
    switch(
        &handle,
        target_backend(&address, &options()),
        &mut FakeControl::new("source", &log),
        &mut target_control(&log, signal).losing("import", 2),
        &Digests::default(),
        &settings(),
    )
    .unwrap();
    assert_eq!(
        log.lines(),
        [
            "target expect Some(10)",
            "source export freeze=true",
            "target import parts=all-but-position announce=Some(true) place=false (answer lost)",
            "target reopen",
            "target import parts=all-but-position announce=Some(true) place=false (answer lost)",
            "target reopen",
            "target import parts=all-but-position announce=Some(true) place=false",
            "target confirm 9f2c4be0a1d34e6b8c0f1e2d3c4b5a69",
            "source release 9f2c4be0a1d34e6b8c0f1e2d3c4b5a69 Moved announce=true",
        ]
    );
    finish(player, target_thread, run);
}

/// A repeat that the target refuses is a refusal, like any other: the switch ends,
/// and the refusal is not asked again.
#[test]
fn a_confirm_refused_when_asked_again_ends_the_switch() {
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
        &mut target_control(&log, signal)
            .losing("confirm", 1)
            .refusing("confirm", "the player is not held"),
        &Digests::default(),
        &settings(),
    )
    .unwrap_err();
    assert_eq!(
        (failure.step, failure.after),
        (Step::HandOver, After::Disconnected)
    );
    assert!(failure.reason.contains("not held"), "{failure}");
    assert!(!failure.reason.contains("asked"), "{failure}");
    assert_eq!(
        log.lines(),
        [
            "target expect Some(10)",
            "source export freeze=true",
            "target import parts=all-but-position announce=Some(true) place=false",
            "target confirm 9f2c4be0a1d34e6b8c0f1e2d3c4b5a69 (answer lost)",
            "target reopen",
            "target confirm 9f2c4be0a1d34e6b8c0f1e2d3c4b5a69",
            "source release 9f2c4be0a1d34e6b8c0f1e2d3c4b5a69 Abort announce=false",
            "target drop 9f2c4be0a1d34e6b8c0f1e2d3c4b5a69",
        ]
    );
    let mut last = 0;
    while let Ok((id, _)) = player.recv() {
        last = id;
    }
    assert_eq!(last, pc::DISCONNECT);
    run.join().unwrap().unwrap();
}

/// An import refused when asked again leaves the player on the source.
#[test]
fn an_import_refused_when_asked_again_lets_the_source_go_on() {
    let mut bench = bench();
    let log = Log::default();
    let (address, _imported, _target) = target(TargetWorld::default());
    let failure = switch(
        &bench.handle,
        target_backend(&address, &options()),
        &mut FakeControl::new("source", &log),
        &mut FakeControl::new("target", &log)
            .losing("import", 1)
            .refusing("import", "data_version 5022"),
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
            "target import parts=all-but-position announce=Some(true) place=false (answer lost)",
            "target reopen",
            "target import parts=all-but-position announce=Some(true) place=false",
            "source release 9f2c4be0a1d34e6b8c0f1e2d3c4b5a69 Abort announce=false",
            "target drop",
        ]
    );
    still_on_source(&mut bench);
}

/// A confirm whose answer is always lost is asked a few times, no more. Then nothing
/// is known of what the target did, and the failure says so: the player may be placed
/// there.
#[test]
fn a_confirm_that_is_always_lost_is_given_up_and_says_what_is_not_known() {
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
        &mut target_control(&log, signal).losing("confirm", 10),
        &Digests::default(),
        &settings(),
    )
    .unwrap_err();
    assert_eq!(
        (failure.step, failure.after),
        (Step::HandOver, After::Disconnected)
    );
    assert!(failure.reason.contains("asked 3 times"), "{failure}");
    assert!(
        failure.reason.contains("may have placed the player"),
        "{failure}"
    );
    let confirms = log
        .lines()
        .iter()
        .filter(|line| line.starts_with("target confirm"))
        .count();
    assert_eq!(confirms, 3, "{:?}", log.lines());
    let mut last = 0;
    while let Ok((id, _)) = player.recv() {
        last = id;
    }
    assert_eq!(last, pc::DISCONNECT);
    run.join().unwrap().unwrap();
}

/// The asking again stops at the freeze limit: past it the player is let go on the
/// source, not kept frozen for one more try.
#[test]
fn a_lost_confirm_is_not_asked_again_past_the_freeze_limit() {
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
        // The confirm takes longer than the freeze limit allows, and its answer is lost.
        &mut target_control(&log, signal)
            .losing("confirm", 1)
            .on("confirm", || {
                std::thread::sleep(Duration::from_millis(1700))
            }),
        &Digests::default(),
        &Settings {
            freeze_limit: Duration::from_millis(1500),
            ..settings()
        },
    )
    .unwrap_err();
    assert_eq!(
        (failure.step, failure.after),
        (Step::HandOver, After::Disconnected)
    );
    assert!(failure.reason.contains("may have placed"), "{failure}");
    assert!(
        !log.lines().iter().any(|l| l == "target reopen"),
        "{:?}",
        log.lines()
    );
    let confirms = log
        .lines()
        .iter()
        .filter(|line| line.starts_with("target confirm"))
        .count();
    assert_eq!(confirms, 1, "{:?}", log.lines());
    let mut last = 0;
    while let Ok((id, _)) = player.recv() {
        last = id;
    }
    assert_eq!(last, pc::DISCONNECT);
    run.join().unwrap().unwrap();
}
