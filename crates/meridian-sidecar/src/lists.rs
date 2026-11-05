//! The lists of the backend's management protocol (the allowlist, the bans and the ip
//! bans), through the connection that [`crate::management`] already holds
//! (docs/OPERATING.md, section 5.12).
//!
//! - What the server announces about them (`notification/bans/added`, ...) is kept in a
//!   queue of its own, apart from `server/stopping` and `server/started`: a long run of bans
//!   must not push a stop notice out. The proxy asks for it with `access-notices`.
//! - What the proxy asks of the server (`manage`) is a JSON-RPC call made on that same
//!   connection: the thread that listens writes it and hands the answer, found by its `id`,
//!   to the connection that asked. Only the methods of [`CALLABLE`] go through; nothing else
//!   of the protocol (`server/stop`, `players/kick`, ...) is within the proxy's reach.
//! - A call waits at most [`CALL_TIMEOUT`] and never blocks the listener: it is picked up
//!   between two reads.
use crate::management::Notice;
use crate::request::NoticeHeard;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::mpsc::{self, Sender};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// The list notices kept for the proxy: the oldest go first. A proxy that falls behind by
/// more than this is told (`missed`) and reads the lists again.
pub const QUEUE_LEN: usize = 256;

/// The longest the proxy may be made to wait for a notice in one request.
pub const MAX_WAIT: Duration = Duration::from_secs(30);

/// How long a call waits for the server's answer.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(10);

/// The methods of the management protocol the proxy may call (without `minecraft:`): the
/// three lists, to read and to change one entry at a time. Nothing that clears or sets a
/// whole list, and nothing else of the protocol.
pub const CALLABLE: [&str; 9] = [
    "allowlist",
    "allowlist/add",
    "allowlist/remove",
    "bans",
    "bans/add",
    "bans/remove",
    "ip_bans",
    "ip_bans/add",
    "ip_bans/remove",
];

/// Whether `line` is a JSON-RPC message that tells a change of one of the lists. The method
/// is `minecraft:notification/bans/added` there; the prefix is optional.
pub fn is_list_notice(line: &str) -> bool {
    let Ok(message) = serde_json::from_str::<Value>(line) else {
        return false;
    };
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return false;
    };
    matches!(
        method.strip_prefix("minecraft:").unwrap_or(method),
        "notification/allowlist/added"
            | "notification/allowlist/removed"
            | "notification/bans/added"
            | "notification/bans/removed"
            | "notification/ip_bans/added"
            | "notification/ip_bans/removed"
    )
}

/// Why a call did not come back with a result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallError {
    /// The method is not one the proxy may call.
    NotAllowed(String),
    /// The sidecar is not connected to the server's management protocol.
    NotConnected,
    /// The server did not answer in time.
    Timeout,
    /// The connection ended before the answer came.
    Closed,
    /// The server answered with an error.
    Failed(String),
}

impl CallError {
    /// Whether the server never said no: the call may be made again later.
    pub fn is_lost(&self) -> bool {
        matches!(
            self,
            CallError::NotConnected | CallError::Timeout | CallError::Closed
        )
    }
}

impl fmt::Display for CallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CallError::NotAllowed(method) => {
                write!(f, "{method} is not a call the sidecar makes for the proxy")
            }
            CallError::NotConnected => {
                f.write_str("the sidecar is not connected to the server's management protocol")
            }
            CallError::Timeout => {
                f.write_str("the server's management protocol did not answer in time")
            }
            CallError::Closed => {
                f.write_str("the connection to the server's management protocol ended")
            }
            CallError::Failed(why) => f.write_str(why),
        }
    }
}

/// Where the answer to a call goes.
pub type Reply = Sender<Result<Value, CallError>>;

/// A call that is waiting to be written, for the thread that listens.
pub struct Outgoing {
    id: u64,
    text: String,
    reply: Reply,
}

impl Outgoing {
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// The reply, to answer it when the server does.
    pub fn into_reply(self) -> Reply {
        self.reply
    }
}

struct Queue {
    last: u64,
    /// The connections to the server there have been; 0 before the first.
    connection: u64,
    kept: VecDeque<(u64, String, Instant)>,
}

