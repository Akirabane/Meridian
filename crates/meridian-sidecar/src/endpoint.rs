//! The backend's local control endpoint, as a Horizon server offers it: the
//! server writes `{"transport":"tcp","port":N,"token":"..."}` to a file, listens on
//! 127.0.0.1, and takes `auth TOKEN` as the first line of a connection, then one
//! command a line, each answered with one line of JSON.
use crate::Error;
use serde::Deserialize;
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::Path;
use std::time::Duration;

/// Where the endpoint listens and the token it asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub port: u16,
    pub token: String,
}

#[derive(Deserialize)]
struct File {
    transport: String,
    port: Option<u16>,
    token: Option<String>,
}

impl Endpoint {
    /// Reads the file a Horizon server writes when its control endpoint opens.
    pub fn read(path: &Path) -> Result<Endpoint, Error> {
        let text = std::fs::read_to_string(path)?;
        Endpoint::parse(&text)
    }

    /// Reads the text of that file.
    pub fn parse(text: &str) -> Result<Endpoint, Error> {
        let bad = |why: &str| Error::Endpoint(format!("the endpoint file {why}"));
        let file: File =
            serde_json::from_str(text).map_err(|e| bad(&format!("is not JSON: {e}")))?;
        if file.transport != "tcp" {
            return Err(bad(&format!(
                "names the transport {:?}; the sidecar speaks tcp",
                file.transport
            )));
        }
        match (file.port, file.token) {
            (Some(port), Some(token)) if !token.is_empty() => Ok(Endpoint { port, token }),
            _ => Err(bad("has no port or no token")),
        }
    }

    /// Sends one command and returns the endpoint's answer. Each command has a
    /// connection of its own, so a sidecar never holds one open while idle.
    pub fn ask(&self, line: &str, timeout: Duration) -> Result<Value, Error> {
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, self.port));
        let mut stream = TcpStream::connect_timeout(&address, timeout)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        let mut reader = BufReader::new(stream.try_clone()?);
        writeln!(stream, "auth {}", self.token)?;
        let welcome = read_json(&mut reader)?;
        if welcome.get("ok") != Some(&Value::Bool(true)) {
            return Err(Error::Endpoint(format!(
                "the backend refused the sidecar's token: {welcome}"
            )));
        }
        writeln!(stream, "{line}")?;
        read_json(&mut reader)
    }
}

/// One line of JSON.
fn read_json(reader: &mut impl BufRead) -> Result<Value, Error> {
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Err(Error::Endpoint("the backend closed the connection".into()));
    }
    serde_json::from_str(line.trim())
        .map_err(|e| Error::Endpoint(format!("the backend did not answer with JSON: {e}")))
}

#[cfg(test)]
pub(crate) mod fake {
    //! A stand-in for a Horizon server's control endpoint, on 127.0.0.1.
    use super::Endpoint;
    use std::io::{BufRead, BufReader, Write};
    use std::net::{Ipv4Addr, TcpListener};
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;

    /// What the fake endpoint answers to a command line; `None` answers nothing.
    pub type Answer = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

    /// A fake endpoint that checks `token` as Horizon does, answers each command
    /// with `answer`, and keeps the command lines it was sent.
    pub fn start(
        token: &str,
        answer: Answer,
    ) -> (Endpoint, Arc<Mutex<Vec<String>>>, JoinHandle<()>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (kept, wanted) = (Arc::clone(&seen), token.to_owned());
        let thread = std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                if reader.read_line(&mut first).unwrap_or(0) == 0 {
                    continue;
                }
                if first.trim() == "quit" {
                    return;
                }
                if first.trim().strip_prefix("auth ") != Some(wanted.as_str()) {
                    let _ = writeln!(
                        stream,
                        r#"{{"error":"bad or missing token: the first line must be auth TOKEN"}}"#
                    );
                    continue;
                }
                let _ = writeln!(stream, r#"{{"ok":true}}"#);
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap_or(0) > 0 {
                    kept.lock().unwrap().push(line.trim().to_owned());
                    match answer(line.trim()) {
                        Some(reply) => {
                            let _ = writeln!(stream, "{reply}");
                        }
                        // Silent: hold the connection until the caller gives up.
                        None => std::thread::sleep(std::time::Duration::from_secs(1)),
                    }
                    line.clear();
                }
            }
        });
        (
            Endpoint {
                port,
                token: token.to_owned(),
            },
            seen,
            thread,
        )
    }

    /// Stops a fake endpoint's thread.
    pub fn stop(endpoint: &Endpoint, thread: JoinHandle<()>) {
        if let Ok(mut s) = std::net::TcpStream::connect((Ipv4Addr::LOCALHOST, endpoint.port)) {
            let _ = writeln!(s, "quit");
        }
        let _ = thread.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    const WAIT: Duration = Duration::from_secs(2);

    #[test]
    fn the_endpoint_file_is_read_as_horizon_writes_it() {
        assert_eq!(
            Endpoint::parse(r#"{"transport":"tcp","port":51234,"token":"abc"}"#).unwrap(),
            Endpoint {
                port: 51234,
                token: "abc".into()
            }
        );
        for bad in [
            r#"{"transport":"unix","path":"/tmp/x.sock"}"#,
            r#"{"transport":"tcp","port":51234}"#,
            r#"{"transport":"tcp","port":51234,"token":""}"#,
            "not json",
        ] {
            assert!(Endpoint::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_command_is_sent_after_the_token_and_answered() {
        let (endpoint, seen, thread) = fake::start(
            "secret-token",
            Arc::new(|_| Some(r#"{"held":true}"#.to_owned())),
        );
        let answer = endpoint.ask("player-drop {}", WAIT).unwrap();
        assert_eq!(answer, serde_json::json!({ "held": true }));
        assert_eq!(*seen.lock().unwrap(), ["player-drop {}"]);
        fake::stop(&endpoint, thread);
    }

    #[test]
    fn a_wrong_token_is_refused() {
        let (endpoint, seen, thread) = fake::start("right", Arc::new(|_| Some("{}".to_owned())));
        let wrong = Endpoint {
            token: "wrong".into(),
            ..endpoint.clone()
        };
        let error = wrong.ask("player-drop {}", WAIT).unwrap_err().to_string();
        assert!(error.contains("refused the sidecar's token"), "{error}");
        assert!(seen.lock().unwrap().is_empty(), "no command reached it");
        fake::stop(&endpoint, thread);
    }

    #[test]
    fn a_silent_endpoint_times_out() {
        let (endpoint, _, thread) = fake::start("t", Arc::new(|_| None));
        let started = std::time::Instant::now();
        let error = endpoint
            .ask("player-export {}", Duration::from_millis(300))
            .unwrap_err();
        assert!(matches!(error, Error::Timeout), "{error}");
        assert!(started.elapsed() < Duration::from_secs(3));
        fake::stop(&endpoint, thread);
    }

    #[test]
    fn an_answer_that_is_not_json_is_an_error() {
        let (endpoint, _, thread) = fake::start("t", Arc::new(|_| Some("hello".to_owned())));
        let error = endpoint
            .ask("player-export {}", WAIT)
            .unwrap_err()
            .to_string();
        assert!(error.contains("did not answer with JSON"), "{error}");
        fake::stop(&endpoint, thread);
    }
}
