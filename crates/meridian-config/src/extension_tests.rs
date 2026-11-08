//! The `[[extension]]` tables and their token files.
use super::*;

const TOKEN: &str = "0123456789abcdef0123456789abcdef";

/// A folder of the test's own with token files `a.token` and `b.token` (long enough), `short.token`
/// (too short) and a `tokens` subfolder holding `c.token`.
fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("meridian-ext-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("tokens")).unwrap();
    std::fs::write(dir.join("a.token"), format!("{TOKEN}\n")).unwrap();
    std::fs::write(dir.join("b.token"), format!("{TOKEN}b\r\n")).unwrap();
    std::fs::write(dir.join("tokens").join("c.token"), TOKEN).unwrap();
    std::fs::write(dir.join("short.token"), "too short\r\n").unwrap();
    dir
}

/// One backend, `top` before it and `tables` after it.
fn load(dir: &Path, top: &str, tables: &str) -> Result<Config, ConfigError> {
    Config::from_toml_str_in(
        &format!(
            r#"forwarding-secret = "test-secret"
{top}
[[backends]]
name = "lobby"
address = "127.0.0.1:25566"
{tables}"#
        ),
        dir,
    )
}

/// An extension named `name` on the port `port`, with the token file `token` and `more` in its table.
fn extension(name: &str, port: u16, token: &str, more: &str) -> String {
    format!(
        "\n[[extension]]\nname = \"{name}\"\naddress = \"127.0.0.1:{port}\"\ntoken-file = \"{token}\"\n{more}\n"
    )
}

#[test]
fn there_is_no_extension_by_default() {
    let config = load(&folder("none"), "", "").unwrap();
    assert!(config.extensions.is_empty());
    assert_eq!(config.enabled_extensions().count(), 0);
    assert!(Config::default().extensions.is_empty());
}

#[test]
fn an_extension_takes_the_defaults_of_the_specification() {
    let dir = folder("defaults");
    let config = load(&dir, "", &extension("perms", 25590, "a.token", "")).unwrap();
    assert_eq!(
        config.extensions,
        [Extension {
            name: "perms".to_owned(),
            enabled: true,
            address: "127.0.0.1:25590".to_owned(),
            token_file: dir.join("a.token"),
            grant: Vec::new(),
            max_in_flight: 64,
            breaker_failures: 5,
            breaker_seconds: 30,
            decide_login: LoginDecision {
                timeout_ms: 1000,
                on_error: OnError::Default,
            },
            decide_command: CommandDecision {
                timeout_ms: 250,
                on_error: OnError::Default,
                cache_seconds_max: 300,
            },
            decide_server: ServerDecision {
                timeout_ms: 1000,
                on_error: ServerOnError::Default,
            },
            own_commands: OwnCommands { timeout_ms: 2000 },
        }]
    );
}

/// The example of docs/EXTENSIONS.md, 4.1, with every key written.
#[test]
fn every_key_can_be_set_and_the_token_file_is_taken_from_the_configurations_folder() {
    let dir = folder("keys");
    let text = extension(
        "perms",
        25590,
        "tokens/c.token",
        r#"enabled = false
grant = ["decide.login", "decide.command", "decide.server", "commands", "orders.read"]
max-in-flight = 8
breaker-failures = 3
breaker-seconds = 10

[extension.decide-login]
timeout-ms = 700
on-error = "deny"

[extension.decide-command]
timeout-ms = 100
on-error = "deny"
cache-seconds-max = 60

[extension.decide-server]
timeout-ms = 400
on-error = "refuse"
"#,
    );
    let config = load(&dir, "", &text).unwrap();
    let e = &config.extensions[0];
    assert!(!e.enabled);
    assert_eq!(e.token_file, dir.join("tokens/c.token"));
    assert_eq!(
        e.grant,
        [
            "decide.login",
            "decide.command",
            "decide.server",
            "commands",
            "orders.read"
        ]
    );
    assert!(e.grants(DECIDE_LOGIN) && !e.grants("orders.move"));
    assert_eq!(
        (e.max_in_flight, e.breaker_failures, e.breaker_seconds),
        (8, 3, 10)
    );
    assert_eq!(
        e.decide_login,
        LoginDecision {
            timeout_ms: 700,
            on_error: OnError::Deny
        }
    );
    assert_eq!(
        e.decide_command,
        CommandDecision {
            timeout_ms: 100,
            on_error: OnError::Deny,
            cache_seconds_max: 60
        }
    );
    assert_eq!(
        e.decide_server,
        ServerDecision {
            timeout_ms: 400,
            on_error: ServerOnError::Refuse
        }
    );
    // Not enabled: it is in the configuration and not among the extensions to connect to.
    assert_eq!(config.enabled_extensions().count(), 0);
}

