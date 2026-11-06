//! Meridian's extension protocol (docs/EXTENSIONS.md): what the proxy and an extension send each other
//! over a WebSocket, as JSON-RPC 2.0.
//!
//! This crate holds what both sides share and the SDK of an extension. [`wire`] has the messages of
//! protocol 0 and the JSON-RPC envelope around them, and [`handshake`] the proofs that open a connection.
//! The proxy and the SDK both depend on it, so that the two sides read and write the same shapes
//! (section 8.1). The golden files in `golden/` are the bytes both sides test against (section 8.3).
//! [`decide`] has the decision points as both sides read and write them. [`serve`] is the extension's end: it listens on the loopback, does the hello and answers the proxy.
//! [`text`] makes what the other side says safe to log.
//!
//! **Protocol 0 is experimental** (section 3): anything in it may change between two releases.

pub mod decide;
pub mod handshake;
pub mod serve;
pub mod text;
pub mod wire;
