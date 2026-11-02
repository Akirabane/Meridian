//! Switches from end to end against fake servers: a player relayed by the proxy
//! on "lobby" (the test plays the client and the backend), the fake held target of
//! `meridian_testkit::held` as "east", and fake sidecars that answer from memory.
use super::*;
use crate::console::Commands;
use crate::session::Player;
use crate::state::Proxy;
use horizon_protocol::{
    connection::Connection, crypto::offline_uuid, ids::play::clientbound as pc,
    packets::play::Clientbound as C, text,
};
use meridian_config::{AuthMode, Backend};
use meridian_handoff::{Outcome, Record, Session};
use meridian_net::{
    Profile,
    backend::BackendLink,
    client::{AuthKind, ClientSession},
};
use meridian_testkit::{held::*, pipe};
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::thread::JoinHandle;

/// What the fake sidecars were asked, by both backends, in order.
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

/// A sidecar that answers from memory. The target's tells the fake target of the
/// import and the confirmation.
struct FakeSidecar {
    backend: String,
    log: Log,
    signal: Option<Signal>,
    refuse: Option<(&'static str, String)>,
    on_export: Option<Box<dyn FnMut()>>,
}

impl FakeSidecar {
    fn answer(&mut self, request: &'static str, line: String) -> Result<(), String> {
        self.log.push(format!("{} {line}", self.backend));
        if let Some(signal) = &self.signal
            && (request == "import" || request == "confirm")
        {
            let _ = signal.send(request);
        }
        match &self.refuse {
            Some((refused, why)) if *refused == request => Err(why.clone()),
            _ => Ok(()),
        }
    }
}

impl PlayerControl for FakeSidecar {
    fn expect(&mut self, _: &str, _: Option<u64>) -> Result<(), String> {
        self.answer("expect", "expect".into())
    }

    fn export(&mut self, _: &str, _: bool) -> Result<Record, String> {
        if let Some(hook) = &mut self.on_export {
            hook();
        }
        self.answer("export", "export".into())?;
        Ok(serde_json::json!({ "version": 1, "dimension": OVERWORLD }))
    }

    fn import(
        &mut self,
        _: &str,
        _: Record,
        _: Option<Vec<String>>,
        _: Option<bool>,
        place: bool,
    ) -> Result<(), String> {
        self.answer("import", format!("import place={place}"))
    }

    fn confirm(&mut self, _: &str) -> Result<(), String> {
        self.answer("confirm", "confirm".into())
    }

    fn release(&mut self, _: &str, outcome: Outcome, _: bool) -> Result<(), String> {
        self.answer("release", format!("release {outcome:?}"))
    }

