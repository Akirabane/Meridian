//! The proxy's console: one command a line on standard input, read on a thread of
//! its own so that players keep being accepted while it waits. Answers go to
//! standard output; what a command changes is also logged.
//!
//! The commands are a table of [`Command`]s, each a name, a usage line, a line of
//! help and the function that runs it. [`Commands::register`] adds one, so the
//! proxy's own extensions can add theirs later.
use crate::log::{debug, info, warning};
use crate::session::{Player, hyphenated};
use crate::state::Proxy;
use meridian_config::Config;
use std::io::BufRead;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

/// What a command answers: lines to show, or a line saying what was wrong.
pub type Reply = Result<Vec<String>, String>;

/// What runs a command: the proxy, the table (for `help`), and the words after
/// the command's name.
pub type Run = Box<dyn Fn(&Proxy, &Commands, &[&str]) -> Reply + Send + Sync>;

/// One console command.
pub struct Command {
    pub name: &'static str,
    /// The command with its arguments, as `send <player|all> <server>`.
    pub usage: &'static str,
    pub help: &'static str,
    pub run: Run,
}

/// The commands the console knows.
pub struct Commands {
    list: Vec<Command>,
}

impl Commands {
    /// The proxy's own commands.
    pub fn builtin() -> Self {
        let mut commands = Commands { list: Vec::new() };
        for command in [
            command(
                "help",
                "help [command]",
                "lists the commands, or explains one",
                help,
            ),
            command(
                "list",
                "list",
                "lists the players online and where they are",
                list,
            ),
            command(
                "servers",
                "servers",
                "lists the backends and whether each one answers",
                servers,
            ),
            command(
                "send",
                "send <player|all> <server>",
                "moves players to another backend (not available yet)",
                send,
            ),
            command(
                "kick",
                "kick <player> [reason]",
                "disconnects a player, telling them why",
                kick,
            ),
            command(
                "reload",
                "reload",
                "reads the configuration file again and applies what it can",
                reload,
            ),
            command("end", "end", "stops the proxy, as Ctrl+C does", end),
        ] {
            commands
                .register(command)
                .expect("built-in names are unique");
        }
        commands
    }

    /// Adds a command. A name that is already taken is refused.
    pub fn register(&mut self, command: Command) -> Result<(), String> {
        if self.find(command.name).is_some() {
            return Err(format!("a command called {} already exists", command.name));
        }
        self.list.push(command);
        Ok(())
    }

    fn find(&self, name: &str) -> Option<&Command> {
        self.list.iter().find(|c| c.name.eq_ignore_ascii_case(name))
    }

    /// Runs one line of input and returns what to show.
    pub fn execute(&self, proxy: &Proxy, line: &str) -> Vec<String> {
        let words: Vec<&str> = line.split_whitespace().collect();
        let Some((name, args)) = words.split_first() else {
            return Vec::new();
        };
        let Some(command) = self.find(name) else {
            return vec![format!("unknown command {name}; type help for the list")];
        };
        match (command.run)(proxy, self, args) {
            Ok(lines) => lines,
            Err(problem) => vec![problem],
        }
    }
}

fn command(
    name: &'static str,
    usage: &'static str,
    help: &'static str,
    run: fn(&Proxy, &Commands, &[&str]) -> Reply,
) -> Command {
    Command {
        name,
        usage,
        help,
        run: Box::new(run),
    }
}

/// Reads commands from standard input until it closes, on a thread of its own.
pub fn start(proxy: Arc<Proxy>) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("console".to_owned())
        .spawn(move || {
            let commands = Commands::builtin();
            for line in std::io::stdin().lock().lines() {
                let line = match line {
                    Ok(line) => line,
                    Err(e) => {
                        warning!("the console cannot read its input: {e}");
                        break;
                    }
                };
                for answer in commands.execute(&proxy, &line) {
                    println!("{answer}");
                }
                if proxy.stopping() {
                    break;
                }
            }
            // A proxy run without a terminal has no console: it keeps running.
            debug!("the console's input is closed");
        })
}

