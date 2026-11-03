//! Where to place a player among the servers of one role (docs/LOAD-BALANCING.md,
//! section 4): fill a server up to its soft cap, then open the next one. Everything
//! here is a pure function of the counts, the limits and what the caller knows of
//! each server, so it is tested without any connection, as `route.rs` is.
//!
//! The caller says which servers are candidates (the ones of a role that the
//! player's route allows, see `route::candidates`) and whether each is available.
//! Nothing here reads the configuration, the clock or the network.
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use meridian_config::TieBreak;

/// The number of players on each backend, by name. A backend with no player is
/// absent.
pub fn count_by_backend<'a>(backends: impl IntoIterator<Item = &'a str>) -> HashMap<String, usize> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for backend in backends {
        *counts.entry(backend.to_owned()).or_default() += 1;
    }
    counts
}

/// One server a player may be placed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Candidate<'a> {
    pub backend: &'a str,
    /// The players on it now.
    pub players: usize,
    /// The players it is filled to before the next server is opened.
    pub soft_cap: u32,
    /// The most it ever takes.
    pub hard_cap: u32,
    /// Whether it takes new players at all. The caller says `false` for a server that
    /// does not accept transfers, whose report is too old, or that just failed a
    /// login. Its players are not affected.
    pub available: bool,
    /// How long the proxy measured the server to take to answer, if it did.
    pub ping: Option<Duration>,
}

impl Candidate<'_> {
    fn has_room(&self) -> bool {
        self.available && self.players < self.hard_cap as usize
    }
}

/// Which servers are closed: they reached their soft cap and have not yet fallen to
/// `soft_cap - margin`. Kept between placements, so that a server at its cap does
/// not flip between open and closed with each player who leaves.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Gates {
    closed: HashSet<String>,
}

impl Gates {
    /// Whether `backend` is closed.
    pub fn is_closed(&self, backend: &str) -> bool {
        self.closed.contains(backend)
    }

    /// Takes the players of `candidate` into account: it closes at its soft cap and
    /// opens again at `soft_cap - margin` or below. Between the two it keeps the
    /// state it had. A margin of 0 opens it as soon as it is below its soft cap.
    pub fn update(&mut self, candidate: &Candidate<'_>, margin: u32) {
        let soft = candidate.soft_cap as usize;
        let reopen_at = soft.saturating_sub(margin as usize);
        if candidate.players >= soft {
            self.closed.insert(candidate.backend.to_owned());
        } else if candidate.players <= reopen_at {
            self.closed.remove(candidate.backend);
        }
    }

    /// Forgets a server that is no longer configured.
    pub fn retain(&mut self, mut keep: impl FnMut(&str) -> bool) {
        self.closed.retain(|b| keep(b));
    }
}

/// The server a player is placed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement<'a> {
    pub backend: &'a str,
    /// Whether every open server was full and this one is past its soft cap, taking
    /// the overflow.
    pub overflow: bool,
}

/// No server can take the player.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoRoom;

impl fmt::Display for NoRoom {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("no server of this role has room")
    }
}

impl std::error::Error for NoRoom {}

/// Where to place a player among `candidates`, listed in the order the backends are
/// listed:
///
/// 1. only servers that are available and below their hard cap count;
/// 2. among those that are open (below the soft cap, or fallen back to
///    `soft_cap - margin` since they closed), the fullest, so one server is filled
///    before the next is opened;
/// 3. if none is open, the emptiest of the closed ones: the overflow;
/// 4. a tie is broken by `tie_break`: the quickest server (one with no measure ranks
///    after one with, and two without are told apart by their order), or the order.
///
/// `gates` is updated for every candidate, available or not, so it follows the
/// counts even while a server is passed over.
pub fn place<'a>(
    candidates: &[Candidate<'a>],
    gates: &mut Gates,
    margin: u32,
    tie_break: TieBreak,
) -> Result<Placement<'a>, NoRoom> {
    for c in candidates {
        gates.update(c, margin);
    }
    let with_room = || candidates.iter().enumerate().filter(|(_, c)| c.has_room());
    let open = with_room().filter(|(_, c)| !gates.is_closed(c.backend));
    if let Some(best) = pick(open, tie_break, |c| std::cmp::Reverse(c.players)) {
        return Ok(Placement {
            backend: best.backend,
            overflow: false,
        });
    }
    let closed = with_room().filter(|(_, c)| gates.is_closed(c.backend));
    pick(closed, tie_break, |c| c.players)
        .map(|c| Placement {
            backend: c.backend,
            overflow: true,
        })
        .ok_or(NoRoom)
}

/// The best of `servers` by `rank` (lowest first), ties broken by `tie_break`.
fn pick<'a, 'b, R: Ord>(
    servers: impl Iterator<Item = (usize, &'b Candidate<'a>)>,
    tie_break: TieBreak,
    rank: impl Fn(&Candidate<'a>) -> R,
) -> Option<&'b Candidate<'a>>
where
    'a: 'b,
{
    servers
        .min_by_key(|(order, c)| {
            let ping = match tie_break {
                // A measured server comes before one that is not.
                TieBreak::Ping => Some(c.ping.map_or((1, Duration::ZERO), |p| (0, p))),
                TieBreak::Order => None,
            };
            (rank(c), ping, *order)
        })
        .map(|(_, c)| c)
}

/// Why a server named on purpose (a forced host, `/server <name>`) cannot take a
/// player.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The server does not take new players: it is draining, its report is too old, or
    /// it just failed a login.
    Unavailable,
    /// The server has its hard cap.
    AtHardCap,
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Refused::Unavailable => "the server is not taking players",
            Refused::AtHardCap => "the server is full",
        })
    }
}

