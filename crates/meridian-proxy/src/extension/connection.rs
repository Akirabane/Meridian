//! The connection to an extension: open it, do the hello, read it, and open it again after a wait that
//! doubles (docs/EXTENSIONS.md, 2.1, 2.2 and 3). The proxy is the client: it connects to the port the
//! extension listens on, on the loopback.
//!
//! One thread for each extension keeps the connection up and is its reader. It is the `run` loop, as
//! the sidecar's `management::run` is for a server's management protocol.

use super::link::{Info, Link};
use meridian_extension::handshake::{self, Nonce, Secret};
use meridian_extension::text;
use meridian_extension::wire::{
    self, Confirm, Discover, Hello, HelloAnswer, Id, Message, RpcError, SUBPROTOCOL, SUPPORTED,
};
use meridian_websocket::{Next, Socket, close};
use serde_json::Value;
use std::io;
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How a try ended, which decides the wait before the next.
enum End {
    /// The connection was up and has ended. The wait starts again from the first.
    Served(String),
    /// It could not be opened or the hello did not finish. The wait doubles.
    Failed(String),
    /// The other end was refused, or refused us: a wrong token, no version in common. The proxy
    /// tries again only after the longest wait.
    Refused(String),
}

/// The thread of one extension.
pub(super) fn run(link: Arc<Link>) {
    let timings = link.settings.timings.clone();
    let mut wait = timings.first_wait;
    let mut said = String::new();
    while !link.stopped() {
        let end = attempt(&link);
        let (message, longest) = match end {
            End::Served(message) => {
                wait = timings.first_wait;
                said.clear();
                (message, false)
            }
            End::Failed(message) => (message, false),
            End::Refused(message) => (message, true),
        };
        if link.stopped() {
            break;
        }
        // The same words again and again say nothing new.
        if message != said {
            crate::log::warning!(
                "extension \"{}\": {message}; trying again",
                link.settings.name
            );
            said = message;
        }
        if longest {
            wait = timings.longest_wait;
        }
        link.last_wait_ms.store(
            u64::try_from(wait.as_millis()).unwrap_or(u64::MAX),
            std::sync::atomic::Ordering::Relaxed,
        );
        link.pause(wait);
        wait = (wait * 2).min(timings.longest_wait);
    }
}

/// One try: open, hello, and serve until the connection ends.
fn attempt(link: &Arc<Link>) -> End {
    let settings = &link.settings;
    let timings = &settings.timings;
    let token = match read_token(&settings.token_file) {
        Ok(token) => token,
        Err(why) => return End::Failed(why),
    };
    let addresses = match loopback_addresses(&settings.address) {
        Ok(addresses) => addresses,
        Err(why) => return End::Failed(why),
    };
    let Some(stream) = open_stream(&addresses, link) else {
        return End::Failed(format!("cannot connect to {}", settings.address));
    };
    let address = stream.peer_addr().unwrap_or(addresses[0]);
    let host = match address.ip() {
        IpAddr::V4(ip) => ip.to_string(),
        IpAddr::V6(ip) => format!("[{ip}]"),
    };
    let key = match meridian_websocket::new_key() {
        Ok(key) => key,
        Err(e) => return End::Failed(format!("no random bytes for the upgrade: {e}")),
    };
    // The upgrade's answer is one read, which may take as long as the hello.
    let _ = stream.set_read_timeout(Some(timings.hello));
    let request = wire::upgrade_request(&host, address.port(), &key);
    let mut socket = match Socket::open_checked(stream, &request, wire::MAX_MESSAGE) {
        Ok(socket) => socket,
        Err(why) => return End::Failed(format!("the upgrade failed: {why}")),
    };
    debug_assert_eq!(socket.protocol(), Some(SUBPROTOCOL));
    let _ = socket.stream_mut().set_read_timeout(Some(timings.look));

    let info = match hello(&mut socket, link, &token) {
        Ok(info) => info,
        Err(end) => return end,
    };
    let (mut reader, writer) = match socket.split() {
        Ok(halves) => halves,
        Err(e) => return End::Failed(format!("cannot split the connection: {e}")),
    };
    let name = &settings.name;
    crate::log::info!(
        "extension \"{name}\": connected, protocol {}, \"{}\" {}, granted [{}], refused [{}]",
        info.protocol,
        info.name,
        info.version,
        info.granted.join(", "),
        info.refused.join(", ")
    );
    if info.protocol == 0 {
        crate::log::info!("extension \"{name}\": protocol 0 is experimental");
    }
    link.connected(writer.clone(), info);

    let ended = serve(&mut reader, &writer, link);
    link.disconnected();
    let _ = writer.shutdown();
    End::Served(match ended {
        Ok(()) => "the connection ended".to_owned(),
        Err(why) => format!("the connection ended: {why}"),
    })
}

/// The token, read again at each connection. The error names the file and never its content.
pub(super) fn read_token(path: &Path) -> Result<Secret, String> {
    let contents = std::fs::read(path)
        .map_err(|e| format!("cannot read the token file {}: {e}", path.display()))?;
    Secret::from_file_contents(&contents)
        .map_err(|why| format!("the token file {}: {why}", path.display()))
}

