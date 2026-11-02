//! Entities: removed, made and updated so that the ghost ends as the target.
use super::*;
use horizon_protocol::entity_data::Value;

fn metadata(id: i32, index: u8, value: i8) -> Packet {
    p(C::SetEntityData(play::SetEntityData {
        id,
        values: vec![(index, 0, Value::Byte(value))],
    }))
}

#[test]
fn entities_only_in_the_ghost_go_and_the_targets_are_made() {
    let old = vec![
        login(1, 2),
        add(10, 0xa, 5, [1.0, 64.0, 1.0]),
        add(11, 0xb, 5, [2.0, 64.0, 2.0]),
    ];
    let new = vec![
        login(1, 2),
        add(11, 0xb, 5, [2.0, 64.0, 2.0]),
        add(12, 0xc, 7, [3.0, 64.0, 3.0]),
        metadata(12, 0, 0x20),
    ];
    let (after, delta) = switch(&old, &new);
    let order = ids_of(&delta);
    assert_eq!(order[0], ids::REMOVE_ENTITIES);
    let removed = match delta[1].decode().unwrap() {
        C::RemoveEntities(r) => r.ids,
        other => panic!("{other:?}"),
    };
    assert_eq!(removed, vec![10], "entity 11 is the same on both sides");
    assert_eq!(after.counts().1, 2);
}

#[test]
fn an_id_reused_for_another_entity_is_removed_then_made_again() {
    // The same id for a zombie on one server and an item on the other, and the
    // same id and type with another UUID.
    let old = vec![
        login(1, 2),
        add(20, 0x1, 5, [0.0, 64.0, 0.0]),
        add(21, 0x2, 5, [0.0, 64.0, 0.0]),
    ];
    let new = vec![
        login(1, 2),
        add(20, 0x1, 9, [0.0, 64.0, 0.0]),
        add(21, 0x3, 5, [0.0, 64.0, 0.0]),
    ];
    let (_, delta) = switch(&old, &new);
    let decoded: Vec<C> = delta
        .iter()
        .filter(|p| p.id != ids::BUNDLE_DELIMITER)
        .map(|p| p.decode().unwrap())
        .collect();
    let C::RemoveEntities(removed) = &decoded[0] else {
        panic!("the removal comes first: {decoded:?}");
    };
    assert_eq!(removed.ids, vec![20, 21]);
    let added: Vec<(i32, u128, i32)> = decoded
        .iter()
        .filter_map(|p| match p {
            C::AddEntity(a) => Some((a.id, a.uuid, a.kind)),
            _ => None,
        })
        .collect();
    assert_eq!(added, vec![(20, 0x1, 9), (21, 0x3, 5)]);
}

#[test]
fn a_moved_entity_is_updated_in_place_with_its_exact_state() {
    let old = vec![
        login(1, 2),
        add(30, 0x30, 5, [0.0, 64.0, 0.0]),
        metadata(30, 0, 1),
    ];
    let mut new = old.clone();
    new.push(p(C::MoveEntityPosRot(play::MoveEntityPosRot(
        play::MoveEntity {
            id: 30,
            delta: Some(play::VecDelta::Linear([4096, 0, -2048])),
            rotation: Some((32, -8)),
            on_ground: true,
        },
    ))));
    new.push(p(C::SetEntityMotion(play::SetEntityMotion {
        id: 30,
        velocity: [0.25, 0.0, -0.125],
    })));
    new.push(metadata(30, 1, 3));
    let (_, delta) = switch(&old, &new);
    let order = ids_of(&delta);
    assert!(!order.contains(&ids::REMOVE_ENTITIES), "{order:?}");
    assert!(!order.contains(&ids::ADD_ENTITY), "{order:?}");
    assert!(order.contains(&ids::TELEPORT_ENTITY));
    // Only the new index of synced data goes.
    let data: Vec<_> = delta
        .iter()
        .filter(|p| p.id == ids::SET_ENTITY_DATA)
        .map(|p| match p.decode().unwrap() {
            C::SetEntityData(d) => d.values,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(data, vec![vec![(1, 0, Value::Byte(3))]]);
}

#[test]
fn synced_data_the_target_lacks_makes_the_entity_again() {
    // The client cannot unset a synced value: the entity is made again.
    let old = vec![
        login(1, 2),
        add(40, 0x40, 5, [0.0, 64.0, 0.0]),
        metadata(40, 3, 1),
    ];
    let new = vec![login(1, 2), add(40, 0x40, 5, [0.0, 64.0, 0.0])];
    let (_, delta) = switch(&old, &new);
    let order = ids_of(&delta);
    assert_eq!(order[..2], [ids::REMOVE_ENTITIES, ids::ADD_ENTITY]);
}

#[test]
fn rides_and_leads_survive_an_entity_made_again() {
    // Entity 51 rides 50 and holds 52's lead on both sides, but 51 changes UUID:
    // removing it unseats it and drops the lead, which the delta puts back.
    let old = vec![
        login(1, 2),
        add(50, 0x50, 5, [0.0, 64.0, 0.0]),
        add(51, 0x51, 5, [0.0, 64.0, 0.0]),
        add(52, 0x52, 5, [0.0, 64.0, 0.0]),
        p(C::SetPassengers(play::SetPassengers {
            vehicle: 50,
            passengers: vec![51],
        })),
        p(C::SetEntityLink(play::SetEntityLink {
            source: 52,
            dest: 51,
        })),
    ];
    let mut new = old.clone();
    new[2] = add(51, 0x99, 5, [0.0, 64.0, 0.0]);
    let (_, delta) = switch(&old, &new);
    let order = ids_of(&delta);
    let last = &order[order.len() - 2..];
    assert_eq!(last, [ids::SET_PASSENGERS, ids::SET_ENTITY_LINK]);
}
