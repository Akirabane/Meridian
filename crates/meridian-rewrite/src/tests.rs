use super::*;
use horizon_protocol::{ids::play as ids_play, packets::play};

/// The player is 7 on the backend and 1 on the client; 3 is another entity.
const SWAP: Swap = Swap {
    backend: 7,
    client: 1,
};

fn body(packet: &C) -> (i32, Vec<u8>) {
    let encoded = packet.encode();
    (packet.id(), payload_of(&encoded))
}

fn rewrite(packet: C) -> C {
    let (id, payload) = body(&packet);
    let out = SWAP.clientbound(id, payload).unwrap();
    C::decode(id, &out).unwrap()
}

#[test]
fn the_swap_exchanges_two_ids_and_is_its_own_inverse() {
    for id in [-5, 0, 1, 3, 7, 8, i32::MAX] {
        assert_eq!(SWAP.map(SWAP.map(id)), id);
    }
    assert_eq!(SWAP.map(7), 1);
    assert_eq!(SWAP.map(1), 7, "a backend entity with the client's id");
    assert_eq!(SWAP.map(3), 3);
    assert!(Swap::new(4, 4).is_identity());
}

#[test]
fn entity_ids_are_swapped_in_clientbound_packets() {
    assert_eq!(
        rewrite(C::RemoveEntities(play::RemoveEntities {
            ids: vec![7, 3, 1]
        })),
        C::RemoveEntities(play::RemoveEntities { ids: vec![1, 3, 7] })
    );
    assert_eq!(
        rewrite(C::UpdateMobEffect(play::UpdateMobEffect {
            id: 7,
            effect: 2,
            amplifier: 0,
            duration: 100,
            flags: 2,
        })),
        C::UpdateMobEffect(play::UpdateMobEffect {
            id: 1,
            effect: 2,
            amplifier: 0,
            duration: 100,
            flags: 2,
        })
    );
    assert_eq!(
        rewrite(C::SetPassengers(play::SetPassengers {
            vehicle: 3,
            passengers: vec![7]
        })),
        C::SetPassengers(play::SetPassengers {
            vehicle: 3,
            passengers: vec![1]
        })
    );
    assert_eq!(
        rewrite(C::TakeItemEntity(play::TakeItemEntity {
            item: 3,
            player: 7,
            amount: 1
        })),
        C::TakeItemEntity(play::TakeItemEntity {
            item: 3,
            player: 1,
            amount: 1
        })
    );
    assert_eq!(
        rewrite(C::DamageEvent(play::DamageEvent {
            id: 3,
            source_type: 1,
            cause: Some(7),
            direct: None,
            position: None,
        })),
        C::DamageEvent(play::DamageEvent {
            id: 3,
            source_type: 1,
            cause: Some(1),
            direct: None,
            position: None,
        })
    );
    // No holder stays no holder, whatever the swap.
    let swap_zero = Swap::new(0, 1);
    let (id, payload) = body(&C::SetEntityLink(play::SetEntityLink {
        source: 3,
        dest: 0,
    }));
    let out = swap_zero.clientbound(id, payload).unwrap();
    assert_eq!(
        C::decode(id, &out).unwrap(),
        C::SetEntityLink(play::SetEntityLink { source: 3, dest: 0 })
    );
}

#[test]
fn a_backend_entity_with_the_clients_id_takes_the_backends_player_id() {
    assert_eq!(
        rewrite(C::AddEntity(play::AddEntity {
            id: 1,
            uuid: 9,
            kind: 5,
            position: [0.0; 3],
            velocity: [0.0; 3],
            pitch: 0,
            yaw: 0,
            head_yaw: 0,
            data: 0,
        })),
        C::AddEntity(play::AddEntity {
            id: 7,
            uuid: 9,
            kind: 5,
            position: [0.0; 3],
            velocity: [0.0; 3],
            pitch: 0,
            yaw: 0,
            head_yaw: 0,
            data: 0,
        })
    );
}

#[test]
fn packets_without_ids_pass_as_bytes() {
    let packet = C::SetHealth(play::SetHealth {
        health: 20.0,
        food: 20,
        saturation: 5.0,
    });
    let (id, payload) = body(&packet);
    assert_eq!(SWAP.clientbound(id, payload.clone()).unwrap(), payload);
    // Bytes that do not even decode pass untouched when the packet has no id.
    assert_eq!(
        SWAP.clientbound(ids_play::clientbound::SET_HEALTH, vec![1, 2])
            .unwrap(),
        vec![1, 2]
    );
}

