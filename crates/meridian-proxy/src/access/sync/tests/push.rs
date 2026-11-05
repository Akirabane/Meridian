//! What the console tells the servers, with a stand-in for each sidecar.
use super::super::events::SOURCE;
use super::super::push::*;
use super::{config, sidecar};
use crate::console::Commands;
use crate::state::Proxy;
use meridian_config::{Config, Target};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const ID: &str = "069a79f4-44e9-4726-a5be-fca90e38aaf5";

fn target(text: &str) -> Target {
    Target::parse(text).unwrap()
}

fn ban(who: &str, reason: Option<&str>) -> Change {
    Change::Ban {
        who: target(who),
        reason: reason.map(str::to_owned),
    }
}

#[test]
fn a_ban_becomes_the_call_the_management_protocol_takes() {
    assert_eq!(
        ban("Alex", Some("griefing")).call().unwrap(),
        (
            "bans/add",
            json!({ "add": [{ "player": { "name": "Alex" }, "source": SOURCE, "reason": "griefing" }] })
        )
    );
    // Without a reason there is none; by unique id the id is sent, hyphenated.
    assert_eq!(
        ban(ID, None).call().unwrap(),
        (
            "bans/add",
            json!({ "add": [{ "player": { "id": ID }, "source": SOURCE }] })
        )
    );
    assert_eq!(
        ban("203.0.113.9", Some("spam")).call().unwrap(),
        (
            "ip_bans/add",
            json!({ "add": [{ "ip": "203.0.113.9", "source": SOURCE, "reason": "spam" }] })
        )
    );
}

#[test]
fn a_pardon_and_the_whitelist_changes_become_their_calls() {
    let call = |change: Change| change.call().unwrap();
    assert_eq!(
        call(Change::Pardon {
            who: target("Alex")
        }),
        ("bans/remove", json!({ "remove": [{ "name": "Alex" }] }))
    );
    assert_eq!(
        call(Change::Pardon { who: target(ID) }),
        ("bans/remove", json!({ "remove": [{ "id": ID }] }))
    );
    assert_eq!(
        call(Change::Pardon {
            who: target("203.0.113.9")
        }),
        ("ip_bans/remove", json!({ "ip": ["203.0.113.9"] }))
    );
    assert_eq!(
        call(Change::Allow {
            who: target("Alex")
        }),
        ("allowlist/add", json!({ "add": [{ "name": "Alex" }] }))
    );
    assert_eq!(
        call(Change::Disallow { who: target(ID) }),
        ("allowlist/remove", json!({ "remove": [{ "id": ID }] }))
    );
}

#[test]
fn a_range_of_addresses_cannot_be_sent() {
    let why = ban("203.0.113.0/24", None).call().unwrap_err();
    assert!(
        why.contains("range") && why.contains("single addresses"),
        "{why}"
    );
    assert!(
        Change::Pardon {
            who: target("203.0.113.0/24")
        }
        .call()
        .is_err()
    );
    // An address on the whitelist is already refused by the whitelist itself.
    assert!(
        Change::Allow {
            who: target("203.0.113.9")
        }
        .call()
        .is_err()
    );
}

#[test]
fn a_lane_keeps_the_order_and_tries_one_change_at_a_time() {
    let outbox = Outbox::default();
    let backends = ["east".to_owned(), "west".to_owned()];
    let first = ban("Alex", None);
    let second = Change::Pardon {
        who: target("Alex"),
    };
    outbox.send(&backends, &first).unwrap();
    outbox.send(&backends, &second).unwrap();
    let now = Instant::now();
    // Both lanes offer their first change, and only that.
    assert_eq!(
        outbox.take_ready(now),
        [
            ("east".to_owned(), first.clone()),
            ("west".to_owned(), first.clone())
        ]
    );
    assert!(outbox.take_ready(now).is_empty(), "under way: nothing else");
    assert_eq!(outbox.finish("east", &Outcome::Sent, now), Fate::Done);
    // East goes on to the second; west is still on the first.
    assert_eq!(
        outbox.take_ready(now),
        [("east".to_owned(), second.clone())]
    );
    assert_eq!(outbox.waiting("west"), 2);
}

