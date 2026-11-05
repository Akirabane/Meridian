//! What the proxy takes from a server's lists, with a stand-in for each sidecar.
use super::super::events::{self, Event};
use super::super::hear::*;
use super::{config, sidecar};
use crate::session::{Open, Player, Sessions, tests::idle_session_on};
use horizon_protocol::{
    connection::Connection, crypto::offline_uuid, packets::configuration, text,
};
use meridian_config::{Access, Config, Target};
use meridian_sidecar::lists::AccessReport;
use meridian_sidecar::request::NoticeHeard;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const ID: &str = "069a79f4-44e9-4726-a5be-fca90e38aaf5";

fn notice(seq: u64, method: &str, params: Value) -> NoticeHeard {
    NoticeHeard {
        seq,
        line: json!({ "jsonrpc": "2.0", "method": format!("minecraft:notification/{method}"), "params": [params] })
            .to_string(),
        age: Duration::ZERO,
    }
}

fn banned(seq: u64, name: &str, reason: Option<&str>) -> NoticeHeard {
    let mut ban = json!({ "player": { "name": name }, "source": "Kim" });
    if let Some(reason) = reason {
        ban["reason"] = json!(reason);
    }
    notice(seq, "bans/added", ban)
}

fn report(connection: u64, last: u64, notices: Vec<NoticeHeard>) -> AccessReport {
    AccessReport {
        enabled: true,
        watching: true,
        connection,
        last,
        missed: false,
        notices,
    }
}

fn nothing() -> State {
    State {
        bans: json!([]),
        ip_bans: json!([]),
        allowlist: json!([]),
    }
}

/// What a sidecar answers, in order, and what it was asked.
#[derive(Default)]
struct Script {
    reports: Mutex<VecDeque<Result<AccessReport, String>>>,
    states: Mutex<VecDeque<Result<State, String>>>,
    asked: Mutex<Vec<Option<u64>>>,
    read: AtomicUsize,
    opened: AtomicUsize,
    refuse_to_open: Mutex<Option<String>>,
}

impl Script {
    fn with(
        reports: Vec<Result<AccessReport, String>>,
        states: Vec<Result<State, String>>,
    ) -> Arc<Self> {
        Arc::new(Script {
            reports: Mutex::new(reports.into()),
            states: Mutex::new(states.into()),
            ..Script::default()
        })
    }

    fn asked(&self) -> Vec<Option<u64>> {
        self.asked.lock().unwrap().clone()
    }

    fn reads(&self) -> usize {
        self.read.load(Ordering::SeqCst)
    }

    fn open(
        self: &Arc<Self>,
    ) -> impl Fn(&meridian_config::Sidecar) -> Result<Box<dyn Source>, String> + use<> {
        let script = Arc::clone(self);
        move |_| {
            script.opened.fetch_add(1, Ordering::SeqCst);
            if let Some(why) = script.refuse_to_open.lock().unwrap().clone() {
                return Err(why);
            }
            Ok(Box::new(Scripted(Arc::clone(&script))))
        }
    }
}

struct Scripted(Arc<Script>);

impl Source for Scripted {
    fn report(&mut self, after: Option<u64>) -> Result<AccessReport, String> {
        self.0.asked.lock().unwrap().push(after);
        self.0
            .reports
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Ok(report(1, 0, vec![])))
    }

    fn state(&mut self) -> Result<State, String> {
        self.0.read.fetch_add(1, Ordering::SeqCst);
        self.0
            .states
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Ok(nothing()))
    }
}

fn run(config: &Config, script: &Arc<Script>, hearers: &Hearers, sessions: &Sessions) {
    round(config, &script.open(), hearers, sessions);
}

fn only_east() -> Config {
    config(true, false, &["east"])
}

fn access(sessions: &Sessions) -> Access {
    sessions.access.snapshot()
}

fn target(text: &str) -> Target {
    Target::parse(text).unwrap()
}

fn reason_of(access: &Access, who: &str) -> Option<String> {
    access
        .bans()
        .iter()
        .find(|b| b.target.same(&target(who)))
        .and_then(|b| b.reason.clone())
}

#[test]
fn a_ban_made_on_a_server_is_a_ban_of_the_proxy_with_the_server_as_its_source() {
    let sessions = Sessions::default();
    let hearers = Hearers::default();
    let script = Script::with(
        vec![
            Ok(report(1, 4, vec![])),
            Ok(report(1, 5, vec![banned(5, "Alex", Some("griefing"))])),
        ],
        vec![],
    );
    run(&only_east(), &script, &hearers, &sessions);
    assert!(access(&sessions).bans().is_empty());
    run(&only_east(), &script, &hearers, &sessions);
    let access = access(&sessions);
    assert_eq!(access.bans().len(), 1);
    assert_eq!(
        reason_of(&access, "Alex").as_deref(),
        Some("banned on east: griefing")
    );
    // The first answer gave where the count stands; the second asked for what came after.
    assert_eq!(script.asked(), [None, Some(4)]);
}

