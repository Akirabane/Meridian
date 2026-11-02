//! The sidecar: a small process that runs next to a Horizon server, talks to that
//! server's local control endpoint, and offers the proxy the player requests of a
//! switch (DESIGN.md section 6, decision 8) over a connection signed with a secret
//! the two share.
//!
//! - [`wire`] is the signed exchange between the proxy and a sidecar.
pub mod wire;
