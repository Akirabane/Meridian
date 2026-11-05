//! The bans and the whitelist kept in step with the servers' own (docs/OPERATING.md,
//! section 5.10), in two directions, both off (`[access.sync]`):
//!
//! - `from-servers` ([`hear`]): what is banned, pardoned or allowed on a server reaches the
//!   proxy's lists, so that a player banned on one server cannot join another through the
//!   proxy;
//! - `to-servers` ([`push`]): what the console bans, pardons or adds to the whitelist is sent
//!   to every server whose sidecar answers.
//!
//! Both go through the sidecars, which are clients of the servers' management protocol
//! (`meridian-sidecar`, `lists.rs`). There is no loop: the console decides what is sent, a
//! change that comes from a server is never sent on, and a change that is already in the lists
//! is no change.
use crate::state::Proxy;
use std::sync::Arc;

pub mod events;
pub mod hear;
pub mod push;

pub use push::{Change, Outbox};

/// Tells the servers of a change the console just made, if `to-servers` is on, and returns the
/// line to show the operator. Never waits for a server.
pub fn send(proxy: &Proxy, change: &Change) -> Vec<String> {
    let config = proxy.config();
    if !config.access.sync.to_servers {
        return Vec::new();
    }
    let backends: Vec<String> = config.sidecars.iter().map(|s| s.backend.clone()).collect();
    if backends.is_empty() {
        return vec!["no backend has a sidecar: nothing to send to the servers".to_owned()];
    }
    match proxy.access_sync.send(&backends, change) {
        Ok(()) => vec![format!(
            "sending {} to the servers ({}); one that cannot be reached is tried again",
            change.describe(),
            backends.join(", ")
        )],
        Err(why) => vec![format!("not sent to the servers: {why}")],
    }
}

/// Starts the two threads. Each does nothing while its direction is off.
pub fn spawn(proxy: &Arc<Proxy>) -> std::io::Result<()> {
    hear::spawn(proxy)?;
    push::spawn(proxy)?;
    Ok(())
}

#[cfg(test)]
mod tests;