    fn drop_player(&mut self, _: &str) -> Result<(), String> {
        self.answer("drop", "drop".into())
    }
}

/// How the fake sidecars behave in one test.
#[derive(Default)]
struct Sidecars {
    /// A request the target's sidecar refuses, with the reason.
    target_refuses: Option<(&'static str, String)>,
    /// Run when the source exports the player, in the middle of the switch.
    during_export: Option<Box<dyn Fn() + Send + Sync>>,
}

/// A proxy with "lobby" (the player's) and "east" (the fake target at
/// `east`), each with a sidecar, and a player called Alex on lobby.
struct World {
    proxy: Arc<Proxy>,
    log: Log,
    player: Connection,
    _source: Connection,
    relay: JoinHandle<meridian_net::Result<meridian_net::relay::Report>>,
    target: JoinHandle<Result<TargetSaw, String>>,
}

fn sidecar(backend: &str, port: u16) -> Sidecar {
    Sidecar {
        backend: backend.into(),
        address: format!("127.0.0.1:{port}"),
        secret_file: format!("{backend}.key").into(),
    }
}

fn world(sidecars: Sidecars) -> World {
    let (east, signal, target) = spawn_target(TargetWorld::default());
    let config = Config {
        backends: vec![
            Backend {
                name: "lobby".into(),
                address: "127.0.0.1:25601".into(),
            },
            Backend {
                name: "east".into(),
                address: east,
            },
        ],
        default_backend: "lobby".into(),
        online_mode: false,
        auth_mode: AuthMode::Offline,
        forwarding_secret: "secret".into(),
        sidecars: vec![sidecar("lobby", 25580), sidecar("east", 25581)],
        ..Config::default()
    };
    let mut proxy = Proxy::new(
        config,
        "does-not-exist/meridian.toml".into(),
        Arc::new(AtomicBool::new(false)),
    );
    let log = Log::default();
    let signal = Mutex::new(signal);
    let Sidecars {
        target_refuses,
        during_export,
    } = sidecars;
    let during_export = during_export.map(Arc::new);
    let controls = {
        let log = log.clone();
        move |sidecar: &Sidecar, _: Duration| -> Result<Box<dyn PlayerControl>, String> {
            let east = sidecar.backend == "east";
            let on_export = during_export
                .clone()
                .filter(|_| !east)
                .map(|hook| Box::new(move || hook()) as Box<dyn FnMut()>);
            Ok(Box::new(FakeSidecar {
                backend: sidecar.backend.clone(),
                log: log.clone(),
                signal: east.then(|| signal.lock().unwrap().clone()),
                refuse: target_refuses.clone().filter(|_| east),
                on_export,
            }))
        }
    };
    proxy.sessions.switcher = Arc::new(Switcher {
        controls: Arc::new(controls),
        digests: Digests::default(),
    });
    // Alex, relayed between the test's two pipes, joins lobby's world.
    let (proxy_client, mut player) = pipe();
    let (proxy_backend, mut source) = pipe();
    let (session, handle) = Session::new(
        ClientSession {
            profile: Profile {
                name: "Alex".into(),
                uuid: offline_uuid("Alex").to_be_bytes(),
                properties: Vec::new(),
            },
            auth: AuthKind::Offline,
            virtual_host: None,
            connection: proxy_client,
        },
        BackendLink {
            connection: proxy_backend,
        },
        "lobby",
    )
    .unwrap();
    let relay = std::thread::spawn(move || session.run());
    join_source(&mut player, &mut source);
    let proxy = Arc::new(proxy);
    let open = proxy.sessions.open(
        Player {
            name: "Alex".into(),
            uuid: offline_uuid("Alex").to_be_bytes(),
            backend: "lobby".into(),
            address: "198.51.100.7:50123".parse().unwrap(),
            joined: Instant::now(),
        },
        handle,
    );
    // The session stays counted in for the whole test.
    std::mem::forget(open);
    World {
        proxy,
        log,
        player,
        _source: source,
        relay,
        target,
    }
}

fn chat(id: i32, payload: &[u8]) -> Option<String> {
    match C::decode(id, payload) {
        Ok(C::SystemChat(chat)) => Some(text::plain(&chat.content, &Default::default())),
        _ => None,
    }
}

/// The client after a switch: the delta and the target's extras, then, once it
/// confirmed the position, what follows up to the target's first live packet.
/// Returns the chat lines among them and that live packet.
fn client_after_switch(player: &mut Connection) -> (Vec<String>, C) {
    let mut lines = Vec::new();
    loop {
        let (id, payload) = player.recv().unwrap();
        lines.extend(chat(id, &payload));
        if id == pc::COMMANDS {
            break;
        }
    }
    send(
        player,
        horizon_protocol::ids::play::serverbound::ACCEPT_TELEPORTATION,
        &varint(2),
    );
    loop {
        let (id, payload) = player.recv().unwrap();
        lines.extend(chat(id, &payload));
        if id == pc::UPDATE_MOB_EFFECT {
            return (lines, C::decode(id, &payload).unwrap());
        }
    }
}

/// The console's `switch` moves Alex to east: the line gives the bytes and the
/// times, the player is told, `list` shows them on east, and the target's world
/// and numbering reach the client.
#[test]
fn the_console_switches_a_player_from_end_to_end() {
    let World {
        proxy,
        log,
        mut player,
        _source,
        relay,
        target,
    } = world(Sidecars::default());
    let lines = Commands::builtin().execute(&proxy, "switch alex east");
    // The measure, for whoever runs the tests with --nocapture.
    eprintln!("{lines:?}");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].starts_with("Alex moved from lobby to east in "),
        "{}",
        lines[0]
    );
    assert!(
        lines[0].contains(" bytes (9 chunks, 1 entities)"),
        "{}",
        lines[0]
    );
    assert_eq!(
        log.lines(),
        [
            "east expect",
            "lobby export",
            "east import place=false",
            "east confirm",
            "lobby release Moved",
        ]
    );
    let list = Commands::builtin().execute(&proxy, "list");
    assert!(list[1].contains(" on east for "), "{list:?}");
    let (chat, live) = client_after_switch(&mut player);
    assert_eq!(chat, ["You are now on east."]);
    // The target's player 9 is the client's 5.
    assert_eq!(live, effect(CLIENT_ID));
    attack(&mut player, TARGET_ID);
    assert_eq!(target.join().unwrap().unwrap().attacked, CLIENT_ID);
    relay.join().unwrap().unwrap();
}

