//! A bench for switches: a fake client and a fake source joined to a session by
//! pipes, the fake target of `meridian_testkit::held`, and fake controls that
//! answer from memory.
use super::*;
use horizon_protocol::{
    connection::Connection,
    ids::{
        configuration as cfg,
        play::{clientbound as pc, serverbound as ps},
    },
    packets::play::{self, Clientbound as C},
};
use meridian_net::{
    Profile,
    backend::{BackendLink, BackendOptions},
    client::{AuthKind, ClientSession},
    relay::Report,
};
use meridian_testkit::{held::*, pipe};
use serde_json::json;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    thread::JoinHandle,
    time::{Duration, Instant},
};

/// The fake target of the test kit.
fn target(world: TargetWorld) -> (String, Signal, JoinHandle<Result<TargetSaw, String>>) {
    spawn_target(world)
}

mod configure;
mod fallback;
mod hostile;
mod session;
mod switch;

fn profile() -> Profile {
    Profile {
        name: "Alex".into(),
        uuid: 0x0123456789abcdef_fedcba9876543210_u128.to_be_bytes(),
        properties: Vec::new(),
    }
}

fn decode(id: i32, payload: &[u8]) -> C {
    C::decode(id, payload).unwrap()
}

/// The hand-over id of the fake exports.
const HANDOVER: &str = "9f2c4be0a1d34e6b8c0f1e2d3c4b5a69";

/// A record as `player-export` with freeze gives it, as far as the proxy reads it.
fn record(dimension: &str) -> Record {
    json!({ "version": 1, "handover": HANDOVER, "dimension": dimension, "player": "AAAA" })
}

/// What the fake controls were asked, in order, by both sides.
#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<String>>>);

impl Log {
    fn push(&self, line: String) {
        self.0.lock().unwrap().push(line);
    }

