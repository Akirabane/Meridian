//! The player list (tab list): its entries by profile UUID, and its header and
//! footer.
use crate::Packet;
use horizon_protocol::packets::play::{self, Clientbound as C};
use std::collections::BTreeMap;

/// `ClientboundPlayerInfoUpdatePacket.Action`, as bits.
const ADD_PLAYER: u8 = 1;
const INITIALIZE_CHAT: u8 = 2;
const UPDATE_GAME_MODE: u8 = 4;
const UPDATE_LISTED: u8 = 8;
const UPDATE_LATENCY: u8 = 16;
const UPDATE_DISPLAY_NAME: u8 = 32;
const UPDATE_LIST_ORDER: u8 = 64;
const UPDATE_HAT: u8 = 128;

/// One entry: what the packets set, and which actions set it. An action never
/// sent and an action that cleared a value (a chat session, a display name) are
/// told apart by `seen`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Entry {
    pub info: play::PlayerInfo,
    pub seen: u8,
}

impl Entry {
    /// Whether this action's value is the same in both entries.
    fn same(&self, other: &Entry, action: u8) -> bool {
        let (a, b) = (&self.info, &other.info);
        match action {
            INITIALIZE_CHAT => a.chat_session == b.chat_session,
            UPDATE_GAME_MODE => a.game_mode == b.game_mode,
            UPDATE_LISTED => a.listed == b.listed,
            UPDATE_LATENCY => a.latency == b.latency,
            UPDATE_DISPLAY_NAME => a.display_name == b.display_name,
            UPDATE_LIST_ORDER => a.list_order == b.list_order,
            UPDATE_HAT => a.show_hat == b.show_hat,
            _ => true,
        }
    }

    /// Whether packets can turn this entry into `other`: the same profile, and
    /// every action this one has seen also set on the other (a value can be
    /// changed, not returned to never having been sent).
    fn updatable_to(&self, other: &Entry) -> bool {
        self.info.name == other.info.name
            && self.info.properties == other.info.properties
            && self.seen & !other.seen & !ADD_PLAYER == 0
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Tab {
    pub entries: BTreeMap<u128, Entry>,
    pub header_footer: Option<play::TabList>,
}

impl Tab {
    pub fn apply(&mut self, packet: &C) {
        match packet {
            // `ClientPacketListener.handlePlayerInfoUpdate`: Add Player makes the
            // entry, the other actions change one that exists.
            C::PlayerInfoUpdate(p) => {
                for info in &p.entries {
                    if p.actions & ADD_PLAYER != 0 {
                        self.entries.insert(
                            info.uuid,
                            Entry {
                                info: play::PlayerInfo {
                                    uuid: info.uuid,
                                    name: info.name.clone(),
                                    properties: info.properties.clone(),
                                    ..play::PlayerInfo::default()
                                },
                                seen: ADD_PLAYER,
                            },
                        );
                    }
                    if let Some(entry) = self.entries.get_mut(&info.uuid) {
                        set(entry, info, p.actions);
                    }
                }
            }
            C::PlayerInfoRemove(p) => {
                for uuid in &p.uuids {
                    self.entries.remove(uuid);
                }
            }
            C::TabList(p) => self.header_footer = Some(p.clone()),
            _ => {}
        }
    }

    /// Entries the target lacks, or holds with another profile or with fewer
    /// actions seen, go first; they come back with the others.
    pub fn to_remove(&self, to: &Tab) -> Vec<u128> {
        self.entries
            .iter()
            .filter(|(uuid, e)| to.entries.get(uuid).is_none_or(|t| !e.updatable_to(t)))
            .map(|(&uuid, _)| uuid)
            .collect()
    }

    /// The packets that turn the list into the target's, once `removed` are gone:
    /// an added entry carries every action the target saw, a kept one only those
    /// whose value differs.
    pub fn delta(&self, to: &Tab, removed: &[u128], out: &mut Vec<Packet>) {
        if !removed.is_empty() {
            out.push(Packet::of(&C::PlayerInfoRemove(play::PlayerInfoRemove {
                uuids: removed.to_vec(),
            })));
        }
        for (uuid, target) in &to.entries {
            let kept = self.entries.get(uuid).filter(|_| !removed.contains(uuid));
            let actions = match kept {
                None => target.seen | ADD_PLAYER,
                Some(current) => {
                    let mut actions = 0;
                    for bit in (1..8).map(|i| 1u8 << i) {
                        if target.seen & bit != 0
                            && (current.seen & bit == 0 || !current.same(target, bit))
                        {
                            actions |= bit;
                        }
                    }
                    actions
                }
            };
            if actions != 0 {
                out.push(Packet::of(&C::PlayerInfoUpdate(play::PlayerInfoUpdate {
                    actions,
                    entries: vec![target.info.clone()],
                })));
            }
        }
        if let Some(lines) = to
            .header_footer
            .as_ref()
            .filter(|_| to.header_footer != self.header_footer)
        {
            out.push(Packet::of(&C::TabList(lines.clone())));
        }
    }
}

/// The values of `actions` from `info` into `entry`.
fn set(entry: &mut Entry, info: &play::PlayerInfo, actions: u8) {
    let e = &mut entry.info;
    if actions & INITIALIZE_CHAT != 0 {
        e.chat_session.clone_from(&info.chat_session);
    }
    if actions & UPDATE_GAME_MODE != 0 {
        e.game_mode = info.game_mode;
    }
    if actions & UPDATE_LISTED != 0 {
        e.listed = info.listed;
    }
    if actions & UPDATE_LATENCY != 0 {
        e.latency = info.latency;
    }
    if actions & UPDATE_DISPLAY_NAME != 0 {
        e.display_name.clone_from(&info.display_name);
    }
    if actions & UPDATE_LIST_ORDER != 0 {
        e.list_order = info.list_order;
    }
    if actions & UPDATE_HAT != 0 {
        e.show_hat = info.show_hat;
    }
    entry.seen |= actions;
}
