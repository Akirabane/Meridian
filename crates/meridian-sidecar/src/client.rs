//! The proxy's side of the signed exchange: one connection to one sidecar, and a
//! function for each player request.
use crate::Error;
use crate::request::{Arrival, NoticeReport, Outcome, Request, ServerStatus};
use crate::wire::{self, Challenge, Direction, Expected};
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

/// A connection to a sidecar.
pub struct SidecarClient {
    writer: TcpStream,
    reader: BufReader<TcpStream>,
    secret: Vec<u8>,
    challenge: Challenge,
    seq: u64,
    /// Set by any failure: the connection is not used again, so a reply that
    /// arrives late can never be taken for the answer to the next request.
    broken: bool,
    /// Where it connected, and the time it allows, to connect again.
    address: String,
    timeout: Duration,
    /// Whether the last request was lost on the way ([`Error::is_lost`]).
    lost: bool,
}

impl SidecarClient {
    /// Connects to the sidecar at `address` (`host:port`) and reads its challenge.
    /// `timeout` bounds the connection and every read and write after it.
    pub fn connect(address: &str, secret: &[u8], timeout: Duration) -> Result<Self, Error> {
        let target = address
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| Error::Refused(format!("{address} has no address")))?;
        let stream = TcpStream::connect_timeout(&target, timeout)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        let writer = stream.try_clone()?;
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Err(Error::Refused("the sidecar closed the connection".into()));
        }
        let challenge = wire::read_challenge(line.trim())?;
        Ok(Self {
            writer,
            reader,
            secret: secret.to_vec(),
            challenge,
            seq: 0,
            broken: false,
            address: address.to_owned(),
            timeout,
            lost: false,
        })
    }

    /// Sends one request and returns the backend's answer.
    pub fn request(&mut self, request: &Request) -> Result<Value, Error> {
        if self.broken {
            return Err(Error::Refused(
                "this connection failed before; open a new one".into(),
            ));
        }
        let result = self.exchange(request);
        if matches!(
            result,
            Err(Error::Io(_) | Error::Timeout | Error::Wire(_) | Error::Endpoint(_))
        ) {
            self.broken = true;
        }
        self.lost = result.as_ref().is_err_and(Error::is_lost);
        result
    }

    /// Whether the last request was lost on the way ([`Error::is_lost`]): the backend
    /// may have acted on it. `false` after an answer, a refusal included.
    pub fn was_lost(&self) -> bool {
        self.lost
    }

    /// After a request that was lost on the way ([`Error::is_lost`]), makes the
    /// connection ready for the same request again: a new one, with a new challenge,
    /// when the old one broke. `Ok(false)` when the last request was not lost (it
    /// worked, or the backend refused it), so asking again would change nothing.
    ///
    /// The backend may have acted on the lost request, so only a request that can be
    /// asked twice with one effect goes again: an export of a frozen player, which
    /// answers the same record.
    pub fn recover(&mut self) -> Result<bool, Error> {
        if !self.lost {
            return Ok(false);
        }
        if self.broken {
            *self = SidecarClient::connect(&self.address, &self.secret, self.timeout)?;
        } else {
            self.lost = false;
        }
        Ok(true)
    }

    fn exchange(&mut self, request: &Request) -> Result<Value, Error> {
        self.seq += 1;
        let body = serde_json::to_string(request).expect("a request serialises");
        let line = wire::seal(
            &self.secret,
            &self.challenge,
            Direction::Request,
            self.seq,
            &body,
        );
        writeln!(self.writer, "{line}")?;
        let mut reply = String::new();
        if self.reader.read_line(&mut reply)? == 0 {
            return Err(Error::Endpoint("the sidecar closed the connection".into()));
        }
        // The sidecar refuses a line it cannot trust with an unsigned error, then
        // closes; nothing else may be taken without a signature.
        if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(reply.trim())
            && !map.contains_key("mac")
            && let Some(error) = map.get("error")
        {
            self.broken = true;
            return Err(Error::Refused(error.as_str().unwrap_or("").to_owned()));
        }
        let (_, body) = wire::open(
            &self.secret,
            &self.challenge,
            Direction::Reply,
            reply.trim(),
            Expected::Exactly(self.seq),
        )?;
        let answer: Value = serde_json::from_str(&body)
            .map_err(|e| Error::Endpoint(format!("the sidecar's reply is not JSON: {e}")))?;
        match answer.get("ok") {
            Some(Value::Bool(true)) => Ok(answer.get("result").cloned().unwrap_or(Value::Null)),
            Some(Value::Bool(false)) => {
                let why = answer
                    .get("error")
                    .map(|e| e.as_str().map_or_else(|| e.to_string(), str::to_owned))
                    .unwrap_or_default();
                if answer.get("unreached") == Some(&Value::Bool(true)) {
                    Err(Error::Unreached(why))
                } else {
                    Err(Error::Refused(why))
                }
            }
            _ => Err(Error::Endpoint(format!(
                "the sidecar's reply has no outcome: {answer}"
            ))),
        }
    }

    /// The player will arrive through the proxy within `seconds` (the backend's
    /// default when `None`): the backend holds them when they log in.
    pub fn expect(&mut self, uuid: &str, seconds: Option<u64>) -> Result<Value, Error> {
        self.request(&Request::Expect {
            uuid: uuid.to_owned(),
            seconds,
        })
    }

    /// The player's record, to move to another backend; with `freeze`, the
    /// player's own inputs stop being acted on until a release.
    pub fn export(&mut self, uuid: &str, freeze: bool) -> Result<Value, Error> {
        self.request(&Request::Export {
            uuid: uuid.to_owned(),
            freeze,
        })
    }

    /// Puts a record, as [`SidecarClient::export`] got it, on the held player: every part
    /// of it, or only `parts`; `announce` false keeps the arrival quiet. With
    /// `place` `Some(false)` the player stays held until [`SidecarClient::confirm`] or
    /// [`SidecarClient::drop_player`]; `None` takes the backend's default, which places.
    /// An `arrival` puts the player there instead of the record's place, with the
    /// `position` part only.
    pub fn import(
        &mut self,
        uuid: &str,
        record: Value,
        parts: Option<Vec<String>>,
        announce: Option<bool>,
        place: Option<bool>,
        arrival: Option<Arrival>,
    ) -> Result<Value, Error> {
        self.request(&Request::Import {
            uuid: uuid.to_owned(),
            record,
            parts,
            announce,
            place,
            arrival,
        })
    }

    /// Places a held player whose record was imported with `place` false;
    /// `handover` is the id of that record.
    pub fn confirm(&mut self, uuid: &str, handover: &str) -> Result<Value, Error> {
        self.request(&Request::Confirm {
            uuid: uuid.to_owned(),
            handover: handover.to_owned(),
        })
    }

    /// Ends a switch on this backend; `handover` is the id its export with freeze
    /// gave, and `announce` (with [`Outcome::Moved`] only) tells the others the player
    /// left.
    pub fn release(
        &mut self,
        uuid: &str,
        handover: &str,
        outcome: Outcome,
        announce: bool,
    ) -> Result<Value, Error> {
        self.request(&Request::Release {
            uuid: uuid.to_owned(),
            handover: handover.to_owned(),
            outcome,
            announce,
        })
    }

    /// The state of the backend: its players, its capacity, whether it accepts
    /// transfers. What the backend leaves out is `None`; an answer that is not a status
    /// is an error.
    pub fn status(&mut self) -> Result<ServerStatus, Error> {
        let answer = self.request(&Request::Status {})?;
        ServerStatus::from_value(&answer)
            .map_err(|why| Error::Endpoint(format!("the status is not usable: {why}")))
    }

    /// The notices of the backend's management protocol after number `after` (none
    /// without it: the answer then only says where the count stands), waiting up to `wait`
    /// for one when there are none. The connection's timeout must be longer than `wait`.
    pub fn notices(&mut self, after: Option<u64>, wait: Duration) -> Result<NoticeReport, Error> {
        let answer = self.request(&Request::Notices {
            after,
            wait_ms: Some(u64::try_from(wait.as_millis()).unwrap_or(u64::MAX)),
        })?;
        NoticeReport::from_value(&answer)
            .map_err(|why| Error::Endpoint(format!("the notices are not usable: {why}")))
    }

    /// Forgets an expected or held player. With a `handover`, only the arrival of the
    /// record with that id, which must be the imported one; before any import there
    /// is no id to name.
    pub fn drop_player(&mut self, uuid: &str, handover: Option<&str>) -> Result<Value, Error> {
        self.request(&Request::Drop {
            uuid: uuid.to_owned(),
            handover: handover.map(str::to_owned),
        })
    }
}
