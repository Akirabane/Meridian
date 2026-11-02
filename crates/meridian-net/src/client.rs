//! The player's side: the proxy answers the handshake, the status request and the
//! login itself, and the rest of the session goes through the connection it
//! leaves behind.
use crate::{Error, Profile, Result};
use horizon_protocol::connection::Connection;
use meridian_config::Config;

/// A player who has logged in to the proxy and is ready to be relayed.
pub struct ClientSession {
    pub profile: Profile,
    pub connection: Connection,
}

/// Takes a new connection through handshake and login. A status request is
/// answered and ends the connection, so it returns `Ok(None)`.
pub fn accept(_connection: Connection, _config: &Config) -> Result<Option<ClientSession>> {
    Err(Error::NotImplemented("accepting a player"))
}