impl std::error::Error for Refused {}

/// Whether a server named on purpose may take one more player. It may go past its soft
/// cap, never past its hard cap, and never while it is not available.
pub fn allow_named(candidate: &Candidate<'_>) -> Result<(), Refused> {
    if !candidate.available {
        Err(Refused::Unavailable)
    } else if candidate.players >= candidate.hard_cap as usize {
        Err(Refused::AtHardCap)
    } else {
        Ok(())
    }
}

/// [`place`] in the first of `tiers` that can take the player: for `/hub`, the servers
/// of role `lobby`, then those of role `sublobby`. Each tier's gates are updated, so a
/// tier is kept up to date even when an earlier one answered.
#[cfg_attr(not(test), allow(dead_code))]
pub fn place_first<'a>(
    tiers: &[&[Candidate<'a>]],
    gates: &mut Gates,
    margin: u32,
    tie_break: TieBreak,
) -> Result<Placement<'a>, NoRoom> {
    let mut found = Err(NoRoom);
    for tier in tiers {
        let placed = place(tier, gates, margin, tie_break);
        if found.is_err() {
            found = placed;
        }
    }
    found
}

/// A server as the caller knows it, without its players, which the [`Balancer`]
/// counts itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Server<'a> {
    pub backend: &'a str,
    pub soft_cap: u32,
    pub hard_cap: u32,
    /// As in [`Candidate::available`].
    pub available: bool,
    /// As in [`Candidate::ping`].
    pub ping: Option<Duration>,
    /// The players the backend reports it has. The proxy counts the players it sent
    /// itself, and a backend may have others, so the larger of the two is used.
    pub reported_players: Option<usize>,
}

impl<'a> Server<'a> {
    fn with_players(self, counted: usize) -> Candidate<'a> {
        let players = counted.max(self.reported_players.unwrap_or(0));
        Candidate {
            backend: self.backend,
            players,
            soft_cap: self.soft_cap,
            hard_cap: self.hard_cap,
            available: self.available,
            ping: self.ping,
        }
    }
}

/// The state placement keeps between logins: which servers are closed, and the
/// places taken by players who are still being sent to their server.
///
/// A player counts from the moment a place is reserved for them until their
/// session is open, so that logins at the same time cannot all see the same free
/// place. The count and the reservation are one step under one lock.
#[derive(Default)]
pub struct Balancer {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    gates: Gates,
    reserved: HashMap<String, usize>,
}

/// A place taken on a server for a player who is being sent to it, given back when
/// it is dropped. It is dropped once the player's session is open (then the
/// session counts) or when the login fails.
pub struct Reservation<'a> {
    balancer: &'a Balancer,
    backend: String,
    /// Whether the server was past its soft cap: the player took the overflow.
    pub overflow: bool,
}

impl Reservation<'_> {
    /// The server the place is on.
    pub fn backend(&self) -> &str {
        &self.backend
    }
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        let mut state = self.balancer.lock();
        if let Some(n) = state.reserved.get_mut(&self.backend) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                state.reserved.remove(&self.backend);
            }
        }
    }
}

impl Balancer {
    /// Places a player among `servers` as [`place`] does and reserves their place.
    /// `counts` gives the players on each backend and is called with the lock held,
    /// so that no session can be counted both before and after a reservation drops.
    pub fn reserve(
        &self,
        servers: &[Server<'_>],
        counts: impl FnOnce() -> HashMap<String, usize>,
        margin: u32,
        tie_break: TieBreak,
    ) -> Result<Reservation<'_>, NoRoom> {
        let mut state = self.lock();
        let counts = counts();
        let candidates = candidates(servers, &counts, &state.reserved);
        let State { gates, reserved } = &mut *state;
        let placed = place(&candidates, gates, margin, tie_break)?;
        let (backend, overflow) = (placed.backend.to_owned(), placed.overflow);
        *reserved.entry(backend.clone()).or_default() += 1;
        Ok(Reservation {
            balancer: self,
            backend,
            overflow,
        })
    }

    /// Reserves a place on `server`, named on purpose: it may be past its soft cap,
    /// never past its hard cap ([`allow_named`]).
    pub fn reserve_named(
        &self,
        server: Server<'_>,
        counts: impl FnOnce() -> HashMap<String, usize>,
    ) -> Result<Reservation<'_>, Refused> {
        let mut state = self.lock();
        let counts = counts();
        let candidate = candidates(&[server], &counts, &state.reserved).remove(0);
        allow_named(&candidate)?;
        *state.reserved.entry(server.backend.to_owned()).or_default() += 1;
        Ok(Reservation {
            balancer: self,
            backend: server.backend.to_owned(),
            overflow: candidate.players >= candidate.soft_cap as usize,
        })
    }

    /// Forgets the servers `keep` refuses, such as one a reload removed. Places
    /// already reserved on them are kept until they are given back.
    pub fn retain(&self, mut keep: impl FnMut(&str) -> bool) {
        self.lock().gates.retain(&mut keep);
    }

    /// Whether `backend` is closed.
    #[cfg(test)]
    pub fn is_closed(&self, backend: &str) -> bool {
        self.lock().gates.is_closed(backend)
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// `servers` with the players on each: the sessions counted plus the places
/// reserved.
fn candidates<'a>(
    servers: &[Server<'a>],
    counts: &HashMap<String, usize>,
    reserved: &HashMap<String, usize>,
) -> Vec<Candidate<'a>> {
    servers
        .iter()
        .map(|s| {
            let players = counts.get(s.backend).copied().unwrap_or(0)
                + reserved.get(s.backend).copied().unwrap_or(0);
            s.with_players(players)
        })
        .collect()
}

#[cfg(test)]
mod tests;
