//! An ordered record of the packets a session carried: which way each went, its id
//! and its payload. Two traces compare with `==`, so a test can check that what one
//! side sent is exactly what the other side received.
use std::sync::{Arc, Mutex};

/// Which way a packet went.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Dir {
    /// From a server, or the proxy, to the player.
    ToClient,
    /// From the player, or the proxy, to a server.
    ToServer,
}

/// One packet: its direction, its id and its payload (the bytes after the id).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packet {
    pub dir: Dir,
    pub id: i32,
    pub payload: Vec<u8>,
}

/// The packets of a session, in the order they were recorded.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Trace {
    pub packets: Vec<Packet>,
}

impl Trace {
    /// Adds a packet at the end.
    pub fn push(&mut self, dir: Dir, id: i32, payload: &[u8]) {
        self.packets.push(Packet {
            dir,
            id,
            payload: payload.to_vec(),
        });
    }

    /// The packets that went one way, in order.
    pub fn packets(&self, dir: Dir) -> impl Iterator<Item = &Packet> {
        self.packets.iter().filter(move |p| p.dir == dir)
    }

    /// The ids of the packets that went one way, in order.
    pub fn ids(&self, dir: Dir) -> Vec<i32> {
        self.packets(dir).map(|p| p.id).collect()
    }

    /// How many packets went one way.
    pub fn count(&self, dir: Dir) -> usize {
        self.packets(dir).count()
    }

    /// The payload bytes that went one way (ids and framing not counted).
    pub fn bytes(&self, dir: Dir) -> usize {
        self.packets(dir).map(|p| p.payload.len()).sum()
    }

    /// The packets that went one way, as a trace of their own.
    pub fn only(&self, dir: Dir) -> Trace {
        Trace {
            packets: self.packets(dir).cloned().collect(),
        }
    }
}

/// A trace that several threads can add to.
#[derive(Clone, Debug, Default)]
pub struct Recorder(Arc<Mutex<Trace>>);

impl Recorder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a packet at the end.
    pub fn record(&self, dir: Dir, id: i32, payload: &[u8]) {
        self.lock().push(dir, id, payload);
    }

    /// A copy of everything recorded so far.
    pub fn trace(&self) -> Trace {
        self.lock().clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Trace> {
        // A thread that panicked while recording leaves a trace that is still
        // readable: keep it for the test's report.
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trace_keeps_order_and_splits_by_direction() {
        let mut trace = Trace::default();
        trace.push(Dir::ToClient, 7, &[1, 2, 3]);
        trace.push(Dir::ToServer, 4, &[9]);
        trace.push(Dir::ToClient, 3, &[]);
        assert_eq!(trace.ids(Dir::ToClient), vec![7, 3]);
        assert_eq!(trace.ids(Dir::ToServer), vec![4]);
        assert_eq!(trace.count(Dir::ToClient), 2);
        assert_eq!(trace.bytes(Dir::ToClient), 3);
        assert_eq!(trace.bytes(Dir::ToServer), 1);
        assert_eq!(trace.only(Dir::ToServer).packets.len(), 1);
    }

    #[test]
    fn a_recorder_collects_from_several_threads() {
        let recorder = Recorder::new();
        let threads: Vec<_> = (0..4)
            .map(|i| {
                let recorder = recorder.clone();
                std::thread::spawn(move || {
                    for _ in 0..10 {
                        recorder.record(Dir::ToServer, i, &[]);
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(recorder.trace().count(Dir::ToServer), 40);
    }
}
