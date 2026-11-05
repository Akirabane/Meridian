//! Switches from end to end against fake servers: a player relayed by the proxy
//! on "lobby" (the test plays the client and the backend), the fake held target of
//! `meridian_testkit::held` as "east", and fake sidecars that answer from memory.
use super::*;
use crate::console::Commands;
use crate::session::{Player, Sessions};
use crate::state::Proxy;
use horizon_protocol::{
    connection::Connection, crypto::offline_uuid, ids::play::clientbound as pc,
    packets::play::Clientbound as C, text,
};
use meridian_config::{AuthMode, Backend};
use meridian_handoff::{Exported, Imported, Outcome, Record, Session};
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

    fn export(&mut self, _: &str, _: bool) -> Result<Exported, String> {
        if let Some(hook) = &mut self.on_export {
            hook();
        }
        self.answer("export", "export".into())?;
        Ok(Exported {
            record: serde_json::json!({
                "version": 1,
                "handover": "9f2c4be0a1d34e6b8c0f1e2d3c4b5a69",
                "dimension": OVERWORLD
            }),
            freeze_ticks_left: Some(600),
        })
    }

    fn import(
        &mut self,
        _: &str,
        _: Record,
        _: Option<Vec<String>>,
        _: Option<bool>,
        place: bool,
        _: Option<meridian_handoff::Arrival>,
    ) -> Result<Imported, String> {
        self.answer("import", format!("import place={place}"))
            .map(|()| Imported::default())
    }

    fn confirm(&mut self, _: &str, _: &str) -> Result<(), String> {
        self.answer("confirm", "confirm".into())
    }

    fn release(&mut self, _: &str, _: &str, outcome: Outcome, _: bool) -> Result<(), String> {
        self.answer("release", format!("release {outcome:?}"))
    }

    fn drop_player(&mut self, _: &str, _: Option<&str>) -> Result<(), String> {
        self.answer("drop", "drop".into())
    }
}

