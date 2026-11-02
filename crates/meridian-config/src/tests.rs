use super::*;

const MINIMAL: &str = r#"
forwarding-secret = "test-secret"
[[backends]]
name = "lobby"
address = "127.0.0.1:25566"
"#;

fn error(text: &str) -> ConfigError {
    Config::from_toml_str(text).expect_err("the configuration should be refused")
}

fn with_backends(backends: &str) -> String {
    format!("default-backend = \"lobby\"\nforwarding-secret = \"test-secret\"\n{backends}")
}

#[test]
fn the_default_listens_on_the_game_port() {
    assert_eq!(Config::default().listen.port(), 25565);
}

#[test]
fn a_file_with_one_backend_takes_every_other_default() {
    let config = Config::from_toml_str(MINIMAL).unwrap();
    let defaults = Config::default();
    assert_eq!(config.listen, defaults.listen);
    assert_eq!(config.motd, defaults.motd);
    assert_eq!(config.online_mode, defaults.online_mode);
    assert_eq!(config.compression_threshold, defaults.compression_threshold);
    assert_eq!(config.connect_timeout_ms, defaults.connect_timeout_ms);
    assert_eq!(
        config.backends,
        vec![Backend {
            name: "lobby".into(),
            address: "127.0.0.1:25566".into()
        }]
    );
}

#[test]
fn a_missing_default_backend_is_the_first_backend() {
    let config = Config::from_toml_str(
        r#"
        forwarding-secret = "test-secret"
        [[backends]]
        name = "east"
        address = "east.internal:25566"
        [[backends]]
        name = "west"
        address = "west.internal:25566"
        "#,
    )
    .unwrap();
    assert_eq!(config.default_backend, "east");
}

#[test]
fn every_setting_is_read() {
    let config = Config::from_toml_str(
        r#"
        listen = "127.0.0.1:30000"
        motd = "Two servers, one world"
        online-mode = false
        compression-threshold = -1
        connect-timeout-ms = 1500
        default-backend = "west"
        forwarding-secret = "shared between the proxy and the backends"
        [[backends]]
        name = "east"
        address = "10.0.0.2:25566"
        [[backends]]
        name = "west"
        address = "[::1]:25567"
        "#,
    )
    .unwrap();
    assert_eq!(config.listen, "127.0.0.1:30000".parse().unwrap());
    assert_eq!(config.motd, "Two servers, one world");
    assert!(!config.online_mode);
    assert_eq!(config.compression_threshold, -1);
    assert_eq!(config.connect_timeout_ms, 1500);
    assert_eq!(config.default_backend, "west");
    assert_eq!(config.backend("west").unwrap().address, "[::1]:25567");
    assert_eq!(
        config.forwarding_secret,
        "shared between the proxy and the backends"
    );
}

#[test]
fn the_example_file_is_valid() {
    let config = Config::from_toml_str(include_str!("../../../meridian.example.toml")).unwrap();
    assert_eq!(config.default_backend, "lobby");
    assert_eq!(config.backends.len(), 2);
    // The example documents the defaults, so it must agree with them.
    let defaults = Config::default();
    assert_eq!(config.listen, defaults.listen);
    assert_eq!(config.motd, defaults.motd);
    assert_eq!(config.online_mode, defaults.online_mode);
    assert_eq!(config.compression_threshold, defaults.compression_threshold);
    assert_eq!(config.connect_timeout_ms, defaults.connect_timeout_ms);
    assert_eq!(config.max_players, defaults.max_players);
    assert_eq!(config.auth_mode, defaults.auth_mode);
    assert_eq!(config.auth, defaults.auth);
    assert_eq!(config.on_duplicate, defaults.on_duplicate);
    assert_eq!(
        config.connections_per_minute,
        defaults.connections_per_minute
    );
    assert_eq!(config.backend_retry_seconds, defaults.backend_retry_seconds);
    assert_eq!(config.status_sample, defaults.status_sample);
    // The example's secret is a placeholder that says so.
    assert!(config.forwarding_secret.starts_with("replace-me"));
    // The lists and tables are examples, one of each.
    assert_eq!(config.try_backends, ["east"]);
    assert_eq!(config.forced_backend("east.example.com"), Some("east"));
    assert_eq!(config.groups.len(), 1);
    assert_eq!(
        config.route_group(AuthKind::Verified).unwrap().name,
        "accounts"
    );
}

