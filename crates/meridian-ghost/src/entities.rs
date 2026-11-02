//! Entities the client shows: what each one is, where it is, and the synced state
//! that changes how it looks.
//!
//! An entity is recreated with Add Entity followed by an exact Teleport Entity:
//! Add Entity carries its angles as bytes and its velocity at low precision, and
//! the client takes position, velocity and rotation from Teleport Entity as they
//! are (`ClientPacketListener.setValuesFromPositionPacket` calls `setPos`,
//! `setDeltaMovement`, `setYRot` and `setXRot`).
use crate::Packet;
use horizon_protocol::{
    buf::{pack_degrees, unpack_degrees},
    entity_data::Value,
    item::Stack,
    packets::play::{self, Clientbound as C, relative},
};
use std::collections::BTreeMap;

/// One entity the client shows.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Entity {
    pub uuid: u128,
    pub kind: i32,
    /// Add Entity's `data`: the object data (a falling block's state, a
    /// projectile's owner, an item frame's direction).
    pub data: i32,
    pub position: [f64; 3],
    pub velocity: [f64; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub head_yaw: i8,
    pub on_ground: bool,
    /// Synced data by index: serializer and value.
    pub metadata: BTreeMap<u8, (i32, Value)>,
    /// Equipment by slot.
    pub equipment: BTreeMap<u8, Option<Stack>>,
    /// Attributes by registry id.
    pub attributes: BTreeMap<i32, play::AttributeSnapshot>,
    /// Effects by registry id: amplifier, duration and flags as last sent.
    pub effects: BTreeMap<i32, (i32, i32, i8)>,
    /// The entities riding this one, in order.
    pub passengers: Vec<i32>,
    /// The entity holding this one's lead, 0 for none.
    pub leash_holder: i32,
}

impl Entity {
    fn new(p: &play::AddEntity) -> Self {
        Entity {
            uuid: p.uuid,
            kind: p.kind,
            data: p.data,
            position: p.position,
            velocity: p.velocity,
            yaw: unpack_degrees(p.yaw),
            pitch: unpack_degrees(p.pitch),
            head_yaw: p.head_yaw,
            on_ground: false,
            metadata: BTreeMap::new(),
            equipment: BTreeMap::new(),
            attributes: BTreeMap::new(),
            effects: BTreeMap::new(),
            passengers: Vec::new(),
            leash_holder: 0,
        }
    }

    /// Whether `other` is the same entity: the same thing, which packets can turn
    /// from this state into the other's. Synced data, equipment and attributes
    /// can be set but never unset, so an entry this one has and the other lacks
    /// means the entity must be made again.
    fn updatable_to(&self, other: &Entity) -> bool {
        self.uuid == other.uuid
            && self.kind == other.kind
            && self.data == other.data
            && self.metadata.keys().all(|k| other.metadata.contains_key(k))
            && self
                .equipment
                .keys()
                .all(|k| other.equipment.contains_key(k))
            && self
                .attributes
                .keys()
                .all(|k| other.attributes.contains_key(k))
    }
}

/// The entities the client shows, by the id the server gave them. The player's
/// own entity is not among them: it is never added with Add Entity.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Entities {
    pub by_id: BTreeMap<i32, Entity>,
}

