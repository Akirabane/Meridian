//! The ghost and the target view fed hostile packets: valid ones changed at random,
//! and random ids with random payloads, from a fixed seed. A view takes or refuses
//! each without a panic, holds no more than it was given, and the delta between two
//! such views is computed and applied without one either.
use super::*;
use meridian_testkit::mutate::{Rng, mutate};

#[test]
fn hostile_packets_are_taken_or_refused_without_a_panic() {
    let valid = [
        login(3, 2),
        login(9, 8),
        chunk(0, 0, 1),
        chunk(-1, 2, 7),
        placed(1.5, 64.0, -2.5, 4),
        add(60, 0x60, 5, [1.0, 2.0, 3.0]),
    ];
    let mut rng = Rng::new(0x0067_686f_7374);
    for round in 0..3000 {
        let (mut ghost, mut target) = (View::ghost(), View::target());
        let mut given = 0;
        for _ in 0..20 {
            let packet = if rng.below(3) == 0 {
                let n = rng.below(64);
                Packet {
                    id: rng.below(0x90) as i32,
                    payload: rng.bytes(n),
                }
            } else {
                let base = &valid[rng.below(valid.len())];
                Packet {
                    id: base.id,
                    payload: mutate(&mut rng, &base.payload),
                }
            };
            given += 1;
            let _ = ghost.apply(&packet);
            let _ = target.apply(&packet);
        }
        let (chunks, entities) = ghost.counts();
        assert!(
            chunks + entities <= given,
            "round {round}: {chunks} chunks and {entities} entities from {given} packets"
        );
        for packet in delta(&ghost, &target) {
            let _ = ghost.apply(&packet);
        }
    }
}

/// A column at the edge of the coordinates a packet can name, far from the player:
/// the delta orders it without the distance overflowing.
#[test]
fn a_column_at_the_edge_of_the_coordinates_is_ordered() {
    let ghost = view(View::ghost(), &[login(3, 2)]);
    let target = view(
        View::target(),
        &[
            login(3, 2),
            placed(100.0, 64.0, 100.0, 1),
            chunk(i32::MIN, i32::MAX, 1),
            chunk(6, 6, 2),
        ],
    );
    let out = delta(&ghost, &target);
    assert!(out.iter().any(|p| p.id == ids::LEVEL_CHUNK_WITH_LIGHT));
}

/// A radius and a centre at the edge of what a packet can say: the columns out of
/// range are forgotten without the range overflowing.
#[test]
fn a_radius_and_a_centre_at_the_edge_keep_the_columns_in_range() {
    let mut target = view(
        View::target(),
        &[login(3, 2), chunk(0, 0, 1), chunk(i32::MIN, 0, 2)],
    );
    target
        .apply(&p(C::SetChunkCacheRadius(play::Distance {
            distance: i32::MAX,
        })))
        .unwrap();
    target
        .apply(&p(C::SetChunkCacheCenter(play::SetChunkCacheCenter {
            x: i32::MAX,
            z: 0,
        })))
        .unwrap();
    // From the far edge, i32::MAX columns of radius reach the column at 0, not the one
    // at the other edge, 2^32 columns away.
    assert_eq!(target.counts().0, 1);
}
