//! Meridian's extension protocol (docs/EXTENSIONS.md): what the proxy and an extension send each other
//! over a WebSocket, as JSON-RPC 2.0.
//!
//! This crate holds the wire, and nothing that runs: [`wire`] has the messages of protocol 0 and the
//! JSON-RPC envelope around them. The proxy and the SDK both depend on it, so that the two sides read
//! and write the same shapes (section 8.1). The golden files in `golden/` are the bytes both sides test
//! against (section 8.3).
//!
//! **Protocol 0 is experimental** (section 3): anything in it may change between two releases.

pub mod wire;
