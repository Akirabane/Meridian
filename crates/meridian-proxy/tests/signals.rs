//! SIGTERM and SIGHUP sent to a running `meridian` (Unix only). The proxy is the real
//! binary, started on a port the system picks, and the signals are really sent, with the
//! `kill` program.
#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(30);

fn config(motd: &str) -> String {
    format!(
        "listen = \"127.0.0.1:0\"\n\
         motd = \"{motd}\"\n\
         forwarding-secret = \"test-secret-test-secret\"\n\
         default-backend = \"lobby\"\n\
         \n\
         [[backends]]\n\
         name = \"lobby\"\n\
         address = \"127.0.0.1:1\"\n"
    )
}

/// A proxy that runs, its log lines, and the folder holding its configuration.
struct Running {
    child: Child,
    lines: Receiver<String>,
    seen: Vec<String>,
    file: PathBuf,
}

impl Running {
    fn start(name: &str) -> Running {
        let dir =
            std::env::temp_dir().join(format!("meridian-signals-{name}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("meridian.toml");
        fs::write(&file, config("before")).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_meridian"))
            .arg(&file)
            // Open for as long as the proxy runs: a console whose input closes is no reason to stop.
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let (send, lines) = mpsc::channel();
        let stderr = child.stderr.take().unwrap();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if send.send(line).is_err() {
                    break;
                }
            }
        });
        let mut running = Running {
            child,
            lines,
            seen: Vec::new(),
            file,
        };
        running.wait_for("listening on");
        running
    }

    /// Waits for a log line that holds `text`, and returns it.
    fn wait_for(&mut self, text: &str) -> String {
        let until = Instant::now() + WAIT;
        loop {
            let left = until.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(line) => {
                    self.seen.push(line.clone());
                    if line.contains(text) {
                        return line;
                    }
                }
                Err(_) => panic!(
                    "no log line with {text:?}; the log so far:\n{}",
                    self.seen.join("\n")
                ),
            }
        }
    }

    fn signal(&self, name: &str) {
        let status = Command::new("kill")
            .arg(format!("-{name}"))
            .arg(self.child.id().to_string())
            .status()
            .unwrap();
        assert!(status.success(), "kill -{name} failed");
    }

    fn exit(&mut self) -> ExitStatus {
        let until = Instant::now() + WAIT;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < until,
                "the proxy is still running; the log so far:\n{}",
                self.seen.join("\n")
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn folder(&self) -> &Path {
        self.file.parent().unwrap()
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(self.folder());
    }
}

#[test]
fn sigterm_stops_the_proxy_cleanly() {
    let mut proxy = Running::start("term");
    proxy.signal("TERM");
    proxy.wait_for("SIGTERM: stopping");
    // The proxy says it has stopped once its sessions are closed, as after Ctrl+C.
    proxy.wait_for("stopped");
    let status = proxy.exit();
    assert!(status.success(), "{status:?}");
}

#[test]
fn sighup_reads_the_configuration_again_and_keeps_the_proxy_running() {
    let mut proxy = Running::start("hup");
    fs::write(&proxy.file, config("after")).unwrap();
    proxy.signal("HUP");
    proxy.wait_for("reload: motd: after");
    assert!(
        proxy.child.try_wait().unwrap().is_none(),
        "the proxy stopped"
    );

    // A file that does not pass the checks changes nothing and does not stop the proxy.
    fs::write(&proxy.file, "this is not a configuration").unwrap();
    proxy.signal("HUP");
    proxy.wait_for("reload: not reloaded");
    assert!(
        proxy.child.try_wait().unwrap().is_none(),
        "the proxy stopped"
    );

    // And it still stops on SIGTERM.
    proxy.signal("TERM");
    assert!(proxy.exit().success());
}
