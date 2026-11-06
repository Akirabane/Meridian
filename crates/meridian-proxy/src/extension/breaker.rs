//! The breaker of an extension (docs/EXTENSIONS.md, 4.1): after `failures` decisions in a row that end
//! without a usable answer it opens for `open_for`; while it is open every decision takes its on-error
//! policy at once, without a request. When the time has passed one decision is sent, a trial: an answer
//! closes the breaker, and a failure opens it again for the same time. A broken connection opens it at
//! once, with no end of its own, and the next hello closes it.
//!
//! It is a plain state machine: the caller gives it the time and does the logging, so that it can be
//! tested without waiting.

use std::time::{Duration, Instant};

#[derive(Debug)]
enum State {
    /// Decisions go through; `failures` have failed in a row.
    Closed { failures: u32 },
    /// Decisions are refused: until `until`, or until the next hello when there is none.
    Open { until: Option<Instant> },
    /// One decision is out as a trial; the others are refused until it is answered.
    Trial,
}

/// What a decision may do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Permit {
    /// Send it.
    Go,
    /// Send it: it is the trial, and its result decides.
    Trial,
    /// Do not send it.
    Refuse,
}

/// A change to tell people about, once each way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Opened,
    Closed,
}

#[derive(Debug)]
pub struct Breaker {
    failures: u32,
    open_for: Duration,
    state: State,
}

impl Breaker {
    /// A closed breaker that opens after `failures` failures in a row (at least one), for `open_for`.
    pub fn new(failures: u32, open_for: Duration) -> Self {
        Self {
            failures: failures.max(1),
            open_for,
            state: State::Closed { failures: 0 },
        }
    }

    pub fn is_open(&self) -> bool {
        !matches!(self.state, State::Closed { .. })
    }

    /// Whether a decision may be sent now.
    pub fn permit(&mut self, now: Instant) -> Permit {
        match self.state {
            State::Closed { .. } => Permit::Go,
            State::Open { until: Some(until) } if now >= until => {
                self.state = State::Trial;
                Permit::Trial
            }
            State::Open { .. } | State::Trial => Permit::Refuse,
        }
    }

    /// A decision got a usable answer. `trial` is whether it was the trial. An answer to a decision
    /// that was sent before the breaker opened says nothing, and is ignored.
    pub fn success(&mut self, trial: bool) -> Option<Change> {
        match self.state {
            State::Closed { .. } => {
                self.state = State::Closed { failures: 0 };
                None
            }
            State::Trial if trial => {
                self.state = State::Closed { failures: 0 };
                Some(Change::Closed)
            }
            State::Trial | State::Open { .. } => None,
        }
    }

    /// A decision ended without a usable answer.
    pub fn failure(&mut self, now: Instant, trial: bool) -> Option<Change> {
        match self.state {
            State::Closed { failures } if !trial => {
                if failures + 1 >= self.failures {
                    self.state = State::Open {
                        until: Some(now + self.open_for),
                    };
                    Some(Change::Opened)
                } else {
                    self.state = State::Closed {
                        failures: failures + 1,
                    };
                    None
                }
            }
            State::Trial if trial => {
                // Open again for the same time. It was open already: nothing new to tell.
                self.state = State::Open {
                    until: Some(now + self.open_for),
                };
                None
            }
            _ => None,
        }
    }

    /// The connection broke: open at once, until the next hello.
    pub fn trip(&mut self) -> Option<Change> {
        let was_open = self.is_open();
        self.state = State::Open { until: None };
        (!was_open).then_some(Change::Opened)
    }

