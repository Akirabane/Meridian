//! `from-servers`: a ban, a pardon or a whitelist addition made on a server reaches the
//! proxy's lists (docs/OPERATING.md, section 5.12).
//!
//! Each sidecar is asked, over a connection of its own kept between rounds, for what its
//! server announced about its lists (`access-notices`), with a wait the sidecar holds the
//! request for. The proxy remembers the number of the last notice it took, so a notice is
//! never taken twice. When the sidecar has connected to the server again since (its count
//! of connections moved), when it lost notices or when it started over, and at the first
//! answer, the proxy reads the three lists the server holds and takes what it does not
//! have. That reading never removes anything.
//!
//! Only what the server changes is followed: a ban the proxy sent is not taken back
//! ([`super::events::SOURCE`]), and nothing the proxy takes is sent on to the other servers.
//!
//! The same rounds read the servers' operators when `[operators] from-servers` is on
//! (docs/OPERATING.md, section 5.9). They are reflected, not followed: the list of a server
//! replaces what was known of it, whenever the server is read again (the first answer, a
//! reconnection, lost notices) and whenever it announces that an operator was added or removed.
//! A server that cannot be read for `stale-after-seconds` has no operators.
use super::events::{self, Event};
use crate::access::{enforce, not_saved};
use crate::log::{info, warning};
use crate::session::Sessions;
use meridian_config::{Config, Sidecar};
use meridian_sidecar::client::SidecarClient;
use meridian_sidecar::lists::AccessReport;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long a sidecar holds a request for a notice before it answers that there is none.
const WAIT: Duration = Duration::from_secs(2);

/// How long a connection to a sidecar waits for an answer: longer than the sidecar waits
/// for the server's own (`meridian_sidecar::lists::CALL_TIMEOUT`).
const BUDGET: Duration = Duration::from_secs(15);

/// The wait after the first failure, and the longest it grows to.
const FIRST_RETRY: Duration = Duration::from_secs(1);
const LONGEST_RETRY: Duration = Duration::from_secs(30);

/// How long a sidecar that does not listen, or a server it is not connected to, is left
/// alone before it is asked again.
const NOT_LISTENING_RETRY: Duration = Duration::from_secs(30);
const NOT_CONNECTED_RETRY: Duration = Duration::from_secs(1);

/// How often the stop flag is looked at, and the least time between two rounds.
const TICK: Duration = Duration::from_millis(100);

/// The three lists of a server, as its management protocol gives them.
#[derive(Clone, Debug, PartialEq)]
pub struct State {
    pub bans: Value,
    pub ip_bans: Value,
    pub allowlist: Value,
}

/// What a sidecar is asked through: the connection to it, in tests a stand-in.
pub trait Source: Send {
    /// The changes of the lists after number `after`, or only where the count stands.
    fn report(&mut self, after: Option<u64>) -> Result<AccessReport, String>;
    /// The lists the server holds now.
    fn state(&mut self) -> Result<State, String>;
    /// The operators the server holds now, as its `operators` gives them.
    fn operators(&mut self) -> Result<Value, String> {
        Ok(Value::Array(Vec::new()))
    }
}

/// What a round reads from a server: the bans and the whitelist (`[access.sync] from-servers`),
/// its operators (`[operators] from-servers`, for a server that counts).
#[derive(Clone, Copy, Debug)]
struct Wants {
    bans: bool,
    operators: bool,
}

impl Wants {
    /// What is not followed when the sidecar cannot be listened to.
    fn what(self) -> &'static str {
        match (self.bans, self.operators) {
            (true, true) => "its bans and its operators are",
            (true, false) => "its bans are",
            _ => "its operators are",
        }
    }

    /// The setting that asked.
    fn setting(self) -> &'static str {
        match (self.bans, self.operators) {
            (true, true) => "[access.sync] and [operators] from-servers are",
            (true, false) => "[access.sync] from-servers is",
            _ => "[operators] from-servers is",
        }
    }
}

/// How a sidecar is reached.
pub type Open<'a> = dyn Fn(&Sidecar) -> Result<Box<dyn Source>, String> + Send + Sync + 'a;