fn usage(command: &str, commands: &Commands) -> String {
    match commands.find(command) {
        Some(c) => format!("usage: {}", c.usage),
        None => format!("usage: {command}"),
    }
}

fn help(_: &Proxy, commands: &Commands, args: &[&str]) -> Reply {
    match args {
        [] => Ok(commands
            .list
            .iter()
            .map(|c| format!("{:<28} {}", c.usage, c.help))
            .collect()),
        [name] => match commands.find(name) {
            Some(c) => Ok(vec![format!("{}: {}", c.usage, c.help)]),
            None => Err(format!("unknown command {name}; type help for the list")),
        },
        _ => Err(usage("help", commands)),
    }
}

fn list(proxy: &Proxy, commands: &Commands, args: &[&str]) -> Reply {
    if !args.is_empty() {
        return Err(usage("list", commands));
    }
    let players = proxy.sessions.players();
    let mut lines = vec![match players.len() {
        0 => return Ok(vec!["no players online".to_owned()]),
        1 => "1 player online:".to_owned(),
        n => format!("{n} players online:"),
    }];
    lines.extend(players.iter().map(describe_player));
    Ok(lines)
}

fn describe_player(player: &Player) -> String {
    format!(
        "  {} ({}) on {} for {}, from {}",
        player.name,
        hyphenated(player.uuid),
        player.backend,
        duration(player.joined.elapsed()),
        player.address.ip()
    )
}

/// `1 h 02 min`, `3 min 05 s`, `7 s`.
fn duration(d: Duration) -> String {
    let s = d.as_secs();
    match (s / 3600, s / 60 % 60, s % 60) {
        (0, 0, s) => format!("{s} s"),
        (0, m, s) => format!("{m} min {s:02} s"),
        (h, m, _) => format!("{h} h {m:02} min"),
    }
}

fn servers(proxy: &Proxy, commands: &Commands, args: &[&str]) -> Reply {
    if !args.is_empty() {
        return Err(usage("servers", commands));
    }
    let config = proxy.config();
    let timeout = Duration::from_millis(config.connect_timeout_ms);
    let players = proxy.sessions.players();
    // The backends are asked at the same time, so a dead one costs one timeout.
    let answers: Vec<Result<Duration, String>> = std::thread::scope(|scope| {
        let asked: Vec<_> = config
            .backends
            .iter()
            .map(|b| scope.spawn(|| (proxy.probe)(&b.address, timeout)))
            .collect();
        asked
            .into_iter()
            .map(|t| {
                t.join()
                    .unwrap_or_else(|_| Err("the check failed".to_owned()))
            })
            .collect()
    });
    Ok(config
        .backends
        .iter()
        .zip(answers)
        .map(|(b, answer)| {
            let default = if b.name == config.default_backend {
                " (default)"
            } else {
                ""
            };
            let on = players.iter().filter(|p| p.backend == b.name).count();
            let state = match answer {
                Ok(took) => format!("answers in {} ms", took.as_millis()),
                Err(e) => format!("does not answer: {e}"),
            };
            format!(
                "  {}{default} at {}: {state}, {}",
                b.name,
                b.address,
                count(on, "player", "players")
            )
        })
        .collect())
}

