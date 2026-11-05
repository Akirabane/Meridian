//! What a switch needs of the servers, for the load runs: the hand-overs the servers share, a
//! stand-in for each server's control endpoint (the five player requests of Horizon's held
//! player), and the sidecar itself, the real one, serving the proxy in front of that endpoint.
//!
//! The proxy under load is the real `meridian` program, so a switch goes through its sidecar
//! client, the signed exchange, the sidecar's server, and a control endpoint that answers as
//! the contract documents (`meridian-sidecar`'s tests hold the same answers, line by line).
use horizon_protocol::connection::Closer;
use meridian_sidecar::endpoint::Endpoint;
use meridian_sidecar::server::{Settings, serve};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// The secret the proxy and every sidecar of a run share.
pub const SECRET: &[u8] = b"meridian-load-sidecar-secret-0001";

/// The token of every stand-in endpoint.
const TOKEN: &str = "meridian-load-endpoint-token";

/// What the proxy's requests tell a held player.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    /// The record was imported: the held player gets the changes.
    Import,
    /// The player is placed: it is live.
    Confirm,
}

/// The hand-overs under way, shared by both servers and both endpoints: a player is keyed by
/// the 32 hexadecimal digits of their unique id.
#[derive(Default)]
pub struct Transfers {
    senders: Mutex<HashMap<String, Sender<Signal>>>,
    receivers: Mutex<HashMap<String, Receiver<Signal>>>,
    /// The connection of each player on each server, to close when the server lets them go.
    live: Mutex<HashMap<(&'static str, String), Closer>>,
    handovers: AtomicU64,
    /// What the endpoints were asked, by request.
    pub asked: AsRequests,
}

impl std::fmt::Debug for Transfers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Transfers")
            .field("asked", &self.asked)
            .finish_non_exhaustive()
    }
}

/// How many times each request reached an endpoint.
#[derive(Debug, Default)]
pub struct AsRequests {
    pub expect: AtomicU64,
    pub export: AtomicU64,
    pub import: AtomicU64,
    pub confirm: AtomicU64,
    pub release: AtomicU64,
    pub drop: AtomicU64,
}

impl Transfers {
    /// The player is expected: their next login is a held player's.
    fn expect(&self, uuid: &str) {
        let (tx, rx) = channel();
        self.senders.lock().unwrap().insert(uuid.to_owned(), tx);
        self.receivers.lock().unwrap().insert(uuid.to_owned(), rx);
    }

    /// The signals of a player who was expected, taken once, as their login arrives.
    pub fn take_expected(&self, uuid: &str) -> Option<Receiver<Signal>> {
        self.receivers.lock().unwrap().remove(uuid)
    }

    fn signal(&self, uuid: &str, signal: Signal) {
        if let Some(tx) = self.senders.lock().unwrap().get(uuid) {
            let _ = tx.send(signal);
        }
    }

    /// A player on `server`, whose connection closes when the server lets them go.
    pub fn keep(&self, server: &'static str, uuid: &str, closer: Closer) {
        self.live
            .lock()
            .unwrap()
            .insert((server, uuid.to_owned()), closer);
    }

    /// The player left `server` by themselves.
    pub fn forget(&self, server: &'static str, uuid: &str) {
        self.live.lock().unwrap().remove(&(server, uuid.to_owned()));
    }

    /// The server lets the player go: their connection to it closes.
    fn let_go(&self, server: &'static str, uuid: &str) {
        if let Some(closer) = self.live.lock().unwrap().remove(&(server, uuid.to_owned())) {
            closer.close();
        }
    }

    fn next_handover(&self) -> String {
        format!(
            "{:032x}",
            self.handovers.fetch_add(1, Ordering::Relaxed) + 1
        )
    }
}

/// The unique id of a request line without its dashes.
fn plain(uuid: &str) -> String {
    uuid.chars().filter(|c| *c != '-').collect()
}

