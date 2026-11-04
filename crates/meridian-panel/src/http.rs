//! The panel's page and its API, over HTTP on 127.0.0.1 only.
//!
//! Every API call needs the token of this run in the `X-Panel-Token` header, reads
//! included: the consoles show names and addresses. The token is in the page,
//! which a page from another site cannot read. The `Host` header must name the
//! panel itself, so a site that points its own name at 127.0.0.1 is refused too.
//! The API takes the name of a process of the bench, never a program or a path.
use crate::supervisor::{State, Supervisor};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

const PAGE: &str = include_str!("page.html");
const SCRIPT: &str = include_str!("page.js");
/// The largest request body the API reads.
const MAX_BODY: usize = 16 * 1024;
/// The longest a console command's answer is waited for; it is usually back once
/// its output has been quiet for a moment.
const ANSWER_WAIT: Duration = Duration::from_secs(8);

/// The panel's server: the bench, this run's token and the port.
pub struct Panel {
    pub supervisor: Arc<Supervisor>,
    pub token: String,
    pub port: u16,
}

/// Binds the panel's port on 127.0.0.1. There is no other address.
pub fn bind(port: u16) -> std::io::Result<TcpListener> {
    TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port)))
}

/// Serves the page and the API on `listener` until `stop` is set.
pub fn serve(listener: TcpListener, panel: Arc<Panel>, stop: &AtomicBool) -> std::io::Result<()> {
    listener.set_nonblocking(true)?;
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                let panel = Arc::clone(&panel);
                let _ = thread::Builder::new()
                    .name("panel-http".to_owned())
                    .spawn(move || {
                        let _ = handle(stream, &panel);
                    });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(50));
            }
            Err(_) => thread::sleep(Duration::from_millis(50)),
        }
    }
    Ok(())
}

/// A request, as far as the panel needs it.
#[derive(Debug, Default)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub query: String,
    pub host: Option<String>,
    pub token: Option<String>,
    pub body: Vec<u8>,
}

/// An answer: a status, a content type and a body.
#[derive(Debug, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
}

impl Response {
    fn json(status: u16, value: Value) -> Self {
        Self {
            status,
            content_type: "application/json",
            body: value.to_string().into_bytes(),
        }
    }

    fn error(status: u16, message: &str) -> Self {
        Self::json(status, json!({ "error": message }))
    }
}

fn handle(stream: TcpStream, panel: &Panel) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let response = match read_request(&mut reader) {
        Ok(request) => route(panel, &request),
        Err(message) => Response::error(400, &message),
    };
    write_response(stream, &response)
}

fn read_request(reader: &mut impl BufRead) -> Result<Request, String> {
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|e| format!("cannot read the request: {e}"))?;
    let mut parts = line.split_whitespace();
    let method = parts.next().ok_or("an empty request")?.to_owned();
    let target = parts.next().ok_or("a request without a path")?;
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let mut request = Request {
        method,
        path: path.to_owned(),
        query: query.to_owned(),
        ..Request::default()
    };
    let mut length = 0usize;
    loop {
        line.clear();
        reader
            .read_line(&mut line)
            .map_err(|e| format!("cannot read the headers: {e}"))?;
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        let Some((name, value)) = header.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            "host" => request.host = Some(value.to_owned()),
            "x-panel-token" => request.token = Some(value.to_owned()),
            "content-length" => {
                length = value.parse().map_err(|_| "a bad Content-Length")?;
            }
            _ => {}
        }
    }
    if length > MAX_BODY {
        return Err("the request body is too large".to_owned());
    }
    request.body = vec![0; length];
    reader
        .read_exact(&mut request.body)
        .map_err(|e| format!("cannot read the body: {e}"))?;
    Ok(request)
}

fn write_response(mut stream: TcpStream, response: &Response) -> std::io::Result<()> {
    let reason = match response.status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nX-Frame-Options: DENY\r\n\
         Referrer-Policy: no-referrer\r\n\
         Content-Security-Policy: default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
         connect-src 'self'; img-src 'self' data:; frame-ancestors 'none'\r\n\
         Connection: close\r\n\r\n",
        response.status,
        response.content_type,
        response.body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(&response.body)?;
    stream.flush()
}

