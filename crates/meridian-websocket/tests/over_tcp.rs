//! The crate's public side over a real socket: a server in a thread that speaks just enough
//! WebSocket to answer an upgrade and exchange frames, and the client side of the crate against it.
//! The unit tests of the crate use a buffer; this is what the sidecar (and later the proxy) does.
use meridian_websocket::{MAX_MESSAGE, Next, Socket, new_key, upgrade_request};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::JoinHandle;
use std::time::Duration;

/// Reads the upgrade request up to its blank line.
fn read_head(stream: &mut TcpStream) -> String {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).expect("the request ends");
        head.push(byte[0]);
    }
    String::from_utf8(head).expect("the request is text")
}

/// An unmasked frame, as a server sends it.
fn server_frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![0x80 | opcode];
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

/// The frame a client sent, read from `stream`: its first byte and its unmasked payload. A client
/// frame must be masked (RFC 6455, 5.1), and this asserts it.
fn client_frame(stream: &mut TcpStream) -> (u8, Vec<u8>) {
    let mut head = [0u8; 2];
    stream.read_exact(&mut head).expect("a frame comes");
    assert!(head[1] & 0x80 != 0, "a client frame is masked");
    let len = match head[1] & 0x7f {
        126 => {
            let mut two = [0u8; 2];
            stream.read_exact(&mut two).unwrap();
            usize::from(u16::from_be_bytes(two))
        }
        127 => {
            let mut eight = [0u8; 8];
            stream.read_exact(&mut eight).unwrap();
            usize::try_from(u64::from_be_bytes(eight)).unwrap()
        }
        short => usize::from(short),
    };
    let mut mask = [0u8; 4];
    stream.read_exact(&mut mask).unwrap();
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload).unwrap();
    for (b, m) in payload.iter_mut().zip(mask.iter().cycle()) {
        *b ^= m;
    }
    (head[0], payload)
}

const SWITCHING: &[u8] =
    b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n";

/// A server on the loopback that runs `serve` on its first connection.
fn serve(serve: impl FnOnce(TcpStream) + Send + 'static) -> (std::net::SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let thread = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        serve(stream);
    });
    (address, thread)
}

fn connect(address: std::net::SocketAddr) -> Result<Socket<TcpStream>, String> {
    let stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let key = new_key().unwrap();
    Socket::open(
        stream,
        &upgrade_request("127.0.0.1", address.port(), "s3cret", &key),
    )
}

#[test]
fn a_text_goes_each_way_over_a_real_socket_and_a_close_ends_it() {
    let (address, thread) = serve(|mut stream| {
        let head = read_head(&mut stream);
        assert!(head.starts_with("GET / HTTP/1.1\r\n"), "{head}");
        assert!(head.contains("Upgrade: websocket\r\n"), "{head}");
        assert!(head.contains("Authorization: Bearer s3cret\r\n"), "{head}");
        assert!(head.contains("Sec-WebSocket-Version: 13\r\n"), "{head}");
        stream.write_all(SWITCHING).unwrap();
        stream.write_all(&server_frame(0x1, b"hello")).unwrap();
        // The client's text comes back in one masked text frame.
        let (first, text) = client_frame(&mut stream);
        assert_eq!(first, 0x81);
        assert_eq!(text, b"from the client");
        stream.write_all(&server_frame(0x8, &[])).unwrap();
        // And the close is answered with a close.
        let (first, _) = client_frame(&mut stream);
        assert_eq!(first, 0x88);
    });
    let mut socket = connect(address).unwrap();
    assert_eq!(socket.read_message().unwrap(), Next::Text("hello".into()));
    socket.send_text("from the client").unwrap();
    assert_eq!(socket.read_message().unwrap(), Next::Closed);
    thread.join().unwrap();
}

#[test]
fn a_quiet_connection_is_idle_at_the_read_timeout_and_still_works_after() {
    let (address, thread) = serve(|mut stream| {
        read_head(&mut stream);
        stream.write_all(SWITCHING).unwrap();
        // Nothing for a while, then a text.
        std::thread::sleep(Duration::from_millis(600));
        stream.write_all(&server_frame(0x1, b"late")).unwrap();
        let _ = client_frame(&mut stream);
    });
    let mut socket = connect(address).unwrap();
    socket
        .stream_mut()
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    assert_eq!(socket.read_message().unwrap(), Next::Idle);
    socket
        .stream_mut()
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    assert_eq!(socket.read_message().unwrap(), Next::Text("late".into()));
    socket.send_text("done").unwrap();
    thread.join().unwrap();
}

#[test]
fn a_ping_is_answered_by_a_pong_with_the_same_bytes_and_reading_goes_on() {
    let (address, thread) = serve(|mut stream| {
        read_head(&mut stream);
        stream.write_all(SWITCHING).unwrap();
        stream
            .write_all(&server_frame(0x9, b"are you there"))
            .unwrap();
        let (first, payload) = client_frame(&mut stream);
        assert_eq!(first, 0x8A, "a pong");
        assert_eq!(payload, b"are you there");
        stream.write_all(&server_frame(0x1, b"yes")).unwrap();
    });
    let mut socket = connect(address).unwrap();
    assert_eq!(socket.read_message().unwrap(), Next::Text("yes".into()));
    thread.join().unwrap();
}

