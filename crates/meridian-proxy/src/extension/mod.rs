//! The proxy's connection to the extensions (docs/EXTENSIONS.md, sections 2, 3 and 4.1): an external
//! program, each on a loopback port, that the proxy connects to, proves itself to (and asks to prove
//! itself) with the token, and asks for decisions.
//!
//! This is the connection, and only that: no decision point is wired to it yet. A [`Link`] keeps one
//! extension's connection up, with the doubling wait, and offers [`Link::call`], which the decision
//! points will use: it holds the `max-in-flight` places, the breaker, the deadline, the counters and the
//! log lines. Without an extension in the [`Registry`], which is how the proxy starts, nothing here runs
//! and nothing is on the metrics page.

// Nothing calls a link outside the tests until the decision points are wired in the pull requests
// that follow (docs/EXTENSIONS.md, 10.1, items 5 to 8).
#![allow(
    dead_code,
    reason = "the decision points that call a link come in the next pull requests"
)]

mod breaker;
mod connection;
mod link;
mod stats;

#[cfg(test)]
mod tests;

pub use link::{Link, Settings};

use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// A link and the thread that keeps it up.
type Started = (Arc<Link>, Option<JoinHandle<()>>);

/// The extensions of a running proxy.
#[derive(Default)]
pub struct Registry {
    started: Mutex<Vec<Started>>,
}

impl Registry {
    /// Starts keeping a connection to the extension of `settings` up.
    pub fn start(&self, settings: Settings) -> std::io::Result<Arc<Link>> {
        let link = Link::new(settings);
        let thread = {
            let link = Arc::clone(&link);
            std::thread::Builder::new()
                .name(format!("extension-{}", link.name()))
                .spawn(move || connection::run(link))?
        };
        self.started
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((Arc::clone(&link), Some(thread)));
        Ok(link)
    }

    /// The links, in the order they were started.
    pub fn links(&self) -> Vec<Arc<Link>> {
        self.started
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(link, _)| Arc::clone(link))
            .collect()
    }

    /// Ends every connection and waits for the threads. The decisions that wait end as
    /// disconnected.
    pub fn stop(&self) {
        let started = std::mem::take(&mut *self.started.lock().unwrap_or_else(|e| e.into_inner()));
        for (link, _) in &started {
            link.stop();
        }
        for (_, thread) in started {
            if let Some(thread) = thread {
                let _ = thread.join();
            }
        }
    }

    /// The extensions' families for the page of `/metrics`.
    pub fn render(&self, out: &mut String) {
        let stats: Vec<_> = self
            .links()
            .iter()
            .map(|link| (link.name().to_owned(), Arc::clone(link.stats())))
            .collect();
        stats::render(&stats, out);
    }
}

impl Drop for Registry {
    fn drop(&mut self) {
        self.stop();
    }
}
