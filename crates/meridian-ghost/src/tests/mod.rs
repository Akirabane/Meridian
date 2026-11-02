//! Scenes: two servers' packets, a ghost fed with the first, a target view fed with
//! the second, and the check that matters: the ghost fed with the delta is the
//! target view.
use super::*;
use horizon_protocol::packets::play::{self, Clientbound as C};

mod entities;
mod lists;
mod player;
mod world;

/// A packet, as the test kit carries it.
fn p(packet: C) -> Packet {
    Packet::of(&packet)
}

/// A chunk packet for the column at `x`, `z`: only the position is read, the rest
/// is opaque bytes, `fill` marking what the column holds.
fn chunk(x: i32, z: i32, fill: u8) -> Packet {
    let mut payload = Vec::new();
    payload.extend_from_slice(&x.to_be_bytes());
    payload.extend_from_slice(&z.to_be_bytes());
    payload.extend_from_slice(&[fill; 64]);
    Packet {
        id: ids::LEVEL_CHUNK_WITH_LIGHT,
        payload,
    }
}

fn login(player_id: i32, radius: i32) -> Packet {
    // Login is read only for the player's id and the view distance; the rest
    // comes from a decoded one so that it encodes.
    let login = play::Login {
        player_id,
        hardcore: false,
        levels: vec!["minecraft:overworld".into()],
        max_players: 20,
        chunk_radius: radius,
        simulation_distance: radius,
        reduced_debug_info: false,
        show_death_screen: true,
        limited_crafting: false,
        spawn: play::SpawnInfo {
            dimension_type: 0,
            dimension: "minecraft:overworld".into(),
            seed: 0,
            game_type: 0,
            previous_game_type: None,
            debug: false,
            flat: false,
            last_death: None,
            portal_cooldown: 0,
            sea_level: 63,
        },
        online_mode: false,
        enforces_secure_chat: false,
    };
    p(C::Login(login))
}

fn placed(x: f64, y: f64, z: f64, teleport: i32) -> Packet {
    p(C::PlayerPosition(play::PlayerPosition {
        teleport,
        position: [x, y, z],
        delta: [0.0; 3],
        yaw: 90.0,
        pitch: 0.0,
        relatives: 0,
    }))
}

fn add(id: i32, uuid: u128, kind: i32, at: [f64; 3]) -> Packet {
    p(C::AddEntity(play::AddEntity {
        id,
        uuid,
        kind,
        position: at,
        velocity: [0.0; 3],
        pitch: 0,
        yaw: 64,
        head_yaw: 64,
        data: 0,
    }))
}

/// A view of `kind` fed with `packets`.
fn view(mut view: View, packets: &[Packet]) -> View {
    for packet in packets {
        view.apply(packet).unwrap();
    }
    view
}

/// The delta from `old` to `new`, checked: nothing forbidden, bundles well formed
/// and within the limit, and the ghost fed with it equals the target view.
fn switch(old: &[Packet], new: &[Packet]) -> (View, Vec<Packet>) {
    let ghost = view(View::ghost(), old);
    let target = view(View::target(), new);
    let delta = delta(&ghost, &target);
    assert!(
        delta.iter().all(|p| !forbidden(p)),
        "a forbidden packet in the delta"
    );
    check_bundles(&delta);
    // The rewriting layer gives the player's own packets the client's id.
    let delivered: Vec<Packet> = delta
        .iter()
        .map(|p| as_client(p, target.player_id(), ghost.player_id()))
        .collect();
    let after = view(ghost, &delivered);
    assert_eq!(after, target, "the ghost after the delta is not the target");
    (after, delta)
}

/// `packet` with the target's id for the player replaced by the client's, as the
/// rewriting layer will do.
fn as_client(packet: &Packet, target: Option<i32>, client: Option<i32>) -> Packet {
    let (Some(target), Some(client)) = (target, client) else {
        return packet.clone();
    };
    let own = |id: &mut i32| {
        if *id == target {
            *id = client;
        }
    };
    match packet.decode() {
        Ok(C::UpdateMobEffect(mut p)) => {
            own(&mut p.id);
            Packet::of(&C::UpdateMobEffect(p))
        }
        Ok(C::RemoveMobEffect(mut p)) => {
            own(&mut p.id);
            Packet::of(&C::RemoveMobEffect(p))
        }
        Ok(C::UpdateAttributes(mut p)) => {
            own(&mut p.id);
            Packet::of(&C::UpdateAttributes(p))
        }
        _ => packet.clone(),
    }
}

