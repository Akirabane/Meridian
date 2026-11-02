//! `meridian-panel init`: writes a test bench of one proxy and three Horizon
//! servers (lobby, game1, game2) into a folder, from the programs the user names.
//! It copies nothing and never accepts the Minecraft EULA: it says how the user
//! accepts it themselves, with `accept-eula`.
use std::fmt::Write as _;
use std::hash::{BuildHasher, Hasher};
use std::path::{Path, PathBuf};

/// The EULA a Horizon server asks its owner to accept.
pub const EULA_URL: &str = "https://aka.ms/MinecraftEULA";

/// The flag `accept-eula` needs, so that it is never run by accident.
pub const ACCEPT_FLAG: &str = "--i-accept-the-minecraft-eula";

/// What `init` is given.
#[derive(Clone, Debug)]
pub struct InitOptions {
    /// The bench folder; made if missing.
    pub dir: PathBuf,
    /// The `horizon-server` program, used by all three servers.
    pub horizon_server: PathBuf,
    /// The `meridian` program.
    pub meridian: PathBuf,
    /// The prepared game files, if the servers should not look in their usual places.
    pub resources: Option<PathBuf>,
}

/// The servers of a bench: name, port, message of the day.
pub const SERVERS: [(&str, u16, &str); 3] = [
    ("lobby", 25601, "Meridian bench: lobby"),
    ("game1", 25602, "Meridian bench: game 1"),
    ("game2", 25603, "Meridian bench: game 2"),
];
/// The proxy's port: the address players join.
pub const PROXY_PORT: u16 = 25565;

/// Writes the bench and returns what to tell the user.
pub fn init(options: &InitOptions) -> Result<String, String> {
    for (what, path) in [
        ("horizon-server", &options.horizon_server),
        ("meridian", &options.meridian),
    ] {
        if !path.is_file() {
            return Err(format!("{what}: {} is not a file", path.display()));
        }
    }
    if let Some(resources) = &options.resources {
        if !resources.is_dir() {
            return Err(format!(
                "resources: {} is not a folder",
                resources.display()
            ));
        }
    }
    let dir = &options.dir;
    let bench_file = dir.join("bench.toml");
    if bench_file.exists() {
        return Err(format!(
            "{} already exists: init does not overwrite a bench",
            bench_file.display()
        ));
    }
    let secret = random_hex(32);
    write(
        &dir.join("proxy").join("meridian.toml"),
        &proxy_config(&secret),
    )?;
    for (name, port, motd) in SERVERS {
        let server = dir.join(name);
        write(
            &server.join("server.toml"),
            &server_config(port, motd, options.resources.as_deref()),
        )?;
        // One line, no newline at the end, as Horizon reads it.
        write(&server.join("forwarding.secret"), &secret)?;
    }
    write(&bench_file, &bench_config(options))?;

    let mut out = String::new();
    let _ = writeln!(out, "Wrote a bench in {}:", dir.display());
    let _ = writeln!(
        out,
        "  bench.toml, proxy/meridian.toml, and lobby, game1, game2 with server.toml and forwarding.secret"
    );
    let _ = writeln!(
        out,
        "  The proxy listens on 127.0.0.1:{PROXY_PORT}; the servers on 127.0.0.1:25601 to 25603."
    );
    if options.resources.is_none() {
        let _ = writeln!(
            out,
            "  The servers look for the prepared game files in their usual places; pass --resources to name a folder."
        );
    }
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "The servers will not start until their EULA is accepted. Read it at"
    );
    let _ = writeln!(out, "  {EULA_URL}");
    let _ = writeln!(
        out,
        "and, if you agree to it, accept it yourself for the three servers with:"
    );
    let _ = writeln!(out, "  {}", accept_command(dir));
    let _ = writeln!(out);
    let _ = writeln!(out, "Then run the panel:");
    let _ = writeln!(out, "  meridian-panel \"{}\"", bench_file.display());
    let _ = write!(
        out,
        "Three Horizon servers take about 1.5 GB of memory each when idle, about 4.5 GB in all."
    );
    Ok(out)
}

/// The command the user runs to accept the EULA for the servers of `dir`.
pub fn accept_command(dir: &Path) -> String {
    format!(
        "meridian-panel accept-eula --dir \"{}\" {ACCEPT_FLAG}",
        dir.display()
    )
}

