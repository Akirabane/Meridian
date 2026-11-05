//! Both directions over the signed exchange, to a real sidecar whose connection to the
//! server's management protocol is played by a thread (the sidecar's own tests play the
//! WebSocket against a fake management server).
use super::super::events::SOURCE;
use super::super::hear::{self, Hearers};
use super::super::push;
use super::config;
use crate::console::Commands;
use crate::state::Proxy;
use meridian_config::{Config, Sidecar};
use meridian_sidecar::management::Notices;
use serde_json::{Value, json};
use std::net::{Ipv4Addr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const SECRET: &[u8] = b"the secret the proxy and the sidecar share";
const ID: &str = "069a79f4-44e9-4726-a5be-fca90e38aaf5";

/// A sidecar on 127.0.0.1 whose server's management connection is up, and what the "server"
/// holds in its lists.
struct Server {
    notices: Arc<Notices>,
    stop: Arc<AtomicBool>,
    serving: Option<JoinHandle<()>>,
    playing: Option<JoinHandle<()>>,
    /// The calls the "server" received, as the JSON-RPC text the sidecar wrote.
    received: Arc<Mutex<Vec<Value>>>,
    sidecar: Sidecar,
    folder: std::path::PathBuf,
}

fn server(backend: &str, bans: Value, allowlist: Value) -> Server {
    let notices = Arc::new(Notices::default());
    notices.lists.begin_connection();
    notices.set_watching(true);
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let settings = Arc::new(meridian_sidecar::server::Settings {
        secret: SECRET.to_vec(),
        endpoint: Box::new(|| Err(meridian_sidecar::Error::Endpoint("no backend".into()))),
        endpoint_timeout: Duration::from_secs(1),
        idle_timeout: Duration::from_secs(10),
        insecure_remote: false,
        notices: Some(Arc::clone(&notices)),
    });
    let stop = Arc::new(AtomicBool::new(false));
    let serving = {
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            meridian_sidecar::server::serve(listener, settings, &stop).unwrap();
        })
    };
    let received = Arc::new(Mutex::new(Vec::new()));
    let playing = {
        let (notices, stop, received) = (
            Arc::clone(&notices),
            Arc::clone(&stop),
            Arc::clone(&received),
        );
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                for call in notices.lists.take_calls() {
                    let text: Value = serde_json::from_str(call.text()).unwrap();
                    let result = match text["method"].as_str().unwrap() {
                        "minecraft:bans" => bans.clone(),
                        "minecraft:allowlist" => allowlist.clone(),
                        "minecraft:ip_bans" => json!([]),
                        _ => json!([]),
                    };
                    received.lock().unwrap().push(text);
                    let _ = call.into_reply().send(Ok(result));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        })
    };
    // One folder for each server of each test: tests run side by side in one process, and the
    // one that ends removes only its own.
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let folder = std::env::temp_dir().join(format!(
        "meridian-access-sync-{}-{}-{backend}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&folder).unwrap();
    let secret_file = folder.join("sidecar.key");
    std::fs::write(&secret_file, SECRET).unwrap();
    Server {
        notices,
        stop,
        serving: Some(serving),
        playing: Some(playing),
        received,
        sidecar: Sidecar {
            backend: backend.into(),
            address,
            secret_file,
            notices: false,
        },
        folder,
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for thread in [self.serving.take(), self.playing.take()]
            .into_iter()
            .flatten()
        {
            let _ = thread.join();
        }
        let _ = std::fs::remove_dir_all(&self.folder);
    }
}

fn banned_on_east(name: &str) -> Value {
    json!({ "player": { "name": name }, "source": "Kim", "reason": "griefing" })
}

fn notification(method: &str, element: Value) -> String {
    json!({ "jsonrpc": "2.0", "method": format!("minecraft:notification/{method}"), "params": [element] })
        .to_string()
}

fn configured(east: &Server) -> Config {
    let mut config = config(true, true, &["east"]);
    config.sidecars = vec![east.sidecar.clone()];
    config.sidecar_timeout_ms = 3_000;
    config
}

fn proxy(config: Config) -> Proxy {
    Proxy::new(
        config,
        "does-not-exist/meridian.toml".into(),
        Arc::new(AtomicBool::new(false)),
    )
}

#[test]
fn what_a_server_bans_and_pardons_reaches_the_proxy_over_the_signed_exchange() {
    let east = server(
        "east",
        json!([banned_on_east("Steve")]),
        json!([{ "id": ID, "name": "Alex" }]),
    );
    let proxy = proxy(configured(&east));
    let hearers = Hearers::default();
    let round = |proxy: &Proxy| {
        hear::round(
            &proxy.config(),
            &hear::open_sidecar,
            &hearers,
            &proxy.sessions,
        );
    };
    // The first round reads the lists the server holds.
    round(&proxy);
    let access = proxy.sessions.access.snapshot();
    assert_eq!(access.bans().len(), 1);
    assert_eq!(
        access.bans()[0].reason.as_deref(),
        Some("banned on east: griefing")
    );
    assert_eq!(access.allowed().len(), 1);
    let methods: Vec<String> = east
        .received
        .lock()
        .unwrap()
        .iter()
        .map(|r| r["method"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        methods,
        ["minecraft:bans", "minecraft:ip_bans", "minecraft:allowlist"]
    );
    // Reading a list sends no `params`: the server refuses a null one.
    assert!(
        east.received
            .lock()
            .unwrap()
            .iter()
            .all(|r| r.get("params").is_none())
    );

    // A ban made on the server while the proxy listens, then its pardon.
    assert!(
        east.notices
            .lists
            .offer(&notification("bans/added", banned_on_east("Alex")))
    );
    round(&proxy);
    assert_eq!(proxy.sessions.access.snapshot().bans().len(), 2);
    assert!(
        east.notices
            .lists
            .offer(&notification("bans/removed", json!({ "name": "Alex" })))
    );
    round(&proxy);
    let names: Vec<String> = proxy
        .sessions
        .access
        .snapshot()
        .bans()
        .iter()
        .map(|b| b.target.to_string())
        .collect();
    assert_eq!(names, ["Steve"]);
}

#[test]
fn a_ban_at_the_console_reaches_the_server_and_its_echo_changes_nothing() {
    let east = server("east", json!([]), json!([]));
    let proxy = proxy(configured(&east));
    let hearers = Hearers::default();
    let round = |proxy: &Proxy| {
        hear::round(
            &proxy.config(),
            &hear::open_sidecar,
            &hearers,
            &proxy.sessions,
        );
    };
    round(&proxy);
    let before = east.received.lock().unwrap().len();

    let lines = Commands::builtin().execute(&proxy, "ban Alex griefing");
    assert_eq!(lines[0], "banned Alex: griefing");
    let open: Arc<push::Open> = Arc::new(push::open_sidecar);
    for attempt in push::start(&proxy.config(), &open, &proxy.access_sync, Instant::now()) {
        attempt.join().unwrap();
    }
    let received = east.received.lock().unwrap();
    let call = &received[before];
    assert_eq!(call["method"], "minecraft:bans/add");
    assert_eq!(
        call["params"],
        json!({ "add": [{ "player": { "name": "Alex" }, "source": SOURCE, "reason": "griefing" }] })
    );
    drop(received);
    assert_eq!(proxy.access_sync.waiting("east"), 0);

    // The server announces the ban to every client, the sidecar among them: it is the
    // proxy's own, and the next round neither adds nor sends anything.
    let echo = notification(
        "bans/added",
        json!({ "player": { "name": "Alex" }, "source": SOURCE, "reason": "griefing" }),
    );
    assert!(east.notices.lists.offer(&echo));
    let calls = east.received.lock().unwrap().len();
    round(&proxy);
    let access = proxy.sessions.access.snapshot();
    assert_eq!(access.bans().len(), 1);
    assert_eq!(
        access.bans()[0].reason.as_deref(),
        Some("griefing"),
        "not marked as the server's"
    );
    assert_eq!(proxy.access_sync.waiting("east"), 0);
    for attempt in push::start(&proxy.config(), &open, &proxy.access_sync, Instant::now()) {
        attempt.join().unwrap();
    }
    assert_eq!(
        east.received.lock().unwrap().len(),
        calls,
        "nothing was sent back"
    );
}

#[test]
fn a_ban_taken_from_a_server_is_not_sent_to_the_others() {
    let east = server("east", json!([]), json!([]));
    let west = server("west", json!([]), json!([]));
    let mut config = config(true, true, &["east", "west"]);
    config.sidecars = vec![east.sidecar.clone(), west.sidecar.clone()];
    config.sidecar_timeout_ms = 3_000;
    let proxy = proxy(config);
    let hearers = Hearers::default();
    hear::round(
        &proxy.config(),
        &hear::open_sidecar,
        &hearers,
        &proxy.sessions,
    );
    assert!(
        east.notices
            .lists
            .offer(&notification("bans/added", banned_on_east("Alex")))
    );
    hear::round(
        &proxy.config(),
        &hear::open_sidecar,
        &hearers,
        &proxy.sessions,
    );
    assert_eq!(proxy.sessions.access.snapshot().bans().len(), 1);
    // West is refused the player by the proxy, and is not told: only the console sends.
    let open: Arc<push::Open> = Arc::new(push::open_sidecar);
    for attempt in push::start(&proxy.config(), &open, &proxy.access_sync, Instant::now()) {
        attempt.join().unwrap();
    }
    let adds = |server: &Server| {
        server
            .received
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r["method"] == "minecraft:bans/add")
            .count()
    };
    assert_eq!((adds(&east), adds(&west)), (0, 0));
}

#[test]
fn a_sidecar_without_a_connection_to_its_server_is_not_reached_and_the_ban_is_kept_for_later() {
    let east = server("east", json!([]), json!([]));
    east.notices.lists.end_connection();
    east.notices.set_watching(false);
    let proxy = proxy(configured(&east));
    let lines = Commands::builtin().execute(&proxy, "ban Alex");
    assert_eq!(lines[0], "banned Alex");
    let open: Arc<push::Open> = Arc::new(push::open_sidecar);
    let now = Instant::now();
    for attempt in push::start(&proxy.config(), &open, &proxy.access_sync, now) {
        attempt.join().unwrap();
    }
    // Not lost for good: it waits for the next try, and the player is refused meanwhile.
    assert_eq!(proxy.access_sync.waiting("east"), 1);
    assert_eq!(proxy.sessions.access.snapshot().bans().len(), 1);
    assert!(east.received.lock().unwrap().is_empty());
    // The connection comes back; the next try, after the wait, delivers it.
    east.notices.lists.begin_connection();
    east.notices.set_watching(true);
    let later = now + push::RETRIES[0] + Duration::from_secs(1);
    for attempt in push::start(&proxy.config(), &open, &proxy.access_sync, later) {
        attempt.join().unwrap();
    }
    assert_eq!(proxy.access_sync.waiting("east"), 0);
    assert_eq!(east.received.lock().unwrap().len(), 1);
}
