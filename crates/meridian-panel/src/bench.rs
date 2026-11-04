//! The bench file (`bench.toml`): the processes of a test bench, how each one is
//! started and stopped, and the port that tells it is up. The panel starts and
//! stops these processes and no other.
use serde::Deserialize;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// What a process is, which decides how it is stopped and whether it needs an EULA.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// The Meridian proxy.
    Proxy,
    /// A Horizon server.
    Server,
    /// A server's sidecar (`meridian-sidecar`), which has no console.
    Sidecar,
}

impl Kind {
    /// The console command that stops it cleanly; `None` for a process with no
    /// console, which is ended at once.
    pub fn default_stop_command(self) -> Option<&'static str> {
        match self {
            Kind::Proxy => Some("end"),
            Kind::Server => Some("stop"),
            Kind::Sidecar => None,
        }
    }

    /// The name shown on the page.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Proxy => "proxy",
            Kind::Server => "server",
            Kind::Sidecar => "sidecar",
        }
    }
}

/// One process of the bench.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessSpec {
    /// A short name: letters, digits and dashes.
    pub name: String,
    pub kind: Kind,
    /// The program to run.
    pub program: PathBuf,
    pub args: Vec<String>,
    /// The folder it runs in.
    pub dir: PathBuf,
    /// The port on 127.0.0.1 that answers once the process is up.
    pub port: u16,
    /// The console line that stops it.
    pub stop_command: Option<String>,
    /// How long a stop waits before the process is killed.
    pub stop_timeout: Duration,
    /// The name the proxy knows a server by, when it is not the process's own name.
    /// Only for servers.
    pub backend: Option<String>,
}

impl ProcessSpec {
    /// The name of this server in the proxy's configuration.
    pub fn backend_name(&self) -> &str {
        self.backend.as_deref().unwrap_or(&self.name)
    }
}

/// What the panel tells the proxy before it stops or restarts a server, so that no
/// new player is sent to it and, if asked, the players on it are moved away first.
/// Off unless the bench file turns it on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub enabled: bool,
    /// How long the panel waits after telling the proxy, before it stops the server.
    pub grace: Duration,
    /// Also send the players of the server elsewhere (`drain <server> --move`).
    pub move_players: bool,
    /// The proxy to tell; the bench's only proxy when not named.
    pub proxy: Option<String>,
}

/// The grace the panel waits when the file does not say.
pub const DEFAULT_GRACE: Duration = Duration::from_secs(5);

impl Default for Notice {
    fn default() -> Self {
        Self {
            enabled: false,
            grace: DEFAULT_GRACE,
            move_players: false,
            proxy: None,
        }
    }
}

