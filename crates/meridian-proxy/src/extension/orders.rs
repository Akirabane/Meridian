//! The orders of an extension (docs/EXTENSIONS.md, section 5): requests it sends the proxy on the connection the
//! proxy opened. The table [`ORDERS`] says, for each method, the capability it needs and what it does; every
//! other method, and every method whose capability was not granted, is answered `-32601` as if it did not
//! exist.
//!
//! **An order never runs on the thread that reads the connection** (`connection::serve`): that thread also hands
//! the proxy's own requests their answers, and a slow order must not hold them. An order takes one of the
//! extension's `max-running` places and its budget (a token bucket: `rate-per-second`, `burst`) **before** it is
//! started, on the reader, which does nothing more than count and answer an order it refuses. An order that is
//! over the budget or finds no place is answered with an error and is not run. It then runs on a thread of its
//! own, which writes its answer.
//!
//! Counted in `meridian_extension_orders_total{extension,method,outcome}`; a method that is not in the table
//! is counted as `unknown`, never under the name the extension gave (a label an extension could make up).

use super::link::{Link, OrderPlace};
use crate::log::debug;
use meridian_config::DECIDE_COMMAND;
use meridian_extension::orders::{self, Ack, Forget};
use meridian_extension::wire::{self, Discover, DiscoverMethod, Id, RpcError, code};
use meridian_websocket::Writer;
use serde_json::Value;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::time::Instant;

/// What an order does with its parameters: its answer, or the error to send.
type Run = fn(&Arc<Link>, Option<&Value>) -> Result<Value, RpcError>;

/// One order of the proxy.
pub(super) struct Order {
    pub method: &'static str,
    /// The capability the extension needs.
    pub capability: &'static str,
    pub run: Run,
    /// How `rpc.discover` of the proxy describes it.
    pub describe: fn() -> DiscoverMethod,
}

const INVALIDATE: Order = Order {
    method: orders::PERMISSIONS_INVALIDATE,
    capability: DECIDE_COMMAND,
    run: invalidate,
    describe: orders::invalidate_method,
};

/// The orders the proxy serves.
#[cfg(not(test))]
pub(super) const ORDERS: &[Order] = &[INVALIDATE];

/// The orders the proxy serves, and two that only the tests have: one that waits to be let go and one that
/// panics, to see what the dispatcher does with a slow order and a failing one.
#[cfg(test)]
pub(super) const ORDERS: &[Order] = &[INVALIDATE, test_support::BLOCK, test_support::PANIC];

/// How an order ended, for the metrics.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Outcome {
    /// It ran and answered.
    Done,
    /// Its parameters were not what the method takes.
    Invalid,
    /// It ran and failed on the proxy's side (the handler panicked, the thread could not start).
    Failed,
    /// The extension's budget of orders was spent.
    OverBudget,
    /// As many orders as it may have running were running.
    Busy,
    /// No such method, or its capability was not granted.
    Refused,
}

impl Outcome {
    pub const ALL: [Outcome; 6] = [
        Outcome::Done,
        Outcome::Invalid,
        Outcome::Failed,
        Outcome::OverBudget,
        Outcome::Busy,
        Outcome::Refused,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Outcome::Done => "done",
            Outcome::Invalid => "invalid",
            Outcome::Failed => "failed",
            Outcome::OverBudget => "budget",
            Outcome::Busy => "busy",
            Outcome::Refused => "refused",
        }
    }
}

/// The label of a method that is not in the table.
pub(super) const UNKNOWN: &str = "unknown";

/// The extension's budget of orders: a token bucket that holds `burst` tokens and gets `rate` each second.
pub(super) struct Budget {
    rate: f64,
    burst: f64,
    tokens: f64,
    at: Instant,
}

impl Budget {
    /// A rested budget: `burst` orders may go at once.
    pub fn new(rate_per_second: u32, burst: u32, now: Instant) -> Self {
        Self {
            rate: f64::from(rate_per_second),
            burst: f64::from(burst.max(1)),
            tokens: f64::from(burst.max(1)),
            at: now,
        }
    }

