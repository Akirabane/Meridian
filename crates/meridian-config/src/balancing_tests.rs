//! The roles of the backends, their limits and the `[balancing]` table.
use super::*;

fn with(top: &str, backends: &str, tables: &str) -> Result<Config, ConfigError> {
    Config::from_toml_str(&format!(
        r#"{top}
forwarding-secret = "test-secret"
{backends}
{tables}"#
    ))
}

const LOBBY_AND_GAMES: &str = r#"
[[backends]]
name = "lobby"
address = "127.0.0.1:25566"
role = "lobby"
soft-cap = 40
[[backends]]
name = "queue"
address = "127.0.0.1:25567"
role = "sublobby"
soft-cap = 20
hard-cap = 25
[[backends]]
name = "game-1"
address = "127.0.0.1:25568"
role = "game"
soft-cap = 12
[[backends]]
name = "game-2"
address = "127.0.0.1:25569"
role = "game"
soft-cap = 12
hard-cap = 16
[[backends]]
name = "spare"
address = "127.0.0.1:25570"
"#;

#[test]
fn nothing_is_balanced_by_default() {
    let config = with(
        "",
        "[[backends]]\nname = \"a\"\naddress = \"127.0.0.1:25566\"",
        "",
    )
    .unwrap();
    assert!(config.balanced.is_empty());
    assert_eq!(config.balancing, Balancing::default());
    assert!(!config.balancing.enabled);
    assert_eq!(config.balancing.margin, 2);
    assert_eq!(config.balancing.state_max_age_seconds, 15);
    assert_eq!(config.balancing.tie_break, TieBreak::Ping);
    assert_eq!(config.balancing.probe_interval_seconds, 10);
    assert!(config.balanced("a").is_none());
}

#[test]
fn roles_and_limits_are_read_and_the_hard_cap_defaults_to_the_soft_cap() {
    let config = with("", LOBBY_AND_GAMES, "").unwrap();
    assert_eq!(config.balanced.len(), 4);
    let lobby = config.balanced("lobby").unwrap();
    assert_eq!(
        (lobby.role, lobby.soft_cap, lobby.hard_cap),
        (Role::Lobby, 40, 40)
    );
    let queue = config.balanced("queue").unwrap();
    assert_eq!(
        (queue.role, queue.soft_cap, queue.hard_cap),
        (Role::Sublobby, 20, 25)
    );
    let game = config.balanced("game-2").unwrap();
    assert_eq!(
        (game.role, game.soft_cap, game.hard_cap),
        (Role::Game, 12, 16)
    );
    // A backend with no role is not balanced.
    assert!(config.balanced("spare").is_none());
}

#[test]
fn servers_of_a_role_come_in_the_order_the_backends_are_listed() {
    let config = with("", LOBBY_AND_GAMES, "").unwrap();
    let games: Vec<&str> = config
        .servers_with_role(Role::Game)
        .map(|s| s.backend.as_str())
        .collect();
    assert_eq!(games, ["game-1", "game-2"]);
    assert_eq!(config.servers_with_role(Role::Lobby).count(), 1);
    assert_eq!(Role::Sublobby.name(), "sublobby");
}

#[test]
fn the_balancing_table_is_read() {
    let config = with(
        "",
        LOBBY_AND_GAMES,
        "[balancing]\nmargin = 4\nstate-max-age-seconds = 30\ntie-break = \"order\"\nprobe-interval-seconds = 5",
    )
    .unwrap();
    assert_eq!(
        config.balancing,
        Balancing {
            enabled: false,
            margin: 4,
            state_max_age_seconds: 30,
            tie_break: TieBreak::Order,
            probe_interval_seconds: 5,
        }
    );
    assert_eq!(config.balancing.tie_break.name(), "order");
}

#[test]
fn a_margin_of_zero_turns_the_hysteresis_off() {
    let config = with("", LOBBY_AND_GAMES, "[balancing]\nmargin = 0").unwrap();
    assert_eq!(config.balancing.margin, 0);
}

