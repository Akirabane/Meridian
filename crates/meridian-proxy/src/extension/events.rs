//! The events the proxy tells the extensions granted `events` (docs/EXTENSIONS.md, section 6).
//!
//! **Nothing here waits for an extension.** An event is a change of state of a session (a join, a leave...), never traffic, and
//! [`Bus::emit`] is called by the thread that changed it: with nobody listening it is **one relaxed load** (the event is described by a
//! closure that is not called); with listeners it makes the event once, and hands it to each connection's own bounded queue with a
//! `try_send`, which never blocks. Each connection has a thread of its own that writes its events (apart from the one that writes the
//! decisions), so that an extension that does not read holds that thread and nobody else.
//!
//! What is promised, and what is not: at most once, in order for one connection, never replayed (a queue is emptied when its
//! connection ends, and nothing is queued for an extension that is not connected). When the queue of a connection is full the **newest**
//! event is dropped and counted, and the next event written is preceded by `events/dropped` with the count. `seq` is taken for every event
//! offered to a connection, the dropped ones included, so the extension sees a loss as a gap.

use super::link::Link;
use super::stats::Stats;
use crate::log::warning;
use meridian_extension::decide::{Auth, Player};
use meridian_extension::events::{
    Dropped, EVENTS_DROPPED, Joined, Left, LeftReason, PLAYERS_JOINED, PLAYERS_LEFT,
};
use meridian_extension::wire;
use meridian_websocket::Writer;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// What happened, as the proxy knows it: each connection writes it with its own `seq`, and with the address only if it is granted
/// `addresses`.
#[derive(Debug)]
pub enum Kind {
    Joined {
        player: Player,
        server: String,
        /// Where the player connected from: written only to a connection granted `addresses`.
        address: String,
    },
    Left {
        player: Player,
        server: String,
        reason: LeftReason,
    },
}

impl Kind {
    /// The player as an event tells them: name, unique id, and how the account was checked.
    pub fn player(name: &str, uuid: [u8; 16], verified: bool) -> Player {
        Player {
            name: name.to_owned(),
            uuid: crate::session::hyphenated(uuid),
            auth: if verified {
                Auth::Verified
            } else {
                Auth::Unverified
            },
        }
    }

    /// The name of the event for the counts and the metrics.
    pub fn label(&self) -> &'static str {
        match self {
            Kind::Joined { .. } => "players/joined",
            Kind::Left { .. } => "players/left",
        }
    }

    /// The notification, as the text of one message.
    fn render(&self, seq: u64, at: u64, with_address: bool) -> String {
        match self {
            Kind::Joined {
                player,
                server,
                address,
            } => wire::notification(
                PLAYERS_JOINED,
                &Joined {
                    seq,
                    at,
                    player: player.clone(),
                    server: server.clone(),
                    address: with_address.then(|| address.clone()),
                },
            ),
            Kind::Left {
                player,
                server,
                reason,
            } => wire::notification(
                PLAYERS_LEFT,
                &Left {
                    seq,
                    at,
                    player: player.clone(),
                    server: server.clone(),
                    reason: *reason,
                },
            ),
        }
    }
}

/// An event waiting in the queue of a connection.
struct Queued {
    seq: u64,
    at: u64,
    kind: Arc<Kind>,
}

/// One connection that is told events.
struct Sink {
    /// The number of the last event offered, taken and handed to the queue under one lock so that the numbers go up in the order of
    /// the queue.
    seq: Mutex<u64>,
    sender: SyncSender<Queued>,
    /// How many events were dropped since the writer told the extension.
    dropped: Arc<AtomicU64>,
    stats: Arc<Stats>,
}

impl Sink {
    /// Hands the event to the queue of this connection, or drops it if the queue is full. Never waits.
    fn offer(&self, kind: &Arc<Kind>, at: u64) {
        let mut seq = self.seq.lock().unwrap_or_else(|e| e.into_inner());
        *seq += 1;
        let queued = Queued {
            seq: *seq,
            at,
            kind: Arc::clone(kind),
        };
        match self.sender.try_send(queued) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                self.stats.event_dropped();
            }
            // The thread that writes ended: the connection is closing, and there is nobody to tell.
            Err(TrySendError::Disconnected(_)) => {}
        }
    }
}

/// The connections that are told events, shared by every link.
#[derive(Default)]
pub struct Bus {
    /// Whether any connection is listening: read by every emission, so that a proxy nobody listens to pays one load.
    listening: AtomicBool,
    sinks: Mutex<Vec<Arc<Sink>>>,
}

