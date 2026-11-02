//! Helpers for tests that need no real game: a scripted client and backend that
//! talk over in-memory pipes, and a recorder that keeps the ordered packet ids and
//! bytes of a session so that two sessions can be compared.
//!
//! - [`pipe`] gives two connected ends, each a protocol [`Connection`].
//! - [`trace`] records packets in order, from several threads at once.
//! - [`peer`] runs a script of packets to send and to expect on one end.
//! - [`backend`] takes a player through the login as a Horizon server does, with
//!   compression and the player information forwarding, then runs a script.
//! - [`client`] logs in as an offline vanilla client does, then runs a script.
pub mod backend;
pub mod client;
pub mod peer;
pub mod trace;

use horizon_protocol::connection::{Connection, Pipe};

/// Two connections joined by an in-memory pipe: what one sends the other receives.
pub fn pipe() -> (Connection, Connection) {
    let (a, b) = Pipe::pair();
    (Connection::local(a), Connection::local(b))
}

/// A packet body: the id as a VarInt, then the payload.
pub fn body(id: i32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 5);
    horizon_protocol::buf::write_varint(&mut out, id);
    out.extend_from_slice(payload);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pipe_carries_packets_both_ways() {
        let (mut a, mut b) = pipe();
        a.send(&body(3, &[1, 2])).unwrap();
        assert_eq!(b.recv().unwrap(), (3, vec![1, 2]));
        b.send(&body(300, &[])).unwrap();
        assert_eq!(a.recv().unwrap(), (300, vec![]));
    }
}