    /// Spends `cost` tokens if there are that many. A cost is at least one and **at most the burst**: an order
    /// that reaches more players than the burst holds (a `send all`) costs the whole burst, and is not made
    /// impossible by its own size.
    pub fn take(&mut self, cost: u32, now: Instant) -> bool {
        let passed = now.saturating_duration_since(self.at).as_secs_f64();
        self.tokens = (self.tokens + passed * self.rate).min(self.burst);
        self.at = self.at.max(now);
        let cost = f64::from(cost).clamp(1.0, self.burst);
        if self.tokens >= cost {
            self.tokens -= cost;
            true
        } else {
            false
        }
    }
}

/// The document `rpc.discover` of the proxy answers: the orders the connection was granted the capability of.
pub(super) fn discover(table: &[Order], link: &Link) -> Discover {
    let protocol = link.info().map_or(0, |info| info.protocol);
    Discover::new(
        protocol,
        table
            .iter()
            .filter(|order| link.is_granted(order.capability))
            .map(|order| (order.describe)())
            .collect(),
    )
}

fn invalidate(link: &Arc<Link>, params: Option<&Value>) -> Result<Value, RpcError> {
    match orders::parse_invalidate(params) {
        Ok(Forget::All) => link.forget_all(),
        Ok(Forget::Player(uuid)) => link.forget_player(uuid),
        Err(why) => return Err(RpcError::new(code::INVALID_PARAMS, why)),
    }
    Ok(serde_json::to_value(Ack::done()).unwrap_or(Value::Null))
}

/// Serves one request of the extension that is not `rpc.discover`: answers at once an order it refuses, and
/// starts the others on a thread of their own. The error is a failed write, which ends the connection.
pub(super) fn dispatch(
    table: &'static [Order],
    link: &Arc<Link>,
    writer: &Writer,
    id: Id,
    method: &str,
    params: Option<Value>,
) -> Result<(), String> {
    let refuse = |label: &'static str, outcome: Outcome, error: RpcError| {
        link.stats().order(label, outcome);
        debug!(
            "extension \"{}\": order {label} not run: {}",
            link.name(),
            outcome.label()
        );
        writer
            .send_text(&wire::error(Some(&id), &error))
            .map_err(|e| format!("cannot send: {e}"))
    };
    let Some(order) = table.iter().find(|order| order.method == method) else {
        return refuse(
            UNKNOWN,
            Outcome::Refused,
            RpcError::new(code::METHOD_NOT_FOUND, "method not found"),
        );
    };
    // As if the method did not exist: the answer does not tell that it does.
    if !link.is_granted(order.capability) {
        return refuse(
            order.method,
            Outcome::Refused,
            RpcError::new(code::METHOD_NOT_FOUND, "method not found"),
        );
    }
    let Some(place) = link.order_place() else {
        return refuse(
            order.method,
            Outcome::Busy,
            RpcError::new(code::TOO_MANY_RUNNING, "too many orders running"),
        );
    };
    if !link.take_order(1) {
        return refuse(
            order.method,
            Outcome::OverBudget,
            RpcError::new(code::OVER_BUDGET, "over the budget of orders"),
        );
    }
    let started = {
        let (link, writer, id) = (Arc::clone(link), writer.clone(), id.clone());
        std::thread::Builder::new()
            .name(format!("extension-{}-order", link.name()))
            .spawn(move || run(order, &link, &writer, &id, params.as_ref(), place))
    };
    match started {
        Ok(_) => Ok(()),
        Err(e) => {
            crate::log::warning!(
                "extension \"{}\": cannot start a thread for an order: {e}",
                link.name()
            );
            refuse(
                order.method,
                Outcome::Failed,
                RpcError::new(code::INTERNAL_ERROR, "cannot run the order"),
            )
        }
    }
}

/// The thread of one order: runs it, counts it, gives its place back and writes its answer. The place goes **before**
/// the answer is written, so that an extension that sends its next order when it reads the answer finds it free.
fn run(
    order: &Order,
    link: &Arc<Link>,
    writer: &Writer,
    id: &Id,
    params: Option<&Value>,
    place: OrderPlace,
) {
    let (outcome, reply) = match catch_unwind(AssertUnwindSafe(|| (order.run)(link, params))) {
        Ok(Ok(result)) => (Outcome::Done, wire::result(id, &result)),
        Ok(Err(error)) => {
            let outcome = if error.code == code::INVALID_PARAMS {
                Outcome::Invalid
            } else {
                Outcome::Failed
            };
            (outcome, wire::error(Some(id), &error))
        }
        Err(_) => (
            Outcome::Failed,
            wire::error(
                Some(id),
                &RpcError::new(code::INTERNAL_ERROR, "the order failed"),
            ),
        ),
    };
    link.stats().order(order.method, outcome);
    drop(place);
    debug!(
        "extension \"{}\": order {} ended: {}",
        link.name(),
        order.method,
        outcome.label()
    );
    if let Err(e) = writer.send_text(&reply) {
        crate::log::warning!(
            "extension \"{}\": cannot write the answer to an order: {e}; closing the connection",
            link.name()
        );
        // A frame may be half written: the connection is not used again.
        let _ = writer.shutdown();
    }
}

