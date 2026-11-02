//! The `[commands]` table: the commands players can type that the proxy answers
//! itself, without passing them to the backend.
use crate::{Config, ConfigError};
use serde::Deserialize;

/// A command the proxy answers in game.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProxyCommand {
    /// `/server` lists the backends; `/server <name>` asks to move to one.
    Server,
    /// `/hub` and `/lobby` ask to move to the hub backend.
    Hub,
}

impl ProxyCommand {
    /// The name used in the file.
    pub fn name(self) -> &'static str {
        match self {
            ProxyCommand::Server => "server",
            ProxyCommand::Hub => "hub",
        }
    }

    /// The words a player types for it, without the slash.
    pub fn words(self) -> &'static [&'static str] {
        match self {
            ProxyCommand::Server => &["server"],
            ProxyCommand::Hub => &["hub", "lobby"],
        }
    }
}

/// The commands the proxy answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandSettings {
    /// The commands the proxy answers; any other command goes to the backend.
    pub enabled: Vec<ProxyCommand>,
    /// The backend `/hub` leads to; `None` means the default backend.
    pub hub_backend: Option<String>,
    /// Whether players may list the backends with `/server`.
    pub server_list: bool,
}

impl Default for CommandSettings {
    fn default() -> Self {
        Self {
            enabled: vec![ProxyCommand::Server, ProxyCommand::Hub],
            hub_backend: None,
            server_list: true,
        }
    }
}

impl CommandSettings {
    /// Whether the proxy answers `command`.
    pub fn is_enabled(&self, command: ProxyCommand) -> bool {
        self.enabled.contains(&command)
    }

    /// Each command once, and a hub backend that is configured.
    pub(crate) fn validate(&self, config: &Config) -> Result<(), ConfigError> {
        for (i, command) in self.enabled.iter().enumerate() {
            if self.enabled[..i].contains(command) {
                return Err(ConfigError::DuplicateCommand(command.name()));
            }
        }
        if let Some(hub) = &self.hub_backend {
            if config.backend(hub).is_none() {
                return Err(ConfigError::UnknownBackend {
                    setting: "[commands] hub-backend".to_owned(),
                    name: hub.clone(),
                });
            }
        }
        Ok(())
    }
}

impl Config {
    /// The backend `/hub` and `/lobby` lead to.
    pub fn hub_backend(&self) -> &str {
        self.commands
            .hub_backend
            .as_deref()
            .unwrap_or(&self.default_backend)
    }
}

/// The `[commands]` table as written.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct FileCommands {
    enabled: Option<Vec<ProxyCommand>>,
    hub_backend: Option<String>,
    server_list: Option<bool>,
}

impl FileCommands {
    pub(crate) fn into_settings(self) -> CommandSettings {
        let defaults = CommandSettings::default();
        CommandSettings {
            enabled: self.enabled.unwrap_or(defaults.enabled),
            hub_backend: self.hub_backend,
            server_list: self.server_list.unwrap_or(defaults.server_list),
        }
    }
}
