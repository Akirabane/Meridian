use super::*;
use std::io::Write;
use std::net::TcpListener;

fn addr(text: &str) -> SocketAddr {
    text.parse().unwrap()
}

const SOURCE: &str = "203.0.113.9:51234";
const DESTINATION: &str = "198.51.100.2:25565";

// ---- Version 1 ----

#[test]
fn a_tcp4_line_gives_the_source_and_the_destination() {
    let header = parse_v1(b"PROXY TCP4 203.0.113.9 198.51.100.2 51234 25565").unwrap();
    assert_eq!(
        header,
        Header::Proxied {
            source: addr(SOURCE),
            destination: addr(DESTINATION),
        }
    );
}

#[test]
fn a_tcp6_line_gives_an_ipv6_source() {
    let header = parse_v1(b"PROXY TCP6 2001:db8::7 2001:db8::1 40000 25565").unwrap();
    assert_eq!(
        header,
        Header::Proxied {
            source: addr("[2001:db8::7]:40000"),
            destination: addr("[2001:db8::1]:25565"),
        }
    );
}

#[test]
fn unknown_says_nothing_about_the_player_whatever_follows() {
    assert_eq!(parse_v1(b"PROXY UNKNOWN").unwrap(), Header::Local);
    assert_eq!(
        parse_v1(b"PROXY UNKNOWN ffff::1 ffff::2 65535 65535").unwrap(),
        Header::Local
    );
    assert_eq!(
        parse_v1(b"PROXY UNKNOWN anything at all").unwrap(),
        Header::Local
    );
}

#[test]
fn a_version_1_line_that_is_not_exact_is_refused() {
    for bad in [
        &b""[..],
        b"PROXY",
        b"PROXY ",
        b"PROXY TCP4",
        b"proxy TCP4 203.0.113.9 198.51.100.2 1 2",
        b"PROXY tcp4 203.0.113.9 198.51.100.2 1 2",
        b"PROXY UDP4 203.0.113.9 198.51.100.2 1 2",
        // A family that does not match the addresses.
        b"PROXY TCP4 2001:db8::7 2001:db8::1 1 2",
        b"PROXY TCP6 203.0.113.9 198.51.100.2 1 2",
        // Bad addresses.
        b"PROXY TCP4 203.0.113.256 198.51.100.2 1 2",
        b"PROXY TCP4 203.0.113 198.51.100.2 1 2",
        b"PROXY TCP4 host.example 198.51.100.2 1 2",
        // The wrong number of fields, and doubled spaces.
        b"PROXY TCP4 203.0.113.9 198.51.100.2 1",
        b"PROXY TCP4 203.0.113.9 198.51.100.2 1 2 3",
        b"PROXY  TCP4 203.0.113.9 198.51.100.2 1 2",
        b"PROXY TCP4 203.0.113.9 198.51.100.2 1 2 ",
        // Bad ports.
        b"PROXY TCP4 203.0.113.9 198.51.100.2 65536 2",
        b"PROXY TCP4 203.0.113.9 198.51.100.2 -1 2",
        b"PROXY TCP4 203.0.113.9 198.51.100.2 01 2",
        b"PROXY TCP4 203.0.113.9 198.51.100.2 +1 2",
        b"PROXY TCP4 203.0.113.9 198.51.100.2 x 2",
        b"PROXY TCP4 203.0.113.9 198.51.100.2 100000 2",
        // Not ASCII.
        b"PROXY TCP4 203.0.113.9 198.51.100.2 1 \xff",
    ] {
        assert!(
            matches!(parse_v1(bad), Err(Error::Malformed(_))),
            "{:?}",
            String::from_utf8_lossy(bad)
        );
    }
    // Port 0 is a port.
    assert!(parse_v1(b"PROXY TCP4 203.0.113.9 198.51.100.2 0 0").is_ok());
}

// ---- Version 2 ----

fn v2(command: u8, family: u8, payload: &[u8]) -> ([u8; 16], Vec<u8>) {
    let mut head = [0u8; 16];
    head[..12].copy_from_slice(&V2_SIGNATURE);
    head[12] = 0x20 | command;
    head[13] = family;
    head[14..].copy_from_slice(&(payload.len() as u16).to_be_bytes());
    (head, payload.to_vec())
}

fn v4_payload() -> Vec<u8> {
    let mut p = Vec::new();
    p.extend([203, 0, 113, 9]);
    p.extend([198, 51, 100, 2]);
    p.extend(51234u16.to_be_bytes());
    p.extend(25565u16.to_be_bytes());
    p
}

