//! What the orders that operate the network use (docs/EXTENSIONS.md, section 5): the balancer's drain, the evacuation of a server and
//! the reload of the configuration, as the console's `drain`, `undrain` and `reload` do them: the same calls, so what an operator
//! can do at the console an extension with `orders.operate` can do over its connection.
//!
//! It holds the proxy weakly: the registry that serves the orders is held by the sessions, which the proxy holds, and a hold both
//! ways would keep both for good.

use super::link::SendLot;
use super::orders::{MovePlan, Operate, Paid, Refusal};
use crate::console;
use crate::evacuate;
use crate::log::{info, warning};
use crate::state::Proxy;
use std::sync::{Arc, Weak};

/// The proxy's own way to operate.
pub(crate) struct ProxyOperate {
    proxy: Weak<Proxy>,
}

impl ProxyOperate {
    pub(crate) fn new(proxy: &Arc<Proxy>) -> Self {
        Self {
            proxy: Arc::downgrade(proxy),
        }
    }
}

impl Operate for ProxyOperate {
    fn players_on(&self, server: &str) -> Result<usize, Refusal> {
        let Some(proxy) = self.proxy.upgrade() else {
            return Err(Refusal::NoRole);
        };
        if proxy.config().balanced(server).is_none() {
            return Err(Refusal::NoRole);
        }
        Ok(proxy
            .sessions
            .players()
            .iter()
            .filter(|p| p.backend == server)
            .count())
    }

    fn drain(&self, server: &str) -> Result<bool, Refusal> {
        let Some(proxy) = self.proxy.upgrade() else {
            return Err(Refusal::NoRole);
        };
        if proxy.config().balanced(server).is_none() {
            return Err(Refusal::NoRole);
        }
        let newly = proxy.sessions.balancer.drain(server);
        // A backend the detector drained stays drained when it is back: the operator, here the extension, drained it too.
        proxy.restarts.keep_drained(server);
        if newly {
            info!("backend \"{server}\" is drained by an extension");
        }
        Ok(newly)
    }

    fn undrain(&self, server: &str) -> Result<bool, Refusal> {
        let Some(proxy) = self.proxy.upgrade() else {
            return Ok(false);
        };
        // The detector drained it for a restart: it is offered again when it is back, not before. (The console's `undrain` takes
        // the operator's word for it; an extension's is not taken that far.)
        if proxy.restarts.is_restarting(server) {
            return Err(Refusal::Restarting);
        }
        let undrained = proxy.sessions.balancer.undrain(server);
        if undrained {
            info!("backend \"{server}\" is offered again by an extension");
        }
        Ok(undrained)
    }

    fn plan_evacuation(&self, server: &str) -> Box<dyn MovePlan> {
        let count = self.proxy.upgrade().map_or(0, |proxy| {
            proxy
                .sessions
                .players()
                .iter()
                .filter(|p| p.backend == server)
                .count()
        });
        Box::new(Evacuation {
            proxy: self.proxy.clone(),
            server: server.to_owned(),
            count,
        })
    }

    fn reload(&self) {
        let Some(proxy) = self.proxy.upgrade() else {
            return;
        };
        match console::reload_file(&proxy) {
            Ok(lines) => {
                for line in lines {
                    info!("reload (asked by an extension): {line}");
                }
            }
            Err(why) => warning!("reload (asked by an extension): {why}"),
        }
    }
}

/// The players of a drained server, to be sent to the other servers of its role: one at a time, on a thread of its own, as the
/// console's `drain --move` does (here the order is answered when the moves start, not when they end).
struct Evacuation {
    proxy: Weak<Proxy>,
    server: String,
    count: usize,
}

impl MovePlan for Evacuation {
    fn count(&self) -> usize {
        self.count
    }

    fn start(self: Box<Self>, _paid: Paid, lot: SendLot) -> Result<usize, Refusal> {
        let Evacuation {
            proxy,
            server,
            count,
        } = *self;
        if count == 0 {
            return Ok(0);
        }
        let named = server.clone();
        std::thread::Builder::new()
            .name("extension-evacuate".into())
            .spawn(move || {
                // The extension's place for a send is held until every move is done, and let go however this ends.
                let _lot = lot;
                let Some(proxy) = proxy.upgrade() else {
                    return;
                };
                let config = proxy.config();
                if let Some(outcome) = evacuate::send_away(&proxy.sessions, &config, &server) {
                    for line in outcome.report(&server) {
                        info!("evacuation (asked by an extension): {line}");
                    }
                }
            })
            .map(|_| count)
            .map_err(|e| {
                warning!("cannot start a thread to send the players of {named} away: {e}");
                Refusal::CannotStart
            })
    }
}
