//! The player's own state: what differs is sent, in the target's numbering.
use super::*;
use horizon_protocol::item::Stack;

fn inventory(state: i32, hotbar_first: Option<Stack>, carried: Option<Stack>) -> Packet {
    let mut items = vec![None; 46];
    items[36] = hotbar_first;
    p(C::ContainerSetContent(play::ContainerSetContent {
        container: 0,
        state,
        items,
        carried,
    }))
}

fn game_mode(mode: f32) -> Packet {
    p(C::GameEvent(play::GameEvent {
        event: 3,
        param: mode,
    }))
}

fn effect(id: i32, effect: i32, amplifier: i32) -> Packet {
    p(C::UpdateMobEffect(play::UpdateMobEffect {
        id,
        effect,
        amplifier,
        duration: 200,
        flags: 2,
    }))
}

#[test]
fn health_experience_inventory_and_game_mode_follow_the_target() {
    let old = vec![
        login(1, 2),
        p(C::SetHealth(play::SetHealth {
            health: 20.0,
            food: 20,
            saturation: 5.0,
        })),
        p(C::SetExperience(play::SetExperience {
            progress: 0.5,
            level: 3,
            total: 40,
        })),
        p(C::SetHeldSlot(play::SetHeldSlot { slot: 0 })),
        inventory(1, Some(Stack::new(5, 1)), None),
    ];
    let new = vec![
        login(9, 2),
        p(C::SetHealth(play::SetHealth {
            health: 12.5,
            food: 17,
            saturation: 0.0,
        })),
        p(C::SetExperience(play::SetExperience {
            progress: 0.5,
            level: 3,
            total: 40,
        })),
        p(C::SetHeldSlot(play::SetHeldSlot { slot: 4 })),
        inventory(7, Some(Stack::new(6, 32)), Some(Stack::new(8, 1))),
        game_mode(1.0),
    ];
    let (_, delta) = switch(&old, &new);
    let order = ids_of(&delta);
    for id in [
        ids::SET_HEALTH,
        ids::SET_HELD_SLOT,
        ids::CONTAINER_SET_CONTENT,
        ids::GAME_EVENT,
    ] {
        assert!(order.contains(&id), "{id} missing from {order:?}");
    }
    // The experience is the same on both sides.
    assert!(!order.contains(&ids::SET_EXPERIENCE), "{order:?}");
}

/// Open Screen for `container`, a chest-like menu titled "hi". Built from its bytes:
/// the title is an NBT string tag (type 8, length, text).
fn open_screen(container: i32) -> Packet {
    let mut payload = vec![];
    horizon_protocol::buf::write_varint(&mut payload, container);
    horizon_protocol::buf::write_varint(&mut payload, 2);
    payload.extend_from_slice(&[8, 0, 2, b'h', b'i']);
    let packet = Packet {
        id: ids::OPEN_SCREEN,
        payload,
    };
    assert!(matches!(packet.decode(), Ok(C::OpenScreen(_))));
    packet
}

#[test]
fn an_open_container_is_closed_first() {
    let old = vec![login(1, 2), inventory(1, None, None), open_screen(3)];
    let new = vec![login(1, 2), inventory(1, None, None)];
    let (after, delta) = switch(&old, &new);
    assert_eq!(ids_of(&delta)[0], ids::CONTAINER_CLOSE);
    assert!(after.player.open.is_none());
}

#[test]
fn the_targets_open_container_is_opened_with_its_contents() {
    let mut contents = vec![None; 27];
    contents[4] = Some(Stack::new(3, 2));
    let filled = p(C::ContainerSetContent(play::ContainerSetContent {
        container: 5,
        state: 2,
        items: contents,
        carried: None,
    }));
    let old = vec![login(1, 2), inventory(1, None, None), open_screen(3)];
    let new = vec![
        login(1, 2),
        inventory(1, None, None),
        open_screen(5),
        filled,
    ];
    let (after, delta) = switch(&old, &new);
    let order = ids_of(&delta);
    assert_eq!(order[0], ids::CONTAINER_CLOSE);
    assert_eq!(
        order[order.len() - 2..],
        [ids::OPEN_SCREEN, ids::CONTAINER_SET_CONTENT]
    );
    assert_eq!(after.player.open.as_ref().unwrap().items.len(), 27);
}
#[test]
fn a_hotbar_slot_lands_in_the_inventory_menu() {
    let ghost = view(
        View::ghost(),
        &[
            login(1, 2),
            inventory(1, None, None),
            p(C::SetPlayerInventory(play::SetPlayerInventory {
                slot: 2,
                item: Some(Stack::new(9, 4)),
            })),
            p(C::SetPlayerInventory(play::SetPlayerInventory {
                slot: 40,
                item: Some(Stack::new(10, 1)),
            })),
        ],
    );
    let items = ghost.player.inventory.as_ref().unwrap();
    assert_eq!(
        items[38],
        Some(Stack::new(9, 4)),
        "hotbar 2 is menu slot 38"
    );
    assert_eq!(
        items[45],
        Some(Stack::new(10, 1)),
        "the off hand is slot 45"
    );
}

#[test]
fn the_players_effects_go_under_the_targets_id() {
    let old = vec![login(1, 2), effect(1, 5, 0), effect(1, 6, 1)];
    let new = vec![login(9, 2), effect(9, 6, 1), effect(9, 7, 2)];
    let (_, delta) = switch(&old, &new);
    let decoded: Vec<C> = delta
        .iter()
        .filter(|p| p.id != ids::BUNDLE_DELIMITER)
        .map(|p| p.decode().unwrap())
        .collect();
    assert_eq!(
        decoded,
        vec![
            C::RemoveMobEffect(play::RemoveMobEffect { id: 9, effect: 5 }),
            C::UpdateMobEffect(play::UpdateMobEffect {
                id: 9,
                effect: 7,
                amplifier: 2,
                duration: 200,
                flags: 2,
            }),
        ]
    );
}

#[test]
fn cooldowns_the_target_lacks_are_cleared() {
    let cooldown = |group: &str, duration| {
        p(C::Cooldown(play::Cooldown {
            group: group.into(),
            duration,
        }))
    };
    let old = vec![login(1, 2), cooldown("minecraft:ender_pearl", 20)];
    let new = vec![login(1, 2), cooldown("minecraft:shield", 100)];
    let (after, _) = switch(&old, &new);
    assert_eq!(after.player.cooldowns.len(), 1);
}