fn v6_payload() -> Vec<u8> {
    let mut p = Vec::new();
    p.extend("2001:db8::7".parse::<Ipv6Addr>().unwrap().octets());
    p.extend("2001:db8::1".parse::<Ipv6Addr>().unwrap().octets());
    p.extend(40000u16.to_be_bytes());
    p.extend(25565u16.to_be_bytes());
    p
}

#[test]
fn a_version_2_ipv4_header_gives_the_source() {
    let (head, payload) = v2(1, 0x11, &v4_payload());
    assert_eq!(
        parse_v2(&head, &payload).unwrap(),
        Header::Proxied {
            source: addr(SOURCE),
            destination: addr(DESTINATION),
        }
    );
}

#[test]
fn a_version_2_ipv6_header_gives_the_source() {
    let (head, payload) = v2(1, 0x21, &v6_payload());
    assert_eq!(
        parse_v2(&head, &payload).unwrap(),
        Header::Proxied {
            source: addr("[2001:db8::7]:40000"),
            destination: addr("[2001:db8::1]:25565"),
        }
    );
}

/// The extensions follow the addresses and are not read.
#[test]
fn the_extensions_of_version_2_are_skipped() {
    let mut payload = v4_payload();
    // A TLV: type 0x04 (NOOP), length 3.
    payload.extend([0x04, 0x00, 0x03, 1, 2, 3]);
    let (head, payload) = v2(1, 0x11, &payload);
    assert!(matches!(
        parse_v2(&head, &payload).unwrap(),
        Header::Proxied { source, .. } if source == addr(SOURCE)
    ));
}

#[test]
fn local_unspec_and_unix_sockets_leave_the_address_of_the_connection() {
    // LOCAL, with or without anything after it.
    let (head, payload) = v2(0, 0x00, &[]);
    assert_eq!(parse_v2(&head, &payload).unwrap(), Header::Local);
    let (head, payload) = v2(0, 0x11, &v4_payload());
    assert_eq!(parse_v2(&head, &payload).unwrap(), Header::Local);
    // PROXY with an unspecified family.
    let (head, payload) = v2(1, 0x00, &[]);
    assert_eq!(parse_v2(&head, &payload).unwrap(), Header::Local);
    // PROXY over a Unix socket: 216 bytes of paths.
    let (head, payload) = v2(1, 0x31, &[0u8; 216]);
    assert_eq!(parse_v2(&head, &payload).unwrap(), Header::Local);
}

#[test]
fn a_version_2_header_that_is_not_valid_is_refused() {
    // Not enough bytes for the addresses.
    let (head, payload) = v2(1, 0x11, &v4_payload()[..11]);
    assert!(matches!(
        parse_v2(&head, &payload),
        Err(Error::Malformed(_))
    ));
    let (head, payload) = v2(1, 0x21, &v6_payload()[..35]);
    assert!(matches!(
        parse_v2(&head, &payload),
        Err(Error::Malformed(_))
    ));
    // A datagram connection, an unknown family.
    let (head, payload) = v2(1, 0x12, &v4_payload());
    assert!(matches!(
        parse_v2(&head, &payload),
        Err(Error::Malformed(_))
    ));
    let (head, payload) = v2(1, 0x41, &v4_payload());
    assert!(matches!(
        parse_v2(&head, &payload),
        Err(Error::Malformed(_))
    ));
    // A wrong version, an unknown command, a wrong signature.
    let (mut head, payload) = v2(1, 0x11, &v4_payload());
    head[12] = 0x11;
    assert!(matches!(
        parse_v2(&head, &payload),
        Err(Error::Malformed(_))
    ));
    head[12] = 0x22;
    assert!(matches!(
        parse_v2(&head, &payload),
        Err(Error::Malformed(_))
    ));
    let (mut head, payload) = v2(1, 0x11, &v4_payload());
    head[0] = b'x';
    assert!(matches!(
        parse_v2(&head, &payload),
        Err(Error::Malformed(_))
    ));
    // The payload is not as long as the header says.
    let (head, payload) = v2(1, 0x11, &v4_payload());
    assert!(matches!(
        parse_v2(&head, &payload[..payload.len() - 1]),
        Err(Error::Malformed(_))
    ));
}

#[test]
fn a_version_2_length_over_the_bound_is_refused_before_it_is_read() {
    let (mut head, _) = v2(1, 0x11, &[]);
    head[14..].copy_from_slice(&(V2_MAX_PAYLOAD as u16 + 1).to_be_bytes());
    assert!(matches!(v2_payload_length(&head), Err(Error::TooLong)));
    head[14..].copy_from_slice(&u16::MAX.to_be_bytes());
    assert!(matches!(v2_payload_length(&head), Err(Error::TooLong)));
    head[14..].copy_from_slice(&(V2_MAX_PAYLOAD as u16).to_be_bytes());
    assert_eq!(v2_payload_length(&head).unwrap(), V2_MAX_PAYLOAD);
}