#[test]
fn several_extensions_keep_their_order() {
    let dir = folder("several");
    let text = format!(
        "{}{}",
        extension("first", 25590, "a.token", "grant = [\"decide.login\"]"),
        extension("second", 25591, "b.token", "grant = [\"decide.command\"]"),
    );
    let names: Vec<_> = load(&dir, "", &text)
        .unwrap()
        .extensions
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(names, ["first", "second"]);
}

/// An extension runs on this machine: an address elsewhere is refused when the file is read.
#[test]
fn only_a_loopback_address_is_accepted() {
    let dir = folder("loopback");
    let with = |address: &str| {
        load(
            &dir,
            "",
            &format!(
                "\n[[extension]]\nname = \"x\"\naddress = \"{address}\"\ntoken-file = \"a.token\"\n"
            ),
        )
    };
    for good in [
        "127.0.0.1:25590",
        "127.0.0.2:25590",
        "[::1]:25590",
        "localhost:25590",
        "LocalHost:25590",
    ] {
        assert!(with(good).is_ok(), "{good}");
    }
    for bad in [
        "192.168.1.20:25590",
        "10.0.0.1:25590",
        "0.0.0.0:25590",
        "[::]:25590",
        "[::ffff:127.0.0.1]:25590",
        "example.com:25590",
        "my-host:25590",
    ] {
        let e = with(bad).unwrap_err();
        assert!(
            matches!(e, ConfigError::InvalidExtensionAddress { .. }),
            "{bad}: {e}"
        );
        assert!(e.to_string().contains(bad), "{e}");
    }
    // Not host:port at all.
    for bad in [
        "127.0.0.1",
        "127.0.0.1:0",
        "127.0.0.1:x",
        ":25590",
        "::1:25590",
    ] {
        assert!(
            matches!(
                with(bad).unwrap_err(),
                ConfigError::InvalidExtensionAddress { .. }
            ),
            "{bad}"
        );
    }
}

#[test]
fn a_name_is_plain_and_unique() {
    let dir = folder("names");
    for bad in [
        "",
        "has space",
        "a.b",
        "quote\\\"",
        "caf\u{e9}",
        &"n".repeat(33),
    ] {
        let e = load(&dir, "", &extension(bad, 25590, "a.token", "")).unwrap_err();
        assert!(matches!(e, ConfigError::InvalidExtensionName { .. }), "{e}");
    }
    assert!(load(&dir, "", &extension(&"n".repeat(32), 25590, "a.token", "")).is_ok());
    assert!(load(&dir, "", &extension("a-b_C9", 25590, "a.token", "")).is_ok());
    let twice = format!(
        "{}{}",
        extension("same", 25590, "a.token", ""),
        extension("same", 25591, "b.token", "")
    );
    assert!(matches!(
        load(&dir, "", &twice).unwrap_err(),
        ConfigError::DuplicateExtension(name) if name == "same"
    ));
    // The case does not tell two names apart: their permission nodes are lower case.
    let cases = format!(
        "{}{}",
        extension("Perms", 25590, "a.token", ""),
        extension("perms", 25591, "b.token", "")
    );
    assert!(matches!(
        load(&dir, "", &cases).unwrap_err(),
        ConfigError::DuplicateExtension(name) if name == "perms"
    ));
}

#[test]
fn the_commands_of_an_extension_wait_two_seconds_unless_told_and_need_their_capability() {
    let dir = folder("own-commands");
    let granted = |more: &str| {
        extension(
            "x",
            25590,
            "a.token",
            &format!("grant = [\"commands\"]\n{more}"),
        )
    };
    let config = load(&dir, "", &granted("")).unwrap();
    assert_eq!(config.extensions[0].own_commands.timeout_ms, 2000);
    let config = load(&dir, "", &granted("[extension.commands]\ntimeout-ms = 750")).unwrap();
    assert_eq!(config.extensions[0].own_commands.timeout_ms, 750);
    // An empty table is the defaults.
    let config = load(&dir, "", &granted("[extension.commands]")).unwrap();
    assert_eq!(config.extensions[0].own_commands.timeout_ms, 2000);
    // Not granted, the table would be read and never used.
    let text = extension(
        "x",
        25590,
        "a.token",
        "[extension.commands]\ntimeout-ms = 750",
    );
    let e = load(&dir, "", &text).unwrap_err();
    assert!(matches!(e, ConfigError::DecideWithoutGrant { .. }), "{e}");
    assert!(e.to_string().contains("commands"), "{e}");
    // Zero is no time at all; a key nobody knows is a mistake.
    let e = load(&dir, "", &granted("[extension.commands]\ntimeout-ms = 0")).unwrap_err();
    assert!(matches!(e, ConfigError::ZeroExtensionSetting { .. }), "{e}");
    assert!(e.to_string().contains("commands timeout-ms"), "{e}");
    assert!(
        load(
            &dir,
            "",
            &granted("[extension.commands]\non-error = \"deny\"")
        )
        .is_err()
    );
}

