//! The copying of packets between a player and a backend, from the configuration
//! phase to the end of the session.
use crate::{Error, Result, backend::BackendLink, client::ClientSession};

/// Relays the session until either side closes it.
pub fn run(_client: ClientSession, _backend: BackendLink) -> Result<()> {
    Err(Error::NotImplemented("the relay"))
}
