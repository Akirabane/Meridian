//! The accept loop: one thread per player, until asked to stop.
use crate::log::{debug, info, warning};
use horizon_protocol::connection::Connection;
use meridian_config::Config;
use meridian_net::client;
use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

/// How often the loop looks at the stop flag while no one connects.
const POLL: Duration = Duration::from_millis(50);
/// How long a stop waits for players' threads to finish.
const DRAIN: Duration = Duration::from_secs(5);

/// Accepts players on `listener` until `stop` is set, then waits up to
/// [`DRAIN`] for the players' threads to end.
pub fn serve(listener: TcpListener, config: Arc<Config>, stop: &AtomicBool) -> io::Result<()> {
    // A blocking accept could not see the stop flag, so the listener polls.
    listener.set_nonblocking(true)?;
    let active = Arc::new(AtomicUsize::new(0));
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, peer)) => start_player(stream, peer, &config, &active),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => thread::sleep(POLL),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => {
                // For example too many open files: wait instead of spinning.
                warning!("cannot accept a connection: {e}");
                thread::sleep(POLL);
            }
        }
    }
    info!("stopping: no new connections are accepted");
    drain(&active);
    Ok(())
}

fn start_player(
    stream: TcpStream,
    peer: SocketAddr,
    config: &Arc<Config>,
    active: &Arc<AtomicUsize>,
) {
    debug!("{peer}: connected");
    // On some systems an accepted socket inherits the listener's non-blocking mode.
    if let Err(e) = stream.set_nonblocking(false) {
        warning!("{peer}: cannot set up the connection: {e}");
        return;
    }
    let config = Arc::clone(config);
    let guard = Active::enter(active);
    let spawned = thread::Builder::new()
        .name(format!("player {peer}"))
        .spawn(move || {
            let _guard = guard;
            handle(stream, peer, &config);
        });
    if let Err(e) = spawned {
        warning!("{peer}: cannot start a thread for the connection: {e}");
    }
}

/// One player's connection, from the first byte to the close.
fn handle(stream: TcpStream, peer: SocketAddr, config: &Config) {
    let connection = match Connection::from_stream(stream) {
        Ok(connection) => connection,
        Err(e) => {
            warning!("{peer}: cannot set up the connection: {e}");
            return;
        }
    };
    match client::accept(connection, config) {
        Ok(Some(session)) => {
            // Relaying to a backend is a later step; until then the session ends here.
            info!(
                "{peer}: {} logged in; relaying is not available yet, closing",
                session.profile.name
            );
        }
        Ok(None) => debug!("{peer}: status answered"),
        Err(e) => warning!("{peer}: {e}; closing the connection"),
    }
    // The connection is dropped here, which closes the socket.
}

/// Waits for the players' threads, up to [`DRAIN`].
fn drain(active: &AtomicUsize) {
    let deadline = Instant::now() + DRAIN;
    while active.load(Ordering::Relaxed) > 0 && Instant::now() < deadline {
        thread::sleep(POLL);
    }
    match active.load(Ordering::Relaxed) {
        0 => info!("all connections are closed"),
        n => warning!("{n} connections are still open; they close when the proxy exits"),
    }
}

/// Counts a player's thread while it runs.
struct Active(Arc<AtomicUsize>);

impl Active {
    fn enter(count: &Arc<AtomicUsize>) -> Self {
        count.fetch_add(1, Ordering::Relaxed);
        Active(Arc::clone(count))
    }
}

impl Drop for Active {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use meridian_config::Backend;
    use std::io::Read;

    fn config() -> Arc<Config> {
        Arc::new(Config {
            backends: vec![Backend {
                name: "lobby".into(),
                address: "127.0.0.1:25566".into(),
            }],
            default_backend: "lobby".into(),
            ..Config::default()
        })
    }

    #[test]
    fn a_player_is_closed_cleanly_and_the_loop_stops_when_asked() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let server = {
            let stop = Arc::clone(&stop);
            thread::spawn(move || serve(listener, config(), &stop))
        };

        // Accepting a player is not written yet, so the proxy closes the
        // connection: the client reads the end of the stream.
        let mut player = TcpStream::connect(address).unwrap();
        player
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut rest = Vec::new();
        assert_eq!(player.read_to_end(&mut rest).unwrap(), 0);

        stop.store(true, Ordering::Relaxed);
        server.join().unwrap().unwrap();
    }

    #[test]
    fn the_active_count_follows_the_guards() {
        let count = Arc::new(AtomicUsize::new(0));
        let a = Active::enter(&count);
        let b = Active::enter(&count);
        assert_eq!(count.load(Ordering::Relaxed), 2);
        drop(a);
        drop(b);
        assert_eq!(count.load(Ordering::Relaxed), 0);
    }
}
