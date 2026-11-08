//! What the order that runs a console line uses (docs/EXTENSIONS.md, section 5): the proxy's console. **The table of commands is the
//! one the operator's console reads** (`main.rs` builds it once and gives it to both), so an extension granted `orders.console` can
//! run exactly the lines the operator can: no command more, none less, and with the operator's rights, which are all of them.
//!
//! It holds the proxy weakly: the registry that serves the orders is held by the sessions, which the proxy holds, and a hold both
//! ways would keep both for good.

use super::orders::Console;
use crate::console::Commands;
use crate::log::info;
use crate::state::Proxy;
use std::sync::{Arc, Weak};

/// The proxy's own console, for the extensions.
pub(crate) struct ProxyConsole {
    proxy: Weak<Proxy>,
    commands: Arc<Commands>,
}

impl ProxyConsole {
    pub(crate) fn new(proxy: &Arc<Proxy>, commands: Arc<Commands>) -> Self {
        Self {
            proxy: Arc::downgrade(proxy),
            commands,
        }
    }
}

impl Console for ProxyConsole {
    fn execute(&self, line: &str) -> Vec<String> {
        match self.proxy.upgrade() {
            Some(proxy) => self.commands.execute(&proxy, line),
            None => vec!["the proxy is stopping".to_owned()],
        }
    }

    fn journal(&self, entry: &str) {
        info!("{entry}");
    }
}