/// The bench file, read and checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bench {
    /// The port of the panel's page, on 127.0.0.1.
    pub panel_port: u16,
    pub processes: Vec<ProcessSpec>,
    pub notice: Notice,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct File {
    panel_port: Option<u16>,
    #[serde(default, rename = "process")]
    processes: Vec<FileProcess>,
    drain_before_stop: Option<FileNotice>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct FileNotice {
    #[serde(default)]
    enabled: bool,
    grace_seconds: Option<u64>,
    #[serde(default)]
    move_players: bool,
    proxy: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct FileProcess {
    name: String,
    kind: Kind,
    program: PathBuf,
    #[serde(default)]
    args: Vec<String>,
    dir: PathBuf,
    port: u16,
    stop_command: Option<String>,
    stop_timeout_seconds: Option<u64>,
    backend: Option<String>,
}

/// The panel's port when the file does not say.
pub const DEFAULT_PANEL_PORT: u16 = 8140;
/// How long a stop waits, when the file does not say: a server saves its world.
pub const DEFAULT_STOP_TIMEOUT: Duration = Duration::from_secs(30);

/// Why a bench file cannot be used.
#[derive(Debug)]
pub struct BenchError(pub String);

impl fmt::Display for BenchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BenchError {}

fn error(message: impl Into<String>) -> BenchError {
    BenchError(message.into())
}

impl Bench {
    /// Reads `path`. Relative folders and programs are taken from the folder the
    /// file is in.
    pub fn load(path: &Path) -> Result<Bench, BenchError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| error(format!("cannot read {}: {e}", path.display())))?;
        let root = path.parent().unwrap_or(Path::new("."));
        Bench::from_toml_str(&text, root)
    }

    /// Reads a bench written in TOML; `root` is the folder relative paths start from.
    pub fn from_toml_str(text: &str, root: &Path) -> Result<Bench, BenchError> {
        let file: File = toml::from_str(text).map_err(|e| error(e.to_string()))?;
        let processes = file
            .processes
            .into_iter()
            .map(|p| ProcessSpec {
                stop_command: p
                    .stop_command
                    .or_else(|| p.kind.default_stop_command().map(str::to_owned)),
                stop_timeout: p
                    .stop_timeout_seconds
                    .map(Duration::from_secs)
                    .unwrap_or(DEFAULT_STOP_TIMEOUT),
                name: p.name,
                kind: p.kind,
                program: resolve(root, &p.program),
                args: p.args,
                dir: resolve(root, &p.dir),
                port: p.port,
                backend: p.backend,
            })
            .collect();
        let notice = file
            .drain_before_stop
            .map_or_else(Notice::default, |n| Notice {
                enabled: n.enabled,
                grace: n.grace_seconds.map_or(DEFAULT_GRACE, Duration::from_secs),
                move_players: n.move_players,
                proxy: n.proxy,
            });
        let bench = Bench {
            panel_port: file.panel_port.unwrap_or(DEFAULT_PANEL_PORT),
            processes,
            notice,
        };
        bench.validate()?;
        Ok(bench)
    }

    fn validate(&self) -> Result<(), BenchError> {
        if self.processes.is_empty() {
            return Err(error("the bench has no [[process]]"));
        }
        if self.panel_port == 0 {
            return Err(error("panel-port must be from 1 to 65535"));
        }
        for (i, p) in self.processes.iter().enumerate() {
            if !valid_name(&p.name) {
                return Err(error(format!(
                    "process name \"{}\" must be letters, digits, dashes or underscores",
                    p.name
                )));
            }
            let before = &self.processes[..i];
            if before.iter().any(|q| q.name == p.name) {
                return Err(error(format!("two processes are named \"{}\"", p.name)));
            }
            if p.port == 0 {
                return Err(error(format!(
                    "process \"{}\": port must be from 1 to 65535",
                    p.name
                )));
            }
            if p.port == self.panel_port {
                return Err(error(format!(
                    "process \"{}\" uses port {}, the panel's own",
                    p.name, p.port
                )));
            }
            if let Some(q) = before.iter().find(|q| q.port == p.port) {
                return Err(error(format!(
                    "processes \"{}\" and \"{}\" both use port {}",
                    q.name, p.name, p.port
                )));
            }
            if p.stop_command
                .as_deref()
                .is_some_and(|c| c.trim().is_empty())
            {
                return Err(error(format!(
                    "process \"{}\": stop-command is empty",
                    p.name
                )));
            }
            if let Some(backend) = &p.backend {
                if p.kind != Kind::Server {
                    return Err(error(format!(
                        "process \"{}\": backend is for a server, which the proxy knows by a name",
                        p.name
                    )));
                }
                if !valid_name(backend) {
                    return Err(error(format!(
                        "process \"{}\": backend \"{backend}\" must be letters, digits, dashes or                          underscores",
                        p.name
                    )));
                }
            }
        }
        self.validate_notice()
    }

    fn validate_notice(&self) -> Result<(), BenchError> {
        let notice = &self.notice;
        if let Some(name) = &notice.proxy {
            match self.process(name) {
                Some(p) if p.kind == Kind::Proxy => {}
                Some(_) => {
                    return Err(error(format!(
                        "[drain-before-stop]: \"{name}\" is not a proxy"
                    )));
                }
                None => {
                    return Err(error(format!(
                        "[drain-before-stop]: no process called \"{name}\" in the bench"
                    )));
                }
            }
        }
        if notice.enabled && self.proxy_of_notice().is_none() {
            return Err(error(
                "[drain-before-stop] is enabled: name the proxy to tell with proxy = \"...\",                  the bench has not exactly one proxy",
            ));
        }
        Ok(())
    }

    /// The proxy the panel tells before it stops a server: the one the file names,
    /// or the bench's only proxy.
    pub fn proxy_of_notice(&self) -> Option<&ProcessSpec> {
        match &self.notice.proxy {
            Some(name) => self.process(name),
            None => {
                let mut proxies = self.processes.iter().filter(|p| p.kind == Kind::Proxy);
                let first = proxies.next();
                if proxies.next().is_some() {
                    None
                } else {
                    first
                }
            }
        }
    }

    /// The process called `name`.
    pub fn process(&self, name: &str) -> Option<&ProcessSpec> {
        self.processes.iter().find(|p| p.name == name)
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn resolve(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        root.join(path)
    }
}

/// Whether the server in `dir` has its EULA accepted: `eula.txt` has the line
/// `eula=true`, as vanilla reads it.
pub fn eula_accepted(dir: &Path) -> bool {
    std::fs::read_to_string(dir.join("eula.txt"))
        .map(|text| {
            text.lines()
                .map(str::trim)
                .any(|l| l.eq_ignore_ascii_case("eula=true"))
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE: &str = r#"
[[process]]
name = "proxy"
kind = "proxy"
program = "meridian.exe"
args = ["meridian.toml"]
dir = "proxy"
port = 25565
"#;

    fn root() -> &'static Path {
        Path::new("/bench")
    }

    #[test]
    fn a_process_takes_its_defaults_and_paths_start_from_the_file() {
        let bench = Bench::from_toml_str(ONE, root()).unwrap();
        assert_eq!(bench.panel_port, DEFAULT_PANEL_PORT);
        let p = &bench.processes[0];
        assert_eq!(p.stop_command.as_deref(), Some("end"));
        assert_eq!(p.stop_timeout, DEFAULT_STOP_TIMEOUT);
        assert_eq!(p.dir, Path::new("/bench").join("proxy"));
        assert_eq!(p.program, Path::new("/bench").join("meridian.exe"));
        assert_eq!(p.args, ["meridian.toml"]);
    }

    #[test]
    fn a_server_stops_with_stop() {
        let text = ONE.replace("kind = \"proxy\"", "kind = \"server\"");
        let bench = Bench::from_toml_str(&text, root()).unwrap();
        assert_eq!(bench.processes[0].stop_command.as_deref(), Some("stop"));
        let text = ONE.replace("kind = \"proxy\"", "kind = \"sidecar\"");
        let bench = Bench::from_toml_str(&text, root()).unwrap();
        assert_eq!(bench.processes[0].stop_command, None);
    }

    #[test]
    fn bad_benches_are_refused() {
        let twice = format!("{ONE}{}", ONE.replace("25565", "25566"));
        let same_port = format!("{ONE}{}", ONE.replace("\"proxy\"\nkind", "\"lobby\"\nkind"));
        for (text, says) in [
            ("panel-port = 8140", "no [[process]]"),
            (twice.as_str(), "two processes are named"),
            (same_port.as_str(), "both use port 25565"),
            (
                &ONE.replace("port = 25565", "port = 8140") as &str,
                "the panel's own",
            ),
            (
                &ONE.replace("\"proxy\"\nkind", "\"pro xy\"\nkind") as &str,
                "letters, digits",
            ),
            (
                &ONE.replace("kind = \"proxy\"", "kind = \"client\"") as &str,
                "unknown variant",
            ),
            (&format!("{ONE}shell = \"rm\"") as &str, "unknown field"),
        ] {
            let e = Bench::from_toml_str(text, root()).unwrap_err();
            assert!(e.to_string().contains(says), "{says}: {e}");
        }
    }

    #[test]
    fn telling_the_proxy_is_off_unless_the_file_turns_it_on() {
        let bench = Bench::from_toml_str(ONE, root()).unwrap();
        assert!(!bench.notice.enabled);
        assert_eq!(bench.notice, Notice::default());
        assert_eq!(bench.notice.grace, DEFAULT_GRACE);
    }

    #[test]
    fn the_notice_is_read_with_the_only_proxy_by_default() {
        let text = format!(
            "{ONE}
[drain-before-stop]
enabled = true
grace-seconds = 2
move-players = true
"
        );
        let bench = Bench::from_toml_str(&text, root()).unwrap();
        assert!(bench.notice.enabled && bench.notice.move_players);
        assert_eq!(bench.notice.grace, Duration::from_secs(2));
        assert_eq!(bench.proxy_of_notice().unwrap().name, "proxy");
    }

    #[test]
    fn a_server_has_a_backend_name_that_defaults_to_its_own() {
        let server = ONE
            .replace("kind = \"proxy\"", "kind = \"server\"")
            .replace(
                "\"proxy\"
kind",
                "\"lobby-card\"
kind",
            );
        let bench = Bench::from_toml_str(&server, root()).unwrap();
        assert_eq!(bench.processes[0].backend_name(), "lobby-card");
        let named = format!(
            "{server}backend = \"lobby\"
"
        );
        let bench = Bench::from_toml_str(&named, root()).unwrap();
        assert_eq!(bench.processes[0].backend_name(), "lobby");
    }

    #[test]
    fn a_bad_notice_is_refused() {
        let two = format!(
            "{ONE}{}",
            ONE.replace(
                "\"proxy\"
kind",
                "\"proxy2\"
kind"
            )
            .replace("25565", "25566")
        );
        let server = ONE.replace("kind = \"proxy\"", "kind = \"server\"");
        for (text, says) in [
            (
                // Two proxies: which one to tell?
                format!(
                    "{two}
[drain-before-stop]
enabled = true
"
                ),
                "not exactly one proxy",
            ),
            (
                format!(
                    "{ONE}
[drain-before-stop]
proxy = \"nowhere\"
"
                ),
                "no process called",
            ),
            (
                format!(
                    "{server}
[drain-before-stop]
proxy = \"proxy\"
"
                ),
                "is not a proxy",
            ),
            (
                format!(
                    "{ONE}backend = \"lobby\"
"
                ),
                "backend is for a server",
            ),
            (
                format!(
                    "{server}backend = \"lo by\"
"
                ),
                "must be letters, digits",
            ),
            (
                format!(
                    "{ONE}
[drain-before-stop]
speed = 3
"
                ),
                "unknown field",
            ),
        ] {
            let e = Bench::from_toml_str(&text, root()).unwrap_err();
            assert!(e.to_string().contains(says), "{says}: {e}");
        }
    }

    #[test]
    fn the_eula_is_read_as_vanilla_reads_it() {
        let dir = std::env::temp_dir().join(format!("panel-eula-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!eula_accepted(&dir));
        std::fs::write(dir.join("eula.txt"), "#comment\neula=false\n").unwrap();
        assert!(!eula_accepted(&dir));
        std::fs::write(
            dir.join("eula.txt"),
            "#By changing the setting below...\neula=true\n",
        )
        .unwrap();
        assert!(eula_accepted(&dir));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
