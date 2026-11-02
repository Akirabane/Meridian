//! World values: one packet per difference, none when they are equal.
use super::*;

fn weather(event: u8, param: f32) -> Packet {
    p(C::GameEvent(play::GameEvent { event, param }))
}

fn border(size: f64) -> Packet {
    p(C::InitializeBorder(play::InitializeBorder {
        center_x: 0.5,
        center_z: -3.0,
        old_size: size,
        new_size: size,
        lerp_time: 0,
        absolute_max_size: 29_999_984,
        warning_blocks: 5,
        warning_time: 15,
    }))
}

fn time(game_time: i64, day: i64) -> Packet {
    p(C::SetTime(play::SetTime {
        game_time,
        clocks: vec![play::ClockState {
            clock: 0,
            total_ticks: day,
            partial_tick: 0.0,
            rate: 1.0,
        }],
    }))
}

fn spawn(x: i32) -> Packet {
    p(C::SetDefaultSpawnPosition(play::SetDefaultSpawnPosition {
        pos: play::GlobalPos {
            dimension: "minecraft:overworld".into(),
            pos: [x, 64, 0],
        },
        yaw: 0.0,
        pitch: 0.0,
    }))
}

fn difficulty(difficulty: i32, locked: bool) -> Packet {
    p(C::ChangeDifficulty(play::ChangeDifficulty {
        difficulty,
        locked,
    }))
}

/// One world, as a server describes it at a join.
fn world(size: f64, day: i64, x: i32, level: i32) -> Vec<Packet> {
    vec![
        login(1, 2),
        border(size),
        time(1000, day),
        spawn(x),
        difficulty(level, false),
        p(C::TickingState(play::TickingState {
            rate: 20.0,
            frozen: false,
        })),
    ]
}

#[test]
fn equal_worlds_send_nothing() {
    let same = world(100.0, 6000, 0, 2);
    let (_, delta) = switch(&same, &same);
    assert!(ids_of(&delta).is_empty(), "{:?}", ids_of(&delta));
}

#[test]
fn each_world_value_that_differs_is_sent_once() {
    let (_, delta) = switch(&world(100.0, 6000, 0, 2), &world(500.0, 18000, 40, 3));
    let order = ids_of(&delta);
    for id in [
        ids::INITIALIZE_BORDER,
        ids::SET_TIME,
        ids::SET_DEFAULT_SPAWN_POSITION,
        ids::CHANGE_DIFFICULTY,
    ] {
        assert_eq!(
            order.iter().filter(|&&i| i == id).count(),
            1,
            "{id} in {order:?}"
        );
    }
    assert!(
        !order.contains(&ids::TICKING_STATE),
        "the tick rate is equal"
    );
}

#[test]
fn rain_starts_then_the_levels_follow() {
    let old = vec![login(1, 2)];
    let new = vec![
        login(1, 2),
        weather(1, 0.0),
        weather(7, 0.7),
        weather(8, 0.2),
    ];
    let (after, delta) = switch(&old, &new);
    let events: Vec<(u8, f32)> = delta
        .iter()
        .filter_map(|p| match p.decode() {
            Ok(C::GameEvent(e)) => Some((e.event, e.param)),
            _ => None,
        })
        .collect();
    assert_eq!(events, vec![(1, 0.0), (7, 0.7), (8, 0.2)]);
    assert!(after.world.raining);
}

#[test]
fn rain_that_stops_on_the_target_leaves_the_level_where_it_fades_from() {
    // Stopping rain sets its level to 1 on the client, as on the target that sent
    // the same event: no level packet is needed after it.
    let old = vec![login(1, 2), weather(1, 0.0), weather(7, 1.0)];
    let new = vec![login(1, 2), weather(1, 0.0), weather(2, 0.0)];
    let (_, delta) = switch(&old, &new);
    let events: Vec<u8> = delta
        .iter()
        .filter_map(|p| match p.decode() {
            Ok(C::GameEvent(e)) => Some(e.event),
            _ => None,
        })
        .collect();
    assert_eq!(events, vec![2]);
}

#[test]
fn a_border_change_after_the_join_is_kept() {
    let old = vec![login(1, 2), border(100.0)];
    let new = vec![
        login(1, 2),
        border(100.0),
        p(C::SetBorderLerpSize(play::SetBorderLerpSize {
            old_size: 100.0,
            new_size: 50.0,
            lerp_time: 20_000,
        })),
        p(C::SetBorderWarningDistance(
            play::SetBorderWarningDistance { blocks: 9 },
        )),
    ];
    let (after, delta) = switch(&old, &new);
    assert_eq!(ids_of(&delta), vec![ids::INITIALIZE_BORDER]);
    let b = after.world.border.unwrap();
    assert_eq!(
        (b.new_size, b.lerp_time, b.warning_blocks),
        (50.0, 20_000, 9)
    );
}
