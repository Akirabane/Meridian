//! The stand-in backend of a run: it logs each player in as a Horizon server
//! does (with the player information forwarding when the proxy is in front),
//! ends the configuration phase, then sends every measuring packet straight back.
//! It answers at once and does nothing else, so a run measures the proxy and not
//! a game server.
use crate::echo;
use horizon_protocol::connection::Connection;
use horizon_protocol::ids::{configuration as cfg, play};
use meridian_testkit::backend::{FakeBackend, finish_configuration};
use meridian_testkit::body;
use meridian_testkit::peer::Step;
use meridian_testkit::trace::Recorder;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

/// The compression threshold the backend asks for, as a server does by default.
pub const COMPRESSION: i32 = 256;

/// A running backend.
pub struct Backend {
    pub address: SocketAddr,
    /// Logins completed and refused so far.
    pub logged_in: Arc<AtomicU64>,
    pub refused: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Backend {
    /// Starts a backend on 127.0.0.1. With a `secret` it asks every login for the
    /// player information forwarding and refuses one the secret did not sign, as
    /// a server behind the proxy does; without one it takes players directly.
    pub fn start(secret: Option<Vec<u8>>) -> io::Result<Backend> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let address = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let (logged_in, refused) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        let thread = {
            let (stop, logged_in, refused) = (
                Arc::clone(&stop),
                Arc::clone(&logged_in),
                Arc::clone(&refused),
            );
            std::thread::Builder::new()
                .name("bench-backend".into())
                .spawn(move || {
                    // A blocking accept, so a login is taken at once; a stop
                    // wakes it with a connection of its own.
                    loop {
                        let accepted = listener.accept();
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        let Ok((stream, _)) = accepted else {
                            // Out of descriptors, for example: the player's side
                            // sees its connection fail and reports it.
                            std::thread::sleep(Duration::from_millis(1));
                            continue;
                        };
                        let (player_secret, player_logged_in, player_refused) =
                            (secret.clone(), Arc::clone(&logged_in), Arc::clone(&refused));
                        let spawned = std::thread::Builder::new()
                            .name("bench-backend-player".into())
                            .stack_size(256 * 1024)
                            .spawn(move || {
                                let Ok(connection) = Connection::from_stream(stream) else {
                                    return;
                                };
                                if serve(connection, player_secret, &player_logged_in).is_err() {
                                    player_refused.fetch_add(1, Ordering::Relaxed);
                                }
                            });
                        if spawned.is_err() {
                            refused.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                })?
        };
        Ok(Backend {
            address,
            logged_in,
            refused,
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            // Wakes the accept; if even this connection fails, the thread is left
            // to end with the program rather than waited for.
            if TcpStream::connect_timeout(&self.address, Duration::from_secs(1)).is_ok() {
                let _ = thread.join();
            }
        }
    }
}

/// One player: the login, the end of the configuration phase, then the echo until
/// the player leaves. `logged_in` counts the player as soon as the login went
/// through, so a count read while players are still connected is complete.
fn serve(
    mut connection: Connection,
    secret: Option<Vec<u8>>,
    logged_in: &AtomicU64,
) -> Result<(), String> {
    FakeBackend {
        compression: Some(COMPRESSION),
        forwarding_secret: secret,
        online_mode: false,
        after_login: vec![
            finish_configuration(),
            Step::Expect {
                id: cfg::serverbound::FINISH_CONFIGURATION,
            },
        ],
    }
    .run(&mut connection, &Recorder::new())?;
    logged_in.fetch_add(1, Ordering::Relaxed);
    // The login had a short timeout; the play phase waits as long as the player
    // stays, between measuring packets and before the measured window.
    let _ = connection.reader.set_read_timeout(None);
    let mut reply = Vec::new();
    while let Ok((id, payload)) = connection.recv() {
        if id == play::serverbound::CUSTOM_PAYLOAD && echo::is_echo(&payload) {
            reply.clear();
            reply.extend_from_slice(&body(play::clientbound::CUSTOM_PAYLOAD, &payload));
            if connection.send(&reply).is_err() {
                break;
            }
        }
    }
    Ok(())
}
