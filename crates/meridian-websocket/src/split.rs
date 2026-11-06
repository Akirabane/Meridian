//! A socket in two halves, for a connection that one thread reads while others send: the extension
//! protocol's (docs/EXTENSIONS.md, 8.1), where the proxy's threads ask decisions at the same time as a
//! reader thread takes the answers.
//!
//! The reading half is a [`Socket`] like any other, and answers a ping or a close through the writing
//! half. The writing half is cloned for as many threads as send. All writes to the stream go through
//! one lock, and a frame is written whole under it, so frames of two threads are never mixed.

use super::{Role, Socket, close_payload, encode_frame, too_long};
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

/// What the two halves share.
struct Shared {
    /// The stream as it is written to, under the lock.
    out: Mutex<TcpStream>,
    /// Another handle on the same socket, to shut it down without waiting for the lock.
    closer: TcpStream,
    masked: bool,
    max_message: usize,
}

impl Shared {
    fn lock(&self) -> io::Result<MutexGuard<'_, TcpStream>> {
        self.out
            .lock()
            .map_err(|_| io::Error::other("a thread panicked while it wrote to the WebSocket"))
    }

    /// One whole frame, written and flushed under the lock.
    fn send(&self, opcode: u8, payload: &[u8]) -> io::Result<()> {
        let frame = encode_frame(self.masked, opcode, payload)?;
        let mut out = self.lock()?;
        out.write_all(&frame)?;
        out.flush()
    }
}

/// The stream of the reading half: it reads from its own handle, and what it writes (the answer to a
/// ping or a close) goes through the shared lock. `Socket` writes each frame with one `write_all`,
/// which is held under the lock whole.
pub struct Half {
    read: TcpStream,
    shared: Arc<Shared>,
}

impl Half {
    /// Sets the read timeout of the socket, for [`Next::Idle`](crate::Next::Idle).
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.read.set_read_timeout(timeout)
    }
}

impl Read for Half {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.read.read(buf)
    }
}

impl Write for Half {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.shared.lock()?.write(buf)
    }

    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.shared.lock()?.write_all(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.shared.lock()?.flush()
    }
}

/// The reading half of a split socket: `read_message` on one thread.
pub type Reader = Socket<Half>;

/// The writing half of a split socket. It is cloned for each thread that sends; every send takes
/// `&self`.
#[derive(Clone)]
pub struct Writer {
    shared: Arc<Shared>,
}

impl Writer {
    /// Sends one text message in one frame, whole even while other threads send.
    pub fn send_text(&self, text: &str) -> io::Result<()> {
        if text.len() > self.shared.max_message {
            return Err(too_long(self.shared.max_message));
        }
        self.shared.send(0x1, text.as_bytes())
    }

    /// As [`Socket::close`].
    pub fn close(&self, code: u16, reason: &str) -> io::Result<()> {
        self.shared.send(0x8, &close_payload(code, reason))
    }

    /// Shuts the socket down, both ways: the other end sees the stream end at once, a send fails, and
    /// the reader's next read is the end of the stream. A read that is already blocked is not woken
    /// on every system (Windows does not), so a reader that is to be freed at once reads with a
    /// timeout and looks at a flag when it gets [`Next::Idle`](crate::Next::Idle). This does not wait
    /// for a thread that is writing.
    pub fn shutdown(&self) -> io::Result<()> {
        self.shared.closer.shutdown(Shutdown::Both)
    }
}

impl Socket<TcpStream> {
    /// Splits the socket into a [`Reader`] and a [`Writer`] on the same connection. What was read
    /// from the stream and not yet parsed stays with the reader.
    pub fn split(self) -> io::Result<(Reader, Writer)> {
        let shared = Arc::new(Shared {
            out: Mutex::new(self.stream.try_clone()?),
            closer: self.stream.try_clone()?,
            masked: self.role == Role::Client,
            max_message: self.max_message,
        });
        let reader = Socket {
            stream: Half {
                read: self.stream,
                shared: Arc::clone(&shared),
            },
            role: self.role,
            protocol: self.protocol,
            buf: self.buf,
            message: self.message,
            max_message: self.max_message,
            close_code: self.close_code,
            message_timeout: self.message_timeout,
            partial_since: self.partial_since,
        };
        Ok((reader, Writer { shared }))
    }
}