/// Answers one request. Public so tests reach it without a socket.
pub fn route(panel: &Panel, request: &Request) -> Response {
    let allowed_hosts = [
        format!("127.0.0.1:{}", panel.port),
        format!("localhost:{}", panel.port),
    ];
    if !request
        .host
        .as_deref()
        .is_some_and(|host| allowed_hosts.iter().any(|a| a.eq_ignore_ascii_case(host)))
    {
        return Response::error(403, "the Host header must name the panel");
    }
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/") => Response {
            status: 200,
            content_type: "text/html; charset=utf-8",
            body: PAGE.replace("{{TOKEN}}", &panel.token).into_bytes(),
        },
        ("GET", "/page.js") => Response {
            status: 200,
            content_type: "text/javascript; charset=utf-8",
            body: SCRIPT.as_bytes().to_vec(),
        },
        (_, path) if path.starts_with("/api/") => {
            if !token_matches(request.token.as_deref(), &panel.token) {
                return Response::error(401, "missing or wrong panel token");
            }
            api(panel, request)
        }
        _ => Response::error(404, "not found"),
    }
}

/// Compares the whole token whatever the first difference.
fn token_matches(given: Option<&str>, expected: &str) -> bool {
    let Some(given) = given else {
        return false;
    };
    given.len() == expected.len()
        && given
            .bytes()
            .zip(expected.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

fn api(panel: &Panel, request: &Request) -> Response {
    let supervisor = &panel.supervisor;
    let body = || serde_json::from_slice::<Value>(&request.body).unwrap_or(Value::Null);
    let text = |value: &Value, key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
    let result: Result<Value, String> = match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/api/state") => Ok(state(supervisor)),
        ("GET", "/api/log") => {
            let name = query(&request.query, "name").unwrap_or_default();
            let since = query(&request.query, "since")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            supervisor.log(&name, since).map(|(lines, next)| {
                json!({
                    "lines": lines.iter().map(|l| json!({ "seq": l.seq, "text": l.text })).collect::<Vec<_>>(),
                    "next": next,
                })
            })
        }
        ("POST", "/api/action") => {
            let body = body();
            let name = text(&body, "name").unwrap_or_default();
            let action = text(&body, "action").unwrap_or_default();
            action_on(supervisor, &name, &action).map(|()| json!({ "ok": true }))
        }
        ("POST", "/api/command") => {
            let body = body();
            let name = text(&body, "name").unwrap_or_default();
            let line = text(&body, "line").unwrap_or_default();
            supervisor
                .ask(&name, &line, ANSWER_WAIT)
                .map(|lines| json!({ "lines": lines }))
        }
        ("POST", "/api/test/players") => proxy_name(supervisor)
            .and_then(|proxy| supervisor.ask(&proxy, "list", ANSWER_WAIT))
            .map(|lines| json!({ "lines": lines })),
        ("POST", "/api/test/send") => {
            let body = body();
            let player = text(&body, "player").unwrap_or_default();
            let target = text(&body, "target").unwrap_or_default();
            send_player(supervisor, &player, &target).map(|lines| json!({ "lines": lines }))
        }
        (
            _,
            "/api/state" | "/api/log" | "/api/action" | "/api/command" | "/api/test/players"
            | "/api/test/send",
        ) => return Response::error(405, "method not allowed"),
        _ => return Response::error(404, "not found"),
    };
    match result {
        Ok(value) => Response::json(200, value),
        Err(message) => Response::error(400, &message),
    }
}

fn action_on(supervisor: &Arc<Supervisor>, name: &str, action: &str) -> Result<(), String> {
    match (name, action) {
        ("all", "start") => {
            supervisor.start_all();
            Ok(())
        }
        ("all", "stop") => {
            supervisor.stop_all();
            Ok(())
        }
        ("all", _) => Err("the whole bench can be started or stopped".to_owned()),
        (name, "start") => supervisor.start(name),
        (name, "stop") => supervisor.stop_noticed(name),
        (name, "restart") => supervisor.restart(name),
        (_, other) => Err(format!("unknown action \"{other}\"")),
    }
}

/// The name of the bench's proxy.
fn proxy_name(supervisor: &Supervisor) -> Result<String, String> {
    supervisor
        .snapshots()
        .into_iter()
        .find(|s| s.kind == crate::bench::Kind::Proxy)
        .map(|s| s.name)
        .ok_or_else(|| "the bench has no proxy".to_owned())
}

/// Asks the proxy to move `player` to the server `target`, both checked first.
fn send_player(supervisor: &Supervisor, player: &str, target: &str) -> Result<Vec<String>, String> {
    if player.is_empty()
        || player.len() > 16
        || !player
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return Err("a player name is 1 to 16 letters, digits or underscores".to_owned());
    }
    let servers: Vec<String> = supervisor
        .snapshots()
        .into_iter()
        .filter(|s| s.kind == crate::bench::Kind::Server)
        .map(|s| s.name)
        .collect();
    if !servers.iter().any(|s| s == target) {
        return Err(format!("\"{target}\" is not a server of the bench"));
    }
    let proxy = proxy_name(supervisor)?;
    supervisor.ask(&proxy, &format!("send {player} {target}"), ANSWER_WAIT)
}

