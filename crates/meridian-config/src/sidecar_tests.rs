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
            },
            Sidecar {
                backend: "east".into(),
                address: "127.0.0.1:25581".into(),
                secret_file: dir.join("secrets/b.key"),
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
