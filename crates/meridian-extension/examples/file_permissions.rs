//! Permissions from a file: it gives or takes the proxy's command permissions from a TOML file of players, lets the
//! proxy keep each answer for a minute, and reads the file again on a command of its own, `/extperms reload`, after
//! which it tells the proxy to forget what it kept (docs/EXTENSION-GUIDE.md). It is an example: the file stays small,
//! and it is not a second permissions system.
//!
//! ```text
//! cargo run -p meridian-extension --example file_permissions -- <token-file> <address> <permissions-file>
//! ```
//!
//! The token file is the one named by `token-file` in the extension's `[[extension]]` of `meridian.toml`, the
//! address is its `address`, and the extension is granted `decide.command` and `commands`. The file:
//!
//! ```toml
//! # Give the nodes in `allow` to players whose account the session service verified, unless the operator says
//! # otherwise here (an offline network has no verified account). With `true`, an offline player's id comes from the
//! # name they type, so anyone may use another's name; a `deny` still applies to everyone.
//! trust-unverified = false
//!
//! [[player]]
//! uuid = "0a2ea4ba-bced-39d2-80e1-1ad51a0e925c"
//! allow = ["meridian.send", "meridian.server.*"]   # a node, or every node below one with `.*`
//! deny = ["meridian.alert"]                        # wins over `allow`, and is taken from every player
//! reload = true                                    # may type /extperms reload
//! ```
//!
//! A pattern follows the rule of `permissions.toml` (lower-case words joined by dots, `*`, or words ending in `.*`) and
//! anything else makes the file not valid. A wide pattern is wide by construction: `meridian.*` gives `/glist`,
//! `/send`, `/alert` and any node a later proxy adds under `meridian.`; prefer narrow patterns and use `deny` for the
//! exceptions. A node the file does not name is left to the proxy (`permissions.toml` and the operators). The node of
//! `/extperms` follows `reload` alone: no pattern touches it (`deny = ["*"]` does not take the reload, and
//! `allow = ["ext.*"]` does not give it); to take the reload from a player, set `reload = false` or leave it out.
//!
//! The rule it keeps: **no right is ever wider than what a file it read in full gives.** A file that cannot be read,
//! is not valid, or holds no key at all (one cut to nothing would take every `deny` away), at a reload, leaves the
//! permissions as they were, says so to whoever typed the command and on the standard error stream, and tells the
//! proxy to forget nothing. A file cut short that is still valid TOML cannot be told from a whole one: write it in
//! one step (a temporary file, then a rename) and only then type `/extperms reload`. After a reload that worked, if
//! the proxy could not be told to forget, the new rules are in force but what the proxy kept stays up to a minute
//! (the player who typed the command reads so). It never writes a unique id or a file's path, neither in what a
//! player reads nor in its own lines (the standard error stream).

use meridian_extension::decide::{Auth, CommandRequest, NodeAnswers, Player};
use meridian_extension::handshake::Secret;
use meridian_extension::orders::canonical_uuid;
use meridian_extension::run::{Lines, RunRequest};
use meridian_extension::serve::{Extension, OrderError, Proxy};
use meridian_extension::wire::CommandSpec;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, RwLock};
use std::time::Duration;

/// How long the proxy may keep an answer, in seconds (its own `cache-seconds-max` may make it shorter).
const KEEP_SECONDS: u64 = 60;

/// The command of the extension's own, and the node the proxy gives it (`ext.<extension>.extperms`).
const COMMAND: &str = "extperms";

/// The file as it is written.
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct File {
    // Optional, and told apart from absent: a file with no key at all is refused as empty.
    trust_unverified: Option<bool>,
    #[serde(default)]
    player: Vec<Entry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    uuid: String,
    #[serde(default)]
    allow: Vec<String>,
    #[serde(default)]
    deny: Vec<String>,
    #[serde(default)]
    reload: bool,
}

