//! The events an extension reads (docs/EXTENSIONS.md, section 6): the handler never runs on the thread that reads the connection.
//!
//! The reader gives each event to a queue and goes on reading, so a handler that is slow never delays an answer to the proxy or the
//! reading of its next message. The queue is bounded: when it is full the **newest** event is dropped. Every event is numbered as it is
//! offered, the dropped ones included, and the thread tells the handler the gap where it is: before it hands over an event whose number is
//! not the one after the last it handed over, it hands over a local [`Event::Dropped`] with the size of the gap. A handler that panics loses
//! that event and not the thread.

use crate::events::{Dropped, Event};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::time::{SystemTime, UNIX_EPOCH};

/// How many events wait for the handler.
pub(crate) const QUEUE: usize = 1024;

/// The queue of the events of an extension, and the thread that runs its handler.
pub(crate) struct Events {
    sender: SyncSender<(u64, Event)>,
    /// The number of the last event offered, taken for every event, the dropped ones included.
    offered: AtomicU64,
}

impl Events {
    /// A queue of `room` events and a thread that runs `handler` for each.
    pub(crate) fn start(
        room: usize,
        handler: impl Fn(&Event) + Send + Sync + 'static,
    ) -> std::io::Result<Self> {
        let (sender, inbox) = mpsc::sync_channel::<(u64, Event)>(room.max(1));
        std::thread::Builder::new()
            .name("extension-events".to_owned())
            .spawn(move || {
                let mut last = 0;
                for (number, event) in inbox {
                    if number != last + 1 {
                        let overflow = Event::Dropped(Dropped {
                            at: now_ms(),
                            count: number - last - 1,
                        });
                        let _ = catch_unwind(AssertUnwindSafe(|| handler(&overflow)));
                    }
                    last = number;
                    let _ = catch_unwind(AssertUnwindSafe(|| handler(&event)));
                }
            })?;
        Ok(Self {
            sender,
            offered: AtomicU64::new(0),
        })
    }

    /// Hands an event to the handler's queue, or drops it if the queue is full. Never waits.
    pub(crate) fn push(&self, event: Event) {
        let number = self.offered.fetch_add(1, Ordering::SeqCst) + 1;
        // Full: the event is dropped, and its number is the gap the thread tells.
        let _: Result<(), TrySendError<(u64, Event)>> = self.sender.try_send((number, event));
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}
