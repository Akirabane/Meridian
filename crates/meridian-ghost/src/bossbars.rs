//! Boss bars by UUID, each as the Add operation that would make it now.
use crate::Packet;
use horizon_protocol::packets::play::{self, BossAction, Clientbound as C};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct BossBars {
    /// Every bar as a `BossAction::Add` with its current values.
    pub bars: BTreeMap<u128, BossAction>,
}

impl BossBars {
    pub fn apply(&mut self, packet: &C) {
        let C::BossEvent(p) = packet else {
            return;
        };
        match &p.action {
            BossAction::Add { .. } => {
                self.bars.insert(p.id, p.action.clone());
            }
            BossAction::Remove => {
                self.bars.remove(&p.id);
            }
            change => {
                if let Some(BossAction::Add {
                    name,
                    progress,
                    color,
                    overlay,
                    darken,
                    music,
                    fog,
                }) = self.bars.get_mut(&p.id)
                {
                    match change {
                        BossAction::UpdateProgress { progress: v } => *progress = *v,
                        BossAction::UpdateName { name: v } => name.clone_from(v),
                        BossAction::UpdateStyle {
                            color: c,
                            overlay: o,
                        } => (*color, *overlay) = (*c, *o),
                        BossAction::UpdateProperties {
                            darken: d,
                            music: m,
                            fog: f,
                        } => (*darken, *music, *fog) = (*d, *m, *f),
                        _ => {}
                    }
                }
            }
        }
    }

    /// Bars the target lacks are removed, new ones added, and a bar on both
    /// sides gets one update per part that differs.
    pub fn delta(&self, to: &BossBars, out: &mut Vec<Packet>) {
        let event = |id: u128, action: BossAction| {
            Packet::of(&C::BossEvent(play::BossEvent { id, action }))
        };
        for &id in self.bars.keys().filter(|id| !to.bars.contains_key(id)) {
            out.push(event(id, BossAction::Remove));
        }
        for (&id, target) in &to.bars {
            let Some(current) = self.bars.get(&id) else {
                out.push(event(id, target.clone()));
                continue;
            };
            let (
                BossAction::Add {
                    name: n1,
                    progress: p1,
                    color: c1,
                    overlay: o1,
                    darken: d1,
                    music: m1,
                    fog: f1,
                },
                BossAction::Add {
                    name: n2,
                    progress: p2,
                    color: c2,
                    overlay: o2,
                    darken: d2,
                    music: m2,
                    fog: f2,
                },
            ) = (current, target)
            else {
                continue;
            };
            if p1 != p2 {
                out.push(event(id, BossAction::UpdateProgress { progress: *p2 }));
            }
            if n1 != n2 {
                out.push(event(id, BossAction::UpdateName { name: n2.clone() }));
            }
            if (c1, o1) != (c2, o2) {
                out.push(event(
                    id,
                    BossAction::UpdateStyle {
                        color: *c2,
                        overlay: *o2,
                    },
                ));
            }
            if (d1, m1, f1) != (d2, m2, f2) {
                out.push(event(
                    id,
                    BossAction::UpdateProperties {
                        darken: *d2,
                        music: *m2,
                        fog: *f2,
                    },
                ));
            }
        }
    }
}
