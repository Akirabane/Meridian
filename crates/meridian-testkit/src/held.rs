//! Two small worlds for switch tests: a source the player joins first, and a
//! fake target that answers as Horizon's held player is documented to
//! (`docs/server.md`, "Players arriving from another server"). The test plays the
//! player and the source by hand on two pipes; the target runs on a TCP port of
//! its own, since a switch logs the player in on it over a new connection.
//!
//! The two worlds differ in every way a delta must show: other chunks, another
//! position, another entity, which the target numbers with the client's player id
//! ([`CLIENT_ID`]) so that the id swap is exercised.
use crate::{
    backend::{FakeBackend, finish_configuration},
    body,
    peer::Step,
    trace::Recorder,
};
use horizon_protocol::{
    buf::{Reader, write_varint},
    connection::Connection,
    ids::{
        configuration as cfg,
        play::{clientbound as pc, serverbound as ps},
    },
    packets::play::{self, Clientbound as C},
};
use std::{net::TcpListener, sync::mpsc, thread::JoinHandle, time::Duration};

/// The player's id on the source, which is also the client's.
pub const CLIENT_ID: i32 = 5;
/// The player's id on the target.
pub const TARGET_ID: i32 = 9;
pub const OVERWORLD: &str = "minecraft:overworld";
/// What both servers send in their configuration phase, unless told otherwise.
pub const REGISTRY: [u8; 300] = [7; 300];
pub const PACKS: [u8; 3] = [1, 2, 3];
/// The client's answer to the known packs, and its Client Information.
pub const PACKS_REPLY: [u8; 1] = [0];
pub const INFORMATION: [u8; 12] = [4; 12];
/// How long a side waits for a packet.
pub const WAIT: Duration = Duration::from_secs(5);

pub fn send(connection: &mut Connection, id: i32, payload: &[u8]) {
    connection.send(&body(id, payload)).unwrap();
}

pub fn send_typed(connection: &mut Connection, packet: &C) {
    connection.send(&packet.encode()).unwrap();
}

/// The next packet, which must be `id`.
pub fn expect(connection: &mut Connection, id: i32) -> Result<Vec<u8>, String> {
    let (got, payload) = connection.recv().map_err(|e| e.to_string())?;
    if got != id {
        return Err(format!("expected packet {id}, received {got}"));
    }
    Ok(payload)
}

pub fn varint(v: i32) -> Vec<u8> {
    let mut out = Vec::new();
    write_varint(&mut out, v);
    out
}

/// A typed packet as an id and a payload.
pub fn packet(c: &C) -> (i32, Vec<u8>) {
    let body = c.encode();
    let mut r = Reader::new(&body);
    let id = r.varint().expect("an encoded packet starts with its id");
    (id, r.rest().to_vec())
}