#[test]
fn two_extensions_do_not_share_an_address_or_a_token_file() {
    let dir = folder("shared");
    let same_address = format!(
        "{}{}",
        extension("one", 25590, "a.token", ""),
        extension("two", 25590, "b.token", "")
    );
    let e = load(&dir, "", &same_address).unwrap_err();
    assert!(
        matches!(e, ConfigError::SharedExtensionAddress { .. }),
        "{e}"
    );
    let same_token = format!(
        "{}{}",
        extension("one", 25590, "a.token", ""),
        extension("two", 25591, "./a.token", "")
    );
    let e = load(&dir, "", &same_token).unwrap_err();
    assert!(matches!(e, ConfigError::SharedExtensionToken { .. }), "{e}");
}

#[test]
fn the_token_of_an_enabled_extension_must_exist_and_be_long_enough() {
    let dir = folder("token");
    for file in ["missing.token", "short.token"] {
        let e = load(&dir, "", &extension("x", 25590, file, "")).unwrap_err();
        assert!(matches!(e, ConfigError::ExtensionToken { .. }), "{e}");
        let text = e.to_string();
        assert!(
            text.contains(file) && text.contains("extension \"x\""),
            "{text}"
        );
    }
    // Exactly the length of the handshake's minimum, after the line end an editor leaves.
    std::fs::write(dir.join("edge.token"), format!("{}\n\n", &TOKEN[..32])).unwrap();
    assert!(load(&dir, "", &extension("x", 25590, "edge.token", "")).is_ok());
    std::fs::write(dir.join("edge.token"), format!("{}\n\n", &TOKEN[..31])).unwrap();
    assert!(load(&dir, "", &extension("x", 25590, "edge.token", "")).is_err());
    // An extension that is switched off does not need its file.
    let off = extension("x", 25590, "missing.token", "enabled = false");
    assert!(load(&dir, "", &off).is_ok());
}

#[test]
fn the_error_about_a_token_never_shows_it() {
    let dir = folder("secret");
    std::fs::write(dir.join("odd.token"), "very-secret-but-short").unwrap();
    let e = load(&dir, "", &extension("x", 25590, "odd.token", "")).unwrap_err();
    assert!(!e.to_string().contains("very-secret"), "{e}");
    // Nor does the configuration, which holds only the path.
    let config = load(&dir, "", &extension("x", 25590, "a.token", "")).unwrap();
    assert!(!format!("{config:?}").contains(TOKEN));
}

#[test]
fn a_capability_exists_and_is_granted_once() {
    let dir = folder("grant");
    let e = load(
        &dir,
        "",
        &extension(
            "x",
            25590,
            "a.token",
            "grant = [\"decide.login\", \"decide.logins\"]",
        ),
    )
    .unwrap_err();
    assert!(matches!(e, ConfigError::UnknownCapability { .. }), "{e}");
    assert!(e.to_string().contains("decide.logins"), "{e}");
    let e = load(
        &dir,
        "",
        &extension("x", 25590, "a.token", "grant = [\"events\", \"events\"]"),
    )
    .unwrap_err();
    assert!(matches!(e, ConfigError::DuplicateCapability { .. }), "{e}");
    // Every capability of the table in the specification is accepted.
    let all = format!("grant = {CAPABILITIES:?}");
    assert!(load(&dir, "", &extension("x", 25590, "a.token", &all)).is_ok());
    assert_eq!(CAPABILITIES.len(), 12);
}

