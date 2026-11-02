//! The connections of the proxy. A player's side is in [`client`], a backend's
//! side in [`backend`], and the copying of packets between them in [`relay`]. The
//! signatures here are the agreed shape of milestone J1; each module is filled in
//! by its own change.
pub mod backend;
pub mod client;
pub mod login_policy;
pub mod relay;

use std::fmt;

/// What went wrong on a connection.
#[derive(Debug)]
pub enum Error {
    /// The step is not written yet.
    NotImplemented(&'static str),
    /// The protocol layer refused the packet or lost the connection.
    Protocol(horizon_protocol::Error),
    /// The operating system refused.
    Io(std::io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotImplemented(what) => write!(f, "{what} is not implemented yet"),
            Error::Protocol(e) => write!(f, "{e}"),
            Error::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<horizon_protocol::Error> for Error {
    fn from(e: horizon_protocol::Error) -> Self {
        Error::Protocol(e)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Who a player is, as the proxy learned it at login.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    pub name: String,
    /// The unique id, 16 bytes.
    pub uuid: [u8; 16],
    /// Profile properties as `(name, value, signature)`; the signature is empty
    /// when there is none.
    pub properties: Vec<(String, String, String)>,
}
