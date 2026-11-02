//! The accept loop: one thread per player, until asked to stop. A stop ends the
//! sessions still being relayed, telling each player the server closed, so that
//! every backend sees its player leave and saves them, then waits for the
//! players' threads to end.
use crate::log::{debug, info, warning};
use crate::session;
use crate::state::Proxy;
use horizon_protocol::connection::Connection;
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

/// How often the stop flag is looked at, and how long the loop waits after an
/// error before accepting again.
const POLL: Duration = Duration::from_millis(50);
/// How long a stop waits for players' threads to finish.
const DRAIN: Duration = Duration::from_secs(5);
/// How many connections may wait in the listener's queue for the accept loop.
/// A burst of logins larger than the queue is refused by the system, and the
/// client's system tries again about every half second; std's listener has 128.
const BACKLOG: i32 = 4096;

/// Listens on `address` with a queue of [`BACKLOG`] connections.
///
/// Windows holds a plain queue length to 200, whatever is asked; the length is
/// given as `SOMAXCONN_HINT(BACKLOG)`, which Windows writes as its negative,
/// for it to be kept. Elsewhere the system may lower it to its own maximum
/// (`net.core.somaxconn` on Linux). Like std, the address is made reusable at
/// once on Unix, and not on Windows, where that would let another program take
/// the port.
pub fn listen(address: SocketAddr) -> io::Result<TcpListener> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(
        Domain::for_address(address),
        Type::STREAM,
        Some(Protocol::TCP),
    )?;
    #[cfg(not(windows))]
    socket.set_reuse_address(true)?;
    socket.bind(&address.into())?;
    socket.listen(if cfg!(windows) { -BACKLOG } else { BACKLOG })?;
    Ok(socket.into())
}