#[test]
fn a_player_banned_on_a_server_is_sent_away_from_the_proxy() {
    let sessions = Sessions::default();
    let (relay, handle, mut player, _server): (_, _, Connection, Connection) =
        idle_session_on("Alex", "east");
    let _open: Open<'_> = sessions
        .open(
            Player {
                name: "Alex".into(),
                uuid: offline_uuid("Alex").to_be_bytes(),
                backend: "lobby".into(),
                address: "198.51.100.7:50123".parse().unwrap(),
                joined: Instant::now(),
            },
            handle,
        )
        .unwrap();
    let relay = std::thread::spawn(move || relay.run());
    let script = Script::with(
        vec![
            Ok(report(1, 0, vec![])),
            Ok(report(1, 1, vec![banned(1, "Alex", Some("griefing"))])),
        ],
        vec![],
    );
    let hearers = Hearers::default();
    run(&only_east(), &script, &hearers, &sessions);
    run(&only_east(), &script, &hearers, &sessions);
    let (id, payload) = player.recv().unwrap();
    let configuration::Clientbound::Disconnect(d) =
        configuration::Clientbound::decode(id, &payload).unwrap()
    else {
        panic!("not a disconnect");
    };
    let shown = text::plain(&d.reason, &Default::default());
    assert!(shown.contains("banned on east: griefing"), "{shown}");
    assert!(relay.join().unwrap().is_ok());
}

#[test]
fn the_first_answer_reads_the_lists_and_takes_what_the_proxy_does_not_have() {
    let sessions = Sessions::default();
    let hearers = Hearers::default();
    let held = State {
        bans: json!([{ "player": { "id": ID, "name": "Alex" }, "source": "Kim", "reason": "griefing" }]),
        ip_bans: json!([{ "ip": "203.0.113.9", "source": "Rcon" }]),
        allowlist: json!([{ "id": ID, "name": "Alex" }]),
    };
    let script = Script::with(vec![Ok(report(1, 7, vec![]))], vec![Ok(held)]);
    run(&only_east(), &script, &hearers, &sessions);
    assert_eq!(script.reads(), 1);
    let access = access(&sessions);
    assert_eq!(
        reason_of(&access, ID).as_deref(),
        Some("banned on east: griefing")
    );
    assert_eq!(
        reason_of(&access, "203.0.113.9").as_deref(),
        Some("banned on east")
    );
    assert_eq!(access.allowed(), [target(ID)]);
}

#[test]
fn reading_a_servers_lists_never_removes_an_entry_of_the_proxy() {
    let sessions = Sessions::default();
    let (_, saved) = sessions.access.change(|a| {
        a.ban(target("Steve"), Some("our own reason".into()));
        a.ban(target("203.0.113.0/24"), None);
        let _ = a.allow(target("Herobrine"));
        ((), true)
    });
    assert!(saved.is_ok());
    let before = access(&sessions);
    // The server holds none of them, and an entry of its own.
    let held = State {
        bans: json!([{ "player": { "name": "Alex" }, "source": "Kim" }]),
        ip_bans: json!([]),
        allowlist: json!([]),
    };
    let script = Script::with(vec![Ok(report(1, 0, vec![]))], vec![Ok(held)]);
    run(&only_east(), &script, &Hearers::default(), &sessions);
    let after = access(&sessions);
    for ban in before.bans() {
        assert!(after.bans().contains(ban), "{ban:?} is gone");
    }
    assert_eq!(after.bans().len(), 3);
    assert_eq!(after.allowed(), before.allowed());
}

#[test]
fn a_ban_the_proxy_already_has_is_left_as_it_is_and_nothing_is_said() {
    let sessions = Sessions::default();
    let (_, _) = sessions.access.change(|a| {
        a.ban(target("Alex"), Some("ours".into()));
        ((), true)
    });
    let events = vec![
        Event::Ban {
            who: target("alex"),
            reason: Some("theirs".into()),
        },
        Event::Allow {
            who: target("Notch"),
        },
        Event::Allow {
            who: target("notch"),
        },
    ];
    let lines = apply_all(&sessions, "east", events);
    // Only the first whitelist entry was a change.
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(
        reason_of(&access(&sessions), "Alex").as_deref(),
        Some("ours")
    );
}

