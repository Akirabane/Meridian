//! The upgrade of RFC 6455 (section 4): what a client checks in the answer, when it wants it checked,
//! and what a server reads in the request and answers.
//!
//! `Sec-WebSocket-Accept` is the base64 of the SHA-1 of the request's `Sec-WebSocket-Key` followed by a
//! fixed GUID. It proves that the other end is a WebSocket server and read this request; it is not
//! authentication (the extension protocol proves the token in its first messages, section 2.2 of
//! docs/EXTENSIONS.md).
use super::{MAX_HEAD, Role, Socket, base64, read_head};
use base64::Engine;
use sha1::{Digest, Sha1};
use std::io::{Read, Write};

/// The GUID of RFC 6455, section 1.3.
const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// The `Sec-WebSocket-Accept` that a server answers to a request with this `Sec-WebSocket-Key`.
pub fn accept_key(key: &str) -> String {
    let mut sha = Sha1::new();
    sha.update(key.as_bytes());
    sha.update(GUID.as_bytes());
    base64(&sha.finalize())
}

/// The value of the first header `name` (any case) of an HTTP head, without the line it starts with.
pub(crate) fn header_of(head: &str, name: &str) -> Option<String> {
    head.lines().skip(1).find_map(|line| {
        let (n, v) = line.split_once(':')?;
        n.trim()
            .eq_ignore_ascii_case(name)
            .then(|| v.trim().to_owned())
    })
}

/// Whether the comma-separated `value` of a header has `token` (any case).
fn has_token(value: &str, token: &str) -> bool {
    value
        .split(',')
        .any(|t| t.trim().eq_ignore_ascii_case(token))
}

/// What a client checks in the 101 it was answered (RFC 6455, 4.2.2), for the request it sent: the
/// upgrade headers, the accept key and the subprotocol. A request that offered subprotocols is
/// answered with one of them: RFC 6455 lets a server name none and the connection go on, but a client
/// that offered one wants it, and an answer without it is a server that is not the one it meant to
/// reach. The subprotocol the server chose, if any.
pub(crate) fn check_answer(request: &str, head: &str) -> Result<Option<String>, String> {
    let key = header_of(request, "sec-websocket-key")
        .ok_or("the upgrade request has no Sec-WebSocket-Key to check the answer against")?;
    if !header_of(head, "upgrade").is_some_and(|v| v.eq_ignore_ascii_case("websocket")) {
        return Err("the answer to the upgrade does not say Upgrade: websocket".into());
    }
    if !header_of(head, "connection").is_some_and(|v| has_token(&v, "upgrade")) {
        return Err("the answer to the upgrade does not say Connection: Upgrade".into());
    }
    match header_of(head, "sec-websocket-accept") {
        Some(got) if got == accept_key(&key) => {}
        Some(_) => {
            return Err(
                "the Sec-WebSocket-Accept of the answer is not the one the key calls for".into(),
            );
        }
        None => return Err("the answer to the upgrade has no Sec-WebSocket-Accept".into()),
    }
    let protocol = header_of(head, "sec-websocket-protocol");
    let offered = header_of(request, "sec-websocket-protocol").unwrap_or_default();
    match &protocol {
        Some(chosen) if !has_token(&offered, chosen) => {
            return Err("the server chose a subprotocol that the request did not offer".into());
        }
        None if !offered.trim().is_empty() => {
            return Err("the server chose none of the subprotocols the request offered".into());
        }
        _ => {}
    }
    Ok(protocol)
}

/// The upgrade request a server read, for it to decide whether to accept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Upgrade {
    target: String,
    headers: Vec<(String, String)>,
}

impl Upgrade {
    /// The path asked for (`GET <target> HTTP/1.1`).
    pub fn target(&self) -> &str {
        &self.target
    }