/// How the fake sidecars behave in one test.
#[derive(Default)]
struct Sidecars {
    /// A request the target's sidecar refuses, with the reason.
    target_refuses: Option<(&'static str, String)>,
    /// A request the source's sidecar refuses, with the reason.
    source_refuses: Option<(&'static str, String)>,
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
        notices: false,
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
        source_refuses,
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
                refuse: if east {
                    target_refuses.clone()
                } else {
                    source_refuses.clone()
                },
                on_export,
            }))
        }
    };
    proxy.sessions = Arc::new(Sessions::with_switcher(Arc::new(Switcher {
        controls: Arc::new(controls),
        digests: Digests::default(),
        metrics: Arc::default(),
    })));
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
    accept(player, 2, [41.0, 70.0, 41.0]);
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
    // One transfer, timed in each phase, no alert.
    let page = crate::metrics::render(&proxy);
    assert!(
        page.contains(
            "
meridian_transfers_total 1
"
        ),
        "{page}"
    );
    assert!(
        page.contains(
            "
meridian_transfer_duration_seconds_count{phase=\"prepare\"} 1
"
        ),
        "{page}"
    );
    assert!(
        page.contains(
            "
meridian_transfer_duration_seconds_count{phase=\"total\"} 1
"
        ),
        "{page}"
    );
    assert!(
        page.contains(
            "
meridian_transfer_alerts_total 0
"
        ),
        "{page}"
    );
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
        None,
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
    // A failure at the first step: counted as a failure of that step, not as a transfer.
    let page = crate::metrics::render(&proxy);
    assert!(
        page.contains(
            "
meridian_transfers_total 0
"
        ),
        "{page}"
    );
    assert!(
        page.contains(
            "
meridian_transfer_failures_total{step=\"prepare\"} 1
"
        ),
        "{page}"
    );
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

/// The place a move keeps on its target is given back once the switch has ended, and
/// the player counts on the target from then on.
#[test]
fn the_place_reserved_for_a_switch_is_given_back_when_it_ends() {
    use crate::balance::Server;
    let World {
        proxy,
        mut player,
        _source,
        relay,
        target,
        ..
    } = world(Sidecars::default());
    let (_, handle) = proxy.sessions.session("Alex").unwrap();
    let place = proxy
        .sessions
        .balancer
        .reserve_named(
            Server {
                backend: "east",
                soft_cap: 5,
                hard_cap: 5,
                available: true,
                ping: None,
                reported_players: None,
            },
            || proxy.sessions.counts(),
        )
        .unwrap();
    assert_eq!(proxy.sessions.balancer.reserved("east"), 1);
    in_game(
        proxy.config(),
        Arc::clone(&proxy.sessions.switcher),
        handle,
        "Alex".into(),
        "198.51.100.7".parse().unwrap(),
        "east".into(),
        Some(place),
    );
    let (mut lines, _) = client_after_switch(&mut player);
    // The switch's thread tells the player once the source let them go, which may be
    // after the target's first packet.
    while lines.is_empty() {
        let (id, payload) = player.recv().unwrap();
        lines.extend(chat(id, &payload));
    }
    assert_eq!(lines, ["You are now on east."]);
    // The thread gives the place back after it has told the player.
    let started = std::time::Instant::now();
    while proxy.sessions.balancer.reserved("east") != 0
        && started.elapsed() < Duration::from_secs(5)
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(proxy.sessions.balancer.reserved("east"), 0);
    assert_eq!(proxy.sessions.counts().get("east"), Some(&1));
    attack(&mut player, TARGET_ID);
    target.join().unwrap().unwrap();
    relay.join().unwrap().unwrap();
}

/// A switch that fails gives the place back too.
#[test]
fn the_place_reserved_for_a_switch_that_fails_is_given_back() {
    use crate::balance::Server;
    let World {
        proxy,
        mut player,
        _source,
        ..
    } = world(Sidecars::default());
    let (_, handle) = proxy.sessions.session("Alex").unwrap();
    let place = proxy
        .sessions
        .balancer
        .reserve_named(
            Server {
                backend: "nowhere",
                soft_cap: 5,
                hard_cap: 5,
                available: true,
                ping: None,
                reported_players: None,
            },
            || proxy.sessions.counts(),
        )
        .unwrap();
    in_game(
        proxy.config(),
        Arc::clone(&proxy.sessions.switcher),
        handle,
        "Alex".into(),
        "198.51.100.7".parse().unwrap(),
        "nowhere".into(),
        Some(place),
    );
    let started = std::time::Instant::now();
    while proxy.sessions.balancer.reserved("nowhere") != 0
        && started.elapsed() < Duration::from_secs(5)
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(proxy.sessions.balancer.reserved("nowhere"), 0);
    let _ = &mut player;
}

/// The new server placed the player, but the old one would not let them go: nothing
/// undoes the placing, so the player stays on the new server and the operator is told,
/// in the console's lines and in the log, with the id of the transfer.
#[test]
fn a_source_that_keeps_the_player_is_an_alert_not_a_failure() {
    let World {
        proxy,
        log,
        mut player,
        _source,
        relay,
        target,
    } = world(Sidecars {
        source_refuses: Some(("release", "that hand-over is over".to_owned())),
        ..Sidecars::default()
    });
    let lines = Commands::builtin().execute(&proxy, "switch alex east");
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(
        lines[0].starts_with("Alex moved from lobby to east in "),
        "{}",
        lines[0]
    );
    assert!(
        lines[1].starts_with(
            "ALERT Alex: transfer 9f2c4be0a1d34e6b8c0f1e2d3c4b5a69 from lobby to east: lobby \
             would not let the player go (that hand-over is over)"
        ),
        "{}",
        lines[1]
    );
    // No drop and no abort: the target has the player placed.
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
    // The alert is counted, and the transfer is a success all the same.
    let page = crate::metrics::render(&proxy);
    assert!(
        page.contains(
            "
meridian_transfer_alerts_total 1
"
        ),
        "{page}"
    );
    assert!(
        page.contains(
            "
meridian_transfers_total 1
"
        ),
        "{page}"
    );
    // The player is told they are on east, and plays on there.
    let (chat, live) = client_after_switch(&mut player);
    assert_eq!(chat, ["You are now on east."]);
    assert_eq!(live, effect(CLIENT_ID));
    attack(&mut player, TARGET_ID);
    assert_eq!(target.join().unwrap().unwrap().attacked, CLIENT_ID);
    relay.join().unwrap().unwrap();
}

#[test]
fn a_freeze_limit_a_server_would_not_allow_is_warned_about_at_start() {
    let with = |limit: u64, sidecars: Vec<Sidecar>| Config {
        sidecars,
        switch_freeze_limit_ms: limit,
        ..Config::default()
    };
    let one = || vec![sidecar("lobby", 25580)];
    // The default is well below Horizon's 30 seconds.
    assert!(startup_warnings(&with(8_000, one())).is_empty());
    assert!(startup_warnings(&with(29_999, one())).is_empty());
    let warnings = startup_warnings(&with(30_000, one()));
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("max_freeze_seconds"), "{warnings:?}");
    assert!(warnings[0].contains("30000 ms"), "{warnings:?}");
    // Without a sidecar no switch happens, so there is nothing to warn of.
    assert!(startup_warnings(&with(60_000, Vec::new())).is_empty());
}

// ---- The scripted player of a real run, over TCP, typing in game.

/// A lobby on a TCP port of its own: it logs the proxy in, configures the player, sends them
/// its world (the source's), and reads whatever comes until the proxy closes the connection.
fn spawn_lobby() -> (String, JoinHandle<Result<(), String>>) {
    use horizon_protocol::ids::configuration as cfg;
    use meridian_testkit::{
        backend::{FakeBackend, finish_configuration},
        peer::Step,
        trace::Recorder,
    };
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let thread = std::thread::spawn(move || {
        let (stream, _) = listener.accept().map_err(|e| e.to_string())?;
        let mut connection = Connection::from_stream(stream).map_err(|e| e.to_string())?;
        FakeBackend {
            after_login: vec![
                Step::Expect {
                    id: cfg::serverbound::CLIENT_INFORMATION,
                },
                Step::send(cfg::clientbound::REGISTRY_DATA, REGISTRY),
                Step::send(cfg::clientbound::SELECT_KNOWN_PACKS, PACKS),
                Step::Expect {
                    id: cfg::serverbound::SELECT_KNOWN_PACKS,
                },
                finish_configuration(),
                Step::Expect {
                    id: cfg::serverbound::FINISH_CONFIGURATION,
                },
            ],
            ..FakeBackend::default()
        }
        .run(&mut connection, &Recorder::new())?;
        for (id, payload) in source_world() {
            send(&mut connection, id, &payload);
        }
        connection
            .reader
            .set_read_timeout(Some(Duration::from_secs(20)))
            .map_err(|e| e.to_string())?;
        loop {
            match connection.recv() {
                Ok(_) => {}
                Err(e) if e.is_end_of_stream() => return Ok(()),
                Err(e) => return Err(e.to_string()),
            }
        }
    });
    (address, thread)
}

/// A bot that logs in over TCP and types `/server east` in game, as the network simulator's
/// bots will: the proxy answers in the chat, moves it from lobby to east with fake sidecars
/// and a fake east, and the bot reads both lines.
#[test]
fn a_scripted_bot_types_server_east_and_reads_the_proxys_answers() {
    use meridian_testkit::{
        live::{End, Options, link, session_linked},
        trace::{Dir, Recorder},
    };
    let (lobby, lobby_thread) = spawn_lobby();
    let (east, signal, east_thread) = spawn_target(TargetWorld {
        information: meridian_testkit::live::information_payload(),
        // The bot says nothing after the move until the test is done with it, which on a
        // slow machine can be long.
        idle: Duration::from_secs(60),
        ..TargetWorld::default()
    });
    let config = Config {
        backends: vec![
            Backend {
                name: "lobby".into(),
                address: lobby,
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
    let controls = {
        let log = log.clone();
        move |sidecar: &Sidecar, _: Duration| -> Result<Box<dyn PlayerControl>, String> {
            let east = sidecar.backend == "east";
            Ok(Box::new(FakeSidecar {
                backend: sidecar.backend.clone(),
                log: log.clone(),
                signal: east.then(|| signal.lock().unwrap().clone()),
                refuse: None,
                on_export: None,
            }))
        }
    };
    proxy.sessions = Arc::new(Sessions::with_switcher(Arc::new(Switcher {
        controls: Arc::new(controls),
        digests: Digests::default(),
        metrics: Arc::default(),
    })));
    let proxy = Arc::new(proxy);
    let listener = crate::serve::listen("127.0.0.1:0".parse().unwrap()).unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = {
        let proxy = Arc::clone(&proxy);
        std::thread::spawn(move || crate::serve::serve(listener, &proxy))
    };

    let (handle, link) = link();
    let recorder = Recorder::new();
    let bot = std::thread::spawn({
        let recorder = recorder.clone();
        move || {
            session_linked(
                &Options {
                    host: "127.0.0.1".into(),
                    port,
                    name: "Bot".into(),
                    // Long enough for a slow machine; the test ends the session itself.
                    seconds: 30,
                },
                &recorder,
                &link,
            )
        }
    });
    // The proxy refuses a move until it has the player's world: its Login packet must have
    // gone through, and that, not the bot being in the play phase, is what the move needs. So
    // type only once the bot has been sent it and the proxy lists the bot on lobby.
    let joined = Instant::now() + Duration::from_secs(15);
    let has_joined = || {
        recorder.trace().ids(Dir::ToClient).contains(&pc::LOGIN)
            && proxy
                .sessions
                .players()
                .iter()
                .any(|p| p.name == "Bot" && p.backend == "lobby")
    };
    while !has_joined() && Instant::now() < joined {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        has_joined(),
        "the bot did not join: {:?}",
        proxy.sessions.players()
    );
    handle.controller.command("/server east").unwrap();
    let wait = |wanted: &'static str| {
        handle
            .journal
            .wait_for(0, Duration::from_secs(15), move |line| line == wanted)
    };
    let moving = wait("Moving you to east...");
    let now_on = wait("You are now on east.");
    let (moving, now_on) = (
        moving.unwrap_or_else(|| panic!("no answer: {:?}", handle.journal.lines())),
        now_on.unwrap_or_else(|| panic!("not moved: {:?}", handle.journal.lines())),
    );
    assert!(moving.seq < now_on.seq, "the move was announced first");
    // The proxy lists the bot on east, and the source's sidecar let them go.
    let on_east = proxy
        .sessions
        .players()
        .iter()
        .any(|p| p.name == "Bot" && p.backend == "east");
    assert!(on_east, "{:?}", proxy.sessions.players());
    assert!(
        log.lines().contains(&"lobby release Moved".to_owned()),
        "{:?}",
        log.lines()
    );

    // A move that cannot be made is read as plainly: the answer names the server.
    let mark = handle.journal.mark();
    handle.controller.command("server nowhere").unwrap();
    let refused = handle
        .journal
        .wait_for(mark, Duration::from_secs(15), |line| {
            line.contains("nowhere")
        })
        .unwrap_or_else(|| panic!("no answer: {:?}", handle.journal.lines()));
    assert!(!refused.text.contains("You are now on"), "{}", refused.text);
    eprintln!("{}", refused.text);

    // The bot leaves when the test is done, cleanly: it was not disconnected on the way.
    handle.controller.leave().unwrap();
    assert_eq!(bot.join().unwrap(), End::Clean);
    proxy.stop();
    server.join().unwrap().unwrap();
    // The target waits for an attack the bot does not make: it ends when the bot leaves.
    let _ = east_thread.join();
    lobby_thread.join().unwrap().unwrap();
}