    /// A hello was checked: close.
    pub fn reset(&mut self) -> Option<Change> {
        let was_open = self.is_open();
        self.state = State::Closed { failures: 0 };
        was_open.then_some(Change::Closed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPEN_FOR: Duration = Duration::from_secs(30);

    fn at(seconds: u64, base: Instant) -> Instant {
        base + Duration::from_secs(seconds)
    }

    #[test]
    fn it_opens_after_that_many_failures_in_a_row_and_an_answer_resets_the_count() {
        let (base, mut breaker) = (Instant::now(), Breaker::new(3, OPEN_FOR));
        assert_eq!(breaker.failure(base, false), None);
        assert_eq!(breaker.failure(base, false), None);
        assert_eq!(breaker.success(false), None);
        // The count began again.
        assert_eq!(breaker.failure(base, false), None);
        assert_eq!(breaker.failure(base, false), None);
        assert_eq!(breaker.permit(base), Permit::Go);
        assert_eq!(breaker.failure(base, false), Some(Change::Opened));
        assert!(breaker.is_open());
        assert_eq!(breaker.permit(at(1, base)), Permit::Refuse);
    }

    #[test]
    fn after_the_time_one_decision_is_the_trial_and_the_others_wait_for_it() {
        let (base, mut breaker) = (Instant::now(), Breaker::new(1, OPEN_FOR));
        assert_eq!(breaker.failure(base, false), Some(Change::Opened));
        assert_eq!(breaker.permit(at(29, base)), Permit::Refuse);
        assert_eq!(breaker.permit(at(30, base)), Permit::Trial);
        // While the trial is out, no other goes.
        assert_eq!(breaker.permit(at(31, base)), Permit::Refuse);
    }

    #[test]
    fn a_trial_that_is_answered_closes_it_and_a_failed_one_opens_it_again_for_the_same_time() {
        let (base, mut breaker) = (Instant::now(), Breaker::new(1, OPEN_FOR));
        breaker.failure(base, false);
        assert_eq!(breaker.permit(at(30, base)), Permit::Trial);
        assert_eq!(breaker.success(true), Some(Change::Closed));
        assert_eq!(breaker.permit(at(30, base)), Permit::Go);

        breaker.failure(at(40, base), false);
        assert_eq!(breaker.permit(at(70, base)), Permit::Trial);
        // The trial fails: open again until 70 + 30, with nothing new to tell.
        assert_eq!(breaker.failure(at(70, base), true), None);
        assert_eq!(breaker.permit(at(99, base)), Permit::Refuse);
        assert_eq!(breaker.permit(at(100, base)), Permit::Trial);
    }

    #[test]
    fn what_was_sent_before_it_opened_does_not_change_it() {
        let (base, mut breaker) = (Instant::now(), Breaker::new(1, OPEN_FOR));
        breaker.failure(base, false);
        // A late answer, or a late failure, of a decision that was not the trial.
        assert_eq!(breaker.success(false), None);
        assert_eq!(breaker.failure(at(1, base), false), None);
        assert!(breaker.is_open());
        assert_eq!(breaker.permit(at(30, base)), Permit::Trial);
        // During the trial too.
        assert_eq!(breaker.success(false), None);
        assert_eq!(breaker.permit(at(31, base)), Permit::Refuse);
    }

    #[test]
    fn a_broken_connection_opens_it_with_no_end_and_the_next_hello_closes_it() {
        let (base, mut breaker) = (Instant::now(), Breaker::new(5, OPEN_FOR));
        assert_eq!(breaker.trip(), Some(Change::Opened));
        // Tripping an open breaker tells nothing.
        assert_eq!(breaker.trip(), None);
        assert_eq!(breaker.permit(at(3600, base)), Permit::Refuse);
        assert_eq!(breaker.reset(), Some(Change::Closed));
        assert_eq!(breaker.reset(), None);
        assert_eq!(breaker.permit(base), Permit::Go);
    }

    #[test]
    fn a_breaker_that_is_open_for_failures_is_closed_by_a_hello_too() {
        let (base, mut breaker) = (Instant::now(), Breaker::new(1, OPEN_FOR));
        breaker.failure(base, false);
        assert_eq!(breaker.reset(), Some(Change::Closed));
    }

    #[test]
    fn at_least_one_failure_is_needed() {
        let (base, mut breaker) = (Instant::now(), Breaker::new(0, OPEN_FOR));
        assert_eq!(breaker.failure(base, false), Some(Change::Opened));
    }
}
