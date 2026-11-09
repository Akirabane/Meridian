//! The SDK's example `file-permissions` (crates/meridian-extension/examples/file_permissions.rs) run as a
//! separate process, with a file of players: a right the file gives opens a command `permissions.toml` keeps
//! closed, the proxy keeps the answer, `/extperms reload` after the file changes takes the right back at once (the
//! proxy is told to forget what it kept), and a file that cannot be read at a reload leaves the rights as they were
//! and says so.
use super::command::DENIED_SEND;
use super::example::Example;
use super::own::{TAKEN_AT_ONCE, config_of, hook_of, told_by_now};
use super::*;
use meridian_config::CommandDecision;

/// The deadline of each decision: far above what the program takes, so that a slow machine is not taken for a
/// slow extension.
const DEADLINE_MS: u64 = 5000;

/// Alex, as the command hook of the tests types: `[1; 16]`, an account nobody verified.
const ALEX: &str = "01010101-0101-0101-0101-010101010101";

/// Gives Alex `/send`, and the reload.
fn gives_send() -> String {
    format!(
        "trust-unverified = true\n\n[[player]]\nuuid = \"{ALEX}\"\nallow = [\"meridian.send\"]\nreload = true\n"
    )
}

/// Gives Alex the reload, and nothing else.
fn gives_nothing() -> String {
    format!("trust-unverified = true\n\n[[player]]\nuuid = \"{ALEX}\"\nreload = true\n")
}

/// The program, its file of players, and the folder the file is in.
struct Running {
    example: Example,
    file: PathBuf,
    _folder: Folder,
}

impl Running {
    fn start(text: &str) -> Self {
        let folder = Folder::new();
        let file = folder.0.join("players.toml");
        std::fs::write(&file, text).unwrap();
        let example = Example::start("file_permissions", &[file.as_os_str()]);
        Self {
            example,
            file,
            _folder: folder,
        }
    }

    /// Waits for its line that holds `text`, which never names the file or holds a unique id.
    fn logged(&self, text: &str) {
        let line = self.example.said(text);
        let path = self.file.display().to_string();
        assert!(
            !line.contains(&path) && !line.contains("players.toml") && !line.contains(ALEX),
            "{line}"
        );
    }

    /// Its `[[extension]]`: granted `decide.command` and `commands`, and the proxy keeps an answer up to a
    /// minute. The proxy names its command's node `ext.gate.extperms`.
    fn extension(&self) -> ExtensionConfig {
        let mut extension = extension_config(
            self.example.address.clone(),
            self.example.folder.token(),
            &["decide.command", "commands"],
            OnError::Default,
            DEADLINE_MS,
        );
        extension.decide_command = CommandDecision {
            timeout_ms: DEADLINE_MS,
            on_error: OnError::Default,
            cache_seconds_max: 60,
        };
        extension
    }
}

fn decisions(sessions: &Sessions, decision: &'static str) -> u64 {
    sessions
        .extensions
        .link("gate")
        .unwrap()
        .stats()
        .decisions("command", decision)
}

#[test]
fn a_right_from_the_file_opens_a_command_the_proxy_closes_and_a_reload_takes_it_back_at_once() {
    let running = Running::start(&gives_send());
    let config = config_of(vec![running.extension()], &[]);
    let sessions = sessions_for(&config);
    // `permissions.toml` gives Alex nothing: without the extension, /send is closed.
    let (hook, _) = hook_of(&config_of(Vec::new(), &[]), &Sessions::default(), "");
    assert_eq!(hook("send all east").as_deref(), Some(DENIED_SEND));

    let (hook, told) = hook_of(&config, &sessions, "");
    assert_eq!(hook("send all east").as_deref(), Some("done"));
    assert_eq!(decisions(&sessions, "allow"), 1);
    // The proxy keeps the answer: the file that changes changes nothing until the reload.
    std::fs::write(&running.file, gives_nothing()).unwrap();
    assert_eq!(hook("send all east").as_deref(), Some("done"));
    assert_eq!(decisions(&sessions, "cached"), 1);

    assert_eq!(hook("extperms reload").as_deref(), TAKEN_AT_ONCE);
    assert_eq!(
        told_by_now(&told, 1),
        ["The file was read again: 1 player."]
    );
    running.logged("read again, 1 player");
    // At once, not a minute later: the proxy forgot what it kept, asks again, and the file now gives nothing.
    assert_eq!(hook("send all east").as_deref(), Some(DENIED_SEND));
    // Asked again, the extension said nothing of /send; its other `allow` was the reload's own node.
    assert_eq!(decisions(&sessions, "default"), 1);
    assert_eq!(decisions(&sessions, "allow"), 2);
}