struct Calls {
    /// Whether a connection is up to carry them.
    open: bool,
    next_id: u64,
    waiting: Vec<Outgoing>,
}

/// What the proxy hears when it asks for list notices.
#[derive(Debug, PartialEq, Eq)]
pub struct Heard {
    /// The number of the last notice there has been, 0 if none.
    pub last: u64,
    /// The connections to the server there have been: when it is not the number the proxy
    /// last saw, the lists may have changed without a notice, and are read again.
    pub connection: u64,
    /// Whether notices after the number asked for were pushed out of the queue.
    pub missed: bool,
    pub notices: Vec<Notice>,
}

/// The list notices and the calls of the management protocol. Shared between the thread
/// that listens and the proxy's connections.
pub struct Lists {
    queue: Mutex<Queue>,
    changed: Condvar,
    calls: Mutex<Calls>,
}

impl Default for Lists {
    fn default() -> Self {
        Self {
            queue: Mutex::new(Queue {
                last: 0,
                connection: 0,
                kept: VecDeque::new(),
            }),
            changed: Condvar::new(),
            calls: Mutex::new(Calls {
                open: false,
                next_id: 1,
                waiting: Vec::new(),
            }),
        }
    }
}

impl Lists {
    /// Keeps `line` if it is a notice of a change of the lists, and says whether it was.
    pub fn offer(&self, line: &str) -> bool {
        if !is_list_notice(line) {
            return false;
        }
        let mut queue = self.lock();
        queue.last += 1;
        let seq = queue.last;
        queue.kept.push_back((seq, line.to_owned(), Instant::now()));
        while queue.kept.len() > QUEUE_LEN {
            queue.kept.pop_front();
        }
        drop(queue);
        self.changed.notify_all();
        true
    }

    /// A connection to the server is up: calls may be made.
    pub fn begin_connection(&self) {
        self.lock().connection += 1;
        let mut calls = self.calls();
        calls.open = true;
        // A call written for the connection before is not carried over to this one.
        calls.waiting.clear();
        drop(calls);
        self.changed.notify_all();
    }

    /// The connection is over: the calls that wait for it fail, and so do the next ones
    /// until another connection is up. The caller drops the replies it still holds.
    pub fn end_connection(&self) {
        let mut calls = self.calls();
        calls.open = false;
        for waiting in calls.waiting.drain(..) {
            let _ = waiting.reply.send(Err(CallError::Closed));
        }
    }

