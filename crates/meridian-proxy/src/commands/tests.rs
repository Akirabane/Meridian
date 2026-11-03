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
    Typist {
        auth: AuthKind::Verified,
        backend: backend.into(),
    }
}

/// The answer as one line: what the player is shown, or `move to <backend>`.
fn said(answer: Option<Answer>) -> Option<String> {
    answer.map(|answer| match answer {
        Answer::Say(line) => line,
        Answer::Move(target) => format!("move to {target}"),
        Answer::Hub => "move to the balancer's lobby".to_owned(),
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
    let guest = Typist {
        auth: AuthKind::Unverified,
        backend: "guests".into(),
    };
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
        |_: &Typist| "Moving you to the lobby...".to_owned(),
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