fn state(supervisor: &Supervisor) -> Value {
    let processes: Vec<Value> = supervisor
        .snapshots()
        .into_iter()
        .map(|s| {
            let last = s.history.last();
            json!({
                "name": s.name,
                "kind": s.kind.name(),
                "port": s.port,
                "state": s.state.name(),
                "exitCode": match s.state { State::Exited(code) => json!(code), _ => Value::Null },
                "pid": s.pid,
                "uptimeSeconds": s.uptime.map(|u| u.as_secs()),
                "cpu": last.map(|l| l.cpu),
                "memoryMb": last.map(|l| l.memory_mb),
                "history": s.history.iter().map(|h| json!([h.at, h.cpu, h.memory_mb])).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({ "processes": processes })
}

/// The value of `key` in a query string, with `+` and `%xx` decoded.
fn query(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        (k == key).then(|| decode(v))
    })
}

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let escaped = (bytes[i] == b'%')
            .then(|| bytes.get(i + 1..i + 3))
            .flatten()
            .and_then(|hex| std::str::from_utf8(hex).ok())
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match (bytes[i], escaped) {
            (_, Some(b)) => {
                out.push(b);
                i += 2;
            }
            (b'+', None) => out.push(b' '),
            (b, None) => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_values_are_decoded() {
        assert_eq!(query("name=game1&since=12", "since").as_deref(), Some("12"));
        assert_eq!(query("name=a%20b+c", "name").as_deref(), Some("a b c"));
        assert_eq!(query("name=50%", "name").as_deref(), Some("50%"));
        assert_eq!(query("name=game1", "since"), None);
    }

    #[test]
    fn the_token_is_compared_whole() {
        assert!(token_matches(Some("abc"), "abc"));
        assert!(!token_matches(Some("abd"), "abc"));
        assert!(!token_matches(Some("ab"), "abc"));
        assert!(!token_matches(None, "abc"));
    }

    #[test]
    fn a_request_is_read_with_its_headers_and_body() {
        let raw = "POST /api/action?x=1 HTTP/1.1\r\nHost: 127.0.0.1:8140\r\nX-Panel-Token: t\r\n\
                   Content-Length: 4\r\n\r\nabcd";
        let request = read_request(&mut raw.as_bytes()).unwrap();
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/api/action");
        assert_eq!(request.query, "x=1");
        assert_eq!(request.host.as_deref(), Some("127.0.0.1:8140"));
        assert_eq!(request.token.as_deref(), Some("t"));
        assert_eq!(request.body, b"abcd");
        let big = format!(
            "POST / HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            MAX_BODY + 1
        );
        assert!(read_request(&mut big.as_bytes()).is_err());
    }
}
