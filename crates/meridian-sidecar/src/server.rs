//! The sidecar's side of the signed exchange: it takes the proxy's requests,
//! passes each one to the backend's control endpoint, and signs the answer.
use crate::Error;
use crate::endpoint::Endpoint;
use crate::lists::{self, AccessReport};
use crate::management::Notices;
use crate::request::{NoticeHeard, NoticeReport, Request};
use crate::wire::{self, Direction, Expected};
use serde_json::{Value, json};
use std::io::{self, BufReader, Write};
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
    /// The notices heard from the backend's management protocol, when the sidecar listens
    /// to it.
    pub notices: Option<Arc<Notices>>,
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
        // A line longer than any signed one is refused as it comes: whoever sends it
        // does not have the secret, and the sidecar keeps no more of it.
        if wire::read_line(&mut reader, &mut line, wire::MAX_SIGNED_LINE)? == 0 {
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
    if let Request::Notices { after, wait_ms } = &request {
        return notices_answer(settings, *after, *wait_ms);
    }
    if let Request::AccessNotices { after, wait_ms } = &request {
        return access_notices_answer(settings, *after, *wait_ms);
    }
    if let Request::Manage { method, params } = &request {
        return manage_answer(settings, method, params);
    }
    // A request that cannot be a well-formed line never reaches the backend.
    let line = match request.endpoint_line() {
        Ok(line) => line,
        Err(why) => return json!({ "ok": false, "error": why }),
    };
    let asked =
        (settings.endpoint)().and_then(|endpoint| endpoint.ask(&line, settings.endpoint_timeout));
    match asked {
        Ok(Value::Object(map)) if map.contains_key("error") => {
            json!({ "ok": false, "error": map["error"] })
        }
        Ok(result) => json!({ "ok": true, "result": result }),
        // The backend never answered: the proxy may ask again what can be asked twice.
        Err(e) => json!({ "ok": false, "error": e.to_string(), "unreached": true }),
    }
}

/// The answer to a notices request, from what the sidecar heard of the backend's
/// management protocol. It waits up to `wait_ms` for a notice, on this connection's own
/// thread, so the other connections do not wait with it.
fn notices_answer(settings: &Settings, after: Option<u64>, wait_ms: Option<u64>) -> Value {
    let report = match &settings.notices {
        None => NoticeReport::default(),
        Some(notices) => {
            let heard = notices.heard(after, Duration::from_millis(wait_ms.unwrap_or(0)));
            NoticeReport {
                enabled: true,
                watching: heard.watching,
                last: heard.last,
                notices: heard
                    .notices
                    .into_iter()
                    .map(|n| NoticeHeard {
                        seq: n.seq,
                        line: n.line,
                        age: n.age,
                    })
                    .collect(),
            }
        }
    };
    json!({ "ok": true, "result": report.to_value() })
}

/// The answer to an `access-notices` request: what the server announced of its lists, as
/// [`notices_answer`] does for its life.
fn access_notices_answer(settings: &Settings, after: Option<u64>, wait_ms: Option<u64>) -> Value {
    let report = match &settings.notices {
        None => AccessReport::default(),
        Some(notices) => {
            let heard = notices
                .lists
                .heard(after, Duration::from_millis(wait_ms.unwrap_or(0)));
            AccessReport {
                enabled: true,
                // Asked after the wait, which may have been long enough for the
                // connection to come or to go.
                watching: notices.heard(None, Duration::ZERO).watching,
                connection: heard.connection,
                last: heard.last,
                missed: heard.missed,
                notices: heard
                    .notices
                    .into_iter()
                    .map(|n| NoticeHeard {
                        seq: n.seq,
                        line: n.line,
                        age: n.age,
                    })
                    .collect(),
            }
        }
    };
    json!({ "ok": true, "result": report.to_value() })
}

/// The answer to a `manage` request: the call is made on the connection the sidecar holds to
/// the server's management protocol. A server that did not answer is `unreached`, like a
/// backend's endpoint that did not; one that answered with an error is a refusal.
fn manage_answer(settings: &Settings, method: &str, params: &Value) -> Value {
    let Some(notices) = &settings.notices else {
        return json!({
            "ok": false,
            "error": "the sidecar does not listen to the server's management protocol \
                      (start it with --management-properties)",
        });
    };
    match notices.lists.call(method, params, lists::CALL_TIMEOUT) {
        Ok(result) => json!({ "ok": true, "result": result }),
        Err(e) if e.is_lost() => json!({ "ok": false, "error": e.to_string(), "unreached": true }),
        Err(e) => json!({ "ok": false, "error": e.to_string() }),
    }
}
