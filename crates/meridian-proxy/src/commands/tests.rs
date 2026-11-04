use super::*;

const BACKENDS: &str = r#"
forwarding-secret = "test-secret"
default-backend = "lobby"
[[backends]]
name = "lobby"
address = "127.0.0.1:25601"
[[backends]]
name = "east"
address = "127.0.0.1:25602"
[[backends]]
name = "guests"
address = "127.0.0.1:25603"
"#;

fn config(extra: &str) -> Config {
    Config::from_toml_str(&format!("{BACKENDS}{extra}")).unwrap()
}

fn alex_on(backend: &str) -> Typist {
    typist(AuthKind::Verified, backend, "")
}

/// Alex, on `backend`, with the permissions written in `file` (`permissions.toml`).
fn typist(auth: AuthKind, backend: &str, file: &str) -> Typist {
    Typist {
        auth,
        backend: backend.into(),
        name: "Alex".into(),
        uuid: [1; 16],
        permissions: Arc::new(Permissions::parse(file).unwrap()),
    }
}

/// The answer as one line: what the player is shown, or `move to <backend>`.
fn said(answer: Option<Answer>) -> Option<String> {
    answer.map(|answer| match answer {
        Answer::Say(line) => line,
        Answer::Move(target) => format!("move to {target}"),
        Answer::Hub => "move to the balancer's lobby".to_owned(),
        Answer::Play => "move to the balancer's game server".to_owned(),
        Answer::Send { who, server } => format!("send {who:?} to {server}"),
        Answer::Glist(server) => format!("glist {server:?}"),
        Answer::Alert { server, message } => format!("alert {server:?}: {message}"),
    })
}

fn ask(config: &Config, command: &str) -> Option<String> {
    said(answer(config, &alex_on("lobby"), command))
}

#[test]
fn server_lists_the_backends_and_where_the_player_is() {
    assert_eq!(
        ask(&config(""), "server").as_deref(),
        Some("Servers: lobby (default), east, guests. You are on lobby.")
    );
}

#[test]
fn server_with_a_name_moves_the_player() {
    let config = config("");
    assert_eq!(ask(&config, "server east").as_deref(), Some("move to east"));
    assert_eq!(
        ask(&config, "server lobby").as_deref(),
        Some("You are already on lobby.")
    );
    assert_eq!(
        ask(&config, "server nether").as_deref(),
        Some("There is no server called nether.")
    );
    assert_eq!(
        ask(&config, "server east west").as_deref(),
        Some("Usage: /server [name]")
    );
}

#[test]
fn hub_and_lobby_lead_to_the_hub_backend() {
    let on_east = alex_on("east");
    let config = config("");
    for word in ["hub", "lobby"] {
        assert_eq!(
            said(answer(&config, &on_east, word)).as_deref(),
            Some("move to lobby"),
            "{word}"
        );
    }
    assert_eq!(
        ask(&config, "hub").as_deref(),
        Some("You are already on lobby.")
    );
    assert_eq!(ask(&config, "hub now").as_deref(), Some("Usage: /hub"));
    // A hub of its own.
    let config = self::config("[commands]\nhub-backend = \"east\"");
    assert_eq!(ask(&config, "lobby").as_deref(), Some("move to east"));
}

#[test]
fn other_commands_are_the_backends() {
    let config = config("");
    for command in [
        "tell Bob hi",
        "servers",
        "hubs",
        "Server",
        "gamemode creative",
        "",
    ] {
        assert_eq!(ask(&config, command), None, "{command:?}");
    }
}

#[test]
fn a_command_turned_off_goes_to_the_backend() {
    let config = config("[commands]\nenabled = [\"hub\"]");
    assert_eq!(ask(&config, "server"), None);
    assert!(ask(&config, "hub").is_some());
    let config = self::config("[commands]\nenabled = []");
    assert_eq!(ask(&config, "hub"), None);
    assert_eq!(ask(&config, "lobby"), None);
}

#[test]
fn the_list_can_be_withheld() {
    let config = config("[commands]\nserver-list = false");
    assert_eq!(
        ask(&config, "server").as_deref(),
        Some("The list of servers is not available.")
    );
    // Moving is still answered.
    assert!(
        ask(&config, "server east")
            .unwrap()
            .contains("move to east")
    );
}

const GROUPS: &str = r#"
[[group]]
name = "accounts"
auth = "verified"
backends = ["lobby", "east"]
[[group]]
name = "open"
auth = "unverified"
backends = ["guests"]
[[route]]
auth = "verified"
group = "accounts"
[[route]]
auth = "unverified"
group = "open"
"#;