impl Bus {
    /// Tells every connection that listens that `make()` happened. `make` is not called when nobody listens. Never blocks.
    pub fn emit(&self, make: impl FnOnce() -> Kind) {
        if !self.listening.load(Ordering::Relaxed) {
            return;
        }
        let sinks: Vec<Arc<Sink>> = self
            .sinks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(Arc::clone)
            .collect();
        if sinks.is_empty() {
            return;
        }
        let kind = Arc::new(make());
        let at = now_ms();
        for sink in sinks {
            sink.offer(&kind, at);
        }
    }

    fn add(&self, sink: &Arc<Sink>) {
        let mut sinks = self.sinks.lock().unwrap_or_else(|e| e.into_inner());
        sinks.push(Arc::clone(sink));
        self.listening.store(true, Ordering::Relaxed);
    }

    fn remove(&self, sink: &Arc<Sink>) {
        let mut sinks = self.sinks.lock().unwrap_or_else(|e| e.into_inner());
        sinks.retain(|s| !Arc::ptr_eq(s, sink));
        self.listening.store(!sinks.is_empty(), Ordering::Relaxed);
    }

    /// How many connections listen (for the tests).
    #[cfg(test)]
    pub(super) fn listeners(&self) -> usize {
        self.sinks.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

/// For the tests: holds the thread that writes a connection's events, to see what a stalled extension does to the queue.
#[cfg(test)]
#[derive(Default)]
pub(super) struct Hold {
    pub(super) on: AtomicBool,
    /// How many writers are waiting in the hold now.
    pub(super) waiting: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
impl Hold {
    fn pause(&self) {
        if self.on.load(Ordering::SeqCst) {
            self.waiting.fetch_add(1, Ordering::SeqCst);
            while self.on.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            self.waiting.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

/// The milliseconds since the Unix epoch by this machine's clock.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// A connection's place on the bus, for as long as the connection is up: dropped, it leaves the bus, its queue is emptied (not replayed
/// at the next connection) and its thread ends.
pub(super) struct Subscription {
    link: Arc<Link>,
    sink: Arc<Sink>,
}

impl Subscription {
    /// Starts the thread that writes the events of `link` to `writer` and puts the connection on the bus. The caller does this only for
    /// a link granted `events`.
    pub(super) fn start(link: &Arc<Link>, writer: Writer) -> std::io::Result<Self> {
        let (sender, inbox) = mpsc::sync_channel::<Queued>(link.settings.events_queue.max(1));
        let dropped = Arc::new(AtomicU64::new(0));
        let with_address = link.is_granted(meridian_config::ADDRESSES);
        let (name, stats) = (link.name().to_owned(), Arc::clone(link.stats()));
        #[cfg(test)]
        let hold = Arc::clone(&link.event_hold);
        {
            let (dropped, stats, name) = (Arc::clone(&dropped), Arc::clone(&stats), name.clone());
            std::thread::Builder::new()
                .name(format!("extension-{name}-events"))
                .spawn(move || {
                    for queued in inbox {
                        #[cfg(test)]
                        hold.pause();
                        // The events that were dropped are told before the next one that goes out.
                        let lost = dropped.swap(0, Ordering::Relaxed);
                        if lost > 0 {
                            let text = wire::notification(
                                EVENTS_DROPPED,
                                &Dropped {
                                    at: queued.at,
                                    count: lost,
                                },
                            );
                            if write(&writer, &name, &text).is_err() {
                                break;
                            }
                        }
                        let text = queued.kind.render(queued.seq, queued.at, with_address);
                        if write(&writer, &name, &text).is_err() {
                            break;
                        }
                        stats.event_sent(queued.kind.label());
                    }
                })?;
        }
        let sink = Arc::new(Sink {
            seq: Mutex::new(0),
            sender,
            dropped,
            stats,
        });
        link.events().add(&sink);
        Ok(Self {
            link: Arc::clone(link),
            sink,
        })
    }
}

/// Writes one event. A write that fails (the extension does not read, or the connection is gone) closes the connection, as the other
/// writers do: a frame may be half written.
fn write(writer: &Writer, name: &str, text: &str) -> Result<(), ()> {
    writer.send_text(text).map_err(|e| {
        warning!("extension \"{name}\": cannot write an event: {e}; closing the connection");
        let _ = writer.shutdown();
    })
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.link.events().remove(&self.sink);
    }
}