/// What the endpoint of `server` answers to one command line.
fn answer(server: &'static str, transfers: &Transfers, line: &str) -> Value {
    let refuse = |why: &str| json!({ "error": why });
    let (command, rest) = line.split_once(' ').unwrap_or((line, ""));
    let mut args = rest.split_whitespace();
    let Some(uuid) = args.next().map(plain) else {
        return refuse("a unique id is needed");
    };
    let asked = &transfers.asked;
    match command {
        "player-expect" => {
            asked.expect.fetch_add(1, Ordering::Relaxed);
            transfers.expect(&uuid);
            json!({ "ok": true, "renewed": false, "ttl_seconds": 10 })
        }
        "player-export" => {
            asked.export.fetch_add(1, Ordering::Relaxed);
            let freeze = args.next() == Some("freeze");
            let handover = freeze.then(|| transfers.next_handover());
            json!({
                "ok": true,
                "freeze_ticks_left": freeze.then_some(600),
                "record": {
                    "version": 1,
                    "data_version": 5023,
                    "handover": handover,
                    "dimension": "minecraft:overworld",
                    "position": [10.5, 70.0, -3.25],
                    "rotation": [90.0, 15.0],
                    "player": "CgAAAA==",
                },
            })
        }
        "player-import" => {
            asked.import.fetch_add(1, Ordering::Relaxed);
            let body = rest.split_once(' ').map(|(_, b)| b).unwrap_or("");
            let Ok(request) = serde_json::from_str::<Value>(body) else {
                return refuse("the request is not JSON");
            };
            let Some(id) = request["record"]["handover"].as_str() else {
                return refuse("the record has no hand-over id: export it with freeze");
            };
            let placed = request
                .get("place")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            transfers.signal(&uuid, Signal::Import);
            json!({ "ok": true, "handover": id, "parts": [], "placed": placed, "announced": false })
        }
        "player-confirm" => {
            asked.confirm.fetch_add(1, Ordering::Relaxed);
            transfers.signal(&uuid, Signal::Confirm);
            json!({ "ok": true, "placed": true, "announced": false })
        }
        "player-release" => {
            asked.release.fetch_add(1, Ordering::Relaxed);
            let _handover = args.next();
            match args.next() {
                Some("moved") => {
                    transfers.let_go(server, &uuid);
                    json!({ "ok": true, "released": "moved", "announced": false })
                }
                _ => json!({ "ok": true, "released": "abort" }),
            }
        }
        "player-drop" => {
            asked.drop.fetch_add(1, Ordering::Relaxed);
            json!({ "ok": true, "held": true, "expected": false })
        }
        _ => refuse("unknown command"),
    }
}

/// One server's control endpoint and the sidecar in front of it, both on 127.0.0.1.
pub struct Sidecar {
    /// Where the sidecar listens: what the proxy's configuration names.
    pub address: String,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
    endpoint: Endpoint,
}