#[test]
fn the_hybrid_example_in_the_example_file_is_valid() {
    // Uncommenting the hybrid example, with its backend, must give a valid file.
    let example = include_str!("../../../meridian.example.toml")
        .replace("auth-mode = \"online\"", "auth-mode = \"hybrid\"");
    let start = example.find("for example:\n").unwrap() + "for example:\n".len();
    let end = start
        + example[start..]
            .find("[[route]]\nauth = \"verified\"")
            .unwrap();
    let uncommented = example[start..end].replace("#   ", "").replace("#\n", "\n");
    let example = format!("{}{uncommented}{}", &example[..start], &example[end..]);
    let text =
        format!("{example}\n[[backends]]\nname = \"guest-lobby\"\naddress = \"127.0.0.1:25568\"\n");
    let config = Config::from_toml_str(&text).unwrap();
    assert_eq!(
        config.route_group(AuthKind::Unverified).unwrap().name,
        "guests"
    );
}

#[test]
fn a_file_is_loaded_from_disk() {
    let path = std::env::temp_dir().join(format!("meridian-config-{}.toml", std::process::id()));
    std::fs::write(&path, MINIMAL).unwrap();
    let loaded = Config::load(&path);
    std::fs::remove_file(&path).unwrap();
    assert_eq!(loaded.unwrap().default_backend, "lobby");
}

#[test]
fn a_missing_file_names_its_path() {
    let path = Path::new("this-file-does-not-exist.toml");
    let e = Config::load(path).unwrap_err();
    assert!(matches!(e, ConfigError::Read { .. }));
    assert!(
        e.to_string().contains("this-file-does-not-exist.toml"),
        "{e}"
    );
}

#[test]
fn a_file_without_backends_is_refused() {
    assert!(matches!(error(""), ConfigError::NoBackend));
    assert!(matches!(error("backends = []"), ConfigError::NoBackend));
}

#[test]
fn an_unknown_default_backend_is_refused() {
    let e = error(&format!("default-backend = \"nether\"\n{MINIMAL}"));
    assert!(matches!(&e, ConfigError::UnknownDefaultBackend(name) if name == "nether"));
    assert!(e.to_string().contains("nether"), "{e}");
}

#[test]
fn two_backends_with_one_name_are_refused() {
    let e = error(&with_backends(
        r#"
        [[backends]]
        name = "lobby"
        address = "127.0.0.1:25566"
        [[backends]]
        name = "lobby"
        address = "127.0.0.1:25567"
        "#,
    ));
    assert!(matches!(&e, ConfigError::DuplicateBackend(name) if name == "lobby"));
}

#[test]
fn an_empty_backend_name_is_refused() {
    let e = error("[[backends]]\nname = \" \"\naddress = \"127.0.0.1:25566\"");
    assert!(matches!(e, ConfigError::EmptyBackendName));
}

#[test]
fn bad_backend_addresses_are_refused() {
    for address in [
        "127.0.0.1",
        "127.0.0.1:",
        ":25566",
        "127.0.0.1:0",
        "127.0.0.1:65536",
        "127.0.0.1:port",
        "lobby .internal:25566",
        "::1:25566",
        "[::1:25566",
        "[not-ipv6]:25566",
    ] {
        let text = with_backends(&format!(
            "[[backends]]\nname = \"lobby\"\naddress = \"{address}\""
        ));
        let e = error(&text);
        assert!(
            matches!(&e, ConfigError::InvalidAddress { address: a, .. } if a == address),
            "{address}: {e}"
        );
        assert!(e.to_string().contains("lobby"), "{e}");
    }
}

