//! Sidecars in `[[backends]]` and their secret files.
use super::*;
use std::path::PathBuf;

const SECRET: &str = "0123456789abcdef0123456789abcdef";

/// A folder of the test's own with secret files `a.key` (long enough) and
/// `short.key` (too short), and a `secrets` subfolder holding `b.key`.
fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("meridian-config-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("secrets")).unwrap();
    std::fs::write(dir.join("a.key"), format!("{SECRET}\n")).unwrap();
    std::fs::write(dir.join("secrets").join("b.key"), SECRET).unwrap();
    std::fs::write(dir.join("short.key"), "too short\r\n").unwrap();
    dir
}

/// Two backends; `lobby` and `east` get `lobby_extra` and `east_extra` in their tables.
fn text(top: &str, lobby_extra: &str, east_extra: &str) -> String {
    format!(
        r#"forwarding-secret = "test-secret"
{top}
[[backends]]
name = "lobby"
address = "127.0.0.1:25601"
{lobby_extra}
[[backends]]
name = "east"
address = "127.0.0.1:25602"
{east_extra}
"#
    )
}

fn load(dir: &Path, top: &str, lobby: &str, east: &str) -> Result<Config, ConfigError> {
    Config::from_toml_str_in(&text(top, lobby, east), dir)
}

#[test]
fn backends_have_no_sidecar_by_default() {
    let config = Config::from_toml_str(&text("", "", "")).unwrap();
    assert!(config.sidecars.is_empty());
    assert_eq!(config.sidecar_timeout_ms, 5000);
}

