//! What a backend tells of its own stop (docs/RESTART.md). A Horizon server pushes
//! `server/stopping` and `server/started` on its management protocol; the sidecar next
//! to it listens, and the proxy asks the sidecar for what it heard, over the signed
//! exchange (the `notices` request). What arrives goes to the restart detector
//! ([`Detector::handle_notification`]), which stops offering the backend and hands its
//! players to the evacuation.
//!
//! This is off unless a backend has `sidecar-notices = true`, and it is only asked of
//! backends with a role, as the detector is only told of those.
//!
//! Each such sidecar is asked over a connection of its own, kept between rounds, with a
//! wait the sidecar holds the request for, so that a notice comes within a moment of
//! being heard and the exchange is quiet when there is none. The proxy remembers the
//! number of the last notice it took, so an answer that was lost loses nothing, and a
//! notice is never taken twice. A sidecar that cannot be reached is tried again after a
//! wait that grows to half a minute, and said once, not at every try.
use crate::log::{info, warning};
use crate::restart::Detector;
use crate::state::Proxy;
use meridian_config::{Config, Sidecar};
use meridian_sidecar::client::SidecarClient;
use meridian_sidecar::request::NoticeReport;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long a sidecar holds a request for a notice before it answers that there is none.
const WAIT: Duration = Duration::from_secs(2);

/// A notice heard longer ago than this is not acted on: the backend has long since
/// stopped or come back, and the other ways of telling (its sidecar not answering) say
/// what is true now.
const STALE: Duration = Duration::from_secs(120);

/// The wait after the first failure, and the longest it grows to.
const FIRST_RETRY: Duration = Duration::from_secs(1);
const LONGEST_RETRY: Duration = Duration::from_secs(30);

/// How long the sidecar that does not listen is left alone before it is asked again: it
/// is set up by hand, and may be restarted with the option.
const NOT_LISTENING_RETRY: Duration = Duration::from_secs(30);

/// How often the stop flag is looked at, and the least time between two rounds.
const TICK: Duration = Duration::from_millis(100);

/// What a sidecar is asked for notices through: the connection to it, in tests a
/// stand-in.
pub trait NoticeSource: Send {
    /// The notices after number `after`, or only where the count stands without it.
    fn notices(&mut self, after: Option<u64>) -> Result<NoticeReport, String>;
}

/// How a sidecar is reached for notices, given the time one request may take on top of
/// the wait the sidecar holds it for.
pub type Open<'a> =
    dyn Fn(&Sidecar, Duration) -> Result<Box<dyn NoticeSource>, String> + Send + Sync + 'a;

struct Connection {
    client: SidecarClient,
}

impl NoticeSource for Connection {
    fn notices(&mut self, after: Option<u64>) -> Result<NoticeReport, String> {
        self.client.notices(after, WAIT).map_err(|e| e.to_string())
    }
}

/// Connects to the sidecar over TCP with the secret of its file. The connection waits
/// for an answer longer than the sidecar holds a request.
pub fn open_sidecar(sidecar: &Sidecar, timeout: Duration) -> Result<Box<dyn NoticeSource>, String> {
    let secret = sidecar.read_secret()?;
    let client = SidecarClient::connect(&sidecar.address, &secret, timeout + WAIT)
        .map_err(|e| format!("cannot reach it at {}: {e}", sidecar.address))?;
    Ok(Box::new(Connection { client }))
}

/// What one sidecar's listener remembers between rounds.
#[derive(Default)]
struct Slot {
    source: Option<Box<dyn NoticeSource>>,
    /// The number of the last notice taken; none before the first answer.
    after: Option<u64>,
    /// The failures in a row, which set the wait.
    failures: u32,
    /// Not asked before this.
    retry_at: Option<Instant>,
    /// The state that was said last, so that it is said once.
    said: Said,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Said {
    #[default]
    Nothing,
    Unreachable,
    NotListening,
    NotConnected,
    Hearing,
}

/// The listeners of every sidecar that was asked, by backend.
#[derive(Default)]
pub struct Listeners {
    slots: Mutex<HashMap<String, Slot>>,
}

impl Listeners {
    fn take(&self, backend: &str) -> Slot {
        self.slots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(backend)
            .unwrap_or_default()
    }

    fn put(&self, backend: &str, slot: Slot) {
        self.slots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(backend.to_owned(), slot);
    }

    fn retain(&self, mut keep: impl FnMut(&str) -> bool) {
        self.slots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|b, _| keep(b));
    }
}

/// The wait after `failures` failures in a row.
fn retry_after(failures: u32) -> Duration {
    FIRST_RETRY
        .saturating_mul(1u32 << failures.saturating_sub(1).min(5))
        .min(LONGEST_RETRY)
}