// ---- Reading from a connection ----

const WAIT: Duration = Duration::from_millis(400);

/// A connection: `.0` is what the proxy would read, `.1` what the balancer writes to.
fn pair() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let writer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (reader, _) = listener.accept().unwrap();
    (reader, writer)
}

fn rest(stream: &TcpStream) -> Vec<u8> {
    stream
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let mut out = Vec::new();
    let _ = (&*stream).read_to_end(&mut out);
    out
}

const V1_LINE: &[u8] = b"PROXY TCP4 203.0.113.9 198.51.100.2 51234 25565\r\n";

#[test]
fn a_version_1_header_is_read_and_the_players_bytes_stay() {
    let (reader, mut writer) = pair();
    writer.write_all(V1_LINE).unwrap();
    writer.write_all(&[0x10, 0x00, 0xff]).unwrap();
    drop(writer);
    let header = read_header(&reader, false, WAIT).unwrap();
    assert!(matches!(
        header,
        Some(Header::Proxied { source, .. }) if source == addr(SOURCE)
    ));
    assert_eq!(rest(&reader), [0x10, 0x00, 0xff]);
}

#[test]
fn a_version_2_header_is_read_and_the_players_bytes_stay() {
    let (reader, mut writer) = pair();
    let (head, payload) = v2(1, 0x11, &v4_payload());
    writer.write_all(&head).unwrap();
    writer.write_all(&payload).unwrap();
    writer.write_all(&[0x0d, 0x00, 0x01]).unwrap();
    drop(writer);
    let header = read_header(&reader, true, WAIT).unwrap();
    assert!(matches!(
        header,
        Some(Header::Proxied { source, .. }) if source == addr(SOURCE)
    ));
    assert_eq!(rest(&reader), [0x0d, 0x00, 0x01]);
}

/// The header arrives in pieces, as a segment boundary can cut it anywhere.
#[test]
fn a_header_that_arrives_in_pieces_is_read() {
    let (reader, mut writer) = pair();
    let sender = std::thread::spawn(move || {
        for chunk in V1_LINE.chunks(7) {
            writer.write_all(chunk).unwrap();
            writer.flush().unwrap();
            std::thread::sleep(Duration::from_millis(15));
        }
        writer
    });
    let header = read_header(&reader, true, Duration::from_secs(5)).unwrap();
    assert!(matches!(header, Some(Header::Proxied { .. })));
    drop(sender.join().unwrap());

    let (reader, mut writer) = pair();
    let (head, payload) = v2(1, 0x21, &v6_payload());
    let mut all = head.to_vec();
    all.extend(payload);
    let sender = std::thread::spawn(move || {
        for chunk in all.chunks(5) {
            writer.write_all(chunk).unwrap();
            writer.flush().unwrap();
            std::thread::sleep(Duration::from_millis(15));
        }
        writer
    });
    let header = read_header(&reader, true, Duration::from_secs(5)).unwrap();
    assert!(matches!(
        header,
        Some(Header::Proxied { source, .. }) if source == addr("[2001:db8::7]:40000")
    ));
    drop(sender.join().unwrap());
}

/// A connection that does not begin with a header, where one is optional, is a player's:
/// nothing is consumed. The first bytes of a handshake share a first byte with each
/// version of the header and differ at the second.
#[test]
fn without_a_header_nothing_is_consumed_when_it_is_optional() {
    for start in [
        // A handshake of 80 bytes (0x50 is `P`), of 13 bytes (0x0d), a legacy ping, a
        // handshake like any.
        &[0x50u8, 0x00, 0x01, 0x02][..],
        &[0x0d, 0x00, 0x01, 0x02],
        &[0xfe, 0x01, 0xfa],
        &[0x10, 0x00, 0xff, 0x05],
        // Starts like the text header and then is not.
        b"PRIVATE MESSAGE",
        b"PROXZ TCP4",
    ] {
        let (reader, mut writer) = pair();
        writer.write_all(start).unwrap();
        assert!(read_header(&reader, false, WAIT).unwrap().is_none());
        // Still all there, and the stream has no timeout left from the reading.
        assert_eq!(reader.read_timeout().unwrap(), None);
        let mut got = vec![0u8; start.len()];
        (&reader).read_exact(&mut got).unwrap();
        assert_eq!(got, start);
    }
}

#[test]
fn without_a_header_it_is_refused_when_it_is_required() {
    for start in [
        &[0x10u8, 0x00, 0xff][..],
        b"GET / HTTP/1.1\r\n",
        b"PRIVATE MESSAGE",
    ] {
        let (reader, mut writer) = pair();
        writer.write_all(start).unwrap();
        assert!(matches!(
            read_header(&reader, true, WAIT),
            Err(Error::Missing)
        ));
    }
}