/// What `address` resolves to, if every address is on the loopback (docs/EXTENSIONS.md, 2.1). The
/// configuration checks it when it is read; this is the second check, at the point of connecting.
pub(super) fn loopback_addresses(address: &str) -> Result<Vec<SocketAddr>, String> {
    let addresses: Vec<SocketAddr> = address
        .to_socket_addrs()
        .map_err(|e| format!("cannot resolve {address}: {e}"))?
        .collect();
    if addresses.is_empty() {
        return Err(format!("{address} resolves to no address"));
    }
    if addresses.iter().any(|a| !a.ip().is_loopback()) {
        return Err(format!("{address} is not on the loopback"));
    }
    Ok(addresses)
}

fn open_stream(addresses: &[SocketAddr], link: &Link) -> Option<TcpStream> {
    let timings = &link.settings.timings;
    for address in addresses {
        if link.stopped() {
            return None;
        }
        if let Ok(stream) = TcpStream::connect_timeout(address, timings.connect) {
            let _ = stream.set_nodelay(true);
            let _ = stream.set_write_timeout(Some(timings.write));
            return Some(stream);
        }
    }
    None
}

/// The hello (docs/EXTENSIONS.md, 2.2): the proxy's nonce, the extension's proof, the proxy's proof.
/// Nothing but `rpc.discover` is accepted from the extension until both proofs are checked.
fn hello(socket: &mut Socket<TcpStream>, link: &Link, token: &Secret) -> Result<Info, End> {
    let settings = &link.settings;
    let deadline = Instant::now() + settings.timings.hello;
    let proxy_nonce = Nonce::random().map_err(End::Failed)?;
    let offered = Hello {
        protocol: SUPPORTED.to_vec(),
        proxy: env!("CARGO_PKG_VERSION").to_owned(),
        proxy_nonce: proxy_nonce.to_hex(),
    };
    let params = serde_json::to_value(&offered).unwrap_or(Value::Null);
    send(
        socket,
        &wire::request(1, wire::method::HELLO, Some(&params)),
    )?;

    let answer = match await_response(socket, link, 1, deadline)? {
        Ok(result) => result,
        Err(error) => {
            return Err(match error.supported() {
                Some(theirs) => End::Refused(format!(
                    "refused: no protocol version in common (the proxy speaks {SUPPORTED:?}, the extension {theirs:?})"
                )),
                None => End::Failed(format!(
                    "the extension refused the hello with the error {}",
                    error.code
                )),
            });
        }
    };
    let answer: HelloAnswer = wire::decode(Some(&answer))
        .map_err(|why| End::Failed(format!("invalid answer to the hello: {why}")))?;
    if !SUPPORTED.contains(&answer.protocol) {
        return Err(End::Refused(format!(
            "refused: it chose protocol {}, which the proxy did not offer",
            answer.protocol
        )));
    }
    let extension_nonce = Nonce::from_hex(&answer.extension_nonce)
        .map_err(|why| End::Failed(format!("invalid answer to the hello: {why}")))?;
    if !handshake::check_extension_proof(token, &proxy_nonce, &extension_nonce, &answer.proof) {
        // Nothing is said to the other side beyond the close.
        farewell(socket, close::POLICY);
        return Err(End::Refused("refused: wrong token".into()));
    }

    let confirm = Confirm {
        proof: handshake::proxy_proof_hex(token, &proxy_nonce, &extension_nonce),
    };
    let params = serde_json::to_value(&confirm).unwrap_or(Value::Null);
    send(
        socket,
        &wire::request(2, wire::method::CONFIRM, Some(&params)),
    )?;
    match await_response(socket, link, 2, deadline)? {
        Ok(_) => {}
        Err(error) => {
            return Err(End::Refused(format!(
                "refused: the extension did not accept the proxy's proof (error {})",
                error.code
            )));
        }
    }
    Ok(info_from(settings, answer))
}

fn info_from(settings: &super::link::Settings, answer: HelloAnswer) -> Info {
    let mut granted = Vec::new();
    let mut refused = Vec::new();
    for capability in answer.capabilities.iter().take(32) {
        let capability = text::clean(capability, 32);
        if settings.grant.contains(&capability) {
            granted.push(capability);
        } else {
            refused.push(capability);
        }
    }
    Info {
        protocol: answer.protocol,
        name: text::clean(&answer.name, 64),
        version: text::clean(&answer.version, 64),
        granted,
        refused,
        commands: answer.commands.into_iter().take(64).collect(),
    }
}

fn send(socket: &mut Socket<TcpStream>, text: &str) -> Result<(), End> {
    socket
        .send_text(text)
        .map_err(|e| End::Failed(format!("cannot send: {e}")))
}

