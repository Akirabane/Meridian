//! The accept loop: one thread per player, until asked to stop. A stop ends the
//! sessions still being relayed, telling each player the server closed, so that
//! every backend sees its player leave and saves them, then waits for the
//! players' threads to end.
use crate::log::{debug, info, warning};
use crate::session;
use crate::state::Proxy;
use horizon_protocol::connection::Connection;
use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

/// How often the loop looks at the stop flag while no one connects.
const POLL: Duration = Duration::from_millis(50);
/// How long a stop waits for players' threads to finish.
const DRAIN: Duration = Duration::from_secs(5);

/// Accepts players on `listener` until the proxy is asked to stop, then ends the
/// sessions being relayed and waits up to [`DRAIN`] for the players' threads.
pub fn serve(listener: TcpListener, proxy: &Arc<Proxy>) -> io::Result<()> {
    // A blocking accept could not see the stop flag, so the listener polls.
    listener.set_nonblocking(true)?;
    let active = Arc::new(AtomicUsize::new(0));
    while !proxy.stopping() {
        match listener.accept() {
            Ok((stream, peer)) => start_player(stream, peer, proxy, &active),
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
    match proxy.sessions.close_all() {
        0 => {}
        1 => info!("closing 1 session"),
        n => info!("closing {n} sessions"),
    }
    drain(&active);
    Ok(())
}

fn start_player(
    stream: TcpStream,
    peer: SocketAddr,
    proxy: &Arc<Proxy>,
    active: &Arc<AtomicUsize>,
) {
    debug!("{peer}: connected");
    // On some systems an accepted socket inherits the listener's non-blocking mode.
    if let Err(e) = stream.set_nonblocking(false) {
        warning!("{peer}: cannot set up the connection: {e}");
        return;
    }
    let proxy = Arc::clone(proxy);
    let guard = Active::enter(active);
    let spawned = thread::Builder::new()
        .name(format!("player {peer}"))
        .spawn(move || {
            let _guard = guard;
            handle(stream, peer, &proxy);
        });
    if let Err(e) = spawned {
        warning!("{peer}: cannot start a thread for the connection: {e}");
    }
}

/// One player's connection, from the first byte to the close. The configuration
/// is read once here: a reload applies to the connections that come after it.
fn handle(stream: TcpStream, peer: SocketAddr, proxy: &Proxy) {
    let connection = match Connection::from_stream(stream) {
        Ok(connection) => connection,
        Err(e) => {
            warning!("{peer}: cannot set up the connection: {e}");
            return;
        }
    };
    let config = proxy.config();
    session::handle(connection, peer, &config, &*proxy.connect, &proxy.sessions);
    // The connections are dropped here, which closes the sockets.
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
    use meridian_config::{Backend, Config};
    use std::io::Read;
    use std::sync::atomic::AtomicBool;

    fn proxy() -> Arc<Proxy> {
        let config = Config {
            backends: vec![Backend {
                name: "lobby".into(),
                address: "127.0.0.1:25566".into(),
            }],
            default_backend: "lobby".into(),
            ..Config::default()
        };
        Arc::new(Proxy::new(
            config,
            "meridian.toml".into(),
            Arc::new(AtomicBool::new(false)),
        ))
    }

    #[test]
    fn a_player_is_closed_cleanly_and_the_loop_stops_when_asked() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let proxy = proxy();
        let server = {
            let proxy = Arc::clone(&proxy);
            thread::spawn(move || serve(listener, &proxy))
        };

        // A player who closes before the handshake: the proxy gives up on the
        // connection and closes it, and the client reads the end of the stream.
        let mut player = TcpStream::connect(address).unwrap();
        player
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        player.shutdown(std::net::Shutdown::Write).unwrap();
        let mut rest = Vec::new();
        assert_eq!(player.read_to_end(&mut rest).unwrap(), 0);

        proxy.stop();
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