/// One round: every sidecar of a backend with a role that is set to give notices is
/// asked, all at once, each over its own connection, and what it heard goes to the
/// detector. `now` is when the notices are acted on.
pub fn round(
    config: &Config,
    open: &Open<'_>,
    listeners: &Listeners,
    detector: &Detector,
    now: Instant,
) {
    let timeout = Duration::from_millis(config.sidecar_timeout_ms);
    let asked = |s: &&Sidecar| s.notices && config.balanced(&s.backend).is_some();
    listeners.retain(|b| config.sidecars.iter().any(|s| s.backend == b && asked(&s)));
    std::thread::scope(|scope| {
        for sidecar in config.sidecars.iter().filter(asked) {
            scope.spawn(move || {
                let mut slot = listeners.take(&sidecar.backend);
                ask(sidecar, timeout, open, &mut slot, detector, now);
                listeners.put(&sidecar.backend, slot);
            });
        }
    });
}

/// Asks one sidecar once and acts on the answer.
fn ask(
    sidecar: &Sidecar,
    timeout: Duration,
    open: &Open<'_>,
    slot: &mut Slot,
    detector: &Detector,
    now: Instant,
) {
    let backend = sidecar.backend.as_str();
    if slot.retry_at.is_some_and(|at| Instant::now() < at) {
        return;
    }
    let answer = match slot.source.as_mut() {
        Some(source) => source.notices(slot.after),
        None => match open(sidecar, timeout) {
            Ok(mut source) => {
                let answer = source.notices(slot.after);
                slot.source = Some(source);
                answer
            }
            Err(why) => Err(why),
        },
    };
    let report = match answer {
        Ok(report) => report,
        Err(why) => {
            // The connection is not used again: a new one answers the next try.
            slot.source = None;
            slot.failures = slot.failures.saturating_add(1);
            slot.retry_at = Some(Instant::now() + retry_after(slot.failures));
            if slot.said != Said::Unreachable {
                warning!(
                    "cannot ask the sidecar of backend \"{backend}\" what its server announces: \
                     {why}; trying again"
                );
                slot.said = Said::Unreachable;
            }
            return;
        }
    };
    slot.failures = 0;
    slot.retry_at = None;
    if !report.enabled {
        slot.retry_at = Some(Instant::now() + NOT_LISTENING_RETRY);
        if slot.said != Said::NotListening {
            warning!(
                "backend \"{backend}\" has sidecar-notices, but its sidecar does not listen to the \
                 server's management protocol (start it with --management-properties): a stop of \
                 the server will not be announced"
            );
            slot.said = Said::NotListening;
        }
        return;
    }
    if !report.watching {
        if slot.said != Said::NotConnected {
            warning!(
                "the sidecar of backend \"{backend}\" is not connected to the server's \
                 management protocol (it is off on the server, or the server is down): a stop of \
                 the server is not announced while it is not"
            );
            slot.said = Said::NotConnected;
        }
    } else if slot.said != Said::Hearing {
        if slot.said != Said::Nothing {
            info!("the sidecar of backend \"{backend}\" hears the server's management protocol");
        }
        slot.said = Said::Hearing;
    }
    let was = slot.after;
    slot.after = Some(match was {
        // The first answer: where the count stands. What was heard before the proxy
        // asked is not replayed.
        None => report.last,
        // The sidecar began its count again: it was restarted.
        Some(after) if report.last < after => report.last,
        Some(after) => after,
    });
    for notice in report.notices {
        if slot.after.is_some_and(|after| notice.seq <= after) {
            continue;
        }
        slot.after = Some(notice.seq);
        if notice.age > STALE {
            continue;
        }
        detector.handle_notification(backend, &notice.line, now);
    }
}

/// Starts the thread that asks the sidecars for notices until the proxy stops. The
/// configuration is read again for each round, so a reload applies to the next one. It
/// does nothing while `[balancing] enabled` is off, as the detector then has nothing to
/// stop offering.
pub fn spawn(proxy: &Arc<Proxy>) -> std::io::Result<std::thread::JoinHandle<()>> {
    let proxy = Arc::clone(proxy);
    std::thread::Builder::new()
        .name("backend notices".into())
        .spawn(move || {
            let listeners = Listeners::default();
            while !proxy.stopping() {
                let config = proxy.config();
                let begun = Instant::now();
                if config.balancing.enabled {
                    round(
                        &config,
                        &open_sidecar,
                        &listeners,
                        &proxy.restarts,
                        Instant::now(),
                    );
                }
                // Never a tight loop: a round that answered at once is followed by a pause.
                let until = begun + TICK;
                while !proxy.stopping() && Instant::now() < until {
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        })
}

#[cfg(test)]
mod tests;