/// Accepts players on `listener` until the proxy is asked to stop, then ends the
/// sessions being relayed and waits up to [`DRAIN`] for the players' threads.
pub fn serve(listener: TcpListener, proxy: &Arc<Proxy>) -> io::Result<()> {
    // The accept blocks, so a player is taken the moment they connect; a stop
    // wakes it with a connection of its own (see `waker`). Without the waker
    // nothing could wake it, and the loop polls the listener instead.
    let ended = Arc::new(AtomicBool::new(false));
    let waker = waker(wake_address(listener.local_addr()?), proxy, &ended);
    listener.set_nonblocking(waker.is_none())?;
    let active = Arc::new(AtomicUsize::new(0));
    loop {
        let accepted = listener.accept();
        // A connection taken after a stop, the waker's or a player's, is
        // closed unread: no new connections are accepted.
        if proxy.stopping() {
            break;
        }
        match accepted {
            Ok((stream, peer)) => {
                let per_minute = proxy.config().connections_per_minute;
                if proxy.throttle.allow(peer.ip(), per_minute, Instant::now()) {
                    start_player(stream, peer, proxy, &active);
                } else {
                    // Closed before a byte is read: the cheapest answer, and a
                    // client in a loop learns nothing more from a reason.
                    debug!("{peer}: over {per_minute} connections this minute; closed");
                    drop(stream);
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => thread::sleep(POLL),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => {
                // For example too many open files: wait instead of spinning.
                warning!("cannot accept a connection: {e}");
                thread::sleep(POLL);
            }
        }
    }
    ended.store(true, Ordering::Relaxed);
    let _ = waker.map(thread::JoinHandle::join);
    info!("stopping: no new connections are accepted");
    match proxy.sessions.close_all() {
        0 => {}
        1 => info!("closing 1 session"),
        n => info!("closing {n} sessions"),
    }
    drain(&active);
    Ok(())
}

/// Watches the stop flag, which Ctrl+C and the console set, and once it is set
/// opens connections to the listener until the accept loop has ended, to wake
/// its blocking accept. Watching the flag costs a player nothing, unlike the
/// loop polling the listener, which made each login wait up to [`POLL`].
fn waker(
    address: SocketAddr,
    proxy: &Arc<Proxy>,
    ended: &Arc<AtomicBool>,
) -> Option<thread::JoinHandle<()>> {
    let (proxy, ended) = (Arc::clone(proxy), Arc::clone(ended));
    let spawned = thread::Builder::new()
        .name("accept waker".into())
        .spawn(move || {
            while !proxy.stopping() && !ended.load(Ordering::Relaxed) {
                thread::sleep(POLL);
            }
            let mut warned = false;
            while !ended.load(Ordering::Relaxed) {
                if let Err(e) = TcpStream::connect_timeout(&address, Duration::from_secs(1))
                    && !warned
                {
                    warning!("cannot wake the accept loop on {address} to stop: {e}");
                    warned = true;
                }
                thread::sleep(POLL);
            }
        });
    match spawned {
        Ok(handle) => Some(handle),
        Err(e) => {
            warning!("cannot start the thread that stops the accept loop: {e}");
            None
        }
    }
}

/// Where the waker connects: the listener's own address, on the loopback when
/// it listens on every interface.
fn wake_address(listening: SocketAddr) -> SocketAddr {
    let mut address = listening;
    if address.ip().is_unspecified() {
        address.set_ip(match address {
            SocketAddr::V4(_) => Ipv4Addr::LOCALHOST.into(),
            SocketAddr::V6(_) => Ipv6Addr::LOCALHOST.into(),
        });
    }
    address
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
        proxy_with(Config::default())
    }

    fn proxy_with(config: Config) -> Arc<Proxy> {
        let config = Config {
            backends: vec![Backend {
                name: "lobby".into(),
                address: "127.0.0.1:25566".into(),
            }],
            default_backend: "lobby".into(),
            ..config
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
    fn connections_over_the_limit_are_closed_at_once() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let proxy = proxy_with(Config {
            connections_per_minute: 1,
            ..Config::default()
        });
        let server = {
            let proxy = Arc::clone(&proxy);
            thread::spawn(move || serve(listener, &proxy))
        };

        // The first connection is kept open: the proxy waits for its handshake.
        let first = TcpStream::connect(address).unwrap();
        first
            .set_read_timeout(Some(Duration::from_millis(300)))
            .unwrap();
        let mut byte = [0u8; 1];
        let waiting = (&first).read(&mut byte).unwrap_err();
        assert!(
            matches!(
                waiting.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ),
            "{waiting}"
        );

        // The second, from the same address in the same minute, is closed without
        // the proxy waiting for anything.
        let mut second = TcpStream::connect(address).unwrap();
        second
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut rest = Vec::new();
        let closed = second.read_to_end(&mut rest);
        assert!(
            matches!(closed, Ok(0))
                || closed
                    .as_ref()
                    .is_err_and(|e| e.kind() == io::ErrorKind::ConnectionReset),
            "{closed:?}"
        );

        drop(first);
        proxy.stop();
        server.join().unwrap().unwrap();
    }

    /// A stop wakes the blocking accept while no one connects, whether the proxy
    /// listens on the loopback or on every interface, and the loop ends soon.
    #[test]
    fn a_stop_wakes_the_accept_loop_while_no_one_connects() {
        for bind in ["127.0.0.1:0", "0.0.0.0:0"] {
            let listener = TcpListener::bind(bind).unwrap();
            let proxy = proxy();
            let (done, ended) = std::sync::mpsc::channel();
            {
                let proxy = Arc::clone(&proxy);
                thread::spawn(move || {
                    let _ = done.send(serve(listener, &proxy));
                });
            }
            // Long enough for the loop to be waiting in accept.
            thread::sleep(Duration::from_millis(200));
            proxy.stop();
            let result = ended
                .recv_timeout(Duration::from_secs(10))
                .unwrap_or_else(|_| panic!("the loop on {bind} did not stop"));
            result.unwrap();
        }
    }

    #[test]
    fn the_waker_connects_on_the_loopback_for_every_interface() {
        let address = |text: &str| text.parse::<SocketAddr>().unwrap();
        assert_eq!(
            wake_address(address("0.0.0.0:25565")),
            address("127.0.0.1:25565")
        );
        assert_eq!(wake_address(address("[::]:25565")), address("[::1]:25565"));
        assert_eq!(
            wake_address(address("192.168.1.2:25565")),
            address("192.168.1.2:25565")
        );
    }

    /// The listener holds a burst of connections far larger than std's 128
    /// before the accept loop takes any.
    #[test]
    fn the_listener_queues_a_large_burst() {
        // Linux holds the queue to net.core.somaxconn, 4096 by default since
        // kernel 5.4 but lower on older systems.
        if let Ok(max) = std::fs::read_to_string("/proc/sys/net/core/somaxconn")
            && max.trim().parse::<u32>().is_ok_and(|max| max < 1000)
        {
            eprintln!("skipped: net.core.somaxconn is {}", max.trim());
            return;
        }
        let listener = listen("127.0.0.1:0".parse().unwrap()).unwrap();
        let address = listener.local_addr().unwrap();
        let mut waiting = Vec::new();
        for _ in 0..1000 {
            match TcpStream::connect_timeout(&address, Duration::from_secs(2)) {
                Ok(stream) => waiting.push(stream),
                Err(e) => panic!("refused after {} connections: {e}", waiting.len()),
            }
        }
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
