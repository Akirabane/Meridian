//! The events an extension reads (docs/EXTENSIONS.md, section 6): the handler never runs on the thread that reads the connection.
//!
//! The reader gives each event to a queue and goes on reading, so a handler that is slow never delays an answer to the proxy or the
//! reading of its next message. The queue is bounded: when it is full the **newest** event is dropped and counted, and the handler is
//! told by a local [`Event::Dropped`] before the next event it gets. A handler that panics loses that event and not the thread.

use crate::events::{Dropped, Event};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::time::{SystemTime, UNIX_EPOCH};

/// How many events wait for the handler.
pub(crate) const QUEUE: usize = 1024;

/// The queue of the events of an extension, and the thread that runs its handler.
pub(crate) struct Events {
    sender: SyncSender<Event>,
    dropped: Arc<AtomicU64>,
}

impl Events {
    /// A queue of `room` events and a thread that runs `handler` for each.
    pub(crate) fn start(
        room: usize,
        handler: impl Fn(&Event) + Send + Sync + 'static,
    ) -> std::io::Result<Self> {
        let (sender, inbox) = mpsc::sync_channel::<Event>(room.max(1));
        let dropped = Arc::new(AtomicU64::new(0));
        let lost = Arc::clone(&dropped);
        std::thread::Builder::new()
            .name("extension-events".to_owned())
            .spawn(move || {
                for event in inbox {
                    let count = lost.swap(0, Ordering::Relaxed);
                    if count > 0 {
                        let overflow = Event::Dropped(Dropped {
                            at: now_ms(),
                            count,
                        });
                        let _ = catch_unwind(AssertUnwindSafe(|| handler(&overflow)));
                    }
                    let _ = catch_unwind(AssertUnwindSafe(|| handler(&event)));
                }
            })?;
        Ok(Self { sender, dropped })
    }

    /// Hands an event to the handler's queue, or drops it if the queue is full. Never waits.
    pub(crate) fn push(&self, event: Event) {
        if let Err(TrySendError::Full(_)) = self.sender.try_send(event) {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}