#[test]
fn an_upgrade_that_is_refused_is_an_error_that_says_so() {
    let (address, thread) = serve(|mut stream| {
        read_head(&mut stream);
        stream
            .write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n")
            .unwrap();
    });
    let refused = connect(address).err().expect("the upgrade is refused");
    assert!(
        refused.contains("refused") && refused.contains("401"),
        "{refused}"
    );
    thread.join().unwrap();
}

#[test]
fn a_server_that_hangs_up_before_it_answers_is_an_error() {
    let (address, thread) = serve(|mut stream| {
        read_head(&mut stream);
    });
    let error = connect(address).err().expect("no answer");
    assert!(error.contains("closed"), "{error}");
    thread.join().unwrap();
}

#[test]
fn a_frame_that_claims_more_than_the_limit_is_refused_before_it_is_waited_for() {
    let (address, thread) = serve(|mut stream| {
        read_head(&mut stream);
        stream.write_all(SWITCHING).unwrap();
        // The head of a text frame that says it carries one byte more than the limit.
        let mut head = vec![0x81, 127];
        head.extend_from_slice(&((MAX_MESSAGE as u64) + 1).to_be_bytes());
        stream.write_all(&head).unwrap();
        // The client reads nothing more: it must refuse at once, not wait for the payload.
        let mut rest = Vec::new();
        let _ = stream.read_to_end(&mut rest);
    });
    let mut socket = connect(address).unwrap();
    let error = socket.read_message().expect_err("too long");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    drop(socket);
    thread.join().unwrap();
}

#[test]
fn keys_are_new_each_time_and_twenty_four_characters_of_base64() {
    let (a, b) = (new_key().unwrap(), new_key().unwrap());
    assert_ne!(a, b);
    for key in [a, b] {
        assert_eq!(key.len(), 24);
        assert!(key.ends_with("=="), "{key}");
        assert!(
            key.bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'+' || c == b'/' || c == b'='),
            "{key}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// The crate against itself: its client (checked) against its server, over a real socket.

fn request_for(address: std::net::SocketAddr, extra_header: &str) -> (String, String) {
    let key = new_key().unwrap();
    let mut request = upgrade_request("127.0.0.1", address.port(), "unused", &key);
    // The extension protocol sends no secret in the upgrade: a subprotocol instead.
    request = request.replace("Authorization: Bearer unused\r\n", extra_header);
    (request, key)
}

#[test]
fn a_checked_client_and_the_crates_server_talk_both_ways_and_agree_on_a_subprotocol() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut socket = Socket::accept(stream, 256 << 10, |upgrade| {
            assert_eq!(upgrade.protocols(), ["meridian-extension"]);
            Ok(Some("meridian-extension".to_owned()))
        })
        .unwrap();
        assert_eq!(socket.protocol(), Some("meridian-extension"));
        let Next::Text(hello) = socket.read_message().unwrap() else {
            panic!("a text was expected");
        };
        socket.send_text(&format!("you said: {hello}")).unwrap();
        // A long answer, past one frame's short form.
        socket.send_text(&"x".repeat(70_000)).unwrap();
        assert_eq!(socket.read_message().unwrap(), Next::Closed);
    });
    let (request, _) = request_for(address, "Sec-WebSocket-Protocol: meridian-extension\r\n");
    let stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut socket = Socket::open_checked(stream, &request, 256 << 10).unwrap();
    assert_eq!(socket.protocol(), Some("meridian-extension"));
    socket.send_text("hello").unwrap();
    assert_eq!(
        socket.read_message().unwrap(),
        Next::Text("you said: hello".into())
    );
    assert_eq!(
        socket.read_message().unwrap(),
        Next::Text("x".repeat(70_000))
    );
    // Dropping the socket closes the stream: the server sees the end.
    drop(socket);
    server.join().unwrap();
}

#[test]
fn the_server_refuses_what_its_caller_refuses_and_the_client_sees_the_refusal() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let refused = Socket::accept(stream, 1024, |_| Err("not for you".to_owned()))
            .err()
            .expect("refused");
        assert_eq!(refused, "not for you");
    });
    let (request, _) = request_for(address, "");
    let stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let refused = Socket::open_checked(stream, &request, 1024)
        .err()
        .expect("the upgrade is refused");
    assert!(
        refused.contains("refused") && refused.contains("403"),
        "{refused}"
    );
    server.join().unwrap();
}

#[test]
fn a_raw_client_sees_unmasked_frames_and_the_server_closes_on_an_unmasked_one() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut socket = Socket::accept(stream, 1024, |_| Ok(None)).unwrap();
        socket.send_text("hi").unwrap();
        // The client's frame below is not masked: the server refuses it.
        let error = socket.read_message().unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    });
    let (request, key) = request_for(address, "");
    let mut raw = TcpStream::connect(address).unwrap();
    raw.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    raw.write_all(request.as_bytes()).unwrap();
    let head = read_head(&mut raw);
    assert!(head.starts_with("HTTP/1.1 101 "), "{head}");
    assert!(
        head.contains(&format!(
            "Sec-WebSocket-Accept: {}\r\n",
            meridian_websocket::accept_key(&key)
        )),
        "{head}"
    );
    // The server's text, as bytes: fin + text, a length without the mask bit, the text.
    let mut frame = [0u8; 4];
    raw.read_exact(&mut frame).unwrap();
    assert_eq!(&frame, b"\x81\x02hi");
    // A client frame that is not masked.
    raw.write_all(&server_frame(0x1, b"no mask")).unwrap();
    server.join().unwrap();
}