impl Sidecar {
    pub fn start(server: &'static str, transfers: Arc<Transfers>) -> std::io::Result<Sidecar> {
        let stop = Arc::new(AtomicBool::new(false));
        // The control endpoint: `auth TOKEN`, then one command a line, one answer each.
        let control = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let endpoint = Endpoint {
            port: control.local_addr()?.port(),
            token: TOKEN.to_owned(),
        };
        let mut threads = Vec::new();
        {
            let stop = Arc::clone(&stop);
            threads.push(
                std::thread::Builder::new()
                    .name(format!("load-{server}-endpoint"))
                    .spawn(move || {
                        for stream in control.incoming() {
                            if stop.load(Ordering::Relaxed) {
                                return;
                            }
                            let Ok(stream) = stream else { continue };
                            let transfers = Arc::clone(&transfers);
                            let _ = std::thread::Builder::new()
                                .name(format!("load-{server}-control"))
                                .spawn(move || control_connection(server, &transfers, stream));
                        }
                    })?,
            );
        }
        // The sidecar, as `meridian-sidecar` runs it, in front of that endpoint.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let address = listener.local_addr()?.to_string();
        let settings = Arc::new(Settings {
            secret: SECRET.to_vec(),
            endpoint: {
                let endpoint = endpoint.clone();
                Box::new(move || Ok(endpoint.clone()))
            },
            endpoint_timeout: Duration::from_secs(5),
            idle_timeout: Duration::from_secs(120),
            insecure_remote: false,
            notices: None,
        });
        {
            let stop = Arc::clone(&stop);
            threads.push(
                std::thread::Builder::new()
                    .name(format!("load-{server}-sidecar"))
                    .spawn(move || {
                        let _ = serve(listener, settings, &stop);
                    })?,
            );
        }
        Ok(Sidecar {
            address,
            stop,
            threads,
            endpoint,
        })
    }
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Wakes the endpoint's accept.
        let _ = TcpStream::connect_timeout(
            &(Ipv4Addr::LOCALHOST, self.endpoint.port).into(),
            Duration::from_secs(1),
        );
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

/// One connection of the sidecar to the endpoint: the token, then a command.
fn control_connection(server: &'static str, transfers: &Transfers, stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    if line.trim().strip_prefix("auth ") != Some(TOKEN) {
        let _ = writeln!(writer, r#"{{"error":"bad or missing token"}}"#);
        return;
    }
    let _ = writeln!(writer, r#"{{"ok":true}}"#);
    line.clear();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let reply = answer(server, transfers, line.trim());
    let _ = writeln!(writer, "{reply}");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A switch's five requests, as the proxy sends them to the two endpoints, answer as the
    /// contract does, and the signals reach the held player.
    #[test]
    fn the_endpoint_answers_the_requests_of_a_switch() {
        let transfers = Transfers::default();
        let uuid = "0123456789abcdef0123456789abcdef";
        let dashed = "01234567-89ab-cdef-0123-456789abcdef";
        let reply = answer("east", &transfers, &format!("player-expect {dashed}"));
        assert_eq!(reply["ok"], json!(true));
        let signals = transfers.take_expected(uuid).expect("expected");
        assert!(transfers.take_expected(uuid).is_none());
        let exported = answer(
            "lobby",
            &transfers,
            &format!("player-export {dashed} freeze"),
        );
        let handover = exported["record"]["handover"].as_str().unwrap().to_owned();
        assert_eq!(handover.len(), 32);
        assert_eq!(exported["freeze_ticks_left"], json!(600));
        // Without a freeze it is a read: no hand-over.
        let read = answer("lobby", &transfers, &format!("player-export {dashed}"));
        assert!(read["record"]["handover"].is_null());
        let body = json!({ "record": exported["record"], "place": false });
        answer(
            "east",
            &transfers,
            &format!("player-import {dashed} {body}"),
        );
        assert_eq!(signals.recv().unwrap(), Signal::Import);
        answer(
            "east",
            &transfers,
            &format!("player-confirm {dashed} {handover}"),
        );
        assert_eq!(signals.recv().unwrap(), Signal::Confirm);
        let released = answer(
            "lobby",
            &transfers,
            &format!("player-release {dashed} {handover} moved"),
        );
        assert_eq!(released["released"], json!("moved"));
        let dropped = answer("east", &transfers, &format!("player-drop {dashed}"));
        assert_eq!(dropped["ok"], json!(true));
        assert_eq!(transfers.asked.import.load(Ordering::Relaxed), 1);
    }

    /// The sidecar, the real one, in front of the endpoint: a proxy's client gets the answers.
    #[test]
    fn the_sidecar_serves_the_endpoint_over_the_signed_exchange() {
        let transfers = Arc::new(Transfers::default());
        let sidecar = Sidecar::start("lobby", Arc::clone(&transfers)).unwrap();
        let mut client = meridian_sidecar::client::SidecarClient::connect(
            &sidecar.address,
            SECRET,
            Duration::from_secs(3),
        )
        .unwrap();
        let uuid = "0123456789abcdef0123456789abcdef";
        client.expect(uuid, None).unwrap();
        assert!(transfers.take_expected(uuid).is_some());
        let exported = client.export(uuid, true).unwrap();
        assert!(exported["record"]["handover"].is_string());
    }
}