#[test]
fn a_decide_table_needs_its_capability() {
    let dir = folder("tables");
    for (table, capability) in [
        ("decide-login", "decide.login"),
        ("decide-command", "decide.command"),
        ("decide-server", "decide.server"),
    ] {
        let text = extension(
            "x",
            25590,
            "a.token",
            &format!("[extension.{table}]\ntimeout-ms = 100"),
        );
        let e = load(&dir, "", &text).unwrap_err();
        assert!(matches!(e, ConfigError::DecideWithoutGrant { .. }), "{e}");
        assert!(e.to_string().contains(table), "{e}");
        // Granted, the same table is accepted.
        let text = extension(
            "x",
            25590,
            "a.token",
            &format!("grant = [\"{capability}\"]\n[extension.{table}]\ntimeout-ms = 100"),
        );
        assert!(load(&dir, "", &text).is_ok(), "{table}");
    }
}

#[test]
fn a_decision_point_has_one_extension_at_most() {
    let dir = folder("one");
    let both = format!(
        "{}{}",
        extension(
            "one",
            25590,
            "a.token",
            "grant = [\"decide.login\", \"events\"]"
        ),
        extension(
            "two",
            25591,
            "b.token",
            "grant = [\"events\", \"decide.login\"]"
        ),
    );
    let e = load(&dir, "", &both).unwrap_err();
    assert!(
        matches!(&e, ConfigError::SharedDecisionPoint { capability, .. } if *capability == "decide.login"),
        "{e}"
    );
    // Other capabilities may be granted to several, and so may different points.
    let others = format!(
        "{}{}",
        extension(
            "one",
            25590,
            "a.token",
            "grant = [\"decide.login\", \"events\"]"
        ),
        extension(
            "two",
            25591,
            "b.token",
            "grant = [\"decide.command\", \"events\"]"
        ),
    );
    assert!(load(&dir, "", &others).is_ok());
    // An extension that is switched off holds no point: the operator can keep a spare.
    let spare = format!(
        "{}{}",
        extension("one", 25590, "a.token", "grant = [\"decide.login\"]"),
        extension(
            "two",
            25591,
            "b.token",
            "grant = [\"decide.login\"]\nenabled = false"
        ),
    );
    assert!(load(&dir, "", &spare).is_ok());
}

#[test]
fn zero_is_refused() {
    let dir = folder("zero");
    for (more, setting) in [
        ("max-in-flight = 0", "max-in-flight"),
        ("breaker-failures = 0", "breaker-failures"),
        ("breaker-seconds = 0", "breaker-seconds"),
        (
            "grant = [\"decide.login\"]\n[extension.decide-login]\ntimeout-ms = 0",
            "decide-login timeout-ms",
        ),
        (
            "grant = [\"decide.command\"]\n[extension.decide-command]\ntimeout-ms = 0",
            "decide-command timeout-ms",
        ),
        (
            "grant = [\"decide.server\"]\n[extension.decide-server]\ntimeout-ms = 0",
            "decide-server timeout-ms",
        ),
    ] {
        let e = load(&dir, "", &extension("x", 25590, "a.token", more)).unwrap_err();
        assert!(matches!(e, ConfigError::ZeroExtensionSetting { .. }), "{e}");
        assert!(e.to_string().contains(setting), "{e}");
    }
}

#[test]
fn an_unknown_key_or_value_is_refused() {
    let dir = folder("unknown");
    for more in [
        "port = 25590",
        "grant = \"decide.login\"",
        "grant = [\"decide.login\"]\n[extension.decide-login]\non-error = \"refuse\"",
        "grant = [\"decide.server\"]\n[extension.decide-server]\non-error = \"allow\"",
        "grant = [\"decide.login\"]\n[extension.decide-login]\ncache-seconds-max = 3",
        "grant = [\"decide.login\"]\n[extension.decide-login]\ntimeout = 3",
    ] {
        assert!(
            load(&dir, "", &extension("x", 25590, "a.token", more)).is_err(),
            "{more}"
        );
    }
    // The name, the address and the token file are written.
    for left_out in ["name", "address", "token-file"] {
        let text = extension("x", 25590, "a.token", "")
            .lines()
            .filter(|l| !l.starts_with(left_out))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(load(&dir, "", &text).is_err(), "{left_out}");
    }
}

