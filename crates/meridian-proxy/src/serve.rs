//! The accept loop: one thread per player, until asked to stop. A stop ends the
//! sessions still being relayed, telling each player the server closed, so that
//! every backend sees its player leave and saves them, then waits for the
//! players' threads to end.
use crate::log::{debug, info, warning};
use crate::session;
use crate::state::Proxy;
use horizon_protocol::connection::Connection;
use meridian_config::ProxyProtocolMode;
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
                let config = proxy.config();
                let per_minute = config.connections_per_minute;
                proxy.sessions.metrics().connection();
                let table = &config.proxy_protocol;
                if table.trusts(peer.ip()) {
                    // A load balancer's connection: the player is who its PROXY header
                    // says, which is read on the player's own thread, so the limit is
                    // counted there, on that address and not on the balancer's.
                    let header = HeaderRead {
                        required: table.mode == ProxyProtocolMode::Required,
                        timeout: Duration::from_millis(table.header_timeout_ms),
                        per_minute,
                    };
                    start_player(stream, peer, proxy, &active, None, Some(header));
                } else if table.mode == ProxyProtocolMode::Required {
                    // Not a balancer, and the proxy takes players only through one: the
                    // address of a direct client is never taken for a player's.
                    debug!("{peer}: not a trusted address, and a PROXY header is required; closed");
                    drop(stream);
                } else if proxy.sessions.access.address_banned(peer.ip()) {
                    banned_address(proxy, peer);
                    drop(stream);
                } else if !proxy.throttle.allow(peer.ip(), per_minute, Instant::now()) {
                    proxy.sessions.metrics().throttled();
                    // Closed before a byte is read: the cheapest answer, and a
                    // client in a loop learns nothing more from a reason.
                    debug!("{peer}: over {per_minute} connections this minute; closed");
                    drop(stream);
                } else if let Some(pending) = enter_login(proxy, &config, peer) {
                    start_player(stream, peer, proxy, &active, Some(pending), None);
                } else {
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

/// What to do with the PROXY header of a connection from a trusted address.
struct HeaderRead {
    required: bool,
    timeout: Duration,
    per_minute: u32,
}

fn start_player(
    stream: TcpStream,
    peer: SocketAddr,
    proxy: &Arc<Proxy>,
    active: &Arc<AtomicUsize>,
    pending: Option<crate::throttle::Pending>,
    header: Option<HeaderRead>,
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
            let (peer, pending) = match header {
                None => (peer, pending),
                Some(header) => match player_address(&stream, peer, &header, &proxy) {
                    // Behind a balancer the place is the player's address, known now.
                    Some(player) => match enter_login(&proxy, &proxy.config(), player) {
                        Some(pending) => (player, Some(pending)),
                        None => return,
                    },
                    None => return,
                },
            };
            handle(stream, peer, &proxy, pending);
        });
    if let Err(e) = spawned {
        warning!("{peer}: cannot start a thread for the connection: {e}");
    }
}

/// The address of the player behind `balancer`, a trusted address: the source its PROXY
/// header gives, or `balancer`'s own when the header says nothing (a health check) or, when
/// the header is optional, is not there. `None` when the connection is to be closed: the
/// header is malformed, too long or late, or the player's address is over the limit. Nothing
/// is written back, since the sender is a balancer and not a Minecraft client.
fn player_address(
    stream: &TcpStream,
    balancer: SocketAddr,
    header: &HeaderRead,
    proxy: &Proxy,
) -> Option<SocketAddr> {
    use meridian_net::proxy_protocol::{Error, Header, read_header};
    let player = match read_header(stream, header.required, header.timeout) {
        Ok(Some(Header::Proxied { source, .. })) => {
            debug!("{balancer}: the player is {source}");
            source
        }
        Ok(Some(Header::Local)) => {
            debug!(
                "{balancer}: a PROXY header with no player address; the connection's own stands"
            );
            balancer
        }
        Ok(None) => balancer,
        // A balancer's health check connects and closes, or says nothing: not worth a line
        // above the debug level.
        Err(e @ (Error::Closed | Error::Timeout)) => {
            debug!("{balancer}: {e}; closed");
            return None;
        }
        Err(e) => {
            warning!("{balancer}: {e}; closed");
            return None;
        }
    };
    if proxy.sessions.access.address_banned(player.ip()) {
        banned_address(proxy, player);
        return None;
    }
    if !proxy
        .throttle
        .allow(player.ip(), header.per_minute, Instant::now())
    {
        proxy.sessions.metrics().throttled();
        debug!(
            "{player}: over {} connections this minute; closed",
            header.per_minute
        );
        return None;
    }
    Some(player)
}

