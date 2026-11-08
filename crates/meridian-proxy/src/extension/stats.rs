//! What the proxy counts about an extension (docs/EXTENSIONS.md, section 7). Atomic counters bumped
//! where things happen, and a page worked out when it is asked for. A label is the extension's name from
//! the configuration, a method of the proxy's own, or a fixed word: never a player's name, address or
//! unique id, and never anything the extension said.

use super::orders;
use crate::metrics::{Histogram, escaped, family};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How a request of the proxy ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// A usable answer.
    Answered,
    /// The deadline passed.
    Timeout,
    /// An answer that is an error, or not what the method defines.
    Invalid,
    /// No place for one more request (`max-in-flight`).
    Busy,
    /// The breaker is open.
    Breaker,
    /// There was no connection, or it broke while the request waited.
    Disconnected,
}

impl Outcome {
    pub const ALL: [Outcome; 6] = [
        Outcome::Answered,
        Outcome::Timeout,
        Outcome::Invalid,
        Outcome::Busy,
        Outcome::Breaker,
        Outcome::Disconnected,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Outcome::Answered => "answered",
            Outcome::Timeout => "timeout",
            Outcome::Invalid => "invalid",
            Outcome::Busy => "busy",
            Outcome::Breaker => "breaker",
            Outcome::Disconnected => "disconnected",
        }
    }
}

#[derive(Default)]
struct Method {
    outcomes: [AtomicU64; Outcome::ALL.len()],
    seconds: Histogram,
}

/// The counters of one extension.
#[derive(Default)]
pub struct Stats {
    connected: AtomicBool,
    breaker_open: AtomicBool,
    late: AtomicU64,
    methods: Mutex<BTreeMap<&'static str, Arc<Method>>>,
    /// Decisions by point and by what became of them.
    decisions: Mutex<BTreeMap<(&'static str, &'static str), u64>>,
    /// The extension's orders by method and by how they ended.
    orders: Mutex<BTreeMap<(&'static str, orders::Outcome), u64>>,
}

impl Stats {
    pub fn set_connected(&self, connected: bool) {
        self.connected.store(connected, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    pub fn set_breaker_open(&self, open: bool) {
        self.breaker_open.store(open, Ordering::Relaxed);
    }

    /// An answer came after its request was given up on, or for a request nobody sent.
    pub fn late_answer(&self) {
        self.late.fetch_add(1, Ordering::Relaxed);
    }

    fn method(&self, method: &'static str) -> Arc<Method> {
        let mut methods = self.methods.lock().unwrap_or_else(|e| e.into_inner());
        Arc::clone(methods.entry(method).or_default())
    }

    /// A request for `method` ended as `outcome`. `took` is the time from its send to its end, for
    /// the requests that were sent.
    pub fn request(&self, method: &'static str, outcome: Outcome, took: Option<Duration>) {
        let method = self.method(method);
        let i = Outcome::ALL
            .iter()
            .position(|o| *o == outcome)
            .unwrap_or_default();
        method.outcomes[i].fetch_add(1, Ordering::Relaxed);
        if let Some(took) = took {
            method.seconds.observe(took);
        }
    }

    /// A decision of `point` (`"login"`) was taken: `decision` is what the extension answered (`"allow"`,
    /// `"deny"`, `"default"`) or what the on-error policy did for want of an answer (`"policy_allow"`,
    /// `"policy_deny"`, `"policy_default"`).
    pub fn decided(&self, point: &'static str, decision: &'static str) {
        *self
            .decisions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry((point, decision))
            .or_default() += 1;
    }

    /// An order of the extension, `method` (one of the proxy's own, or `unknown`), ended as `outcome`.
    pub fn order(&self, method: &'static str, outcome: orders::Outcome) {
        *self
            .orders
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry((method, outcome))
            .or_default() += 1;
    }

    /// How many orders of `method` ended as `outcome`.
    #[cfg(test)]
    pub fn orders(&self, method: &'static str, outcome: orders::Outcome) -> u64 {
        self.orders
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(method, outcome))
            .copied()
            .unwrap_or(0)
    }

    /// How many decisions of `point` ended as `decision`.
    #[cfg(test)]
    pub fn decisions(&self, point: &'static str, decision: &'static str) -> u64 {
        self.decisions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(point, decision))
            .copied()
            .unwrap_or(0)
    }

