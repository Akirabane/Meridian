//! The server's side: the proxy connects to a backend as a client and takes the
//! connection through login, leaving it at the start of the configuration phase.
use crate::{Error, Profile, Result};
use horizon_protocol::connection::Connection;

/// A connection to a backend on which the player is logged in.
pub struct BackendLink {
    pub connection: Connection,
}

/// Opens a connection to `address` (`host:port`) and logs `profile` in.
pub fn connect(_address: &str, _profile: &Profile) -> Result<BackendLink> {
    Err(Error::NotImplemented("connecting to a backend"))
}
