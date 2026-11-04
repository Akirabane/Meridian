//! The console's `ban`, `pardon`, `banlist` and `whitelist`, the sessions a ban ends, and
//! the file the lists are kept in.
use super::*;
use crate::console::Commands;
use crate::session::{Open, Player, tests::idle_session_on};
use crate::state::Proxy;
use horizon_protocol::{
    connection::Connection, crypto::offline_uuid, packets::configuration, text,
};
use meridian_config::{Backend, Config, Target};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::thread::JoinHandle;
use std::time::Instant;

fn config() -> Config {
    Config {
        backends: vec![Backend {
            name: "lobby".into(),
            address: "127.0.0.1:25601".into(),
        }],
        default_backend: "lobby".into(),
        online_mode: false,
        auth_mode: meridian_config::AuthMode::Offline,
        forwarding_secret: "secret".into(),
        ..Config::default()
    }
}

fn proxy() -> Proxy {
    Proxy::new(
        config(),
        "does-not-exist/meridian.toml".into(),
        Arc::new(AtomicBool::new(false)),
    )
}

fn run(proxy: &Proxy, line: &str) -> Vec<String> {
    Commands::builtin().execute(proxy, line)
}

/// A player relayed between two idle pipes, counted in the proxy's sessions.
struct Joined<'a> {
    _open: Open<'a>,
    relay: JoinHandle<meridian_net::Result<meridian_net::relay::Report>>,
    player: Connection,
    _server: Connection,
}

fn join<'a>(proxy: &'a Proxy, name: &str, address: &str) -> Joined<'a> {
    let (relay, handle, player, server) = idle_session_on(name, "lobby");
    let open = proxy
        .sessions
        .open(
            Player {
                name: name.into(),
                uuid: offline_uuid(name).to_be_bytes(),
                backend: "lobby".into(),
                address: format!("{address}:50123").parse().unwrap(),
                joined: Instant::now(),
            },
            handle,
        )
        .unwrap();
    Joined {
        _open: open,
        relay: std::thread::spawn(move || relay.run()),
        player,
        _server: server,
    }
}

/// What the player was told when their session ended, and that it did end.
fn told(mut joined: Joined<'_>) -> String {
    let (id, payload) = joined.player.recv().unwrap();
    let configuration::Clientbound::Disconnect(d) =
        configuration::Clientbound::decode(id, &payload).unwrap()
    else {
        panic!("not a disconnect");
    };
    assert!(joined.relay.join().unwrap().is_ok());
    text::plain(&d.reason, &Default::default())
}

#[test]
fn a_ban_ends_the_session_of_the_player_it_holds_and_only_theirs() {
    let proxy = proxy();
    let alex = join(&proxy, "Alex", "198.51.100.7");
    let _steve = join(&proxy, "Steve", "198.51.100.8");
    assert_eq!(
        run(&proxy, "ban alex griefing the spawn"),
        [
            "banned alex: griefing the spawn",
            "kicked Alex from lobby: banned"
        ]
    );
    assert_eq!(
        told(alex),
        "You are banned from this server.\nReason: griefing the spawn"
    );
    assert!(proxy.sessions.find("Steve").is_some());
    assert_eq!(
        run(&proxy, "banlist"),
        ["1 ban:", "  name  alex: griefing the spawn"]
    );
}

#[test]
fn a_ban_by_unique_id_or_range_ends_every_session_it_holds() {
    let proxy = proxy();
    let alex = join(&proxy, "Alex", "198.51.100.7");
    let steve = join(&proxy, "Steve", "198.51.100.8");
    let notch = join(&proxy, "Notch", "203.0.113.9");
    let uuid = meridian_config::Target::Uuid(offline_uuid("Notch").to_be_bytes()).to_string();
    assert_eq!(run(&proxy, &format!("ban {uuid}")).len(), 2);
    assert_eq!(told(notch), "You are banned from this server.");
    let lines = run(&proxy, "ban 198.51.100.0/24 bad network");
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert_eq!(
        told(alex),
        "You are banned from this server.\nReason: bad network"
    );
    told(steve);
    assert_eq!(proxy.sessions.players().len(), 0);
}

