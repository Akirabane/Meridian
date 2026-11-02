//! The sidecar: a small process that runs next to a Horizon server, talks to that
//! server's local control endpoint, and offers the proxy the player requests of a
//! switch (DESIGN.md section 6, decision 8) over a connection signed with a secret
//! the two share.
//!
//! - [`wire`] is the signed exchange between the proxy and a sidecar.
//! - [`request`] holds the player requests and the endpoint command each becomes.
//! - [`endpoint`] talks to the backend's local control endpoint.
//! - [`server`] serves the proxy's requests; [`client`] is the proxy's side.
pub mod client;
pub mod endpoint;
pub mod request;
pub mod server;
pub mod wire;

#[cfg(test)]
mod tests;

use std::fmt;
use std::io;

/// What went wrong between the proxy, the sidecar and the backend.
#[derive(Debug)]
pub enum Error {
    /// The operating system refused, or a connection broke.
    Io(io::Error),
    /// The other side did not answer in time.
    Timeout,
    /// A line of the signed exchange was refused.
    Wire(wire::WireError),
    /// The backend's control endpoint refused or answered something unusable.
    Endpoint(String),
    /// The sidecar refused the request, and said why.
    Refused(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::Timeout => f.write_str("no answer in time"),
            Error::Wire(e) => write!(f, "{e}"),
            Error::Endpoint(why) => f.write_str(why),
            Error::Refused(why) => write!(f, "the sidecar refused: {why}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        // A read timeout: Windows reports TimedOut, Unix WouldBlock.
        match e.kind() {
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => Error::Timeout,
            _ => Error::Io(e),
        }
    }
}

impl From<wire::WireError> for Error {
    fn from(e: wire::WireError) -> Self {
        Error::Wire(e)
    }
}