fn count(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

fn send(proxy: &Proxy, commands: &Commands, args: &[&str]) -> Reply {
    let [who, server] = args else {
        return Err(usage("send", commands));
    };
    let config = proxy.config();
    let Some(backend) = config.backend(server) else {
        return Err(format!(
            "there is no server called {server}; type servers for the list"
        ));
    };
    let players: Vec<Player> = if who.eq_ignore_ascii_case("all") {
        proxy.sessions.players()
    } else {
        match proxy.sessions.find(who) {
            Some(player) => vec![player],
            None => return Err(format!("{who} is not online")),
        }
    };
    let moving: Vec<&Player> = players
        .iter()
        .filter(|p| p.backend != backend.name)
        .collect();
    if moving.is_empty() {
        return Err(if players.is_empty() {
            "no players online".to_owned()
        } else {
            format!("already on {}", backend.name)
        });
    }
    let names: Vec<&str> = moving.iter().map(|p| p.name.as_str()).collect();
    let line = format!(
        "transfer not available yet: {} would move to {}",
        names.join(", "),
        backend.name
    );
    info!("{line}");
    Ok(vec![line])
}

fn kick(proxy: &Proxy, commands: &Commands, args: &[&str]) -> Reply {
    let Some((who, reason)) = args.split_first() else {
        return Err(usage("kick", commands));
    };
    let reason = if reason.is_empty() {
        "Kicked by an operator".to_owned()
    } else {
        reason.join(" ")
    };
    match proxy.sessions.kick(who, &reason) {
        Some(player) => {
            let line = format!("kicked {} from {}: {reason}", player.name, player.backend);
            info!("{line}");
            Ok(vec![line])
        }
        None => Err(format!("{who} is not online")),
    }
}

fn end(proxy: &Proxy, commands: &Commands, args: &[&str]) -> Reply {
    if !args.is_empty() {
        return Err(usage("end", commands));
    }
    proxy.stop();
    Ok(vec!["stopping".to_owned()])
}

fn reload(proxy: &Proxy, commands: &Commands, args: &[&str]) -> Reply {
    if !args.is_empty() {
        return Err(usage("reload", commands));
    }
    let new = Config::load(&proxy.config_path).map_err(|e| {
        format!(
            "not reloaded, {} is unchanged in use: {e}",
            proxy.config_path.display()
        )
    })?;
    Ok(apply(proxy, new))
}

/// Puts a checked configuration in force and says what changed. No player is
/// disconnected: what concerns a login applies to the logins that come next, and
/// the listening address only to the next start.
fn apply(proxy: &Proxy, new: Config) -> Vec<String> {
    let old = proxy.config();
    let mut lines = Vec::new();
    if new.motd != old.motd {
        lines.push(format!("motd: {}", new.motd));
    }
    for b in &new.backends {
        match old.backend(&b.name) {
            None => lines.push(format!("backend {} added at {}", b.name, b.address)),
            Some(was) if was.address != b.address => {
                lines.push(format!("backend {} moved to {}", b.name, b.address));
            }
            Some(_) => {}
        }
    }
    let players = proxy.sessions.players();
    for b in &old.backends {
        if new.backend(&b.name).is_none() {
            let on = players.iter().filter(|p| p.backend == b.name).count();
            lines.push(match on {
                0 => format!("backend {} removed", b.name),
                n => format!(
                    "backend {} removed; {} on it stay until they leave",
                    b.name,
                    count(n, "player", "players")
                ),
            });
        }
    }
    if new.default_backend != old.default_backend {
        lines.push(format!("default backend: {}", new.default_backend));
    }
    let next = "for the next logins";
    if new.online_mode != old.online_mode {
        let mode = if new.online_mode { "on" } else { "off" };
        lines.push(format!("online mode {mode} {next}"));
    }
    if new.compression_threshold != old.compression_threshold {
        lines.push(format!(
            "compression threshold {} {next}",
            new.compression_threshold
        ));
    }
    if new.connect_timeout_ms != old.connect_timeout_ms {
        lines.push(format!("connect timeout {} ms", new.connect_timeout_ms));
    }
    if new.forwarding_secret != old.forwarding_secret {
        lines.push(format!("forwarding secret changed {next}"));
    }
    if new.listen != old.listen {
        lines.push(format!(
            "listen {} needs a restart; the proxy still listens on {}",
            new.listen, old.listen
        ));
    }
    if lines.is_empty() {
        return vec!["reloaded: nothing changed".to_owned()];
    }
    proxy.set_config(new);
    for line in &lines {
        info!("reload: {line}");
    }
    lines.insert(0, "reloaded:".to_owned());
    lines
}