#[test]
fn a_sidecar_is_read_with_its_secret_file_from_the_configurations_folder() {
    let dir = folder("read");
    let config = load(
        &dir,
        "sidecar-timeout-ms = 2500",
        "sidecar = \"127.0.0.1:25580\"\nsidecar-secret-file = \"a.key\"",
        "sidecar = \"127.0.0.1:25581\"\nsidecar-secret-file = \"secrets/b.key\"",
    )
    .unwrap();
    assert_eq!(config.sidecar_timeout_ms, 2500);
    assert_eq!(
        config.sidecars,
        [
            Sidecar {
                backend: "lobby".into(),
                address: "127.0.0.1:25580".into(),
                secret_file: dir.join("a.key"),
                notices: false,
            },
            Sidecar {
                backend: "east".into(),
                address: "127.0.0.1:25581".into(),
                secret_file: dir.join("secrets/b.key"),
                notices: false,
            },
        ]
    );
    // The secret is read as the sidecar reads it: without the final line break.
    assert_eq!(config.sidecars[0].read_secret().unwrap(), SECRET.as_bytes());
    assert_eq!(config.sidecars[1].read_secret().unwrap(), SECRET.as_bytes());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_file_loaded_from_disk_takes_its_paths_from_its_own_folder() {
    let dir = folder("disk");
    let path = dir.join("meridian.toml");
    std::fs::write(
        &path,
        text(
            "",
            "sidecar = \"127.0.0.1:25580\"\nsidecar-secret-file = \"a.key\"",
            "",
        ),
    )
    .unwrap();
    let config = Config::load(&path).unwrap();
    assert_eq!(config.sidecars[0].secret_file, dir.join("a.key"));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn debug_output_shows_the_path_and_never_the_secret() {
    let dir = folder("debug");
    let config = load(
        &dir,
        "",
        "sidecar = \"127.0.0.1:25580\"\nsidecar-secret-file = \"a.key\"",
        "",
    )
    .unwrap();
    let shown = format!("{config:?}");
    assert!(shown.contains("a.key"), "{shown}");
    assert!(!shown.contains(SECRET), "{shown}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_sidecar_needs_both_its_address_and_its_secret_file() {
    let dir = folder("pair");
    let e = load(&dir, "", "sidecar = \"127.0.0.1:25580\"", "").unwrap_err();
    assert!(
        matches!(&e, ConfigError::SidecarWithoutSecret(b) if b == "lobby"),
        "{e}"
    );
    assert!(e.to_string().contains("never in the configuration"), "{e}");
    let e = load(&dir, "", "sidecar-secret-file = \"a.key\"", "").unwrap_err();
    assert!(
        matches!(&e, ConfigError::SecretWithoutSidecar(b) if b == "lobby"),
        "{e}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_missing_or_short_secret_file_is_refused_without_showing_it() {
    let dir = folder("bad-secret");
    let e = load(
        &dir,
        "",
        "sidecar = \"127.0.0.1:25580\"\nsidecar-secret-file = \"nowhere.key\"",
        "",
    )
    .unwrap_err();
    assert!(
        matches!(&e, ConfigError::SidecarSecret { backend, .. } if backend == "lobby"),
        "{e}"
    );
    assert!(e.to_string().contains("nowhere.key"), "{e}");

    let e = load(
        &dir,
        "",
        "sidecar = \"127.0.0.1:25580\"\nsidecar-secret-file = \"short.key\"",
        "",
    )
    .unwrap_err();
    let message = e.to_string();
    assert!(
        message.contains("holds 9 bytes") && message.contains("at least 16"),
        "{message}"
    );
    assert!(
        !message.contains("too short"),
        "the message shows the content: {message}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn two_backends_may_not_share_a_sidecar_address_or_a_secret_file() {
    let dir = folder("shared");
    let e = load(
        &dir,
        "",
        "sidecar = \"127.0.0.1:25580\"\nsidecar-secret-file = \"a.key\"",
        "sidecar = \"127.0.0.1:25580\"\nsidecar-secret-file = \"secrets/b.key\"",
    )
    .unwrap_err();
    assert!(matches!(e, ConfigError::SharedSidecarAddress { .. }), "{e}");

    // The same file, spelled another way.
    let e = load(
        &dir,
        "",
        "sidecar = \"127.0.0.1:25580\"\nsidecar-secret-file = \"secrets/b.key\"",
        "sidecar = \"127.0.0.1:25581\"\nsidecar-secret-file = \"secrets/../secrets/b.key\"",
    )
    .unwrap_err();
    assert!(
        matches!(&e, ConfigError::SharedSidecarSecret { first, second, .. } if first == "lobby" && second == "east"),
        "{e}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn bad_sidecar_settings_are_refused() {
    let dir = folder("bad");
    let e = load(
        &dir,
        "",
        "sidecar = \"127.0.0.1\"\nsidecar-secret-file = \"a.key\"",
        "",
    )
    .unwrap_err();
    assert!(
        matches!(e, ConfigError::InvalidSidecarAddress { .. }),
        "{e}"
    );
    let e = load(&dir, "sidecar-timeout-ms = 0", "", "").unwrap_err();
    assert!(matches!(e, ConfigError::ZeroSidecarTimeout), "{e}");
    let e = load(&dir, "", "sidecar-secret = \"inline\"", "").unwrap_err();
    assert!(
        matches!(e, ConfigError::Parse(_)),
        "a secret is never written in the file: {e}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_switch_carries_the_position_only_when_asked() {
    let config = Config::from_toml_str(&text("", "", "")).unwrap();
    assert!(!config.switch_carry_position);
    let config = Config::from_toml_str(&text("switch-carry-position = true", "", "")).unwrap();
    assert!(config.switch_carry_position);
}

#[test]
fn a_switch_announces_the_arrival_and_the_departure_unless_told_not_to() {
    let config = Config::from_toml_str(&text("", "", "")).unwrap();
    assert!(config.switch_announce_arrival);
    assert!(config.switch_announce_departure);
    let config = Config::from_toml_str(&text("switch-announce-arrival = false", "", "")).unwrap();
    assert!(!config.switch_announce_arrival);
    assert!(config.switch_announce_departure);
    let config = Config::from_toml_str(&text("switch-announce-departure = false", "", "")).unwrap();
    assert!(config.switch_announce_arrival);
    assert!(!config.switch_announce_departure);
    let config = Config::from_toml_str(&text(
        "switch-announce-arrival = false
switch-announce-departure = false",
        "",
        "",
    ))
    .unwrap();
    assert!(!config.switch_announce_arrival && !config.switch_announce_departure);
    let config = Config::from_toml_str(&text(
        "switch-announce-arrival = true
switch-announce-departure = true",
        "",
        "",
    ))
    .unwrap();
    assert!(config.switch_announce_arrival && config.switch_announce_departure);
    // A value that is not true or false is refused, not read as one of them.
    for bad in [
        "switch-announce-arrival = \"no\"",
        "switch-announce-departure = 0",
    ] {
        let e = Config::from_toml_str(&text(bad, "", "")).unwrap_err();
        assert!(matches!(e, ConfigError::Parse(_)), "{bad}: {e}");
    }
}

#[test]
fn the_freeze_a_switch_may_use_has_a_limit_and_a_margin() {
    let config = Config::from_toml_str(&text("", "", "")).unwrap();
    assert_eq!(config.switch_freeze_limit_ms, 8_000);
    assert_eq!(config.switch_commit_margin_ms, 2_000);
    let config = Config::from_toml_str(&text(
        "switch-freeze-limit-ms = 12000\nswitch-commit-margin-ms = 500",
        "",
        "",
    ))
    .unwrap();
    assert_eq!(config.switch_freeze_limit_ms, 12_000);
    assert_eq!(config.switch_commit_margin_ms, 500);
    // No margin at all is allowed; no time to freeze is not.
    let config = Config::from_toml_str(&text("switch-commit-margin-ms = 0", "", "")).unwrap();
    assert_eq!(config.switch_commit_margin_ms, 0);
    let e = Config::from_toml_str(&text("switch-freeze-limit-ms = 0", "", "")).unwrap_err();
    assert!(matches!(e, ConfigError::ZeroFreezeLimit), "{e}");
}

#[test]
fn a_sidecar_gives_notices_only_when_the_backend_says_so() {
    let dir = folder("notices");
    let config = load(
        &dir,
        "",
        "sidecar = \"127.0.0.1:25580\"\nsidecar-secret-file = \"a.key\"\nsidecar-notices = true",
        "sidecar = \"127.0.0.1:25581\"\nsidecar-secret-file = \"secrets/b.key\"",
    )
    .unwrap();
    let notices: Vec<(&str, bool)> = config
        .sidecars
        .iter()
        .map(|s| (s.backend.as_str(), s.notices))
        .collect();
    assert_eq!(notices, [("lobby", true), ("east", false)]);
    // Without a sidecar there is nobody to ask.
    let e = load(&dir, "", "sidecar-notices = true", "").unwrap_err();
    assert!(
        matches!(&e, ConfigError::NoticesWithoutSidecar(b) if b == "lobby"),
        "{e}"
    );
    assert!(e.to_string().contains("sidecar-notices"), "{e}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_move_carries_the_state_only_inside_one_state_group() {
    // No group anywhere: every move carries the state, as before the setting.
    let config = Config::from_toml_str(&text("", "", "")).unwrap();
    assert!(config.state_groups.is_empty());
    assert!(config.carries_state("lobby", "east"));
    // Two groups: nothing is carried between them, either way.
    let config = Config::from_toml_str(&text(
        "",
        "state-group = \"lobby\"",
        "state-group = \"survival\"",
    ))
    .unwrap();
    assert_eq!(
        config.state_groups.get("lobby").map(String::as_str),
        Some("lobby")
    );
    assert_eq!(
        config.state_groups.get("east").map(String::as_str),
        Some("survival")
    );
    assert!(!config.carries_state("lobby", "east"));
    assert!(!config.carries_state("east", "lobby"));
    // The same group: carried.
    let config =
        Config::from_toml_str(&text("", "state-group = \"one\"", "state-group = \"one\"")).unwrap();
    assert!(config.carries_state("lobby", "east"));
    // One server in a group and one in none: they are not the same group.
    let config = Config::from_toml_str(&text("", "state-group = \"lobby\"", "")).unwrap();
    assert!(!config.carries_state("lobby", "east"));
    assert!(!config.carries_state("east", "lobby"));
    // An empty name is refused, not taken as "no group".
    for empty in ["state-group = \"\"", "state-group = \"  \""] {
        let e = Config::from_toml_str(&text("", empty, "")).unwrap_err();
        assert!(
            matches!(e, ConfigError::EmptyStateGroup(ref b) if b == "lobby"),
            "{empty}: {e}"
        );
    }
    // A group given to a backend the configuration does not have (a configuration built in
    // code) is refused.
    let mut config = Config::from_toml_str(&text("", "", "")).unwrap();
    config.state_groups.insert("west".into(), "survival".into());
    let e = config.validate().unwrap_err();
    assert!(
        matches!(e, ConfigError::StateGroupOfUnknownBackend(ref b) if b == "west"),
        "{e}"
    );
}

#[test]
fn a_seamless_forced_host_needs_the_sidecar_of_its_backend() {
    let dir = folder("seamless");
    let forced = |seamless: &str| {
        format!(
            "[[forced-host]]
host = \"Survival.Example.org\"
backend = \"east\"
{seamless}"
        )
    };
    let sidecar = "sidecar = \"127.0.0.1:7001\"\nsidecar-secret-file = \"a.key\"";
    // Off by default.
    let config = load(&dir, &forced(""), "", "").unwrap();
    assert!(!config.forced_hosts[0].seamless);
    // On, with the backend's sidecar.
    let config = load(&dir, &forced("seamless = true"), "", sidecar).unwrap();
    assert!(config.forced_hosts[0].seamless);
    assert_eq!(config.forced_hosts[0].host, "survival.example.org");
    // On, without it: refused, naming the host and the backend.
    let e = load(&dir, &forced("seamless = true"), sidecar, "").unwrap_err();
    assert!(
        matches!(e, ConfigError::SeamlessWithoutSidecar { ref backend, .. } if backend == "east"),
        "{e}"
    );
    assert!(e.to_string().contains("survival.example.org"), "{e}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_state_group_is_one_name_whatever_its_case_and_has_no_spaces_around_it() {
    let config = Config::from_toml_str(&text(
        "",
        "state-group = \"Lobby\"",
        "state-group = \"lobby\"",
    ))
    .unwrap();
    assert!(config.carries_state("lobby", "east"));
    assert_eq!(
        config.state_groups.get("lobby").map(String::as_str),
        Some("lobby")
    );
    for spaced in ["state-group = \"lobby \"", "state-group = \" lobby\""] {
        let e = Config::from_toml_str(&text("", spaced, "")).unwrap_err();
        assert!(
            matches!(e, ConfigError::StateGroupSpaces(ref b) if b == "lobby"),
            "{spaced}: {e}"
        );
    }
}

#[test]
fn a_backend_may_take_no_seamless_arrival_and_then_no_seamless_host_leads_to_it() {
    let dir = folder("closed");
    let sidecar = "sidecar = \"127.0.0.1:7001\"\nsidecar-secret-file = \"a.key\"";
    // Open by default.
    let config = load(&dir, "", "", sidecar).unwrap();
    assert!(config.takes_seamless_arrival("east"));
    // Closed.
    let closed = format!("{sidecar}\nseamless-arrival = false");
    let config = load(&dir, "", "", &closed).unwrap();
    assert!(!config.takes_seamless_arrival("east"));
    assert!(config.takes_seamless_arrival("lobby"));
    // A forced host that is not seamless may lead to it (the game's own transfer, with a login).
    let forced = |seamless: &str| {
        format!("[[forced-host]]\nhost = \"east.example.org\"\nbackend = \"east\"\n{seamless}")
    };
    load(&dir, &forced(""), "", &closed).unwrap();
    // A seamless one may not.
    let e = load(&dir, &forced("seamless = true"), "", &closed).unwrap_err();
    assert!(
        matches!(e, ConfigError::SeamlessToClosedBackend { ref backend, .. } if backend == "east"),
        "{e}"
    );
    // Not true or false: refused.
    let e = load(&dir, "", "", "seamless-arrival = \"no\"").unwrap_err();
    assert!(matches!(e, ConfigError::Parse(_)), "{e}");
    // Given to a backend the configuration does not have, in code: refused.
    let mut config = load(&dir, "", "", "").unwrap();
    config.no_seamless_arrival.insert("west".into());
    assert!(
        matches!(config.validate().unwrap_err(), ConfigError::StateGroupOfUnknownBackend(ref b) if b == "west")
    );
    std::fs::remove_dir_all(&dir).unwrap();
}