impl Entities {
    /// Reads a packet that concerns an entity. Packets about an entity the view
    /// does not hold are ignored, as the client ignores them.
    pub fn apply(&mut self, packet: &C) {
        match packet {
            C::AddEntity(p) => {
                // `ClientLevel.addEntity` replaces an entity with the same id.
                self.remove(p.id);
                self.by_id.insert(p.id, Entity::new(p));
            }
            C::RemoveEntities(p) => {
                for &id in &p.ids {
                    self.remove(id);
                }
            }
            C::MoveEntityPos(p) => self.moved(&p.0),
            C::MoveEntityPosRot(p) => self.moved(&p.0),
            C::MoveEntityRot(p) => self.moved(&p.0),
            C::TeleportEntity(p) => {
                if let Some(e) = self.by_id.get_mut(&p.id) {
                    let r = p.relatives;
                    for i in 0..3 {
                        let (position, delta) = (
                            [relative::X, relative::Y, relative::Z][i],
                            [relative::DELTA_X, relative::DELTA_Y, relative::DELTA_Z][i],
                        );
                        e.position[i] = p.position[i]
                            + if r & position != 0 {
                                e.position[i]
                            } else {
                                0.0
                            };
                        e.velocity[i] =
                            p.delta[i] + if r & delta != 0 { e.velocity[i] } else { 0.0 };
                    }
                    e.yaw = p.yaw + if r & relative::Y_ROT != 0 { e.yaw } else { 0.0 };
                    e.pitch = p.pitch
                        + if r & relative::X_ROT != 0 {
                            e.pitch
                        } else {
                            0.0
                        };
                    e.on_ground = p.on_ground;
                }
            }
            C::EntityPositionSync(p) => {
                if let Some(e) = self.by_id.get_mut(&p.id) {
                    e.position = p.path.end();
                    e.yaw = p.yaw;
                    e.pitch = p.pitch;
                    e.on_ground = p.on_ground;
                }
            }
            C::RotateHead(p) => {
                if let Some(e) = self.by_id.get_mut(&p.id) {
                    e.head_yaw = p.head_yaw;
                }
            }
            C::SetEntityMotion(p) => {
                if let Some(e) = self.by_id.get_mut(&p.id) {
                    e.velocity = p.velocity;
                }
            }
            C::SetEntityData(p) => {
                if let Some(e) = self.by_id.get_mut(&p.id) {
                    for (index, serializer, value) in &p.values {
                        e.metadata.insert(*index, (*serializer, value.clone()));
                    }
                }
            }
            C::SetEquipment(p) => {
                if let Some(e) = self.by_id.get_mut(&p.id) {
                    for (slot, stack) in &p.slots {
                        e.equipment.insert(*slot, stack.clone());
                    }
                }
            }
            C::UpdateAttributes(p) => {
                if let Some(e) = self.by_id.get_mut(&p.id) {
                    for snapshot in &p.values {
                        e.attributes.insert(snapshot.attribute, snapshot.clone());
                    }
                }
            }
            C::UpdateMobEffect(p) => {
                if let Some(e) = self.by_id.get_mut(&p.id) {
                    e.effects
                        .insert(p.effect, (p.amplifier, p.duration, p.flags));
                }
            }
            C::RemoveMobEffect(p) => {
                if let Some(e) = self.by_id.get_mut(&p.id) {
                    e.effects.remove(&p.effect);
                }
            }
            C::SetPassengers(p) => {
                if let Some(e) = self.by_id.get_mut(&p.vehicle) {
                    e.passengers.clone_from(&p.passengers);
                }
            }
            C::SetEntityLink(p) => {
                if let Some(e) = self.by_id.get_mut(&p.source) {
                    e.leash_holder = p.dest;
                }
            }
            _ => {}
        }
    }

    fn moved(&mut self, p: &play::MoveEntity) {
        if let Some(e) = self.by_id.get_mut(&p.id) {
            if let Some(delta) = &p.delta {
                e.position = delta.end(e.position);
            }
            if let Some((yaw, pitch)) = p.rotation {
                e.yaw = unpack_degrees(yaw);
                e.pitch = unpack_degrees(pitch);
            }
            e.on_ground = p.on_ground;
        }
    }

    /// An entity leaves: it no longer rides anything or holds a lead.
    fn remove(&mut self, id: i32) {
        if self.by_id.remove(&id).is_none() {
            return;
        }
        for other in self.by_id.values_mut() {
            other.passengers.retain(|&p| p != id);
            if other.leash_holder == id {
                other.leash_holder = 0;
            }
        }
    }

    /// The ids of the entities the client must forget before the target's are
    /// made: those the target does not have, and those it has under the same id
    /// as something else (another UUID or type), which must be made again.
    pub fn to_remove(&self, to: &Entities) -> Vec<i32> {
        self.by_id
            .iter()
            .filter(|(id, e)| to.by_id.get(id).is_none_or(|t| !e.updatable_to(t)))
            .map(|(&id, _)| id)
            .collect()
    }

