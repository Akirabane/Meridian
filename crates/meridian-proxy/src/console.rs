//! The proxy's console: one command a line on standard input, read on a thread of
//! its own so that players keep being accepted while it waits. Answers go to
//! standard output; what a command changes is also logged.
//!
//! The commands are a table of [`Command`]s, each a name, a usage line, a line of
//! help and the function that runs it. [`Commands::register`] adds one, so the
//! proxy's own extensions can add theirs later.
use crate::access;
use crate::log::{debug, info, warning};
use crate::permissions;
use crate::session::{Player, hyphenated};
use crate::staff;
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
                "lists the backends, whether each one answers, and the role and state of those that have one",
                servers,
            ),
            command(
                "drain",
                "drain <server> [--move]",
                "stops offering a server with a role to new players; its players stay, or with --move are sent one at a time to other servers of the same role",
                drain,
            ),
            command(
                "undrain",
                "undrain <server>",
                "offers a drained server to new players again",
                undrain,
            ),
            command(
                "switch",
                "switch <player> <server>",
                "moves a player to another backend while they stay connected",
                switch,
            ),
            command(
                "send",
                "send <player|all> <server>",
                "moves players to another backend, one after the other, as switch does",
                send,
            ),
            command(
                "kick",
                "kick <player> [reason]",
                "disconnects a player, telling them why",
                kick,
            ),
            command(
                "ban",
                "ban <name|uuid|ip> [reason]",
                "bans a player, an account or an address (or a range, as 203.0.113.0/24), and ends the sessions it holds",
                access::commands::ban,
            ),
            command(
                "pardon",
                "pardon <name|uuid|ip>",
                "lifts a ban",
                access::commands::pardon,
            ),
            command(
                "banlist",
                "banlist",
                "lists the bans",
                access::commands::banlist,
            ),
            command(
                "whitelist",
                "whitelist <add|remove> <name|uuid> | list | on | off",
                "lets in only the players of the whitelist while it is on",
                access::commands::whitelist,
            ),
            command(
                "alert",
                "alert [server:<name>] <message>",
                "shows a message in the chat of the players online, or of those on one server",
                alert,
            ),
            command(
                "perm",
                "perm list [player] | check <player> <node> | grant|revoke <player> <group> | group <group> create|delete|allow <node>|deny <node>|unset <node>|inherit <group>|uninherit <group>",
                "manages the permission groups and the players in them (docs/OPERATING.md, section 5.9)",
                permissions::commands::perm,
            ),
            command(
                "queue",
                "queue",
                "lists the players who wait for a place, when the queue is on",
                crate::queue::command,
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

pub(crate) fn usage(command: &str, commands: &Commands) -> String {
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
        .flat_map(|(b, answer)| {
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
            let line = format!(
                "  {}{default} at {}: {state}, {}",
                b.name,
                b.address,
                count(on, "player", "players")
            );
            // A backend with a role has a second line: what the balancer makes of it.
            let balancing = crate::placing::describe(
                &config,
                &proxy.sessions.shared(),
                &b.name,
                on,
                proxy.restarts.describe(&b.name).as_deref(),
            )
            .map(|l| format!("    {l}"));
            std::iter::once(line).chain(balancing)
        })
        .collect())
}

fn drain(proxy: &Proxy, commands: &Commands, args: &[&str]) -> Reply {
    let (server, moving) = match args {
        [server] => (server, false),
        [server, flag] if flag.eq_ignore_ascii_case("--move") => (server, true),
        _ => return Err(usage("drain", commands)),
    };
    let config = proxy.config();
    if config.balanced(server).is_none() {
        return Err(format!(
            "{server} has no role, so there is nothing to drain; it takes players only where              a route, a forced host or a command sends them"
        ));
    }
    let newly = proxy.sessions.balancer.drain(server);
    // A backend the detector drained stays drained when it is back: the operator drained it too.
    proxy.restarts.keep_drained(server);
    if newly {
        info!("backend \"{server}\" is drained from the console");
    }
    let mut lines = vec![match (newly, moving) {
        (true, false) => format!(
            "{server} takes no new player; its players stay. undrain {server} offers it again"
        ),
        (true, true) => format!(
            "{server} takes no new player; its players are being sent to other servers of its role"
        ),
        (false, _) => format!("{server} is already drained"),
    }];
    if moving {
        lines.extend(evacuate(proxy, &config, server));
    }
    Ok(lines)
}

