//! A quiet player for a real server: it connects over TCP, logs in offline,
//! follows the configuration phase, answers what a server needs answered, stays
//! in the world for a number of seconds, and leaves. It is the client of a real
//! run, and it is tested here against the fake backend over a real socket.
//!
//! When the server sends it back to the configuration phase while it plays (Start
//! Configuration, which a proxy does to move a player whose server was lost), it does as
//! a vanilla client does: it acknowledges, sends its information again, goes through the
//! new configuration phase with the code it uses at login, and is back in the world at
//! Finish Configuration. [`Moves`] counts those returns.
use crate::{
    client::FakeClient,
    trace::{Dir, Recorder},
};
use horizon_protocol::{
    connection::Connection,
    ids,
    packets::{Empty, configuration, play},
};
use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};
use std::time::{Duration, Instant};

/// How many times the server took the player back to the configuration phase and the
/// player came out of it, in the world again. Clones count together, so a test keeps one
/// and reads it while the session runs.
#[derive(Clone, Debug, Default)]
pub struct Moves(Arc<AtomicU32>);

impl Moves {
    /// The returns to the world so far.
    pub fn done(&self) -> u32 {
        self.0.load(Ordering::SeqCst)
    }
}

/// Where to connect, who to be, and for how long.
#[derive(Clone, Debug)]
pub struct Options {
    pub host: String,
    pub port: u16,
    pub name: String,
    /// Seconds to stay in the world once the configuration is done.
    pub seconds: u64,
}

/// How a session ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum End {
    /// The client stayed its time and left.
    Clean,
    /// The server sent a disconnect, or refused the login; the text follows.
    Disconnect(String),
    /// The connection was closed with no packet.
    Closed,
    /// Something else went wrong.
    Error(String),
}

/// Runs one session. Every packet the server sent after the login is recorded
/// as going to the client.
pub fn session(o: &Options, recorder: &Recorder) -> End {
    session_counting(o, recorder, &Moves::default())
}

/// As [`session`], and counts in `moves` each return from a configuration phase the server
/// began while the player was in the world.
pub fn session_counting(o: &Options, recorder: &Recorder, moves: &Moves) -> End {
    let mut connection = match Connection::connect(&o.host, o.port, Duration::from_secs(5)) {
        Ok(c) => c,
        Err(e) => return End::Error(format!("cannot connect: {e}")),
    };
    let client = FakeClient {
        host: o.host.clone(),
        port: o.port,
        ..FakeClient::named(&o.name)
    };
    // The login only: the script after it is empty.
    if let Err(e) = client.run(&mut connection, recorder) {
        return End::Disconnect(e);
    }
    let _ = connection
        .reader
        .set_read_timeout(Some(Duration::from_millis(250)));
    let send = |connection: &mut Connection, body: Vec<u8>| connection.send(&body);
    if send(&mut connection, client_information()).is_err() {
        return End::Closed;
    }
    let mut playing = false;
    // The configuration phase the server began while the player played, not the first.
    let mut reconfiguring = false;
    let mut play_started = Instant::now();
    let mut said = false;
    loop {
        if playing {
            let elapsed = play_started.elapsed();
            if elapsed >= Duration::from_secs(o.seconds) {
                connection.writer.shutdown();
                return End::Clean;
            }
            if !said && elapsed >= Duration::from_secs(3) {
                said = true;
                let body = play::Serverbound::ChatCommand(play::ChatCommand {
                    command: "list".into(),
                })
                .encode();
                if send(&mut connection, body).is_err() {
                    return End::Closed;
                }
            }
        }
        let (id, payload) = match connection.recv() {
            Ok(p) => p,
            Err(e) if e.is_timed_out() => continue,
            Err(e) if e.is_end_of_stream() => return End::Closed,
            Err(e) => return End::Error(e.to_string()),
        };
        recorder.record(Dir::ToClient, id, &payload);
        let reply = if playing {
            match id {
                ids::play::clientbound::KEEP_ALIVE => match play::Clientbound::decode(id, &payload)
                {
                    Ok(play::Clientbound::KeepAlive(k)) => Some(
                        play::Serverbound::KeepAlive(configuration::KeepAlive { id: k.id })
                            .encode(),
                    ),
                    _ => None,
                },
                ids::play::clientbound::PING => match play::Clientbound::decode(id, &payload) {
                    Ok(play::Clientbound::Ping(k)) => {
                        Some(play::Serverbound::Pong(configuration::Ping { id: k.id }).encode())
                    }
                    _ => None,
                },
                ids::play::clientbound::CHUNK_BATCH_FINISHED => Some(
                    play::Serverbound::ChunkBatchReceived(play::ChunkBatchReceived {
                        chunks_per_tick: 9.0,
                    })
                    .encode(),
                ),
                ids::play::clientbound::PLAYER_POSITION => {
                    match play::Clientbound::decode(id, &payload) {
                        Ok(play::Clientbound::PlayerPosition(p)) => {
                            let accept =
                                play::Serverbound::AcceptTeleportation(play::AcceptTeleportation {
                                    id: p.teleport,
                                    x: p.position[0],
                                    y: p.position[1],
                                    z: p.position[2],
                                    yaw: p.yaw,
                                    pitch: p.pitch,
                                })
                                .encode();
                            if send(&mut connection, accept).is_err() {
                                return End::Closed;
                            }
                            Some(play::Serverbound::PlayerLoaded(Empty).encode())
                        }
                        _ => None,
                    }
                }
                ids::play::clientbound::DISCONNECT => {
                    return End::Disconnect(disconnect_text(&payload));
                }
                // The server takes the player back to the configuration phase: the
                // acknowledgement is the last thing sent as a player, and the information
                // is the first thing sent in the new phase, as a vanilla client does.
                ids::play::clientbound::START_CONFIGURATION => {
                    playing = false;
                    reconfiguring = true;
                    if send(
                        &mut connection,
                        play::Serverbound::ConfigurationAcknowledged(Empty).encode(),
                    )
                    .is_err()
                    {
                        return End::Closed;
                    }
                    Some(client_information())
                }
                _ => None,
            }
        } else {
            match id {
                ids::configuration::clientbound::KEEP_ALIVE => {
                    match configuration::Clientbound::decode(id, &payload) {
                        Ok(configuration::Clientbound::KeepAlive(k)) => Some(
                            configuration::Serverbound::KeepAlive(configuration::KeepAlive {
                                id: k.id,
                            })
                            .encode(),
                        ),
                        _ => None,
                    }
                }
                ids::configuration::clientbound::PING => {
                    match configuration::Clientbound::decode(id, &payload) {
                        Ok(configuration::Clientbound::Ping(k)) => Some(
                            configuration::Serverbound::Pong(configuration::Ping { id: k.id })
                                .encode(),
                        ),
                        _ => None,
                    }
                }
                ids::configuration::clientbound::SELECT_KNOWN_PACKS => Some(
                    configuration::Serverbound::SelectKnownPacks(configuration::SelectKnownPacks {
                        packs: Vec::new(),
                    })
                    .encode(),
                ),
                ids::configuration::clientbound::FINISH_CONFIGURATION => {
                    playing = true;
                    if std::mem::take(&mut reconfiguring) {
                        moves.0.fetch_add(1, Ordering::SeqCst);
                    } else {
                        play_started = Instant::now();
                    }
                    Some(configuration::Serverbound::FinishConfiguration(Empty).encode())
                }
                ids::configuration::clientbound::DISCONNECT => {
                    return End::Disconnect(disconnect_text(&payload));
                }
                _ => None,
            }
        };
        if let Some(body) = reply
            && send(&mut connection, body).is_err()
        {
            return End::Closed;
        }
    }
}