#[test]
fn backends_of_the_other_kind_are_neither_listed_nor_offered() {
    // auth-mode is a top-level key, so it goes before the tables.
    let config =
        Config::from_toml_str(&format!("auth-mode = \"hybrid\"\n{BACKENDS}{GROUPS}")).unwrap();
    assert_eq!(
        ask(&config, "server").as_deref(),
        Some("Servers: lobby (default), east. You are on lobby.")
    );
    assert_eq!(
        ask(&config, "server guests").as_deref(),
        Some("There is no server called guests.")
    );
    let guest = typist(AuthKind::Unverified, "guests", "");
    assert_eq!(
        said(answer(&config, &guest, "server")).as_deref(),
        Some("Servers: guests. You are on guests.")
    );
    assert_eq!(
        said(answer(&config, &guest, "hub")).as_deref(),
        Some("There is no server called lobby.")
    );
}

/// The hook shows what `answer` says, starts a move through `mover`, and asks
/// where the player is at each command.
#[test]
fn the_hook_answers_and_moves() {
    let config = Arc::new(config(""));
    let on = Arc::new(std::sync::Mutex::new("lobby".to_owned()));
    let where_now = Arc::clone(&on);
    let hook = hook(
        Arc::clone(&config),
        move || alex_on(&where_now.lock().unwrap()),
        |target: &str| format!("Moving you to {target}..."),
        |_: Role, _: &Typist| "Moving you to the lobby...".to_owned(),
        |_: &Typist, _: Answer| unreachable!("no staff command is typed"),
    );
    assert_eq!(
        hook("server east").as_deref(),
        Some("Moving you to east...")
    );
    assert_eq!(hook("tell Bob hi"), None);
    *on.lock().unwrap() = "east".into();
    assert_eq!(
        hook("server east").as_deref(),
        Some("You are already on east.")
    );
}

// ---- /hub with balancing ----

/// `config("")` with `lobby` and `east` as lobbies, balancing as given.
fn balanced(enabled: bool, extra: &str) -> Config {
    let mut config = config(extra);
    config.balanced = ["lobby", "east"]
        .iter()
        .map(|b| meridian_config::BalancedServer {
            backend: (*b).into(),
            role: meridian_config::Role::Lobby,
            soft_cap: 5,
            hard_cap: 5,
        })
        .collect();
    config.balancing.enabled = enabled;
    config
}

fn ask_on(config: &Config, backend: &str, command: &str) -> Option<String> {
    said(answer(config, &alex_on(backend), command))
}

/// With balancing on, no `hub-backend` written and a lobby among the servers, `/hub`
/// and `/lobby` are left to the balancer, wherever the player is.
#[test]
fn hub_is_left_to_the_balancer_when_balancing_is_on() {
    let config = balanced(true, "");
    for command in ["hub", "lobby"] {
        for from in ["lobby", "east", "guests"] {
            assert_eq!(
                ask_on(&config, from, command).as_deref(),
                Some("move to the balancer's lobby"),
                "/{command} from {from}"
            );
        }
    }
}

/// Anything else keeps the ordinary hub: balancing off, a hub written (an explicit
/// choice), or no server with the role lobby.
#[test]
fn hub_keeps_its_ordinary_answer_otherwise() {
    assert_eq!(
        ask_on(&balanced(false, ""), "east", "hub").as_deref(),
        Some("move to lobby")
    );
    assert_eq!(
        ask_on(
            &balanced(true, "[commands]\nhub-backend = \"east\""),
            "lobby",
            "hub"
        )
        .as_deref(),
        Some("move to east")
    );
    let mut no_lobby = balanced(true, "");
    for b in &mut no_lobby.balanced {
        b.role = meridian_config::Role::Game;
    }
    assert_eq!(
        ask_on(&no_lobby, "east", "hub").as_deref(),
        Some("move to lobby")
    );
    assert_eq!(
        ask_on(&balanced(true, ""), "east", "hub now").as_deref(),
        Some("Usage: /hub")
    );
}

/// `/server <name>` is always a named move: the balancer only checks the caps.
#[test]
fn server_with_a_name_stays_a_named_move_with_balancing_on() {
    assert_eq!(
        ask_on(&balanced(true, ""), "lobby", "server east").as_deref(),
        Some("move to east")
    );
}

// ---- /play with balancing ----

const PLAY_ON: &str = "[commands]\nenabled = [\"server\", \"hub\", \"play\"]";

