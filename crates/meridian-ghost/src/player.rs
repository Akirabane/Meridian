//! The player's own state as the client holds it: health, food, experience,
//! abilities, game mode, held slot, effects, attributes, item cooldowns, the
//! inventory, the cursor stack and the open container.
use crate::Packet;
use horizon_protocol::{
    item::Stack,
    packets::play::{self, Clientbound as C},
};
use std::collections::BTreeMap;

/// `ClientboundGameEventPacket.CHANGE_GAME_MODE`.
pub(crate) const CHANGE_GAME_MODE: u8 = 3;

/// The slots of the player's inventory menu (`InventoryMenu`): crafting result
/// and grid, armour, the 27 storage slots, the hotbar and the off hand.
const INVENTORY_SLOTS: usize = 46;

/// A container the player has open, other than the inventory.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Open {
    /// The Open Screen packet: container id, menu type and title.
    pub screen: play::OpenScreen,
    pub items: Vec<Option<Stack>>,
    /// The state id of the last full content, in the server's numbering.
    pub state: i32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Player {
    /// Set Health: health, food, saturation.
    pub health: Option<(f32, i32, f32)>,
    /// Set Experience: progress, level, total.
    pub experience: Option<(f32, i32, i32)>,
    /// Player Abilities: flags, flying speed, walking speed.
    pub abilities: Option<(i8, f32, f32)>,
    pub held_slot: Option<i32>,
    pub game_mode: Option<i32>,
    /// Effects on the player by registry id: amplifier, duration and flags as
    /// last sent.
    pub effects: BTreeMap<i32, (i32, i32, i8)>,
    pub attributes: BTreeMap<i32, play::AttributeSnapshot>,
    /// Item cooldowns by group, in ticks as last sent.
    pub cooldowns: BTreeMap<String, i32>,
    /// The inventory menu's slots, once a full content was sent.
    pub inventory: Option<Vec<Option<Stack>>>,
    /// The state id of the inventory's last full content.
    pub inventory_state: i32,
    pub carried: Option<Stack>,
    pub open: Option<Open>,
}

impl Player {
    /// Reads a packet about the player. `own` is the player's entity id.
    pub fn apply(&mut self, packet: &C, own: Option<i32>) {
        let mine = |id: i32| own == Some(id);
        match packet {
            C::SetHealth(p) => self.health = Some((p.health, p.food, p.saturation)),
            C::SetExperience(p) => self.experience = Some((p.progress, p.level, p.total)),
            C::PlayerAbilities(p) => {
                self.abilities = Some((p.flags, p.flying_speed, p.walking_speed));
            }
            C::SetHeldSlot(p) => self.held_slot = Some(p.slot),
            C::GameEvent(p) if p.event == CHANGE_GAME_MODE => {
                self.game_mode = Some(p.param as i32);
            }
            C::UpdateMobEffect(p) if mine(p.id) => {
                self.effects
                    .insert(p.effect, (p.amplifier, p.duration, p.flags));
            }
            C::RemoveMobEffect(p) if mine(p.id) => {
                self.effects.remove(&p.effect);
            }
            C::UpdateAttributes(p) if mine(p.id) => {
                for snapshot in &p.values {
                    self.attributes.insert(snapshot.attribute, snapshot.clone());
                }
            }
            C::Cooldown(p) => {
                if p.duration > 0 {
                    self.cooldowns.insert(p.group.clone(), p.duration);
                } else {
                    self.cooldowns.remove(&p.group);
                }
            }
            C::ContainerSetContent(p) if p.container == 0 => {
                self.inventory = Some(p.items.clone());
                self.inventory_state = p.state;
                self.carried.clone_from(&p.carried);
            }
            C::ContainerSetContent(p) => {
                if let Some(open) = self
                    .open
                    .as_mut()
                    .filter(|o| o.screen.container == p.container)
                {
                    open.items.clone_from(&p.items);
                    open.state = p.state;
                    self.carried.clone_from(&p.carried);
                }
            }
            C::ContainerSetSlot(p) => {
                let slot = usize::try_from(p.slot).ok();
                let items = if p.container == 0 {
                    self.inventory.as_mut()
                } else {
                    self.open
                        .as_mut()
                        .filter(|o| o.screen.container == p.container)
                        .map(|o| &mut o.items)
                };
                if let (Some(items), Some(slot)) = (items, slot)
                    && let Some(item) = items.get_mut(slot)
                {
                    item.clone_from(&p.item);
                }
            }
            C::SetPlayerInventory(p) => {
                if let (Some(items), Some(slot)) = (self.inventory.as_mut(), menu_slot(p.slot))
                    && let Some(item) = items.get_mut(slot)
                {
                    item.clone_from(&p.item);
                }
            }
            C::SetCursorItem(p) => self.carried.clone_from(&p.item),
            C::OpenScreen(p) => {
                self.open = Some(Open {
                    screen: p.clone(),
                    items: Vec::new(),
                    state: 0,
                });
            }
            C::ContainerClose(p)
                if self
                    .open
                    .as_ref()
                    .is_some_and(|o| o.screen.container == p.container) =>
            {
                self.open = None;
            }
            _ => {}
        }
    }

