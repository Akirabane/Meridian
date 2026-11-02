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
        name: "Alex".into(),
        auth: AuthKind::Verified,
        backend: backend.into(),
    }
}

fn ask(config: &Config, command: &str) -> Option<String> {
    answer(config, &alex_on("lobby"), command)
}

#[test]
fn server_lists_the_backends_and_where_the_player_is() {
    assert_eq!(
        ask(&config(""), "server").as_deref(),
        Some("Servers: lobby (default), east, guests. You are on lobby.")
    );
}

#[test]
fn server_with_a_name_answers_as_send_does() {
    let config = config("");
    assert_eq!(
        ask(&config, "server east").as_deref(),
        Some("transfer not available yet: Alex would move to east")
    );
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
            answer(&config, &on_east, word).as_deref(),
            Some("transfer not available yet: Alex would move to lobby"),
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
    assert_eq!(
        ask(&config, "lobby").as_deref(),
        Some("transfer not available yet: Alex would move to east")
    );
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
            .contains("would move to east")
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
        name: "guest".into(),
        auth: AuthKind::Unverified,
        backend: "guests".into(),
    };
    assert_eq!(
        answer(&config, &guest, "server").as_deref(),
        Some("Servers: guests. You are on guests.")
    );
    assert_eq!(
        answer(&config, &guest, "hub").as_deref(),
        Some("There is no server called lobby.")
    );
}

#[test]
fn the_hook_answers_like_answer() {
    let config = Arc::new(config(""));
    let hook = hook(Arc::clone(&config), alex_on("lobby"));
    assert_eq!(hook("server east"), ask(&config, "server east"));
    assert_eq!(hook("tell Bob hi"), None);
}
