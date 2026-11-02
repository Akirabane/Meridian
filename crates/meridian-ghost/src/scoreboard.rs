//! The scoreboard as the client holds it: objectives, scores, display slots and
//! teams (`ClientPacketListener` and `Scoreboard`).
use crate::Packet;
use horizon_protocol::packets::play::{self, Clientbound as C};
use std::collections::{BTreeMap, BTreeSet};

/// `ClientboundSetObjectivePacket` methods.
const OBJECTIVE_ADD: i8 = 0;
const OBJECTIVE_REMOVE: i8 = 1;
const OBJECTIVE_CHANGE: i8 = 2;

/// `ClientboundSetPlayerTeamPacket` methods.
const TEAM_ADD: i8 = 0;
const TEAM_REMOVE: i8 = 1;
const TEAM_CHANGE: i8 = 2;
const TEAM_JOIN: i8 = 3;
const TEAM_LEAVE: i8 = 4;

/// A team: its parameters and its members' names.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Team {
    pub parameters: Option<play::TeamParameters>,
    pub players: BTreeSet<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Scoreboard {
    /// Objectives by name, each as the Set Objective that adds it now.
    pub objectives: BTreeMap<String, play::SetObjective>,
    /// Scores by owner and objective, each as its last Set Score.
    pub scores: BTreeMap<(String, String), play::SetScore>,
    /// Display slots and the objective each shows.
    pub display: BTreeMap<i32, String>,
    pub teams: BTreeMap<String, Team>,
}