#[test]
fn a_server_that_cannot_be_reached_is_tried_again_a_bounded_number_of_times() {
    let outbox = Outbox::default();
    outbox
        .send(&["east".to_owned()], &ban("Alex", None))
        .unwrap();
    let lost = Outcome::Lost("connection refused".into());
    let mut now = Instant::now();
    for (n, wait) in RETRIES.iter().enumerate() {
        assert_eq!(outbox.take_ready(now).len(), 1, "try {}", n + 1);
        assert_eq!(outbox.finish("east", &lost, now), Fate::Retry(*wait, n + 1));
        // Not before the wait is over.
        assert!(
            outbox
                .take_ready(now + *wait - Duration::from_millis(1))
                .is_empty()
        );
        now += *wait;
    }
    assert_eq!(outbox.take_ready(now).len(), 1, "the last try");
    assert_eq!(
        outbox.finish("east", &lost, now),
        Fate::GaveUp(RETRIES.len() + 1)
    );
    assert_eq!(outbox.waiting("east"), 0);
    assert!(
        outbox
            .take_ready(now + Duration::from_secs(1000))
            .is_empty()
    );
}

#[test]
fn a_refusal_is_not_tried_again_and_a_given_up_change_lets_the_next_through() {
    let outbox = Outbox::default();
    let east = ["east".to_owned()];
    outbox.send(&east, &ban("Alex", None)).unwrap();
    outbox.send(&east, &ban("Steve", None)).unwrap();
    let now = Instant::now();
    outbox.take_ready(now);
    let refused = Outcome::Refused("Invalid params: unknown profile".into());
    assert_eq!(outbox.finish("east", &refused, now), Fate::Done);
    assert_eq!(outbox.waiting("east"), 1);
    // The next change starts clean: no wait carried over.
    assert_eq!(outbox.take_ready(now).len(), 1);
}

#[test]
fn a_lane_holds_so_many_changes_and_drops_the_oldest_past_them() {
    let outbox = Outbox::default();
    let east = ["east".to_owned()];
    for i in 0..LANE_LEN + 5 {
        outbox.send(&east, &ban(&format!("P{i}"), None)).unwrap();
    }
    assert_eq!(outbox.waiting("east"), LANE_LEN);
    let ready = outbox.take_ready(Instant::now());
    assert_eq!(ready[0].1, ban("P5", None));
}

#[test]
fn a_change_no_server_can_take_queues_nothing() {
    let outbox = Outbox::default();
    assert!(
        outbox
            .send(&["east".to_owned()], &ban("203.0.113.0/24", None))
            .is_err()
    );
    assert_eq!(outbox.waiting("east"), 0);
}

/// What the stand-in sidecars were called with, and how each answers.
#[derive(Default)]
struct Servers {
    calls: Mutex<Vec<(String, String, Value)>>,
    /// Backends that cannot be reached.
    down: Mutex<Vec<String>>,
    /// Backends that refuse with this.
    refuse: Mutex<HashMap<String, String>>,
    /// A backend that waits for a signal before it answers.
    hold: Mutex<HashMap<String, mpsc::Receiver<()>>>,
}

impl Servers {
    fn calls_to(&self, backend: &str) -> Vec<(String, Value)> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(b, ..)| b == backend)
            .map(|(_, m, p)| (m.clone(), p.clone()))
            .collect()
    }

    fn open(self: &Arc<Self>) -> Arc<Open> {
        let servers = Arc::clone(self);
        Arc::new(move |sidecar: &meridian_config::Sidecar| {
            if servers.down.lock().unwrap().contains(&sidecar.backend) {
                return Err(Failure {
                    lost: true,
                    why: "connection refused".into(),
                });
            }
            Ok(Box::new(Stand {
                servers: Arc::clone(&servers),
                backend: sidecar.backend.clone(),
            }) as Box<dyn Manager>)
        })
    }
}

struct Stand {
    servers: Arc<Servers>,
    backend: String,
}

impl Manager for Stand {
    fn call(&mut self, method: &str, params: Value) -> Result<(), Failure> {
        let held = self.servers.hold.lock().unwrap().remove(&self.backend);
        if let Some(signal) = held {
            let _ = signal.recv_timeout(Duration::from_secs(10));
        }
        if let Some(why) = self.servers.refuse.lock().unwrap().get(&self.backend) {
            return Err(Failure {
                lost: false,
                why: why.clone(),
            });
        }
        self.servers
            .calls
            .lock()
            .unwrap()
            .push((self.backend.clone(), method.to_owned(), params));
        Ok(())
    }
}