#[test]
fn the_console_says_what_is_wrong_with_what_it_is_given() {
    let proxy = proxy();
    assert_eq!(run(&proxy, "ban"), ["usage: ban <name|uuid|ip> [reason]"]);
    assert!(run(&proxy, "ban bad-name")[0].contains("not a player name"));
    assert!(run(&proxy, "ban 203.0.113.0/40")[0].contains("not a length"));
    assert_eq!(run(&proxy, "pardon"), ["usage: pardon <name|uuid|ip>"]);
    assert_eq!(run(&proxy, "pardon Alex"), ["Alex is not banned"]);
    assert_eq!(run(&proxy, "banlist"), ["no bans"]);
    assert_eq!(run(&proxy, "ban Alex"), ["banned Alex"]);
    assert_eq!(run(&proxy, "ban ALEX again"), ["ALEX is already banned"]);
    assert_eq!(run(&proxy, "ban 203.0.113.9/24"), ["banned 203.0.113.0/24"]);
    assert_eq!(
        run(&proxy, "banlist"),
        ["2 bans:", "  name  Alex", "  ip    203.0.113.0/24"]
    );
    assert_eq!(run(&proxy, "pardon alex"), ["pardoned alex"]);
    assert_eq!(
        run(&proxy, "pardon 203.0.113.0/24"),
        ["pardoned 203.0.113.0/24"]
    );
    assert_eq!(run(&proxy, "banlist"), ["no bans"]);
}

#[test]
fn the_whitelist_is_managed_from_the_console() {
    let proxy = proxy();
    assert_eq!(run(&proxy, "whitelist"), ["whitelist off, 0 listed"]);
    assert_eq!(
        run(&proxy, "whitelist on"),
        [
            "whitelist on",
            "the whitelist is empty: no one can join until players are added",
            "players already connected stay; kick ends a session by hand"
        ]
    );
    assert_eq!(
        run(&proxy, "whitelist on"),
        [
            "whitelist on already",
            "the whitelist is empty: no one can join until players are added"
        ]
    );
    assert_eq!(
        run(&proxy, "whitelist add Alex"),
        ["Alex added to the whitelist"]
    );
    assert_eq!(
        run(&proxy, "whitelist add alex"),
        ["alex is already on the whitelist"]
    );
    assert!(run(&proxy, "whitelist add 203.0.113.9")[0].contains("not addresses"));
    assert_eq!(
        run(&proxy, "whitelist list"),
        ["whitelist on, 1 listed:", "  name  Alex"]
    );
    assert_eq!(
        run(&proxy, "whitelist remove nobody"),
        ["nobody is not on the whitelist"]
    );
    assert_eq!(
        run(&proxy, "whitelist remove ALEX"),
        ["ALEX removed from the whitelist"]
    );
    assert_eq!(run(&proxy, "whitelist off"), ["whitelist off"]);
    assert!(run(&proxy, "whitelist fly")[0].starts_with("usage: whitelist"));
    // A player connected when it was turned on stays.
    let _alex = join(&proxy, "Alex", "198.51.100.7");
    run(&proxy, "whitelist on");
    assert!(proxy.sessions.find("Alex").is_some());
}