/// What a player is given and denied.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Rights {
    allow: Vec<String>,
    deny: Vec<String>,
    reload: bool,
}

/// The file once read: the players by their unique id, in lower case with hyphens.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Rules {
    trust_unverified: bool,
    players: HashMap<String, Rights>,
}

/// `text` as a node or a pattern, in lower case: the rule of `permissions.toml` (words of `a-z`, `0-9`, `_` and `-`
/// joined by dots, `*` alone, or words ending in `.*`). `None` for anything else, a space or a line end included: a
/// pattern that can never name a node the proxy asks about would deny nothing, and nothing would say so.
fn pattern(text: &str) -> Option<String> {
    let pattern = text.to_ascii_lowercase();
    if pattern == "*" {
        return Some(pattern);
    }
    let words = pattern.strip_suffix(".*").unwrap_or(&pattern);
    let word = |w: &str| {
        !w.is_empty()
            && w.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
    };
    words.split('.').all(word).then_some(pattern)
}

/// Whether `pattern` (`a.b`, `a.*` for every node below `a`, or `*` for every node) names `node`.
fn names(pattern: &str, node: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    match pattern.strip_suffix(".*") {
        Some(prefix) => node
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('.')),
        None => pattern == node,
    }
}

/// Whether `node` is the node of this extension's own command: `ext.<the name the operator gave it>.extperms`.
fn is_own(node: &str) -> bool {
    node.starts_with("ext.") && node.ends_with(&format!(".{COMMAND}"))
}

impl Rules {
    /// Reads `text`. Every unique id must be one and every pattern a node's (see [`pattern`]); a player named twice
    /// is an error, as is a key nobody knows, and so is a file with no key at all (one cut to nothing would otherwise
    /// take every `deny` away). The error says where it is, never what the file holds.
    fn parse(text: &str) -> Result<Self, String> {
        let file: File = toml::from_str(text).map_err(|e| match e.span() {
            Some(span) => {
                let line = text[..span.start.min(text.len())].lines().count().max(1);
                format!("not valid at line {line}")
            }
            None => "not valid".to_owned(),
        })?;
        if file.trust_unverified.is_none() && file.player.is_empty() {
            return Err("the file is empty".to_owned());
        }
        let mut players = HashMap::new();
        for (i, entry) in file.player.into_iter().enumerate() {
            let uuid = canonical_uuid(&entry.uuid)
                .ok_or_else(|| format!("player {} has no valid uuid", i + 1))?;
            let patterns = |list: Vec<String>| {
                list.iter()
                    .map(|p| pattern(p))
                    .collect::<Option<Vec<_>>>()
                    .ok_or_else(|| format!("player {}: a pattern is not a node", i + 1))
            };
            let rights = Rights {
                allow: patterns(entry.allow)?,
                deny: patterns(entry.deny)?,
                reload: entry.reload,
            };
            if players.insert(uuid, rights).is_some() {
                return Err(format!("player {} is named twice", i + 1));
            }
        }
        Ok(Self {
            trust_unverified: file.trust_unverified.unwrap_or(false),
            players,
        })
    }

    fn read(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| e.kind().to_string())?;
        Self::parse(&text)
    }

    fn of(&self, player: &Player) -> Option<&Rights> {
        self.players.get(&canonical_uuid(&player.uuid)?)
    }

    /// Whether the file gives `player` what it allows: always for a verified account, and for the others only if
    /// the file trusts them. What it denies is denied to everyone.
    fn trusts(&self, player: &Player) -> bool {
        player.auth == Auth::Verified || self.trust_unverified
    }

    /// The answer for a command: `deny` for a node the player's entry denies, `allow` for one it allows (and the
    /// player is trusted), nothing for the others. The node of `/extperms` is denied to whoever may not reload.
    fn answer(&self, request: &CommandRequest) -> NodeAnswers {
        let mut answers = NodeAnswers::new().cache_seconds(KEEP_SECONDS);
        let rights = self.of(&request.player);
        let trusted = self.trusts(&request.player);
        for node in &request.nodes {
            if is_own(node) {
                answers = if self.may_reload(&request.player) {
                    answers.allow(node)
                } else {
                    answers.deny(node)
                };
                continue;
            }
            let Some(rights) = rights else { continue };
            if rights.deny.iter().any(|p| names(p, node)) {
                answers = answers.deny(node);
            } else if trusted && rights.allow.iter().any(|p| names(p, node)) {
                answers = answers.allow(node);
            }
        }
        answers
    }

    fn may_reload(&self, player: &Player) -> bool {
        self.trusts(player) && self.of(player).is_some_and(|r| r.reload)
    }
}

