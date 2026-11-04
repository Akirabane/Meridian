//! `staff`: what `/glist` shows, who an `/alert` reaches, and who a `/send` names.
use super::*;
use crate::commands::SendTarget;
use crate::session::{Open, Sessions, tests::idle_session_on};
use horizon_protocol::{connection::Connection, crypto::offline_uuid};
use meridian_config::Backend;
use std::thread::JoinHandle;
use std::time::Instant;

fn config() -> Config {
    Config {
        backends: ["lobby", "east", "west"]
            .iter()
            .enumerate()
            .map(|(i, name)| Backend {
                name: (*name).into(),
                address: format!("127.0.0.1:{}", 25601 + i),
            })
            .collect(),
        default_backend: "lobby".into(),
        online_mode: false,
        auth_mode: meridian_config::AuthMode::Offline,
        forwarding_secret: "secret".into(),
        ..Config::default()
    }
}

struct Joined<'a> {
    _open: Open<'a>,
    _relay: JoinHandle<meridian_net::Result<meridian_net::relay::Report>>,
    _ends: (Connection, Connection),
}

fn join<'a>(sessions: &'a Sessions, name: &str, backend: &str) -> Joined<'a> {
    let (relay, handle, player, server) = idle_session_on(name, backend);
    let open = sessions
        .open(
            Player {
                name: name.into(),
                uuid: offline_uuid(name).to_be_bytes(),
                backend: backend.into(),
                address: "198.51.100.7:50123".parse().unwrap(),
                joined: Instant::now(),
            },
            handle,
        )
        .unwrap();
    Joined {
        _open: open,
        _relay: std::thread::spawn(move || relay.run()),
        _ends: (player, server),
    }
}

#[test]
fn the_list_has_a_line_for_each_server_that_has_players() {
    let sessions = Sessions::default();
    let config = config();
    let shared = sessions.shared();
    assert_eq!(
        glist(&shared, &config, None).unwrap(),
        ["no players online"]
    );
    let _a = join(&sessions, "Alex", "east");
    assert_eq!(
        glist(&shared, &config, None).unwrap(),
        ["1 player online:", "east (1): Alex"]
    );
    let _b = join(&sessions, "Steve", "lobby");
    let _c = join(&sessions, "Notch", "east");
    // In the order of the configuration, the players in the order they came.
    assert_eq!(
        glist(&shared, &config, None).unwrap(),
        [
            "3 players online:",
            "lobby (1): Steve",
            "east (2): Alex, Notch"
        ]
    );
    assert_eq!(
        glist(&shared, &config, Some("east")).unwrap(),
        ["east (2): Alex, Notch"]
    );
    assert_eq!(glist(&shared, &config, Some("west")).unwrap(), ["west (0)"]);
    assert_eq!(
        glist(&shared, &config, Some("nowhere")).unwrap_err(),
        "there is no server called nowhere"
    );
}

#[test]
fn an_alert_reaches_the_players_of_one_server_or_of_all() {
    let sessions = Sessions::default();
    let config = config();
    let shared = sessions.shared();
    assert_eq!(alert(&shared, &config, None, "hi"), Ok(0));
    let _a = join(&sessions, "Alex", "east");
    let _b = join(&sessions, "Steve", "lobby");
    let _c = join(&sessions, "Notch", "east");
    assert_eq!(alert(&shared, &config, None, "hi"), Ok(3));
    assert_eq!(alert(&shared, &config, Some("east"), "hi"), Ok(2));
    assert_eq!(alert(&shared, &config, Some("lobby"), "hi"), Ok(1));
    assert_eq!(alert(&shared, &config, Some("west"), "hi"), Ok(0));
    assert_eq!(
        alert(&shared, &config, Some("nowhere"), "hi"),
        Err("there is no server called nowhere".to_owned())
    );
}

#[test]
fn a_send_names_one_player_everyone_or_the_players_of_a_server() {
    let sessions = Sessions::default();
    let config = config();
    let shared = sessions.shared();
    let _a = join(&sessions, "Alex", "east");
    let _b = join(&sessions, "Steve", "lobby");
    let _c = join(&sessions, "Notch", "east");
    let names = |who: SendTarget| -> Vec<String> {
        pick(&shared, &config, &who)
            .unwrap()
            .into_iter()
            .map(|p| p.name)
            .collect()
    };
    assert_eq!(names(SendTarget::Player("steve".into())), ["Steve"]);
    assert_eq!(names(SendTarget::All), ["Alex", "Steve", "Notch"]);
    assert_eq!(names(SendTarget::Server("east".into())), ["Alex", "Notch"]);
    assert_eq!(
        names(SendTarget::Server("west".into())),
        Vec::<String>::new()
    );
    assert_eq!(
        pick(&shared, &config, &SendTarget::Player("Herobrine".into())).unwrap_err(),
        "Herobrine is not online"
    );
    assert_eq!(
        pick(&shared, &config, &SendTarget::Server("nowhere".into())).unwrap_err(),
        "there is no server called nowhere"
    );
}

#[test]
fn a_message_for_a_player_is_a_sentence() {
    assert_eq!(
        sentence("there is no server called x"),
        "There is no server called x."
    );
    assert_eq!(
        sentence("herobrine is not online"),
        "herobrine is not online."
    );
}
