//! The copying of packets between a player and a backend, from the configuration
//! phase to the end of the session.
//!
//! Two threads copy whole packets, one each way. Each packet is read with the
//! framing, compression and encryption of the connection it came from and written
//! again with those of the other, so the two sides may compress differently. In this
//! first version nothing is interpreted: keep-alives, the end of the configuration
//! phase and a return to it all pass through as they are.
//!
//! When either side closes, the other is closed too, and both threads end.
use crate::{Result, backend::BackendLink, client::ClientSession};
use horizon_protocol::connection::{Closer, ConnReader, ConnWriter};
use std::sync::mpsc;

/// What went one way through the relay.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flow {
    /// Packets copied.
    pub packets: u64,
    /// Bytes of the packets copied, ids included, before compression and framing.
    pub bytes: u64,
}

/// What a relayed session carried.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// From the backend to the player.
    pub to_client: Flow,
    /// From the player to the backend.
    pub to_backend: Flow,
}

/// Relays the session until either side closes it.
pub fn run(client: ClientSession, backend: BackendLink) -> Result<()> {
    run_counting(client, backend).map(|_| ())
}

/// Relays the session until either side closes it, and says what it carried.
///
/// The session ends well when one side closes its connection, whichever it is. It
/// ends with an error when a connection breaks in another way, for example on a
/// packet that cannot be read.
pub fn run_counting(client: ClientSession, backend: BackendLink) -> Result<Report> {
    let (client_reader, client_writer) = client.connection.split();
    let (backend_reader, backend_writer) = backend.connection.split();
    let closers = [client_reader.closer()?, backend_reader.closer()?];
    let (done, ended) = mpsc::channel();
    let spawn = |name: &str, side: Side, from: ConnReader, to: ConnWriter| {
        let done = done.clone();
        std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                let _ = done.send((side, copy(from, to)));
            })
    };
    let to_client = spawn(
        "relay-to-client",
        Side::ToClient,
        backend_reader,
        client_writer,
    )?;
    let to_backend = spawn(
        "relay-to-backend",
        Side::ToBackend,
        client_reader,
        backend_writer,
    )?;
    drop(done);
    // The first thread to stop says how the session ended; closing both
    // connections then stops the other one, whose error is only the echo.
    let mut first = None;
    let mut report = Report::default();
    for (side, (flow, end)) in ended.iter() {
        if first.is_none() {
            close(&closers);
            first = Some(end);
        }
        match side {
            Side::ToClient => report.to_client = flow,
            Side::ToBackend => report.to_backend = flow,
        }
    }
    let _ = to_client.join();
    let _ = to_backend.join();
    match first {
        Some(End::Closed) | None => Ok(report),
        Some(End::Broken(e)) => Err(e.into()),
    }
}

#[derive(Clone, Copy)]
enum Side {
    ToClient,
    ToBackend,
}

/// How one direction stopped.
enum End {
    /// The side it reads from closed, or the side it writes to did.
    Closed,
    /// The side it reads from sent something that could not be read.
    Broken(horizon_protocol::Error),
}

/// Copies packets until reading or writing fails.
fn copy(mut from: ConnReader, mut to: ConnWriter) -> (Flow, End) {
    let mut flow = Flow::default();
    let mut body = Vec::new();
    loop {
        let (id, payload) = match from.recv() {
            Ok(packet) => packet,
            Err(e) if e.is_end_of_stream() => return (flow, End::Closed),
            Err(e) => return (flow, End::Broken(e)),
        };
        body.clear();
        horizon_protocol::buf::write_varint(&mut body, id);
        body.extend_from_slice(&payload);
        // A write fails only when the side written to has gone away.
        if to.send(&body).is_err() {
            return (flow, End::Closed);
        }
        flow.packets += 1;
        flow.bytes += body.len() as u64;
    }
}