impl Scoreboard {
    pub fn apply(&mut self, packet: &C) {
        match packet {
            C::SetObjective(p) => match p.method {
                OBJECTIVE_ADD => {
                    self.objectives.insert(p.name.clone(), adding(p));
                }
                OBJECTIVE_REMOVE => self.remove_objective(&p.name),
                OBJECTIVE_CHANGE => {
                    if let Some(o) = self.objectives.get_mut(&p.name) {
                        o.display.clone_from(&p.display);
                    }
                }
                _ => {}
            },
            // A score for an objective the client lacks is ignored.
            C::SetScore(p) => {
                if self.objectives.contains_key(&p.objective) {
                    self.scores
                        .insert((p.owner.clone(), p.objective.clone()), p.clone());
                }
            }
            C::ResetScore(p) => match &p.objective {
                Some(objective) => {
                    self.scores.remove(&(p.owner.clone(), objective.clone()));
                }
                None => self.scores.retain(|(owner, _), _| *owner != p.owner),
            },
            C::SetDisplayObjective(p) => {
                if p.objective.is_empty() || !self.objectives.contains_key(&p.objective) {
                    self.display.remove(&p.slot);
                } else {
                    self.display.insert(p.slot, p.objective.clone());
                }
            }
            C::SetPlayerTeam(p) => match p.method {
                TEAM_ADD => {
                    let players = p.players.iter().cloned().collect();
                    self.teams.insert(
                        p.name.clone(),
                        Team {
                            parameters: p.parameters.clone(),
                            players: BTreeSet::new(),
                        },
                    );
                    self.join(&p.name, &players);
                }
                TEAM_REMOVE => {
                    self.teams.remove(&p.name);
                }
                TEAM_CHANGE => {
                    if let Some(t) = self.teams.get_mut(&p.name) {
                        t.parameters.clone_from(&p.parameters);
                    }
                }
                TEAM_JOIN => {
                    let players = p.players.iter().cloned().collect();
                    self.join(&p.name, &players);
                }
                TEAM_LEAVE => {
                    if let Some(t) = self.teams.get_mut(&p.name) {
                        for name in &p.players {
                            t.players.remove(name);
                        }
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    /// `Scoreboard.removeObjective`: its scores and the slots showing it go too.
    fn remove_objective(&mut self, name: &str) {
        self.objectives.remove(name);
        self.scores.retain(|(_, objective), _| objective != name);
        self.display.retain(|_, shown| shown != name);
    }

    /// `Scoreboard.addPlayerToTeam`: a player is in one team at most, so joining
    /// one leaves the other.
    fn join(&mut self, team: &str, players: &BTreeSet<String>) {
        if !self.teams.contains_key(team) {
            return;
        }
        for (name, t) in &mut self.teams {
            if name != team {
                t.players.retain(|p| !players.contains(p));
            }
        }
        if let Some(t) = self.teams.get_mut(team) {
            t.players.extend(players.iter().cloned());
        }
    }

    /// The packets that turn the scoreboard into the target's: removals first,
    /// then objectives, scores, display slots and teams.
    pub fn delta(&self, to: &Scoreboard, out: &mut Vec<Packet>) {
        // Removing an objective also removes its scores and its display slots on
        // the client; what is left is compared below.
        let mut now = self.clone();
        for name in self
            .objectives
            .keys()
            .filter(|n| !to.objectives.contains_key(*n))
        {
            out.push(Packet::of(&C::SetObjective(play::SetObjective {
                name: name.clone(),
                method: OBJECTIVE_REMOVE,
                display: None,
            })));
            now.remove_objective(name);
        }
        for (name, target) in &to.objectives {
            match now.objectives.get(name) {
                None => out.push(Packet::of(&C::SetObjective(target.clone()))),
                Some(current) if current.display != target.display => {
                    out.push(Packet::of(&C::SetObjective(play::SetObjective {
                        method: OBJECTIVE_CHANGE,
                        ..target.clone()
                    })));
                }
                Some(_) => {}
            }
        }
        for (owner, objective) in now.scores.keys().filter(|k| !to.scores.contains_key(*k)) {
            out.push(Packet::of(&C::ResetScore(play::ResetScore {
                owner: owner.clone(),
                objective: Some(objective.clone()),
            })));
        }
        for (key, score) in &to.scores {
            if now.scores.get(key) != Some(score) {
                out.push(Packet::of(&C::SetScore(score.clone())));
            }
        }
        for &slot in now.display.keys().filter(|s| !to.display.contains_key(*s)) {
            out.push(Packet::of(&C::SetDisplayObjective(
                play::SetDisplayObjective {
                    slot,
                    objective: String::new(),
                },
            )));
        }
        for (&slot, objective) in &to.display {
            if now.display.get(&slot) != Some(objective) {
                out.push(Packet::of(&C::SetDisplayObjective(
                    play::SetDisplayObjective {
                        slot,
                        objective: objective.clone(),
                    },
                )));
            }
        }
        self.teams_delta(to, out);
    }

    /// Teams: those the target lacks are removed and the members who leave go,
    /// before teams are made and members join, since joining a team leaves the
    /// one a player was in.
    fn teams_delta(&self, to: &Scoreboard, out: &mut Vec<Packet>) {
        let team = |name: &str, method: i8, parameters, players: Vec<String>| {
            Packet::of(&C::SetPlayerTeam(play::SetPlayerTeam {
                name: name.to_owned(),
                method,
                parameters,
                players,
            }))
        };
        for name in self.teams.keys().filter(|n| !to.teams.contains_key(*n)) {
            out.push(team(name, TEAM_REMOVE, None, Vec::new()));
        }
        for (name, target) in &to.teams {
            if let Some(current) = self.teams.get(name) {
                let leaving: Vec<String> = current
                    .players
                    .difference(&target.players)
                    .cloned()
                    .collect();
                if !leaving.is_empty() {
                    out.push(team(name, TEAM_LEAVE, None, leaving));
                }
            }
        }
        for (name, target) in &to.teams {
            match self.teams.get(name) {
                None => out.push(team(
                    name,
                    TEAM_ADD,
                    target.parameters.clone(),
                    target.players.iter().cloned().collect(),
                )),
                Some(current) => {
                    if current.parameters != target.parameters {
                        out.push(team(
                            name,
                            TEAM_CHANGE,
                            target.parameters.clone(),
                            Vec::new(),
                        ));
                    }
                    let joining: Vec<String> = target
                        .players
                        .difference(&current.players)
                        .cloned()
                        .collect();
                    if !joining.is_empty() {
                        out.push(team(name, TEAM_JOIN, None, joining));
                    }
                }
            }
        }
    }
}

/// An objective as the Set Objective that adds it.
fn adding(p: &play::SetObjective) -> play::SetObjective {
    play::SetObjective {
        method: OBJECTIVE_ADD,
        ..p.clone()
    }
}
