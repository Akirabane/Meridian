//! The `[operators]` table.
use super::*;
use std::path::PathBuf;

const SECRET: &str = "0123456789abcdef0123456789abcdef";

/// A folder with a secret file `a.key`, for backends that have a sidecar.
fn folder(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("meridian-operators-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.key"), format!("{SECRET}\n")).unwrap();
    dir
}

/// `lobby` has a sidecar, `east` has none; `tables` is written after the backends.
fn load(dir: &Path, tables: &str) -> Result<Config, ConfigError> {
    Config::from_toml_str_in(
        &format!(
            r#"forwarding-secret = "test-secret"
[[backends]]
name = "lobby"
address = "127.0.0.1:25601"
sidecar = "127.0.0.1:25580"
sidecar-secret-file = "a.key"
[[backends]]
name = "east"
address = "127.0.0.1:25602"
{tables}"#
        ),
        dir,
    )
}

#[test]
fn the_operators_are_not_read_by_default() {
    let dir = folder("default");
    let settings = load(&dir, "").unwrap().operators;
    assert_eq!(settings, OperatorSettings::default());
    assert!(!settings.from_servers);
    assert_eq!(settings.group, "operator");
    assert_eq!(settings.min_level, 4);
    assert!(settings.from.is_empty());
    assert_eq!(settings.stale_after_seconds, 600);
    assert_eq!(
        load(&dir, "[operators]").unwrap().operators,
        OperatorSettings::default()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn every_key_can_be_set() {
    let dir = folder("keys");
    let settings = load(
        &dir,
        "[operators]\nfrom-servers = true\ngroup = \"staff\"\nmin-level = 2\nfrom = [\"lobby\"]\nstale-after-seconds = 60",
    )
    .unwrap()
    .operators;
    assert_eq!(
        settings,
        OperatorSettings {
            from_servers: true,
            group: "staff".into(),
            min_level: 2,
            from: vec!["lobby".into()],
            stale_after_seconds: 60,
        }
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unknown_key_is_refused() {
    let dir = folder("unknown");
    assert!(load(&dir, "[operators]\nlevel = 4").is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_level_above_the_highest_is_refused_whether_or_not_the_table_is_on() {
    let dir = folder("level");
    for on in ["true", "false"] {
        let tables = format!("[operators]\nfrom-servers = {on}\nmin-level = 5");
        let e = load(&dir, &tables).unwrap_err();
        assert!(matches!(e, ConfigError::OperatorLevelTooHigh(5)), "{e}");
    }
    // 0 to 4 are levels.
    for level in 0..=4 {
        assert!(load(&dir, &format!("[operators]\nmin-level = {level}")).is_ok());
    }
    // A number that is not a level at all is not read.
    assert!(load(&dir, "[operators]\nmin-level = 300").is_err());
    assert!(load(&dir, "[operators]\nmin-level = -1").is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_group_must_be_a_group_name_when_the_table_is_on() {
    let dir = folder("group");
    let e = load(
        &dir,
        "[operators]\nfrom-servers = true\ngroup = \"the staff\"",
    )
    .unwrap_err();
    assert!(matches!(e, ConfigError::BadOperatorGroup(_)), "{e}");
    assert!(e.to_string().contains("the staff"), "{e}");
    assert!(load(&dir, "[operators]\nfrom-servers = true\ngroup = \"\"").is_err());
    // Off, it is not used.
    assert!(load(&dir, "[operators]\ngroup = \"the staff\"").is_ok());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_time_a_list_is_believed_has_bounds() {
    let dir = folder("stale");
    for bad in [0, MAX_OPERATOR_STALE_SECONDS + 1] {
        let tables = format!("[operators]\nfrom-servers = true\nstale-after-seconds = {bad}");
        let e = load(&dir, &tables).unwrap_err();
        assert!(matches!(e, ConfigError::OperatorStaleOutOfRange(_)), "{e}");
    }
    for good in [1, MAX_OPERATOR_STALE_SECONDS] {
        let tables = format!("[operators]\nfrom-servers = true\nstale-after-seconds = {good}");
        assert!(load(&dir, &tables).is_ok());
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_servers_that_count_are_known_and_have_a_sidecar() {
    let dir = folder("from");
    let on = "[operators]\nfrom-servers = true\n";
    // `east` is a backend, but nothing can be read from it.
    let e = load(&dir, &format!("{on}from = [\"east\"]")).unwrap_err();
    assert!(
        matches!(&e, ConfigError::OperatorSourceWithoutSidecar(name) if name == "east"),
        "{e}"
    );
    let e = load(&dir, &format!("{on}from = [\"nowhere\"]")).unwrap_err();
    assert!(matches!(e, ConfigError::UnknownBackend { .. }), "{e}");
    let e = load(&dir, &format!("{on}from = [\"lobby\", \"lobby\"]")).unwrap_err();
    assert!(matches!(e, ConfigError::DuplicateOperatorSource(_)), "{e}");
    assert!(load(&dir, &format!("{on}from = [\"lobby\"]")).is_ok());
    // Off, `from` is not looked at.
    assert!(load(&dir, "[operators]\nfrom = [\"east\"]").is_ok());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn which_servers_count() {
    let all = OperatorSettings::default();
    assert!(all.counts("lobby") && all.counts("anything"));
    let some = OperatorSettings {
        from: vec!["lobby".into()],
        ..OperatorSettings::default()
    };
    assert!(some.counts("lobby"));
    assert!(!some.counts("east"));
}
