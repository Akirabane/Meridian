//! Entities the client shows. The model and its delta come with the next change;
//! until then a view holds none.
use crate::Packet;
use horizon_protocol::packets::play::Clientbound as C;
use std::collections::BTreeMap;

/// The entities the client shows, by the id the server gave them.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Entities {
    pub by_id: BTreeMap<i32, ()>,
}

impl Entities {
    pub fn apply(&mut self, _packet: &C) {}

    pub fn to_remove(&self, _to: &Entities) -> Vec<i32> {
        Vec::new()
    }

    pub fn delta(&self, _to: &Entities, _removed: &[i32], _out: &mut Vec<Packet>) {}
}
