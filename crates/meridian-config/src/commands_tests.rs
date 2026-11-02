//! The `[commands]` table.
use super::*;

fn with(top: &str, commands: &str) -> Result<Config, ConfigError> {
    Config::from_toml_str(&format!(
        r#"{top}
forwarding-secret = "test-secret"
[[backends]]
name = "lobby"
address = "127.0.0.1:25566"
[[backends]]
name = "hub"
address = "127.0.0.1:25567"
{commands}"#
    ))
}

#[test]
fn both_commands_are_on_and_the_hub_is_the_default_backend_by_default() {
    let config = with("", "").unwrap();
    assert_eq!(config.commands, CommandSettings::default());
    assert_eq!(
        config.commands.enabled,
        [ProxyCommand::Server, ProxyCommand::Hub]
    );
    assert!(config.commands.server_list);
    assert_eq!(config.hub_backend(), "lobby");
}

#[test]
fn the_commands_table_is_read() {
    let config = with(
        "",
        "[commands]\nenabled = [\"hub\"]\nhub-backend = \"hub\"\nserver-list = false",
    )
    .unwrap();
    assert_eq!(config.commands.enabled, [ProxyCommand::Hub]);
    assert!(config.commands.is_enabled(ProxyCommand::Hub));
    assert!(!config.commands.is_enabled(ProxyCommand::Server));
    assert!(!config.commands.server_list);
    assert_eq!(config.hub_backend(), "hub");
}

#[test]
fn every_command_can_be_turned_off() {
    let config = with("", "[commands]\nenabled = []").unwrap();
    assert!(config.commands.enabled.is_empty());
}

#[test]
fn the_hub_follows_the_default_backend_when_not_set() {
    let config = with("default-backend = \"hub\"", "").unwrap();
    assert_eq!(config.hub_backend(), "hub");
}

#[test]
fn bad_commands_are_refused() {
    let e = with("", "[commands]\nhub-backend = \"nether\"").unwrap_err();
    assert!(
        matches!(&e, ConfigError::UnknownBackend { setting, name } if setting == "[commands] hub-backend" && name == "nether"),
        "{e}"
    );
    let e = with("", "[commands]\nenabled = [\"hub\", \"hub\"]").unwrap_err();
    assert!(matches!(e, ConfigError::DuplicateCommand("hub")), "{e}");
    for bad in [
        "[commands]\nenabled = [\"tp\"]",
        "[commands]\nserver-list = \"yes\"",
        "[commands]\nlobby = true",
    ] {
        assert!(matches!(with("", bad), Err(ConfigError::Parse(_))), "{bad}");
    }
}
