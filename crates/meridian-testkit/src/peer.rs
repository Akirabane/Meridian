//! A scripted peer: one side of a connection that sends and expects packets in a
//! fixed order. The fake client of a test is one, and so is the fake backend once
//! its login is done.
use crate::{
    body,
    trace::{Dir, Recorder},
};
use horizon_protocol::connection::Connection;
use std::{thread::JoinHandle, time::Duration};

/// How long a peer waits for a packet before it gives up, so that a test fails
/// instead of hanging.
pub const WAIT: Duration = Duration::from_secs(5);

/// One step of a script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Send a packet.
    Send { id: i32, payload: Vec<u8> },
    /// Receive the next packet, which must have this id.
    Expect { id: i32 },
    /// Receive the next packet, which must have this id and this payload.
    ExpectExactly { id: i32, payload: Vec<u8> },
    /// Close the connection.
    Close,
    /// Receive nothing more: the other side closes the connection.
    ExpectClose,
}

impl Step {
    pub fn send(id: i32, payload: impl Into<Vec<u8>>) -> Self {
        Step::Send {
            id,
            payload: payload.into(),
        }
    }

    pub fn expect_exactly(id: i32, payload: impl Into<Vec<u8>>) -> Self {
        Step::ExpectExactly {
            id,
            payload: payload.into(),
        }
    }
}

/// Runs `steps` on `connection`. Every packet received is recorded as going
/// `incoming`. Returns at the first step that fails, with a message that names it.
pub fn run(
    connection: &mut Connection,
    steps: &[Step],
    incoming: Dir,
    recorder: &Recorder,
) -> Result<(), String> {
    connection
        .reader
        .set_read_timeout(Some(WAIT))
        .map_err(|e| e.to_string())?;
    for (n, step) in steps.iter().enumerate() {
        let fail = |what: String| format!("step {n} ({step:?}): {what}");
        match step {
            Step::Send { id, payload } => connection
                .send(&body(*id, payload))
                .map_err(|e| fail(e.to_string()))?,
            Step::Expect { id } | Step::ExpectExactly { id, .. } => {
                let (got, payload) = connection.recv().map_err(|e| fail(e.to_string()))?;
                recorder.record(incoming, got, &payload);
                if got != *id {
                    return Err(fail(format!("received packet {got}")));
                }
                if let Step::ExpectExactly {
                    payload: wanted, ..
                } = step
                    && *wanted != payload
                {
                    return Err(fail(format!("received the payload {payload:?}")));
                }
            }
            Step::Close => {
                connection.writer.shutdown();
                return Ok(());
            }
            Step::ExpectClose => match connection.recv() {
                Ok((got, payload)) => {
                    recorder.record(incoming, got, &payload);
                    return Err(fail(format!("received packet {got}")));
                }
                Err(e) if e.is_end_of_stream() => {}
                Err(e) => return Err(fail(e.to_string())),
            },
        }
    }
    Ok(())
}

/// Runs `steps` on a thread of its own and hands back the connection at the end,
/// so a test can keep using it or drop it to close it.
pub fn spawn(
    mut connection: Connection,
    steps: Vec<Step>,
    incoming: Dir,
    recorder: Recorder,
) -> JoinHandle<Result<Connection, String>> {
    std::thread::spawn(move || {
        run(&mut connection, &steps, incoming, &recorder).map(|()| connection)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipe;

    #[test]
    fn two_peers_follow_their_scripts() {
        let (a, b) = pipe();
        let recorder = Recorder::new();
        let left = spawn(
            a,
            vec![Step::send(1, [10, 11]), Step::Expect { id: 2 }, Step::Close],
            Dir::ToClient,
            recorder.clone(),
        );
        let right = spawn(
            b,
            vec![
                Step::expect_exactly(1, [10, 11]),
                Step::send(2, []),
                Step::ExpectClose,
            ],
            Dir::ToServer,
            recorder.clone(),
        );
        left.join().unwrap().unwrap();
        right.join().unwrap().unwrap();
        let trace = recorder.trace();
        assert_eq!(trace.ids(Dir::ToServer), vec![1]);
        assert_eq!(trace.ids(Dir::ToClient), vec![2]);
    }

    #[test]
    fn a_wrong_packet_fails_the_script() {
        let (mut a, b) = pipe();
        a.send(&body(5, &[])).unwrap();
        let result = spawn(
            b,
            vec![Step::Expect { id: 6 }],
            Dir::ToServer,
            Recorder::new(),
        )
        .join()
        .unwrap();
        let error = result.err().unwrap();
        assert!(error.contains("received packet 5"), "{error}");
    }
}
