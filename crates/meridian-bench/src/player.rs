//! One quiet player of a run: it logs in offline, ends the configuration phase,
//! waits for every other player, then sends measuring packets at a steady rate,
//! one at a time, and times each until its echo comes back.
use crate::echo;
use horizon_protocol::connection::Connection;
use horizon_protocol::crypto::offline_uuid;
use horizon_protocol::ids::{configuration as cfg, play};
use meridian_testkit::backend::acknowledge_configuration;
use meridian_testkit::body;
use meridian_testkit::client::FakeClient;
use meridian_testkit::peer::Step;
use meridian_testkit::trace::Recorder;
use std::io;
use std::net::{SocketAddr, TcpStream};
use std::sync::Barrier;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// How long a connection or an echo may take before the player gives up.
const PATIENCE: Duration = Duration::from_secs(10);

/// What one player does.
#[derive(Clone, Debug)]
pub struct Plan {
    pub address: SocketAddr,
    pub name: String,
    /// Measuring packets a second.
    pub rate: u32,
    /// Bytes of data in each measuring packet.
    pub size: usize,
    /// How long the measured window lasts.
    pub window: Duration,
}

/// Why a player did not get through.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The machine ran out of ports, sockets or file descriptors.
    Exhausted(String),
    Other(String),
}

/// What one player measured.
#[derive(Clone, Debug, Default)]
pub struct Outcome {
    /// From the connection to the start of the play phase.
    pub login: Option<Duration>,
    /// One time for each measuring packet that came back.
    pub round_trips: Vec<Duration>,
    pub packets_sent: u64,
    pub bytes_sent: u64,
    pub failure: Option<Failure>,
}

/// Runs one player. Every player of a run, and the run itself, meet at `start`
/// once logged in, so the measured window begins for all at once; a player whose
/// login failed still meets there.
pub fn run(plan: &Plan, start: &Barrier, stop: &AtomicBool) -> Outcome {
    let mut outcome = Outcome::default();
    let began = Instant::now();
    let connection = match login(plan) {
        Ok(connection) => {
            outcome.login = Some(began.elapsed());
            Some(connection)
        }
        Err(failure) => {
            outcome.failure = Some(failure);
            None
        }
    };
    start.wait();
    if let Some(mut connection) = connection
        && let Err(failure) = measure(&mut connection, plan, stop, &mut outcome)
    {
        outcome.failure = Some(failure);
    }
    outcome
}

fn login(plan: &Plan) -> Result<Connection, Failure> {
    let stream = TcpStream::connect_timeout(&plan.address, PATIENCE).map_err(classify)?;
    let mut connection =
        Connection::from_stream(stream).map_err(|e| Failure::Other(e.to_string()))?;
    FakeClient {
        name: plan.name.clone(),
        uuid: offline_uuid(&plan.name),
        host: plan.address.ip().to_string(),
        port: plan.address.port(),
        after_login: vec![
            Step::Expect {
                id: cfg::clientbound::FINISH_CONFIGURATION,
            },
            acknowledge_configuration(),
        ],
    }
    .run(&mut connection, &Recorder::new())
    .map_err(Failure::Other)?;
    connection
        .reader
        .set_read_timeout(Some(PATIENCE))
        .map_err(|e| Failure::Other(e.to_string()))?;
    Ok(connection)
}

fn measure(
    connection: &mut Connection,
    plan: &Plan,
    stop: &AtomicBool,
    outcome: &mut Outcome,
) -> Result<(), Failure> {
    let interval = Duration::from_secs_f64(1.0 / f64::from(plan.rate.max(1)));
    let start = Instant::now();
    let mut next = start;
    let mut seq = 0u64;
    while start.elapsed() < plan.window && !stop.load(Ordering::Relaxed) {
        seq += 1;
        let packet = body(
            play::serverbound::CUSTOM_PAYLOAD,
            &echo::payload(seq, plan.size),
        );
        let sent = Instant::now();
        connection
            .send(&packet)
            .map_err(|e| Failure::Other(format!("sending: {e}")))?;
        outcome.packets_sent += 1;
        outcome.bytes_sent += packet.len() as u64;
        loop {
            let (id, payload) = connection
                .recv()
                .map_err(|e| Failure::Other(format!("waiting for an echo: {e}")))?;
            if id == play::clientbound::CUSTOM_PAYLOAD && echo::seq(&payload) == Some(seq) {
                outcome.round_trips.push(sent.elapsed());
                break;
            }
        }
        next += interval;
        if let Some(wait) = next.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
    }
    Ok(())
}

/// An error that says the machine is out of something, rather than the proxy or
/// the backend failing: out of ports (Windows' WSAEADDRINUSE and WSAENOBUFS, or
/// EADDRNOTAVAIL) or out of descriptors (WSAEMFILE, EMFILE, ENFILE).
fn classify(e: io::Error) -> Failure {
    let exhausted = matches!(
        e.kind(),
        io::ErrorKind::AddrInUse | io::ErrorKind::AddrNotAvailable
    ) || matches!(e.raw_os_error(), Some(10048 | 10055 | 10024 | 99 | 24 | 23));
    if exhausted {
        Failure::Exhausted(e.to_string())
    } else {
        Failure::Other(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::Backend;
    use std::sync::Arc;

    /// Three players straight to the backend for half a second, at 20 packets a
    /// second: every packet comes back, and every player logs in.
    #[test]
    fn players_measure_a_direct_backend() {
        let backend = Backend::start(None).unwrap();
        let start = Arc::new(Barrier::new(3));
        let stop = Arc::new(AtomicBool::new(false));
        let players: Vec<_> = (0..3)
            .map(|i| {
                let plan = Plan {
                    address: backend.address,
                    name: format!("Bench{i}"),
                    rate: 20,
                    size: 64,
                    window: Duration::from_millis(500),
                };
                let (start, stop) = (Arc::clone(&start), Arc::clone(&stop));
                std::thread::spawn(move || run(&plan, &start, &stop))
            })
            .collect();
        for player in players {
            let outcome = player.join().unwrap();
            assert_eq!(outcome.failure, None);
            assert!(outcome.login.is_some());
            assert!(outcome.packets_sent >= 5, "{}", outcome.packets_sent);
            assert_eq!(outcome.round_trips.len() as u64, outcome.packets_sent);
        }
        assert_eq!(backend.logged_in.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn running_out_of_ports_or_descriptors_is_told_apart() {
        let out_of_ports = io::Error::from_raw_os_error(if cfg!(windows) { 10055 } else { 99 });
        assert!(matches!(classify(out_of_ports), Failure::Exhausted(_)));
        let refused = io::Error::from(io::ErrorKind::ConnectionRefused);
        assert!(matches!(classify(refused), Failure::Other(_)));
    }
}