    fn lines(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

type Hook = Box<dyn FnMut()>;

/// A control that answers from memory.
struct FakeControl {
    side: &'static str,
    log: Log,
    record: Record,
    /// Requests refused, with the reason.
    refuse: HashMap<&'static str, String>,
    /// Run when a request comes, before its answer.
    hooks: HashMap<&'static str, Hook>,
    /// What the import answers about the player's chunk.
    recentered: Option<bool>,
    /// The ticks the export says the freeze has left.
    ticks_left: Option<u64>,
    /// How many answers of each request are lost on the way before one comes. An
    /// export is lost before it gets there; any other request is acted on, and its
    /// answer is what is lost.
    losing: HashMap<&'static str, u32>,
    /// Whether the last request was lost, so that the connection can be reopened.
    lost: bool,
    /// The requests acted on once already: a repeat is answered, as Horizon answers a
    /// repeated import or confirm of the same hand-over, without acting again.
    acted: HashSet<&'static str>,
    /// Whether the connection cannot be reopened.
    cannot_reopen: bool,
}

impl FakeControl {
    fn new(side: &'static str, log: &Log) -> Self {
        FakeControl {
            side,
            log: log.clone(),
            record: record(OVERWORLD),
            refuse: HashMap::new(),
            hooks: HashMap::new(),
            recentered: None,
            // Horizon's default: 30 s of freeze.
            ticks_left: Some(600),
            losing: HashMap::new(),
            lost: false,
            acted: HashSet::new(),
            cannot_reopen: false,
        }
    }

    fn with_ticks_left(mut self, ticks: Option<u64>) -> Self {
        self.ticks_left = ticks;
        self
    }

    fn losing_exports(self, count: u32) -> Self {
        self.losing("export", count)
    }

    /// The answers to the first `count` `request`s are lost.
    fn losing(mut self, request: &'static str, count: u32) -> Self {
        self.losing.insert(request, count);
        self
    }

    /// Whether the answer to this `request` is lost; counts it.
    fn loses(&mut self, request: &'static str) -> bool {
        match self.losing.get_mut(request) {
            Some(left) if *left > 0 => {
                *left -= 1;
                true
            }
            _ => false,
        }
    }

    /// The answer of a request the backend acted on: lost if it is to be, and then
    /// the line of the log says so.
    fn answer_or_lose(&mut self, request: &'static str, line: String) -> Result<(), String> {
        let lost = self.loses(request);
        let line = if lost {
            format!("{line} (answer lost)")
        } else {
            line
        };
        let repeat = !self.acted.insert(request);
        let answered = self.answer_with(request, line, !repeat);
        self.lost = lost;
        if lost {
            return Err("no answer in time".to_owned());
        }
        answered
    }

    fn refusing(mut self, request: &'static str, why: &str) -> Self {
        self.refuse.insert(request, why.into());
        self
    }

    fn on(mut self, request: &'static str, hook: impl FnMut() + 'static) -> Self {
        self.hooks.insert(request, Box::new(hook));
        self
    }

    fn answer(&mut self, request: &'static str, line: String) -> Result<(), String> {
        self.answer_with(request, line, true)
    }

    fn answer_with(
        &mut self,
        request: &'static str,
        line: String,
        run_hook: bool,
    ) -> Result<(), String> {
        self.log.push(format!("{} {line}", self.side));
        if run_hook && let Some(hook) = self.hooks.get_mut(request) {
            hook();
        }
        match self.refuse.get(request) {
            Some(why) => Err(why.clone()),
            None => Ok(()),
        }
    }
}

impl PlayerControl for FakeControl {
    fn expect(&mut self, _: &str, seconds: Option<u64>) -> Result<(), String> {
        self.answer("expect", format!("expect {seconds:?}"))
    }

    fn export(&mut self, _: &str, freeze: bool) -> Result<Exported, String> {
        if self.loses("export") {
            self.lost = true;
            self.log
                .push(format!("{} export freeze={freeze} (lost)", self.side));
            return Err("no answer in time".to_owned());
        }
        self.lost = false;
        self.answer("export", format!("export freeze={freeze}"))
            .map(|()| Exported {
                record: self.record.clone(),
                freeze_ticks_left: self.ticks_left,
            })
    }

    fn lost(&self) -> bool {
        self.lost
    }

    fn recover(&mut self) -> Result<bool, String> {
        if !std::mem::take(&mut self.lost) {
            return Ok(false);
        }
        self.log.push(format!("{} reopen", self.side));
        if self.cannot_reopen {
            return Err("cannot reach the sidecar again: refused".to_owned());
        }
        Ok(true)
    }

    fn import(
        &mut self,
        _: &str,
        record: Record,
        parts: Option<Vec<String>>,
        announce: Option<bool>,
        place: bool,
        arrival: Option<Arrival>,
    ) -> Result<Imported, String> {
        assert_eq!(record_dimension(&record), Some(OVERWORLD));
        // The record goes to the target as the source gave it, with its hand-over id.
        assert_eq!(record_handover(&record).as_deref(), Some(HANDOVER));
        let parts = match parts {
            None => "all".to_owned(),
            Some(parts)
                if parts.len() == PARTS.len() - 1 && !parts.iter().any(|p| p == "position") =>
            {
                "all-but-position".to_owned()
            }
            Some(parts) => parts.join(","),
        };
        // Where the import puts the player, when it says.
        let pose = arrival.map_or(String::new(), |a| {
            format!(
                " at={:?} look={:?} velocity={:?} ground={:?}",
                a.position, a.rotation, a.velocity, a.on_ground
            )
        });
        self.answer_or_lose(
            "import",
            format!("import parts={parts} announce={announce:?} place={place}{pose}"),
        )
        .map(|()| Imported {
            recentered: self.recentered,
        })
    }

    fn confirm(&mut self, _: &str, handover: &str) -> Result<(), String> {
        self.answer_or_lose("confirm", format!("confirm {handover}"))
    }

    fn release(
        &mut self,
        _: &str,
        handover: &str,
        outcome: Outcome,
        announce: bool,
    ) -> Result<(), String> {
        self.answer(
            "release",
            format!("release {handover} {outcome:?} announce={announce}"),
        )
    }

    fn drop_player(&mut self, _: &str, handover: Option<&str>) -> Result<(), String> {
        self.answer(
            "drop",
            handover.map_or("drop".to_owned(), |id| format!("drop {id}")),
        )
    }
}

/// A session with its player in the play phase on the source.
struct Bench {
    handle: SessionHandle,
    run: JoinHandle<meridian_net::Result<Report>>,
    player: Connection,
    source: Connection,
}

fn bench() -> Bench {
    bench_as(&INFORMATION)
}

/// As [`bench`], for a client that sends `information` as its Client Information.
fn bench_as(information: &[u8]) -> Bench {
    let (proxy_client, mut player) = pipe();
    let (proxy_backend, mut source) = pipe();
    let client = ClientSession {
        profile: profile(),
        auth: AuthKind::Offline,
        virtual_host: None,
        connection: proxy_client,
    };
    let backend = BackendLink {
        connection: proxy_backend,
    };
    let (session, handle) = Session::new(client, backend, "source").unwrap();
    let run = std::thread::spawn(move || session.run());
    join_source_as(&mut player, &mut source, information);
    Bench {
        handle,
        run,
        player,
        source,
    }
}

fn options() -> BackendOptions {
    BackendOptions {
        forwarding_secret: b"unused".to_vec(),
        connect_timeout: WAIT,
        player_address: "127.0.0.1".parse().unwrap(),
    }
}

fn settings() -> Settings {
    Settings {
        prepare_timeout: WAIT,
        settle: Duration::from_millis(100),
        ..Settings::default()
    }
}

/// Waits for `done`, up to [`WAIT`].
fn wait_for(mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !done() {
        assert!(Instant::now() < deadline, "waited too long");
        std::thread::sleep(Duration::from_millis(10));
    }
}
