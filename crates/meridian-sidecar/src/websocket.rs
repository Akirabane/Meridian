//! The little of WebSocket (RFC 6455) that a client which only listens needs: the
//! upgrade with a bearer token, text messages (whole or in fragments), and the
//! control frames. Used to read the notifications of a Horizon server's management
//! protocol (`management.rs`), which sends them and asks nothing of the client.
//!
//! What is left out, on purpose: the server's `Sec-WebSocket-Accept` is not checked
//! (it needs SHA-1, which nothing else here uses), and the sidecar reads only a
//! server on its own machine, which the caller has already seen is on a loopback
//! address; and what is sent is pongs, a close, and the requests of the management
//! protocol (`send_text`).
use std::io::{self, Read, Write};

/// The longest message taken: a notification is a line, and anything near this is not
/// one. The answer to a request for a server's ban list is the longest thing it reads, a
/// couple of hundred bytes for each ban.
pub const MAX_MESSAGE: usize = 4 << 20;

/// The longest reply to the upgrade read before giving up.
const MAX_HEAD: usize = 16 << 10;

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
    const DIGITS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(DIGITS[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// A WebSocket the upgrade of which was accepted.
pub struct Socket<T: Read + Write> {
    stream: T,
    /// Bytes read and not yet parsed.
    buf: Vec<u8>,
    /// The message being gathered from fragments: `None` between messages, and
    /// `Some(None)` for one that is not text, which is dropped.
    message: Option<Option<Vec<u8>>>,
}

impl<T: Read + Write> Socket<T> {
    /// Sends the upgrade and reads the answer, which must be a 101. The `stream` may
    /// have a read timeout: it applies to the answer too.
    pub fn open(mut stream: T, request: &str) -> Result<Self, String> {
        stream
            .write_all(request.as_bytes())
            .and_then(|()| stream.flush())
            .map_err(|e| format!("cannot send the upgrade: {e}"))?;
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        let end = loop {
            if let Some(end) = find(&buf, b"\r\n\r\n") {
                break end;
            }
            if buf.len() > MAX_HEAD {
                return Err("the answer to the upgrade is too long".into());
            }
            match stream.read(&mut chunk) {
                Ok(0) => return Err("the server closed the connection before it answered".into()),
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(e) => return Err(format!("no answer to the upgrade: {e}")),
            }
        };
        let head = String::from_utf8_lossy(&buf[..end]).into_owned();
        let status = head.lines().next().unwrap_or_default();
        if !status.split(' ').nth(1).is_some_and(|code| code == "101") {
            return Err(format!("the upgrade was refused: {status}"));
        }
        buf.drain(..end + 4);
        Ok(Self {
            stream,
            buf,
            message: None,
        })
    }

    /// The stream under the socket, to set its timeouts.
    pub fn stream_mut(&mut self) -> &mut T {
        &mut self.stream
    }

    /// The next text message. Ping is answered and other frames are skipped; `Idle`
    /// when the stream's read timeout passes with no whole message.
    pub fn read_message(&mut self) -> io::Result<Next> {
        loop {
            if let Some(frame) = self.take_frame()? {
                match frame.opcode {
                    // Text starts a message, binary starts one that is dropped, and a
                    // continuation adds to the one under way.
                    0x1 => self.message = Some(Some(frame.payload)),
                    0x2 => self.message = Some(None),
                    0x0 => {
                        if let Some(Some(text)) = &mut self.message {
                            text.extend_from_slice(&frame.payload);
                            if text.len() > MAX_MESSAGE {
                                return Err(too_long());
                            }
                        }
                    }
                    0x8 => {
                        let _ = self.send(0x8, &frame.payload);
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

    /// A frame the client sends: masked, as RFC 6455 requires of a client.
    fn send(&mut self, opcode: u8, payload: &[u8]) -> io::Result<()> {
        // A control frame carries 125 bytes at most.
        let payload = &payload[..payload.len().min(125)];
        let mut mask = [0u8; 4];
        getrandom::fill(&mut mask).map_err(|e| io::Error::other(e.to_string()))?;
        let mut frame = vec![0x80 | opcode, 0x80 | payload.len() as u8];
        frame.extend_from_slice(&mask);
        frame.extend(payload.iter().zip(mask.iter().cycle()).map(|(b, m)| b ^ m));
        self.stream.write_all(&frame)?;
        self.stream.flush()
    }

    /// Sends one text message (a request of the management protocol), in a single masked
    /// frame.
    pub fn send_text(&mut self, text: &str) -> io::Result<()> {
        let payload = text.as_bytes();
        if payload.len() > MAX_MESSAGE {
            return Err(too_long());
        }
        let mut mask = [0u8; 4];
        getrandom::fill(&mut mask).map_err(|e| io::Error::other(e.to_string()))?;
        let mut frame = vec![0x81];
        match payload.len() {
            n if n < 126 => frame.push(0x80 | n as u8),
            n if n <= 0xffff => {
                frame.push(0x80 | 126);
                frame.extend_from_slice(&(n as u16).to_be_bytes());
            }
            n => {
                frame.push(0x80 | 127);
                frame.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        frame.extend_from_slice(&mask);
        frame.extend(payload.iter().zip(mask.iter().cycle()).map(|(b, m)| b ^ m));
        self.stream.write_all(&frame)?;
        self.stream.flush()
    }

    /// The first whole frame in the buffer, taken out of it.
    fn take_frame(&mut self) -> io::Result<Option<Frame>> {
        let buf = &self.buf;
        if buf.len() < 2 {
            return Ok(None);
        }
        let (fin, opcode) = (buf[0] & 0x80 != 0, buf[0] & 0x0f);
        let masked = buf[1] & 0x80 != 0;
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
                    Ok(len) if len <= MAX_MESSAGE => (len, 10),
                    _ => return Err(too_long()),
                }
            }
            short => (usize::from(short), 2),
        };
        if len > MAX_MESSAGE {
            return Err(too_long());
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

fn too_long() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "a WebSocket message longer than the sidecar reads",
    )
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
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
    fn a_message_that_is_too_long_is_an_error() {
        let mut bytes = vec![0x81, 127];
        bytes.extend_from_slice(&((MAX_MESSAGE as u64) + 1).to_be_bytes());
        let mut socket = socket(bytes);
        assert_eq!(
            socket.read_message().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
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