fn check_bundles(delta: &[Packet]) {
    let mut open = false;
    let mut inside = 0;
    for packet in delta {
        if packet.id == ids::BUNDLE_DELIMITER {
            open = !open;
            inside = 0;
        } else {
            assert!(open, "a packet outside a bundle");
            inside += 1;
            assert!(inside <= BUNDLE_LIMIT, "a bundle past the limit");
        }
    }
    assert!(!open, "an unclosed bundle");
}

fn ids_of(delta: &[Packet]) -> Vec<i32> {
    delta
        .iter()
        .map(|p| p.id)
        .filter(|&id| id != ids::BUNDLE_DELIMITER)
        .collect()
}

#[test]
fn the_same_world_needs_no_chunk() {
    let world: Vec<Packet> = std::iter::once(login(1, 2))
        .chain((-1..=1).flat_map(|x| (-1..=1).map(move |z| chunk(x, z, 7))))
        .chain([placed(8.0, 64.0, 8.0, 1)])
        .collect();
    let (_, delta) = switch(&world, &world);
    assert!(
        !ids_of(&delta).contains(&ids::LEVEL_CHUNK_WITH_LIGHT),
        "{:?}",
        ids_of(&delta)
    );
}

#[test]
fn another_world_is_sent_nearest_first_and_the_player_after_the_ground() {
    let old: Vec<Packet> = std::iter::once(login(1, 2))
        .chain((-2..=2).flat_map(|x| (-2..=2).map(move |z| chunk(x, z, 1))))
        .collect();
    let new: Vec<Packet> = std::iter::once(login(9, 2))
        .chain((-2..=2).flat_map(|x| (-2..=2).map(move |z| chunk(x, z, 2))))
        .chain([placed(40.0, 70.0, 40.0, 5)])
        .collect();
    let (_, delta) = switch(&old, &new);
    // The player stands in column (2, 2): the chunks come by distance from it.
    let columns: Vec<(i32, i32)> = delta
        .iter()
        .filter(|p| p.id == ids::LEVEL_CHUNK_WITH_LIGHT)
        .map(|p| {
            let x = i32::from_be_bytes(p.payload[0..4].try_into().unwrap());
            let z = i32::from_be_bytes(p.payload[4..8].try_into().unwrap());
            (x, z)
        })
        .collect();
    assert_eq!(columns.len(), 25);
    assert_eq!(columns[0], (2, 2));
    let rings: Vec<i32> = columns
        .iter()
        .map(|&(x, z)| (x - 2).abs().max((z - 2).abs()))
        .collect();
    assert!(rings.windows(2).all(|w| w[0] <= w[1]), "{rings:?}");
    // The position follows the columns next to the player and precedes the rest.
    let order = ids_of(&delta);
    let position = order
        .iter()
        .position(|&id| id == ids::PLAYER_POSITION)
        .unwrap();
    let before = order[..position]
        .iter()
        .filter(|&&id| id == ids::LEVEL_CHUNK_WITH_LIGHT)
        .count();
    assert_eq!(before, rings.iter().filter(|&&r| r <= 1).count());
    // Every column is replaced in place: nothing is forgotten.
    assert!(!order.contains(&ids::FORGET_LEVEL_CHUNK));
}

#[test]
fn a_smaller_target_view_forgets_the_rest_last() {
    let old: Vec<Packet> = std::iter::once(login(1, 4))
        .chain((-3..=3).flat_map(|x| (-3..=3).map(move |z| chunk(x, z, 1))))
        .collect();
    let new: Vec<Packet> = std::iter::once(login(1, 4))
        .chain((-1..=1).flat_map(|x| (-1..=1).map(move |z| chunk(x, z, 1))))
        .collect();
    let (_, delta) = switch(&old, &new);
    let order = ids_of(&delta);
    assert_eq!(
        order
            .iter()
            .filter(|&&id| id == ids::FORGET_LEVEL_CHUNK)
            .count(),
        49 - 9
    );
    assert!(!order.contains(&ids::LEVEL_CHUNK_WITH_LIGHT));
}

