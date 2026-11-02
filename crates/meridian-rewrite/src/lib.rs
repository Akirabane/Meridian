//! What is numbered per connection or per server, translated between the client
//! and the backend it is relayed to (DESIGN.md section 7). This first version does
//! the minimum a switch needs, starting with:
//!
//! - [`Swap`]: the player's own entity id. The client keeps the id of its first
//!   Login for the whole session; each backend gives the player its own. The swap
//!   exchanges the two numbers and leaves every other id as the backend gave it, so
//!   the client sees the active backend's entities under the backend's ids. It is
//!   its own inverse, and an entity of the backend that happens to carry the
//!   client's id takes the backend's player id instead, so nothing collides.
//!
//! # The packets with an entity id
//!
//! [`Swap::clientbound`] decodes these clientbound Play packets, changes their ids
//! and writes them again: Add Entity (`id`), Animate, Block Destruction, Damage Event
//! (the entity and the optional cause and direct source), Entity Event, Entity
//! Position Sync, Hurt Animation, Mount Screen Open, the three Move Entity packets,
//! Move Minecart Along Track, Player Combat Kill, Player Look At (its entity
//! target), Projectile Power, Remove Entities, Remove Mob Effect, Rotate Head, Set
//! Camera, Set Entity Data (`id`), Set Entity Link (both ends), Set Entity Motion,
//! Set Equipment, Set Passengers (the vehicle and every rider), Sound Entity, Swing
//! Animation, Take Item Entity (item and collector), Teleport Entity, Update
//! Attributes and Update Mob Effect.
//!
//! [`Swap::serverbound`] does the same for Attack, Entity Tag Query, Interact, Pick
//! Item From Entity, Player Command, Set Command Minecart and Spectator Action.
//!
//! Every other packet passes as bytes.
//!
//! # Not rewritten
//!
//! - **Add Entity's object data**, which holds an entity id for some kinds (a
//!   projectile's owner, a fishing hook's angler). Which kinds, and how they encode
//!   it, is not verified here; an owner equal to one of the two swapped ids is left
//!   as it is.
//! - **Synced data values that name an entity** (optional entity references in Set
//!   Entity Data): not decoded by kind, so not rewritten.
//! - **Debug Entity Value**, read as raw bytes by the protocol crate.
//! - **Login's player id.** The client gets one Login, the first, which sets the
//!   client's id; no later Login reaches it (it is a forbidden packet).
//! - **Whether Spectator Action's optional target is an entity id** as the protocol
//!   crate reads it is not verified against the game.
use horizon_protocol::{
    Result,
    buf::Reader,
    ids::play,
    packets::play::{Clientbound as C, Serverbound as S},
};

/// The exchange of the player's entity id between a backend's numbering and the
/// client's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Swap {
    /// The player's id on the backend.
    pub backend: i32,
    /// The player's id on the client: that of the client's first Login.
    pub client: i32,
}

impl Swap {
    pub fn new(backend: i32, client: i32) -> Self {
        Swap { backend, client }
    }

    /// Whether the swap changes nothing (the backend of the client's first Login).
    pub fn is_identity(&self) -> bool {
        self.backend == self.client
    }

    /// An id in one numbering, in the other. The same function goes both ways.
    pub fn map(&self, id: i32) -> i32 {
        if id == self.backend {
            self.client
        } else if id == self.client {
            self.backend
        } else {
            id
        }
    }

    /// A clientbound Play packet's payload, its entity ids changed. A packet with no
    /// entity id, or a swap that changes nothing, comes back as it was.
    pub fn clientbound(&self, id: i32, payload: Vec<u8>) -> Result<Vec<u8>> {
        if self.is_identity() || !clientbound_has_ids(id) {
            return Ok(payload);
        }
        let mut packet = C::decode(id, &payload)?;
        self.clientbound_ids(&mut packet);
        Ok(payload_of(&packet.encode()))
    }

    /// A serverbound Play packet's payload, its entity ids changed.
    pub fn serverbound(&self, id: i32, payload: Vec<u8>) -> Result<Vec<u8>> {
        if self.is_identity() || !serverbound_has_ids(id) {
            return Ok(payload);
        }
        let mut packet = S::decode(id, &payload)?;
        let m = |id: &mut i32| *id = self.map(*id);
        match &mut packet {
            S::Attack(p) => m(&mut p.id),
            S::EntityTagQuery(p) => m(&mut p.entity),
            S::Interact(p) => m(&mut p.id),
            S::PickItemFromEntity(p) => m(&mut p.id),
            S::PlayerCommand(p) => m(&mut p.id),
            S::SetCommandMinecart(p) => m(&mut p.entity),
            S::SpectatorAction(p) => {
                if let Some(target) = p.target.as_mut() {
                    m(target);
                }
            }
            _ => {}
        }
        Ok(payload_of(&packet.encode()))
    }

