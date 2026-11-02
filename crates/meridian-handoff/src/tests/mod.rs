//! A bench for switches: a fake client and a fake source joined to a session by
//! pipes, a fake target reached over TCP that answers as Horizon's held player is
//! documented to, and fake controls that answer from memory.
use super::*;
use horizon_protocol::{
    buf::{Reader, write_varint},
    connection::Connection,
    ids::{
        configuration as cfg,
        play::{clientbound as pc, serverbound as ps},
    },
    packets::play::{self, Clientbound as C},
};
use meridian_ghost::Packet;
use meridian_net::{
    Profile,
    backend::{BackendLink, BackendOptions},
    client::{AuthKind, ClientSession},
    relay::Report,
};
use meridian_testkit::{
    backend::{FakeBackend, finish_configuration},
    body,
    peer::Step as Script,
    pipe,
    trace::Recorder,
};
use serde_json::json;
use std::{
    collections::HashMap,
    net::TcpListener,
    sync::{Arc, Mutex, mpsc},
    thread::JoinHandle,
    time::{Duration, Instant},
};

mod configure;
mod session;
mod switch;

/// The player's id on the source, which is also the client's.
const CLIENT_ID: i32 = 5;
/// The player's id on the target.
const TARGET_ID: i32 = 9;
const OVERWORLD: &str = "minecraft:overworld";
const REGISTRY: [u8; 300] = [7; 300];
const PACKS: [u8; 3] = [1, 2, 3];
const PACKS_REPLY: [u8; 1] = [0];
const INFORMATION: [u8; 12] = [4; 12];
const WAIT: Duration = Duration::from_secs(5);

fn profile() -> Profile {
    Profile {
        name: "Alex".into(),
        uuid: 0x0123456789abcdef_fedcba9876543210_u128.to_be_bytes(),
        properties: Vec::new(),
    }
}

fn send(connection: &mut Connection, id: i32, payload: &[u8]) {
    connection.send(&body(id, payload)).unwrap();
}

fn send_typed(connection: &mut Connection, packet: &C) {
    connection.send(&packet.encode()).unwrap();
}

/// The next packet, which must be `id`.
fn expect(connection: &mut Connection, id: i32) -> Result<Vec<u8>, String> {
    let (got, payload) = connection.recv().map_err(|e| e.to_string())?;
    if got != id {
        return Err(format!("expected packet {id}, received {got}"));
    }
    Ok(payload)
}

fn varint(v: i32) -> Vec<u8> {
    let mut out = Vec::new();
    write_varint(&mut out, v);
    out
}

fn login(player_id: i32, dimension: &str) -> C {
    C::Login(play::Login {
        player_id,
        hardcore: false,
        levels: vec![OVERWORLD.into(), "minecraft:the_nether".into()],
        max_players: 20,
        chunk_radius: 2,
        simulation_distance: 2,
        reduced_debug_info: false,
        show_death_screen: true,
        limited_crafting: false,
        spawn: play::SpawnInfo {
            dimension_type: 0,
            dimension: dimension.into(),
            seed: 0,
            game_type: 0,
            previous_game_type: None,
            debug: false,
            flat: false,
            last_death: None,
            portal_cooldown: 0,
            sea_level: 63,
        },
        online_mode: false,
        enforces_secure_chat: false,
    })
}

fn position(teleport: i32, x: f64) -> C {
    C::PlayerPosition(play::PlayerPosition {
        teleport,
        position: [x, 70.0, x],
        delta: [0.0; 3],
        yaw: 0.0,
        pitch: 0.0,
        relatives: 0,
    })
}

fn add_entity(id: i32, uuid: u128) -> C {
    C::AddEntity(play::AddEntity {
        id,
        uuid,
        kind: 5,
        position: [1.0, 70.0, 1.0],
        velocity: [0.0; 3],
        pitch: 0,
        yaw: 0,
        head_yaw: 0,
        data: 0,
    })
}

fn effect(id: i32) -> C {
    C::UpdateMobEffect(play::UpdateMobEffect {
        id,
        effect: 2,
        amplifier: 0,
        duration: 100,
        flags: 2,
    })
}

