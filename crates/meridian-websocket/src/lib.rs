//! The little of WebSocket (RFC 6455) that Meridian's processes speak to each other: the upgrade,
//! text messages (whole or in fragments), and the control frames, on both sides of a connection.
//!
//! It is the code the sidecar had, moved here and extended so that the proxy and the extensions
//! share it (docs/EXTENSIONS.md, section 2.5):
//!
//! - **The client**, [`Socket::open`] and [`Socket::open_with`]: it sends the upgrade, reads the answer
//!   (which must be a 101), then text messages, whole or in fragments, and answers pings and a close.
//!   The sidecar is one, of a Horizon server's management protocol. It does not check the server's
//!   `Sec-WebSocket-Accept`, because the servers it talks to are on its own machine and it has
//!   always been so. A client that wants the check, [`Socket::open_checked`], has it.
//! - **The server**, [`Socket::accept`]: it reads the upgrade request, lets the caller decide, answers
//!   with the `Sec-WebSocket-Accept` a client expects, reads masked frames and sends unmasked
//!   ones. It is for the extensions' side of the proxy's connection to them.
//! - **The limit of a message** is the caller's to set ([`MAX_MESSAGE`] by default).
//!
//! What is left out, on purpose: extensions of the protocol (compression, `permessage-deflate`), binary
//! messages (read and dropped), and TLS: the caller has seen that the other end is on a loopback address.
mod split;
mod upgrade;

pub use split::{Half, Reader, Writer};
use std::io::{self, Read, Write};
use std::time::{Duration, Instant};
pub use upgrade::{Upgrade, accept_key};

/// The longest message the sidecar takes, which is what [`Socket::open`] uses: a notification
/// is a line, and anything near this is not one. The answer to a request for a server's ban
/// list is the longest thing it reads, a couple of hundred bytes for each ban. Another
/// caller gives its own limit to [`Socket::open_with`] (an extension's messages are at most
/// 256 KiB).
pub const MAX_MESSAGE: usize = 4 << 20;

/// Close codes (RFC 6455, 7.4.1) a side may end a connection with.
pub mod close {
    /// The connection ends as it should.
    pub const NORMAL: u16 = 1000;
    /// A message broke a rule of the protocol above (a proof that is wrong, for the extensions').
    pub const POLICY: u16 = 1008;
    /// A message was longer than this side takes.
    pub const TOO_BIG: u16 = 1009;
}

/// The longest reply to the upgrade read before giving up.
const MAX_HEAD: usize = 16 << 10;

/// How long the head of an upgrade may take to arrive in all, when the caller does not say: a read that
/// times out is counted by the stream, and a peer that sends a byte just before each timeout would hold
/// the connection for as long as it likes. [`Socket::accept_within`] takes the time as a parameter.
pub const HEAD_TIMEOUT: Duration = Duration::from_secs(10);

/// What [`Socket::read_message`] found.
#[derive(Debug, PartialEq, Eq)]
pub enum Next {
    /// One text message.
    Text(String),
    /// Nothing came before the read timeout of the stream.
    Idle,
    /// The server closed the connection, or sent a close.
    Closed,
}

/// The upgrade request, with the secret as a bearer token.
pub fn upgrade_request(host: &str, port: u16, secret: &str, key: &str) -> String {
    format!(
        "GET / HTTP/1.1\r\nHost: {host}:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\
         Authorization: Bearer {secret}\r\n\r\n"
    )
}

/// A `Sec-WebSocket-Key`: 16 random bytes in base64.
pub fn new_key() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| io::Error::other(e.to_string()))?;
    Ok(base64(&bytes))
}

/// Standard base64, with padding.
pub fn base64(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Which end of the connection a socket is: a client masks what it sends and takes frames either way
/// (as it always has); a server sends unmasked frames and refuses a client frame that is not masked
/// (RFC 6455, 5.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Client,
    Server,
}

