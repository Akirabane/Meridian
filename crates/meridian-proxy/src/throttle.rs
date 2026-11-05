//! The limit of connections per address (`connections-per-minute`). It is
//! checked as soon as a connection is accepted, before a thread is started or a
//! byte is read, so that an address opening connections in a loop costs the proxy
//! as little as possible.
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// The window the limit counts in.
const WINDOW: Duration = Duration::from_secs(60);
/// Above this many addresses, the ones whose window is over are swept out.
const SWEEP_ABOVE: usize = 10_000;

/// The connections each address opened in its current minute.
#[derive(Default)]
pub struct Throttle {
    windows: Mutex<HashMap<IpAddr, (Instant, u32)>>,
}

impl Throttle {
    /// Whether `address` may open one more connection at `now`, counting it if
    /// so. `per_minute` 0 lets every connection through.
    pub fn allow(&self, address: IpAddr, per_minute: u32, now: Instant) -> bool {
        if per_minute == 0 {
            return true;
        }
        let mut windows = self.windows.lock().unwrap_or_else(|e| e.into_inner());
        if windows.len() >= SWEEP_ABOVE {
            windows.retain(|_, &mut (start, _)| now.saturating_duration_since(start) < WINDOW);
        }
        let (start, count) = windows.entry(address).or_insert((now, 0));
        if now.saturating_duration_since(*start) >= WINDOW {
            *start = now;
            *count = 0;
        }
        if *count >= per_minute {
            return false;
        }
        *count += 1;
        true
    }

    /// How many addresses are remembered.
    #[cfg(test)]
    fn remembered(&self) -> usize {
        self.windows.lock().unwrap().len()
    }
}

/// The connections that are logging in (`max-pending-logins`,
/// `max-pending-logins-per-address`). A connection holds a place from its accept to the
/// end of its login, whichever way the login ends, and the place is given back when its
/// [`Pending`] is dropped. Each address is counted whether its own limit is on or not, so
/// that turning it on at a reload counts the connections already there.
#[derive(Default)]
pub struct Logins {
    state: Mutex<Counts>,
}

#[derive(Default)]
struct Counts {
    total: usize,
    by_address: HashMap<IpAddr, u32>,
}

/// One connection's place among those logging in.
pub struct Pending {
    logins: Arc<Logins>,
    address: IpAddr,
}

impl Logins {
    /// A place for a connection from `address` that starts logging in, unless `max`
    /// connections are logging in already, or `max_per_address` from that address. A
    /// limit of 0 is none.
    pub fn enter(
        self: &Arc<Self>,
        address: IpAddr,
        max: u32,
        max_per_address: u32,
    ) -> Option<Pending> {
        let mut counts = self.lock();
        let from_address = counts.by_address.get(&address).copied().unwrap_or(0);
        if (max > 0 && counts.total >= max as usize)
            || (max_per_address > 0 && from_address >= max_per_address)
        {
            return None;
        }
        counts.total += 1;
        *counts.by_address.entry(address).or_insert(0) += 1;
        Some(Pending {
            logins: Arc::clone(self),
            address,
        })
    }

    /// How many connections are logging in now.
    #[cfg(test)]
    pub(crate) fn count(&self) -> usize {
        self.lock().total
    }

    fn lock(&self) -> MutexGuard<'_, Counts> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        let mut counts = self.logins.lock();
        counts.total = counts.total.saturating_sub(1);
        if let Some(n) = counts.by_address.get_mut(&self.address) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                counts.by_address.remove(&self.address);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn the_connections_logging_in_are_held_to_their_limits() {
        let logins = Arc::new(Logins::default());
        // Without a limit for an address, one address may take every place.
        let a: Vec<_> = (0..3).map(|_| logins.enter(A, 3, 0)).collect();
        assert!(a.iter().all(Option::is_some));
        assert!(logins.enter(B, 3, 0).is_none(), "every place is taken");
        drop(a);
        assert_eq!(logins.count(), 0, "the places are given back");
        // With one, an address is stopped at it and the others are not.
        let first = logins.enter(A, 3, 1);
        assert!(first.is_some());
        assert!(logins.enter(A, 3, 1).is_none());
        let other = logins.enter(B, 3, 1);
        assert!(other.is_some());
        drop(first);
        assert!(
            logins.enter(A, 3, 1).is_some(),
            "a place given back is taken again"
        );
        drop(other);
        // 0 is no limit.
        let many: Vec<_> = (0..100).map(|_| logins.enter(A, 0, 0)).collect();
        assert!(many.iter().all(Option::is_some));
        drop(many);
        assert_eq!(logins.count(), 0);
        assert!(logins.lock().by_address.is_empty(), "no address is kept");
    }

    const A: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1));
    const B: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 2));

    fn at(start: Instant, secs: u64) -> Instant {
        start + Duration::from_secs(secs)
    }

    #[test]
    fn an_address_is_stopped_at_its_limit_for_the_minute() {
        let throttle = Throttle::default();
        let start = Instant::now();
        for i in 0..3 {
            assert!(throttle.allow(A, 3, at(start, i)), "connection {i}");
        }
        assert!(!throttle.allow(A, 3, at(start, 30)));
        assert!(!throttle.allow(A, 3, at(start, 59)));
        // A new minute, a new count.
        assert!(throttle.allow(A, 3, at(start, 60)));
    }

    #[test]
    fn each_address_counts_on_its_own() {
        let throttle = Throttle::default();
        let now = Instant::now();
        assert!(throttle.allow(A, 1, now));
        assert!(!throttle.allow(A, 1, now));
        assert!(throttle.allow(B, 1, now));
    }

    #[test]
    fn zero_means_no_limit_and_remembers_nothing() {
        let throttle = Throttle::default();
        let now = Instant::now();
        for _ in 0..1000 {
            assert!(throttle.allow(A, 0, now));
        }
        assert_eq!(throttle.remembered(), 0);
    }

    #[test]
    fn a_refused_connection_does_not_lengthen_the_wait() {
        // The window starts at the first connection, not at the last refusal.
        let throttle = Throttle::default();
        let start = Instant::now();
        assert!(throttle.allow(A, 1, start));
        for secs in [10, 20, 50] {
            assert!(!throttle.allow(A, 1, at(start, secs)));
        }
        assert!(throttle.allow(A, 1, at(start, 60)));
    }
}
