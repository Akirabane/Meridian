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
use meridian_config::{ADDRESSES, DECIDE_COMMAND, ORDERS_MESSAGE, ORDERS_READ};
use meridian_extension::orders::Told;
use meridian_extension::orders::{
    self, Ack, Forget, Found, PlayerInfo, QueueState, ServerInfo, ServersList, Who,
};
use meridian_extension::wire::{self, Discover, DiscoverMethod, Id, RpcError, code};
use meridian_websocket::Writer;
use serde_json::Value;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// The facts the orders that read tell, taken from the rest of the proxy: who is online and where, the servers and
/// what the balancer makes of them, and who waits in the queue. Given to the registry once the proxy is built
/// ([`super::Registry::serve_facts`]), which is `extension/facts.rs` for the proxy and a stand-in in the tests.
///
/// **A player's `address` is given only when `with_addresses` says the extension was granted `addresses`**: the source of
/// facts does not hand an address to a caller that did not ask for it, so that a mistake of a caller cannot tell it.
/// No server address is told.
pub trait Facts: Send + Sync {
    /// Every player online, in any order, each with an address if `with_addresses`.
    fn players(&self, with_addresses: bool) -> Vec<PlayerInfo>;
    /// The servers, in the order of the configuration.
    fn servers(&self) -> Vec<ServerInfo>;
    /// The queue, with at most [`orders::MAX_PAGE`] of those who wait.
    fn queue(&self) -> QueueState;
}

/// What the orders that act on the players use, taken from the rest of the proxy: the sessions, to tell a player, and the
/// players of a server, to show them a line (`/alert`). Given to the registry once the proxy is built
/// ([`super::Registry::serve_actions`]): `extension/actions.rs` for the proxy and a stand-in in the tests.
pub trait Actions: Send + Sync {
    /// Shows `text` in the chat of the player `who` is, if they are online. How many were told: 0 or 1.
    fn tell(&self, who: &Who, text: &str) -> usize;
    /// How many players a line to `server` (every server when `None`) reaches, or why there is no such server.
    fn audience(&self, server: Option<&str>) -> Result<usize, String>;
    /// Shows `text` to them, as `/alert` does, and tells how many were told.
    fn broadcast(&self, server: Option<&str>, text: &str) -> Result<usize, String>;
}

/// What the orders share with the rest of the proxy.
#[derive(Default)]
pub struct Services {
    facts: Mutex<Option<Arc<dyn Facts>>>,
    actions: Mutex<Option<Arc<dyn Actions>>>,
}

impl Services {
    pub(super) fn facts(&self) -> Option<Arc<dyn Facts>> {
        self.facts.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub(super) fn set_facts(&self, facts: Arc<dyn Facts>) {
        *self.facts.lock().unwrap_or_else(|e| e.into_inner()) = Some(facts);
    }

    pub(super) fn actions(&self) -> Option<Arc<dyn Actions>> {
        self.actions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub(super) fn set_actions(&self, actions: Arc<dyn Actions>) {
        *self.actions.lock().unwrap_or_else(|e| e.into_inner()) = Some(actions);
    }
}

/// What an order is given: the link it came on, and the facts, if the proxy has given them.
pub(super) struct Ctx<'a> {
    pub link: &'a Arc<Link>,
    installed: Option<Arc<dyn Facts>>,
    acting: Option<Arc<dyn Actions>>,
}

impl Ctx<'_> {
    fn facts(&self) -> Result<&Arc<dyn Facts>, RpcError> {
        self.installed
            .as_ref()
            .ok_or_else(|| RpcError::new(code::INTERNAL_ERROR, "not available"))
    }

    fn actions(&self) -> Result<&Arc<dyn Actions>, RpcError> {
        self.acting
            .as_ref()
            .ok_or_else(|| RpcError::new(code::INTERNAL_ERROR, "not available"))
    }

    /// Charges the budget for an order that reaches `reached` players: **the number of players, and at most the burst**
    /// (the dispatcher took one already). An order that reaches more than the burst holds costs the whole burst, so that
    /// it can run at all; one that the budget cannot pay is refused here with the code of a spent budget, **before
    /// anyone is reached**.
    fn spend_for(&self, reached: usize) -> Result<(), RpcError> {
        let burst = self.link.settings.order_burst.max(1);
        let total = u32::try_from(reached).unwrap_or(u32::MAX).clamp(1, burst);
        if total > 1 && !self.link.take_order(total - 1) {
            return Err(RpcError::new(
                code::OVER_BUDGET,
                "over the budget of orders",
            ));
        }
        Ok(())
    }

    /// The players, with an address only if the extension was granted `addresses`.
    fn players(&self) -> Result<Vec<PlayerInfo>, RpcError> {
        Ok(self.facts()?.players(self.link.is_granted(ADDRESSES)))
    }
}

fn invalid(why: String) -> RpcError {
    RpcError::new(code::INVALID_PARAMS, why)
}