/// A WebSocket the upgrade of which was accepted.
pub struct Socket<T: Read + Write> {
    stream: T,
    role: Role,
    /// The subprotocol the other end chose (`Sec-WebSocket-Protocol`), if it named one.
    protocol: Option<String>,
    /// Bytes read and not yet parsed.
    buf: Vec<u8>,
    /// The message being gathered from fragments: `None` between messages, and
    /// `Some(None)` for one that is not text, which is dropped.
    message: Option<Option<Vec<u8>>>,
    /// The longest message read or sent; a longer one is an error.
    max_message: usize,
    /// The code of the close frame the other end sent, once it has.
    close_code: Option<u16>,
    /// The longest a message may take to arrive once its first byte has: `None` waits as long as the
    /// other end likes, which the stream's own read timeout does not limit (it counts each read).
    message_timeout: Option<Duration>,
    /// When the part of a message that is being gathered began to arrive.
    partial_since: Option<Instant>,
}

impl<T: Read + Write> Socket<T> {
    /// Sends the upgrade and reads the answer, which must be a 101. The `stream` may
    /// have a read timeout: it applies to the answer too. Messages are at most
    /// [`MAX_MESSAGE`].
    pub fn open(stream: T, request: &str) -> Result<Self, String> {
        Self::open_with(stream, request, MAX_MESSAGE)
    }

    /// As [`Socket::open`], with messages of at most `max_message` bytes, read or sent.
    pub fn open_with(stream: T, request: &str, max_message: usize) -> Result<Self, String> {
        Self::open_inner(stream, request, max_message, false)
    }

    /// As [`Socket::open_with`], and the answer is checked as RFC 6455 (4.2.2) asks of a client: it
    /// carries `Upgrade: websocket`, `Connection: Upgrade` and the `Sec-WebSocket-Accept` that
    /// the request's `Sec-WebSocket-Key` calls for, and a subprotocol it names is one the
    /// request offered. The request must carry a key (as [`upgrade_request`] and [`new_key`] make one).
    pub fn open_checked(stream: T, request: &str, max_message: usize) -> Result<Self, String> {
        Self::open_inner(stream, request, max_message, true)
    }

    fn open_inner(
        mut stream: T,
        request: &str,
        max_message: usize,
        check: bool,
    ) -> Result<Self, String> {
        stream
            .write_all(request.as_bytes())
            .and_then(|()| stream.flush())
            .map_err(|e| format!("cannot send the upgrade: {e}"))?;
        let mut buf = Vec::new();
        let end = read_head(
            &mut stream,
            &mut buf,
            Instant::now() + HEAD_TIMEOUT,
            "the answer to the upgrade is too long",
            "the server closed the connection before it answered",
            "no answer to the upgrade",
        )?;
        let head = String::from_utf8_lossy(&buf[..end]).into_owned();
        let status = head.lines().next().unwrap_or_default();
        if !status.split(' ').nth(1).is_some_and(|code| code == "101") {
            return Err(format!("the upgrade was refused: {status}"));
        }
        let protocol = if check {
            upgrade::check_answer(request, &head)?
        } else {
            upgrade::header_of(&head, "sec-websocket-protocol")
        };
        buf.drain(..end + 4);
        Ok(Self {
            stream,
            role: Role::Client,
            protocol,
            buf,
            message: None,
            max_message,
            close_code: None,
            message_timeout: None,
            partial_since: None,
        })
    }

    /// Limits how long a message may take to arrive, from its first byte to its last: a message that
    /// is not whole by then is a [`io::ErrorKind::TimedOut`] error from [`Socket::read_message`], and
    /// the caller ends the connection. The stream's read timeout alone does not do it, since it
    /// counts each read and a peer that sends a byte before each one never trips it. Nothing is
    /// limited by default, as the sidecar has always read; a peer that is not trusted needs this.
    pub fn set_message_timeout(&mut self, timeout: Option<Duration>) {
        self.message_timeout = timeout;
    }

    /// The subprotocol the other end chose, if it named one.
    pub fn protocol(&self) -> Option<&str> {
        self.protocol.as_deref()
    }