    /// How many requests of `method` ended as `outcome`.
    #[cfg(test)]
    pub fn count(&self, method: &'static str, outcome: Outcome) -> u64 {
        let i = Outcome::ALL
            .iter()
            .position(|o| *o == outcome)
            .unwrap_or_default();
        self.method(method).outcomes[i].load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub fn late(&self) -> u64 {
        self.late.load(Ordering::Relaxed)
    }
}

/// The families of every extension, in the Prometheus text format, to follow the proxy's own.
pub fn render(extensions: &[(String, Arc<Stats>)], out: &mut String) {
    if extensions.is_empty() {
        return;
    }
    family(
        out,
        "meridian_extension_connected",
        "gauge",
        "1 while the proxy has a checked connection to the extension.",
    );
    for (name, stats) in extensions {
        let _ = writeln!(
            out,
            "meridian_extension_connected{{extension=\"{}\"}} {}",
            escaped(name),
            u8::from(stats.connected.load(Ordering::Relaxed))
        );
    }
    family(
        out,
        "meridian_extension_breaker_open",
        "gauge",
        "1 while the extension's breaker is open: its decisions take their on-error policy.",
    );
    for (name, stats) in extensions {
        let _ = writeln!(
            out,
            "meridian_extension_breaker_open{{extension=\"{}\"}} {}",
            escaped(name),
            u8::from(stats.breaker_open.load(Ordering::Relaxed))
        );
    }
    family(
        out,
        "meridian_extension_requests_total",
        "counter",
        "Requests to the extension, by how they ended.",
    );
    for (name, stats) in extensions {
        let methods = stats.methods.lock().unwrap_or_else(|e| e.into_inner());
        for (method, counts) in methods.iter() {
            for (outcome, count) in Outcome::ALL.iter().zip(&counts.outcomes) {
                let _ = writeln!(
                    out,
                    "meridian_extension_requests_total{{extension=\"{}\",method=\"{}\",outcome=\"{}\"}} {}",
                    escaped(name),
                    escaped(method),
                    outcome.label(),
                    count.load(Ordering::Relaxed)
                );
            }
        }
    }
    family(
        out,
        "meridian_extension_request_seconds",
        "histogram",
        "Time from sending a request to the extension to the end of its wait.",
    );
    for (name, stats) in extensions {
        let methods = stats.methods.lock().unwrap_or_else(|e| e.into_inner());
        for (method, counts) in methods.iter() {
            counts.seconds.render(
                out,
                "meridian_extension_request_seconds",
                &format!(
                    "extension=\"{}\",method=\"{}\"",
                    escaped(name),
                    escaped(method)
                ),
            );
        }
    }
    family(
        out,
        "meridian_extension_decisions_total",
        "counter",
        "Decisions asked of the extension, by point and by what became of them: its answer, or the on-error policy.",
    );
    for (name, stats) in extensions {
        let decisions = stats.decisions.lock().unwrap_or_else(|e| e.into_inner());
        for ((point, decision), count) in decisions.iter() {
            let _ = writeln!(
                out,
                "meridian_extension_decisions_total{{extension=\"{}\",point=\"{point}\",decision=\"{decision}\"}} {count}",
                escaped(name),
            );
        }
    }
    family(
        out,
        "meridian_extension_orders_total",
        "counter",
        "Orders the extension gave the proxy, by method and by how they ended.",
    );
    for (name, stats) in extensions {
        let orders = stats.orders.lock().unwrap_or_else(|e| e.into_inner());
        let methods: BTreeSet<&str> = orders.keys().map(|(method, _)| *method).collect();
        for method in methods {
            for outcome in orders::Outcome::ALL {
                let _ = writeln!(
                    out,
                    "meridian_extension_orders_total{{extension=\"{}\",method=\"{}\",outcome=\"{}\"}} {}",
                    escaped(name),
                    escaped(method),
                    outcome.label(),
                    orders.get(&(method, outcome)).copied().unwrap_or(0)
                );
            }
        }
    }
    family(
        out,
        "meridian_extension_late_answers_total",
        "counter",
        "Answers that came after their request was given up on, and were dropped.",
    );
    for (name, stats) in extensions {
        let _ = writeln!(
            out,
            "meridian_extension_late_answers_total{{extension=\"{}\"}} {}",
            escaped(name),
            stats.late.load(Ordering::Relaxed)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_has_a_line_for_each_extension_and_each_outcome_of_a_method_asked() {
        let stats = Arc::new(Stats::default());
        stats.set_connected(true);
        stats.request(
            "meridian:decide/login",
            Outcome::Answered,
            Some(Duration::from_millis(30)),
        );
        stats.request("meridian:decide/login", Outcome::Busy, None);
        stats.late_answer();
        let mut out = String::new();
        render(&[("perms".to_owned(), stats)], &mut out);
        for line in [
            "meridian_extension_connected{extension=\"perms\"} 1",
            "meridian_extension_breaker_open{extension=\"perms\"} 0",
            "meridian_extension_requests_total{extension=\"perms\",method=\"meridian:decide/login\",outcome=\"answered\"} 1",
            "meridian_extension_requests_total{extension=\"perms\",method=\"meridian:decide/login\",outcome=\"busy\"} 1",
            "meridian_extension_requests_total{extension=\"perms\",method=\"meridian:decide/login\",outcome=\"timeout\"} 0",
            "meridian_extension_request_seconds_count{extension=\"perms\",method=\"meridian:decide/login\"} 1",
            "meridian_extension_late_answers_total{extension=\"perms\"} 1",
        ] {
            assert!(out.contains(line), "{line}\n{out}");
        }
        // A request that was never sent is not in the histogram.
        assert!(!out.contains(
            "request_seconds_count{extension=\"perms\",method=\"meridian:decide/login\"} 2"
        ));
    }

    #[test]
    fn with_no_extension_the_page_gets_nothing() {
        let mut out = String::new();
        render(&[], &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn a_name_is_escaped_in_a_label() {
        let stats = Arc::new(Stats::default());
        let mut out = String::new();
        render(&[("a\"b".to_owned(), stats)], &mut out);
        assert!(out.contains("extension=\"a\\\"b\""), "{out}");
    }
}
