//! Moving a player to another backend while they stay connected: the console's
//! `switch` (and `send`), and `/server` and `/hub` in game. The switch itself is
//! `meridian-handoff`'s; this module finds the sidecars of the two backends in the
//! configuration, reaches them, and says how it went, step by step, in the log.
use crate::log::{error, info, warning};
use crate::metrics::{self, Metrics};
use meridian_config::{Config, Sidecar};
use meridian_handoff::{Digests, Done, PlayerControl, SessionHandle, Settings, TargetBackend};
use meridian_net::backend::BackendOptions;
use meridian_sidecar::client::SidecarClient;
use std::cell::Cell;
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
    /// What the proxy counts, here because every part of the proxy that moves players
    /// reaches the sessions through the switcher (`Sessions::metrics`).
    pub metrics: Arc<Metrics>,
    /// The extensions that are told how each switch ended (docs/EXTENSIONS.md, section 6): the registry of the sessions, given once
    /// when the proxy is built. Until it is, nobody is told.
    pub extensions: std::sync::OnceLock<Arc<crate::extension::Registry>>,
}

impl Default for Switcher {
    fn default() -> Self {
        Switcher {
            controls: Arc::new(sidecar_control),
            digests: Digests::default(),
            metrics: Arc::default(),
            extensions: std::sync::OnceLock::new(),
        }
    }
}

impl Switcher {
    /// Tells the extensions that listen that `make()` happened, if there is a registry to tell. Never waits.
    fn emit(&self, make: impl FnOnce() -> crate::extension::events::Kind) {
        if let Some(extensions) = self.extensions.get() {
            extensions.emit(make);
        }
    }

    /// Whether the player was let in as a verified account.
    fn verified(&self, uuid: [u8; 16]) -> bool {
        self.extensions
            .get()
            .is_some_and(|extensions| extensions.verified(uuid))
    }
}

/// The step a failed switch is told to have failed at: the words of the metrics, as the events know them.
fn failed_step(label: &str) -> meridian_extension::events::FailedStep {
    use meridian_extension::events::FailedStep;
    match label {
        "prepare" => FailedStep::Prepare,
        "freeze" => FailedStep::Freeze,
        "hand_over" => FailedStep::HandOver,
        "commit" => FailedStep::Commit,
        _ => FailedStep::Check,
    }
}

/// Connects to `sidecar` with the secret of its file.
fn sidecar_control(sidecar: &Sidecar, timeout: Duration) -> Result<Box<dyn PlayerControl>, String> {
    let secret = sidecar.read_secret()?;
    let client = SidecarClient::connect(&sidecar.address, &secret, timeout)
        .map_err(|e| format!("cannot reach it at {}: {e}", sidecar.address))?;
    Ok(Box::new(client))
}

/// How long Horizon lets a player stay frozen by default (`max_freeze_seconds` of a
/// server's `[transfer]`). The proxy cannot read a server's setting, so this is what
/// it warns about.
const HORIZON_FREEZE_MS: u64 = 30_000;

/// What the operator should know about the configuration before any switch, one
/// line each: the freeze limit must stay below what the servers allow.
pub fn startup_warnings(config: &Config) -> Vec<String> {
    let mut warnings = Vec::new();
    if !config.sidecars.is_empty() && config.switch_freeze_limit_ms >= HORIZON_FREEZE_MS {
        warnings.push(format!(
            "switch-freeze-limit-ms is {} ms, but a Horizon server lets a frozen player go by \
             itself after its max_freeze_seconds ({} s by default): a switch gives up at the \
             limit only if the server's is longer. Lower the limit, or raise max_freeze_seconds \
             in [transfer] on every server that has a sidecar. Each switch also reads the \
             freeze its server says is left, and gives up before it ends",
            config.switch_freeze_limit_ms,
            HORIZON_FREEZE_MS / 1_000,
        ));
    }
    warnings
}

/// A switch that happened.
#[derive(Debug)]
pub struct Moved {
    pub from: String,
    pub to: String,
    pub done: Done,
    pub took: Duration,
    /// What the operator must know, when the old server would not let the player go
    /// after the new one placed them.
    pub alert: Option<String>,
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
    // The step a failure happened at: anything before the switch itself is a check.
    let step = Cell::new(metrics::step_label(meridian_handoff::Step::Check));
    // A switch is told to the extensions that listen when it was **tried**: to a server the proxy has, other than the one the player is
    // on. A name that is none, or the server they are on, is not a switch, and what a player typed is never in an event.
    let from = handle.backend();
    let attempted = config
        .backend(target)
        .map(|b| b.name.clone())
        .filter(|to| *to != from);
    let result = attempt(
        config,
        switcher,
        handle,
        name,
        player_address,
        target,
        &step,
    );
    match &result {
        Ok(moved) => switcher.metrics.transfer_done(&moved.done, moved.took),
        Err(_) => switcher.metrics.transfer_failed(step.get()),
    }
    if let Some(to) = attempted {
        use meridian_extension::events::MoveOutcome;
        let uuid = handle.profile().uuid;
        switcher.emit(|| crate::extension::events::Kind::Moved {
            player: crate::extension::events::Kind::player(name, uuid, switcher.verified(uuid)),
            from,
            to,
            outcome: if result.is_ok() {
                MoveOutcome::Seamless
            } else {
                MoveOutcome::Failed
            },
            reason: result.is_err().then(|| failed_step(step.get())),
        });
    }
    result
}

fn attempt(
    config: &Config,
    switcher: &Switcher,
    handle: &SessionHandle,
    name: &str,
    player_address: IpAddr,
    target: &str,
    step: &Cell<&'static str>,
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
    // Between two state groups nothing of the player goes: each server keeps its own copy
    // (`state-group`, OPERATING.md 5.15). Whatever started the move.
    let carry_state = config.carries_state(&from, &backend.name);
    let settings = Settings {
        trace: Some(Arc::new(move |step: &str, at: Duration| {
            info!("{who}: switch to {to}: {step} (+{} ms)", at.as_millis());
        })),
        carry_position: config.switch_carry_position,
        carry_state,
        announce_arrival: config.switch_announce_arrival,
        announce_departure: config.switch_announce_departure,
        freeze_limit: Duration::from_millis(config.switch_freeze_limit_ms),
        commit_margin: Duration::from_millis(config.switch_commit_margin_ms),
        ..Settings::default()
    };
    info!(
        "{name}: switch from {from} to {}: starting{}",
        backend.name,
        if carry_state {
            ""
        } else {
            " (other state group: no state is carried, each server keeps its own copy)"
        }
    );
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
    .map_err(|failure| {
        step.set(metrics::step_label(failure.step));
        failed(failure.to_string())
    })?;
    // The new server has the player placed; the old one would not let them go, so it
    // saves its own copy when the proxy closes its connection, and the two servers'
    // files differ. Nothing undoes that: the operator is told, in the log and on the
    // console, with the id both servers carry in their logs.
    let alert = done.source_kept.as_ref().map(|why| {
        let alert = format!(
            "ALERT {name}: transfer {id} from {from} to {to}: {from} would not let the player \
             go ({why}). {name} is on {to}, which placed them and saved their files; {from} \
             keeps its own copy and saves it, so the two servers' files for {name} now differ: \
             compare them and keep the one that is right. Both servers' logs name the transfer \
             {id}",
            id = done.handover,
            to = backend.name,
        );
        error!("{alert}");
        switcher.metrics.alert();
        alert
    });
    Ok(Moved {
        from,
        to: backend.name.clone(),
        done,
        took: started.elapsed(),
        alert,
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
