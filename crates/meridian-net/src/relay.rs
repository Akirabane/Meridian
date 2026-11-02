//! The copying of packets between a player and a backend, from the configuration
//! phase to the end of the session.
//!
//! Two threads copy whole packets, one each way. Each packet is read with the
//! framing, compression and encryption of the connection it came from and written
//! again with those of the other, so the two sides may compress differently. In this
//! first version nothing is interpreted: keep-alives, the end of the configuration
//! phase and a return to it all pass through as they are. The relay only notes
//! which of those two phases the player's client is in, from the packets it sends
//! it, so that a [`Handle`] can tell the player why the session ends.
//!
//! When either side closes, the other is closed too, and both threads end.
use crate::{Result, backend::BackendLink, client::ClientSession};
use horizon_protocol::{
    connection::{Closer, ConnReader, ConnWriter},
    ids,
    packets::{configuration, play},
};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};

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

/// The protocol phase the player's client reads packets in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Configuration,
    Play,
}

/// Relays the session until either side closes it.
pub fn run(client: ClientSession, backend: BackendLink) -> Result<()> {
    run_counting(client, backend).map(|_| ())
}

/// Relays the session until either side closes it, and says what it carried.
pub fn run_counting(client: ClientSession, backend: BackendLink) -> Result<Report> {
    let (relay, _handle) = Relay::new(client, backend)?;
    relay.run()
}

/// A session ready to be relayed. [`Relay::new`] also gives the [`Handle`] that
/// ends it from another thread.
pub struct Relay {
    client_reader: ConnReader,
    backend_reader: ConnReader,
    backend_writer: ConnWriter,
    shared: Arc<Shared>,
}

/// Ends a relayed session from another thread. It can be cloned and kept after the
/// session is over, when it does nothing.
#[derive(Clone)]
pub struct Handle(Arc<Shared>);

struct Shared {
    /// What the player is sent, with the phase it is read in: one lock, so a
    /// disconnect is written in the phase of the packets before it.
    client: Mutex<ToClient>,
    closers: [Closer; 2],
}

struct ToClient {
    writer: ConnWriter,
    phase: Phase,
}

impl Relay {
    /// Takes over both connections. The player's client is in the configuration
    /// phase, as [`crate::client::accept`] and [`crate::backend::connect`] leave
    /// both sides.
    pub fn new(client: ClientSession, backend: BackendLink) -> Result<(Relay, Handle)> {
        let (client_reader, client_writer) = client.connection.split();
        let (backend_reader, backend_writer) = backend.connection.split();
        let closers = [client_reader.closer()?, backend_reader.closer()?];
        let shared = Arc::new(Shared {
            client: Mutex::new(ToClient {
                writer: client_writer,
                phase: Phase::Configuration,
            }),
            closers,
        });
        let handle = Handle(Arc::clone(&shared));
        let relay = Relay {
            client_reader,
            backend_reader,
            backend_writer,
            shared,
        };
        Ok((relay, handle))
    }

