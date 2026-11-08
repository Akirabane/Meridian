//! The proxy's connection to the extensions (docs/EXTENSIONS.md, sections 2, 3 and 4.1): an external
//! program, each on a loopback port, that the proxy connects to, proves itself to (and asks to prove
//! itself) with the token, and asks for decisions.
//!
//! A [`Link`] keeps one extension's connection up, with the doubling wait, and offers [`Link::call`],
//! which the decision points use: it holds the `max-in-flight` places, the breaker, the deadline, the
//! counters and the log lines. The decision points wired so far are the admission at login ([`login`]), the
//! choice of server ([`server`]) and the permission of a command ([`command`]); [`own`] takes the commands
//! of an extension's own, and [`orders`] serves the requests the extension sends the proxy (the budget, the
//! places, the thread of each order, and `permissions/invalidate`).
//! Without an extension in the [`Registry`], which is how the proxy starts, nothing here runs and nothing
//! is on the metrics page.

pub(crate) mod actions;
mod breaker;
pub(crate) mod command;
mod connection;
pub(crate) mod facts;
mod link;
pub(crate) mod login;
mod orders;
pub(crate) mod own;
pub(crate) mod server;
mod stats;

#[cfg(test)]
mod tests;

pub use link::{Link, Settings};
#[cfg(test)]
pub(crate) use orders::Refusal;
pub use orders::{Actions, Facts, MovePlan, Services};
#[cfg(test)]
pub(crate) use stats::Outcome as RequestOutcome;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// A link and the thread that keeps it up.
type Started = (Arc<Link>, Option<JoinHandle<()>>);

/// The extensions of a running proxy.
#[derive(Default)]
pub struct Registry {
    started: Mutex<Vec<Started>>,
    /// Held while the registry follows a configuration, so that two reloads at once do not each
    /// start the same extension.
    following: Mutex<()>,
    /// How many configurations the registry has followed: what is kept of an extension's answers is
    /// dropped at a reload, which may change what it is asked, how long it is kept and who is asked.
    generation: AtomicU64,
    /// The words the extensions hold for their own commands, resolved for the hellos they have been through
    /// (`own.rs`).
    resolved: Mutex<Option<Arc<own::Resolved>>>,
    /// What the orders need of the rest of the proxy (`orders.rs`), shared with every link.
    services: Arc<Services>,
}

impl Registry {
    /// Starts keeping a connection to the extension of `settings` up.
    pub fn start(&self, settings: Settings) -> std::io::Result<Arc<Link>> {
        let link = Link::new(settings, Arc::clone(&self.services));
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

    /// Makes the extensions that run the ones of `config` that are enabled: starts those that are
    /// not running, stops those that are not in the configuration any more or are not enabled, and
    /// opens a new connection for those whose connection settings changed. An extension that did
    /// not change is left alone, with its connection, its breaker and its counters. Says what it
    /// did, one line each. The decisions that wait on a connection that is closed end as
    /// disconnected, and take their on-error policy.
    pub fn follow(&self, config: &meridian_config::Config) -> Vec<String> {
        let _following = self.following.lock().unwrap_or_else(|e| e.into_inner());
        self.generation.fetch_add(1, Ordering::Relaxed);
        let wanted: Vec<Settings> = config.enabled_extensions().map(Settings::from).collect();
        let mut lines = Vec::new();

        // What must stop first, so that an extension that changed does not run twice.
        let mut stopping = Vec::new();
        let mut changed = Vec::new();
        {
            let mut started = self.started.lock().unwrap_or_else(|e| e.into_inner());
            for (link, thread) in std::mem::take(&mut *started) {
                let name = link.name().to_owned();
                match wanted.iter().find(|w| w.name == name) {
                    Some(w) if *w == link.settings => started.push((link, thread)),
                    Some(_) => {
                        changed.push(name.clone());
                        lines.push(format!(
                            "extension \"{name}\" changed: its connection is closed and opened again"
                        ));
                        stopping.push((link, thread));
                    }
                    None => {
                        let why = if config.extensions.iter().any(|e| e.name == name) {
                            "disabled"
                        } else {
                            "removed"
                        };
                        lines.push(format!(
                            "extension \"{name}\" {why}: its connection is closed"
                        ));
                        stopping.push((link, thread));
                    }
                }
            }
        }
        for (link, _) in &stopping {
            link.stop();
        }
        for (_, thread) in stopping {
            if let Some(thread) = thread {
                let _ = thread.join();
            }
        }

        for settings in wanted {
            if self.links().iter().any(|l| l.name() == settings.name) {
                continue;
            }
            let (name, address) = (settings.name.clone(), settings.address.clone());
            match self.start(settings) {
                Ok(_) => {
                    // A changed extension was told of above.
                    if !changed.contains(&name) {
                        lines.push(format!(
                            "extension \"{name}\" added: connecting to {address}"
                        ));
                    }
                }
                Err(e) => lines.push(format!("extension \"{name}\" not started: {e}")),
            }
        }
        // What was resolved holds links: those this reload stopped are let go with it.
        self.forget_words();
        lines
    }

    /// Gives the orders that read the facts they tell (the players, the servers, the queue). Until it is called they
    /// answer that they are not available.
    pub fn serve_facts(&self, facts: Arc<dyn Facts>) {
        self.services.set_facts(facts);
    }

    /// Gives the orders that act what they act on (tell a player, show a line to many). Until it is called they answer
    /// that they are not available.
    pub fn serve_actions(&self, actions: Arc<dyn Actions>) {
        self.services.set_actions(actions);
    }

    /// How many configurations the registry has followed (see the field).
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    /// The link of the extension called `name`, if it is running.
    pub fn link(&self, name: &str) -> Option<Arc<Link>> {
        self.started
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|(link, _)| link.name() == name)
            .map(|(link, _)| Arc::clone(link))
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
        self.forget_words();
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