struct Connection {
    client: SidecarClient,
}

impl Source for Connection {
    fn report(&mut self, after: Option<u64>) -> Result<AccessReport, String> {
        self.client
            .access_notices(after, WAIT)
            .map_err(|e| e.to_string())
    }

    fn state(&mut self) -> Result<State, String> {
        let mut read = |method: &str| {
            self.client
                .manage(method, Value::Null)
                .map_err(|e| format!("{method}: {e}"))
        };
        Ok(State {
            bans: read("bans")?,
            ip_bans: read("ip_bans")?,
            allowlist: read("allowlist")?,
        })
    }

    fn operators(&mut self) -> Result<Value, String> {
        self.client
            .manage("operators", Value::Null)
            .map_err(|e| format!("operators: {e}"))
    }
}

/// Connects to the sidecar over TCP with the secret of its file.
pub fn open_sidecar(sidecar: &Sidecar) -> Result<Box<dyn Source>, String> {
    let secret = sidecar.read_secret()?;
    let client = SidecarClient::connect(&sidecar.address, &secret, BUDGET)
        .map_err(|e| format!("cannot reach it at {}: {e}", sidecar.address))?;
    Ok(Box::new(Connection { client }))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Said {
    #[default]
    Nothing,
    Unreachable,
    NotListening,
    NotConnected,
    Failed,
    Hearing,
}

/// What one sidecar's listener remembers between rounds.
#[derive(Default)]
struct Slot {
    source: Option<Box<dyn Source>>,
    /// The number of the last notice taken; none before the first answer.
    after: Option<u64>,
    /// The connection of the sidecar to the server that the lists were read for.
    connection: Option<u64>,
    /// The failures in a row, which set the wait.
    failures: u32,
    /// Not asked before this.
    retry_at: Option<Instant>,
    /// The state that was said last, so that it is said once.
    said: Said,
    /// Whether the operators of the server were read since the sidecar was last connected to
    /// it; read again at once when not.
    ops_read: bool,
    /// The failures in a row of reading the operators, which set the wait.
    ops_failures: u32,
    ops_retry_at: Option<Instant>,
    /// How many operators were said last, so that a change is said once.
    ops_said: Option<usize>,
}

/// The listeners of every sidecar that was asked, by backend.
#[derive(Default)]
pub struct Hearers {
    slots: Mutex<HashMap<String, Slot>>,
}

impl Hearers {
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
pub(super) fn retry_after(failures: u32) -> Duration {
    FIRST_RETRY
        .saturating_mul(1u32 << failures.saturating_sub(1).min(5))
        .min(LONGEST_RETRY)
}

/// One round: every sidecar is asked, all at once, each over its own connection, and what
/// its server announced is applied to the proxy's lists, and its operators are read. Nothing is
/// asked while both `from-servers` are off.
pub fn round(config: &Config, open: &Open<'_>, hearers: &Hearers, sessions: &Sessions) {
    let bans = config.access.sync.from_servers;
    let operators = config.operators.from_servers;
    sessions.operators.configure(&config.operators);
    if !bans && !operators {
        hearers.retain(|_| false);
        return;
    }
    hearers.retain(|b| config.sidecars.iter().any(|s| s.backend == b));
    std::thread::scope(|scope| {
        for sidecar in &config.sidecars {
            let wants = Wants {
                bans,
                operators: operators && config.operators.counts(&sidecar.backend),
            };
            if !wants.bans && !wants.operators {
                continue;
            }
            scope.spawn(move || {
                let mut slot = hearers.take(&sidecar.backend);
                ask(sidecar, wants, open, &mut slot, sessions);
                hearers.put(&sidecar.backend, slot);
            });
        }
    });
}

/// Says `what` about `slot` once, until its state changes.
fn say(slot: &mut Slot, said: Said, line: impl FnOnce() -> String) {
    if slot.said != said {
        let line = line();
        match said {
            Said::Hearing => info!("{line}"),
            _ => warning!("{line}"),
        }
        slot.said = said;
    }
}

/// A failure: the sidecar is left alone for a wait that grows.
fn failed(slot: &mut Slot, said: Said, line: impl FnOnce() -> String) {
    slot.source = None;
    slot.failures = slot.failures.saturating_add(1);
    slot.retry_at = Some(Instant::now() + retry_after(slot.failures));
    say(slot, said, line);
}

/// Asks one sidecar once and applies the answer.
fn ask(sidecar: &Sidecar, wants: Wants, open: &Open<'_>, slot: &mut Slot, sessions: &Sessions) {
    let backend = sidecar.backend.as_str();
    if slot.retry_at.is_some_and(|at| Instant::now() < at) {
        return;
    }
    if slot.source.is_none() {
        match open(sidecar) {
            Ok(source) => slot.source = Some(source),
            Err(why) => {
                failed(slot, Said::Unreachable, || {
                    format!(
                        "cannot ask the sidecar of backend \"{backend}\" for the changes of its \
                         server's lists: {why}; trying again"
                    )
                });
                return;
            }
        }
    }
    let Some(source) = slot.source.as_mut() else {
        return;
    };
    let report = match source.report(slot.after) {
        Ok(report) => report,
        Err(why) => {
            failed(slot, Said::Unreachable, || {
                format!(
                    "cannot ask the sidecar of backend \"{backend}\" for the changes of its \
                     server's lists: {why}; trying again"
                )
            });
            return;
        }
    };
    slot.failures = 0;
    slot.retry_at = None;
    if !report.enabled {
        slot.retry_at = Some(Instant::now() + NOT_LISTENING_RETRY);
        say(slot, Said::NotListening, || {
            format!(
                "{} on, but the sidecar of backend \"{backend}\" does \
                 not listen to the server's management protocol (start it with \
                 --management-properties): {} not followed",
                wants.setting(),
                wants.what()
            )
        });
        return;
    }
    if !report.watching {
        slot.retry_at = Some(Instant::now() + NOT_CONNECTED_RETRY);
        say(slot, Said::NotConnected, || {
            format!(
                "the sidecar of backend \"{backend}\" is not connected to the server's \
                 management protocol (it is off on the server, or the server is down): {} not \
                 followed while it is not",
                wants.what()
            )
        });
        slot.ops_read = false;
        return;
    }
    // A server heard connected is believed for another while; a read below may replace it.
    if wants.operators {
        sessions.operators.heard(backend, Instant::now());
    } else {
        slot.ops_read = false;
    }
    let behind_the_server = slot.after.is_none_or(|after| report.last < after);
    let reconnected = slot.connection != Some(report.connection);
    let resync = behind_the_server || reconnected || report.missed;
    if wants.bans && resync {
        let state = match source.state() {
            Ok(state) => state,
            Err(why) => {
                failed(slot, Said::Failed, || {
                    format!(
                        "cannot read the lists of backend \"{backend}\"'s server: {why}; trying \
                         again"
                    )
                });
                return;
            }
        };
        let events = events::state(&state.bans, &state.ip_bans, &state.allowlist);
        let lines = apply_all(sessions, backend, events);
        if !lines.is_empty() {
            info!(
                "read the lists of backend \"{backend}\"'s server: {} taken",
                lines.len()
            );
        }
    }
    if wants.bans {
        for notice in &report.notices {
            if let Some(event) = events::parse_notice(&notice.line) {
                apply(sessions, backend, event);
            }
        }
    }
    // An operator added or removed: the list is read again, whatever the notice carried.
    let operators_changed = wants.operators
        && report
            .notices
            .iter()
            .any(|notice| events::is_operator_notice(&notice.line));
    let waiting = slot.ops_retry_at.is_some_and(|at| Instant::now() < at);
    if wants.operators && (resync || operators_changed || !slot.ops_read) && !waiting {
        let read = source.operators().and_then(|list| {
            events::operators(&list).ok_or_else(|| "the answer is not a list".to_owned())
        });
        settle_operators(backend, read, slot, sessions);
    }
    slot.connection = Some(report.connection);
    slot.after = Some(report.last);
    say(slot, Said::Hearing, || {
        format!("hearing the changes of the lists of backend \"{backend}\"'s server")
    });
}

/// Puts the operators just read from `backend`'s server in place of what was known. A failure
/// leaves what was known (it ages), says so once, and is tried again after a wait that grows.
fn settle_operators(
    backend: &str,
    read: Result<Vec<crate::operators::Op>, String>,
    slot: &mut Slot,
    sessions: &Sessions,
) {
    match read {
        Ok(ops) => {
            let count = ops.len();
            sessions.operators.replace(backend, ops, Instant::now());
            slot.ops_read = true;
            slot.ops_failures = 0;
            slot.ops_retry_at = None;
            if slot.ops_said != Some(count) {
                info!("read the operators of backend \"{backend}\"'s server: {count}");
                slot.ops_said = Some(count);
            }
        }
        Err(why) => {
            slot.ops_failures = slot.ops_failures.saturating_add(1);
            slot.ops_retry_at = Some(Instant::now() + retry_after(slot.ops_failures));
            if slot.ops_failures == 1 {
                warning!(
                    "cannot read the operators of backend \"{backend}\"'s server: {why}; trying \
                     again"
                );
            }
        }
    }
}

/// Applies `events` in order, and returns a line for each that changed the proxy's lists.
pub fn apply_all(sessions: &Sessions, server: &str, events: Vec<Event>) -> Vec<String> {
    events
        .into_iter()
        .flat_map(|event| apply(sessions, server, event))
        .collect()
}

/// Applies one change of `server`'s lists to the proxy's, and returns what it did, if it
/// did anything. The ban a pardon lifts is the one that was taken from that server.
pub fn apply(sessions: &Sessions, server: &str, event: Event) -> Option<String> {
    let line = match event {
        Event::Skip(why) => {
            let line = format!("backend \"{server}\": {why}");
            warning!("{line}");
            return Some(line);
        }
        Event::Ban { who, reason } => {
            let reason = events::mark(server, reason.as_deref());
            let (made, saved) = sessions.access.change(|access| {
                let made = access.ban(who.clone(), Some(reason.clone()));
                (made, made)
            });
            if !made {
                return None;
            }
            report_saved(saved);
            let line = format!("banned {who}: {reason}");
            info!("{line}");
            // The player may be on another server now.
            enforce(sessions);
            line
        }
        Event::Pardon { who } => {
            let (lifted, saved) = sessions.access.change(|access| {
                let entry = access
                    .bans()
                    .iter()
                    .find(|ban| {
                        who.iter().any(|w| ban.target.same(w))
                            && ban
                                .reason
                                .as_deref()
                                .is_some_and(|r| events::is_marked(server, r))
                    })
                    .map(|ban| ban.target.clone());
                match entry {
                    Some(target) => (Some(target.clone()), access.pardon(&target)),
                    None => (None, false),
                }
            });
            let target = lifted?;
            report_saved(saved);
            let line = format!("pardoned {target}: pardoned on {server}");
            info!("{line}");
            line
        }
        Event::Allow { who } => {
            let (added, saved) = sessions.access.change(|access| {
                let added = access.allow(who.clone()) == Ok(true);
                (added, added)
            });
            if !added {
                return None;
            }
            report_saved(saved);
            let line = format!("{who} added to the whitelist: allowed on {server}");
            info!("{line}");
            line
        }
    };
    Some(line)
}

fn report_saved(saved: Result<(), meridian_config::AccessError>) {
    if let Err(e) = saved {
        not_saved(&e);
    }
}

/// Starts the thread that asks the sidecars until the proxy stops. The configuration is read
/// again for each round, so a reload applies to the next one.
pub fn spawn(proxy: &Arc<crate::state::Proxy>) -> std::io::Result<std::thread::JoinHandle<()>> {
    let proxy = Arc::clone(proxy);
    std::thread::Builder::new()
        .name("bans from servers".into())
        .spawn(move || {
            let hearers = Hearers::default();
            while !proxy.stopping() {
                let begun = Instant::now();
                round(&proxy.config(), &open_sidecar, &hearers, &proxy.sessions);
                // Never a tight loop: a round that answered at once is followed by a pause.
                let until = begun + TICK;
                while !proxy.stopping() && Instant::now() < until {
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        })
}