#[test]
fn a_limit_without_a_role_is_refused() {
    let one = |extra: &str| {
        with(
            "",
            &format!("[[backends]]\nname = \"a\"\naddress = \"127.0.0.1:25566\"\n{extra}"),
            "",
        )
        .unwrap_err()
    };
    let e = one("soft-cap = 5");
    assert!(
        matches!(&e, ConfigError::LimitWithoutRole { backend, key } if backend == "a" && *key == "soft-cap"),
        "{e}"
    );
    let e = one("hard-cap = 5");
    assert!(
        matches!(&e, ConfigError::LimitWithoutRole { key, .. } if *key == "hard-cap"),
        "{e}"
    );
}

#[test]
fn a_role_without_a_soft_cap_is_refused() {
    let e = with(
        "",
        "[[backends]]\nname = \"a\"\naddress = \"127.0.0.1:25566\"\nrole = \"lobby\"",
        "",
    )
    .unwrap_err();
    assert!(
        matches!(&e, ConfigError::RoleWithoutSoftCap(b) if b == "a"),
        "{e}"
    );
}

fn lobby_with(extra: &str) -> Result<Config, ConfigError> {
    with(
        "",
        &format!(
            "[[backends]]\nname = \"a\"\naddress = \"127.0.0.1:25566\"\nrole = \"lobby\"\n{extra}"
        ),
        "",
    )
}

#[test]
fn the_caps_are_checked() {
    let e = lobby_with("soft-cap = 0").unwrap_err();
    assert!(matches!(&e, ConfigError::ZeroSoftCap(b) if b == "a"), "{e}");
    let e = lobby_with("soft-cap = 10\nhard-cap = 9").unwrap_err();
    assert!(
        matches!(
            &e,
            ConfigError::HardCapBelowSoftCap {
                soft_cap: 10,
                hard_cap: 9,
                ..
            }
        ),
        "{e}"
    );
    assert!(lobby_with("soft-cap = 10\nhard-cap = 10").is_ok());
}

#[test]
fn balancing_needs_a_lobby() {
    let e = with(
        "",
        "[[backends]]\nname = \"g\"\naddress = \"127.0.0.1:25566\"\nrole = \"game\"\nsoft-cap = 8",
        "",
    )
    .unwrap_err();
    assert!(matches!(e, ConfigError::NoLobby), "{e}");
}

#[test]
fn bad_balancing_settings_are_refused() {
    let e = with(
        "",
        LOBBY_AND_GAMES,
        "[balancing]\nstate-max-age-seconds = 0",
    )
    .unwrap_err();
    assert!(matches!(e, ConfigError::ZeroStateMaxAge), "{e}");
    let e = with(
        "",
        LOBBY_AND_GAMES,
        "[balancing]\nprobe-interval-seconds = 0",
    )
    .unwrap_err();
    assert!(matches!(e, ConfigError::ZeroProbeInterval), "{e}");
    for bad in [
        "[balancing]\ntie-break = \"random\"",
        "[balancing]\nmargin = -1",
        "[balancing]\nsoft_cap = 3",
        "[balancing]\nmargin = \"two\"",
    ] {
        assert!(
            matches!(with("", LOBBY_AND_GAMES, bad), Err(ConfigError::Parse(_))),
            "{bad}"
        );
    }
    // An unknown role, and the Rust spelling of a key, are refused with their name.
    let e = with(
        "",
        "[[backends]]\nname = \"a\"\naddress = \"127.0.0.1:25566\"\nrole = \"arena\"\nsoft-cap = 5",
        "",
    )
    .unwrap_err();
    assert!(matches!(e, ConfigError::Parse(_)), "{e}");
    let e = with(
        "",
        "[[backends]]\nname = \"a\"\naddress = \"127.0.0.1:25566\"\nrole = \"lobby\"\nsoft_cap = 5",
        "",
    )
    .unwrap_err();
    assert!(matches!(e, ConfigError::Parse(_)), "{e}");
}

#[test]
fn roles_do_not_change_how_a_player_is_routed() {
    // Step 1 reads the roles and nothing else uses them: the routing settings are
    // checked and read as before.
    let config = with("default-backend = \"game-1\"", LOBBY_AND_GAMES, "").unwrap();
    assert_eq!(config.default_backend, "game-1");
    assert!(config.groups.is_empty() && config.routes.is_empty());
}

