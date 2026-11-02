//! The world values the client holds: time, weather, border, difficulty, default
//! spawn, tick rate and simulation distance.
use crate::Packet;
use horizon_protocol::packets::play::{self, Clientbound as C};
use std::collections::BTreeMap;

/// `ClientboundGameEventPacket` weather events.
const START_RAINING: u8 = 1;
const STOP_RAINING: u8 = 2;
const RAIN_LEVEL: u8 = 7;
const THUNDER_LEVEL: u8 = 8;

/// The world border as Initialize World Border describes it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Border {
    pub center: (f64, f64),
    pub old_size: f64,
    pub new_size: f64,
    /// The time left in the size change when it was last sent.
    pub lerp_time: i64,
    pub absolute_max_size: i32,
    pub warning_blocks: i32,
    pub warning_time: i32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct World {
    pub game_time: Option<i64>,
    /// The world clocks by registry id, as last sent.
    pub clocks: BTreeMap<i32, play::ClockState>,
    pub raining: bool,
    pub rain_level: f32,
    pub thunder_level: f32,
    pub border: Option<Border>,
    /// Change Difficulty: the difficulty and whether it is locked.
    pub difficulty: Option<(i32, bool)>,
    pub spawn: Option<play::SetDefaultSpawnPosition>,
    /// Ticking State: the tick rate and whether the level is frozen.
    pub ticking: Option<(f32, bool)>,
    pub ticking_steps: Option<i32>,
    pub simulation_distance: Option<i32>,
}

impl World {
    pub fn apply(&mut self, packet: &C) {
        match packet {
            C::SetTime(p) => {
                self.game_time = Some(p.game_time);
                for clock in &p.clocks {
                    self.clocks.insert(clock.clock, clock.clone());
                }
            }
            // `ClientPacketListener.handleGameEvent`: rain starts from nothing
            // and stops from full, and fades from there.
            C::GameEvent(p) => match p.event {
                START_RAINING => {
                    self.raining = true;
                    self.rain_level = 0.0;
                }
                STOP_RAINING => {
                    self.raining = false;
                    self.rain_level = 1.0;
                }
                RAIN_LEVEL => self.rain_level = p.param,
                THUNDER_LEVEL => self.thunder_level = p.param,
                _ => {}
            },
            C::InitializeBorder(p) => {
                self.border = Some(Border {
                    center: (p.center_x, p.center_z),
                    old_size: p.old_size,
                    new_size: p.new_size,
                    lerp_time: p.lerp_time,
                    absolute_max_size: p.absolute_max_size,
                    warning_blocks: p.warning_blocks,
                    warning_time: p.warning_time,
                });
            }
            C::SetBorderCenter(p) => {
                if let Some(b) = self.border.as_mut() {
                    b.center = (p.x, p.z);
                }
            }
            C::SetBorderLerpSize(p) => {
                if let Some(b) = self.border.as_mut() {
                    (b.old_size, b.new_size, b.lerp_time) = (p.old_size, p.new_size, p.lerp_time);
                }
            }
            C::SetBorderSize(p) => {
                if let Some(b) = self.border.as_mut() {
                    (b.old_size, b.new_size, b.lerp_time) = (p.size, p.size, 0);
                }
            }
            C::SetBorderWarningDelay(p) => {
                if let Some(b) = self.border.as_mut() {
                    b.warning_time = p.delay;
                }
            }
            C::SetBorderWarningDistance(p) => {
                if let Some(b) = self.border.as_mut() {
                    b.warning_blocks = p.blocks;
                }
            }
            C::ChangeDifficulty(p) => self.difficulty = Some((p.difficulty, p.locked)),
            C::SetDefaultSpawnPosition(p) => self.spawn = Some(p.clone()),
            C::TickingState(p) => self.ticking = Some((p.rate, p.frozen)),
            C::TickingStep(p) => self.ticking_steps = Some(p.steps),
            C::SetSimulationDistance(p) => self.simulation_distance = Some(p.distance),
            _ => {}
        }
    }

    /// The packets that bring the world values from `self` to `to`, one per
    /// difference.
    pub fn delta(&self, to: &World, out: &mut Vec<Packet>) {
        if let Some((difficulty, locked)) =
            to.difficulty.filter(|_| to.difficulty != self.difficulty)
        {
            out.push(Packet::of(&C::ChangeDifficulty(play::ChangeDifficulty {
                difficulty,
                locked,
            })));
        }
        if let Some(distance) = to
            .simulation_distance
            .filter(|_| to.simulation_distance != self.simulation_distance)
        {
            out.push(Packet::of(&C::SetSimulationDistance(play::Distance {
                distance,
            })));
        }
        if let Some(b) = to.border.as_ref().filter(|_| to.border != self.border) {
            out.push(Packet::of(&C::InitializeBorder(play::InitializeBorder {
                center_x: b.center.0,
                center_z: b.center.1,
                old_size: b.old_size,
                new_size: b.new_size,
                lerp_time: b.lerp_time,
                absolute_max_size: b.absolute_max_size,
                warning_blocks: b.warning_blocks,
                warning_time: b.warning_time,
            })));
        }
        if let Some(game_time) = to.game_time
            && (to.game_time != self.game_time || to.clocks != self.clocks)
        {
            out.push(Packet::of(&C::SetTime(play::SetTime {
                game_time,
                clocks: to.clocks.values().cloned().collect(),
            })));
        }
        // The weather: the rain's start or stop sets its level, then the levels.
        let mut rain_level = self.rain_level;
        if to.raining != self.raining {
            let event = if to.raining {
                START_RAINING
            } else {
                STOP_RAINING
            };
            out.push(game_event(event, 0.0));
            rain_level = if to.raining { 0.0 } else { 1.0 };
        }
        if to.rain_level != rain_level {
            out.push(game_event(RAIN_LEVEL, to.rain_level));
        }
        if to.thunder_level != self.thunder_level {
            out.push(game_event(THUNDER_LEVEL, to.thunder_level));
        }
        if let Some(spawn) = to.spawn.as_ref().filter(|_| to.spawn != self.spawn) {
            out.push(Packet::of(&C::SetDefaultSpawnPosition(spawn.clone())));
        }
        if let Some((rate, frozen)) = to.ticking.filter(|_| to.ticking != self.ticking) {
            out.push(Packet::of(&C::TickingState(play::TickingState {
                rate,
                frozen,
            })));
        }
        if let Some(steps) = to
            .ticking_steps
            .filter(|_| to.ticking_steps != self.ticking_steps)
        {
            out.push(Packet::of(&C::TickingStep(play::TickingStep { steps })));
        }
    }
}

fn game_event(event: u8, param: f32) -> Packet {
    Packet::of(&C::GameEvent(play::GameEvent { event, param }))
}