/// The silence a client bears is 27 s in all: what the extensions take with the admission and the
/// choice of server counts with the queue's wait and the connection to a backend.
#[test]
fn the_login_decisions_count_in_the_time_a_client_waits_in_silence() {
    let dir = folder("budget");
    let decisions = |login: u64, server: u64| {
        extension(
            "x",
            25590,
            "a.token",
            &format!(
                "grant = [\"decide.login\", \"decide.server\"]\n[extension.decide-login]\ntimeout-ms = {login}\n[extension.decide-server]\ntimeout-ms = {server}"
            ),
        )
    };
    let queue = "\n[queue]\nenabled = true\nwait-seconds = 15\n";
    // 15 s + 5 s (connect) + 1 s + 1 s = 22 s: fits. The defaults of the extension are 1 s each.
    assert!(load(&dir, "", &format!("{queue}{}", decisions(1000, 1000))).is_ok());
    // 15 + 5 + 4 + 3 = 27 s fits, a millisecond more does not.
    assert!(load(&dir, "", &format!("{queue}{}", decisions(4000, 3000))).is_ok());
    let e = load(&dir, "", &format!("{queue}{}", decisions(4000, 3001))).unwrap_err();
    assert!(matches!(e, ConfigError::QueueWaitTooLong { .. }), "{e}");
    assert!(e.to_string().contains("decide-login"), "{e}");
    // Only the points that are granted and enabled count.
    let only_login = extension(
        "x",
        25590,
        "a.token",
        "grant = [\"decide.login\"]\n[extension.decide-login]\ntimeout-ms = 7000",
    );
    assert!(load(&dir, "", &format!("{queue}{only_login}")).is_ok());
    let e = load(
        &dir,
        "",
        &format!("{queue}{}", only_login.replace("7000", "7001")),
    )
    .unwrap_err();
    assert!(matches!(e, ConfigError::QueueWaitTooLong { .. }), "{e}");
    let off = extension(
        "x",
        25590,
        "a.token",
        "enabled = false\ngrant = [\"decide.login\"]\n[extension.decide-login]\ntimeout-ms = 20000",
    );
    assert!(load(&dir, "", &format!("{queue}{off}")).is_ok());
    // The messages of a configuration without an extension are the ones it had.
    let e = load(&dir, "", "\n[queue]\nenabled = true\nwait-seconds = 21\n").unwrap_err();
    assert!(!e.to_string().contains("decide-login"), "{e}");

    // With the queue off the time still counts, with the connection to a backend.
    assert!(load(&dir, "connect-timeout-ms = 5000", &decisions(11000, 11000)).is_ok());
    let e = load(&dir, "connect-timeout-ms = 5000", &decisions(11000, 11001)).unwrap_err();
    assert!(matches!(e, ConfigError::ExtensionBudget { .. }), "{e}");
    // The point of the commands is not on the login's way.
    let command = extension(
        "x",
        25590,
        "a.token",
        "grant = [\"decide.command\"]\n[extension.decide-command]\ntimeout-ms = 60000",
    );
    assert!(load(&dir, "", &command).is_ok());
}

/// With no usable answer `allow` would give every command of the proxy to anyone: refused, with the reason,
/// whether the extension is enabled or not. The login and the choice of server may `allow`.
#[test]
fn allow_is_refused_for_the_permission_of_a_command_and_accepted_for_the_others() {
    let dir = folder("allow-command");
    let command = |policy: &str, enabled: bool| {
        extension(
            "perms",
            25590,
            "a.token",
            &format!(
                "enabled = {enabled}\ngrant = [\"decide.command\"]\n[extension.decide-command]\non-error = \"{policy}\""
            ),
        )
    };
    for enabled in [true, false] {
        let e = load(&dir, "", &command("allow", enabled)).unwrap_err();
        assert!(
            matches!(e, ConfigError::ExtensionCommandAllow { .. }),
            "{e}"
        );
        let said = e.to_string();
        assert!(
            said.contains(
                "would give every command of the proxy to anyone while the extension is down: use \"default\" or \"deny\""
            ),
            "{said}"
        );
        assert!(said.contains("perms"), "{said}");
    }
    for policy in ["default", "deny"] {
        let config = load(&dir, "", &command(policy, true)).unwrap();
        assert_eq!(
            config.extensions[0].decide_command.on_error,
            if policy == "deny" {
                OnError::Deny
            } else {
                OnError::Default
            }
        );
    }
    // `allow` is fine for the login, and the table of the command has the default when it is not written.
    let login = extension(
        "perms",
        25590,
        "a.token",
        "grant = [\"decide.login\"]\n[extension.decide-login]\non-error = \"allow\"",
    );
    let config = load(&dir, "", &login).unwrap();
    assert_eq!(config.extensions[0].decide_login.on_error, OnError::Allow);
    assert_eq!(
        config.extensions[0].decide_command.on_error,
        OnError::Default
    );
    // For the choice of server the words are `default` and `refuse`: `allow` is not one.
    let server = extension(
        "perms",
        25590,
        "a.token",
        "grant = [\"decide.server\"]\n[extension.decide-server]\non-error = \"allow\"",
    );
    assert!(load(&dir, "", &server).is_err());
}