fn proxy(config: Config) -> Proxy {
    Proxy::new(
        config,
        "does-not-exist/meridian.toml".into(),
        Arc::new(AtomicBool::new(false)),
    )
}

fn run(proxy: &Proxy, line: &str) -> Vec<String> {
    Commands::builtin().execute(proxy, line)
}

/// Starts what is ready at `now` and waits for the attempts.
fn pump(proxy: &Proxy, servers: &Arc<Servers>, now: Instant) {
    for attempt in start(&proxy.config(), &servers.open(), &proxy.access_sync, now) {
        attempt.join().unwrap();
    }
}

#[test]
fn a_ban_at_the_console_reaches_every_server_in_the_right_form() {
    let proxy = proxy(config(false, true, &["lobby", "east", "west"]));
    let servers = Arc::new(Servers::default());
    let lines = run(&proxy, "ban Alex griefing");
    assert_eq!(lines[0], "banned Alex: griefing");
    assert!(
        lines
            .iter()
            .any(|l| l.contains("sending the ban of Alex to the servers (lobby, east, west)")),
        "{lines:?}"
    );
    pump(&proxy, &servers, Instant::now());
    let expected = (
        "bans/add".to_owned(),
        json!({ "add": [{ "player": { "name": "Alex" }, "source": SOURCE, "reason": "griefing" }] }),
    );
    for backend in ["lobby", "east", "west"] {
        assert_eq!(
            servers.calls_to(backend),
            std::slice::from_ref(&expected),
            "{backend}"
        );
    }
    // The pardon, the address ban and the whitelist follow the same way.
    run(&proxy, "pardon Alex");
    run(&proxy, "ban 203.0.113.9");
    run(&proxy, "whitelist add Steve");
    run(&proxy, "whitelist remove Steve");
    for _ in 0..4 {
        pump(&proxy, &servers, Instant::now());
    }
    let methods: Vec<String> = servers
        .calls_to("east")
        .into_iter()
        .map(|(m, _)| m)
        .collect();
    assert_eq!(
        methods,
        [
            "bans/add",
            "bans/remove",
            "ip_bans/add",
            "allowlist/add",
            "allowlist/remove"
        ]
    );
}

#[test]
fn nothing_is_sent_while_to_servers_is_off() {
    let proxy = proxy(config(true, false, &["lobby", "east"]));
    let lines = run(&proxy, "ban Alex");
    assert_eq!(lines, ["banned Alex"]);
    assert_eq!(proxy.access_sync.waiting("east"), 0);
    let servers = Arc::new(Servers::default());
    pump(&proxy, &servers, Instant::now());
    assert!(servers.calls.lock().unwrap().is_empty());
}

#[test]
fn a_server_that_is_down_never_holds_the_local_ban_and_is_told_later() {
    let proxy = proxy(config(false, true, &["lobby", "east"]));
    let servers = Arc::new(Servers::default());
    servers.down.lock().unwrap().push("east".into());
    let begun = Instant::now();
    let lines = run(&proxy, "ban Alex");
    assert!(
        begun.elapsed() < Duration::from_secs(1),
        "the console did not wait"
    );
    assert_eq!(lines[0], "banned Alex");
    // Banned at once on the proxy.
    assert_eq!(proxy.sessions.access.snapshot().bans().len(), 1);
    let now = Instant::now();
    pump(&proxy, &servers, now);
    assert_eq!(servers.calls_to("lobby").len(), 1);
    assert!(servers.calls_to("east").is_empty());
    assert_eq!(proxy.access_sync.waiting("east"), 1);
    // Before the wait is over east is not tried; after it, and up again, it is told.
    servers.down.lock().unwrap().clear();
    pump(&proxy, &servers, now + Duration::from_secs(1));
    assert!(servers.calls_to("east").is_empty());
    pump(&proxy, &servers, now + RETRIES[0] + Duration::from_secs(1));
    assert_eq!(servers.calls_to("east").len(), 1);
    assert_eq!(proxy.access_sync.waiting("east"), 0);
    // Lobby was told once, not again.
    assert_eq!(servers.calls_to("lobby").len(), 1);
}

