//! The extension's end of the connection (docs/EXTENSIONS.md, sections 2 and 8.1): an [`Extension`]
//! listens on a loopback address, does the hello with the proxy that connects to it, answers
//! `rpc.discover`, and runs the handlers that are registered for the proxy's requests.
//!
//! It is synchronous, like the rest of Meridian: one thread accepts, one reads the connection that is
//! up, and each request runs on a thread of its own, at most [`Extension::max_in_flight`] at once. One
//! proxy is connected at a time, since the proxy opens one connection for each extension; another that
//! connects meanwhile is told `503` and dropped.
//!
//! A handler that panics is answered with an internal error, and the connection goes on. What the panic
//! says is written by Rust's default panic hook to the standard error stream, as for any panic in the
//! program, and not through [`Log`]: a library sets no hook of its own, so a program that wants those
//! lines elsewhere, or not at all, sets one with `std::panic::set_hook`.
//!
//! What it never does: serve a request before both proofs of the hello are checked, say anything to a
//! proxy that proved a wrong token but the close (code 1008), log a message whole, or log the token.

mod connection;
mod log;

#[cfg(test)]
mod tests;

pub use log::{Log, Stderr};

use crate::handshake::Secret;
use crate::wire::{CommandSpec, DiscoverMethod, RpcError};
use serde_json::Value;
use std::io;
use std::net::{SocketAddr, TcpListener, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// What a handler is given (the request's `params`) and gives back (the `result`, or an error).
pub(crate) type Call = dyn Fn(Option<Value>) -> Result<Value, RpcError> + Send + Sync;

/// A method the extension serves.
pub(crate) struct Handler {
    /// The capability the extension declares for it in the hello.
    pub capability: &'static str,
    /// How `rpc.discover` lists it.
    pub discover: DiscoverMethod,
    pub call: Box<Call>,
}

/// How long the steps of a connection may take.
#[derive(Clone, Debug)]
pub(crate) struct Timings {
    /// For the upgrade and the whole of the hello.
    pub hello: Duration,
    /// A write may not block longer than this.
    pub write: Duration,
    /// How often a connection that hears nothing looks at the stop flag.
    pub look: Duration,
    /// How long a stop waits for the requests that are running.
    pub grace: Duration,
}

impl Default for Timings {
    fn default() -> Self {
        Self {
            hello: Duration::from_secs(5),
            write: Duration::from_secs(5),
            look: Duration::from_millis(50),
            grace: Duration::from_secs(2),
        }
    }
}

/// An extension, before it listens.
pub struct Extension {
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) token: Secret,
    pub(crate) log: Arc<dyn Log>,
    pub(crate) max_in_flight: usize,
    pub(crate) timings: Timings,
    pub(crate) handlers: Vec<(String, Handler)>,
    pub(crate) commands: Vec<CommandSpec>,
}