#[test]
fn good_backend_addresses_are_accepted() {
    for address in [
        "127.0.0.1:25566",
        "lobby.internal:1",
        "[::1]:65535",
        "localhost:25565",
    ] {
        let text = with_backends(&format!(
            "[[backends]]\nname = \"lobby\"\naddress = \"{address}\""
        ));
        assert!(Config::from_toml_str(&text).is_ok(), "{address}");
    }
}

#[test]
fn a_listen_address_without_a_port_is_refused() {
    let e = error(&format!("listen = \"0.0.0.0\"\n{MINIMAL}"));
    assert!(matches!(&e, ConfigError::InvalidListen(value) if value == "0.0.0.0"));
}

#[test]
fn a_zero_connect_timeout_is_refused() {
    let e = error(&format!("connect-timeout-ms = 0\n{MINIMAL}"));
    assert!(matches!(e, ConfigError::ZeroConnectTimeout));
}

#[test]
fn an_unknown_setting_is_refused_with_its_name() {
    let e = error(&format!("view-distance = 10\n{MINIMAL}"));
    assert!(matches!(e, ConfigError::Parse(_)));
    assert!(e.to_string().contains("view-distance"), "{e}");
}

#[test]
fn a_setting_of_the_wrong_type_is_refused() {
    let e = error(&format!("online-mode = \"yes\"\n{MINIMAL}"));
    assert!(matches!(e, ConfigError::Parse(_)));
}

#[test]
fn a_snake_case_setting_is_refused() {
    // The file uses kebab-case; the Rust spelling is not a second name for it.
    let e = error(&format!("online_mode = false\n{MINIMAL}"));
    assert!(matches!(e, ConfigError::Parse(_)));
}

#[test]
fn a_config_built_in_code_is_validated_too() {
    let mut config = Config::default();
    assert!(matches!(config.validate(), Err(ConfigError::NoBackend)));
    config.backends.push(Backend {
        name: "lobby".into(),
        address: "127.0.0.1:25566".into(),
    });
    assert!(matches!(
        config.validate(),
        Err(ConfigError::UnknownDefaultBackend(_))
    ));
    config.default_backend = "lobby".into();
    assert!(matches!(
        config.validate(),
        Err(ConfigError::MissingForwardingSecret)
    ));
    config.forwarding_secret = "test-secret".into();
    assert!(config.validate().is_ok());
}

#[test]
fn backends_without_a_forwarding_secret_are_refused() {
    let text = "[[backends]]\nname = \"lobby\"\naddress = \"127.0.0.1:25566\"";
    let e = error(text);
    assert!(matches!(e, ConfigError::MissingForwardingSecret));
    assert_eq!(
        e.to_string(),
        "forwarding-secret must be set: the backends check it"
    );
    let e = error(&format!("forwarding-secret = \"\"\n{text}"));
    assert!(matches!(e, ConfigError::MissingForwardingSecret));
}

#[test]
fn the_forwarding_secret_is_empty_by_default() {
    assert!(Config::default().forwarding_secret.is_empty());
}

#[test]
fn debug_output_hides_the_forwarding_secret() {
    let config = Config::from_toml_str(&format!(
        "forwarding-secret = \"s3cr3t-value\"\n{}",
        MINIMAL.replace("forwarding-secret = \"test-secret\"\n", "")
    ))
    .unwrap();
    assert_eq!(config.forwarding_secret, "s3cr3t-value");
    for shown in [format!("{config:?}"), format!("{config:#?}")] {
        assert!(!shown.contains("s3cr3t-value"), "{shown}");
        assert!(shown.contains("forwarding_secret: <hidden>"), "{shown}");
        // The other settings are still shown.
        assert!(shown.contains("127.0.0.1:25566"), "{shown}");
    }
}

#[test]
fn debug_output_says_when_the_secret_is_empty() {
    assert!(format!("{:?}", Config::default()).contains("forwarding_secret: <empty>"));
}