/// The answer of an order, or the error if it could not be written as JSON (which a type of ours never fails to be).
fn done(answer: serde_json::Result<Value>) -> Result<Value, RpcError> {
    answer.map_err(|_| RpcError::new(code::INTERNAL_ERROR, "the order failed"))
}

/// What an order does with its parameters: its answer, or the error to send.
type Run = fn(&Ctx<'_>, Option<&Value>) -> Result<Value, RpcError>;

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

const PLAYERS_LIST: Order = Order {
    method: orders::PLAYERS_LIST,
    capability: ORDERS_READ,
    run: players_list,
    describe: || orders::read_methods().swap_remove(0).1,
};

const PLAYERS_FIND: Order = Order {
    method: orders::PLAYERS_FIND,
    capability: ORDERS_READ,
    run: players_find,
    describe: || orders::read_methods().swap_remove(1).1,
};

const SERVERS_LIST: Order = Order {
    method: orders::SERVERS_LIST,
    capability: ORDERS_READ,
    run: servers_list,
    describe: || orders::read_methods().swap_remove(2).1,
};

const QUEUE_LIST: Order = Order {
    method: orders::QUEUE_LIST,
    capability: ORDERS_READ,
    run: queue_list,
    describe: || orders::read_methods().swap_remove(3).1,
};

const PLAYERS_TELL: Order = Order {
    method: orders::PLAYERS_TELL,
    capability: ORDERS_MESSAGE,
    run: players_tell,
    describe: || orders::message_methods().swap_remove(0).1,
};

const BROADCAST: Order = Order {
    method: orders::BROADCAST,
    capability: ORDERS_MESSAGE,
    run: broadcast,
    describe: || orders::message_methods().swap_remove(1).1,
};

/// The orders the proxy serves.
#[cfg(not(test))]
pub(super) const ORDERS: &[Order] = &[
    INVALIDATE,
    PLAYERS_LIST,
    PLAYERS_FIND,
    SERVERS_LIST,
    QUEUE_LIST,
    PLAYERS_TELL,
    BROADCAST,
];

/// The orders the proxy serves, and two that only the tests have: one that waits to be let go and one that
/// panics, to see what the dispatcher does with a slow order and a failing one.
#[cfg(test)]
pub(super) const ORDERS: &[Order] = &[
    INVALIDATE,
    PLAYERS_LIST,
    PLAYERS_FIND,
    SERVERS_LIST,
    QUEUE_LIST,
    PLAYERS_TELL,
    BROADCAST,
    test_support::BLOCK,
    test_support::PANIC,
    test_support::SLOW_WRITE,
];

/// Writes the answer of an order. In the tests an order of `SLOW_WRITE` waits here, as a write that blocks does
/// (an extension that does not read), until the test lets it go: the same thing for any size of socket buffer, which
/// no test can rely on (Linux takes a good deal more than Windows before a write blocks).
fn deliver(order: &Order, writer: &Writer, text: &str) -> std::io::Result<()> {
    #[cfg(test)]
    if order.method == test_support::SLOW_WRITE.method {
        test_support::wait_to_write();
    }
    #[cfg(not(test))]
    let _ = order;
    writer.send_text(text)
}

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

fn invalidate(ctx: &Ctx<'_>, params: Option<&Value>) -> Result<Value, RpcError> {
    match orders::parse_invalidate(params) {
        Ok(Forget::All) => ctx.link.forget_all(),
        Ok(Forget::Player(uuid)) => ctx.link.forget_player(uuid),
        Err(why) => return Err(invalid(why)),
    }
    done(serde_json::to_value(Ack::done()))
}

/// `players/list`: a page of the players, in the order of their ids.
fn players_list(ctx: &Ctx<'_>, params: Option<&Value>) -> Result<Value, RpcError> {
    let page = orders::parse_list(params).map_err(invalid)?;
    done(serde_json::to_value(orders::page_of(ctx.players()?, &page)))
}

/// `players/find`: one player, by unique id or by name (whatever its case), or `null`.
fn players_find(ctx: &Ctx<'_>, params: Option<&Value>) -> Result<Value, RpcError> {
    let who = orders::parse_find(params).map_err(invalid)?;
    let player = ctx.players()?.into_iter().find(|p| match &who {
        Who::Id(id) => p.uuid.eq_ignore_ascii_case(id),
        Who::Name(name) => p.name.eq_ignore_ascii_case(name),
    });
    done(serde_json::to_value(&Found { player }))
}

/// `servers/list`: the servers, with their players and what the balancer makes of them.
fn servers_list(ctx: &Ctx<'_>, params: Option<&Value>) -> Result<Value, RpcError> {
    orders::parse_no_params(params).map_err(invalid)?;
    done(serde_json::to_value(&ServersList {
        servers: ctx.facts()?.servers(),
    }))
}

/// `players/tell`: a line in one player's chat. A player who is not online is not an error: `{"told": 0}`.
fn players_tell(ctx: &Ctx<'_>, params: Option<&Value>) -> Result<Value, RpcError> {
    let (who, text) = orders::parse_tell(params).map_err(invalid)?;
    let told = ctx.actions()?.tell(&who, &text);
    debug!(
        "extension \"{}\" told {told} player(s) a line",
        ctx.link.name()
    );
    done(serde_json::to_value(Told { told }))
}

/// `broadcast`: a line to every player, or to those on one server, as `/alert` shows it. It costs the budget the
/// number of players it reaches, and at most the burst; it is refused for want of budget before anyone is told.
fn broadcast(ctx: &Ctx<'_>, params: Option<&Value>) -> Result<Value, RpcError> {
    let (server, text) = orders::parse_broadcast(params).map_err(invalid)?;
    let actions = ctx.actions()?;
    // The proxy's own words for a server it does not have name it: they are not given back.
    let no_server = |_: String| invalid("\"server\" is not a server of the proxy".into());
    let reached = actions.audience(server.as_deref()).map_err(no_server)?;
    ctx.spend_for(reached)?;
    let told = actions
        .broadcast(server.as_deref(), &text)
        .map_err(no_server)?;
    crate::log::info!(
        "extension \"{}\" alerted {} to {told}: {text}",
        ctx.link.name(),
        server.as_deref().unwrap_or("every server")
    );
    done(serde_json::to_value(Told { told }))
}

/// `queue/list`: whether the queue is on, and who waits.
fn queue_list(ctx: &Ctx<'_>, params: Option<&Value>) -> Result<Value, RpcError> {
    orders::parse_no_params(params).map_err(invalid)?;
    done(serde_json::to_value(ctx.facts()?.queue()))
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
    // The threads first: those that wait to write an answer are not running, and are bounded here.
    let busy = || {
        refuse(
            order.method,
            Outcome::Busy,
            RpcError::new(code::TOO_MANY_RUNNING, "too many orders running"),
        )
    };
    let Some(thread) = link.order_thread() else {
        return busy();
    };
    let Some(place) = link.order_place() else {
        return busy();
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
            .spawn(move || {
                run(order, &link, &writer, &id, params.as_ref(), place);
                // After the write: the thread is alive until its answer is out or has failed.
                drop(thread);
            })
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
    let ctx = Ctx {
        link,
        installed: link.facts(),
        acting: link.actions(),
    };
    let (outcome, reply) = match catch_unwind(AssertUnwindSafe(|| (order.run)(&ctx, params))) {
        Ok(Ok(result)) => (Outcome::Done, wire::result(id, &result)),
        Ok(Err(error)) => {
            let outcome = match error.code {
                code::INVALID_PARAMS => Outcome::Invalid,
                // An order that costs by the players it reaches can find the budget too low once it knows how many.
                code::OVER_BUDGET => Outcome::OverBudget,
                _ => Outcome::Failed,
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
    if let Err(e) = deliver(order, writer, &reply) {
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

    fn block(_: &Ctx<'_>, _: Option<&Value>) -> Result<Value, RpcError> {
        ENTERED.fetch_add(1, Ordering::SeqCst);
        let limit = Instant::now() + std::time::Duration::from_secs(20);
        while !RELEASE.load(Ordering::SeqCst) && Instant::now() < limit {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        Ok(serde_json::to_value(Ack::done()).unwrap())
    }

    fn panic(_: &Ctx<'_>, _: Option<&Value>) -> Result<Value, RpcError> {
        panic!("an order that fails");
    }

    pub const BLOCK: Order = Order {
        method: "meridian:test/block",
        capability: "orders.read",
        run: block,
        describe: || DiscoverMethod::new("meridian:test/block"),
    };

    /// Set to let the answers of `SLOW_WRITE` go.
    pub static WRITE_OPEN: AtomicBool = AtomicBool::new(false);
    /// How many answers of `SLOW_WRITE` wait to be written.
    pub static WAITING_TO_WRITE: AtomicUsize = AtomicUsize::new(0);

    /// What `deliver` does for `SLOW_WRITE`.
    pub fn wait_to_write() {
        WAITING_TO_WRITE.fetch_add(1, Ordering::SeqCst);
        let limit = Instant::now() + std::time::Duration::from_secs(20);
        while !WRITE_OPEN.load(Ordering::SeqCst) && Instant::now() < limit {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        WAITING_TO_WRITE.fetch_sub(1, Ordering::SeqCst);
    }

    fn quick(_: &Ctx<'_>, _: Option<&Value>) -> Result<Value, RpcError> {
        Ok(serde_json::to_value(Ack::done()).unwrap())
    }

    /// An order that runs at once and whose answer then waits to be written.
    pub const SLOW_WRITE: Order = Order {
        method: "meridian:test/slow-write",
        capability: "orders.read",
        run: quick,
        describe: || DiscoverMethod::new("meridian:test/slow-write"),
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
