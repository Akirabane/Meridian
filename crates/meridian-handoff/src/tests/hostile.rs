//! What a client says of where it stands, and what a server says of where it put the
//! player, changed at random from a fixed seed: the pose a switch carries is read from
//! them without a panic, whatever numbers they hold.
use crate::prepare::Pose;
use horizon_protocol::{
    ids::play::{clientbound as pc, serverbound as ps},
    packets::play::{self, Clientbound as C},
};
use meridian_testkit::mutate::{Rng, mutate};

#[test]
fn hostile_moves_and_teleports_give_a_pose_without_a_panic() {
    let moves = [
        ps::MOVE_PLAYER_POS,
        ps::MOVE_PLAYER_POS_ROT,
        ps::MOVE_PLAYER_ROT,
        ps::MOVE_PLAYER_STATUS_ONLY,
        ps::ACCEPT_TELEPORTATION,
    ];
    // Three coordinates, two angles and the flags: the longest of the moves.
    let mut valid = Vec::new();
    for v in [1.5f64, 64.0, -2.5] {
        valid.extend(v.to_be_bytes());
    }
    for v in [90.0f32, 10.0] {
        valid.extend(v.to_be_bytes());
    }
    valid.push(1);
    let teleport = C::PlayerPosition(play::PlayerPosition {
        teleport: 4,
        position: [100.0, 64.0, -20.0],
        delta: [0.0; 3],
        yaw: 0.0,
        pitch: 0.0,
        relatives: 0,
    })
    .encode();
    let mut rng = Rng::new(0x706f_7365);
    let mut pose = Pose::default();
    for _ in 0..5000 {
        let id = moves[rng.below(moves.len())];
        let payload = mutate(&mut rng, &valid);
        if let Some(next) = pose.sent_by_client(id, &payload) {
            pose = next;
        }
        let changed = mutate(&mut rng, &teleport);
        let mut r = horizon_protocol::buf::Reader::new(&changed);
        if r.varint().is_ok()
            && let Ok(C::PlayerPosition(p)) = C::decode(pc::PLAYER_POSITION, r.rest())
        {
            pose.placed(&p);
        }
    }
}