    /// The notices after number `after`, waiting up to `wait` for one when there are none.
    /// Without `after`, none are given: the caller only learns where the count stands.
    pub fn heard(&self, after: Option<u64>, wait: Duration) -> Heard {
        let wait = wait.min(MAX_WAIT);
        let until = Instant::now() + wait;
        let mut queue = self.lock();
        if let Some(after) = after {
            while queue.last <= after {
                let now = Instant::now();
                if now >= until {
                    break;
                }
                queue = self
                    .changed
                    .wait_timeout(queue, until - now)
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
        }
        let now = Instant::now();
        let missed = after.is_some_and(|after| {
            queue
                .kept
                .front()
                .is_some_and(|(first, ..)| *first > after.saturating_add(1))
        });
        Heard {
            last: queue.last,
            connection: queue.connection,
            missed,
            notices: after
                .map(|after| {
                    queue
                        .kept
                        .iter()
                        .filter(|(seq, ..)| *seq > after)
                        .map(|(seq, line, at)| Notice {
                            seq: *seq,
                            line: line.clone(),
                            age: now.saturating_duration_since(*at),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    /// Calls `method` (without `minecraft:`) on the server's management protocol and waits
    /// up to `timeout` for the answer. The method must be one of [`CALLABLE`].
    pub fn call(
        &self,
        method: &str,
        params: &Value,
        timeout: Duration,
    ) -> Result<Value, CallError> {
        if !CALLABLE.contains(&method) {
            return Err(CallError::NotAllowed(method.to_owned()));
        }
        let (reply, answer) = mpsc::channel();
        {
            let mut calls = self.calls();
            if !calls.open {
                return Err(CallError::NotConnected);
            }
            let id = calls.next_id;
            calls.next_id += 1;
            let mut message = json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": format!("minecraft:{method}"),
            });
            // A method with no parameter takes no `params` at all: the server refuses a null.
            if !params.is_null() {
                message["params"] = params.clone();
            }
            let text = message.to_string();
            calls.waiting.push(Outgoing { id, text, reply });
        }
        match answer.recv_timeout(timeout.min(CALL_TIMEOUT)) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => Err(CallError::Timeout),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(CallError::Closed),
        }
    }

    /// The calls waiting to be written, for the thread that listens.
    pub fn take_calls(&self) -> Vec<Outgoing> {
        std::mem::take(&mut self.calls().waiting)
    }

    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn calls(&self) -> MutexGuard<'_, Calls> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Hands `line`, if it is the server's answer to a call, to whoever made it, and says
/// whether it was one. An answer to a call nobody waits for any more is dropped.
pub fn answer(line: &str, waiting: &mut HashMap<u64, Reply>) -> bool {
    let Ok(Value::Object(message)) = serde_json::from_str::<Value>(line) else {
        return false;
    };
    if message.contains_key("method") {
        return false;
    }
    let outcome = match (message.get("result"), message.get("error")) {
        (Some(result), None) => Ok(result.clone()),
        (None, Some(error)) => Err(CallError::Failed(describe(error))),
        _ => return false,
    };
    let Some(id) = message.get("id").and_then(Value::as_u64) else {
        return false;
    };
    if let Some(reply) = waiting.remove(&id) {
        let _ = reply.send(outcome);
    }
    true
}

/// A JSON-RPC error as a line: its message and the detail the server gives.
fn describe(error: &Value) -> String {
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("error");
    match error.get("data").and_then(Value::as_str) {
        Some(data) if !data.is_empty() => format!("{message}: {data}"),
        _ => message.to_owned(),
    }
}

/// What the sidecar answers to `access-notices`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccessReport {
    /// Whether the sidecar listens to the management protocol at all.
    pub enabled: bool,
    /// Whether the connection to the server's management protocol is up now.
    pub watching: bool,
    /// The connections to the server there have been since the sidecar started.
    pub connection: u64,
    /// The number of the last notice there has been, 0 if none.
    pub last: u64,
    /// Whether notices after the number asked for were lost.
    pub missed: bool,
    pub notices: Vec<NoticeHeard>,
}

impl AccessReport {
    /// The answer as the sidecar writes it.
    pub fn to_value(&self) -> Value {
        json!({
            "enabled": self.enabled,
            "watching": self.watching,
            "connection": self.connection,
            "last": self.last,
            "missed": self.missed,
            "notices": self.notices.iter().map(|n| json!({
                "seq": n.seq,
                "line": n.line,
                "age_ms": u64::try_from(n.age.as_millis()).unwrap_or(u64::MAX),
            })).collect::<Vec<_>>(),
        })
    }

    /// Reads the answer.
    pub fn from_value(answer: &Value) -> Result<AccessReport, String> {
        let Value::Object(map) = answer else {
            return Err(format!("the report is not a JSON object: {answer}"));
        };
        let flag = |key: &str| {
            map.get(key)
                .and_then(Value::as_bool)
                .ok_or_else(|| format!("{key} is not true or false"))
        };
        let number = |key: &str| {
            map.get(key)
                .and_then(Value::as_u64)
                .ok_or_else(|| format!("{key} is not a whole number"))
        };
        let mut notices = Vec::new();
        for item in map
            .get("notices")
            .and_then(Value::as_array)
            .ok_or("notices is not a list")?
        {
            let seq = item.get("seq").and_then(Value::as_u64);
            let line = item.get("line").and_then(Value::as_str);
            let age = item.get("age_ms").and_then(Value::as_u64);
            let (Some(seq), Some(line), Some(age)) = (seq, line, age) else {
                return Err(format!("a notice is not usable: {item}"));
            };
            notices.push(NoticeHeard {
                seq,
                line: line.to_owned(),
                age: Duration::from_millis(age),
            });
        }
        Ok(AccessReport {
            enabled: flag("enabled")?,
            watching: flag("watching")?,
            connection: number("connection")?,
            last: number("last")?,
            missed: flag("missed")?,
            notices,
        })
    }
}

#[cfg(test)]
mod tests;
