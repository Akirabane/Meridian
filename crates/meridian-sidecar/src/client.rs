//! The proxy's side of the signed exchange: one connection to one sidecar, and a
//! function for each player request.
use crate::Error;
use crate::request::{Outcome, Request};
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
        result
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
            Some(Value::Bool(false)) => Err(Error::Refused(
                answer
                    .get("error")
                    .map(|e| e.as_str().map_or_else(|| e.to_string(), str::to_owned))
                    .unwrap_or_default(),
            )),
            _ => Err(Error::Endpoint(format!(
                "the sidecar's reply has no outcome: {answer}"
            ))),
        }
    }

    /// The player will arrive through the proxy: the backend holds them.
    pub fn expect(&mut self, uuid: &str, name: &str) -> Result<Value, Error> {
        self.request(&Request::Expect {
            uuid: uuid.to_owned(),
            name: name.to_owned(),
        })
    }

    /// The player's record, to move to another backend.
    pub fn export(&mut self, uuid: &str) -> Result<Value, Error> {
        self.request(&Request::Export {
            uuid: uuid.to_owned(),
        })
    }

    /// Puts a record on the held player.
    pub fn import(&mut self, uuid: &str, record: Value) -> Result<Value, Error> {
        self.request(&Request::Import {
            uuid: uuid.to_owned(),
            record,
        })
    }

    /// Ends a switch on this backend.
    pub fn release(&mut self, uuid: &str, outcome: Outcome) -> Result<Value, Error> {
        self.request(&Request::Release {
            uuid: uuid.to_owned(),
            outcome,
        })
    }

    /// Forgets an expected or held player.
    pub fn drop_player(&mut self, uuid: &str) -> Result<Value, Error> {
        self.request(&Request::Drop {
            uuid: uuid.to_owned(),
        })
    }
}
