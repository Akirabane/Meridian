//! Which backends recently failed to take a player (`backend-retry-seconds`).
//! A backend that just failed is passed over for a while: the players who come
//! next try the other candidates of their list first, instead of each waiting for
//! the connect timeout of a backend that is down. It is still tried last, so a
//! player is never refused only because of an old failure.
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// When each backend last failed, by name.
#[derive(Default)]
pub struct Health {
    failed: Mutex<HashMap<String, Instant>>,
}

impl Health {
    /// A player could not join `backend` at `now`.
    pub fn failed(&self, backend: &str, now: Instant) {
        self.lock().insert(backend.to_owned(), now);
    }

    /// A player joined `backend`: it is no longer passed over.
    pub fn answered(&self, backend: &str) {
        self.lock().remove(backend);
    }

    /// How much longer `backend` is passed over at `now`, if it is.
    pub fn passed_over_for(
        &self,
        backend: &str,
        retry: Duration,
        now: Instant,
    ) -> Option<Duration> {
        let failed = *self.lock().get(backend)?;
        retry
            .checked_sub(now.saturating_duration_since(failed))
            .filter(|left| !left.is_zero())
    }

    /// `candidates` with the backends passed over moved to the end, each group
    /// keeping its order.
    pub fn order(&self, candidates: Vec<String>, retry: Duration, now: Instant) -> Vec<String> {
        let (down, up): (Vec<String>, Vec<String>) = candidates
            .into_iter()
            .partition(|c| self.passed_over_for(c, retry, now).is_some());
        up.into_iter().chain(down).collect()
    }

    /// A line for the console's list of backends: `None` for a backend that is
    /// not passed over.
    pub fn describe(&self, backend: &str, retry: Duration, now: Instant) -> Option<String> {
        let left = self.passed_over_for(backend, retry, now)?;
        Some(format!(
            "passed over for {} more s after a failed login",
            left.as_secs().max(1)
        ))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Instant>> {
        self.failed.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RETRY: Duration = Duration::from_secs(10);

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    fn at(start: Instant, secs: u64) -> Instant {
        start + Duration::from_secs(secs)
    }

    #[test]
    fn a_backend_that_failed_goes_last_until_the_retry_time_is_over() {
        let health = Health::default();
        let start = Instant::now();
        health.failed("lobby", start);
        let list = names(&["lobby", "east", "west"]);
        assert_eq!(
            health.order(list.clone(), RETRY, at(start, 0)),
            ["east", "west", "lobby"]
        );
        assert_eq!(
            health.order(list.clone(), RETRY, at(start, 9)),
            ["east", "west", "lobby"]
        );
        assert_eq!(
            health.order(list, RETRY, at(start, 10)),
            ["lobby", "east", "west"]
        );
    }

    #[test]
    fn a_backend_that_answers_is_trusted_again_at_once() {
        let health = Health::default();
        let now = Instant::now();
        health.failed("lobby", now);
        health.answered("lobby");
        assert_eq!(
            health.order(names(&["lobby", "east"]), RETRY, now),
            ["lobby", "east"]
        );
    }

    #[test]
    fn backends_passed_over_keep_their_order_at_the_end() {
        let health = Health::default();
        let now = Instant::now();
        health.failed("west", now);
        health.failed("lobby", now);
        assert_eq!(
            health.order(names(&["lobby", "east", "west", "north"]), RETRY, now),
            ["east", "north", "lobby", "west"]
        );
    }

    #[test]
    fn every_candidate_down_is_still_tried() {
        let health = Health::default();
        let now = Instant::now();
        health.failed("lobby", now);
        health.failed("east", now);
        assert_eq!(
            health.order(names(&["lobby", "east"]), RETRY, now),
            ["lobby", "east"]
        );
    }

    #[test]
    fn a_retry_of_zero_never_passes_a_backend_over() {
        let health = Health::default();
        let now = Instant::now();
        health.failed("lobby", now);
        assert_eq!(health.passed_over_for("lobby", Duration::ZERO, now), None);
        assert_eq!(
            health.order(names(&["lobby", "east"]), Duration::ZERO, now),
            ["lobby", "east"]
        );
    }

    #[test]
    fn the_console_line_says_how_long_is_left() {
        let health = Health::default();
        let start = Instant::now();
        assert_eq!(health.describe("lobby", RETRY, start), None);
        health.failed("lobby", start);
        assert_eq!(
            health.describe("lobby", RETRY, at(start, 3)).as_deref(),
            Some("passed over for 7 more s after a failed login")
        );
        assert_eq!(health.describe("lobby", RETRY, at(start, 10)), None);
    }
}