/// A folder with a `meridian.toml` that has the bans and whitelist in `access.toml`,
/// and a proxy started from it as `main` starts it.
fn started(name: &str) -> (std::path::PathBuf, Proxy) {
    let dir = std::env::temp_dir().join(format!("meridian-bans-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("meridian.toml");
    std::fs::write(
        &path,
        "forwarding-secret = \"secret\"\nonline-mode = false\n[[backends]]\nname = \"lobby\"\n\
         address = \"127.0.0.1:25601\"\n",
    )
    .unwrap();
    let config = Config::load(&path).unwrap();
    let access = read(config.access.file.as_deref()).unwrap();
    let file = config.access.file.clone();
    let proxy = Proxy::new(config, path, Arc::new(AtomicBool::new(false)));
    proxy.sessions.access.install(access.unwrap(), file);
    (dir, proxy)
}

#[test]
fn a_change_is_written_and_a_restart_finds_it() {
    let (dir, proxy) = started("restart");
    run(&proxy, "ban Alex griefing");
    run(&proxy, "ban 203.0.113.0/24");
    run(&proxy, "whitelist add Steve");
    run(&proxy, "whitelist on");
    let file = dir.join("access.toml");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("whitelist = true"), "{text}");
    assert!(text.contains("name = \"Alex\""), "{text}");
    // A new start reads the same lists.
    assert_eq!(
        read(Some(&file)).unwrap().unwrap(),
        proxy.sessions.access.snapshot()
    );
    // Lifting a ban is written too.
    run(&proxy, "pardon alex");
    let after = Access::load(&file).unwrap();
    assert_eq!(after.bans().len(), 1);
    assert_eq!(after.allowed(), [Target::Name("Steve".into())]);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_reload_reads_the_file_again_and_ends_the_sessions_it_bans() {
    let (dir, proxy) = started("reload");
    let alex = join(&proxy, "Alex", "198.51.100.7");
    let _steve = join(&proxy, "Steve", "198.51.100.8");
    assert_eq!(run(&proxy, "reload"), ["reloaded: nothing changed"]);
    std::fs::write(
        dir.join("access.toml"),
        "whitelist = true\n[[ban]]\nname = \"alex\"\nreason = \"by hand\"\n\
         [[allow]]\nname = \"Steve\"\n",
    )
    .unwrap();
    assert_eq!(
        run(&proxy, "reload"),
        [
            "reloaded:",
            "bans and whitelist read again: 1 ban, whitelist on, 1 listed",
            "kicked Alex from lobby: banned"
        ]
    );
    assert_eq!(
        told(alex),
        "You are banned from this server.\nReason: by hand"
    );
    assert!(
        proxy.sessions.find("Steve").is_some(),
        "the whitelist keeps no one out"
    );
    assert!(
        proxy
            .sessions
            .access
            .refusal("Alex", [0; 16], "198.51.100.1".parse().unwrap())
            .is_some()
    );
    // A second reload with nothing changed says so.
    assert_eq!(run(&proxy, "reload"), ["reloaded: nothing changed"]);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_file_that_cannot_be_read_at_a_reload_changes_nothing() {
    let (dir, proxy) = started("broken");
    run(&proxy, "ban Alex");
    std::fs::write(dir.join("access.toml"), "[[ban]]\nname = \"not a name\"\n").unwrap();
    let lines = run(&proxy, "reload");
    assert!(lines[0].starts_with("not reloaded"), "{lines:?}");
    assert!(lines[0].contains("access.toml"), "{lines:?}");
    assert_eq!(
        proxy.sessions.access.snapshot().bans().len(),
        1,
        "Alex is still banned"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_ban_holds_even_when_the_file_cannot_be_written() {
    let proxy = proxy();
    let nowhere = std::env::temp_dir()
        .join("meridian-bans-no-such-folder")
        .join("access.toml");
    proxy
        .sessions
        .access
        .install(Access::default(), Some(nowhere));
    let _alex = join(&proxy, "Alex", "198.51.100.7");
    let lines = run(&proxy, "ban Alex");
    assert_eq!(lines[0], "banned Alex");
    assert!(
        lines
            .last()
            .unwrap()
            .contains("the change holds until the proxy stops"),
        "{lines:?}"
    );
    assert_eq!(proxy.sessions.access.snapshot().bans().len(), 1);
}

#[test]
fn a_missing_file_is_empty_lists_and_no_file_leaves_the_lists_be() {
    assert!(read(None).unwrap().is_none());
    let missing = std::env::temp_dir()
        .join("meridian-bans-missing")
        .join("access.toml");
    assert_eq!(read(Some(&missing)).unwrap(), Some(Access::default()));
}

#[test]
fn the_summary_counts_the_bans_and_the_whitelist() {
    let mut access = Access::default();
    assert_eq!(summary(&access), "0 bans, whitelist off");
    access.ban(Target::parse("Alex").unwrap(), None);
    access.set_whitelist(true);
    assert_eq!(summary(&access), "1 ban, whitelist on, 0 listed");
}