#[test]
fn a_file_that_cannot_be_read_at_a_reload_leaves_the_rights_as_they_were_and_says_so() {
    let running = Running::start(&gives_send());
    let config = config_of(vec![running.extension()], &[]);
    let sessions = sessions_for(&config);
    let (hook, told) = hook_of(&config, &sessions, "");
    assert_eq!(hook("send all east").as_deref(), Some("done"));

    for (count, broken) in [
        (
            1,
            format!("[[player]]\nuuid = \"{ALEX}\"\nallow = [\"meridian.*\"\n"),
        ),
        (
            2,
            "[[player]]\nuuid = \"Alex\"\nallow = [\"meridian.alert\"]\n".to_owned(),
        ),
    ] {
        std::fs::write(&running.file, broken).unwrap();
        assert_eq!(hook("extperms reload").as_deref(), TAKEN_AT_ONCE);
        let lines = told_by_now(&told, count);
        assert_eq!(
            lines[count - 1],
            "The file could not be read: the permissions stay as they were."
        );
        running.logged("was not read again");
    }
    std::fs::remove_file(&running.file).unwrap();
    assert_eq!(hook("extperms reload").as_deref(), TAKEN_AT_ONCE);
    told_by_now(&told, 3);
    running.logged("was not read again");
    // A file it could not read did not make it tell the proxy to forget: what the proxy kept still answers.
    let allowed = decisions(&sessions, "allow");
    assert_eq!(hook("send all east").as_deref(), Some("done"));
    assert_eq!(decisions(&sessions, "allow"), allowed);

    // The rights are those of the last file read in full: another question, which nothing kept answers, is
    // asked, and the file read at the start still gives /send and still does not give /alert.
    assert_eq!(hook("send Bob lobby").as_deref(), Some("done"));
    assert_eq!(
        hook("alert hello there").as_deref(),
        Some("You do not have permission to use /alert.")
    );
    // Nothing a player reads names the file or holds a unique id.
    let path = running.file.display().to_string();
    for line in told.lock().unwrap().iter() {
        assert!(!line.contains(&path) && !line.contains(ALEX), "{line}");
    }
}

#[test]
fn whoever_the_file_does_not_let_reload_cannot() {
    let running = Running::start(&format!(
        "trust-unverified = true\n\n[[player]]\nuuid = \"{ALEX}\"\nallow = [\"meridian.send\"]\n"
    ));
    let config = config_of(vec![running.extension()], &[]);
    let sessions = sessions_for(&config);
    let (hook, told) = hook_of(&config, &sessions, "");
    assert_eq!(
        hook("extperms reload").as_deref(),
        Some("You do not have permission to use /extperms.")
    );
    assert!(told.lock().unwrap().is_empty());
    assert_eq!(decisions(&sessions, "deny"), 1);
    // Not granted `decide.command`, it is not asked for the command's node, which no rule then closes: the
    // command itself refuses. Another run of the program: it takes one proxy at a time.
    let running = Running::start(&format!(
        "trust-unverified = true\n\n[[player]]\nuuid = \"{ALEX}\"\nallow = [\"meridian.send\"]\n"
    ));
    let mut extension = running.extension();
    extension.grant = vec!["commands".into()];
    let config = config_of(vec![extension], &[]);
    let sessions = sessions_for(&config);
    let (hook, told) = hook_of(&config, &sessions, "");
    assert_eq!(hook("extperms reload").as_deref(), TAKEN_AT_ONCE);
    assert_eq!(
        told_by_now(&told, 1),
        ["You may not reload the permissions."]
    );
}

#[test]
fn a_deny_wins_over_an_allow_and_over_permissions_toml_and_an_account_nobody_verified_is_given_only_if_trusted()
 {
    let file = |trust: bool| {
        format!(
            "trust-unverified = {trust}\n\n[[player]]\nuuid = \"{ALEX}\"\nallow = [\"meridian.*\"]\ndeny = [\"meridian.send\", \"meridian.server.*\"]\n"
        )
    };
    const DENIED_GLIST: &str = "You do not have permission to use /glist.";
    for trust in [false, true] {
        let running = Running::start(&file(trust));
        let config = config_of(vec![running.extension()], &[]);
        let sessions = sessions_for(&config);
        // `permissions.toml` gives Alex /send (and /glist): the file's deny takes it, trusted or not.
        let (hook, _) = hook_of(&config, &sessions, super::command::FILE);
        assert_eq!(
            hook("send all east").as_deref(),
            Some(DENIED_SEND),
            "{trust}"
        );
        // `permissions.toml` gives nothing: `meridian.*` gives /glist, only to an account the file trusts.
        let (hook, _) = hook_of(&config, &sessions, "");
        let expected = if trust { "done" } else { DENIED_GLIST };
        assert_eq!(hook("glist").as_deref(), Some(expected), "{trust}");
        // `meridian.server.*` names every server's node, not `meridian.server` itself: /server is open, east is
        // not.
        assert_eq!(
            hook("server east").as_deref(),
            Some("You do not have permission to go to east."),
            "{trust}"
        );
    }
}
