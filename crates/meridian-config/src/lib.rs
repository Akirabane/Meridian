//! The configuration of the proxy: where it listens, which backends it knows, and
//! the limits it applies. For now this crate only describes the settings; reading
//! them from a file and checking them will live here too.
use std::net::SocketAddr;

/// A server the proxy can send players to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Backend {
    /// The name used in routes and commands.
    pub name: String,
    /// Where the server listens, as `host:port`.
    pub address: String,
}

/// Everything the proxy reads from its configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// The address players connect to.
    pub listen: SocketAddr,
    /// The servers behind the proxy.
    pub backends: Vec<Backend>,
    /// The backend a player is sent to when they log in.
    pub default_backend: String,
    /// The line shown in the server list.
    pub motd: String,
    /// Whether players are checked against the session service.
    pub online_mode: bool,
    /// Packets of at least this many bytes are compressed; a negative value turns
    /// compression off.
    pub compression_threshold: i32,
    /// Milliseconds allowed to reach a backend.
    pub connect_timeout_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: SocketAddr::from(([0, 0, 0, 0], 25565)),
            backends: Vec::new(),
            default_backend: String::new(),
            motd: "A Meridian proxy".to_owned(),
            online_mode: true,
            compression_threshold: 256,
            connect_timeout_ms: 5_000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_listens_on_the_game_port() {
        assert_eq!(Config::default().listen.port(), 25565);
    }
}