impl Extension {
    /// An extension that calls itself `name` and `version` in the proxy's logs, and shares `token` with
    /// the proxy (the file named by `token-file` in its `[[extension]]`; see
    /// [`Secret::from_file_contents`]). It serves nothing until handlers are registered.
    pub fn new(name: impl Into<String>, version: impl Into<String>, token: Secret) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            token,
            log: Arc::new(Stderr),
            max_in_flight: 64,
            timings: Timings::default(),
            handlers: Vec::new(),
            commands: Vec::new(),
        }
    }

    /// Where the SDK writes its lines. The default is [`Stderr`].
    pub fn log(mut self, log: Arc<dyn Log>) -> Self {
        self.log = log;
        self
    }

    /// The most requests that run at once (64 by default, and at least 1). Set it to the
    /// `max-in-flight` of the extension's `[[extension]]`: the proxy sends no more than that, and one
    /// more is answered with an error.
    pub fn max_in_flight(mut self, most: usize) -> Self {
        self.max_in_flight = most.max(1);
        self
    }

    /// Registers the handler of `method`, which the extension declares with `capability`.
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "the decision points that register handlers come next"
        )
    )]
    pub(crate) fn register(
        mut self,
        method: &str,
        capability: &'static str,
        discover: DiscoverMethod,
        call: impl Fn(Option<Value>) -> Result<Value, RpcError> + Send + Sync + 'static,
    ) -> Self {
        assert!(
            !self.handlers.iter().any(|(name, _)| name == method),
            "{method} is registered twice"
        );
        self.handlers.push((
            method.to_owned(),
            Handler {
                capability,
                discover,
                call: Box::new(call),
            },
        ));
        self
    }

    /// The capabilities the extension declares: those of its handlers, once each, in the order the
    /// handlers were registered.
    pub(crate) fn capabilities(&self) -> Vec<String> {
        let mut capabilities: Vec<String> = Vec::new();
        for (_, handler) in &self.handlers {
            if !capabilities.iter().any(|c| c == handler.capability) {
                capabilities.push(handler.capability.to_owned());
            }
        }
        capabilities
    }

    /// Starts listening on `address`, which must be on the loopback (port 0 chooses a free port, which
    /// [`Running::local_addr`] tells). The extension then runs on threads of its own.
    pub fn bind(self, address: impl ToSocketAddrs) -> io::Result<Running> {
        let mut last = None;
        for candidate in address.to_socket_addrs()? {
            if !candidate.ip().is_loopback() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "{candidate} is not on the loopback: an extension listens on 127.0.0.1 or [::1]"
                    ),
                ));
            }
            match TcpListener::bind(candidate) {
                Ok(listener) => return Running::start(self, listener),
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "the address resolves to nothing",
            )
        }))
    }

    /// [`Extension::bind`], then waits until the extension ends. Nothing here can stop it: a program
    /// that must stop cleanly uses `bind`, and its [`Running::stopper`] from another thread.
    pub fn serve(self, address: impl ToSocketAddrs) -> io::Result<()> {
        self.bind(address)?.wait();
        Ok(())
    }
}

/// What the connection threads share.
pub(crate) struct Shared {
    pub extension: Extension,
    pub stop: AtomicBool,
    /// Requests that are running.
    pub in_flight: AtomicUsize,
}

impl Shared {
    pub(crate) fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
}

/// An extension that is listening.
pub struct Running {
    address: SocketAddr,
    shared: Arc<Shared>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Running {
    fn start(extension: Extension, listener: TcpListener) -> io::Result<Self> {
        let address = listener.local_addr()?;
        let name = crate::text::clean(&extension.name, 32);
        let shared = Arc::new(Shared {
            extension,
            stop: AtomicBool::new(false),
            in_flight: AtomicUsize::new(0),
        });
        shared.extension.log.info(&format!(
            "extension \"{name}\" {} listens on {address}",
            crate::text::clean(&shared.extension.version, 64),
        ));
        let thread = {
            let shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name(format!("extension-{name}"))
                .spawn(move || connection::run(&listener, &shared))?
        };
        Ok(Self {
            address,
            shared,
            thread: Mutex::new(Some(thread)),
        })
    }

    /// The address it listens on.
    pub fn local_addr(&self) -> SocketAddr {
        self.address
    }

    /// A handle that stops it from another thread.
    pub fn stopper(&self) -> Stopper {
        Stopper(Arc::clone(&self.shared))
    }

    /// Stops listening, closes the connection that is up with a normal close, waits a moment for the
    /// requests that are running and for the threads, and frees the port.
    pub fn stop(&self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.wait();
    }

    /// Waits until the threads have ended, which they do after a stop.
    pub fn wait(&self) {
        let thread = self.thread.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(thread) = thread {
            let _ = thread.join();
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Stops a [`Running`] extension from a thread that does not own it, such as a signal handler's.
#[derive(Clone)]
pub struct Stopper(Arc<Shared>);

impl Stopper {
    /// Asks it to stop; `serve` or `wait` then return.
    pub fn stop(&self) {
        self.0.stop.store(true, Ordering::Relaxed);
    }
}