#[test]
fn a_changed_block_makes_its_column_go_again() {
    let base: Vec<Packet> = vec![login(1, 2), chunk(0, 0, 3), chunk(1, 0, 3)];
    let mut changed = base.clone();
    changed.push(p(C::BlockUpdate(play::BlockUpdate {
        pos: [20, 64, 3],
        state: 1,
    })));
    let (_, delta) = switch(&base, &changed);
    let order = ids_of(&delta);
    // Column (1, 0) goes again, with its change after it; column (0, 0) stays.
    assert_eq!(
        order,
        vec![ids::LEVEL_CHUNK_WITH_LIGHT, ids::BLOCK_UPDATE],
        "{order:?}"
    );
}

#[test]
fn a_chunk_cache_move_drops_what_the_client_drops() {
    // Radius 2 keeps columns within 5 of the centre.
    let ghost = view(
        View::ghost(),
        &[
            login(1, 2),
            chunk(0, 0, 1),
            chunk(5, 0, 1),
            p(C::SetChunkCacheCenter(play::SetChunkCacheCenter {
                x: 1,
                z: 0,
            })),
        ],
    );
    assert_eq!(ghost.counts().0, 2);
    let ghost = view(
        ghost,
        &[p(C::SetChunkCacheCenter(play::SetChunkCacheCenter {
            x: 6,
            z: 0,
        }))],
    );
    assert_eq!(
        ghost.counts().0,
        1,
        "column (0, 0) is 6 away from the centre"
    );
}

#[test]
fn the_delta_never_holds_a_forbidden_packet() {
    // A target fed with the packets a joining player gets, Login and Game Event 13
    // among them: none of them is ever sent again.
    let new = vec![
        login(9, 2),
        p(C::GameEvent(play::GameEvent {
            event: 13,
            param: 0.0,
        })),
        chunk(0, 0, 1),
        placed(1.0, 64.0, 1.0, 1),
    ];
    let (_, delta) = switch(&[login(1, 2)], &new);
    assert!(delta.iter().all(|p| !forbidden(p)));
    // And the list itself.
    assert!(forbidden(&login(1, 2)));
    assert!(forbidden(&p(C::GameEvent(play::GameEvent {
        event: 13,
        param: 0.0
    }))));
    assert!(!forbidden(&p(C::GameEvent(play::GameEvent {
        event: 1,
        param: 0.0
    }))));
    for id in [
        ids::RESPAWN,
        ids::START_CONFIGURATION,
        ids::TRANSFER,
        ids::DISCONNECT,
    ] {
        assert!(forbidden(&Packet {
            id,
            payload: Vec::new()
        }));
    }
}

#[test]
fn a_large_view_is_cut_into_bundles_the_client_takes() {
    // View distance 32: 65 by 65 columns, more than one bundle holds.
    let new: Vec<Packet> = std::iter::once(login(1, 32))
        .chain((-32..=32).flat_map(|x| (-32..=32).map(move |z| chunk(x, z, 4))))
        .chain([placed(0.0, 64.0, 0.0, 1)])
        .collect();
    let (_, delta) = switch(&[login(1, 32)], &new);
    let bundles = delta
        .iter()
        .filter(|p| p.id == ids::BUNDLE_DELIMITER)
        .count()
        / 2;
    assert_eq!(bundles, 2, "65 * 65 + 1 packets in bundles of 4096");
}

#[test]
fn a_recorded_session_feeds_the_ghost() {
    // The packets as the test kit's peers carry them: id and payload.
    let recorder = meridian_testkit::trace::Recorder::new();
    for packet in [login(3, 2), chunk(0, 0, 1), add(60, 0x60, 5, [0.0; 3])] {
        recorder.record(
            meridian_testkit::trace::Dir::ToClient,
            packet.id,
            &packet.payload,
        );
    }
    let mut ghost = View::ghost();
    for packet in recorder
        .trace()
        .packets(meridian_testkit::trace::Dir::ToClient)
    {
        ghost
            .apply(&Packet {
                id: packet.id,
                payload: packet.payload.clone(),
            })
            .unwrap();
    }
    assert_eq!(ghost.player_id(), Some(3));
    assert_eq!(ghost.counts(), (1, 1));
}