/// A chunk column: only its position is read.
fn chunk(x: i32, z: i32, fill: u8) -> (i32, Vec<u8>) {
    let mut payload = Vec::new();
    payload.extend_from_slice(&x.to_be_bytes());
    payload.extend_from_slice(&z.to_be_bytes());
    payload.extend_from_slice(&[fill; 64]);
    (pc::LEVEL_CHUNK_WITH_LIGHT, payload)
}

fn columns(fill: u8) -> Vec<(i32, Vec<u8>)> {
    (-1..=1)
        .flat_map(|x| (-1..=1).map(move |z| chunk(x, z, fill)))
        .collect()
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
}

impl FakeControl {
    fn new(side: &'static str, log: &Log) -> Self {
        FakeControl {
            side,
            log: log.clone(),
            record: record(OVERWORLD),
            refuse: HashMap::new(),
            hooks: HashMap::new(),
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
    ) -> Result<(), String> {
        assert_eq!(record_dimension(&record), Some(OVERWORLD));
        self.answer(
            "import",
            format!("import parts={parts:?} announce={announce:?} place={place}"),
        )
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

/// The source's world: the player, nine columns, the position, and entity 30.
fn source_world() -> Vec<(i32, Vec<u8>)> {
    let packet = |c: &C| {
        let p = Packet::of(c);
        (p.id, p.payload)
    };
    let mut world = vec![packet(&login(CLIENT_ID, OVERWORLD))];
    world.extend(columns(1));
    world.push(packet(&position(1, 8.0)));
    world.push(packet(&add_entity(30, 300)));
    world
}

fn bench() -> Bench {
    let (proxy_client, mut player) = pipe();
    let (proxy_backend, mut source) = pipe();
    player.reader.set_read_timeout(Some(WAIT)).unwrap();
    source.reader.set_read_timeout(Some(WAIT)).unwrap();
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
    // The configuration phase.
    send(
        &mut player,
        cfg::serverbound::CLIENT_INFORMATION,
        &INFORMATION,
    );
    expect(&mut source, cfg::serverbound::CLIENT_INFORMATION).unwrap();
    send(&mut source, cfg::clientbound::REGISTRY_DATA, &REGISTRY);
    send(&mut source, cfg::clientbound::SELECT_KNOWN_PACKS, &PACKS);
    expect(&mut player, cfg::clientbound::REGISTRY_DATA).unwrap();
    expect(&mut player, cfg::clientbound::SELECT_KNOWN_PACKS).unwrap();
    send(
        &mut player,
        cfg::serverbound::SELECT_KNOWN_PACKS,
        &PACKS_REPLY,
    );
    expect(&mut source, cfg::serverbound::SELECT_KNOWN_PACKS).unwrap();
    send(&mut source, cfg::clientbound::FINISH_CONFIGURATION, &[]);
    expect(&mut player, cfg::clientbound::FINISH_CONFIGURATION).unwrap();
    send(&mut player, cfg::serverbound::FINISH_CONFIGURATION, &[]);
    expect(&mut source, cfg::serverbound::FINISH_CONFIGURATION).unwrap();
    // The play phase.
    for (id, payload) in source_world() {
        send(&mut source, id, &payload);
        assert_eq!(expect(&mut player, id).unwrap(), payload);
    }
    send(&mut player, ps::ACCEPT_TELEPORTATION, &varint(1));
    assert_eq!(
        expect(&mut source, ps::ACCEPT_TELEPORTATION).unwrap(),
        varint(1)
    );
    Bench {
        handle,
        run,
        player,
        source,
    }
}

/// How the fake target differs from the source.
#[derive(Clone)]
struct TargetWorld {
    registry: Vec<u8>,
    dimension: &'static str,
    /// Whether the import sends the held player a position to confirm.
    import_sends_position: bool,
}

impl Default for TargetWorld {
    fn default() -> Self {
        TargetWorld {
            registry: REGISTRY.to_vec(),
            dimension: OVERWORLD,
            import_sends_position: true,
        }
    }
}

/// What the fake target control tells the fake target's connection: "import" or
/// "confirm".
type Signal = mpsc::Sender<&'static str>;

/// What the fake target saw of the player once they were placed.
#[derive(Debug)]
struct TargetSaw {
    /// The id of the entity the client attacked, as the target got it.
    attacked: i32,
}

/// A fake target on a TCP port of its own. It logs the player in, configures them,
/// and sends them its world. On the import (told through the [`Signal`]) it sends
/// the position the client must confirm; once the client confirmed it and the
/// player is placed ("confirm"), its first live packet.
fn target(world: TargetWorld) -> (String, Signal, JoinHandle<Result<TargetSaw, String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let (signal, signals) = mpsc::channel();
    let next = move |expected: &str| match signals.recv_timeout(WAIT) {
        Ok(got) if got == expected => Ok(()),
        Ok(got) => Err(format!("expected {expected}, got {got}")),
        Err(_) => Err(format!("no {expected}")),
    };
    let thread = std::thread::spawn(move || {
        let (stream, _) = listener.accept().map_err(|e| e.to_string())?;
        let mut c = Connection::from_stream(stream).map_err(|e| e.to_string())?;
        let configuration = vec![
            Script::expect_exactly(cfg::serverbound::CLIENT_INFORMATION, INFORMATION),
            Script::send(cfg::clientbound::REGISTRY_DATA, world.registry.clone()),
            Script::send(cfg::clientbound::SELECT_KNOWN_PACKS, PACKS),
            Script::expect_exactly(cfg::serverbound::SELECT_KNOWN_PACKS, PACKS_REPLY),
            Script::send(cfg::clientbound::KEEP_ALIVE, 11u64.to_be_bytes()),
            Script::expect_exactly(cfg::serverbound::KEEP_ALIVE, 11u64.to_be_bytes()),
            finish_configuration(),
            Script::Expect {
                id: cfg::serverbound::FINISH_CONFIGURATION,
            },
        ];
        FakeBackend {
            after_login: configuration,
            ..FakeBackend::default()
        }
        .run(&mut c, &Recorder::new())?;
        // The held player's join sequence and world.
        send_typed(&mut c, &login(TARGET_ID, world.dimension));
        for (id, payload) in columns(2) {
            send(&mut c, id, &payload);
        }
        send_typed(&mut c, &position(1, 40.0));
        // The target's own entity 5 is the client's player id.
        send_typed(&mut c, &add_entity(CLIENT_ID, 500));
        send(&mut c, pc::COMMANDS, &[0, 0]);
        send(&mut c, pc::CHUNK_BATCH_FINISHED, &varint(9));
        if expect(&mut c, ps::ACCEPT_TELEPORTATION)? != varint(1) {
            return Err("the proxy confirmed another teleport".into());
        }
        expect(&mut c, ps::CHUNK_BATCH_RECEIVED)?;
        expect(&mut c, ps::PLAYER_LOADED)?;
        send(&mut c, pc::KEEP_ALIVE, &12u64.to_be_bytes());
        if expect(&mut c, ps::KEEP_ALIVE)? != 12u64.to_be_bytes() {
            return Err("the proxy answered the keep-alive wrongly".into());
        }
        next("import")?;
        if !world.import_sends_position {
            // Silent, with the connection open, until the control is dropped.
            let _ = next("confirm");
            return Err("told not to send the position".into());
        }
        // What the import changes, a position to confirm among it; the player
        // stays held.
        send_typed(&mut c, &position(2, 41.0));
        // The client's confirmation, not the proxy's.
        if expect(&mut c, ps::ACCEPT_TELEPORTATION)? != varint(2) {
            return Err("the client confirmed another teleport".into());
        }
        next("confirm")?;
        send_typed(&mut c, &effect(TARGET_ID));
        let attack = expect(&mut c, ps::ATTACK)?;
        let attacked = Reader::new(&attack).varint().map_err(|e| e.to_string())?;
        Ok(TargetSaw { attacked })
    });
    (address, signal, thread)
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
