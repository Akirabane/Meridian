//! An example program of the SDK (crates/meridian-extension/examples/) run as a separate process: the program
//! cargo builds for it, started with a token file, port 0 and the arguments of the example. The port the system
//! picks is read from the line the SDK writes when it listens.
//!
//! Every wait is for something to happen (a line the program writes), bounded, and fails with what was seen; no
//! port is fixed. The process is killed when the value is dropped, whether the test passed or not.
use super::*;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::sync::mpsc::{self, Receiver};

/// The longest wait for the program to say where it listens, or for a line it writes.
pub(super) const WAIT: Duration = Duration::from_secs(10);

/// The longest wait for cargo to build a program: usually nothing to do when `cargo test` built it already.
const BUILD: Duration = Duration::from_secs(300);

/// The program of the example `name`, built by cargo once for the tests; cargo says where it put it.
fn program(name: &str) -> PathBuf {
    static PROGRAMS: OnceLock<Mutex<HashMap<String, PathBuf>>> = OnceLock::new();
    let mut programs = PROGRAMS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(path) = programs.get(name) {
        return path.clone();
    }
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| env!("CARGO").into());
    let mut build = Command::new(cargo);
    build
        .args(["build", "-p", "meridian-extension", "--example", name])
        .arg("--message-format=json")
        .current_dir(env!("CARGO_MANIFEST_DIR"));
    let (send, built) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = send.send(build.output());
    });
    let output = built
        .recv_timeout(BUILD)
        .unwrap_or_else(|_| panic!("cargo did not build the example {name} in time"))
        .unwrap_or_else(|e| panic!("cannot run cargo to build the example {name}: {e}"));
    let messages = String::from_utf8_lossy(&output.stdout);
    let path = messages
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|m| m["reason"] == "compiler-artifact" && m["target"]["name"] == name)
        .find_map(|m| m["executable"].as_str().map(PathBuf::from))
        .unwrap_or_else(|| {
            panic!(
                "cargo gave no program for the example {name} ({}):\n{}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            )
        });
    programs.insert(name.to_owned(), path.clone());
    path
}

/// The lines of `stream`, as they come.
fn follow(stream: impl Read + Send + 'static) -> Receiver<String> {
    let (send, lines) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            if send.send(line).is_err() {
                break;
            }
        }
    });
    lines
}

/// An example's program while it runs; killed when dropped.
pub(super) struct Example {
    name: &'static str,
    child: Child,
    pub(super) folder: Folder,
    pub(super) address: String,
    /// What it writes on its standard output.
    lines: Receiver<String>,
    /// What it and the SDK write on the standard error stream.
    errors: Receiver<String>,
}

impl Example {
    /// Starts the example `name` with its token file, `127.0.0.1:0` and `args`, and waits until it listens.
    pub(super) fn start(name: &'static str, args: &[&OsStr]) -> Self {
        let folder = Folder::new();
        let mut child = Command::new(program(name))
            .arg(folder.token())
            .arg("127.0.0.1:0")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("cannot start the example {name}: {e}"));
        let lines = follow(child.stdout.take().unwrap());
        let errors = follow(child.stderr.take().unwrap());
        let mut example = Self {
            name,
            child,
            folder,
            address: String::new(),
            lines,
            errors,
        };
        // The SDK says where it listens: the port the system picked.
        let line = example.said(" listens on ");
        example.address = line.split_once(" listens on ").unwrap().1.trim().to_owned();
        example
    }

    /// Waits for a line on the standard error stream that holds `text`, and returns it.
    pub(super) fn said(&self, text: &str) -> String {
        let until = Instant::now() + WAIT;
        let mut seen = Vec::new();
        loop {
            let left = until.saturating_duration_since(Instant::now());
            match self.errors.recv_timeout(left) {
                Ok(line) if line.contains(text) => return line,
                Ok(line) => seen.push(line),
                Err(_) => panic!(
                    "{} wrote no line with {text:?}; it wrote:\n{}",
                    self.name,
                    seen.join("\n")
                ),
            }
        }
    }

    /// The next `count` lines it writes on its standard output, each without its time, which is checked to be a
    /// whole number of milliseconds (not against the test's clock, which a machine may set again while it runs).
    pub(super) fn wrote(&self, count: usize) -> Vec<String> {
        let until = Instant::now() + WAIT;
        let mut lines = Vec::new();
        while lines.len() < count {
            let left = until.saturating_duration_since(Instant::now());
            let Ok(line) = self.lines.recv_timeout(left) else {
                panic!(
                    "{} wrote {} lines of {count}: {lines:?}",
                    self.name,
                    lines.len()
                );
            };
            let (time, rest) = line.split_once(' ').unwrap_or_else(|| panic!("{line:?}"));
            assert!(time.parse::<u128>().is_ok(), "{line:?}");
            lines.push(rest.to_owned());
        }
        lines
    }

    /// Kills the program and waits until it is gone.
    pub(super) fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Example {
    fn drop(&mut self) {
        self.stop();
    }
}
