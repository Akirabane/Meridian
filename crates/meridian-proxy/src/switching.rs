//! Moving a player to another backend while they stay connected: the console's
//! `switch` (and `send`), and `/server` and `/hub` in game. The switch itself is
//! `meridian-handoff`'s; this module finds the sidecars of the two backends in the
//! configuration, reaches them, and says how it went, step by step, in the log.
use crate::log::{info, warning};
use meridian_config::{Config, Sidecar};
use meridian_handoff::{Digests, Done, PlayerControl, SessionHandle, Settings, TargetBackend};
use meridian_net::backend::BackendOptions;
use meridian_sidecar::client::SidecarClient;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How the proxy reaches a backend's sidecar: over TCP with its secret, or a
/// stand-in in tests.
pub type Controls =
    dyn Fn(&Sidecar, Duration) -> Result<Box<dyn PlayerControl>, String> + Send + Sync;

/// What every switch shares: how sidecars are reached, and the configuration
/// digest of each backend seen so far.
pub struct Switcher {
    pub controls: Arc<Controls>,
    pub digests: Digests,
}

impl Default for Switcher {
    fn default() -> Self {
        Switcher {
            controls: Arc::new(sidecar_control),
            digests: Digests::default(),
        }
    }
}

/// Connects to `sidecar` with the secret of its file.
fn sidecar_control(sidecar: &Sidecar, timeout: Duration) -> Result<Box<dyn PlayerControl>, String> {
    let secret = sidecar.read_secret()?;
    let client = SidecarClient::connect(&sidecar.address, &secret, timeout)
        .map_err(|e| format!("cannot reach it at {}: {e}", sidecar.address))?;
    Ok(Box::new(client))
}

/// A switch that happened.
#[derive(Debug)]
pub struct Moved {
    pub from: String,
    pub to: String,
    pub done: Done,
    pub took: Duration,
}

impl Moved {
    /// One line for the console and the log, with the bytes and times.
    pub fn describe(&self, name: &str) -> String {
        let [prepare, freeze, hand_over, commit] = self.done.times.map(|t| t.as_millis());
        format!(
            "{name} moved from {} to {} in {} ms (prepare {prepare} ms, freeze {freeze} ms, \
             hand over {hand_over} ms, commit {commit} ms): {} packets, {} bytes \
             ({} chunks, {} entities)",
            self.from,
            self.to,
            self.took.as_millis(),
            self.done.delta_packets + self.done.extras,
            self.done.delta_bytes,
            self.done.chunks,
            self.done.entities,
        )
    }
}

/// Moves the player of `handle`, called `name`, to the backend `target`. The
/// error is one line that says at which step it failed and where the player is.
pub fn switch(
    config: &Config,
    switcher: &Switcher,
    handle: &SessionHandle,
    name: &str,
    player_address: IpAddr,
    target: &str,
) -> Result<Moved, String> {
    let from = handle.backend();
    let failed = |why: String| format!("{name} could not move from {from} to {target}: {why}");
    let Some(backend) = config.backend(target) else {
        return Err(failed(format!("check: there is no server called {target}")));
    };
    if from == backend.name {
        return Err(format!("{name} is already on {target}"));
    }
    // One switch at a time: the switch itself refuses a second one, but saying so
    // here spares the sidecars a request.
    if handle.is_switching() {
        return Err(failed(
            "check: a switch of this player is already under way".to_owned(),
        ));
    }
    let sidecar = |of: &str| {
        config
            .sidecars
            .iter()
            .find(|s| s.backend == of)
            .ok_or_else(|| {
                failed(format!(
                    "check: {of} has no sidecar in the configuration, so no player can be \
                     switched to or from it"
                ))
            })
    };
    let (source_sidecar, target_sidecar) = (sidecar(&from)?, sidecar(&backend.name)?);
    let timeout = Duration::from_millis(config.sidecar_timeout_ms);
    let reach = |s: &Sidecar| {
        (switcher.controls)(s, timeout)
            .map_err(|e| failed(format!("check: the sidecar of {}: {e}", s.backend)))
    };
    let mut source = reach(source_sidecar)?;
    let mut target_control = reach(target_sidecar)?;
    let options = BackendOptions {
        forwarding_secret: config.forwarding_secret.as_bytes().to_vec(),
        connect_timeout: Duration::from_millis(config.connect_timeout_ms),
        player_address,
    };
    let (who, to) = (name.to_owned(), backend.name.clone());
    let settings = Settings {
        trace: Some(Arc::new(move |step: &str, at: Duration| {
            info!("{who}: switch to {to}: {step} (+{} ms)", at.as_millis());
        })),
        carry_position: config.switch_carry_position,
        ..Settings::default()
    };
    info!("{name}: switch from {from} to {}: starting", backend.name);
    let started = Instant::now();
    let done = meridian_handoff::switch(
        handle,
        TargetBackend {
            name: &backend.name,
            address: &backend.address,
            options: &options,
        },
        source.as_mut(),
        target_control.as_mut(),
        &switcher.digests,
        &settings,
    )
    .map_err(|failure| failed(failure.to_string()))?;
    Ok(Moved {
        from,
        to: backend.name.clone(),
        done,
        took: started.elapsed(),
    })
}

/// A switch asked for in game: the player is told it started, and how it ended;
/// the log gets the same line as the console's.
pub fn in_game(
    config: Arc<Config>,
    switcher: Arc<Switcher>,
    handle: SessionHandle,
    name: String,
    player_address: IpAddr,
    target: String,
    place: Option<crate::balance::Reservation>,
) {
    let spawned = std::thread::Builder::new()
        .name(format!("switch-{name}"))
        .spawn(move || {
            // The place reserved on the target is kept until the switch has ended: the
            // player counts there from the commit, and it is given back after.
            let _place = place;
            match switch(&config, &switcher, &handle, &name, player_address, &target) {
                Ok(moved) => {
                    info!("{}", moved.describe(&name));
                    handle.tell(&format!("You are now on {}.", moved.to));
                }
                Err(why) => {
                    warning!("{why}");
                    handle.tell(&format!("Could not move you to {target}. {why}"));
                }
            }
        });
    if let Err(e) = spawned {
        warning!("cannot start a thread for a switch: {e}");
    }
}

#[cfg(test)]
mod tests;