    /// The packets that bring the player from `self` to `to`. `own` is the
    /// player's entity id on the target server.
    pub fn delta(&self, to: &Player, own: Option<i32>, out: &mut Vec<Packet>) {
        if let Some((health, food, saturation)) = to.health.filter(|_| to.health != self.health) {
            out.push(Packet::of(&C::SetHealth(play::SetHealth {
                health,
                food,
                saturation,
            })));
        }
        if let Some((progress, level, total)) =
            to.experience.filter(|_| to.experience != self.experience)
        {
            out.push(Packet::of(&C::SetExperience(play::SetExperience {
                progress,
                level,
                total,
            })));
        }
        if let Some((flags, flying_speed, walking_speed)) =
            to.abilities.filter(|_| to.abilities != self.abilities)
        {
            out.push(Packet::of(&C::PlayerAbilities(play::PlayerAbilities {
                flags,
                flying_speed,
                walking_speed,
            })));
        }
        if let Some(slot) = to.held_slot.filter(|_| to.held_slot != self.held_slot) {
            out.push(Packet::of(&C::SetHeldSlot(play::SetHeldSlot { slot })));
        }
        if let Some(mode) = to.game_mode.filter(|_| to.game_mode != self.game_mode) {
            out.push(Packet::of(&C::GameEvent(play::GameEvent {
                event: CHANGE_GAME_MODE,
                param: mode as f32,
            })));
        }
        if let Some(id) = own {
            for &effect in self.effects.keys().filter(|k| !to.effects.contains_key(k)) {
                out.push(Packet::of(&C::RemoveMobEffect(play::RemoveMobEffect {
                    id,
                    effect,
                })));
            }
            for (&effect, &(amplifier, duration, flags)) in &to.effects {
                if self.effects.get(&effect) != Some(&(amplifier, duration, flags)) {
                    out.push(Packet::of(&C::UpdateMobEffect(play::UpdateMobEffect {
                        id,
                        effect,
                        amplifier,
                        duration,
                        flags,
                    })));
                }
            }
            let values: Vec<play::AttributeSnapshot> = to
                .attributes
                .iter()
                .filter(|(k, v)| self.attributes.get(k) != Some(v))
                .map(|(_, v)| v.clone())
                .collect();
            if !values.is_empty() {
                out.push(Packet::of(&C::UpdateAttributes(play::UpdateAttributes {
                    id,
                    values,
                })));
            }
        }
        for group in self
            .cooldowns
            .keys()
            .filter(|g| !to.cooldowns.contains_key(*g))
        {
            out.push(Packet::of(&C::Cooldown(play::Cooldown {
                group: group.clone(),
                duration: 0,
            })));
        }
        for (group, &duration) in &to.cooldowns {
            if self.cooldowns.get(group) != Some(&duration) {
                out.push(Packet::of(&C::Cooldown(play::Cooldown {
                    group: group.clone(),
                    duration,
                })));
            }
        }
        if let Some(items) = &to.inventory
            && (to.inventory != self.inventory
                || to.carried != self.carried
                || to.inventory_state != self.inventory_state)
        {
            out.push(Packet::of(&C::ContainerSetContent(
                play::ContainerSetContent {
                    container: 0,
                    state: to.inventory_state,
                    items: items.clone(),
                    carried: to.carried.clone(),
                },
            )));
        }
        if let Some(open) = &to.open {
            out.push(Packet::of(&C::OpenScreen(open.screen.clone())));
            out.push(Packet::of(&C::ContainerSetContent(
                play::ContainerSetContent {
                    container: open.screen.container,
                    state: open.state,
                    items: open.items.clone(),
                    carried: to.carried.clone(),
                },
            )));
        }
    }

    /// The packet that closes the container the client has open, which a switch
    /// does first (DESIGN.md section 4).
    pub fn close(&self) -> Option<Packet> {
        self.open.as_ref().map(|open| {
            Packet::of(&C::ContainerClose(play::ContainerClose {
                container: open.screen.container,
            }))
        })
    }
}

/// The inventory menu slot of an `Inventory` slot (`InventoryMenu`): the hotbar
/// 0-8 is 36-44, the storage 9-35 stays, the armour 36-39 (feet to head) is 8 to
/// 5, the off hand 40 is 45. Other slots are not in the menu.
fn menu_slot(slot: i32) -> Option<usize> {
    let menu = match slot {
        0..=8 => slot + 36,
        9..=35 => slot,
        36..=39 => 44 - slot,
        40 => 45,
        _ => return None,
    };
    usize::try_from(menu).ok().filter(|&m| m < INVENTORY_SLOTS)
}