/// Reads until the answer to request `id` comes, answering `rpc.discover` on the way. Any other
/// request from the extension, or an answer to something that was not asked, ends the hello.
fn await_response(
    socket: &mut Socket<TcpStream>,
    link: &Link,
    id: u64,
    deadline: Instant,
) -> Result<Result<Value, RpcError>, End> {
    loop {
        if link.stopped() {
            return Err(End::Failed("stopped".into()));
        }
        if Instant::now() >= deadline {
            return Err(End::Failed("no answer to the hello in time".into()));
        }
        let text = match socket.read_message() {
            Ok(Next::Text(text)) => text,
            Ok(Next::Idle) => continue,
            Ok(Next::Closed) => {
                // A close for a policy is the extension refusing the proxy's proof, as the proxy
                // refuses the extension's.
                return Err(match socket.close_code() {
                    Some(close::POLICY) => End::Refused(
                        "refused: the extension closed the connection for a policy (code 1008)"
                            .into(),
                    ),
                    Some(code) => End::Failed(format!(
                        "the extension closed the connection during the hello (code {code})"
                    )),
                    None => {
                        End::Failed("the extension closed the connection during the hello".into())
                    }
                });
            }
            Err(e) => return Err(read_failed(socket, &e)),
        };
        match Message::parse(&text) {
            Ok(Message::Response {
                id: Some(Id::Number(got)),
                outcome,
            }) if got == id => return Ok(outcome),
            Ok(Message::Request {
                id: request_id,
                method,
                ..
            }) if wire::method::is_discover(&method) => {
                // Before the hello is done, only the list of methods.
                if let Some(request_id) = request_id {
                    let document = Discover::new(0, Vec::new());
                    send(socket, &wire::result(&request_id, &document))?;
                }
            }
            Ok(_) => {
                farewell(socket, close::POLICY);
                return Err(End::Refused(
                    "refused: it sent something other than the answer before the hello was done"
                        .into(),
                ));
            }
            Err(why) => {
                farewell(socket, close::POLICY);
                return Err(End::Failed(format!("invalid message in the hello: {why}")));
            }
        }
    }
}

/// A read failed: say so, and tell the other end if the message was too long.
fn read_failed(socket: &mut Socket<TcpStream>, error: &io::Error) -> End {
    if error.kind() == io::ErrorKind::InvalidData {
        farewell(socket, close::TOO_BIG);
    }
    End::Failed(format!("cannot read: {error}"))
}

/// How long to wait for the other end's close after sending ours.
const FAREWELL: Duration = Duration::from_millis(500);

/// Closes with `code`, and waits a moment for the other end's close. A socket that is dropped with
/// bytes unread is reset, and a reset can take the close frame from the other end before it reads it.
fn farewell(socket: &mut Socket<TcpStream>, code: u16) {
    if socket.close(code, "").is_err() {
        return;
    }
    let until = Instant::now() + FAREWELL;
    while Instant::now() < until {
        match socket.read_message() {
            Ok(Next::Idle | Next::Text(_)) => {}
            // A message that was too long was refused from its header: its bytes are still to be read,
            // and `read_message` would refuse it again.
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                discard(socket.stream_mut(), until);
                break;
            }
            _ => break,
        }
    }
}

/// Reads and drops what the other end sends until it closes the connection or `until`, so that the
/// socket is not closed over unread bytes, which would reset it and could take our close frame away.
fn discard(stream: &mut impl io::Read, until: Instant) {
    let mut sink = [0u8; 4096];
    while Instant::now() < until {
        match stream.read(&mut sink) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(_) => break,
        }
    }
}

/// Reads the connection that is up until it ends. The extension's requests are answered here; an
/// answer is handed to the call that waits for it.
fn serve(
    reader: &mut meridian_websocket::Reader,
    writer: &meridian_websocket::Writer,
    link: &Link,
) -> Result<(), String> {
    while !link.stopped() {
        let text = match reader.read_message() {
            Ok(Next::Text(text)) => text,
            Ok(Next::Idle) => continue,
            Ok(Next::Closed) => return Ok(()),
            Err(e) => {
                if e.kind() == io::ErrorKind::InvalidData {
                    let _ = writer.close(close::TOO_BIG, "");
                    // Read what is left of the message, and wait a moment for the other end's close, as
                    // `farewell` does.
                    discard(reader.stream_mut(), Instant::now() + FAREWELL);
                }
                return Err(format!("cannot read: {e}"));
            }
        };
        match Message::parse(&text) {
            Ok(Message::Response { id, outcome }) => link.answered(id.as_ref(), outcome),
            Ok(Message::Request { id, method, .. }) => {
                // The orders of section 5 are not served yet: only the list of methods, which has
                // none, and an error for the rest.
                let Some(id) = id else { continue };
                let reply = if wire::method::is_discover(&method) {
                    wire::result(&id, &Discover::new(0, Vec::new()))
                } else {
                    wire::error(
                        Some(&id),
                        &RpcError::new(wire::code::METHOD_NOT_FOUND, "method not found"),
                    )
                };
                writer
                    .send_text(&reply)
                    .map_err(|e| format!("cannot send: {e}"))?;
            }
            Err(why) => {
                crate::log::debug!(
                    "extension \"{}\": a message that is not JSON-RPC: {why}",
                    link.settings.name
                );
            }
        }
    }
    Ok(())
}