    /// The packets that make or update every entity of `to`, once `removed` are
    /// gone, then the rides and leads between them.
    pub fn delta(&self, to: &Entities, removed: &[i32], out: &mut Vec<Packet>) {
        let mut links = Vec::new();
        for (&id, target) in &to.by_id {
            let kept = self.by_id.get(&id).filter(|_| !removed.contains(&id));
            match kept {
                Some(current) => update(id, current, target, out),
                None => create(id, target, out),
            }
            // What the client has once `removed` are gone: a removed entity no
            // longer rides this one or holds its lead.
            let (passengers, holder): (Vec<i32>, i32) = kept.map_or((Vec::new(), 0), |c| {
                (
                    c.passengers
                        .iter()
                        .copied()
                        .filter(|p| !removed.contains(p))
                        .collect(),
                    if removed.contains(&c.leash_holder) {
                        0
                    } else {
                        c.leash_holder
                    },
                )
            });
            if target.passengers != passengers {
                links.push(Packet::of(&C::SetPassengers(play::SetPassengers {
                    vehicle: id,
                    passengers: target.passengers.clone(),
                })));
            }
            if target.leash_holder != holder {
                links.push(Packet::of(&C::SetEntityLink(play::SetEntityLink {
                    source: id,
                    dest: target.leash_holder,
                })));
            }
        }
        // Rides and leads name two entities: both exist by now.
        out.extend(links);
    }
}

/// An entity made from nothing: Add Entity, the exact position, then everything
/// synced.
fn create(id: i32, e: &Entity, out: &mut Vec<Packet>) {
    out.push(Packet::of(&C::AddEntity(play::AddEntity {
        id,
        uuid: e.uuid,
        kind: e.kind,
        position: e.position,
        velocity: e.velocity,
        pitch: pack_degrees(e.pitch),
        yaw: pack_degrees(e.yaw),
        head_yaw: e.head_yaw,
        data: e.data,
    })));
    out.push(teleport(id, e));
    let blank = Entity {
        metadata: BTreeMap::new(),
        equipment: BTreeMap::new(),
        attributes: BTreeMap::new(),
        effects: BTreeMap::new(),
        ..e.clone()
    };
    synced(id, &blank, e, out);
}

/// An entity the client already shows as the same thing: only what differs.
fn update(id: i32, from: &Entity, to: &Entity, out: &mut Vec<Packet>) {
    if (
        from.position,
        from.velocity,
        from.yaw,
        from.pitch,
        from.on_ground,
    ) != (to.position, to.velocity, to.yaw, to.pitch, to.on_ground)
    {
        out.push(teleport(id, to));
    }
    if from.head_yaw != to.head_yaw {
        out.push(Packet::of(&C::RotateHead(play::RotateHead {
            id,
            head_yaw: to.head_yaw,
        })));
    }
    synced(id, from, to, out);
}

/// Teleport Entity with everything absolute.
fn teleport(id: i32, e: &Entity) -> Packet {
    Packet::of(&C::TeleportEntity(play::TeleportEntity {
        id,
        position: e.position,
        delta: e.velocity,
        yaw: e.yaw,
        pitch: e.pitch,
        relatives: 0,
        on_ground: e.on_ground,
    }))
}

/// Synced data, equipment, attributes and effects that differ.
fn synced(id: i32, from: &Entity, to: &Entity, out: &mut Vec<Packet>) {
    let values: Vec<(u8, i32, Value)> = to
        .metadata
        .iter()
        .filter(|(k, v)| from.metadata.get(k) != Some(v))
        .map(|(&k, (s, v))| (k, *s, v.clone()))
        .collect();
    if !values.is_empty() {
        out.push(Packet::of(&C::SetEntityData(play::SetEntityData {
            id,
            values,
        })));
    }
    let slots: Vec<(u8, Option<Stack>)> = to
        .equipment
        .iter()
        .filter(|(k, v)| from.equipment.get(k) != Some(v))
        .map(|(&k, v)| (k, v.clone()))
        .collect();
    if !slots.is_empty() {
        out.push(Packet::of(&C::SetEquipment(play::SetEquipment {
            id,
            slots,
        })));
    }
    let attributes: Vec<play::AttributeSnapshot> = to
        .attributes
        .iter()
        .filter(|(k, v)| from.attributes.get(k) != Some(v))
        .map(|(_, v)| v.clone())
        .collect();
    if !attributes.is_empty() {
        out.push(Packet::of(&C::UpdateAttributes(play::UpdateAttributes {
            id,
            values: attributes,
        })));
    }
    for &effect in from.effects.keys().filter(|k| !to.effects.contains_key(k)) {
        out.push(Packet::of(&C::RemoveMobEffect(play::RemoveMobEffect {
            id,
            effect,
        })));
    }
    for (&effect, &(amplifier, duration, flags)) in &to.effects {
        if from.effects.get(&effect) != Some(&(amplifier, duration, flags)) {
            out.push(Packet::of(&C::UpdateMobEffect(play::UpdateMobEffect {
                id,
                effect,
                amplifier,
                duration,
                flags,
            })));
        }
    }
}
