//! The PROXY protocol, versions 1 and 2, as a receiver: the header that a layer 4 load
//! balancer (HAProxy, nginx `stream`, a host's balancer) writes at the start of a
//! connection to say who the player really is. The text of both versions is
//! <https://www.haproxy.org/download/2.9/doc/proxy-protocol.txt>.
//!
//! The reader is bounded on every side: a version 1 line is at most 107 bytes, a version 2
//! header is 16 bytes and at most [`V2_MAX_PAYLOAD`] more, the whole header must arrive
//! within a deadline, and it takes nothing after the header, so the bytes of the player
//! stay in the socket. The extensions of version 2 (TLVs) are skipped, not read.
//!
//! Whether to read a header from a connection at all is not decided here: that is the
//! caller's, which must do it only for the addresses it trusts, since the header is
//! whatever the sender writes.
use std::fmt;
use std::io::{self, Read};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

/// The longest version 1 line, with its line break: the text of the specification.
pub const V1_MAX: usize = 107;
/// The most bytes read after the 16 of a version 2 header. The specification allows up to
/// 65535; the addresses need at most 36, and what follows is only extensions. 536 is what
/// HAProxy itself keeps to, so that a header fits one segment.
pub const V2_MAX_PAYLOAD: usize = 536;

const V1_PREFIX: &[u8] = b"PROXY ";
const V2_SIGNATURE: [u8; 12] = *b"\r\n\r\n\0\r\nQUIT\n";

/// What a header says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Header {
    /// The connection is a player's, who is `source`, connected to `destination` on the
    /// balancer.
    Proxied {
        source: SocketAddr,
        destination: SocketAddr,
    },
    /// The balancer says nothing about the player: a health check of its own (`LOCAL`), or an
    /// address it cannot give (`UNKNOWN`, `UNSPEC`, a Unix socket). The address of the
    /// connection stands.
    Local,
}

