//! The proxy under measure: the real `meridian` program, started as a child
//! process on 127.0.0.1 in front of the stand-in backend, and always stopped when
//! the run ends, fails or is interrupted.
use std::io;
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

/// How long the proxy has to start listening.
const START: Duration = Duration::from_secs(15);

/// A running proxy.
pub struct Proxy {
    child: Child,
    pub address: SocketAddr,
    pub pid: Pid,
    log: PathBuf,
}

impl Proxy {
    /// Starts `binary` in front of `backend`, with its configuration and its log in
    /// `dir`, and waits until it listens.
    pub fn start(
        binary: &Path,
        backend: SocketAddr,
        secret: &str,
        dir: &Path,
    ) -> io::Result<Proxy> {
        Proxy::start_with(binary, dir, |address| {
            configuration(address, backend, secret)
        })
    }

    /// As [`Proxy::start`], with the configuration `write` makes for the address the proxy is
    /// to listen on.
    pub fn start_with(
        binary: &Path,
        dir: &Path,
        write: impl FnOnce(SocketAddr) -> String,
    ) -> io::Result<Proxy> {
        // A free port, taken and given back at once.
        let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?
            .local_addr()?
            .port();
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let config = dir.join("meridian.toml");
        std::fs::write(&config, write(address))?;
        let log = dir.join("meridian.log");
        let child = Command::new(binary)
            .arg(&config)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log)?)
            .spawn()
            .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", binary.display())))?;
        let pid = Pid::from_u32(child.id());
        let mut proxy = Proxy {
            child,
            address,
            pid,
            log,
        };
        let deadline = Instant::now() + START;
        loop {
            if std::fs::read_to_string(&proxy.log)?.contains("listening on") {
                return Ok(proxy);
            }
            if let Some(status) = proxy.child.try_wait()? {
                return Err(io::Error::other(format!(
                    "the proxy stopped at start ({status}): {}",
                    proxy.log_tail()
                )));
            }
            if Instant::now() > deadline {
                return Err(io::Error::other(format!(
                    "the proxy did not start listening: {}",
                    proxy.log_tail()
                )));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// The last lines of the proxy's log, for an error message.
    pub fn log_tail(&self) -> String {
        let text = std::fs::read_to_string(&self.log).unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        lines[lines.len().saturating_sub(5)..].join(" | ")
    }

    /// The proxy's whole log so far.
    pub fn log_text(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// Whether the proxy process is still there: it must be, all through a run.
    pub fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Lines of the proxy's log at warning level or worse.
    pub fn warnings(&self) -> usize {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .filter(|l| l.contains(" WARN ") || l.contains(" ERROR "))
            .count()
    }

    /// Stops the proxy and waits for it to exit.
    pub fn stop(mut self) -> io::Result<()> {
        self.kill()
    }

    fn kill(&mut self) -> io::Result<()> {
        if self.child.try_wait()?.is_none() {
            self.child.kill()?;
        }
        self.child.wait()?;
        Ok(())
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        let _ = self.kill();
    }
}

/// Whether a process with this id still runs: the check that no proxy is left
/// behind.
pub fn still_running(pid: Pid) -> bool {
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing(),
    );
    system.process(pid).is_some()
}

/// The proxy's configuration for a run: no player limit and no limit on
/// connections from one address, since every player comes from 127.0.0.1;
/// offline logins; the backend under the shared secret.
pub fn configuration(listen: SocketAddr, backend: SocketAddr, secret: &str) -> String {
    format!(
        "listen = \"{listen}\"\n\
         max-players = 0\n\
         connections-per-minute = 0\n\
         auth-mode = \"offline\"\n\
         compression-threshold = 256\n\
         connect-timeout-ms = 5000\n\
         forwarding-secret = \"{secret}\"\n\
         default-backend = \"bench\"\n\
         \n\
         [[backends]]\n\
         name = \"bench\"\n\
         address = \"{backend}\"\n"
    )
}