#[test]
fn balancing_is_turned_on_with_enabled_and_needs_roles() {
    let config = with(
        "",
        LOBBY_AND_GAMES,
        "[balancing]
enabled = true",
    )
    .unwrap();
    assert!(config.balancing.enabled);
    let one = "[[backends]]
name = \"a\"
address = \"127.0.0.1:25566\"";
    let e = with(
        "",
        one,
        "[balancing]
enabled = true",
    )
    .unwrap_err();
    assert!(matches!(e, ConfigError::BalancingWithoutRoles), "{e}");
    // Off, with or without roles, is always valid.
    assert!(
        with(
            "",
            one,
            "[balancing]
enabled = false"
        )
        .is_ok()
    );
    assert!(matches!(
        with(
            "",
            LOBBY_AND_GAMES,
            "[balancing]
enabled = \"yes\""
        ),
        Err(ConfigError::Parse(_))
    ));
}

/// Balancing on, a balanced lobby with a sidecar, and the defaults otherwise; `change` sets the
/// rest. Built in place: loading a file would read the sidecar's secret file.
fn read_through_a_sidecar(change: impl FnOnce(&mut Config)) -> Config {
    let mut config = Config {
        sidecars: vec![Sidecar {
            backend: "lobby".into(),
            address: "127.0.0.1:25580".into(),
            secret_file: "lobby.key".into(),
            notices: false,
        }],
        balanced: vec![BalancedServer {
            backend: "lobby".into(),
            role: Role::Lobby,
            soft_cap: 10,
            hard_cap: 10,
        }],
        ..Config::default()
    };
    config.balancing.enabled = true;
    change(&mut config);
    config
}

/// The age a report is believed for must outlast the longest gap between two readings of a
/// server that answers while another holds the rounds; shorter, the file is used and warned of.
#[test]
fn a_report_age_shorter_than_the_gap_between_two_readings_is_warned_of() {
    // The defaults: rounds of 10 s, a silent server holds one for 5 s and 5 s; a server that
    // answers is read at most 10 + 5 s apart, which is the 15 s a report is believed.
    assert!(
        read_through_a_sidecar(|_| {})
            .balancing_warnings()
            .is_empty()
    );
    // The bench of the validation: rounds of 3 s, held 10 s, read at most 15 s apart.
    let bench = read_through_a_sidecar(|c| c.balancing.probe_interval_seconds = 3);
    assert!(bench.balancing_warnings().is_empty());
    // A shorter age is warned of, with what to raise and to what.
    let short = read_through_a_sidecar(|c| c.balancing.state_max_age_seconds = 12);
    let warnings = short.balancing_warnings();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0].starts_with("state-max-age-seconds is 12 s, but up to 15000 ms can pass"),
        "{}",
        warnings[0]
    );
    assert!(
        warnings[0].contains("Raise state-max-age-seconds to 15 or more"),
        "{}",
        warnings[0]
    );
    assert!(
        warnings[0].contains("sidecar-timeout-ms or connect-timeout-ms"),
        "{}",
        warnings[0]
    );
    // Longer timeouts make the gap longer: 10 s, held 8 + 6 s, plus 8 s is 22 s.
    let slower = |c: &mut Config| {
        c.sidecar_timeout_ms = 8_000;
        c.connect_timeout_ms = 6_000;
    };
    let slow = read_through_a_sidecar(slower);
    let warnings = slow.balancing_warnings();
    assert!(
        warnings[0].contains("Raise state-max-age-seconds to 22 or more"),
        "{warnings:?}"
    );
    // Without the measures of "ping", only the readings hold a round: 10 + 8 s.
    let order = read_through_a_sidecar(|c| {
        slower(c);
        c.balancing.tie_break = TieBreak::Order;
    });
    let warnings = order.balancing_warnings();
    assert!(
        warnings[0].contains("Raise state-max-age-seconds to 18 or more"),
        "{warnings:?}"
    );
    assert!(!warnings[0].contains("connect-timeout-ms"), "{warnings:?}");
    // Nothing is read with balancing off, or with no sidecar: no report grows old.
    let mut off = short.clone();
    off.balancing.enabled = false;
    assert!(off.balancing_warnings().is_empty());
    let mut no_sidecar = short;
    no_sidecar.sidecars.clear();
    assert!(no_sidecar.balancing_warnings().is_empty());
}
