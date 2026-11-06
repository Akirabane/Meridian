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