/// Two orders for the tests of the dispatcher.
#[cfg(test)]
pub(super) mod test_support {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// Set to let the orders that wait go.
    pub static RELEASE: AtomicBool = AtomicBool::new(false);
    /// How many orders of `BLOCK` have started.
    pub static ENTERED: AtomicUsize = AtomicUsize::new(0);

    fn block(_: &Arc<Link>, _: Option<&Value>) -> Result<Value, RpcError> {
        ENTERED.fetch_add(1, Ordering::SeqCst);
        let limit = Instant::now() + std::time::Duration::from_secs(20);
        while !RELEASE.load(Ordering::SeqCst) && Instant::now() < limit {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        Ok(serde_json::to_value(Ack::done()).unwrap())
    }

    fn panic(_: &Arc<Link>, _: Option<&Value>) -> Result<Value, RpcError> {
        panic!("an order that fails");
    }

    pub const BLOCK: Order = Order {
        method: "meridian:test/block",
        capability: "orders.read",
        run: block,
        describe: || DiscoverMethod::new("meridian:test/block"),
    };

    pub const PANIC: Order = Order {
        method: "meridian:test/panic",
        capability: "orders.read",
        run: panic,
        describe: || DiscoverMethod::new("meridian:test/panic"),
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn budget(rate: u32, burst: u32) -> (Budget, Instant) {
        let now = Instant::now();
        (Budget::new(rate, burst, now), now)
    }

    #[test]
    fn a_rested_budget_lets_a_burst_go_and_then_a_few_each_second() {
        let (mut b, t0) = budget(20, 50);
        // The burst: 50 at once, not 51.
        assert!((0..50).all(|_| b.take(1, t0)));
        assert!(!b.take(1, t0));
        // A tenth of a second brings 2 back, not 3.
        let t1 = t0 + Duration::from_millis(100);
        assert!(b.take(1, t1));
        assert!(b.take(1, t1));
        assert!(!b.take(1, t1));
        // A long rest brings the burst back and no more.
        let t2 = t1 + Duration::from_secs(60);
        assert!((0..50).all(|_| b.take(1, t2)));
        assert!(!b.take(1, t2));
    }

    #[test]
    fn an_order_that_costs_more_than_the_burst_costs_the_burst_and_can_still_run() {
        let (mut b, t0) = budget(20, 50);
        // A `send all` that reaches 500 players: the cost is the burst, not 500.
        assert!(b.take(500, t0));
        assert!(!b.take(1, t0));
        // It needs the whole burst, so it waits until the budget has rested.
        let half = t0 + Duration::from_millis(1250);
        assert!(!b.take(500, half));
        let rested = t0 + Duration::from_millis(2500);
        assert!(b.take(500, rested));
        // A cost of 0 is one.
        let (mut b, t0) = budget(1, 2);
        assert!(b.take(0, t0));
        assert!(b.take(0, t0));
        assert!(!b.take(0, t0));
    }

    #[test]
    fn a_clock_that_goes_back_gives_nothing_and_takes_nothing() {
        let (mut b, t0) = budget(20, 2);
        let later = t0 + Duration::from_secs(1);
        assert!(b.take(2, later));
        // An earlier instant than the last one: no tokens come from it.
        assert!(!b.take(1, t0));
        assert!(!b.take(1, later));
    }

    #[test]
    fn the_labels_of_the_outcomes_are_apart_and_fixed() {
        let labels: Vec<_> = Outcome::ALL.iter().map(|o| o.label()).collect();
        assert_eq!(
            labels,
            ["done", "invalid", "failed", "budget", "busy", "refused"]
        );
    }
}