    /// The entity ids of a typed clientbound packet, changed in place.
    pub fn clientbound_ids(&self, packet: &mut C) {
        let m = |id: &mut i32| *id = self.map(*id);
        match packet {
            C::AddEntity(p) => m(&mut p.id),
            C::Animate(p) => m(&mut p.id),
            C::BlockDestruction(p) => m(&mut p.id),
            C::DamageEvent(p) => {
                m(&mut p.id);
                p.cause.iter_mut().for_each(m);
                p.direct.iter_mut().for_each(m);
            }
            C::EntityEvent(p) => m(&mut p.id),
            C::EntityPositionSync(p) => m(&mut p.id),
            C::HurtAnimation(p) => m(&mut p.id),
            C::MountScreenOpen(p) => m(&mut p.entity),
            C::MoveEntityPos(p) => m(&mut p.0.id),
            C::MoveEntityPosRot(p) => m(&mut p.0.id),
            C::MoveEntityRot(p) => m(&mut p.0.id),
            C::MoveMinecartAlongTrack(p) => m(&mut p.id),
            C::PlayerCombatKill(p) => m(&mut p.id),
            C::PlayerLookAt(p) => {
                if let Some(entity) = p.entity.as_mut() {
                    m(&mut entity.id);
                }
            }
            C::ProjectilePower(p) => m(&mut p.id),
            C::RemoveEntities(p) => p.ids.iter_mut().for_each(m),
            C::RemoveMobEffect(p) => m(&mut p.id),
            C::RotateHead(p) => m(&mut p.id),
            C::SetCamera(p) => m(&mut p.camera_id),
            C::SetEntityData(p) => m(&mut p.id),
            // 0 is "no holder", never an entity.
            C::SetEntityLink(p) => {
                m(&mut p.source);
                if p.dest != 0 {
                    m(&mut p.dest);
                }
            }
            C::SetEntityMotion(p) => m(&mut p.id),
            C::SetEquipment(p) => m(&mut p.id),
            C::SetPassengers(p) => {
                m(&mut p.vehicle);
                p.passengers.iter_mut().for_each(m);
            }
            C::SoundEntity(p) => m(&mut p.id),
            C::SwingAnimation(p) => m(&mut p.id),
            C::TakeItemEntity(p) => {
                m(&mut p.item);
                m(&mut p.player);
            }
            C::TeleportEntity(p) => m(&mut p.id),
            C::UpdateAttributes(p) => m(&mut p.id),
            C::UpdateMobEffect(p) => m(&mut p.id),
            _ => {}
        }
    }
}

/// Whether a clientbound Play packet carries an entity id that [`Swap`] changes.
pub fn clientbound_has_ids(id: i32) -> bool {
    use play::clientbound as c;
    matches!(
        id,
        c::ADD_ENTITY
            | c::ANIMATE
            | c::BLOCK_DESTRUCTION
            | c::DAMAGE_EVENT
            | c::ENTITY_EVENT
            | c::ENTITY_POSITION_SYNC
            | c::HURT_ANIMATION
            | c::MOUNT_SCREEN_OPEN
            | c::MOVE_ENTITY_POS
            | c::MOVE_ENTITY_POS_ROT
            | c::MOVE_ENTITY_ROT
            | c::MOVE_MINECART_ALONG_TRACK
            | c::PLAYER_COMBAT_KILL
            | c::PLAYER_LOOK_AT
            | c::PROJECTILE_POWER
            | c::REMOVE_ENTITIES
            | c::REMOVE_MOB_EFFECT
            | c::ROTATE_HEAD
            | c::SET_CAMERA
            | c::SET_ENTITY_DATA
            | c::SET_ENTITY_LINK
            | c::SET_ENTITY_MOTION
            | c::SET_EQUIPMENT
            | c::SET_PASSENGERS
            | c::SOUND_ENTITY
            | c::SWING_ANIMATION
            | c::TAKE_ITEM_ENTITY
            | c::TELEPORT_ENTITY
            | c::UPDATE_ATTRIBUTES
            | c::UPDATE_MOB_EFFECT
    )
}

/// Whether a serverbound Play packet carries an entity id that [`Swap`] changes.
pub fn serverbound_has_ids(id: i32) -> bool {
    use play::serverbound as s;
    matches!(
        id,
        s::ATTACK
            | s::ENTITY_TAG_QUERY
            | s::INTERACT
            | s::PICK_ITEM_FROM_ENTITY
            | s::PLAYER_COMMAND
            | s::SET_COMMAND_MINECART
            | s::SPECTATOR_ACTION
    )
}

/// The payload of an encoded packet body.
fn payload_of(body: &[u8]) -> Vec<u8> {
    let mut r = Reader::new(body);
    // An encoded body starts with its id.
    let _ = r.varint();
    r.rest().to_vec()
}

#[cfg(test)]
mod tests;