#[test]
fn a_pardon_on_a_server_lifts_only_the_ban_taken_from_that_server() {
    let sessions = Sessions::default();
    let (_, _) = sessions.access.change(|a| {
        a.ban(target("Alex"), Some(events::mark("east", Some("griefing"))));
        a.ban(target("Steve"), Some("our own".into()));
        a.ban(target("Notch"), Some(events::mark("west", None)));
        ((), true)
    });
    let pardon = |name: &str| Event::Pardon {
        who: vec![target(name)],
    };
    assert!(
        apply(&sessions, "east", pardon("Steve")).is_none(),
        "ours stays"
    );
    assert!(
        apply(&sessions, "east", pardon("Notch")).is_none(),
        "west's stays"
    );
    assert!(apply(&sessions, "east", pardon("Herobrine")).is_none());
    assert!(apply(&sessions, "east", pardon("alex")).is_some());
    let access = access(&sessions);
    let names: Vec<String> = access.bans().iter().map(|b| b.target.to_string()).collect();
    assert_eq!(names, ["Steve", "Notch"]);
}

#[test]
fn a_pardon_by_unique_id_or_by_name_finds_the_ban_either_way() {
    let sessions = Sessions::default();
    let (_, _) = sessions.access.change(|a| {
        a.ban(target(ID), Some(events::mark("east", None)));
        a.ban(target("203.0.113.9"), Some(events::mark("east", None)));
        ((), true)
    });
    let both = Event::Pardon {
        who: vec![target(ID), target("Alex")],
    };
    assert!(apply(&sessions, "east", both).is_some());
    let address = Event::Pardon {
        who: vec![target("203.0.113.9")],
    };
    assert!(apply(&sessions, "east", address).is_some());
    assert!(access(&sessions).bans().is_empty());
}

#[test]
fn a_player_allowed_on_a_server_is_added_to_the_whitelist_and_never_removed_by_it() {
    let sessions = Sessions::default();
    let notices = vec![
        notice(1, "allowlist/added", json!({ "id": ID, "name": "Alex" })),
        notice(2, "allowlist/removed", json!({ "id": ID, "name": "Alex" })),
    ];
    let script = Script::with(
        vec![Ok(report(1, 0, vec![])), Ok(report(1, 2, notices))],
        vec![],
    );
    let hearers = Hearers::default();
    run(&only_east(), &script, &hearers, &sessions);
    run(&only_east(), &script, &hearers, &sessions);
    assert_eq!(access(&sessions).allowed(), [target(ID)]);
}

#[test]
fn a_ban_for_a_time_is_said_and_not_taken() {
    let sessions = Sessions::default();
    let notices = vec![notice(
        1,
        "bans/added",
        json!({ "player": { "name": "Alex" }, "expires": "2030-01-01T00:00:00Z" }),
    )];
    let script = Script::with(
        vec![Ok(report(1, 0, vec![])), Ok(report(1, 1, notices))],
        vec![],
    );
    let hearers = Hearers::default();
    run(&only_east(), &script, &hearers, &sessions);
    run(&only_east(), &script, &hearers, &sessions);
    assert!(access(&sessions).bans().is_empty());
}

#[test]
fn a_notice_is_taken_once_and_a_ban_the_proxy_sent_is_not_taken_back() {
    let sessions = Sessions::default();
    // The proxy sent this ban (its source): it comes back from the server as a notice.
    let echo = notice(
        2,
        "bans/added",
        json!({ "player": { "name": "Alex" }, "source": events::SOURCE, "reason": "griefing" }),
    );
    let script = Script::with(
        vec![
            Ok(report(1, 1, vec![])),
            Ok(report(1, 3, vec![echo.clone(), banned(3, "Steve", None)])),
            Ok(report(1, 3, vec![])),
        ],
        vec![],
    );
    let hearers = Hearers::default();
    for _ in 0..3 {
        run(&only_east(), &script, &hearers, &sessions);
    }
    let names: Vec<String> = access(&sessions)
        .bans()
        .iter()
        .map(|b| b.target.to_string())
        .collect();
    assert_eq!(names, ["Steve"]);
    // Asked after the number it has each time.
    assert_eq!(script.asked(), [None, Some(1), Some(3)]);
    assert_eq!(script.reads(), 1, "the lists were read once, at the start");
}

#[test]
fn a_server_the_sidecar_connected_to_again_has_its_lists_read_again() {
    let sessions = Sessions::default();
    let second = State {
        bans: json!([{ "player": { "name": "Alex" }, "source": "Kim" }]),
        ip_bans: json!([]),
        allowlist: json!([]),
    };
    let script = Script::with(
        vec![
            Ok(report(1, 2, vec![])),
            // Quiet: the same connection, nothing new.
            Ok(report(1, 2, vec![])),
            // The server restarted and bans were made while the sidecar was not connected.
            Ok(report(2, 2, vec![])),
            Ok(report(2, 2, vec![])),
        ],
        vec![Ok(nothing()), Ok(second)],
    );
    let hearers = Hearers::default();
    run(&only_east(), &script, &hearers, &sessions);
    run(&only_east(), &script, &hearers, &sessions);
    assert_eq!(script.reads(), 1);
    run(&only_east(), &script, &hearers, &sessions);
    assert_eq!(script.reads(), 2);
    assert_eq!(access(&sessions).bans().len(), 1);
    run(&only_east(), &script, &hearers, &sessions);
    assert_eq!(script.reads(), 2, "the same connection is not read again");
}