/// `accept-eula`: writes `eula=true` in each server folder of the bench in `dir`.
/// It is what the user runs themselves, with [`ACCEPT_FLAG`]; nothing else in the
/// panel calls it.
pub fn accept_eula(dir: &Path, flag_given: bool) -> Result<String, String> {
    if !flag_given {
        return Err(format!(
            "Read the Minecraft EULA at {EULA_URL}. To accept it for the servers of this \
             bench, run the command again with {ACCEPT_FLAG}."
        ));
    }
    let bench = crate::bench::Bench::load(&dir.join("bench.toml")).map_err(|e| e.to_string())?;
    let mut out = format!("Minecraft EULA: {EULA_URL}\n");
    for p in bench
        .processes
        .iter()
        .filter(|p| p.kind == crate::bench::Kind::Server)
    {
        let text = format!(
            "#By changing the setting below to TRUE you are indicating your agreement to the EULA ({EULA_URL}).\n\
             #Accepted with meridian-panel accept-eula\neula=true\n"
        );
        write(&p.dir.join("eula.txt"), &text)?;
        let _ = writeln!(out, "accepted for {} ({})", p.name, p.dir.display());
    }
    Ok(out.trim_end().to_owned())
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot make {}: {e}", parent.display()))?;
    }
    std::fs::write(path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

fn proxy_config(secret: &str) -> String {
    let mut text = format!(
        r#"# The proxy of a Meridian test bench, written by meridian-panel init.
# Every setting is described in meridian.example.toml.
listen = "127.0.0.1:{PROXY_PORT}"
motd = "Meridian test bench"
# Nobody's account is checked: a bench on one machine. Use "online" anywhere else.
auth-mode = "offline"
connect-timeout-ms = 5000
# The same secret is in each server's forwarding.secret.
forwarding-secret = "{secret}"
default-backend = "lobby"
try = ["game1", "game2"]
"#
    );
    for (name, port, _) in SERVERS {
        let _ = write!(
            text,
            "\n[[backends]]\nname = \"{name}\"\naddress = \"127.0.0.1:{port}\"\n"
        );
    }
    text.push_str(
        r#"
# Every player of an offline bench is unverified, and goes to these servers.
[[group]]
name = "bench"
auth = "unverified"
backends = ["lobby", "game1", "game2"]

[[route]]
auth = "unverified"
group = "bench"
"#,
    );
    text
}

fn server_config(port: u16, motd: &str, resources: Option<&Path>) -> String {
    let resources = resources
        .map(|r| r.display().to_string().replace('\\', "/"))
        .unwrap_or_default();
    format!(
        r#"# A Horizon server of a Meridian test bench, written by meridian-panel init.
[server]
motd = "{motd}"
port = {port}
bind = "127.0.0.1"           # the proxy only
online_mode = false          # the proxy checks players, not the server
whitelist = false

[world]
name = "world"

[engine]
resources = "{resources}"

[backups]
every = "0"

[panel]
bind = ""                    # off: three servers would want the same panel port
"#
    )
}

fn bench_config(options: &InitOptions) -> String {
    let path = |p: &Path| p.display().to_string().replace('\\', "/");
    let mut text = format!(
        r#"# A Meridian test bench, written by meridian-panel init. The panel starts and
# stops these processes and no other. Folders are relative to this file.
panel-port = {}

[[process]]
name = "proxy"
kind = "proxy"
program = "{}"
args = ["meridian.toml"]
dir = "proxy"
port = {PROXY_PORT}
"#,
        crate::bench::DEFAULT_PANEL_PORT,
        path(&options.meridian)
    );
    for (name, port, _) in SERVERS {
        let _ = write!(
            text,
            r#"
[[process]]
name = "{name}"
kind = "server"
program = "{}"
args = ["run"]
dir = "{name}"
port = {port}
"#,
            path(&options.horizon_server)
        );
    }
    text
}

/// `len` random hexadecimal digits, from the operating system's randomness that
/// seeds the standard library's hash keys. Enough for a secret on one machine.
pub fn random_hex(len: usize) -> String {
    let mut out = String::with_capacity(len);
    while out.len() < len {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_usize(out.len());
        let _ = write!(out, "{:016x}", hasher.finish());
    }
    out.truncate(len);
    out
}
