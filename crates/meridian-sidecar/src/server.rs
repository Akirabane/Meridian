//! The sidecar's side of the signed exchange: it takes the proxy's requests,
//! passes each one to the backend's control endpoint, and signs the answer.
use crate::Error;
use crate::endpoint::Endpoint;
use crate::request::Request;
use crate::wire::{self, Direction, Expected};
use serde_json::{Value, json};
use std::io::{self, BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Where the backend's endpoint is: read again for every request, since a
/// backend that restarts listens on another port with another token.
pub type FindEndpoint = dyn Fn() -> Result<Endpoint, Error> + Send + Sync;

/// What the sidecar serves with.
pub struct Settings {
    /// The secret shared with the proxy.
    pub secret: Vec<u8>,
    pub endpoint: Box<FindEndpoint>,
    /// How long the backend has to answer one command.
    pub endpoint_timeout: Duration,
    /// How long a proxy connection may stay silent before it is closed.
    pub idle_timeout: Duration,
    /// Whether connections may come from other machines without TLS, in which
    /// case each one is logged with a warning.
    pub insecure_remote: bool,
}

/// How often the accept loop looks at the stop flag.
const POLL: Duration = Duration::from_millis(50);

/// Serves proxy connections on `listener` until `stop` is set, one thread each.
pub fn serve(listener: TcpListener, settings: Arc<Settings>, stop: &AtomicBool) -> io::Result<()> {
    listener.set_nonblocking(true)?;
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, peer)) => {
                let settings = Arc::clone(&settings);
                std::thread::Builder::new()
                    .name(format!("sidecar {peer}"))
                    .spawn(move || handle(stream, peer, &settings))?;
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => std::thread::sleep(POLL),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => {
                eprintln!("meridian-sidecar: cannot accept a connection: {e}");
                std::thread::sleep(POLL);
            }
        }
    }
    Ok(())
}

/// One proxy connection: a challenge, then signed requests until it closes or a
/// line is refused.
pub fn handle(stream: TcpStream, peer: SocketAddr, settings: &Settings) {
    if settings.insecure_remote && !peer.ip().is_loopback() {
        eprintln!(
            "meridian-sidecar: WARNING: {peer} connected without TLS: player records cross the \
             network in clear"
        );
    }
    let _ = serve_connection(stream, settings);
}

fn serve_connection(stream: TcpStream, settings: &Settings) -> Result<(), Error> {
    // An accepted socket may inherit the listener's non-blocking mode.
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(settings.idle_timeout))?;
    stream.set_write_timeout(Some(settings.idle_timeout))?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    let challenge = wire::new_challenge()?;
    writeln!(writer, "{}", wire::challenge_line(&challenge))?;
    let mut last = 0;
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let opened = wire::open(
            &settings.secret,
            &challenge,
            Direction::Request,
            line.trim(),
            Expected::After(last),
        );
        let (seq, body) = match opened {
            Ok(opened) => opened,
            Err(e) => {
                // Unsigned: whoever sent it does not share the secret, or replayed.
                writeln!(writer, "{}", json!({ "error": e.to_string() }))?;
                return Err(e.into());
            }
        };
        last = seq;
        let answer = answer(&body, settings);
        let reply = wire::seal(
            &settings.secret,
            &challenge,
            Direction::Reply,
            seq,
            &answer.to_string(),
        );
        writeln!(writer, "{reply}")?;
    }
}

/// The reply body for one request body.
fn answer(body: &str, settings: &Settings) -> Value {
    let request: Request = match serde_json::from_str(body) {
        Ok(request) => request,
        Err(e) => return json!({ "ok": false, "error": format!("not a player request: {e}") }),
    };
    let asked = (settings.endpoint)()
        .and_then(|endpoint| endpoint.ask(&request.endpoint_line(), settings.endpoint_timeout));
    match asked {
        Ok(Value::Object(map)) if map.contains_key("error") => {
            json!({ "ok": false, "error": map["error"] })
        }
        Ok(result) => json!({ "ok": true, "result": result }),
        Err(e) => json!({ "ok": false, "error": e.to_string() }),
    }
}
