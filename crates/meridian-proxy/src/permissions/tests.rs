//! The console's `perm`, the file the permissions are kept in, and what a reload does.
use super::*;
use crate::console::Commands;
use crate::session::{Open, Player, tests::idle_session_on};
use crate::state::Proxy;
use horizon_protocol::{connection::Connection, crypto::offline_uuid};
use meridian_config::{Backend, Config, Target, node};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::thread::JoinHandle;
use std::time::Instant;

fn config() -> Config {
    Config {
        backends: vec![
            Backend {
                name: "lobby".into(),
                address: "127.0.0.1:25601".into(),
            },
            Backend {
                name: "east".into(),
                address: "127.0.0.1:25602".into(),
            },
        ],
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

struct Joined<'a> {
    _open: Open<'a>,
    _relay: JoinHandle<meridian_net::Result<meridian_net::relay::Report>>,
    _player: Connection,
    _server: Connection,
}

fn join<'a>(proxy: &'a Proxy, name: &str, backend: &str) -> Joined<'a> {
    let (relay, handle, player, server) = idle_session_on(name, backend);
    let open = proxy
        .sessions
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
        _player: player,
        _server: server,
    }
}

#[test]
fn the_console_builds_groups_and_ranks_players() {
    let proxy = proxy();
    assert_eq!(
        run(&proxy, "perm list"),
        ["permissions: 0 groups, 0 players"]
    );
    assert_eq!(
        run(&proxy, "perm group staff create"),
        ["group staff created"]
    );
    assert_eq!(
        run(&proxy, "perm group staff create"),
        ["group staff exists"]
    );
    assert_eq!(
        run(&proxy, "perm group staff allow meridian.glist"),
        ["staff allows meridian.glist"]
    );
    assert_eq!(
        run(&proxy, "perm group staff allow Meridian.GList"),
        ["meridian.glist is already so in staff"]
    );
    assert_eq!(
        run(&proxy, "perm group staff deny meridian.send"),
        ["staff denies meridian.send"]
    );
    run(&proxy, "perm group admin create");
    assert_eq!(
        run(&proxy, "perm group admin inherit staff"),
        ["admin inherits staff"]
    );
    assert_eq!(
        run(&proxy, "perm group admin allow meridian.send"),
        ["admin allows meridian.send"]
    );
    assert_eq!(
        run(&proxy, "perm grant Alex admin"),
        ["Alex is now in admin"]
    );
    assert_eq!(
        run(&proxy, "perm grant alex admin"),
        ["alex is already in admin"]
    );
    assert_eq!(
        run(&proxy, "perm list"),
        [
            "permissions: 2 groups, 1 player",
            "  group admin (inherits staff): allow meridian.send; deny -",
            "  group staff: allow meridian.glist; deny meridian.send",
            "  Alex: admin",
        ]
    );
    assert_eq!(run(&proxy, "perm list alex"), ["alex is in admin"]);
    assert_eq!(
        run(&proxy, "perm list Steve"),
        ["Steve is in no group (only default)"]
    );
    // The nearest group decides: admin allows what staff denies.
    assert!(run(&proxy, "perm check Alex meridian.send")[0].starts_with("Alex has meridian.send"));
    assert!(run(&proxy, "perm check Steve meridian.glist")[0].starts_with("Steve does not have"));
    assert!(
        proxy
            .sessions
            .permissions
            .snapshot()
            .allows("Alex", [0; 16], node::GLIST)
    );
    assert_eq!(
        run(&proxy, "perm revoke Alex admin"),
        ["Alex is no longer in admin"]
    );
    assert_eq!(
        run(&proxy, "perm revoke Alex admin"),
        ["Alex is not in admin"]
    );
    assert_eq!(
        run(&proxy, "perm group admin unset meridian.send"),
        ["admin no longer says anything of meridian.send"]
    );
    assert_eq!(
        run(&proxy, "perm group admin uninherit staff"),
        ["admin no longer inherits staff"]
    );
    assert_eq!(
        run(&proxy, "perm group admin delete"),
        ["group admin deleted"]
    );
    assert_eq!(
        run(&proxy, "perm group admin delete"),
        ["there is no group called admin"]
    );
}

#[test]
fn the_console_says_what_is_wrong_with_what_it_is_given() {
    let proxy = proxy();
    run(&proxy, "perm group staff create");
    for bad in [
        "perm fly",
        "perm grant Alex",
        "perm group",
        "perm group staff",
        "perm group staff allow",
        "perm group staff fly node",
    ] {
        assert!(run(&proxy, bad)[0].starts_with("usage: perm"), "{bad}");
    }
    assert!(
        run(&proxy, "perm group ghost allow meridian.send")[0].contains("no group called ghost")
    );
    assert!(run(&proxy, "perm group staff allow bad_node!")[0].contains("not a permission node"));
    assert!(run(&proxy, "perm group bad name create")[0].starts_with("usage: perm"));
    assert!(run(&proxy, "perm group bad! create")[0].contains("not a group name"));
    assert!(run(&proxy, "perm grant Alex ghost")[0].contains("no group called ghost"));
    assert_eq!(
        run(&proxy, "perm grant 203.0.113.9 staff"),
        ["permissions are for player names and unique ids"]
    );
    assert!(run(&proxy, "perm check Alex meridian.*")[0].contains("not a pattern"));
    run(&proxy, "perm group other create");
    run(&proxy, "perm group other inherit staff");
    assert!(run(&proxy, "perm group staff inherit other")[0].contains("already inherits"));
    assert_eq!(
        run(&proxy, "perm group other inherit staff"),
        ["other already inherits staff"]
    );
    assert_eq!(
        run(&proxy, "perm group other uninherit nobody"),
        ["other does not inherit nobody"]
    );
}