/// The permissions in force, and where they are read from.
struct Permissions {
    path: PathBuf,
    rules: RwLock<Rules>,
}

impl Permissions {
    fn answer(&self, request: &CommandRequest) -> NodeAnswers {
        self.rules
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .answer(request)
    }

    /// `/extperms reload`: reads the file again and, only if it was read in full, puts it in force and tells the
    /// proxy to forget what it kept. `forget` is the order, given the proxy's handle.
    fn run(&self, request: &RunRequest, forget: impl Fn() -> Result<(), OrderError>) -> Lines {
        if request.args.len() != 1 || request.args[0] != "reload" {
            return Lines::one("Usage: /extperms reload");
        }
        // Checked here too: the proxy asks this extension for the command's node only if it is granted
        // `decide.command`.
        if !self
            .rules
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .may_reload(&request.player)
        {
            return Lines::one("You may not reload the permissions.");
        }
        let rules = match Rules::read(&self.path) {
            Ok(rules) => rules,
            Err(why) => {
                eprintln!(
                    "file-permissions: the file was not read again ({why}); the permissions stay as they were"
                );
                return Lines::one(
                    "The file could not be read: the permissions stay as they were.",
                );
            }
        };
        let count = players(rules.players.len());
        *self.rules.write().unwrap_or_else(|e| e.into_inner()) = rules;
        eprintln!("file-permissions: read again, {count}");
        match forget() {
            Ok(()) => Lines::one(&format!("The file was read again: {count}.")),
            Err(_) => Lines::new([
                format!("The file was read again: {count}."),
                format!("The proxy could not be told: what it kept goes within {KEEP_SECONDS} s."),
            ]),
        }
    }
}

/// `1 player`, `2 players`.
fn players(count: usize) -> String {
    if count == 1 {
        "1 player".to_owned()
    } else {
        format!("{count} players")
    }
}