fn close(closers: &[Closer]) {
    for closer in closers {
        closer.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Profile;
    use horizon_protocol::{connection::Connection, ids};
    use meridian_testkit::{
        backend::{acknowledge_configuration, finish_configuration},
        body, peer,
        peer::Step,
        pipe,
        trace::{Dir, Recorder},
    };

    /// A player and a backend, each joined to the proxy by a pipe, with the given
    /// compression on each side. Returns the proxy's ends and the peers' ends.
    fn sides(
        client_compression: i32,
        backend_compression: i32,
    ) -> (ClientSession, BackendLink, Connection, Connection) {
        let (mut proxy_client, mut player) = pipe();
        let (mut proxy_backend, mut server) = pipe();
        for (a, b, threshold) in [
            (&mut proxy_client, &mut player, client_compression),
            (&mut proxy_backend, &mut server, backend_compression),
        ] {
            a.set_compression(threshold);
            b.set_compression(threshold);
        }
        let client = ClientSession {
            profile: Profile {
                name: "Alex".into(),
                uuid: [1; 16],
                properties: Vec::new(),
            },
            auth: crate::client::AuthKind::Offline,
            connection: proxy_client,
        };
        (
            client,
            BackendLink {
                connection: proxy_backend,
            },
            player,
            server,
        )
    }

    fn sent(steps: &[Step]) -> Vec<(i32, Vec<u8>)> {
        steps
            .iter()
            .filter_map(|s| match s {
                Step::Send { id, payload } => Some((*id, payload.clone())),
                _ => None,
            })
            .collect()
    }

    fn received(recorder: &Recorder, dir: Dir) -> Vec<(i32, Vec<u8>)> {
        recorder
            .trace()
            .packets(dir)
            .map(|p| (p.id, p.payload.clone()))
            .collect()
    }

    /// A whole session through the relay: the configuration phase, the play phase
    /// with keep-alives both ways and a packet larger than one read, then the
    /// backend closes. Each side receives exactly what the other sent, in order,
    /// although the two sides compress differently.
    #[test]
    fn a_session_is_relayed_unchanged_both_ways() {
        use ids::{configuration as cfg, play};
        let (client, backend, player, server) = sides(256, 64);
        let large: Vec<u8> = (0..200_000).map(|i| (i % 251) as u8).collect();
        let backend_script = vec![
            Step::send(cfg::clientbound::REGISTRY_DATA, vec![7; 1000]),
            Step::send(cfg::clientbound::KEEP_ALIVE, 42u64.to_be_bytes()),
            Step::expect_exactly(cfg::serverbound::KEEP_ALIVE, 42u64.to_be_bytes()),
            finish_configuration(),
            Step::Expect {
                id: cfg::serverbound::FINISH_CONFIGURATION,
            },
            Step::send(play::clientbound::LOGIN, vec![3; 40]),
            Step::send(play::clientbound::KEEP_ALIVE, 43u64.to_be_bytes()),
            Step::expect_exactly(play::serverbound::KEEP_ALIVE, 43u64.to_be_bytes()),
            Step::send(play::clientbound::LOGIN, large.clone()),
            Step::Close,
        ];
        let player_script = vec![
            Step::expect_exactly(cfg::clientbound::REGISTRY_DATA, vec![7; 1000]),
            Step::expect_exactly(cfg::clientbound::KEEP_ALIVE, 42u64.to_be_bytes()),
            Step::send(cfg::serverbound::KEEP_ALIVE, 42u64.to_be_bytes()),
            Step::Expect {
                id: cfg::clientbound::FINISH_CONFIGURATION,
            },
            acknowledge_configuration(),
            Step::Expect {
                id: play::clientbound::LOGIN,
            },
            Step::expect_exactly(play::clientbound::KEEP_ALIVE, 43u64.to_be_bytes()),
            Step::send(play::serverbound::KEEP_ALIVE, 43u64.to_be_bytes()),
            Step::expect_exactly(play::clientbound::LOGIN, large),
            Step::ExpectClose,
        ];
        let recorder = Recorder::new();
        let server = peer::spawn(
            server,
            backend_script.clone(),
            Dir::ToServer,
            recorder.clone(),
        );
        let player = peer::spawn(
            player,
            player_script.clone(),
            Dir::ToClient,
            recorder.clone(),
        );
        let report = run_counting(client, backend).unwrap();
        server.join().unwrap().unwrap();
        player.join().unwrap().unwrap();
        assert_eq!(received(&recorder, Dir::ToClient), sent(&backend_script));
        assert_eq!(received(&recorder, Dir::ToServer), sent(&player_script));
        let bytes = |packets: Vec<(i32, Vec<u8>)>| {
            packets
                .iter()
                .map(|(id, payload)| body(*id, payload).len() as u64)
                .sum()
        };
        assert_eq!(
            report,
            Report {
                to_client: Flow {
                    packets: 6,
                    bytes: bytes(sent(&backend_script)),
                },
                to_backend: Flow {
                    packets: 3,
                    bytes: bytes(sent(&player_script)),
                },
            }
        );
    }

    /// The player leaves: the backend's connection is closed too.
    #[test]
    fn the_player_leaving_closes_the_backend() {
        let (client, backend, player, server) = sides(-1, -1);
        let recorder = Recorder::new();
        let server = peer::spawn(
            server,
            vec![Step::Expect { id: 5 }, Step::ExpectClose],
            Dir::ToServer,
            recorder.clone(),
        );
        let player = peer::spawn(
            player,
            vec![Step::send(5, [1]), Step::Close],
            Dir::ToClient,
            recorder.clone(),
        );
        let report = run_counting(client, backend).unwrap();
        server.join().unwrap().unwrap();
        player.join().unwrap().unwrap();
        assert_eq!(report.to_backend.packets, 1);
        assert_eq!(report.to_client.packets, 0);
    }

    /// A frame that cannot be read ends the session with an error, and the player
    /// is disconnected.
    #[test]
    fn an_unreadable_frame_breaks_the_session() {
        let (client, backend, player, mut server) = sides(-1, -1);
        // A length prefix longer than a VarInt may be.
        server
            .writer
            .write_raw(&[0xff, 0xff, 0xff, 0xff, 0xff, 0x01])
            .unwrap();
        let player = peer::spawn(
            player,
            vec![Step::ExpectClose],
            Dir::ToClient,
            Recorder::new(),
        );
        assert!(run_counting(client, backend).is_err());
        player.join().unwrap().unwrap();
        drop(server);
    }

    #[test]
    fn run_reports_a_clean_end_as_ok() {
        let (client, backend, player, server) = sides(-1, -1);
        drop(server);
        assert!(run(client, backend).is_ok());
        drop(player);
    }
}