pub fn login(player_id: i32, dimension: &str) -> C {
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

pub fn position(teleport: i32, x: f64) -> C {
    C::PlayerPosition(play::PlayerPosition {
        teleport,
        position: [x, 70.0, x],
        delta: [0.0; 3],
        yaw: 0.0,
        pitch: 0.0,
        relatives: 0,
    })
}

pub fn add_entity(id: i32, uuid: u128) -> C {
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

pub fn effect(id: i32) -> C {
    C::UpdateMobEffect(play::UpdateMobEffect {
        id,
        effect: 2,
        amplifier: 0,
        duration: 100,
        flags: 2,
    })
}

/// A chunk column: only its position is read.
pub fn chunk(x: i32, z: i32, fill: u8) -> (i32, Vec<u8>) {
    let mut payload = Vec::new();
    payload.extend_from_slice(&x.to_be_bytes());
    payload.extend_from_slice(&z.to_be_bytes());
    payload.extend_from_slice(&[fill; 64]);
    (pc::LEVEL_CHUNK_WITH_LIGHT, payload)
}

/// The nine columns around the origin.
pub fn columns(fill: u8) -> Vec<(i32, Vec<u8>)> {
    (-1..=1)
        .flat_map(|x| (-1..=1).map(move |z| chunk(x, z, fill)))
        .collect()
}

/// The source's world: the player, nine columns, the position, and entity 30.
pub fn source_world() -> Vec<(i32, Vec<u8>)> {
    let mut world = vec![packet(&login(CLIENT_ID, OVERWORLD))];
    world.extend(columns(1));
    world.push(packet(&position(1, 8.0)));
    world.push(packet(&add_entity(30, 300)));
    world
}

/// Takes the player through the source's configuration phase and into its world,
/// the test playing both: `player` is the client's end of the proxy, `source` the
/// backend's. Each packet must come through the proxy as it was sent.
pub fn join_source(player: &mut Connection, source: &mut Connection) {
    player.reader.set_read_timeout(Some(WAIT)).unwrap();
    source.reader.set_read_timeout(Some(WAIT)).unwrap();
    send(player, cfg::serverbound::CLIENT_INFORMATION, &INFORMATION);
    expect(source, cfg::serverbound::CLIENT_INFORMATION).unwrap();
    send(source, cfg::clientbound::REGISTRY_DATA, &REGISTRY);
    send(source, cfg::clientbound::SELECT_KNOWN_PACKS, &PACKS);
    expect(player, cfg::clientbound::REGISTRY_DATA).unwrap();
    expect(player, cfg::clientbound::SELECT_KNOWN_PACKS).unwrap();
    send(player, cfg::serverbound::SELECT_KNOWN_PACKS, &PACKS_REPLY);
    expect(source, cfg::serverbound::SELECT_KNOWN_PACKS).unwrap();
    send(source, cfg::clientbound::FINISH_CONFIGURATION, &[]);
    expect(player, cfg::clientbound::FINISH_CONFIGURATION).unwrap();
    send(player, cfg::serverbound::FINISH_CONFIGURATION, &[]);
    expect(source, cfg::serverbound::FINISH_CONFIGURATION).unwrap();
    for (id, payload) in source_world() {
        send(source, id, &payload);
        assert_eq!(expect(player, id).unwrap(), payload);
    }
    accept(player, 1, [8.0, 70.0, 8.0]);
    assert_eq!(accepted(source).unwrap(), (1, [8.0, 70.0, 8.0]));
}

/// How the fake target differs from the source.
#[derive(Clone)]
pub struct TargetWorld {
    pub registry: Vec<u8>,
    pub dimension: &'static str,
    /// Whether the import sends the held player a position to confirm.
    pub import_sends_position: bool,
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

/// What a fake target control tells the fake target's connection: "import" or
/// "confirm".
pub type Signal = mpsc::Sender<&'static str>;

/// What the fake target saw of the player once they were placed.
#[derive(Debug)]
pub struct TargetSaw {
    /// The id of the entity the client attacked, as the target got it.
    pub attacked: i32,
}

/// A fake target on a TCP port of its own: returns its address, the [`Signal`]
/// its control uses, and its thread.
///
/// It logs the player in, configures them as the source did (or with
/// `registry`), and sends them its world. It checks that the proxy answers for
/// the client: the first teleport, the chunk batch, the end of loading, a
/// keep-alive. On "import" it sends the position the client must confirm; once
/// the client confirmed it and the player is placed ("confirm"), its first live
/// packet: an effect on its player [`TARGET_ID`]. Then it waits for the client's
/// attack and returns the id it got.
pub fn spawn_target(world: TargetWorld) -> (String, Signal, JoinHandle<Result<TargetSaw, String>>) {
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
            Step::expect_exactly(cfg::serverbound::CLIENT_INFORMATION, INFORMATION),
            Step::send(cfg::clientbound::REGISTRY_DATA, world.registry.clone()),
            Step::send(cfg::clientbound::SELECT_KNOWN_PACKS, PACKS),
            Step::expect_exactly(cfg::serverbound::SELECT_KNOWN_PACKS, PACKS_REPLY),
            Step::send(cfg::clientbound::KEEP_ALIVE, 11u64.to_be_bytes()),
            Step::expect_exactly(cfg::serverbound::KEEP_ALIVE, 11u64.to_be_bytes()),
            finish_configuration(),
            Step::Expect {
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
        // A whole answer, with the place: a server cannot read one without it.
        if accepted(&mut c)? != (1, [40.0, 70.0, 40.0]) {
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
        if accepted(&mut c)?.0 != 2 {
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

/// The client's packets after a switch, up to the last of the target's extras
/// (its command tree); then the client confirms the position, and the target's
/// first live packet follows.
pub fn after_switch(player: &mut Connection) -> Vec<(i32, Vec<u8>)> {
    let mut packets = Vec::new();
    loop {
        let (id, payload) = player.recv().unwrap();
        packets.push((id, payload));
        if id == pc::COMMANDS {
            break;
        }
    }
    accept(player, 2, [41.0, 70.0, 41.0]);
    packets.push(player.recv().unwrap());
    packets
}

/// The client confirms the teleport `id`, which put it at `at`, as a 26.3
/// client does: the id, then where it now is and how it looks.
pub fn accept(player: &mut Connection, id: i32, at: [f64; 3]) {
    let answer = play::Serverbound::AcceptTeleportation(play::AcceptTeleportation {
        id,
        x: at[0],
        y: at[1],
        z: at[2],
        yaw: 0.0,
        pitch: 0.0,
    });
    player.send(&answer.encode()).unwrap();
}

/// The next packet, which must be a whole Accept Teleportation: its id and the
/// place it gives.
pub fn accepted(connection: &mut Connection) -> Result<(i32, [f64; 3]), String> {
    let payload = expect(connection, ps::ACCEPT_TELEPORTATION)?;
    match play::Serverbound::decode(ps::ACCEPT_TELEPORTATION, &payload) {
        Ok(play::Serverbound::AcceptTeleportation(a)) => Ok((a.id, [a.x, a.y, a.z])),
        _ => Err(format!(
            "an Accept Teleportation a server cannot read: {payload:?}"
        )),
    }
}

/// The client attacks the entity it knows as `id`.
pub fn attack(player: &mut Connection, id: i32) {
    player
        .send(&play::Serverbound::Attack(play::Attack { id }).encode())
        .unwrap();
}
