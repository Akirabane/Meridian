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
    collections::HashMap,
    sync::{Arc, Mutex},
    thread::JoinHandle,
    time::{Duration, Instant},
};

/// The fake target of the test kit.
fn target(world: TargetWorld) -> (String, Signal, JoinHandle<Result<TargetSaw, String>>) {
    spawn_target(world)
}

mod configure;
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

/// A record as `player-export` gives it, as far as the proxy reads it.
fn record(dimension: &str) -> Record {
    json!({ "version": 1, "dimension": dimension, "player": "AAAA" })
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
        }
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
        self.log.push(format!("{} {line}", self.side));
        if let Some(hook) = self.hooks.get_mut(request) {
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

    fn export(&mut self, _: &str, freeze: bool) -> Result<Record, String> {
        self.answer("export", format!("export freeze={freeze}"))
            .map(|()| self.record.clone())
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
        self.answer(
            "import",
            format!("import parts={parts} announce={announce:?} place={place}{pose}"),
        )
        .map(|()| Imported {
            recentered: self.recentered,
        })
    }

    fn confirm(&mut self, _: &str) -> Result<(), String> {
        self.answer("confirm", "confirm".into())
    }

    fn release(&mut self, _: &str, outcome: Outcome, announce: bool) -> Result<(), String> {
        self.answer(
            "release",
            format!("release {outcome:?} announce={announce}"),
        )
    }

    fn drop_player(&mut self, _: &str) -> Result<(), String> {
        self.answer("drop", "drop".into())
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
    join_source(&mut player, &mut source);
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