    /// The stream under the socket, to set its timeouts.
    pub fn stream_mut(&mut self) -> &mut T {
        &mut self.stream
    }

    /// The next text message. Ping is answered and other frames are skipped; `Idle`
    /// when the stream's read timeout passes with no whole message.
    pub fn read_message(&mut self) -> io::Result<Next> {
        loop {
            self.check_message_time()?;
            if let Some(frame) = self.take_frame()? {
                match frame.opcode {
                    // Text starts a message, binary starts one that is dropped, and a
                    // continuation adds to the one under way.
                    0x1 => self.message = Some(Some(frame.payload)),
                    0x2 => self.message = Some(None),
                    0x0 => {
                        if let Some(Some(text)) = &mut self.message {
                            text.extend_from_slice(&frame.payload);
                            if text.len() > self.max_message {
                                return Err(too_long(self.max_message));
                            }
                        }
                    }
                    0x8 => {
                        self.close_code = match frame.payload[..] {
                            [high, low, ..] => Some(u16::from_be_bytes([high, low])),
                            _ => None,
                        };
                        let _ = self.send(0x8, &frame.payload);
                        self.partial_since = None;
                        return Ok(Next::Closed);
                    }
                    0x9 => self.send(0xA, &frame.payload)?,
                    _ => {}
                }
                if frame.fin
                    && frame.opcode < 0x3
                    && let Some(Some(text)) = self.message.take()
                {
                    // A message that is not UTF-8 is not one of this protocol's.
                    // The next message, if it has begun, is timed from now.
                    self.partial_since = None;
                    if let Ok(text) = String::from_utf8(text) {
                        return Ok(Next::Text(text));
                    }
                }
                continue;
            }
            let mut chunk = [0u8; 4096];
            match self.stream.read(&mut chunk) {
                Ok(0) => return Ok(Next::Closed),
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    return Ok(Next::Idle);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }

    /// The code of the close frame the other end sent, once `read_message` has returned
    /// [`Next::Closed`] on one. A stream that ended with no close frame has none.
    pub fn close_code(&self) -> Option<u16> {
        self.close_code
    }

    /// Ends the connection politely: a close frame with `code` (see [`close`]) and a `reason` of a few
    /// words, at most 123 bytes of it, cut at a character. The other end is told, and answers with its
    /// own close; the caller then drops the socket.
    pub fn close(&mut self, code: u16, reason: &str) -> io::Result<()> {
        self.send(0x8, &close_payload(code, reason))
    }

    /// A control frame: a client masks it, as RFC 6455 requires, and a server does not.
    fn send(&mut self, opcode: u8, payload: &[u8]) -> io::Result<()> {
        // A control frame carries 125 bytes at most.
        let payload = &payload[..payload.len().min(125)];
        self.write_frame(opcode, payload)
    }

    /// Sends one text message (a request of the management protocol, or of the extension
    /// protocol), in a single frame: masked for a client, not for a server.
    pub fn send_text(&mut self, text: &str) -> io::Result<()> {
        let payload = text.as_bytes();
        if payload.len() > self.max_message {
            return Err(too_long(self.max_message));
        }
        self.write_frame(0x1, payload)
    }

    /// One whole frame (`fin` set), of any length, written with one `write_all`.
    fn write_frame(&mut self, opcode: u8, payload: &[u8]) -> io::Result<()> {
        let frame = encode_frame(self.role == Role::Client, opcode, payload)?;
        self.stream.write_all(&frame)?;
        self.stream.flush()
    }

    /// The first whole frame in the buffer, taken out of it.
    /// Whether part of a message has arrived and the rest has not.
    fn in_progress(&self) -> bool {
        !self.buf.is_empty() || self.message.is_some()
    }

    /// Fails if the message that is arriving has taken longer than the limit.
    fn check_message_time(&mut self) -> io::Result<()> {
        if !self.in_progress() {
            self.partial_since = None;
            return Ok(());
        }
        let since = *self.partial_since.get_or_insert_with(Instant::now);
        match self.message_timeout {
            Some(limit) if since.elapsed() > limit => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "a WebSocket message took too long to arrive",
            )),
            _ => Ok(()),
        }
    }