/// `/server east` in game starts the same switch on a thread of its own, and the
/// player is told how it went.
#[test]
fn a_switch_asked_in_game_tells_the_player() {
    let World {
        proxy,
        mut player,
        _source,
        relay,
        target,
        ..
    } = world(Sidecars::default());
    let (_, handle) = proxy.sessions.session("Alex").unwrap();
    in_game(
        proxy.config(),
        Arc::clone(&proxy.sessions.switcher),
        handle,
        "Alex".into(),
        "198.51.100.7".parse().unwrap(),
        "east".into(),
    );
    let (mut lines, _) = client_after_switch(&mut player);
    // The switch's thread tells the player once the source let them go, which
    // may be after the target's first packet.
    while lines.is_empty() {
        let (id, payload) = player.recv().unwrap();
        lines.extend(chat(id, &payload));
    }
    assert_eq!(lines, ["You are now on east."]);
    attack(&mut player, TARGET_ID);
    target.join().unwrap().unwrap();
    relay.join().unwrap().unwrap();
}

/// A second switch while one is under way is refused at once, with a message.
#[test]
fn one_switch_at_a_time() {
    let second: Arc<Mutex<Option<String>>> = Arc::default();
    let proxy_slot: Arc<Mutex<Option<Arc<Proxy>>>> = Arc::default();
    let during_export = {
        let (second, proxy_slot) = (Arc::clone(&second), Arc::clone(&proxy_slot));
        Box::new(move || {
            let proxy = proxy_slot.lock().unwrap().clone().unwrap();
            let lines = Commands::builtin().execute(&proxy, "switch Alex east");
            *second.lock().unwrap() = lines.into_iter().next();
        }) as Box<dyn Fn() + Send + Sync>
    };
    let World {
        proxy,
        mut player,
        _source,
        target,
        ..
    } = world(Sidecars {
        during_export: Some(during_export),
        ..Sidecars::default()
    });
    *proxy_slot.lock().unwrap() = Some(Arc::clone(&proxy));
    let lines = Commands::builtin().execute(&proxy, "switch Alex east");
    assert!(lines[0].starts_with("Alex moved"), "{lines:?}");
    assert_eq!(
        second.lock().unwrap().as_deref(),
        Some(
            "Alex could not move from lobby to east: check: a switch of this player is \
             already under way"
        )
    );
    // The player is told of the refused one too, before the switch.
    let (chat, _) = client_after_switch(&mut player);
    assert_eq!(
        chat.last().map(String::as_str),
        Some("You are now on east.")
    );
    assert!(
        chat[0].starts_with("Could not move you to east."),
        "{chat:?}"
    );
    attack(&mut player, TARGET_ID);
    target.join().unwrap().unwrap();
    *proxy_slot.lock().unwrap() = None;
}

/// A switch that fails says at which step, and the player stays and is told.
#[test]
fn a_failed_switch_says_where_it_failed() {
    let World {
        proxy,
        log,
        mut player,
        mut _source,
        ..
    } = world(Sidecars {
        target_refuses: Some(("expect", "64 players already expected".into())),
        ..Sidecars::default()
    });
    let lines = Commands::builtin().execute(&proxy, "switch Alex east");
    assert_eq!(
        lines,
        [
            "Alex could not move from lobby to east: prepare: east would not expect the \
             player: 64 players already expected (the player stays where they were)"
        ]
    );
    assert_eq!(log.lines(), ["east expect"]);
    let (id, payload) = player.recv().unwrap();
    assert!(
        chat(id, &payload)
            .unwrap()
            .starts_with("Could not move you to east. Alex could not move")
    );
    // Still on lobby, and relayed.
    assert!(Commands::builtin().execute(&proxy, "list")[1].contains(" on lobby for "));
    send_typed(&mut _source, &effect(CLIENT_ID));
    let (id, payload) = player.recv().unwrap();
    assert_eq!(C::decode(id, &payload).unwrap(), effect(CLIENT_ID));
}

/// A backend with no sidecar cannot be switched to, and the check says so.
#[test]
fn a_backend_without_a_sidecar_is_named() {
    let World { proxy, log, .. } = world(Sidecars::default());
    let mut config = (*proxy.config()).clone();
    config.sidecars.retain(|s| s.backend != "east");
    proxy.set_config(config);
    let lines = Commands::builtin().execute(&proxy, "switch Alex east");
    assert_eq!(
        lines,
        [
            "Alex could not move from lobby to east: check: east has no sidecar in the \
             configuration, so no player can be switched to or from it"
        ]
    );
    assert!(log.lines().is_empty());
}