/// `lobby` is a lobby and `east` a game server, balancing as given, `/play` turned on.
fn with_game(enabled: bool, extra: &str) -> Config {
    let mut config = config(&format!("{PLAY_ON}\n{extra}"));
    config.balanced = [("lobby", Role::Lobby), ("east", Role::Game)]
        .iter()
        .map(|(b, role)| meridian_config::BalancedServer {
            backend: (*b).into(),
            role: *role,
            soft_cap: 5,
            hard_cap: 8,
        })
        .collect();
    config.balancing.enabled = enabled;
    config
}

/// `/play` is the balancer's, wherever the player is, when balancing is on and a server
/// has the role game.
#[test]
fn play_is_left_to_the_balancer_when_balancing_is_on() {
    let config = with_game(true, "");
    for from in ["lobby", "east", "guests"] {
        assert_eq!(
            ask_on(&config, from, "play").as_deref(),
            Some("move to the balancer's game server"),
            "/play from {from}"
        );
    }
}

#[test]
fn play_says_so_without_balancing_or_without_a_game_server() {
    let none = Some("There is no game server to play on.");
    assert_eq!(
        ask_on(&with_game(false, ""), "lobby", "play").as_deref(),
        none
    );
    let mut no_game = with_game(true, "");
    no_game.balanced.retain(|b| b.role != Role::Game);
    assert_eq!(ask_on(&no_game, "lobby", "play").as_deref(), none);
    assert_eq!(
        ask_on(&with_game(true, ""), "lobby", "play now").as_deref(),
        Some("Usage: /play")
    );
}

/// `/play` is opt-in: left out of `enabled`, it is the backend's command, as before.
#[test]
fn play_goes_to_the_backend_unless_it_is_enabled() {
    let mut config = with_game(true, "");
    config.commands.enabled = vec![ProxyCommand::Server, ProxyCommand::Hub];
    assert_eq!(ask_on(&config, "lobby", "play"), None);
    assert_eq!(ask_on(&balanced(true, ""), "lobby", "play"), None);
}

// ---- permissions and the staff commands ----

const STAFF_ON: &str =
    "[commands]\nenabled = [\"server\", \"hub\", \"play\", \"send\", \"glist\", \"alert\"]";

const STAFF: &str = r#"
[group.staff]
allow = ["meridian.send", "meridian.glist", "meridian.alert"]
[[player]]
name = "Alex"
groups = ["staff"]
"#;

fn ask_as(file: &str, command: &str) -> Option<String> {
    said(answer(
        &config(STAFF_ON),
        &typist(AuthKind::Verified, "lobby", file),
        command,
    ))
}

#[test]
fn the_staff_commands_belong_to_the_backend_unless_they_are_turned_on() {
    for command in ["send all east", "glist", "alert hello"] {
        assert_eq!(said(answer(&config(""), &alex_on("lobby"), command)), None);
        // Whatever the player's permissions.
        let staff = typist(AuthKind::Verified, "lobby", STAFF);
        assert_eq!(
            said(answer(&config(""), &staff, command)),
            None,
            "{command}"
        );
    }
}

#[test]
fn a_player_without_the_node_is_told_so_and_nothing_starts() {
    for (command, word) in [
        ("send all east", "send"),
        ("glist", "glist"),
        ("glist east", "glist"),
        ("alert hello", "alert"),
    ] {
        assert_eq!(
            ask_as("", command).as_deref(),
            Some(format!("You do not have permission to use /{word}.").as_str()),
            "{command}"
        );
    }
    // A group that has some of the nodes does not have the others.
    let only_glist =
        "[group.g]\nallow = [\"meridian.glist\"]\n[[player]]\nname = \"Alex\"\ngroups = [\"g\"]\n";
    assert_eq!(ask_as(only_glist, "glist").as_deref(), Some("glist None"));
    assert_eq!(
        ask_as(only_glist, "send all east").as_deref(),
        Some("You do not have permission to use /send.")
    );
}

#[test]
fn send_names_who_and_where() {
    assert_eq!(
        ask_as(STAFF, "send Steve east").as_deref(),
        Some("send Player(\"Steve\") to east")
    );
    assert_eq!(
        ask_as(STAFF, "send ALL east").as_deref(),
        Some("send All to east")
    );
    assert_eq!(
        ask_as(STAFF, "send server:lobby east").as_deref(),
        Some("send Server(\"lobby\") to east")
    );
    for bad in ["send", "send Steve", "send Steve east now"] {
        assert_eq!(
            ask_as(STAFF, bad).as_deref(),
            Some("Usage: /send <player|all|server:name> <server>"),
            "{bad}"
        );
    }
}