/// The client's settings, which it sends at the start of each configuration phase.
fn client_information() -> Vec<u8> {
    configuration::Serverbound::ClientInformation(configuration::ClientInformation::new(4)).encode()
}

/// The text of a disconnect packet's reason.
fn disconnect_text(payload: &[u8]) -> String {
    match configuration::Clientbound::decode(ids::configuration::clientbound::DISCONNECT, payload) {
        Ok(configuration::Clientbound::Disconnect(d)) => {
            horizon_protocol::text::plain(&d.reason, &std::collections::HashMap::new())
        }
        _ => format!("{} undecoded bytes", payload.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::FakeBackend;
    use std::net::TcpListener;

    /// The player logs in to a fake backend over a real socket, answers its
    /// configuration and a keep-alive, and leaves cleanly after its time.
    #[test]
    fn a_quiet_player_stays_and_leaves_over_tcp() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Recorder::new();
        let backend = {
            let seen = seen.clone();
            std::thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                let mut connection = Connection::from_stream(stream).unwrap();
                FakeBackend::default()
                    .run(&mut connection, &seen)
                    .map_err(|e| format!("login: {e}"))?;
                connection
                    .reader
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .map_err(|e| e.to_string())?;
                // The configuration phase: the player's information first.
                let mut ids_seen = Vec::new();
                let (id, _) = connection.recv().map_err(|e| e.to_string())?;
                ids_seen.push(id);
                connection
                    .send(&crate::body(
                        ids::configuration::clientbound::FINISH_CONFIGURATION,
                        &[],
                    ))
                    .map_err(|e| e.to_string())?;
                let (id, _) = connection.recv().map_err(|e| e.to_string())?;
                ids_seen.push(id);
                // The play phase: a keep-alive that must come back.
                let mut keep_alive = Vec::new();
                keep_alive.extend_from_slice(&7_i64.to_be_bytes());
                connection
                    .send(&crate::body(
                        ids::play::clientbound::KEEP_ALIVE,
                        &keep_alive,
                    ))
                    .map_err(|e| e.to_string())?;
                // Everything else until the player closes the connection.
                loop {
                    match connection.recv() {
                        Ok((id, payload)) => {
                            if id == ids::play::serverbound::KEEP_ALIVE {
                                if payload != keep_alive {
                                    return Err("the keep-alive came back changed".to_owned());
                                }
                                ids_seen.push(id);
                            }
                        }
                        Err(e) if e.is_end_of_stream() => return Ok(ids_seen),
                        Err(e) => return Err(e.to_string()),
                    }
                }
            })
        };
        let recorder = Recorder::new();
        let end = session(
            &Options {
                host: "127.0.0.1".into(),
                port,
                name: "Quiet".into(),
                seconds: 1,
            },
            &recorder,
        );
        assert_eq!(end, End::Clean);
        let ids_seen = backend.join().unwrap().unwrap();
        assert_eq!(
            ids_seen,
            vec![
                ids::configuration::serverbound::CLIENT_INFORMATION,
                ids::configuration::serverbound::FINISH_CONFIGURATION,
                ids::play::serverbound::KEEP_ALIVE,
            ]
        );
        assert!(recorder.trace().count(Dir::ToClient) >= 2);
    }

    /// The server takes the player back to the configuration phase while they play, as a
    /// proxy does to move a player whose server was lost: the player acknowledges, sends
    /// its information, answers the new configuration, and plays again, still answering
    /// keep-alives.
    #[test]
    fn a_player_goes_back_to_configuration_when_the_server_says_so_and_plays_again() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let backend = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut connection = Connection::from_stream(stream).unwrap();
            FakeBackend::default()
                .run(&mut connection, &Recorder::new())
                .map_err(|e| format!("login: {e}"))?;
            connection
                .reader
                .set_read_timeout(Some(Duration::from_secs(5)))
                .map_err(|e| e.to_string())?;
            let send = |connection: &mut Connection, id: i32, payload: &[u8]| {
                connection
                    .send(&crate::body(id, payload))
                    .map_err(|e| e.to_string())
            };
            let mut seen = Vec::new();
            let mut next = |connection: &mut Connection| -> Result<i32, String> {
                let (id, _) = connection.recv().map_err(|e| e.to_string())?;
                seen.push(id);
                Ok(id)
            };
            // The first configuration phase and the world.
            next(&mut connection)?;
            send(
                &mut connection,
                ids::configuration::clientbound::FINISH_CONFIGURATION,
                &[],
            )?;
            next(&mut connection)?;
            // The server takes the player back to the configuration phase.
            send(
                &mut connection,
                ids::play::clientbound::START_CONFIGURATION,
                &[],
            )?;
            next(&mut connection)?; // the acknowledgement
            next(&mut connection)?; // the information
            send(
                &mut connection,
                ids::configuration::clientbound::SELECT_KNOWN_PACKS,
                &[0],
            )?;
            next(&mut connection)?; // the packs
            send(
                &mut connection,
                ids::configuration::clientbound::FINISH_CONFIGURATION,
                &[],
            )?;
            next(&mut connection)?;
            // The world again: a keep-alive must come back.
            send(
                &mut connection,
                ids::play::clientbound::KEEP_ALIVE,
                &8_i64.to_be_bytes(),
            )?;
            next(&mut connection)?;
            loop {
                match connection.recv() {
                    Err(e) if e.is_end_of_stream() => return Ok(seen),
                    Err(e) => return Err(e.to_string()),
                    Ok(_) => {}
                }
            }
        });
        let moves = Moves::default();
        let end = session_counting(
            &Options {
                host: "127.0.0.1".into(),
                port,
                name: "Quiet".into(),
                seconds: 1,
            },
            &Recorder::new(),
            &moves,
        );
        assert_eq!(end, End::Clean);
        assert_eq!(moves.done(), 1);
        assert_eq!(
            backend.join().unwrap().unwrap(),
            vec![
                ids::configuration::serverbound::CLIENT_INFORMATION,
                ids::configuration::serverbound::FINISH_CONFIGURATION,
                ids::play::serverbound::CONFIGURATION_ACKNOWLEDGED,
                ids::configuration::serverbound::CLIENT_INFORMATION,
                ids::configuration::serverbound::SELECT_KNOWN_PACKS,
                ids::configuration::serverbound::FINISH_CONFIGURATION,
                ids::play::serverbound::KEEP_ALIVE,
            ]
        );
    }

    /// A server that is not there is reported, not waited for. Port 0 is one
    /// nothing can listen on, so no other test can take it in the meantime, as it
    /// could a port freed for the occasion; the system refuses it at once.
    #[test]
    fn a_closed_port_is_an_error() {
        let port = 0;
        let end = session(
            &Options {
                host: "127.0.0.1".into(),
                port,
                name: "Quiet".into(),
                seconds: 1,
            },
            &Recorder::new(),
        );
        assert!(
            matches!(&end, End::Error(why) if why.starts_with("cannot connect")),
            "{end:?}"
        );
    }
}