/// Tells the proxy to forget every answer it kept, trying again a few times while that may help: the lines of the
/// command must come within the proxy's wait (2 s by default).
fn forget_all(proxy: &Proxy) -> Result<(), OrderError> {
    let mut last = OrderError::NotConnected;
    for attempt in 1..=3 {
        match proxy.invalidate_all() {
            Ok(()) => return Ok(()),
            Err(e) if e.retry_later() || e.may_have_run() || e == OrderError::NotConnected => {
                last = e;
                std::thread::sleep(Duration::from_millis(100 * attempt));
            }
            Err(e) => return Err(e),
        }
    }
    Err(last)
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let (Some(token_file), Some(address), Some(file)) = (args.next(), args.next(), args.next())
    else {
        eprintln!("usage: file_permissions <token-file> <address> <permissions-file>");
        return ExitCode::FAILURE;
    };
    let token = std::fs::read(&token_file)
        .map_err(|e| e.to_string())
        .and_then(|bytes| Secret::from_file_contents(&bytes));
    let token = match token {
        Ok(token) => token,
        Err(why) => {
            eprintln!("cannot use the token file: {why}");
            return ExitCode::FAILURE;
        }
    };
    let path = PathBuf::from(&file);
    // No file read in full, no permissions: it does not start.
    let rules = match Rules::read(&path) {
        Ok(rules) => rules,
        Err(why) => {
            eprintln!("cannot read the permissions file: {why}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!("file-permissions: {}", players(rules.players.len()));
    let permissions = Arc::new(Permissions {
        path,
        rules: RwLock::new(rules),
    });
    let reload = CommandSpec {
        name: COMMAND.into(),
        aliases: Vec::new(),
        usage: "/extperms reload".into(),
    };
    let extension = Extension::new("file-permissions", env!("CARGO_PKG_VERSION"), token);
    let proxy = extension.proxy();
    let extension = extension
        .on_command({
            let permissions = Arc::clone(&permissions);
            move |request| permissions.answer(request)
        })
        .command(reload, move |request| {
            permissions.run(request, || forget_all(&proxy))
        });
    match extension.serve(&address) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("cannot listen on {address}: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const ALEX: &str = "0a2ea4ba-bced-39d2-80e1-1ad51a0e925c";
    const BOB: &str = "1b3fb5cb-cdfe-4ae3-91f2-2be62b1fa36d";

    fn player(uuid: &str, auth: Auth) -> Player {
        Player {
            name: "Alex".into(),
            uuid: uuid.into(),
            auth,
        }
    }

    fn asks(uuid: &str, auth: Auth, nodes: &[&str]) -> CommandRequest {
        CommandRequest {
            player: player(uuid, auth),
            server: "lobby".into(),
            command: "send".into(),
            args: Vec::new(),
            nodes: nodes.iter().map(|n| (*n).to_owned()).collect(),
        }
    }

    fn typed(uuid: &str, args: &[&str]) -> RunRequest {
        RunRequest {
            player: player(uuid, Auth::Verified),
            server: "lobby".into(),
            command: COMMAND.into(),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
        }
    }

    const FILE: &str = r#"
[[player]]
uuid = "0A2EA4BABCED39D280E11AD51A0E925C"
allow = ["meridian.send", "meridian.server.*"]
deny = ["meridian.alert", "meridian.server.secret"]
reload = true

[[player]]
uuid = "1b3fb5cb-cdfe-4ae3-91f2-2be62b1fa36d"
allow = ["meridian.glist"]
"#;

    fn verdicts(rules: &Rules, uuid: &str, auth: Auth, nodes: &[&str]) -> Vec<String> {
        let answers = rules.answer(&asks(uuid, auth, nodes));
        assert_eq!(answers.seconds(), KEEP_SECONDS);
        nodes
            .iter()
            .map(|n| format!("{:?}", answers.verdict(n)))
            .collect()
    }

    #[test]
    fn a_node_is_given_denied_or_left_to_the_proxy() {
        let rules = Rules::parse(FILE).unwrap();
        let nodes = [
            "meridian.send",
            "meridian.alert",
            "meridian.server.east",
            "meridian.server.secret",
            "meridian.server",
            "meridian.glist",
        ];
        assert_eq!(
            verdicts(&rules, ALEX, Auth::Verified, &nodes),
            ["Allow", "Deny", "Allow", "Deny", "Default", "Default"]
        );
        // A player the file does not name: nothing.
        let nobody = "2c4fc6dc-def0-4bf4-a203-3cf73c2fb47e";
        assert!(
            verdicts(&rules, nobody, Auth::Verified, &nodes)
                .iter()
                .all(|v| v == "Default")
        );
    }

    #[test]
    fn an_unverified_account_is_given_nothing_unless_the_file_trusts_it_and_is_denied_all_the_same()
    {
        let rules = Rules::parse(FILE).unwrap();
        let nodes = ["meridian.send", "meridian.alert"];
        assert_eq!(
            verdicts(&rules, ALEX, Auth::Unverified, &nodes),
            ["Default", "Deny"]
        );
        let trusting = Rules::parse(&format!("trust-unverified = true\n{FILE}")).unwrap();
        assert_eq!(
            verdicts(&trusting, ALEX, Auth::Unverified, &nodes),
            ["Allow", "Deny"]
        );
    }

    #[test]
    fn the_node_of_the_reload_is_given_only_to_whoever_may_reload() {
        let rules = Rules::parse(FILE).unwrap();
        let own = ["ext.file-permissions.extperms", "ext.perms.extperms"];
        assert_eq!(
            verdicts(&rules, ALEX, Auth::Verified, &own),
            ["Allow", "Allow"]
        );
        assert_eq!(
            verdicts(&rules, BOB, Auth::Verified, &own),
            ["Deny", "Deny"]
        );
        assert_eq!(
            verdicts(&rules, ALEX, Auth::Unverified, &own),
            ["Deny", "Deny"]
        );
        // Another extension's command is not this one's.
        assert_eq!(
            verdicts(&rules, BOB, Auth::Verified, &["ext.perms.party"]),
            ["Default"]
        );
    }

    #[test]
    fn a_file_that_is_not_valid_is_refused_and_says_where_never_what() {
        let alex = "[[player]]\nuuid = \"0a2ea4ba-bced-39d2-80e1-1ad51a0e925c\"\n";
        for (text, said) in [
            (
                "[[player]]\nuuid = \"Alex\"\n".to_owned(),
                "player 1 has no valid uuid",
            ),
            (format!("{alex}alow = []\n"), "not valid at line"),
            (
                "trust-unverified = \"yes\"\n".to_owned(),
                "not valid at line 1",
            ),
            (
                format!("{alex}[[player]]\nuuid = \"0A2EA4BABCED39D280E11AD51A0E925C\"\n"),
                "player 2 is named twice",
            ),
            // Cut to nothing, or to comments: every deny would be gone.
            (String::new(), "the file is empty"),
            ("# who may use what\n\n".to_owned(), "the file is empty"),
            // A pattern that can never name a node the proxy asks about would deny nothing.
            (
                format!("{alex}deny = [\"meridian.alert \"]\n"),
                "player 1: a pattern is not a node",
            ),
            (
                format!(
                    "{alex}allow = [\"meridian.send\"]\n[[player]]\nuuid = \"{BOB}\"\ndeny = [\"meridian.alert\\n\"]\n"
                ),
                "player 2: a pattern is not a node",
            ),
        ] {
            let why = Rules::parse(&text).unwrap_err();
            assert!(why.starts_with(said), "{text:?}: {why}");
            assert!(
                !why.contains("0a2ea4ba") && !why.contains("alow") && !why.contains("alert"),
                "{why}"
            );
        }
        // A key is enough: a file that names nobody and says whom it trusts is a file.
        assert_eq!(
            Rules::parse("trust-unverified = false\n").unwrap(),
            Rules::default()
        );
    }

    #[test]
    fn a_pattern_is_a_node_of_permissions_toml_in_lower_case() {
        for (text, read) in [
            ("meridian.send", Some("meridian.send")),
            ("Meridian.Alert", Some("meridian.alert")),
            ("meridian.server.*", Some("meridian.server.*")),
            (
                "ext.file-permissions.extperms",
                Some("ext.file-permissions.extperms"),
            ),
            ("*", Some("*")),
            ("meridian.alert ", None),
            (" meridian.alert", None),
            ("meridian.alert\n", None),
            ("meridian..alert", None),
            ("meridian.*.alert", None),
            ("meridian*", None),
            ("", None),
        ] {
            assert_eq!(pattern(text).as_deref(), read, "{text:?}");
        }
        // Read in lower case, a deny written in upper case denies.
        let rules = Rules::parse(&format!(
            "[[player]]\nuuid = \"{ALEX}\"\ndeny = [\"Meridian.Alert\"]\n"
        ))
        .unwrap();
        assert_eq!(
            verdicts(&rules, ALEX, Auth::Verified, &["meridian.alert"]),
            ["Deny"]
        );
        // `*` names every node.
        let everything =
            Rules::parse(&format!("[[player]]\nuuid = \"{ALEX}\"\nallow = [\"*\"]\n")).unwrap();
        assert_eq!(
            verdicts(&everything, ALEX, Auth::Verified, &["meridian.glist"]),
            ["Allow"]
        );
    }

    /// A file of its own in the system's temporary folder, removed at the end.
    struct Temporary(PathBuf);

    impl Temporary {
        fn with(text: &str) -> Self {
            static COUNT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "file-permissions-{}-{}.toml",
                std::process::id(),
                COUNT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::write(&path, text).unwrap();
            Self(path)
        }
    }

    impl Drop for Temporary {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn permissions(file: &Temporary) -> Permissions {
        Permissions {
            path: file.0.clone(),
            rules: RwLock::new(Rules::read(&file.0).unwrap()),
        }
    }

    #[test]
    fn a_reload_puts_the_file_in_force_and_tells_the_proxy_to_forget() {
        let file = Temporary::with(FILE);
        let permissions = permissions(&file);
        std::fs::write(&file.0, FILE.replace("\"meridian.send\", ", "")).unwrap();
        let told = AtomicUsize::new(0);
        let said = permissions.run(&typed(ALEX, &["reload"]), || {
            told.fetch_add(1, Ordering::Relaxed);
            Ok(())
        });
        assert_eq!(said.as_slice(), ["The file was read again: 2 players."]);
        assert_eq!(told.load(Ordering::Relaxed), 1);
        let answers = permissions.answer(&asks(ALEX, Auth::Verified, &["meridian.send"]));
        assert_eq!(format!("{:?}", answers.verdict("meridian.send")), "Default");
        // The proxy could not be told: the player reads that what it kept goes within the minute.
        let said = permissions.run(&typed(ALEX, &["reload"]), || Err(OrderError::NotConnected));
        assert_eq!(said.as_slice().len(), 2);
        assert!(said.as_slice()[1].contains("60 s"));
    }

    #[test]
    fn a_file_that_cannot_be_read_leaves_the_permissions_as_they_were_and_says_so() {
        let file = Temporary::with(FILE);
        let permissions = permissions(&file);
        let before = permissions.rules.read().unwrap().clone();
        for broken in ["[[player]]\nuuid = \"no\"\n", "not toml at all ["] {
            std::fs::write(&file.0, broken).unwrap();
            let said = permissions.run(&typed(ALEX, &["reload"]), || {
                panic!("the proxy is told nothing")
            });
            assert_eq!(
                said.as_slice(),
                ["The file could not be read: the permissions stay as they were."]
            );
            assert_eq!(*permissions.rules.read().unwrap(), before);
        }
        std::fs::remove_file(&file.0).unwrap();
        let said = permissions.run(&typed(ALEX, &["reload"]), || {
            panic!("the proxy is told nothing")
        });
        assert_eq!(
            said.as_slice(),
            ["The file could not be read: the permissions stay as they were."]
        );
        assert_eq!(*permissions.rules.read().unwrap(), before);
    }

    #[test]
    fn only_whoever_may_reload_reloads_and_what_they_read_holds_no_id_or_path() {
        let file = Temporary::with(FILE);
        let permissions = permissions(&file);
        let path = file.0.display().to_string();
        let said = permissions.run(&typed(BOB, &["reload"]), || {
            panic!("the proxy is told nothing")
        });
        assert_eq!(said.as_slice(), ["You may not reload the permissions."]);
        for args in [&[][..], &["reload", "now"], &["dance"]] {
            let said = permissions.run(&typed(ALEX, args), || panic!("the proxy is told nothing"));
            assert_eq!(said.as_slice(), ["Usage: /extperms reload"], "{args:?}");
        }
        let said = permissions.run(&typed(ALEX, &["reload"]), || Ok(()));
        for line in said.as_slice() {
            assert!(
                !line.contains(&path) && !line.contains(ALEX) && !line.contains(BOB),
                "{line}"
            );
        }
    }
}
