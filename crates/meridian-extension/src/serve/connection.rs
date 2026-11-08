//! The threads of a listening extension: the one that accepts, and the one that reads the connection
//! that is up. The hello is docs/EXTENSIONS.md, 2.2, seen from the extension: the proxy sends its nonce,
//! the extension answers with its own and its proof, and the proxy's proof closes the hello. Nothing but
//! `rpc.discover` (and only the methods of the hello) is answered before that.

use super::Shared;
use crate::handshake::{self, Nonce};
use crate::text;
use crate::wire::{
    self, Confirm, Discover, DiscoverMethod, Hello, HelloAnswer, MAX_MESSAGE, Message, RpcError,
    SUBPROTOCOL, SUPPORTED, code, method,
};
use meridian_websocket::{Next, Reader, Socket, Writer, close};
use serde_json::{Value, json};
use std::io::{self, Write};
use std::net::{TcpListener, TcpStream};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How long to wait for the proxy's close after sending ours.
const FAREWELL: Duration = Duration::from_millis(500);

/// How a connection ended before it was up.
enum End {
    /// The proxy was refused, or refused the extension: a wrong proof, no version in common.
    Refused(String),
    /// Anything else.
    Failed(String),
}

/// Accepts connections until the extension is stopped.
pub(super) fn run(listener: &TcpListener, shared: &Arc<Shared>) {
    let log = &*shared.extension.log;
    if let Err(e) = listener.set_nonblocking(true) {
        log.warn(&format!("cannot listen: {e}"));
        return;
    }
    let mut current: Option<JoinHandle<()>> = None;
    while !shared.stopped() {
        if current.as_ref().is_some_and(JoinHandle::is_finished)
            && let Some(done) = current.take()
        {
            let _ = done.join();
        }
        match listener.accept() {
            Ok((stream, _)) => {
                // Accepted from a listener that does not block, it may not block either.
                let _ = stream.set_nonblocking(false);
                if current.is_some() {
                    log.warn("a second connection was refused: a proxy is connected already");
                    refuse(stream);
                    continue;
                }
                let shared = Arc::clone(shared);
                match std::thread::Builder::new()
                    .name("extension-connection".into())
                    .spawn(move || connection(stream, &shared))
                {
                    Ok(thread) => current = Some(thread),
                    Err(e) => log.warn(&format!("cannot start the thread of a connection: {e}")),
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => {
                log.warn(&format!("cannot accept a connection: {e}"));
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
    if let Some(thread) = current {
        let _ = thread.join();
    }
}

/// Tells a connection that comes while another is up that the extension cannot take it.
fn refuse(mut stream: TcpStream) {
    let _ = stream
        .write_all(
            b"HTTP/1.1 503 Service Unavailable\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
        )
        .and_then(|()| stream.flush());
}

/// One connection, from the upgrade to its end.
fn connection(stream: TcpStream, shared: &Arc<Shared>) {
    let extension = &shared.extension;
    let log = &*extension.log;
    let _ = stream.set_nodelay(true);
    let _ = stream.set_write_timeout(Some(extension.timings.write));
    // The upgrade is given the time of the hello, in all: a client that sends it a byte at a time is let
    // go at that time, not held as long as it keeps sending.
    let _ = stream.set_read_timeout(Some(extension.timings.hello));
    let socket = Socket::accept_within(stream, MAX_MESSAGE, extension.timings.hello, |upgrade| {
        if upgrade.protocols().contains(&SUBPROTOCOL) {
            Ok(Some(SUBPROTOCOL.to_owned()))
        } else {
            Err(format!(
                "the upgrade does not offer the subprotocol {SUBPROTOCOL}"
            ))
        }
    });
    let mut socket = match socket {
        Ok(socket) => socket,
        Err(why) => {
            log.warn(&format!(
                "an upgrade was refused: {}",
                text::clean(&why, 200)
            ));
            return;
        }
    };
    let _ = socket
        .stream_mut()
        .set_read_timeout(Some(extension.timings.look));
    // A message that takes longer than this to arrive ends the connection, the hello's included.
    socket.set_message_timeout(Some(extension.timings.message));

    let proxy = match hello(&mut socket, shared) {
        Ok(proxy) => proxy,
        Err(End::Refused(why)) => {
            log.warn(&format!("refused the proxy: {why}"));
            return;
        }
        Err(End::Failed(why)) => {
            log.warn(&format!("the hello failed: {why}"));
            return;
        }
    };
    let (mut reader, writer) = match socket.split() {
        Ok(halves) => halves,
        Err(e) => {
            log.warn(&format!("cannot use the connection: {e}"));
            return;
        }
    };
    log.info(&format!(
        "the proxy {} is connected, protocol {}",
        text::clean(&proxy, 64),
        SUPPORTED[SUPPORTED.len() - 1]
    ));
    shared.extension.client.connected(writer.clone());
    let ended = read_requests(&mut reader, &writer, shared);
    // Before anything else: the orders that wait for an answer that will not come end now, not at their time.
    shared.extension.client.disconnected();
    match ended {
        Ok(()) => log.info("the proxy is disconnected"),
        Err(why) => log.warn(&format!("the connection ended: {why}")),
    }
    finish(&mut reader, &writer, shared);
}

/// The hello. Returns the version of Meridian the proxy says it is.
fn hello(socket: &mut Socket<TcpStream>, shared: &Shared) -> Result<String, End> {
    enum Stage {
        Hello,
        Confirm(Nonce, Nonce, String),
    }
    let extension = &shared.extension;
    let deadline = Instant::now() + extension.timings.hello;
    let mut stage = Stage::Hello;
    loop {
        let text = next_text(socket, shared, deadline)?;
        let message = match Message::parse(&text) {
            Ok(message) => message,
            Err(why) => {
                farewell(socket, close::POLICY);
                return Err(End::Failed(format!("invalid message in the hello: {why}")));
            }
        };
        match (message, &stage) {
            (
                Message::Request {
                    id: Some(id),
                    method: name,
                    ..
                },
                _,
            ) if method::is_discover(&name) => {
                // Before the hello is done, the methods of the hello and no more.
                send(
                    socket,
                    &wire::result(&id, &Discover::new(version(), hello_methods())),
                )?;
            }
            (
                Message::Request {
                    id: Some(id),
                    method: name,
                    params,
                },
                Stage::Hello,
            ) if name == method::HELLO => {
                let offered: Hello = match wire::decode(params.as_ref()) {
                    Ok(offered) => offered,
                    Err(why) => {
                        send(
                            socket,
                            &wire::error(Some(&id), &RpcError::new(code::INVALID_PARAMS, &why)),
                        )?;
                        farewell(socket, close::NORMAL);
                        return Err(End::Failed(why));
                    }
                };
                let Some(chosen) = handshake::choose_version(&offered.protocol) else {
                    send(
                        socket,
                        &wire::error(Some(&id), &RpcError::version_mismatch(SUPPORTED)),
                    )?;
                    farewell(socket, close::NORMAL);
                    let theirs: Vec<_> = offered.protocol.iter().take(8).collect();
                    return Err(End::Refused(format!(
                        "no protocol version in common (the proxy speaks {theirs:?}, the extension {SUPPORTED:?})"
                    )));
                };
                let Ok(proxy_nonce) = Nonce::from_hex(&offered.proxy_nonce) else {
                    send(
                        socket,
                        &wire::error(
                            Some(&id),
                            &RpcError::new(code::INVALID_PARAMS, "invalid proxy_nonce"),
                        ),
                    )?;
                    farewell(socket, close::NORMAL);
                    return Err(End::Failed(
                        "the proxy's nonce is not 32 bytes in hexadecimal".into(),
                    ));
                };
                let extension_nonce = Nonce::random().map_err(End::Failed)?;
                let answer = HelloAnswer {
                    protocol: chosen,
                    extension_nonce: extension_nonce.to_hex(),
                    proof: handshake::extension_proof_hex(
                        &extension.token,
                        &proxy_nonce,
                        &extension_nonce,
                    ),
                    name: extension.name.clone(),
                    version: extension.version.clone(),
                    capabilities: extension.capabilities(),
                    commands: extension.commands.clone(),
                };
                send(socket, &wire::result(&id, &answer))?;
                stage = Stage::Confirm(proxy_nonce, extension_nonce, offered.proxy);
            }
            (
                Message::Request {
                    id: Some(id),
                    method: name,
                    params,
                },
                Stage::Confirm(proxy_nonce, extension_nonce, proxy),
            ) if name == method::CONFIRM => {
                let proved = wire::decode::<Confirm>(params.as_ref()).is_ok_and(|confirm| {
                    handshake::check_proxy_proof(
                        &extension.token,
                        proxy_nonce,
                        extension_nonce,
                        &confirm.proof,
                    )
                });
                if !proved {
                    // Nothing is said to the proxy beyond the close.
                    farewell(socket, close::POLICY);
                    return Err(End::Refused(
                        "wrong token: the proof of the proxy is not the one this extension's token gives"
                            .into(),
                    ));
                }
                send(socket, &wire::result(&id, &json!({})))?;
                return Ok(proxy.clone());
            }
            _ => {
                farewell(socket, close::POLICY);
                return Err(End::Refused(
                    "it sent something other than the hello before the hello was done".into(),
                ));
            }
        }
    }
}

/// The version number `rpc.discover` gives: the newest the extension speaks.
fn version() -> u32 {
    SUPPORTED[SUPPORTED.len() - 1]
}

/// The two methods of the hello, as `rpc.discover` lists them (the golden file keeps them as they are).
pub(super) fn hello_methods() -> Vec<DiscoverMethod> {
    vec![
        DiscoverMethod::with_params(
            method::HELLO,
            &[
                (
                    "protocol",
                    true,
                    json!({"type": "array", "items": {"type": "integer"}}),
                ),
                ("proxy", true, json!({"type": "string"})),
                ("proxy_nonce", true, json!({"type": "string"})),
            ],
        ),
        DiscoverMethod::with_params(
            method::CONFIRM,
            &[("proof", true, json!({"type": "string"}))],
        ),
    ]
}

/// The document of `rpc.discover` once the hello is done: the hello's methods and the handlers'.
fn discover(shared: &Shared) -> Discover {
    let mut methods = hello_methods();
    methods.extend(
        shared
            .extension
            .handlers
            .iter()
            .map(|(_, handler)| handler.discover.clone()),
    );
    Discover::new(version(), methods)
}

/// The next text of the hello, or why there is none.
fn next_text(
    socket: &mut Socket<TcpStream>,
    shared: &Shared,
    deadline: Instant,
) -> Result<String, End> {
    loop {
        if shared.stopped() {
            return Err(End::Failed("stopped".into()));
        }
        if Instant::now() >= deadline {
            return Err(End::Failed(
                "the proxy did not finish the hello in time".into(),
            ));
        }
        match socket.read_message() {
            Ok(Next::Text(text)) => return Ok(text),
            Ok(Next::Idle) => {}
            Ok(Next::Closed) => {
                return Err(match socket.close_code() {
                    Some(code) => End::Failed(format!(
                        "the proxy closed the connection during the hello (code {code})"
                    )),
                    None => End::Failed("the proxy closed the connection during the hello".into()),
                });
            }
            Err(e) => {
                if e.kind() == io::ErrorKind::InvalidData {
                    farewell(socket, close::TOO_BIG);
                }
                return Err(End::Failed(format!("cannot read: {e}")));
            }
        }
    }
}

fn send(socket: &mut Socket<TcpStream>, text: &str) -> Result<(), End> {
    socket
        .send_text(text)
        .map_err(|e| End::Failed(format!("cannot send: {e}")))
}

/// Closes with `code`, and waits a moment for the proxy's close. A socket that is dropped with bytes
/// unread is reset, and a reset can take the close frame from the other end before it reads it.
fn farewell(socket: &mut Socket<TcpStream>, code: u16) {
    if socket.close(code, "").is_err() {
        return;
    }
    let until = Instant::now() + FAREWELL;
    while Instant::now() < until {
        match socket.read_message() {
            Ok(Next::Idle | Next::Text(_)) => {}
            // A message that was too long: its bytes are still to be read.
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                discard(socket.stream_mut(), until);
                break;
            }
            _ => break,
        }
    }
}

/// Reads and drops what the other end sends until it closes the connection or `until`. A message that
/// was refused for its length has not been read, and a socket closed over unread bytes is reset, which
/// can take the close frame from the other end before it reads it.
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

/// Reads the connection that is up until it ends, and answers each request.
fn read_requests(reader: &mut Reader, writer: &Writer, shared: &Arc<Shared>) -> Result<(), String> {
    while !shared.stopped() {
        match reader.read_message() {
            Ok(Next::Text(text)) => dispatch(&text, writer, shared),
            Ok(Next::Idle) => {}
            Ok(Next::Closed) => return Ok(()),
            Err(e) => {
                if e.kind() == io::ErrorKind::InvalidData {
                    let _ = writer.close(close::TOO_BIG, "");
                    discard(reader.stream_mut(), Instant::now() + FAREWELL);
                }
                return Err(format!("cannot read: {e}"));
            }
        }
    }
    Ok(())
}

/// Waits a moment for the proxy's close, as [`farewell`] does.
fn drain(reader: &mut Reader) {
    let until = Instant::now() + FAREWELL;
    while Instant::now() < until {
        if !matches!(reader.read_message(), Ok(Next::Idle | Next::Text(_))) {
            break;
        }
    }
}

/// The end of a connection that was up: a wait for the requests that are still running, which answer
/// before anything else (nothing may be sent after a close frame, RFC 6455, 5.5.1), then a normal close
/// when the extension is stopping.
fn finish(reader: &mut Reader, writer: &Writer, shared: &Shared) {
    let until = Instant::now() + shared.extension.timings.grace;
    while shared.in_flight.load(Ordering::SeqCst) > 0 {
        if Instant::now() >= until {
            shared.extension.log.warn(&format!(
                "{} requests were still running when the connection ended",
                shared.in_flight.load(Ordering::SeqCst)
            ));
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    if shared.stopped() && writer.close(close::NORMAL, "").is_ok() {
        drain(reader);
    }
    let _ = writer.shutdown();
}

/// What the proxy asked, once the hello is done.
fn dispatch(text: &str, writer: &Writer, shared: &Arc<Shared>) {
    let log = &*shared.extension.log;
    match Message::parse(text) {
        Err(why) => {
            log.warn(&format!("a message that is not JSON-RPC: {why}"));
            reply(
                writer,
                shared,
                &wire::error(
                    None,
                    &RpcError::new(code::INVALID_REQUEST, "invalid message"),
                ),
            );
        }
        // The answer to an order of the extension's, for the request that waits for it (if one still does).
        Ok(Message::Response { id, outcome }) => {
            shared.extension.client.answered(id.as_ref(), outcome)
        }
        // Version 0 has no notification from the proxy.
        Ok(Message::Request { id: None, .. }) => {}
        Ok(Message::Request {
            id: Some(id),
            method: name,
            params,
        }) => {
            if method::is_discover(&name) {
                reply(writer, shared, &wire::result(&id, &discover(shared)));
            } else if name == method::HELLO || name == method::CONFIRM {
                let error = RpcError::new(code::INVALID_REQUEST, "the hello is done");
                reply(writer, shared, &wire::error(Some(&id), &error));
            } else if let Some(index) = shared
                .extension
                .handlers
                .iter()
                .position(|(known, _)| *known == name)
            {
                run_handler(index, id, params, writer, shared);
            } else {
                let error = RpcError::new(code::METHOD_NOT_FOUND, "method not found");
                reply(writer, shared, &wire::error(Some(&id), &error));
            }
        }
    }
}

/// Counts a request that runs, and stops counting it when it ends, however it ends.
struct Running(Arc<Shared>);

impl Drop for Running {
    fn drop(&mut self) {
        self.0.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Runs handler number `index` on a thread of its own, if there is a place for one more.
fn run_handler(
    index: usize,
    id: wire::Id,
    params: Option<Value>,
    writer: &Writer,
    shared: &Arc<Shared>,
) {
    let extension = &shared.extension;
    if shared.in_flight.fetch_add(1, Ordering::SeqCst) >= extension.max_in_flight {
        shared.in_flight.fetch_sub(1, Ordering::SeqCst);
        extension.log.warn(&format!(
            "a request was refused: {} are running already",
            extension.max_in_flight
        ));
        let error = RpcError::new(code::INTERNAL_ERROR, "too many requests");
        reply(writer, shared, &wire::error(Some(&id), &error));
        return;
    }
    let guard = Running(Arc::clone(shared));
    let writer = writer.clone();
    let spawned = std::thread::Builder::new()
        .name("extension-request".into())
        .spawn({
            let id = id.clone();
            let writer = writer.clone();
            move || {
                let shared = Arc::clone(&guard.0);
                let (name, handler) = &shared.extension.handlers[index];
                let outcome = catch_unwind(AssertUnwindSafe(|| (handler.call)(params)));
                let answer = match outcome {
                    Ok(Ok(result)) => wire::result(&id, &result),
                    Ok(Err(error)) => wire::error(Some(&id), &error),
                    Err(_) => {
                        shared
                            .extension
                            .log
                            .warn(&format!("the handler of {name} panicked"));
                        let error = RpcError::new(code::INTERNAL_ERROR, "internal error");
                        wire::error(Some(&id), &error)
                    }
                };
                // An answer too long for a message is told as an error, not left unanswered.
                let answer = if answer.len() > MAX_MESSAGE {
                    shared.extension.log.warn(&format!(
                        "the answer of {name} was longer than a message may be"
                    ));
                    wire::error(
                        Some(&id),
                        &RpcError::new(code::INTERNAL_ERROR, "the answer is too long"),
                    )
                } else {
                    answer
                };
                // The place is given back before the answer goes: a proxy that has its answer may send the
                // next request at once, and must find the place free.
                drop(guard);
                reply(&writer, &shared, &answer);
            }
        });
    if let Err(e) = spawned {
        // The closure, and the place it held, were dropped with the failed spawn.
        extension
            .log
            .warn(&format!("cannot start the thread of a request: {e}"));
        let error = RpcError::new(code::INTERNAL_ERROR, "internal error");
        reply(&writer, shared, &wire::error(Some(&id), &error));
    }
}

/// Sends an answer. One that is too long for a message is told as an error, and one that cannot be
/// written ends nothing here: the reader sees the connection break.
fn reply(writer: &Writer, shared: &Shared, text: &str) {
    if let Err(e) = writer.send_text(text) {
        let log = &*shared.extension.log;
        if e.kind() == io::ErrorKind::InvalidInput || e.kind() == io::ErrorKind::InvalidData {
            log.warn("an answer was longer than a message may be and was not sent");
        } else {
            log.warn(&format!("cannot send an answer: {e}"));
        }
    }
}