#[test]
fn the_identity_swap_never_decodes() {
    let identity = Swap::new(4, 4);
    let broken = vec![0xff; 3];
    assert_eq!(
        identity
            .clientbound(ids_play::clientbound::ADD_ENTITY, broken.clone())
            .unwrap(),
        broken
    );
}

#[test]
fn entity_ids_are_swapped_in_serverbound_packets() {
    let attack = S::Attack(play::Attack { id: 1 });
    let encoded = attack.encode();
    let out = SWAP.serverbound(attack.id(), payload_of(&encoded)).unwrap();
    assert_eq!(
        S::decode(attack.id(), &out).unwrap(),
        S::Attack(play::Attack { id: 7 }),
        "the client attacks what it knows as 1, the backend's 7"
    );
    let command = S::PlayerCommand(play::PlayerCommand {
        id: 1,
        action: 0,
        data: 0,
    });
    let out = SWAP
        .serverbound(command.id(), payload_of(&command.encode()))
        .unwrap();
    assert_eq!(
        S::decode(command.id(), &out).unwrap(),
        S::PlayerCommand(play::PlayerCommand {
            id: 7,
            action: 0,
            data: 0
        })
    );
}

#[test]
fn answers_the_proxy_gave_and_old_answers_are_dropped() {
    let mut t = Teleports::default();
    // The old backend's correction 4 is still unanswered at the switch.
    t.sent(4);
    t.switched();
    // The target's correction 1 was answered by the proxy during the prepare.
    t.answered_by_proxy(1);
    assert!(!t.forward(4), "an answer owed to the old backend");
    assert!(!t.forward(1), "an answer the proxy already gave");
    // Each is dropped once: the target may use the same id later.
    assert!(t.forward(1));
    t.sent(2);
    assert!(t.forward(2));
}

#[test]
fn keep_alives_and_pings_are_answered_with_their_payload() {
    let id = 0x0102_0304_0506_0708i64.to_be_bytes();
    assert_eq!(
        answer(false, ids_play::clientbound::KEEP_ALIVE, &id),
        Some((ids_play::serverbound::KEEP_ALIVE, id.to_vec()))
    );
    assert_eq!(
        answer(false, ids_play::clientbound::PING, &[0, 0, 0, 9]),
        Some((ids_play::serverbound::PONG, vec![0, 0, 0, 9]))
    );
    assert_eq!(
        answer(true, configuration::clientbound::KEEP_ALIVE, &id),
        Some((configuration::serverbound::KEEP_ALIVE, id.to_vec()))
    );
    assert_eq!(answer(false, ids_play::clientbound::SET_HEALTH, &[]), None);
}

/// Payloads of every packet id, valid ones changed at random and random ones, from a
/// fixed seed: the swap answers each, without a panic, both ways.
#[test]
fn hostile_payloads_are_rewritten_or_refused_without_a_panic() {
    use meridian_testkit::mutate::{Rng, mutate};
    let valid = [
        C::RemoveEntities(play::RemoveEntities { ids: vec![7, 3, 1] }),
        C::SetPassengers(play::SetPassengers {
            vehicle: 3,
            passengers: vec![7, 1],
        }),
        C::TakeItemEntity(play::TakeItemEntity {
            item: 3,
            player: 7,
            amount: 1,
        }),
        C::DamageEvent(play::DamageEvent {
            id: 3,
            source_type: 1,
            cause: Some(7),
            direct: Some(1),
            position: None,
        }),
        C::SetEntityLink(play::SetEntityLink { source: 7, dest: 3 }),
    ];
    let valid: Vec<(i32, Vec<u8>)> = valid.iter().map(body).collect();
    let mut rng = Rng::new(0x7377_6170);
    for _ in 0..3000 {
        let (id, payload) = if rng.below(2) == 0 {
            let (id, payload) = &valid[rng.below(valid.len())];
            (*id, mutate(&mut rng, payload))
        } else {
            let n = rng.below(48);
            (rng.below(0x90) as i32, rng.bytes(n))
        };
        let _ = SWAP.clientbound(id, payload.clone());
        let _ = SWAP.serverbound(id, payload);
    }
}