    /// The value of the first header `name` (any case), if the request has it.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// The subprotocols the client offers (`Sec-WebSocket-Protocol`), in its order.
    pub fn protocols(&self) -> Vec<&str> {
        self.header("sec-websocket-protocol")
            .map(|v| {
                v.split(',')
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// A request that is an upgrade as RFC 6455 (4.2.1) asks: `GET`, HTTP/1.1, `Upgrade: websocket`,
/// `Connection: Upgrade`, version 13, and a key of 16 bytes in base64.
fn parse_request(head: &str) -> Result<(Upgrade, String), String> {
    let mut lines = head.lines();
    let first = lines.next().unwrap_or_default();
    let mut parts = first.split_whitespace();
    let (method, target, version) = (parts.next(), parts.next(), parts.next());
    if method != Some("GET") || version != Some("HTTP/1.1") || parts.next().is_some() {
        return Err(format!("not an upgrade request: {first}"));
    }
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(n, v)| (n.trim().to_owned(), v.trim().to_owned()))
        .collect();
    let upgrade = Upgrade {
        target: target.unwrap_or("/").to_owned(),
        headers,
    };
    if !upgrade
        .header("upgrade")
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
    {
        return Err("the request does not say Upgrade: websocket".into());
    }
    if !upgrade
        .header("connection")
        .is_some_and(|v| has_token(v, "upgrade"))
    {
        return Err("the request does not say Connection: Upgrade".into());
    }
    if upgrade.header("sec-websocket-version") != Some("13") {
        return Err("the request does not ask for WebSocket version 13".into());
    }
    let key = upgrade
        .header("sec-websocket-key")
        .ok_or("the request has no Sec-WebSocket-Key")?
        .to_owned();
    // Strictly: the padding and the unused bits of the last character are as base64 writes them.
    if !base64::engine::general_purpose::STANDARD
        .decode(&key)
        .is_ok_and(|bytes| bytes.len() == 16)
    {
        return Err("the Sec-WebSocket-Key is not 16 bytes in base64".into());
    }
    Ok((upgrade, key))
}

impl<T: Read + Write> Socket<T> {
    /// The server side: reads the upgrade request from `stream`, and when it is one, asks `choose`
    /// whether to accept it. `choose` gives the subprotocol to answer (one the client offered) or
    /// none, or the reason for refusing; the client is then told `403` and the reason is returned. A
    /// request that is not a valid upgrade is told `400`. On acceptance the answer is the `101` with the
    /// `Sec-WebSocket-Accept` the client expects, and the socket reads masked frames (it refuses an
    /// unmasked one) and sends unmasked ones. Messages are at most `max_message` bytes.
    ///
    /// The `stream` may have a read timeout: it applies to the request too, and a client that does not
    /// send one is given up on at that time.
    pub fn accept(
        mut stream: T,
        max_message: usize,
        choose: impl FnOnce(&Upgrade) -> Result<Option<String>, String>,
    ) -> Result<Self, String> {
        let mut buf = Vec::new();
        let end = read_head(
            &mut stream,
            &mut buf,
            &format!("the upgrade request is longer than {MAX_HEAD} bytes"),
            "the client closed the connection before it asked",
            "no upgrade request",
        )?;
        let head = String::from_utf8_lossy(&buf[..end]).into_owned();
        // What follows the request is the client's first frames, which it may send at once.
        buf.drain(..end + 4);
        let (upgrade, key) = match parse_request(&head) {
            Ok(parsed) => parsed,
            Err(why) => {
                refuse(&mut stream, 400, "Bad Request");
                return Err(why);
            }
        };
        // A browser sends `Origin` with every WebSocket it opens, and a program does not. A page open on
        // the same machine could reach a port on the loopback; the proofs of the handshake already keep
        // it out, and this keeps it from holding the connection.
        if upgrade.header("origin").is_some() {
            refuse(&mut stream, 403, "Forbidden");
            return Err("the upgrade carries an Origin header, which a browser sends".into());
        }
        let protocol = match choose(&upgrade) {
            Ok(protocol) => protocol,
            Err(why) => {
                refuse(&mut stream, 403, "Forbidden");
                return Err(why);
            }
        };
        if let Some(chosen) = &protocol
            && !upgrade.protocols().contains(&chosen.as_str())
        {
            refuse(&mut stream, 400, "Bad Request");
            return Err(format!("the subprotocol \"{chosen}\" was not offered"));
        }
        let mut answer = format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Accept: {}\r\n",
            accept_key(&key)
        );
        if let Some(chosen) = &protocol {
            answer.push_str(&format!("Sec-WebSocket-Protocol: {chosen}\r\n"));
        }
        answer.push_str("\r\n");
        stream
            .write_all(answer.as_bytes())
            .and_then(|()| stream.flush())
            .map_err(|e| format!("cannot answer the upgrade: {e}"))?;
        Ok(Self {
            stream,
            role: Role::Server,
            protocol,
            buf,
            message: None,
            max_message,
        })
    }
}

/// Tells the client the upgrade is refused. A failure to write is of no use to anyone: the connection
/// is dropped by the caller either way.
fn refuse<T: Write>(stream: &mut T, code: u16, reason: &str) {
    let _ = stream
        .write_all(
            format!("HTTP/1.1 {code} {reason}\r\nConnection: close\r\nContent-Length: 0\r\n\r\n")
                .as_bytes(),
        )
        .and_then(|()| stream.flush());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Next, new_key, upgrade_request};
    use std::io::{self, Cursor};

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

    const KEY: &str = "dGhlIHNhbXBsZSBub25jZQ==";

    fn request(extra: &str) -> String {
        format!(
            "GET /chat HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: {KEY}\r\nSec-WebSocket-Version: 13\r\n{extra}\r\n"
        )
    }

    fn served(bytes: &str) -> Pipe {
        Pipe {
            read: Cursor::new(bytes.as_bytes().to_vec()),
            written: Vec::new(),
        }
    }

    fn masked(opcode: u8, payload: &[u8]) -> Vec<u8> {
        let mask = [1u8, 2, 3, 4];
        let mut out = vec![0x80 | opcode, 0x80 | payload.len() as u8];
        out.extend_from_slice(&mask);
        out.extend(payload.iter().zip(mask.iter().cycle()).map(|(b, m)| b ^ m));
        out
    }

    #[test]
    fn the_accept_key_is_the_one_of_rfc_6455() {
        // RFC 6455, section 1.3.
        assert_eq!(accept_key(KEY), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    #[test]
    fn a_checked_client_takes_the_right_answer_and_refuses_the_others() {
        let asking = request("");
        let answer = |extra: &str| {
            format!(
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n{extra}\r\n"
            )
        };
        let open = |extra: &str| Socket::open_checked(served(&answer(extra)), &asking, 1024);
        let good = format!("Sec-WebSocket-Accept: {}\r\n", accept_key(KEY));
        assert!(open(&good).is_ok());
        // Header names and the values of Upgrade and Connection are not case sensitive.
        let shouted = format!(
            "HTTP/1.1 101 Switching Protocols\r\nUPGRADE: WebSocket\r\nconnection: keep-alive, Upgrade\r\n\
             sec-websocket-accept: {}\r\n\r\n",
            accept_key(KEY)
        );
        assert!(Socket::open_checked(served(&shouted), &asking, 1024).is_ok());
        let wrong = open("Sec-WebSocket-Accept: AAAAAAAAAAAAAAAAAAAAAAAAAAA=\r\n");
        assert!(
            wrong
                .err()
                .unwrap()
                .contains("not the one the key calls for")
        );
        let none = open("");
        assert!(none.err().unwrap().contains("no Sec-WebSocket-Accept"));
        // Without the headers of an upgrade.
        let plain = "HTTP/1.1 101 Switching Protocols\r\n\r\n";
        assert!(Socket::open_checked(served(plain), &asking, 1024).is_err());
        // The unchecked client takes all of these, as the sidecar always has.
        for bytes in [answer(""), plain.to_owned()] {
            assert!(Socket::open(served(&bytes), &asking).is_ok());
        }
    }

    #[test]
    fn a_checked_client_wants_a_key_to_check_against_and_an_offered_subprotocol() {
        let no_key = "GET / HTTP/1.1\r\nUpgrade: websocket\r\n\r\n";
        let answer = format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Accept: {}\r\n\r\n",
            accept_key(KEY)
        );
        assert!(
            Socket::open_checked(served(&answer), no_key, 1024)
                .err()
                .unwrap()
                .contains("no Sec-WebSocket-Key")
        );
        let asking = request("Sec-WebSocket-Protocol: meridian-extension\r\n");
        let with = |chosen: &str| {
            format!(
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
                 Sec-WebSocket-Accept: {}\r\nSec-WebSocket-Protocol: {chosen}\r\n\r\n",
                accept_key(KEY)
            )
        };
        let socket =
            Socket::open_checked(served(&with("meridian-extension")), &asking, 1024).unwrap();
        assert_eq!(socket.protocol(), Some("meridian-extension"));
        let other = Socket::open_checked(served(&with("chat")), &asking, 1024);
        assert!(other.err().unwrap().contains("did not offer"));
        // An answer that names none, to a request that offered one, is refused.
        let none = Socket::open_checked(served(&answer), &asking, 1024);
        assert!(none.err().unwrap().contains("chose none"));
        // A request that offered none is answered with none, as before.
        assert!(Socket::open_checked(served(&answer), &request(""), 1024).is_ok());
    }

    #[test]
    fn a_server_answers_a_valid_upgrade_with_the_accept_key_and_the_subprotocol() {
        let asked = request("Sec-WebSocket-Protocol: other, meridian-extension\r\n");
        let mut seen = None;
        let socket = Socket::accept(served(&asked), 1024, |upgrade| {
            seen = Some(upgrade.clone());
            assert_eq!(upgrade.target(), "/chat");
            assert_eq!(upgrade.protocols(), ["other", "meridian-extension"]);
            assert_eq!(upgrade.header("HOST"), Some("localhost"));
            Ok(Some("meridian-extension".into()))
        })
        .unwrap();
        assert!(seen.is_some());
        assert_eq!(socket.protocol(), Some("meridian-extension"));
        let Socket { stream, .. } = socket;
        let said = String::from_utf8(stream.written).unwrap();
        assert!(
            said.starts_with("HTTP/1.1 101 Switching Protocols\r\n"),
            "{said}"
        );
        assert!(
            said.contains("Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n"),
            "{said}"
        );
        assert!(
            said.contains("Sec-WebSocket-Protocol: meridian-extension\r\n"),
            "{said}"
        );
        assert!(said.ends_with("\r\n\r\n"));
    }

    #[test]
    fn a_request_that_is_not_an_upgrade_is_told_400_and_a_refusal_403() {
        let broken = |asked: String| {
            let mut out = Vec::new();
            let error = Socket::accept(served(&asked), 1024, |_| Ok(None))
                .err()
                .expect("refused");
            // What the client was told: re-run on a pipe we can read back.
            let mut pipe = served(&asked);
            let _ = Socket::accept(&mut pipe, 1024, |_| Ok(None));
            out.extend_from_slice(&pipe.written);
            (error, String::from_utf8(out).unwrap())
        };
        for (asked, why) in [
            (request("").replace("GET", "POST"), "not an upgrade request"),
            (
                request("").replace("HTTP/1.1", "HTTP/1.0"),
                "not an upgrade request",
            ),
            (
                request("").replace("Upgrade: websocket", "Upgrade: h2c"),
                "Upgrade: websocket",
            ),
            (
                request("").replace("Connection: Upgrade", "Connection: close"),
                "Connection: Upgrade",
            ),
            (
                request("").replace("Version: 13", "Version: 8"),
                "version 13",
            ),
            (request("").replace(KEY, "short"), "16 bytes in base64"),
            // 24 characters that are not what base64 writes for 16 bytes (the last bits are not zero).
            (
                request("").replace(KEY, "AAAAAAAAAAAAAAAAAAAAA/=="),
                "16 bytes in base64",
            ),
            (
                request("").replace(KEY, "AAAAAAAAAAAAAAAAAAAAAAAA"),
                "16 bytes in base64",
            ),
            (
                request("").replace(&format!("Sec-WebSocket-Key: {KEY}\r\n"), ""),
                "no Sec-WebSocket-Key",
            ),
        ] {
            let (error, told) = broken(asked);
            assert!(error.contains(why), "{error}");
            assert!(told.starts_with("HTTP/1.1 400 Bad Request\r\n"), "{told}");
        }
        let mut pipe = served(&request(""));
        let error = Socket::accept(&mut pipe, 1024, |_| Err("wrong token".into()))
            .err()
            .unwrap();
        assert_eq!(error, "wrong token");
        let told = String::from_utf8(pipe.written).unwrap();
        assert!(told.starts_with("HTTP/1.1 403 Forbidden\r\n"), "{told}");
        // A browser's upgrade carries `Origin`: it is refused with 403, and the caller is not asked.
        let mut pipe = served(&request("Origin: http://localhost:8000\r\n"));
        let mut asked = false;
        let error = Socket::accept(&mut pipe, 1024, |_| {
            asked = true;
            Ok(None)
        })
        .err()
        .unwrap();
        assert!(error.contains("Origin"), "{error}");
        assert!(!asked);
        let told = String::from_utf8(pipe.written).unwrap();
        assert!(told.starts_with("HTTP/1.1 403 Forbidden\r\n"), "{told}");
        // A subprotocol the client did not offer cannot be answered.
        let mut pipe = served(&request("Sec-WebSocket-Protocol: chat\r\n"));
        let error = Socket::accept(&mut pipe, 1024, |_| Ok(Some("meridian-extension".into())))
            .err()
            .unwrap();
        assert!(error.contains("was not offered"), "{error}");
    }

    #[test]
    fn a_server_that_is_sent_nothing_or_too_much_gives_up() {
        let error = Socket::accept(served(""), 1024, |_| Ok(None))
            .err()
            .unwrap();
        assert!(error.contains("closed the connection"), "{error}");
        let long = format!("GET / HTTP/1.1\r\nX: {}", "a".repeat(MAX_HEAD + 100));
        let error = Socket::accept(served(&long), 1024, |_| Ok(None))
            .err()
            .unwrap();
        assert!(error.contains("longer than"), "{error}");
    }

    #[test]
    fn the_server_reads_masked_frames_sends_unmasked_ones_and_refuses_an_unmasked_client() {
        // The client's first frame comes with the request, in the same read.
        let mut bytes = request("").into_bytes();
        bytes.extend(masked(0x1, b"first"));
        bytes.extend(masked(0x9, b"ping"));
        let mut socket = Socket::accept(
            Pipe {
                read: Cursor::new(bytes),
                written: Vec::new(),
            },
            1024,
            |_| Ok(None),
        )
        .unwrap();
        assert_eq!(socket.read_message().unwrap(), Next::Text("first".into()));
        socket.send_text("answer").unwrap();
        // The ping is answered when the next message is waited for, and ends in a close of the stream.
        assert_eq!(socket.read_message().unwrap(), Next::Closed);
        let Socket { stream, .. } = socket;
        let written = stream.written;
        let at = written.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
        // 0x81 (text, fin), 6 (not masked: the high bit is clear), the text.
        assert_eq!(&written[at..at + 8], b"\x81\x06answer");
        // And the pong: 0x8A, 4, the same bytes, unmasked.
        assert_eq!(&written[at + 8..at + 14], b"\x8A\x04ping");

        let mut bytes = request("").into_bytes();
        bytes.extend([0x81, 0x03, b'a', b'b', b'c']);
        let mut socket = Socket::accept(
            Pipe {
                read: Cursor::new(bytes),
                written: Vec::new(),
            },
            1024,
            |_| Ok(None),
        )
        .unwrap();
        let error = socket.read_message().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("not masked"), "{error}");
    }

    #[test]
    fn a_new_key_is_one_a_server_accepts() {
        let key = new_key().unwrap();
        let asked = upgrade_request("localhost", 1, "token", &key);
        let socket = Socket::accept(served(&asked), 1024, |_| Ok(None)).unwrap();
        let Socket { stream, .. } = socket;
        let said = String::from_utf8(stream.written).unwrap();
        assert!(said.contains(&format!("Sec-WebSocket-Accept: {}\r\n", accept_key(&key))));
    }
}
