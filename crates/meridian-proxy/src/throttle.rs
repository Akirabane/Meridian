//! The limit of connections per address (`connections-per-minute`). It is
//! checked as soon as a connection is accepted, before a thread is started or a
//! byte is read, so that an address opening connections in a loop costs the proxy
//! as little as possible.
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

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