#[test]
fn a_ban_and_the_pardon_after_it_reach_a_late_server_in_that_order() {
    let proxy = proxy(config(false, true, &["east"]));
    let servers = Arc::new(Servers::default());
    servers.down.lock().unwrap().push("east".into());
    run(&proxy, "ban Alex");
    run(&proxy, "pardon Alex");
    let mut now = Instant::now();
    pump(&proxy, &servers, now);
    servers.down.lock().unwrap().clear();
    for _ in 0..3 {
        now += Duration::from_secs(130);
        pump(&proxy, &servers, now);
    }
    let methods: Vec<String> = servers
        .calls_to("east")
        .into_iter()
        .map(|(m, _)| m)
        .collect();
    assert_eq!(methods, ["bans/add", "bans/remove"]);
}

#[test]
fn a_slow_server_holds_up_no_other_and_not_the_console() {
    let proxy = proxy(config(false, true, &["lobby", "east"]));
    let servers = Arc::new(Servers::default());
    let (release, held) = mpsc::channel();
    servers.hold.lock().unwrap().insert("east".into(), held);
    run(&proxy, "ban Alex");
    let attempts = start(
        &proxy.config(),
        &servers.open(),
        &proxy.access_sync,
        Instant::now(),
    );
    assert_eq!(attempts.len(), 2);
    // Lobby is told while east still waits; the console, meanwhile, answers.
    let until = Instant::now() + Duration::from_secs(5);
    while servers.calls_to("lobby").is_empty() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(servers.calls_to("lobby").len(), 1);
    assert!(servers.calls_to("east").is_empty());
    let started = Instant::now();
    assert_eq!(run(&proxy, "banlist")[0], "1 ban:");
    assert!(started.elapsed() < Duration::from_secs(1));
    release.send(()).unwrap();
    for attempt in attempts {
        attempt.join().unwrap();
    }
    assert_eq!(servers.calls_to("east").len(), 1);
}

#[test]
fn a_server_that_says_no_is_not_asked_again() {
    let proxy = proxy(config(false, true, &["east"]));
    let servers = Arc::new(Servers::default());
    servers
        .refuse
        .lock()
        .unwrap()
        .insert("east".into(), "Invalid params: unknown profile".into());
    run(&proxy, "ban Ghost");
    pump(&proxy, &servers, Instant::now());
    assert_eq!(proxy.access_sync.waiting("east"), 0);
    assert!(servers.calls.lock().unwrap().is_empty());
}

#[test]
fn a_range_is_banned_here_and_said_not_to_be_sent() {
    let proxy = proxy(config(false, true, &["east"]));
    let lines = run(&proxy, "ban 203.0.113.0/24 spam");
    assert_eq!(lines[0], "banned 203.0.113.0/24: spam");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("not sent to the servers") && l.contains("range")),
        "{lines:?}"
    );
    assert_eq!(proxy.sessions.access.snapshot().bans().len(), 1);
    assert_eq!(proxy.access_sync.waiting("east"), 0);
}

#[test]
fn with_no_sidecar_there_is_nobody_to_tell_and_the_operator_is_told_so() {
    let mut config = config(false, true, &["east"]);
    config.sidecars.clear();
    let proxy = proxy(config);
    let lines = run(&proxy, "ban Alex");
    assert!(
        lines.iter().any(|l| l.contains("no backend has a sidecar")),
        "{lines:?}"
    );
}

#[test]
fn a_backend_that_lost_its_sidecar_in_a_reload_is_forgotten() {
    let proxy = proxy(config(false, true, &["lobby", "east"]));
    run(&proxy, "ban Alex");
    let mut next = config(false, true, &["lobby", "east"]);
    next.sidecars = vec![sidecar("lobby")];
    proxy.set_config(next);
    let servers = Arc::new(Servers::default());
    pump(&proxy, &servers, Instant::now());
    assert_eq!(servers.calls_to("lobby").len(), 1);
    assert!(servers.calls_to("east").is_empty());
    assert_eq!(proxy.access_sync.waiting("east"), 0);
}