/// Sends the players of the drained `server` to other servers of its role, one at a
/// time (see `evacuate`). The console waits for the last move, and the log has each.
fn evacuate(proxy: &Proxy, config: &Config, server: &str) -> Vec<String> {
    match crate::evacuate::send_away(&proxy.sessions, config, server) {
        Some(outcome) => outcome.report(server),
        None => vec![format!("no player is on {server}")],
    }
}

fn undrain(proxy: &Proxy, commands: &Commands, args: &[&str]) -> Reply {
    let [server] = args else {
        return Err(usage("undrain", commands));
    };
    if !proxy.sessions.balancer.undrain(server) {
        return Ok(vec![format!("{server} is not drained")]);
    }
    info!("backend \"{server}\" is offered again");
    Ok(vec![format!("{server} is offered to new players again")])
}

fn count(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

fn switch(proxy: &Proxy, commands: &Commands, args: &[&str]) -> Reply {
    match args {
        [who, _] if who.eq_ignore_ascii_case("all") => Err(format!(
            "{}; to move every player, type send all <server>",
            usage("switch", commands)
        )),
        [who, server] => move_players(proxy, who, server),
        _ => Err(usage("switch", commands)),
    }
}

fn send(proxy: &Proxy, commands: &Commands, args: &[&str]) -> Reply {
    let [who, server] = args else {
        return Err(usage("send", commands));
    };
    move_players(proxy, who, server)
}

/// Moves the player `who`, or every player for `all`, to `server`, one after the
/// other: a line for each, which says how it went or at which step it failed.
/// Each player is told too.
fn move_players(proxy: &Proxy, who: &str, server: &str) -> Reply {
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
    let moving: Vec<Player> = moving.into_iter().cloned().collect();
    let (mut lines, failed) = staff::move_all(
        &config,
        &proxy.sessions.shared(),
        &proxy.sessions.switcher,
        &moving,
        &backend.name,
    );
    match (moving.len(), failed) {
        (1, 1) => Err(lines.remove(0)),
        _ => Ok(lines),
    }
}

fn alert(proxy: &Proxy, commands: &Commands, args: &[&str]) -> Reply {
    let (server, message) = match args {
        [first, message @ ..] if first.starts_with("server:") => (Some(&first[7..]), message),
        message => (None, message),
    };
    if message.is_empty() || server.is_some_and(str::is_empty) {
        return Err(usage("alert", commands));
    }
    let message = message.join(" ");
    let told = staff::alert(&proxy.sessions.shared(), &proxy.config(), server, &message)?;
    let line = match told {
        1 => "alert shown to 1 player".to_owned(),
        n => format!("alert shown to {n} players"),
    };
    info!("{line}: {message}");
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
    reload_file(proxy)
}

/// Reads the configuration file again and puts it in force: what `reload` does, and
/// what SIGHUP does. A file that does not pass the checks changes nothing.
pub fn reload_file(proxy: &Proxy) -> Reply {
    let new = Config::load(&proxy.config_path).map_err(|e| {
        format!(
            "not reloaded, {} is unchanged in use: {e}",
            proxy.config_path.display()
        )
    })?;
    // The bans and the whitelist are read before anything is applied, so that a file
    // that cannot be read changes nothing.
    let file = new.access.file.clone();
    let lists = access::read(file.as_deref()).map_err(|e| {
        format!("not reloaded, the bans and the whitelist are unchanged in use: {e}")
    })?;
    let perm_file = new.permissions.file.clone();
    let perms = permissions::read(perm_file.as_deref())
        .map_err(|e| format!("not reloaded, the permissions are unchanged in use: {e}"))?;
    let warnings: Vec<String> = new
        .balancing_warnings()
        .into_iter()
        .map(|w| format!("warning: {w}"))
        .collect();
    let mut lines = apply(proxy, new);
    let mut more = Vec::new();
    if let Some(perms) = perms {
        more.extend(permissions::commands::reloaded(proxy, perms, perm_file));
    }
    if !more.is_empty() {
        if lines.len() == 1 {
            lines = vec!["reloaded:".to_owned()];
        }
        lines.extend(more);
    }
    if let Some(lists) = lists {
        let more = access::commands::reloaded(proxy, lists, file);
        if !more.is_empty() {
            if lines.len() == 1 {
                lines = vec!["reloaded:".to_owned()];
            }
            lines.extend(more);
        }
    }
    lines.extend(warnings);
    Ok(lines)
}

/// Puts a checked configuration in force and says what changed. No player is
/// disconnected: what concerns a login applies to the logins that come next, and
/// the listening address only to the next start.
fn apply(proxy: &Proxy, new: Config) -> Vec<String> {
    let old = proxy.config();
    if *old == new {
        return vec!["reloaded: nothing changed".to_owned()];
    }
    let mut lines = Vec::new();
    if new.motd != old.motd {
        lines.push(format!("motd: {}", new.motd));
    }
    if new.favicon != old.favicon {
        lines.push(match &new.favicon {
            Some(f) => format!("favicon: {}", f.path.display()),
            None => "favicon: none".to_owned(),
        });
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
                1 => format!(
                    "backend {} removed; the player on it stays until they leave",
                    b.name
                ),
                n => format!(
                    "backend {} removed; the {n} players on it stay until they leave",
                    b.name
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
    if new.try_backends != old.try_backends {
        lines.push(match new.try_backends.as_slice() {
            [] => "no backends to try after the default one".to_owned(),
            names => format!(
                "backends to try after the default one: {}",
                names.join(", ")
            ),
        });
    }
    if new.max_players != old.max_players {
        lines.push(match new.max_players {
            0 => format!("no player limit {next}"),
            n => format!("player limit {n} {next}"),
        });
    }
    if new.forced_hosts != old.forced_hosts {
        lines.push(format!("forced hosts changed {next}"));
    }
    if new.auth_mode != old.auth_mode {
        lines.push(format!("login mode {:?} {next}", new.auth_mode).to_lowercase());
    }
    if new.groups != old.groups || new.routes != old.routes {
        lines.push(format!("groups and routes changed {next}"));
    }
    if new.listen != old.listen {
        lines.push(format!(
            "listen {} needs a restart; the proxy still listens on {}",
            new.listen, old.listen
        ));
    }
    // The extensions the new configuration enables run, and the others do not.
    lines.extend(proxy.extensions.follow(&new));
    if lines.is_empty() {
        // A setting this list does not name yet still takes effect.
        lines.push(format!("settings changed {next}"));
    }
    proxy.set_config(new);
    for line in &lines {
        info!("reload: {line}");
    }
    lines.insert(0, "reloaded:".to_owned());
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{Open, tests::idle_session_on};
    use horizon_protocol::{
        connection::Connection, crypto::offline_uuid, packets::configuration, text,
    };
    use meridian_config::{AuthMode, Backend};
    use std::sync::atomic::AtomicBool;
    use std::thread::JoinHandle;
    use std::time::Instant;

    fn config() -> Config {
        Config {
            backends: vec![
                Backend {
                    name: "lobby".into(),
                    address: "127.0.0.1:25601".into(),
                },
                Backend {
                    name: "east".into(),
                    address: "127.0.0.1:25602".into(),
                },
            ],
            default_backend: "lobby".into(),
            // The pair the configuration's check requires for an offline proxy.
            online_mode: false,
            auth_mode: AuthMode::Offline,
            forwarding_secret: "secret".into(),
            ..Config::default()
        }
    }

    /// A proxy whose backend check says lobby answers in 3 ms and east refuses.
    fn proxy() -> Proxy {
        let mut proxy = Proxy::new(
            config(),
            "does-not-exist/meridian.toml".into(),
            Arc::new(AtomicBool::new(false)),
        );
        proxy.probe = Arc::new(|address, _| match address {
            "127.0.0.1:25601" => Ok(Duration::from_millis(3)),
            _ => Err("connection refused".to_owned()),
        });
        proxy
    }

    /// A player relayed between two idle pipes, counted in the proxy's sessions.
    struct Joined<'a> {
        _open: Open<'a>,
        relay: JoinHandle<meridian_net::Result<meridian_net::relay::Report>>,
        player: Connection,
        _server: Connection,
    }

    fn join<'a>(proxy: &'a Proxy, name: &str, backend: &str) -> Joined<'a> {
        let (relay, handle, player, server) = idle_session_on(name, backend);
        let open = proxy
            .sessions
            .open(
                Player {
                    name: name.into(),
                    uuid: offline_uuid(name).to_be_bytes(),
                    backend: backend.into(),
                    address: "198.51.100.7:50123".parse().unwrap(),
                    joined: Instant::now(),
                },
                handle,
            )
            .unwrap();
        Joined {
            _open: open,
            relay: std::thread::spawn(move || relay.run()),
            player,
            _server: server,
        }
    }

    fn run(proxy: &Proxy, line: &str) -> Vec<String> {
        Commands::builtin().execute(proxy, line)
    }

    #[test]
    fn help_lists_every_command_and_explains_one() {
        let proxy = proxy();
        let lines = run(&proxy, "help");
        for name in [
            "help",
            "list",
            "servers",
            "drain",
            "undrain",
            "switch",
            "send",
            "kick",
            "ban",
            "pardon",
            "banlist",
            "whitelist",
            "queue",
            "reload",
            "end",
        ] {
            assert!(
                lines.iter().any(|l| l.starts_with(name)),
                "{name} missing from {lines:?}"
            );
        }
        assert_eq!(
            run(&proxy, "HELP kick"),
            ["kick <player> [reason]: disconnects a player, telling them why"]
        );
        assert!(run(&proxy, "help fly")[0].starts_with("unknown command fly"));
        assert!(run(&proxy, "fly")[0].starts_with("unknown command fly"));
        assert!(run(&proxy, "   ").is_empty());
    }

    #[test]
    fn list_shows_who_is_where() {
        let proxy = proxy();
        assert_eq!(run(&proxy, "list"), ["no players online"]);
        let _alex = join(&proxy, "Alex", "lobby");
        let lines = run(&proxy, "list");
        assert_eq!(lines[0], "1 player online:");
        let uuid = crate::session::hyphenated(offline_uuid("Alex").to_be_bytes());
        assert!(
            lines[1].starts_with(&format!("  Alex ({uuid}) on lobby for ")),
            "{}",
            lines[1]
        );
        assert!(lines[1].ends_with("from 198.51.100.7"), "{}", lines[1]);
        assert_eq!(run(&proxy, "list now"), ["usage: list"]);
    }

    #[test]
    fn servers_says_which_backends_answer_and_who_is_on_them() {
        let proxy = proxy();
        let _alex = join(&proxy, "Alex", "lobby");
        assert_eq!(
            run(&proxy, "servers"),
            [
                "  lobby (default) at 127.0.0.1:25601: answers in 3 ms, 1 player",
                "  east at 127.0.0.1:25602: does not answer: connection refused, 0 players",
            ]
        );
    }

    /// `switch` and `send` check their arguments and say why a player cannot
    /// move; switches themselves are tested in `switching`.
    #[test]
    fn switch_and_send_check_their_arguments() {
        let proxy = proxy();
        assert_eq!(run(&proxy, "send"), ["usage: send <player|all> <server>"]);
        assert_eq!(
            run(&proxy, "send Alex"),
            ["usage: send <player|all> <server>"]
        );
        assert_eq!(
            run(&proxy, "switch Alex"),
            ["usage: switch <player> <server>"]
        );
        assert_eq!(
            run(&proxy, "switch all east"),
            ["usage: switch <player> <server>; to move every player, type send all <server>"]
        );
        assert_eq!(run(&proxy, "send all east"), ["no players online"]);
        let _alex = join(&proxy, "Alex", "lobby");
        let _steve = join(&proxy, "Steve", "east");
        assert_eq!(
            run(&proxy, "switch Alex west"),
            ["there is no server called west; type servers for the list"]
        );
        assert_eq!(
            run(&proxy, "switch Herobrine east"),
            ["Herobrine is not online"]
        );
        assert_eq!(run(&proxy, "switch alex lobby"), ["already on lobby"]);
        // No sidecar is configured here: the check says so, for each player.
        let no_sidecar = "Alex could not move from lobby to east: check: lobby has no sidecar \
                          in the configuration, so no player can be switched to or from it";
        assert_eq!(run(&proxy, "switch alex east"), [no_sidecar]);
        // Steve is already on east, so only Alex is tried.
        assert_eq!(run(&proxy, "send all east"), [no_sidecar]);
        // Nobody was moved or disconnected.
        assert_eq!(proxy.sessions.players().len(), 2);
    }

    #[test]
    fn kick_tells_the_player_why_and_ends_only_their_session() {
        let proxy = proxy();
        assert_eq!(run(&proxy, "kick"), ["usage: kick <player> [reason]"]);
        assert_eq!(run(&proxy, "kick Herobrine"), ["Herobrine is not online"]);
        let mut alex = join(&proxy, "Alex", "lobby");
        let _steve = join(&proxy, "Steve", "lobby");
        assert_eq!(
            run(&proxy, "kick alex bad manners"),
            ["kicked Alex from lobby: bad manners"]
        );
        let (id, payload) = alex.player.recv().unwrap();
        let configuration::Clientbound::Disconnect(d) =
            configuration::Clientbound::decode(id, &payload).unwrap()
        else {
            panic!("not a disconnect");
        };
        assert_eq!(text::plain(&d.reason, &Default::default()), "bad manners");
        assert!(alex.relay.join().unwrap().is_ok());
        assert!(proxy.sessions.find("Steve").is_some());
    }

    #[test]
    fn a_kick_without_a_reason_says_who_kicked() {
        let proxy = proxy();
        let mut alex = join(&proxy, "Alex", "lobby");
        assert_eq!(
            run(&proxy, "kick Alex"),
            ["kicked Alex from lobby: Kicked by an operator"]
        );
        let (_, payload) = alex.player.recv().unwrap();
        let configuration::Clientbound::Disconnect(d) = configuration::Clientbound::decode(
            horizon_protocol::ids::configuration::clientbound::DISCONNECT,
            &payload,
        )
        .unwrap() else {
            panic!("not a disconnect");
        };
        assert_eq!(
            text::plain(&d.reason, &Default::default()),
            "Kicked by an operator"
        );
    }

    #[test]
    fn end_asks_the_proxy_to_stop() {
        let proxy = proxy();
        assert_eq!(run(&proxy, "end now"), ["usage: end"]);
        assert!(!proxy.stopping());
        assert_eq!(run(&proxy, "end"), ["stopping"]);
        assert!(proxy.stopping());
    }

    #[test]
    fn a_reload_that_cannot_read_the_file_changes_nothing() {
        let proxy = proxy();
        let lines = run(&proxy, "reload");
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("not reloaded"), "{}", lines[0]);
        assert_eq!(*proxy.config(), config());
    }

    /// A reload applies what it can and says what needs a restart; the players
    /// on a removed backend stay.
    #[test]
    fn a_reload_applies_what_it_can_and_names_what_needs_a_restart() {
        let proxy = proxy();
        let _steve = join(&proxy, "Steve", "east");
        let new = Config::from_toml_str(
            r#"
            listen = "0.0.0.0:25570"
            motd = "Welcome"
            online-mode = false
            forwarding-secret = "secret"
            default-backend = "lobby"
            [[backends]]
            name = "lobby"
            address = "127.0.0.1:25601"
            [[backends]]
            name = "west"
            address = "127.0.0.1:25603"
            "#,
        )
        .unwrap();
        let lines = apply(&proxy, new.clone());
        assert_eq!(
            lines,
            [
                "reloaded:",
                "motd: Welcome",
                "backend west added at 127.0.0.1:25603",
                "backend east removed; the player on it stays until they leave",
                "listen 0.0.0.0:25570 needs a restart; the proxy still listens on 0.0.0.0:25565",
            ]
        );
        assert_eq!(*proxy.config(), new);
        assert!(proxy.sessions.find("Steve").is_some());
        assert_eq!(apply(&proxy, new), ["reloaded: nothing changed"]);
    }

    /// Settings beyond the first ones (the player limit, the backends to try)
    /// are applied and named too, not taken for "nothing changed".
    #[test]
    fn a_reload_of_the_routing_settings_applies_them() {
        let proxy = proxy();
        let new = Config {
            max_players: 50,
            try_backends: vec!["east".into()],
            ..config()
        };
        assert_eq!(
            apply(&proxy, new),
            [
                "reloaded:",
                "backends to try after the default one: east",
                "player limit 50 for the next logins",
            ]
        );
        assert_eq!(proxy.config().max_players, 50);
    }

    /// A reload starts the extensions it enables, leaves alone the ones it did not change, opens
    /// a new connection for one whose connection changed, and stops the ones it removes.
    #[test]
    fn a_reload_starts_follows_and_stops_the_extensions() {
        let extension = |max_in_flight: u32, login_timeout_ms: u64| meridian_config::Extension {
            name: "perms".into(),
            enabled: true,
            address: "127.0.0.1:1".into(),
            token_file: "does-not-exist.token".into(),
            grant: vec!["decide.login".into()],
            max_in_flight,
            breaker_failures: 5,
            breaker_seconds: 30,
            decide_login: meridian_config::LoginDecision {
                timeout_ms: login_timeout_ms,
                on_error: meridian_config::OnError::Default,
            },
            decide_command: Default::default(),
            decide_server: Default::default(),
            own_commands: Default::default(),
        };
        let with = |extensions| Config {
            extensions,
            ..config()
        };
        let proxy = proxy();
        assert!(proxy.extensions.links().is_empty());

        assert_eq!(
            apply(&proxy, with(vec![extension(64, 1000)])),
            [
                "reloaded:",
                "extension \"perms\" added: connecting to 127.0.0.1:1"
            ]
        );
        let first = proxy.extensions.links().remove(0);

        // A deadline is read at each decision: the connection stays.
        assert_eq!(
            apply(&proxy, with(vec![extension(64, 2000)])),
            ["reloaded:", "settings changed for the next logins"]
        );
        assert!(Arc::ptr_eq(&first, &proxy.extensions.links()[0]));

        assert_eq!(
            apply(&proxy, with(vec![extension(8, 2000)])),
            [
                "reloaded:",
                "extension \"perms\" changed: its connection is closed and opened again"
            ]
        );
        let links = proxy.extensions.links();
        assert_eq!(links.len(), 1);
        assert!(!Arc::ptr_eq(&first, &links[0]));

        let mut off = extension(8, 2000);
        off.enabled = false;
        assert_eq!(
            apply(&proxy, with(vec![off])),
            [
                "reloaded:",
                "extension \"perms\" disabled: its connection is closed"
            ]
        );
        assert!(proxy.extensions.links().is_empty());

        apply(&proxy, with(vec![extension(8, 2000)]));
        assert_eq!(
            apply(&proxy, with(Vec::new())),
            [
                "reloaded:",
                "extension \"perms\" removed: its connection is closed"
            ]
        );
        assert!(proxy.extensions.links().is_empty());
    }

    #[test]
    fn commands_can_be_added_but_not_twice() {
        let proxy = proxy();
        let mut commands = Commands::builtin();
        let ping = || Command {
            name: "ping",
            usage: "ping",
            help: "answers pong",
            run: Box::new(|_, _, _| Ok(vec!["pong".to_owned()])),
        };
        commands.register(ping()).unwrap();
        assert_eq!(commands.execute(&proxy, "ping"), ["pong"]);
        assert!(commands.register(ping()).is_err());
        assert!(
            commands
                .register(Command {
                    name: "LIST",
                    ..ping()
                })
                .is_err()
        );
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(duration(Duration::from_secs(7)), "7 s");
        assert_eq!(duration(Duration::from_secs(185)), "3 min 05 s");
        assert_eq!(duration(Duration::from_secs(3720)), "1 h 02 min");
    }

    // ---- Load balancing ----

    use meridian_config::{BalancedServer, Role};
    use meridian_sidecar::request::ServerStatus;

    /// The proxy of `proxy()` with the lobby balanced: soft cap 3, hard cap 4.
    fn balanced_proxy() -> Proxy {
        let proxy = proxy();
        proxy.set_config(Config {
            balanced: vec![BalancedServer {
                backend: "lobby".into(),
                role: Role::Lobby,
                soft_cap: 3,
                hard_cap: 4,
            }],
            ..config()
        });
        proxy
    }

    /// The lines of `servers` that belong to the lobby.
    fn lobby_lines(proxy: &Proxy) -> Vec<String> {
        let lines = run(proxy, "servers");
        let at = lines
            .iter()
            .position(|l| l.contains("lobby (default)"))
            .unwrap();
        lines[at..]
            .iter()
            .take_while(|l| at == lines.len() - 1 || !l.starts_with("  east"))
            .cloned()
            .collect()
    }

    #[test]
    fn servers_gives_a_backend_with_a_role_a_second_line() {
        let proxy = balanced_proxy();
        let _alex = join(&proxy, "Alex", "lobby");
        let _steve = join(&proxy, "Steve", "lobby");
        assert_eq!(
            lobby_lines(&proxy),
            [
                "  lobby (default) at 127.0.0.1:25601: answers in 3 ms, 2 players",
                "    role lobby, 2/3 (hard 4): open",
            ]
        );
        // A backend with no role keeps its one line.
        assert!(
            run(&proxy, "servers")
                .iter()
                .any(|l| l.starts_with("  east at"))
        );
        assert_eq!(run(&proxy, "servers").len(), 3);
    }

    #[test]
    fn servers_says_closed_at_the_soft_cap() {
        let proxy = balanced_proxy();
        let _players: Vec<_> = ["Alex", "Steve", "Eve"]
            .iter()
            .map(|n| join(&proxy, n, "lobby"))
            .collect();
        assert_eq!(
            lobby_lines(&proxy)[1],
            "    role lobby, 3/3 (hard 4): closed (soft cap reached)"
        );
    }

    #[test]
    fn servers_shows_the_ping_the_reports_and_the_places_reserved() {
        let proxy = balanced_proxy();
        proxy
            .sessions
            .statuses
            .record_ping("lobby", Duration::from_millis(7), Instant::now());
        assert_eq!(
            lobby_lines(&proxy)[1],
            "    role lobby, 0/3 (hard 4): open, ping 7 ms"
        );
        // A backend that says it does not accept transfers is draining.
        proxy.sessions.statuses.record(
            "lobby",
            ServerStatus {
                accepts_transfers: Some(false),
                ..ServerStatus::default()
            },
            Instant::now(),
        );
        assert!(
            lobby_lines(&proxy)[1].contains("draining (it does not accept transfers)"),
            "{:?}",
            lobby_lines(&proxy)
        );
        // A report that grew old is not believed.
        let old = Instant::now()
            .checked_sub(Duration::from_secs(60))
            .expect("the clock has run long enough");
        proxy
            .sessions
            .statuses
            .record("lobby", ServerStatus::default(), old);
        assert!(
            lobby_lines(&proxy)[1].contains("not offered (no report for 15 s)"),
            "{:?}",
            lobby_lines(&proxy)
        );
    }

    #[test]
    fn drain_stops_offering_a_backend_and_undrain_offers_it_again() {
        let proxy = balanced_proxy();
        let _alex = join(&proxy, "Alex", "lobby");
        let said = run(&proxy, "drain lobby");
        assert!(
            said[0].starts_with("lobby takes no new player; its players stay"),
            "{said:?}"
        );
        assert!(proxy.sessions.balancer.is_drained("lobby"));
        assert_eq!(
            lobby_lines(&proxy)[1],
            "    role lobby, 1/3 (hard 4): draining (drained from the console)"
        );
        assert_eq!(run(&proxy, "drain lobby"), ["lobby is already drained"]);
        // The player on it is still there.
        assert_eq!(proxy.sessions.counts().get("lobby"), Some(&1));
        assert_eq!(
            run(&proxy, "undrain lobby"),
            ["lobby is offered to new players again"]
        );
        assert!(!proxy.sessions.balancer.is_drained("lobby"));
        assert_eq!(run(&proxy, "undrain lobby"), ["lobby is not drained"]);
        assert_eq!(lobby_lines(&proxy)[1], "    role lobby, 1/3 (hard 4): open");
    }

    #[test]
    fn servers_says_why_a_backend_is_restarting_and_offers_it_when_it_is_back() {
        use crate::restart::Cause;
        let proxy = balanced_proxy();
        proxy
            .restarts
            .announce("lobby", Cause::Announced, Instant::now());
        assert!(proxy.sessions.balancer.is_drained("lobby"));
        assert_eq!(
            lobby_lines(&proxy)[1],
            "    role lobby, 0/3 (hard 4): draining (restarting: it announced its stop)"
        );
        proxy.restarts.back("lobby");
        assert!(!proxy.sessions.balancer.is_drained("lobby"));
        assert_eq!(lobby_lines(&proxy)[1], "    role lobby, 0/3 (hard 4): open");
    }

    /// The operator's `drain` and `undrain` win over the detector's marking.
    #[test]
    fn the_operators_drain_outlasts_a_restart_and_their_undrain_is_believed() {
        use crate::restart::Cause;
        let proxy = balanced_proxy();
        proxy
            .restarts
            .announce("lobby", Cause::Panel, Instant::now());
        assert_eq!(run(&proxy, "drain lobby"), ["lobby is already drained"]);
        proxy.restarts.back("lobby");
        assert!(proxy.sessions.balancer.is_drained("lobby"));
        assert_eq!(
            lobby_lines(&proxy)[1],
            "    role lobby, 0/3 (hard 4): draining (drained from the console)"
        );
        // An undrain while the backend is restarting offers it again: the line is not
        // "restarting" for a server the balancer offers.
        run(&proxy, "undrain lobby");
        proxy
            .restarts
            .announce("lobby", Cause::Panel, Instant::now());
        run(&proxy, "undrain lobby");
        assert_eq!(lobby_lines(&proxy)[1], "    role lobby, 0/3 (hard 4): open");
    }

    #[test]
    fn drain_checks_its_arguments() {
        let proxy = balanced_proxy();
        assert_eq!(run(&proxy, "drain"), ["usage: drain <server> [--move]"]);
        assert_eq!(
            run(&proxy, "drain lobby now"),
            ["usage: drain <server> [--move]"]
        );
        assert!(!proxy.sessions.balancer.is_drained("lobby"));
        assert_eq!(run(&proxy, "undrain a b"), ["usage: undrain <server>"]);
        // East has no role: there is nothing to drain, and nothing is changed.
        let said = run(&proxy, "drain east");
        assert!(said[0].starts_with("east has no role"), "{said:?}");
        assert!(!proxy.sessions.balancer.is_drained("east"));
        let said = run(&proxy, "drain nowhere");
        assert!(said[0].starts_with("nowhere has no role"), "{said:?}");
    }

    /// Two lobbies, `lobby` and `east`, with room for two players each.
    fn two_lobbies() -> Proxy {
        let proxy = proxy();
        proxy.set_config(Config {
            balanced: ["lobby", "east"]
                .iter()
                .map(|b| BalancedServer {
                    backend: (*b).into(),
                    role: Role::Lobby,
                    soft_cap: 2,
                    hard_cap: 2,
                })
                .collect(),
            ..config()
        });
        proxy
    }

    #[test]
    fn drain_move_with_nobody_on_the_server_only_drains_it() {
        let proxy = two_lobbies();
        let said = run(&proxy, "drain lobby --move");
        assert!(said[0].starts_with("lobby takes no new player"), "{said:?}");
        assert_eq!(said[1], "no player is on lobby");
        assert!(proxy.sessions.balancer.is_drained("lobby"));
    }

    /// With no other server of its role, a player stays and is listed.
    #[test]
    fn drain_move_lists_the_players_with_nowhere_to_go() {
        let proxy = balanced_proxy();
        let _alex = join(&proxy, "Alex", "lobby");
        let said = run(&proxy, "drain lobby --move");
        assert_eq!(
            said[1..],
            [
                "lobby: 0 moved, 1 stayed",
                "  Alex stays on lobby: no other server of role lobby has room",
            ]
        );
        assert_eq!(proxy.sessions.counts().get("lobby"), Some(&1));
    }

    /// The target is chosen and the move is tried through the switch; with no sidecar in
    /// this configuration it fails, says why, and the player stays where they are.
    #[test]
    fn drain_move_tries_a_switch_to_the_other_lobby_and_says_when_it_fails() {
        let proxy = two_lobbies();
        let _alex = join(&proxy, "Alex", "lobby");
        let said = run(&proxy, "drain lobby --move");
        assert!(
            said[1]
                .starts_with("Alex could not move from lobby to east: check: lobby has no sidecar"),
            "{said:?}"
        );
        assert_eq!(
            said[2..],
            [
                "lobby: 0 moved, 1 stayed",
                "  Alex stays on lobby: the move failed"
            ]
        );
        assert_eq!(proxy.sessions.counts().get("lobby"), Some(&1));
        assert_eq!(proxy.sessions.balancer.reserved("east"), 0);
    }

    /// `--move` on a server that is drained already still moves its players.
    #[test]
    fn drain_move_works_on_a_server_drained_before() {
        let proxy = two_lobbies();
        let _alex = join(&proxy, "Alex", "lobby");
        assert!(run(&proxy, "drain lobby")[0].starts_with("lobby takes no new player"));
        let said = run(&proxy, "drain lobby --move");
        assert_eq!(said[0], "lobby is already drained");
        assert!(said[1].starts_with("Alex could not move"), "{said:?}");
    }
}