#[test]
fn glist_takes_a_server_or_none() {
    assert_eq!(ask_as(STAFF, "glist").as_deref(), Some("glist None"));
    assert_eq!(
        ask_as(STAFF, "glist east").as_deref(),
        Some("glist Some(\"east\")")
    );
    assert_eq!(
        ask_as(STAFF, "glist east west").as_deref(),
        Some("Usage: /glist [server]")
    );
}

#[test]
fn alert_takes_a_message_and_a_server_first_or_last() {
    assert_eq!(
        ask_as(STAFF, "alert restart in  five minutes").as_deref(),
        Some("alert None: restart in five minutes")
    );
    assert_eq!(
        ask_as(STAFF, "alert server:east restart soon").as_deref(),
        Some("alert Some(\"east\"): restart soon")
    );
    assert_eq!(
        ask_as(STAFF, "alert restart soon server:east").as_deref(),
        Some("alert Some(\"east\"): restart soon")
    );
    // A server alone is not a message.
    for bad in ["alert", "alert server:east", "alert server:"] {
        assert_eq!(
            ask_as(STAFF, bad).as_deref(),
            Some("Usage: /alert [server:name] <message>"),
            "{bad}"
        );
    }
}

#[test]
fn the_old_commands_stay_open_until_a_group_denies_them() {
    for command in ["server", "server east", "hub", "play"] {
        let line = ask_as("", command).unwrap();
        assert!(!line.contains("permission"), "{command}: {line}");
    }
    let closed =
        "[group.default]\ndeny = [\"meridian.server\", \"meridian.hub\", \"meridian.play\"]\n";
    for (command, word) in [
        ("server", "server"),
        ("hub", "hub"),
        ("lobby", "lobby"),
        ("play", "play"),
    ] {
        assert_eq!(
            ask_as(closed, command).as_deref(),
            Some(format!("You do not have permission to use /{word}.").as_str()),
            "{command}"
        );
    }
    // A rank that allows them again, for those in it.
    let staff_only = "[group.default]\ndeny = [\"meridian.hub\"]\n\
                      [group.staff]\nallow = [\"meridian.hub\"]\n\
                      [[player]]\nname = \"Alex\"\ngroups = [\"staff\"]\n";
    // Allowed: Alex is on the lobby already, which is the answer of an allowed /hub.
    assert_eq!(
        ask_as(staff_only, "hub").as_deref(),
        Some("You are already on lobby.")
    );
}

#[test]
fn a_server_can_be_closed_to_a_player_by_its_node() {
    let file = "[group.default]\ndeny = [\"meridian.server.east\"]\n";
    assert_eq!(
        ask_as(file, "server").as_deref(),
        Some("Servers: lobby (default), guests. You are on lobby."),
        "not listed"
    );
    assert_eq!(
        ask_as(file, "server east").as_deref(),
        Some("You do not have permission to go to east.")
    );
    assert_eq!(
        ask_as(file, "server guests").as_deref(),
        Some("move to guests")
    );
    // A server that does not exist is still said not to.
    assert_eq!(
        ask_as(file, "server nowhere").as_deref(),
        Some("There is no server called nowhere.")
    );
    // Allowed again by a rank, with a pattern.
    let ranked = "[group.default]\ndeny = [\"meridian.server.*\"]\n\
                  [group.vip]\nallow = [\"meridian.server.east\"]\n\
                  [[player]]\nname = \"Alex\"\ngroups = [\"vip\"]\n";
    assert_eq!(
        ask_as(ranked, "server").as_deref(),
        Some("Servers: east. You are on lobby.")
    );
}

#[test]
fn what_the_hook_cannot_decide_goes_to_the_staff_closure() {
    let config = Arc::new(config(STAFF_ON));
    let staff = Arc::new(Permissions::parse(STAFF).unwrap());
    let hook = hook(
        Arc::clone(&config),
        {
            let staff = Arc::clone(&staff);
            move || Typist {
                permissions: Arc::clone(&staff),
                ..alex_on("lobby")
            }
        },
        |target: &str| format!("Moving you to {target}..."),
        |_: Role, _: &Typist| "Moving you to the lobby...".to_owned(),
        |typist: &Typist, answer: Answer| format!("{} did {answer:?}", typist.name),
    );
    assert_eq!(hook("glist").as_deref(), Some("Alex did Glist(None)"));
    // Without the node the staff closure is never reached.
    let hook_no = super::hook(
        config,
        || alex_on("lobby"),
        |_: &str| String::new(),
        |_: Role, _: &Typist| String::new(),
        |_: &Typist, _: Answer| unreachable!("not allowed"),
    );
    assert_eq!(
        hook_no("glist").as_deref(),
        Some("You do not have permission to use /glist.")
    );
}