/// A connection from a banned address is closed before a byte is read, like one over the
/// limit: the cheapest answer, and one that gives a client in a loop nothing. A ban's
/// reason is shown to the players the ban holds once they are identified, not here.
fn banned_address(proxy: &Proxy, address: SocketAddr) {
    proxy
        .sessions
        .metrics()
        .login_failed(crate::metrics::LoginFailure::Banned);
    debug!("{address}: a banned address; closed");
}

/// One player's connection, from the first byte to the close. The configuration
/// is read once here: a reload applies to the connections that come after it.
/// A place among the connections logging in for `peer`, or `None` when there is none
/// left: the connection is to be closed, before a byte is read.
fn enter_login(
    proxy: &Proxy,
    config: &meridian_config::Config,
    peer: SocketAddr,
) -> Option<crate::throttle::Pending> {
    let pending = proxy.logins.enter(
        peer.ip(),
        config.max_pending_logins,
        config.max_pending_logins_per_address,
    );
    if pending.is_none() {
        proxy.sessions.metrics().throttled();
        debug!("{peer}: too many connections logging in; closed");
    }
    pending
}

fn handle(
    stream: TcpStream,
    peer: SocketAddr,
    proxy: &Proxy,
    pending: Option<crate::throttle::Pending>,
) {
    let connection = match Connection::from_stream(stream) {
        Ok(connection) => connection,
        Err(e) => {
            warning!("{peer}: cannot set up the connection: {e}");
            return;
        }
    };
    let config = proxy.config();
    session::handle(
        connection,
        peer,
        &config,
        &*proxy.connect,
        &proxy.sessions,
        pending,
    );
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

        // Both were accepted; the second was closed for opening too many.
        let page = crate::metrics::render(&proxy);
        assert!(
            page.contains(
                "
meridian_connections_total 2
"
            ),
            "{page}"
        );
        assert!(
            page.contains(
                "
meridian_connections_throttled_total 1
"
            ),
            "{page}"
        );

        drop(first);
        proxy.stop();
        server.join().unwrap().unwrap();
    }

    /// Whether the proxy closed `stream` without waiting for anything from it.
    fn closed_at_once(stream: &TcpStream) -> bool {
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut rest = Vec::new();
        let closed = (&*stream).read_to_end(&mut rest);
        matches!(closed, Ok(0))
            || closed
                .as_ref()
                .is_err_and(|e| e.kind() == io::ErrorKind::ConnectionReset)
    }

    /// Whether the proxy keeps `stream` open, waiting for its handshake.
    fn kept_waiting(stream: &TcpStream) -> bool {
        stream
            .set_read_timeout(Some(Duration::from_millis(300)))
            .unwrap();
        let mut byte = [0u8; 1];
        (&*stream).read(&mut byte).is_err_and(|e| {
            matches!(
                e.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            )
        })
    }

    /// Waits until the proxy counts `n` connections logging in.
    fn logging_in(proxy: &Proxy, n: usize) {
        let until = Instant::now() + Duration::from_secs(10);
        while proxy.logins.count() != n {
            assert!(
                Instant::now() < until,
                "{} logging in",
                proxy.logins.count()
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn connections_past_the_pending_logins_are_closed_at_once() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let proxy = proxy_with(Config {
            connections_per_minute: 0,
            max_pending_logins: 1,
            ..Config::default()
        });
        let server = {
            let proxy = Arc::clone(&proxy);
            thread::spawn(move || serve(listener, &proxy))
        };
        // The first one logs in, slowly: it holds the only place.
        let first = TcpStream::connect(address).unwrap();
        assert!(kept_waiting(&first));
        logging_in(&proxy, 1);
        // The second is closed without a byte read.
        let second = TcpStream::connect(address).unwrap();
        assert!(closed_at_once(&second));
        // Once the first is gone, its place is given back.
        drop(first);
        logging_in(&proxy, 0);
        let third = TcpStream::connect(address).unwrap();
        assert!(kept_waiting(&third));
        let page = crate::metrics::render(&proxy);
        assert!(
            page.contains("\nmeridian_connections_throttled_total 1\n"),
            "{page}"
        );
        drop(third);
        proxy.stop();
        server.join().unwrap().unwrap();
    }

    #[test]
    fn the_pending_logins_of_one_address_have_no_limit_unless_set() {
        for (per_address, second_kept) in [(0, true), (1, false)] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let proxy = proxy_with(Config {
                connections_per_minute: 0,
                max_pending_logins_per_address: per_address,
                ..Config::default()
            });
            let server = {
                let proxy = Arc::clone(&proxy);
                thread::spawn(move || serve(listener, &proxy))
            };
            let first = TcpStream::connect(address).unwrap();
            assert!(kept_waiting(&first));
            let second = TcpStream::connect(address).unwrap();
            if second_kept {
                assert!(kept_waiting(&second), "two players behind one address");
            } else {
                assert!(closed_at_once(&second));
            }
            drop((first, second));
            proxy.stop();
            server.join().unwrap().unwrap();
        }
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

    // ---- The PROXY protocol ----

    use meridian_config::{AuthMode, ProxyProtocol, TrustedNet};
    use meridian_testkit::{client::FakeClient, trace::Recorder};
    use std::io::Write;
    use std::net::IpAddr;
    use std::sync::Mutex;

    const V1_FROM_203: &[u8] = b"PROXY TCP4 203.0.113.9 198.51.100.2 51234 25565\r\n";

    /// A version 2 header for the IPv4 source `ip`:`port`.
    fn v2_from(ip: [u8; 4], port: u16) -> Vec<u8> {
        let mut header = b"\r\n\r\n\0\r\nQUIT\n".to_vec();
        header.extend([0x21, 0x11, 0x00, 12]);
        header.extend(ip);
        header.extend([198, 51, 100, 2]);
        header.extend(port.to_be_bytes());
        header.extend(25565u16.to_be_bytes());
        header
    }

    /// A proxy whose backend is a stand-in that writes down the address of each player
    /// it is asked to log in and then refuses: the player is told so, and the test has
    /// what the proxy would have forwarded.
    fn recording_proxy(
        table: ProxyProtocol,
        extra: Config,
    ) -> (Arc<Proxy>, Arc<Mutex<Vec<IpAddr>>>) {
        let config = Config {
            backends: vec![Backend {
                name: "lobby".into(),
                address: "127.0.0.1:25566".into(),
            }],
            default_backend: "lobby".into(),
            online_mode: false,
            auth_mode: AuthMode::Offline,
            forwarding_secret: "secret".into(),
            proxy_protocol: table,
            ..extra
        };
        let mut proxy = Proxy::new(
            config,
            "meridian.toml".into(),
            Arc::new(AtomicBool::new(false)),
        );
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        proxy.connect = Arc::new(move |_, _, options| {
            record.lock().unwrap().push(options.player_address);
            Err(meridian_net::Error::Io(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                "refused",
            )))
        });
        (Arc::new(proxy), seen)
    }

    fn table(mode: ProxyProtocolMode, trusted: &[&str]) -> ProxyProtocol {
        ProxyProtocol {
            mode,
            trusted: trusted
                .iter()
                .map(|t| TrustedNet::parse(t).unwrap())
                .collect(),
            header_timeout_ms: 1_000,
        }
    }

    /// Runs `proxy` on a listener; `test` gets its address.
    fn serving(proxy: &Arc<Proxy>, test: impl FnOnce(SocketAddr)) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = {
            let proxy = Arc::clone(proxy);
            thread::spawn(move || serve(listener, &proxy))
        };
        test(address);
        proxy.stop();
        server.join().unwrap().unwrap();
    }

    /// Connects, writes `first`, then logs in as a player. The proxy reaches the stand-in
    /// backend or closes the connection before that.
    fn join_after(address: SocketAddr, first: &[u8]) {
        let mut stream = TcpStream::connect(address).unwrap();
        stream.write_all(first).unwrap();
        let mut connection = Connection::from_stream(stream).unwrap();
        let _ = FakeClient::named("Alex").run(&mut connection, &Recorder::new());
    }

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    #[test]
    fn a_trusted_balancers_header_gives_the_address_the_backends_are_told() {
        for mode in [ProxyProtocolMode::Optional, ProxyProtocolMode::Required] {
            for header in [V1_FROM_203.to_vec(), v2_from([203, 0, 113, 9], 51234)] {
                let (proxy, seen) = recording_proxy(table(mode, &["127.0.0.1"]), Config::default());
                serving(&proxy, |address| join_after(address, &header));
                assert_eq!(*seen.lock().unwrap(), [ip("203.0.113.9")], "{mode:?}");
            }
        }
    }

    #[test]
    fn an_optional_header_may_be_left_out_by_a_trusted_address() {
        let (proxy, seen) = recording_proxy(
            table(ProxyProtocolMode::Optional, &["127.0.0.0/8"]),
            Config::default(),
        );
        serving(&proxy, |address| join_after(address, &[]));
        // The connection's own address: this machine.
        assert_eq!(*seen.lock().unwrap(), [ip("127.0.0.1")]);
    }

    /// A header with no player in it (a balancer's own check) leaves the connection's address.
    #[test]
    fn a_header_with_no_player_address_leaves_the_connections() {
        let (proxy, seen) = recording_proxy(
            table(ProxyProtocolMode::Required, &["127.0.0.1"]),
            Config::default(),
        );
        serving(&proxy, |address| join_after(address, b"PROXY UNKNOWN\r\n"));
        assert_eq!(*seen.lock().unwrap(), [ip("127.0.0.1")]);
    }

    /// The point of the trusted list: a client that is not a listed balancer cannot give
    /// itself an address, whatever it sends and whatever the mode.
    #[test]
    fn a_header_from_an_untrusted_address_is_never_believed() {
        for mode in [
            ProxyProtocolMode::Off,
            ProxyProtocolMode::Optional,
            ProxyProtocolMode::Required,
        ] {
            // The trusted address is another machine's. In `off`, the list is not even
            // consulted.
            let (proxy, seen) = recording_proxy(table(mode, &["192.0.2.1"]), Config::default());
            serving(&proxy, |address| join_after(address, V1_FROM_203));
            assert!(
                seen.lock().unwrap().is_empty(),
                "{mode:?}: a header from an untrusted address was used: {:?}",
                seen.lock().unwrap()
            );
        }
        // The same, with a version 2 header.
        let (proxy, seen) = recording_proxy(
            table(ProxyProtocolMode::Optional, &["192.0.2.1"]),
            Config::default(),
        );
        serving(&proxy, |address| {
            join_after(address, &v2_from([203, 0, 113, 9], 1));
        });
        assert!(seen.lock().unwrap().is_empty());
    }

    /// With no table, as before: a header is just bytes the proxy does not understand.
    #[test]
    fn with_the_protocol_off_a_header_is_not_read_and_a_player_is_as_before() {
        let (proxy, seen) = recording_proxy(ProxyProtocol::default(), Config::default());
        serving(&proxy, |address| join_after(address, &[]));
        assert_eq!(*seen.lock().unwrap(), [ip("127.0.0.1")]);
        let (proxy, seen) = recording_proxy(ProxyProtocol::default(), Config::default());
        serving(&proxy, |address| join_after(address, V1_FROM_203));
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn a_required_header_closes_an_untrusted_address_at_once() {
        let (proxy, seen) = recording_proxy(
            table(ProxyProtocolMode::Required, &["192.0.2.1"]),
            Config::default(),
        );
        serving(&proxy, |address| {
            let mut stream = TcpStream::connect(address).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let _ = stream.write_all(V1_FROM_203);
            let mut rest = Vec::new();
            let closed = stream.read_to_end(&mut rest);
            assert!(
                matches!(closed, Ok(0))
                    || closed
                        .as_ref()
                        .is_err_and(|e| e.kind() == io::ErrorKind::ConnectionReset),
                "{closed:?}"
            );
        });
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn a_trusted_connection_with_a_bad_header_is_closed_and_no_backend_is_asked() {
        for (mode, bad) in [
            (
                ProxyProtocolMode::Required,
                &b"PROXY TCP4 not-an-address 198.51.100.2 1 2\r\n"[..],
            ),
            (
                ProxyProtocolMode::Optional,
                b"PROXY TCP4 203.0.113.9 198.51.100.2 1\r\n",
            ),
            // No header where one is required.
            (ProxyProtocolMode::Required, &[0x10, 0x00, 0xff, 0x05, 0x01]),
            // A version 2 header with more than the bound.
            (
                ProxyProtocolMode::Required,
                b"\r\n\r\n\0\r\nQUIT\n\x21\x11\xff\xff",
            ),
        ] {
            let (proxy, seen) = recording_proxy(table(mode, &["127.0.0.1"]), Config::default());
            serving(&proxy, |address| {
                let mut stream = TcpStream::connect(address).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                stream.write_all(bad).unwrap();
                let mut rest = Vec::new();
                let closed = stream.read_to_end(&mut rest);
                assert!(
                    matches!(closed, Ok(0))
                        || closed
                            .as_ref()
                            .is_err_and(|e| e.kind() == io::ErrorKind::ConnectionReset),
                    "{closed:?}"
                );
                // Nothing is written back to a balancer.
                assert!(rest.is_empty(), "{rest:?}");
            });
            assert!(seen.lock().unwrap().is_empty(), "{mode:?}");
        }
    }

    /// A header that never comes does not hold the connection.
    #[test]
    fn a_header_that_does_not_come_in_time_closes_the_connection() {
        let mut table = table(ProxyProtocolMode::Required, &["127.0.0.1"]);
        table.header_timeout_ms = 200;
        let (proxy, seen) = recording_proxy(table, Config::default());
        serving(&proxy, |address| {
            let mut stream = TcpStream::connect(address).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            stream.write_all(b"PROXY TCP4 203.0").unwrap();
            let started = Instant::now();
            let mut rest = Vec::new();
            let closed = stream.read_to_end(&mut rest);
            assert!(
                matches!(closed, Ok(0))
                    || closed
                        .as_ref()
                        .is_err_and(|e| e.kind() == io::ErrorKind::ConnectionReset),
                "{closed:?}"
            );
            assert!(started.elapsed() < Duration::from_secs(5));
        });
        assert!(seen.lock().unwrap().is_empty());
    }

    /// The limit per address is the player's, not the balancer's: every player of a
    /// balancer shares its address, and would be limited together otherwise.
    #[test]
    fn the_connection_limit_counts_the_players_address_and_not_the_balancers() {
        let (proxy, _) = recording_proxy(
            table(ProxyProtocolMode::Required, &["127.0.0.1"]),
            Config {
                connections_per_minute: 1,
                ..Config::default()
            },
        );
        serving(&proxy, |address| {
            let open = |source: [u8; 4]| {
                let mut stream = TcpStream::connect(address).unwrap();
                stream.write_all(&v2_from(source, 1000)).unwrap();
                stream
            };
            // Whether the proxy is still waiting for the player's handshake (open) or
            // has closed the connection (over the limit).
            let state = |stream: &mut TcpStream| {
                stream
                    .set_read_timeout(Some(Duration::from_millis(400)))
                    .unwrap();
                let mut byte = [0u8; 1];
                match stream.read(&mut byte) {
                    Ok(0) => "closed",
                    Err(e) if e.kind() == io::ErrorKind::ConnectionReset => "closed",
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                        ) =>
                    {
                        "open"
                    }
                    other => panic!("{other:?}"),
                }
            };
            let mut first = open([203, 0, 113, 9]);
            assert_eq!(state(&mut first), "open");
            // Another player behind the same balancer.
            let mut second = open([203, 0, 113, 10]);
            assert_eq!(state(&mut second), "open");
            // The first player's address again, in the same minute.
            let mut again = open([203, 0, 113, 9]);
            assert_eq!(state(&mut again), "closed");
        });
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