    fn take_frame(&mut self) -> io::Result<Option<Frame>> {
        let max = self.max_message;
        let buf = &self.buf;
        if buf.len() < 2 {
            return Ok(None);
        }
        let (fin, opcode) = (buf[0] & 0x80 != 0, buf[0] & 0x0f);
        let masked = buf[1] & 0x80 != 0;
        if self.role == Role::Server && !masked {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "a WebSocket client frame that is not masked",
            ));
        }
        let (len, mut at) = match buf[1] & 0x7f {
            126 => {
                if buf.len() < 4 {
                    return Ok(None);
                }
                (usize::from(u16::from_be_bytes([buf[2], buf[3]])), 4)
            }
            127 => {
                if buf.len() < 10 {
                    return Ok(None);
                }
                let len = u64::from_be_bytes(buf[2..10].try_into().expect("eight bytes"));
                match usize::try_from(len) {
                    Ok(len) if len <= max => (len, 10),
                    _ => return Err(too_long(max)),
                }
            }
            short => (usize::from(short), 2),
        };
        if len > max {
            return Err(too_long(max));
        }
        let mask = if masked {
            if buf.len() < at + 4 {
                return Ok(None);
            }
            at += 4;
            Some([buf[at - 4], buf[at - 3], buf[at - 2], buf[at - 1]])
        } else {
            None
        };
        if buf.len() < at + len {
            return Ok(None);
        }
        let mut payload = buf[at..at + len].to_vec();
        if let Some(mask) = mask {
            for (b, m) in payload.iter_mut().zip(mask.iter().cycle()) {
                *b ^= m;
            }
        }
        self.buf.drain(..at + len);
        Ok(Some(Frame {
            fin,
            opcode,
            payload,
        }))
    }
}

struct Frame {
    fin: bool,
    opcode: u8,
    payload: Vec<u8>,
}