    /// Relays the session until either side closes it, and says what it carried.
    ///
    /// The session ends well when one side closes its connection, whichever it is,
    /// or when a [`Handle`] ends it. It ends with an error when a connection breaks
    /// in another way, for example on a packet that cannot be read.
    pub fn run(self) -> Result<Report> {
        let Relay {
            client_reader,
            backend_reader,
            backend_writer,
            shared,
        } = self;
        let (done, ended) = mpsc::channel();
        let to_client = {
            let (done, shared) = (done.clone(), Arc::clone(&shared));
            std::thread::Builder::new()
                .name("relay-to-client".to_owned())
                .spawn(move || {
                    let _ = done.send((Side::ToClient, copy_to_client(backend_reader, &shared)));
                })?
        };
        let to_backend = {
            let done = done.clone();
            std::thread::Builder::new()
                .name("relay-to-backend".to_owned())
                .spawn(move || {
                    let _ = done.send((Side::ToBackend, copy(client_reader, backend_writer)));
                })?
        };
        drop(done);
        // The first thread to stop says how the session ended; closing both
        // connections then stops the other one, whose error is only the echo.
        let mut first = None;
        let mut report = Report::default();
        for (side, (flow, end)) in ended.iter() {
            if first.is_none() {
                shared.close();
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
}

impl Handle {
    /// The phase the player's client is in, as far as the relay has sent it.
    pub fn phase(&self) -> Phase {
        self.0.client().phase
    }

    /// Tells the player why the session ends, with the disconnect packet of the
    /// phase their client is in, then closes both sides. The reason is the same in
    /// both phases: a text component, as `horizon_protocol::text` builds it.
    pub fn disconnect(&self, reason: configuration::Disconnect) {
        {
            let mut client = self.0.client();
            let body = match client.phase {
                Phase::Configuration => configuration::Clientbound::Disconnect(reason).encode(),
                Phase::Play => play::Clientbound::Disconnect(reason).encode(),
            };
            // A player who has already gone cannot be told; the close follows anyway.
            let _ = client.writer.send(&body);
        }
        self.close();
    }

    /// Closes both sides without a word.
    pub fn close(&self) {
        self.0.close();
    }
}

impl Shared {
    fn client(&self) -> MutexGuard<'_, ToClient> {
        self.client.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn close(&self) {
        for closer in &self.closers {
            closer.close();
        }
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

/// Copies the player's packets to the backend until reading or writing fails.
fn copy(mut from: ConnReader, mut to: ConnWriter) -> (Flow, End) {
    let mut flow = Flow::default();
    let mut body = Vec::new();
    loop {
        let (id, payload) = match from.recv() {
            Ok(packet) => packet,
            Err(e) if e.is_end_of_stream() => return (flow, End::Closed),
            Err(e) => return (flow, End::Broken(e)),
        };
        encode(&mut body, id, &payload);
        // A write fails only when the side written to has gone away.
        if to.send(&body).is_err() {
            return (flow, End::Closed);
        }
        flow.add(&body);
    }
}

/// Copies the backend's packets to the player, and follows the phase the
/// player's client reads them in.
fn copy_to_client(mut from: ConnReader, shared: &Shared) -> (Flow, End) {
    let mut flow = Flow::default();
    let mut body = Vec::new();
    loop {
        let (id, payload) = match from.recv() {
            Ok(packet) => packet,
            Err(e) if e.is_end_of_stream() => return (flow, End::Closed),
            Err(e) => return (flow, End::Broken(e)),
        };
        encode(&mut body, id, &payload);
        let mut client = shared.client();
        if client.writer.send(&body).is_err() {
            return (flow, End::Closed);
        }
        // The packet that ends a phase is the last one read in it.
        client.phase = match (client.phase, id) {
            (Phase::Configuration, ids::configuration::clientbound::FINISH_CONFIGURATION) => {
                Phase::Play
            }
            (Phase::Play, ids::play::clientbound::START_CONFIGURATION) => Phase::Configuration,
            (phase, _) => phase,
        };
        drop(client);
        flow.add(&body);
    }
}

fn encode(body: &mut Vec<u8>, id: i32, payload: &[u8]) {
    body.clear();
    horizon_protocol::buf::write_varint(body, id);
    body.extend_from_slice(payload);
}

impl Flow {
    fn add(&mut self, body: &[u8]) {
        self.packets += 1;
        self.bytes += body.len() as u64;
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
            virtual_host: None,
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

    /// A disconnect is written in the phase the player's client reads: the
    /// configuration phase's packet before Finish Configuration, the play phase's
    /// after it, and the configuration phase's again after Start Configuration.
    /// Both sides are closed after it, and the session ends well.
    #[test]
    fn a_disconnect_is_written_in_the_players_phase() {
        use horizon_protocol::{packets::configuration, text};
        use ids::{configuration as cfg, play};
        let back_to_configuration = Step::send(play::clientbound::START_CONFIGURATION, []);
        for (backend_sends, phase, disconnect) in [
            (vec![], Phase::Configuration, cfg::clientbound::DISCONNECT),
            (
                vec![finish_configuration()],
                Phase::Play,
                play::clientbound::DISCONNECT,
            ),
            (
                vec![finish_configuration(), back_to_configuration],
                Phase::Configuration,
                cfg::clientbound::DISCONNECT,
            ),
        ] {
            let (client, backend, mut player, server) = sides(-1, -1);
            let (relay, handle) = Relay::new(client, backend).unwrap();
            let relay = std::thread::spawn(move || relay.run());
            let expected: Vec<Step> = sent(&backend_sends)
                .into_iter()
                .map(|(id, payload)| Step::expect_exactly(id, payload))
                .collect();
            let mut backend_script = backend_sends;
            backend_script.push(Step::ExpectClose);
            let server = peer::spawn(server, backend_script, Dir::ToServer, Recorder::new());
            let recorder = Recorder::new();
            peer::run(&mut player, &expected, Dir::ToClient, &recorder).unwrap();
            assert_eq!(handle.phase(), phase);
            handle.disconnect(configuration::Disconnect {
                reason: text::literal("bye"),
            });
            peer::run(
                &mut player,
                &[Step::Expect { id: disconnect }, Step::ExpectClose],
                Dir::ToClient,
                &recorder,
            )
            .unwrap();
            let last = recorder.trace().packets.last().cloned().unwrap();
            let reason = match phase {
                Phase::Configuration => {
                    match configuration::Clientbound::decode(last.id, &last.payload) {
                        Ok(configuration::Clientbound::Disconnect(d)) => d.reason,
                        other => panic!("{other:?}"),
                    }
                }
                Phase::Play => match horizon_protocol::packets::play::Clientbound::decode(
                    last.id,
                    &last.payload,
                ) {
                    Ok(horizon_protocol::packets::play::Clientbound::Disconnect(d)) => d.reason,
                    other => panic!("{other:?}"),
                },
            };
            assert_eq!(text::plain(&reason, &Default::default()), "bye");
            assert!(relay.join().unwrap().is_ok());
            server.join().unwrap().unwrap();
        }
    }

    /// Closing ends the session without a packet to the player.
    #[test]
    fn a_handle_closes_a_session_without_a_word() {
        let (client, backend, player, server) = sides(-1, -1);
        let (relay, handle) = Relay::new(client, backend).unwrap();
        let relay = std::thread::spawn(move || relay.run());
        let server = peer::spawn(
            server,
            vec![Step::ExpectClose],
            Dir::ToServer,
            Recorder::new(),
        );
        let player = peer::spawn(
            player,
            vec![Step::ExpectClose],
            Dir::ToClient,
            Recorder::new(),
        );
        handle.close();
        assert!(relay.join().unwrap().is_ok());
        server.join().unwrap().unwrap();
        player.join().unwrap().unwrap();
        // A handle kept after the session does nothing.
        handle.close();
    }
}