#[test]
fn a_malformed_header_is_refused_in_both_modes() {
    for bad in [
        &b"PROXY TCP4 not-an-address 198.51.100.2 1 2\r\n"[..],
        // A line break without the carriage return.
        b"PROXY TCP4 203.0.113.9 198.51.100.2 1 2\n",
    ] {
        for required in [false, true] {
            let (reader, mut writer) = pair();
            writer.write_all(bad).unwrap();
            assert!(
                matches!(
                    read_header(&reader, required, WAIT),
                    Err(Error::Malformed(_))
                ),
                "{:?}",
                String::from_utf8_lossy(bad)
            );
        }
    }
    // A version 2 header with a wrong version.
    let (reader, mut writer) = pair();
    let (mut head, payload) = v2(1, 0x11, &v4_payload());
    head[12] = 0x31;
    writer.write_all(&head).unwrap();
    writer.write_all(&payload).unwrap();
    assert!(matches!(
        read_header(&reader, false, WAIT),
        Err(Error::Malformed(_))
    ));
}

#[test]
fn a_header_over_the_bounds_is_refused_without_reading_it_all() {
    // A version 1 line with no end in 107 bytes.
    let (reader, mut writer) = pair();
    let mut line = b"PROXY TCP4 ".to_vec();
    line.extend(std::iter::repeat_n(b'1', 500));
    writer.write_all(&line).unwrap();
    assert!(matches!(
        read_header(&reader, true, WAIT),
        Err(Error::TooLong)
    ));
    // A line of exactly 107 bytes with its CRLF is accepted as far as its length goes.
    let (reader, mut writer) = pair();
    let mut line = b"PROXY UNKNOWN ".to_vec();
    line.extend(std::iter::repeat_n(b'x', V1_MAX - 2 - line.len()));
    line.extend(b"\r\n");
    assert_eq!(line.len(), V1_MAX);
    writer.write_all(&line).unwrap();
    assert_eq!(
        read_header(&reader, true, WAIT).unwrap(),
        Some(Header::Local)
    );
    // A version 2 header that announces more than the bound.
    let (reader, mut writer) = pair();
    let (mut head, _) = v2(1, 0x11, &[]);
    head[14..].copy_from_slice(&60000u16.to_be_bytes());
    writer.write_all(&head).unwrap();
    assert!(matches!(
        read_header(&reader, true, WAIT),
        Err(Error::TooLong)
    ));
}

#[test]
fn a_sender_that_says_nothing_or_stops_short_is_timed_out_or_closed() {
    // Nothing is sent: it times out, both when a header is required and when not.
    for required in [false, true] {
        let (reader, _writer) = pair();
        let started = Instant::now();
        assert!(matches!(
            read_header(&reader, required, Duration::from_millis(150)),
            Err(Error::Timeout)
        ));
        assert!(started.elapsed() < Duration::from_secs(5));
    }
    // Half a header, then nothing.
    let (reader, mut writer) = pair();
    writer.write_all(b"PROXY TCP4 203.0.113.9 198").unwrap();
    assert!(matches!(
        read_header(&reader, true, Duration::from_millis(150)),
        Err(Error::Timeout)
    ));
    // Only the start of the text prefix, then nothing.
    let (reader, mut writer) = pair();
    writer.write_all(b"PRO").unwrap();
    assert!(matches!(
        read_header(&reader, false, Duration::from_millis(150)),
        Err(Error::Timeout)
    ));
    // A header cut short by a close.
    let (reader, mut writer) = pair();
    writer.write_all(b"PROXY TCP4 203.0.113.9 198").unwrap();
    drop(writer);
    assert!(matches!(
        read_header(&reader, true, WAIT),
        Err(Error::Closed)
    ));
    // Closed before anything.
    let (reader, writer) = pair();
    drop(writer);
    assert!(matches!(
        read_header(&reader, true, WAIT),
        Err(Error::Closed)
    ));
}

/// A sender that drips one byte every so often cannot hold the connection past the deadline.
#[test]
fn the_deadline_is_for_the_whole_header() {
    let (reader, mut writer) = pair();
    let sender = std::thread::spawn(move || {
        for byte in V1_LINE {
            if writer.write_all(&[*byte]).is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(40));
        }
    });
    let started = Instant::now();
    let result = read_header(&reader, true, Duration::from_millis(300));
    assert!(matches!(result, Err(Error::Timeout)), "{result:?}");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    drop(reader);
    sender.join().unwrap();
}