/// One whole frame (`fin` set), of any length: masked for a client, not for a server.
fn encode_frame(masked: bool, opcode: u8, payload: &[u8]) -> io::Result<Vec<u8>> {
    let mask_bit = if masked { 0x80 } else { 0 };
    let mut frame = vec![0x80 | opcode];
    match payload.len() {
        n if n < 126 => frame.push(mask_bit | n as u8),
        n if n <= 0xffff => {
            frame.push(mask_bit | 126);
            frame.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            frame.push(mask_bit | 127);
            frame.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    if masked {
        let mut mask = [0u8; 4];
        getrandom::fill(&mut mask).map_err(|e| io::Error::other(e.to_string()))?;
        frame.extend_from_slice(&mask);
        frame.extend(payload.iter().zip(mask.iter().cycle()).map(|(b, m)| b ^ m));
    } else {
        frame.extend_from_slice(payload);
    }
    Ok(frame)
}

/// The payload of a close frame: the code, then a reason of at most 123 bytes, cut at a character.
fn close_payload(code: u16, reason: &str) -> Vec<u8> {
    let mut payload = code.to_be_bytes().to_vec();
    let mut end = reason.len().min(123);
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    payload.extend_from_slice(&reason.as_bytes()[..end]);
    payload
}

fn too_long(limit: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("a WebSocket message longer than the {limit} bytes this side takes"),
    )
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Reads from `stream` into `buf` until the blank line that ends the head of an HTTP message, and
/// returns where it is. What follows it stays in `buf`. The three messages are the errors of a head
/// that is too long, a stream that ends, and a read that fails.
fn read_head<T: Read>(
    stream: &mut T,
    buf: &mut Vec<u8>,
    deadline: Instant,
    too_long: &str,
    closed: &str,
    failed: &str,
) -> Result<usize, String> {
    let mut chunk = [0u8; 1024];
    loop {
        if let Some(end) = find(buf, b"\r\n\r\n") {
            return Ok(end);
        }
        if buf.len() > MAX_HEAD {
            return Err(too_long.into());
        }
        // A read that blocks is cut by the stream's timeout; this is the whole head's.
        if Instant::now() >= deadline {
            return Err(format!("{failed}: the head took too long to arrive"));
        }
        match stream.read(&mut chunk) {
            Ok(0) => return Err(closed.into()),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) => return Err(format!("{failed}: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// A stream that reads from one buffer and writes to another.
    struct Pipe {
        read: Cursor<Vec<u8>>,
        written: Vec<u8>,
    }

    impl Read for Pipe {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            self.read.read(out)
        }
    }

    impl Write for Pipe {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.written.extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    const REQUEST: &str = "GET / HTTP/1.1\r\n\r\n";

    fn frame(fin: bool, opcode: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![if fin { 0x80 } else { 0 } | opcode];
        match payload.len() {
            n if n < 126 => out.push(n as u8),
            n if n <= 0xffff => {
                out.push(126);
                out.extend_from_slice(&(n as u16).to_be_bytes());
            }
            n => {
                out.push(127);
                out.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        out.extend_from_slice(payload);
        out
    }

    fn socket(after_head: Vec<u8>) -> Socket<Pipe> {
        let mut bytes = b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\r\n".to_vec();
        bytes.extend(after_head);
        Socket::open(
            Pipe {
                read: Cursor::new(bytes),
                written: Vec::new(),
            },
            REQUEST,
        )
        .unwrap()
    }

    #[test]
    fn base64_is_the_standard_one() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(new_key().unwrap().len(), 24);
    }

    #[test]
    fn the_upgrade_carries_the_secret_as_a_bearer_token() {
        let request = upgrade_request("localhost", 25585, "s3cret", "a2V5");
        assert!(request.starts_with("GET / HTTP/1.1\r\nHost: localhost:25585\r\n"));
        assert!(request.contains("Authorization: Bearer s3cret\r\n"));
        assert!(request.contains("Sec-WebSocket-Key: a2V5\r\n"));
        assert!(request.ends_with("\r\n\r\n"));
    }

    #[test]
    fn an_upgrade_that_is_refused_says_why() {
        let refused = Socket::open(
            Pipe {
                read: Cursor::new(b"HTTP/1.1 401 Unauthorized\r\n\r\n{}".to_vec()),
                written: Vec::new(),
            },
            "GET / HTTP/1.1\r\n\r\n",
        );
        assert_eq!(
            refused.err().unwrap(),
            "the upgrade was refused: HTTP/1.1 401 Unauthorized"
        );
    }

    #[test]
    fn whole_and_fragmented_texts_are_read_and_the_rest_skipped() {
        let mut bytes = frame(true, 0x1, b"one");
        bytes.extend(frame(true, 0x2, b"binary"));
        bytes.extend(frame(false, 0x1, b"tw"));
        bytes.extend(frame(false, 0x0, b"o, "));
        bytes.extend(frame(true, 0x0, b"in parts"));
        bytes.extend(frame(true, 0xA, b"pong"));
        bytes.extend(frame(true, 0x1, &[b'x'; 300]));
        let mut socket = socket(bytes);
        assert_eq!(socket.read_message().unwrap(), Next::Text("one".into()));
        assert_eq!(
            socket.read_message().unwrap(),
            Next::Text("two, in parts".into())
        );
        assert_eq!(socket.read_message().unwrap(), Next::Text("x".repeat(300)));
        assert_eq!(socket.read_message().unwrap(), Next::Closed);
    }

    /// The payload of the frame that starts at `from` in what the socket wrote, unmasked,
    /// and where the frame ends.
    fn unmask(written: &[u8], from: usize) -> (u8, Vec<u8>, usize) {
        let head = &written[from..];
        assert_ne!(head[1] & 0x80, 0, "a client frame is masked");
        let (len, at) = match head[1] & 0x7f {
            126 => (usize::from(u16::from_be_bytes([head[2], head[3]])), 4),
            127 => (
                usize::try_from(u64::from_be_bytes(head[2..10].try_into().unwrap())).unwrap(),
                10,
            ),
            short => (usize::from(short), 2),
        };
        let mask = &head[at..at + 4];
        let body = &head[at + 4..at + 4 + len];
        let text = body
            .iter()
            .zip(mask.iter().cycle())
            .map(|(b, m)| b ^ m)
            .collect();
        (head[0], text, from + at + 4 + len)
    }

    #[test]
    fn a_text_is_sent_in_one_masked_frame_whatever_its_length() {
        let mut socket = socket(Vec::new());
        let texts = ["{}".to_owned(), "y".repeat(300), "z".repeat(70_000)];
        for text in &texts {
            socket.send_text(text).unwrap();
        }
        let mut from = REQUEST.len();
        for text in &texts {
            let (first, body, end) = unmask(&socket.stream.written, from);
            assert_eq!(first, 0x81, "FIN and the text opcode");
            assert_eq!(body, text.as_bytes());
            from = end;
        }
        assert_eq!(from, socket.stream.written.len());
        let too_long = "x".repeat(MAX_MESSAGE + 1);
        assert!(socket.send_text(&too_long).is_err());
    }

    #[test]
    fn a_ping_is_answered_with_a_masked_pong_and_a_close_with_a_close() {
        let mut bytes = frame(true, 0x9, b"are you there");
        bytes.extend(frame(true, 0x8, &[0x03, 0xe8]));
        let mut socket = socket(bytes);
        assert_eq!(socket.read_message().unwrap(), Next::Closed);
        // After the upgrade request the socket wrote.
        let sent = &socket.stream.written[REQUEST.len()..];
        // Pong: FIN and opcode 10, masked, the same 13 bytes.
        assert_eq!(&sent[..2], &[0x8A, 0x80 | 13]);
        let (mask, body) = (&sent[2..6], &sent[6..19]);
        let unmasked: Vec<u8> = body
            .iter()
            .zip(mask.iter().cycle())
            .map(|(b, m)| b ^ m)
            .collect();
        assert_eq!(unmasked, b"are you there");
        // Then the close, echoing the code.
        assert_eq!(&sent[19..21], &[0x88, 0x80 | 2]);
    }

    #[test]
    fn a_close_frame_has_its_code_and_a_reason_cut_at_a_character() {
        let mut socket = socket(Vec::new());
        let start = socket.stream.written.len();
        socket.close(close::POLICY, "wrong proof").unwrap();
        let (first, payload, _) = unmask(&socket.stream.written, start);
        assert_eq!(first, 0x88);
        assert_eq!(&payload[..2], &1008u16.to_be_bytes());
        assert_eq!(&payload[2..], b"wrong proof");

        // 122 bytes of reason and a two-byte character after them: the 123 bytes that fit would cut
        // the character in two, so it is left out.
        let reason = format!("{}\u{e9}\u{e9}", "a".repeat(122));
        let start = socket.stream.written.len();
        socket.close(close::NORMAL, &reason).unwrap();
        let (_, payload, _) = unmask(&socket.stream.written, start);
        assert!(payload.len() <= 125);
        assert_eq!(std::str::from_utf8(&payload[2..]).unwrap().len(), 122);
    }

    #[test]
    fn the_code_of_the_close_that_was_read_is_kept() {
        let mut socket = socket(frame(true, 0x8, &[0x03, 0xf0, b'x']));
        assert_eq!(socket.close_code(), None);
        assert_eq!(socket.read_message().unwrap(), Next::Closed);
        assert_eq!(socket.close_code(), Some(1008));
        // A close with no payload has no code.
        let mut socket = self::socket(frame(true, 0x8, &[]));
        assert_eq!(socket.read_message().unwrap(), Next::Closed);
        assert_eq!(socket.close_code(), None);
    }

    #[test]
    fn a_message_that_is_too_long_is_an_error() {
        let mut bytes = vec![0x81, 127];
        bytes.extend_from_slice(&((MAX_MESSAGE as u64) + 1).to_be_bytes());
        let mut socket = socket(bytes);
        assert_eq!(
            socket.read_message().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    /// A socket that takes messages of at most `max` bytes, over the same pipe.
    fn limited(after_head: Vec<u8>, max: usize) -> Socket<Pipe> {
        let mut bytes = b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\r\n".to_vec();
        bytes.extend(after_head);
        Socket::open_with(
            Pipe {
                read: Cursor::new(bytes),
                written: Vec::new(),
            },
            REQUEST,
            max,
        )
        .unwrap()
    }

    #[test]
    fn the_limit_is_the_callers_to_set_and_a_message_at_it_still_passes() {
        // Exactly at the limit: read. One byte over: refused, whole or in fragments.
        let mut at = limited(frame(true, 0x1, &[b'a'; 256]), 256);
        assert_eq!(at.read_message().unwrap(), Next::Text("a".repeat(256)));
        let mut over = limited(frame(true, 0x1, &[b'a'; 257]), 256);
        assert_eq!(
            over.read_message().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        let mut pieces = frame(false, 0x1, &[b'a'; 200]);
        pieces.extend(frame(true, 0x0, &[b'a'; 57]));
        let mut fragmented = limited(pieces, 256);
        assert_eq!(
            fragmented.read_message().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        // What is sent is held to it too.
        let mut sender = limited(Vec::new(), 256);
        sender.send_text(&"a".repeat(256)).unwrap();
        assert_eq!(
            sender.send_text(&"a".repeat(257)).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        // A frame that claims more than the limit is refused from its length alone, before
        // its payload is waited for.
        let mut head = vec![0x81, 127];
        head.extend_from_slice(&257u64.to_be_bytes());
        let mut claimed = limited(head, 256);
        assert!(claimed.read_message().is_err());
        // `open` keeps the sidecar's limit.
        let mut default = socket(frame(true, 0x1, &[b'a'; 300]));
        assert_eq!(default.read_message().unwrap(), Next::Text("a".repeat(300)));
    }

    #[test]
    fn a_frame_in_pieces_is_waited_for() {
        // The first half of a frame, then the stream ends: nothing is returned for it.
        let whole = frame(true, 0x1, b"half a frame");
        let mut socket = socket(whole[..5].to_vec());
        assert_eq!(socket.read_message().unwrap(), Next::Closed);
    }

    /// Frames changed at random, from a fixed seed: text, fragments, pings, a close,
    /// and lengths of every size. The reader ends at the end of the bytes, without a
    /// panic, and never gives a message longer than its limit.
    #[test]
    fn hostile_frames_are_read_or_refused_without_a_panic() {
        use meridian_testkit::mutate::{Rng, mutate};
        let mut valid = frame(true, 0x1, b"{\"method\":\"notification/server/stopping\"}");
        valid.extend(frame(false, 0x1, b"in "));
        valid.extend(frame(false, 0x0, b"three "));
        valid.extend(frame(true, 0x0, b"pieces"));
        valid.extend(frame(true, 0x9, b"ping"));
        valid.extend(frame(true, 0x2, &[0, 1, 2]));
        valid.extend(frame(true, 0x1, &[b'x'; 300]));
        valid.extend(frame(true, 0x8, &[]));
        let mut rng = Rng::new(0x7773);
        for _ in 0..3000 {
            let bytes = mutate(&mut rng, &valid);
            let budget = bytes.len() + 2;
            let mut socket = socket(bytes);
            // Every message takes at least one byte of the input: the reader cannot
            // go round more often than that without being stuck.
            for _ in 0..budget {
                match socket.read_message() {
                    Ok(Next::Text(text)) => assert!(text.len() <= MAX_MESSAGE),
                    Ok(Next::Idle) => {}
                    Ok(Next::Closed) | Err(_) => break,
                }
            }
        }
    }
}