#[test]
fn lost_notices_and_a_sidecar_that_started_over_have_the_lists_read_again() {
    let sessions = Sessions::default();
    let mut lost = report(1, 9, vec![]);
    lost.missed = true;
    let script = Script::with(
        vec![
            Ok(report(1, 5, vec![])),
            Ok(lost),
            // The count began again: the sidecar was restarted.
            Ok(report(1, 1, vec![])),
        ],
        vec![],
    );
    let hearers = Hearers::default();
    for expected in [1, 2, 3] {
        run(&only_east(), &script, &hearers, &sessions);
        assert_eq!(script.reads(), expected);
    }
}

#[test]
fn nothing_is_asked_while_from_servers_is_off() {
    let sessions = Sessions::default();
    let script = Script::with(vec![], vec![]);
    let config = config(false, true, &["east"]);
    run(&config, &script, &Hearers::default(), &sessions);
    assert_eq!(script.opened.load(Ordering::SeqCst), 0);
}

#[test]
fn a_sidecar_that_cannot_be_reached_is_tried_again_later_and_the_lists_stay_as_they_are() {
    let sessions = Sessions::default();
    let (_, _) = sessions.access.change(|a| {
        a.ban(target("Steve"), None);
        ((), true)
    });
    let script = Script::with(vec![Ok(report(1, 0, vec![]))], vec![]);
    *script.refuse_to_open.lock().unwrap() = Some("connection refused".into());
    let hearers = Hearers::default();
    run(&only_east(), &script, &hearers, &sessions);
    // Left alone for a wait: the second round does not even try.
    run(&only_east(), &script, &hearers, &sessions);
    assert_eq!(script.opened.load(Ordering::SeqCst), 1);
    assert_eq!(access(&sessions).bans().len(), 1);
}

#[test]
fn the_wait_after_a_failure_grows_to_half_a_minute() {
    let waits: Vec<_> = (1..=8).map(|n| retry_after(n).as_secs()).collect();
    assert_eq!(waits, [1, 2, 4, 8, 16, 30, 30, 30]);
}

#[test]
fn a_sidecar_that_does_not_listen_or_is_not_connected_reads_nothing() {
    let sessions = Sessions::default();
    let mut deaf = report(0, 0, vec![]);
    deaf.enabled = false;
    let script = Script::with(vec![Ok(deaf)], vec![]);
    run(&only_east(), &script, &Hearers::default(), &sessions);
    assert_eq!(script.reads(), 0);

    let mut cut = report(1, 0, vec![]);
    cut.watching = false;
    let script = Script::with(vec![Ok(cut)], vec![]);
    run(&only_east(), &script, &Hearers::default(), &sessions);
    assert_eq!(script.reads(), 0);
}

#[test]
fn a_server_whose_lists_cannot_be_read_is_read_again_later() {
    let sessions = Sessions::default();
    let script = Script::with(
        vec![Ok(report(1, 0, vec![])), Ok(report(1, 0, vec![]))],
        vec![Err(
            "bans: Method cannot be dispatched pre server initialization".into(),
        )],
    );
    let hearers = Hearers::default();
    run(&only_east(), &script, &hearers, &sessions);
    assert_eq!(script.reads(), 1);
    assert!(access(&sessions).bans().is_empty());
    // The failure keeps the connection unmarked, so the next try reads the lists again once
    // the wait is over (not at once).
    run(&only_east(), &script, &hearers, &sessions);
    assert_eq!(script.reads(), 1);
}

#[test]
fn a_backend_a_reload_removed_is_forgotten() {
    let sessions = Sessions::default();
    let script = Script::with(vec![], vec![]);
    let hearers = Hearers::default();
    run(
        &config(true, false, &["east", "west"]),
        &script,
        &hearers,
        &sessions,
    );
    assert_eq!(script.opened.load(Ordering::SeqCst), 2);
    let mut one = config(true, false, &["east"]);
    one.sidecars = vec![sidecar("east")];
    run(&one, &script, &hearers, &sessions);
    // East's connection is kept, so it is not opened again; west's is gone.
    assert_eq!(script.opened.load(Ordering::SeqCst), 2);
    run(
        &config(true, false, &["east", "west"]),
        &script,
        &hearers,
        &sessions,
    );
    assert_eq!(script.opened.load(Ordering::SeqCst), 3);
}