#[test]
fn a_player_online_is_checked_by_name_and_by_unique_id() {
    let proxy = proxy();
    run(&proxy, "perm group staff create");
    run(&proxy, "perm group staff allow meridian.glist");
    let uuid = Target::Uuid(offline_uuid("Alex").to_be_bytes()).to_string();
    run(&proxy, &format!("perm grant {uuid} staff"));
    // Offline, only the unique id is known: a name alone does not see it.
    assert!(run(&proxy, "perm check Alex meridian.glist")[0].contains("does not have"));
    assert!(run(&proxy, "perm check Alex meridian.glist")[0].contains("by name"));
    let _alex = join(&proxy, "Alex", "lobby");
    assert!(run(&proxy, "perm check Alex meridian.glist")[0].contains("has meridian.glist"));
    assert!(!run(&proxy, "perm check Alex meridian.glist")[0].contains("by name"));
    assert!(
        run(&proxy, &format!("perm check {uuid} meridian.glist"))[0].contains("has meridian.glist")
    );
}

/// A folder with a `meridian.toml` whose permissions are in `permissions.toml`, and a proxy
/// started from it as `main` starts it.
fn started(name: &str) -> (std::path::PathBuf, Proxy) {
    let dir = std::env::temp_dir().join(format!("meridian-perm-{name}-{}", std::process::id()));
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
    let permissions = read(config.permissions.file.as_deref()).unwrap();
    let file = config.permissions.file.clone();
    let proxy = Proxy::new(config, path, Arc::new(AtomicBool::new(false)));
    proxy
        .sessions
        .permissions
        .install(permissions.unwrap(), file);
    (dir, proxy)
}

#[test]
fn a_change_is_written_and_a_restart_finds_it() {
    let (dir, proxy) = started("restart");
    run(&proxy, "perm group staff create");
    run(&proxy, "perm group staff allow meridian.*");
    run(&proxy, "perm grant Alex staff");
    let file = dir.join("permissions.toml");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(
        text.contains("[group.staff]") && text.contains("meridian.*"),
        "{text}"
    );
    assert_eq!(
        read(Some(&file)).unwrap().unwrap(),
        proxy.sessions.permissions.snapshot()
    );
    run(&proxy, "perm revoke Alex staff");
    assert!(Permissions::load(&file).unwrap().members().is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_reload_reads_the_file_again_and_so_does_the_reload_of_a_signal() {
    let (dir, proxy) = started("reload");
    assert_eq!(run(&proxy, "reload"), ["reloaded: nothing changed"]);
    let file = dir.join("permissions.toml");
    std::fs::write(
        &file,
        "[group.staff]\nallow = [\"meridian.glist\"]\n[[player]]\nname = \"Alex\"\ngroups = [\"staff\"]\n",
    )
    .unwrap();
    assert_eq!(
        run(&proxy, "reload"),
        ["reloaded:", "permissions read again: 1 group, 1 player"]
    );
    assert!(
        proxy
            .sessions
            .permissions
            .snapshot()
            .allows("Alex", [0; 16], node::GLIST)
    );
    // SIGHUP runs `reload_file`, which is what the command runs: it reads them too.
    std::fs::write(
        &file,
        "[group.staff]\nallow = [\"meridian.alert\"]\n[[player]]\nname = \"Alex\"\ngroups = [\"staff\"]\n",
    )
    .unwrap();
    let lines = crate::console::reload_file(&proxy).unwrap();
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("permissions read again")),
        "{lines:?}"
    );
    let now = proxy.sessions.permissions.snapshot();
    assert!(now.allows("Alex", [0; 16], node::ALERT));
    assert!(!now.allows("Alex", [0; 16], node::GLIST));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_file_that_cannot_be_read_at_a_reload_changes_nothing() {
    let (dir, proxy) = started("broken");
    run(&proxy, "perm group staff create");
    std::fs::write(
        dir.join("permissions.toml"),
        "[group.a]\nallow = [\"bad node\"]\n",
    )
    .unwrap();
    let lines = run(&proxy, "reload");
    assert!(lines[0].starts_with("not reloaded"), "{lines:?}");
    assert!(lines[0].contains("permissions.toml"), "{lines:?}");
    assert!(
        proxy
            .sessions
            .permissions
            .snapshot()
            .group("staff")
            .is_some()
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_change_holds_even_when_the_file_cannot_be_written() {
    let proxy = proxy();
    let nowhere = std::env::temp_dir()
        .join("meridian-perm-no-such-folder")
        .join("permissions.toml");
    proxy
        .sessions
        .permissions
        .install(Permissions::default(), Some(nowhere));
    let lines = run(&proxy, "perm group staff create");
    assert_eq!(lines[0], "group staff created");
    assert!(
        lines
            .last()
            .unwrap()
            .contains("the change holds until the proxy stops")
    );
    assert!(
        proxy
            .sessions
            .permissions
            .snapshot()
            .group("staff")
            .is_some()
    );
}

#[test]
fn a_missing_file_is_no_permissions_and_no_file_leaves_them_be() {
    assert!(read(None).unwrap().is_none());
    let missing = std::env::temp_dir()
        .join("meridian-perm-missing")
        .join("permissions.toml");
    assert_eq!(read(Some(&missing)).unwrap(), Some(Permissions::default()));
}