/// Why no header was read.
#[derive(Debug)]
pub enum Error {
    /// The connection does not begin with a header, and one is required.
    Missing,
    /// It begins like a header but is not a valid one.
    Malformed(String),
    /// The header is longer than the bounds above.
    TooLong,
    /// The whole header did not arrive in time.
    Timeout,
    /// The sender closed the connection before the header was complete.
    Closed,
    Io(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Missing => f.write_str("the connection does not begin with a PROXY header"),
            Error::Malformed(why) => write!(f, "malformed PROXY header: {why}"),
            Error::TooLong => f.write_str("the PROXY header is longer than is accepted"),
            Error::Timeout => f.write_str("the PROXY header did not arrive in time"),
            Error::Closed => f.write_str("the connection closed inside the PROXY header"),
            Error::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

fn malformed(why: impl Into<String>) -> Error {
    Error::Malformed(why.into())
}

/// A version 1 line, without its `\r\n`: `PROXY TCP4 192.0.2.1 198.51.100.2 56324 443`,
/// `PROXY TCP6 ...` or `PROXY UNKNOWN ...`.
pub fn parse_v1(line: &[u8]) -> Result<Header, Error> {
    let text = std::str::from_utf8(line)
        .ok()
        .filter(|t| t.is_ascii())
        .ok_or_else(|| malformed("the line is not ASCII text"))?;
    // One space between fields, as the specification requires: an empty field is a
    // second space.
    let fields: Vec<&str> = text.split(' ').collect();
    if fields.first() != Some(&"PROXY") {
        return Err(malformed("the line does not begin with PROXY"));
    }
    match fields.get(1).copied() {
        // Whatever follows UNKNOWN is to be ignored.
        Some("UNKNOWN") => Ok(Header::Local),
        Some(family @ ("TCP4" | "TCP6")) => {
            if fields.len() != 6 {
                return Err(malformed(format!(
                    "{family} has {} fields where 6 are expected",
                    fields.len()
                )));
            }
            let address = |text: &str| -> Result<IpAddr, Error> {
                if family == "TCP4" {
                    text.parse::<Ipv4Addr>().map(IpAddr::V4)
                } else {
                    text.parse::<Ipv6Addr>().map(IpAddr::V6)
                }
                .map_err(|_| malformed(format!("{text:?} is not a {family} address")))
            };
            Ok(Header::Proxied {
                source: SocketAddr::new(address(fields[2])?, port(fields[4])?),
                destination: SocketAddr::new(address(fields[3])?, port(fields[5])?),
            })
        }
        Some(other) => Err(malformed(format!("{other:?} is not a protocol family"))),
        None => Err(malformed("the line has no protocol family")),
    }
}

/// A decimal port, with no sign, no space and no leading zero.
fn port(text: &str) -> Result<u16, Error> {
    let valid = !text.is_empty()
        && text.len() <= 5
        && text.bytes().all(|b| b.is_ascii_digit())
        && (text == "0" || !text.starts_with('0'));
    valid
        .then(|| text.parse().ok())
        .flatten()
        .ok_or_else(|| malformed(format!("{text:?} is not a port")))
}

/// How many bytes follow the 16 of a version 2 header, once the first 16 are checked: the
/// signature, the version, the command, and the length against [`V2_MAX_PAYLOAD`].
pub fn v2_payload_length(head: &[u8; 16]) -> Result<usize, Error> {
    if head[..12] != V2_SIGNATURE {
        return Err(malformed("the signature is not the PROXY protocol's"));
    }
    if head[12] >> 4 != 2 {
        return Err(malformed(format!("version {} is not 2", head[12] >> 4)));
    }
    if head[12] & 0x0f > 1 {
        return Err(malformed(format!("{} is not a command", head[12] & 0x0f)));
    }
    let length = usize::from(u16::from_be_bytes([head[14], head[15]]));
    if length > V2_MAX_PAYLOAD {
        return Err(Error::TooLong);
    }
    Ok(length)
}

/// A version 2 header: its first 16 bytes, which [`v2_payload_length`] accepted, and the
/// bytes that follow.
pub fn parse_v2(head: &[u8; 16], payload: &[u8]) -> Result<Header, Error> {
    let length = v2_payload_length(head)?;
    if payload.len() != length {
        return Err(malformed(format!(
            "{} bytes follow the header where {length} are announced",
            payload.len()
        )));
    }
    // LOCAL: the balancer's own connection, such as a health check.
    if head[12] & 0x0f == 0 {
        return Ok(Header::Local);
    }
    let (family, protocol) = (head[13] >> 4, head[13] & 0x0f);
    match (family, protocol) {
        // UNSPEC, and a Unix socket, which carries no IP address: the connection's own
        // address stands.
        (0, _) | (3, _) => Ok(Header::Local),
        (1, 1) => {
            if payload.len() < 12 {
                return Err(malformed("an IPv4 header carries 12 bytes of addresses"));
            }
            let ip = |at: usize| {
                Ipv4Addr::new(
                    payload[at],
                    payload[at + 1],
                    payload[at + 2],
                    payload[at + 3],
                )
            };
            let ports = |at: usize| u16::from_be_bytes([payload[at], payload[at + 1]]);
            Ok(Header::Proxied {
                source: SocketAddr::new(ip(0).into(), ports(8)),
                destination: SocketAddr::new(ip(4).into(), ports(10)),
            })
        }
        (2, 1) => {
            if payload.len() < 36 {
                return Err(malformed("an IPv6 header carries 36 bytes of addresses"));
            }
            let ip = |at: usize| {
                let mut octets = [0u8; 16];
                octets.copy_from_slice(&payload[at..at + 16]);
                Ipv6Addr::from(octets)
            };
            let ports = |at: usize| u16::from_be_bytes([payload[at], payload[at + 1]]);
            Ok(Header::Proxied {
                source: SocketAddr::new(ip(0).into(), ports(32)),
                destination: SocketAddr::new(ip(16).into(), ports(34)),
            })
        }
        (1 | 2, _) => Err(malformed("the header is not for a TCP connection")),
        (family, _) => Err(malformed(format!("{family} is not an address family"))),
    }
}

/// What the first bytes of a connection are.
enum Start {
    V1,
    V2,
    /// Not a header.
    Absent,
    /// A prefix of a header: more bytes are needed to tell.
    Unclear,
}

fn classify(seen: &[u8]) -> Start {
    let like = |pattern: &[u8]| {
        let n = seen.len().min(pattern.len());
        seen[..n] == pattern[..n]
    };
    if like(V1_PREFIX) {
        if seen.len() >= V1_PREFIX.len() {
            Start::V1
        } else {
            Start::Unclear
        }
    } else if like(&V2_SIGNATURE) {
        if seen.len() >= V2_SIGNATURE.len() {
            Start::V2
        } else {
            Start::Unclear
        }
    } else {
        Start::Absent
    }
}

/// How long to wait between two looks at a connection whose first bytes are a prefix of a
/// header, so as not to spin on `peek`, which returns the same bytes until more come.
const LOOK_EVERY: Duration = Duration::from_millis(2);

/// Reads the header at the start of `stream`, within `timeout` in all.
///
/// With `required` false, a connection that does not begin with a header gives `Ok(None)`
/// and nothing is consumed, so that the player's first bytes are still there. The first
/// bytes of a Minecraft connection are a packet length and a packet id, which never begin
/// like either version of the header. With `required` true, that is [`Error::Missing`].
///
/// On success the stream has no read timeout.
pub fn read_header(
    stream: &TcpStream,
    required: bool,
    timeout: Duration,
) -> Result<Option<Header>, Error> {
    let deadline = Instant::now() + timeout;
    let header = match start(stream, deadline)? {
        Start::V1 => Some(read_v1(stream, deadline)?),
        Start::V2 => Some(read_v2(stream, deadline)?),
        Start::Absent if required => return Err(Error::Missing),
        Start::Absent => None,
        Start::Unclear => unreachable!("start waits until it can tell"),
    };
    stream.set_read_timeout(None).map_err(Error::Io)?;
    Ok(header)
}

/// Looks at the first bytes without taking them, until it can tell what they are.
fn start(stream: &TcpStream, deadline: Instant) -> Result<Start, Error> {
    let mut seen = [0u8; V2_SIGNATURE.len()];
    loop {
        set_timeout(stream, deadline)?;
        let n = match stream.peek(&mut seen) {
            Ok(0) => return Err(Error::Closed),
            Ok(n) => n,
            Err(e) => return Err(io_error(e)),
        };
        match classify(&seen[..n]) {
            Start::Unclear => {
                if Instant::now() + LOOK_EVERY >= deadline {
                    return Err(Error::Timeout);
                }
                std::thread::sleep(LOOK_EVERY);
            }
            other => return Ok(other),
        }
    }
}

fn read_v1(stream: &TcpStream, deadline: Instant) -> Result<Header, Error> {
    let mut line = Vec::with_capacity(V1_MAX);
    loop {
        let mut byte = [0u8; 1];
        read_exact(stream, &mut byte, deadline)?;
        line.push(byte[0]);
        if byte[0] == b'\n' {
            break;
        }
        if line.len() >= V1_MAX {
            return Err(Error::TooLong);
        }
    }
    if line.len() < 2 || line[line.len() - 2] != b'\r' {
        return Err(malformed("the line does not end with CRLF"));
    }
    parse_v1(&line[..line.len() - 2])
}

fn read_v2(stream: &TcpStream, deadline: Instant) -> Result<Header, Error> {
    let mut head = [0u8; 16];
    read_exact(stream, &mut head, deadline)?;
    let length = v2_payload_length(&head)?;
    let mut payload = vec![0u8; length];
    read_exact(stream, &mut payload, deadline)?;
    parse_v2(&head, &payload)
}

/// Fills `buf`, in whatever pieces the bytes arrive, before `deadline`.
fn read_exact(mut stream: &TcpStream, buf: &mut [u8], deadline: Instant) -> Result<(), Error> {
    let mut filled = 0;
    while filled < buf.len() {
        set_timeout(stream, deadline)?;
        match stream.read(&mut buf[filled..]) {
            Ok(0) => return Err(Error::Closed),
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(io_error(e)),
        }
    }
    Ok(())
}

/// The read timeout for what is left of the time, at least a millisecond, since a timeout
/// of zero is refused by the system.
fn set_timeout(stream: &TcpStream, deadline: Instant) -> Result<(), Error> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(Error::Timeout);
    }
    stream
        .set_read_timeout(Some(left.max(Duration::from_millis(1))))
        .map_err(Error::Io)
}

fn io_error(e: io::Error) -> Error {
    match e.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => Error::Timeout,
        _ => Error::Io(e),
    }
}

#[cfg(test)]
mod tests;
