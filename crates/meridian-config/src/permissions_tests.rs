//! Permissions: the format, the nodes, the way a node is decided, and the file.
use super::*;

const ALEX: [u8; 16] = [1; 16];
const STEVE: [u8; 16] = [2; 16];

fn parse(text: &str) -> Permissions {
    Permissions::parse(text).unwrap_or_else(|e| panic!("{e}"))
}

fn who(name: &str) -> Target {
    Target::parse(name).unwrap()
}

#[test]
fn a_node_is_words_a_pattern_or_everything() {
    for good in [
        "meridian.send",
        "a",
        "meridian.server.*",
        "*",
        "a-b.c_d.9",
        "A.B",
    ] {
        assert!(check_node(good).is_ok(), "{good}");
    }
    assert_eq!(check_node("Meridian.SEND").unwrap(), "meridian.send");
    for bad in [
        "", ".", "a..b", ".a", "a.", "a.*.b", "a b", "a*", "*.a", "a.**", "é",
    ] {
        assert!(check_node(bad).is_err(), "{bad:?} was taken");
    }
}

#[test]
fn nothing_written_leaves_the_old_commands_open_and_the_new_ones_closed() {
    let none = Permissions::default();
    for open in [
        node::SERVER,
        node::HUB,
        node::PLAY,
        "meridian.server.lobby",
        "meridian.server.anything-else",
    ] {
        assert!(none.allows("Alex", ALEX, open), "{open}");
    }
    for closed in [
        node::SEND,
        node::GLIST,
        node::ALERT,
        "meridian.other",
        "meridianserver",
    ] {
        assert!(!none.allows("Alex", ALEX, closed), "{closed}");
    }
}

#[test]
fn a_player_has_the_nodes_of_their_groups() {
    let p = parse(
        r#"
[group.staff]
allow = ["meridian.glist", "meridian.alert"]
[[player]]
name = "Alex"
groups = ["staff"]
"#,
    );
    assert!(p.allows("Alex", ALEX, node::GLIST));
    assert!(p.allows("alex", ALEX, node::ALERT));
    assert!(!p.allows("Alex", ALEX, node::SEND), "not granted");
    assert!(!p.allows("Steve", STEVE, node::GLIST), "not in the group");
    // Nodes are in lower case whatever is asked.
    assert!(p.allows("Alex", ALEX, "Meridian.GList"));
}

#[test]
fn a_player_is_found_by_unique_id_too() {
    let p = parse(
        r#"
[group.staff]
allow = ["meridian.send"]
[[player]]
uuid = "01010101-0101-0101-0101-010101010101"
groups = ["staff"]
"#,
    );
    assert!(p.allows("Whatever", ALEX, node::SEND));
    assert!(!p.allows("Alex", STEVE, node::SEND));
    // A name and an id that are the same player have the groups of both.
    let both = parse(
        r#"
[group.a]
allow = ["x.a"]
[group.b]
allow = ["x.b"]
[[player]]
name = "Alex"
groups = ["a"]
[[player]]
uuid = "01010101-0101-0101-0101-010101010101"
groups = ["b"]
"#,
    );
    assert_eq!(both.groups_of("alex", ALEX), ["a", "b"]);
    assert!(both.allows("Alex", ALEX, "x.a") && both.allows("Alex", ALEX, "x.b"));
}

#[test]
fn a_group_inherits_the_nodes_of_its_parents() {
    let p = parse(
        r#"
[group.moderator]
allow = ["meridian.glist"]
[group.admin]
inherits = ["moderator"]
allow = ["meridian.send"]
[[player]]
name = "Alex"
groups = ["admin"]
[[player]]
name = "Steve"
groups = ["moderator"]
"#,
    );
    assert!(p.allows("Alex", ALEX, node::SEND));
    assert!(p.allows("Alex", ALEX, node::GLIST), "inherited");
    assert!(p.allows("Steve", STEVE, node::GLIST));
    assert!(
        !p.allows("Steve", STEVE, node::SEND),
        "a parent does not inherit its child"
    );
}

#[test]
fn the_nearest_group_decides_and_a_child_overrides_its_parent() {
    let p = parse(
        r#"
[group.default]
deny = ["meridian.hub"]
[group.base]
deny = ["meridian.glist"]
[group.staff]
inherits = ["base"]
allow = ["meridian.glist", "meridian.hub"]
[[player]]
name = "Alex"
groups = ["staff"]
"#,
    );
    // The child's allow beats the parent's deny, and `default`'s, which is last.
    assert!(p.allows("Alex", ALEX, node::GLIST));
    assert!(p.allows("Alex", ALEX, node::HUB));
    // Someone in no group has `default` only.
    assert!(!p.allows("Steve", STEVE, node::HUB));
}

#[test]
fn default_is_everyones_and_can_close_a_command_that_was_open() {
    let p = parse(
        r#"
[group.default]
deny = ["meridian.play", "meridian.server.secret"]
allow = ["meridian.glist"]
"#,
    );
    assert!(!p.allows("Anyone", ALEX, node::PLAY));
    assert!(!p.allows("Anyone", ALEX, "meridian.server.secret"));
    assert!(
        p.allows("Anyone", ALEX, "meridian.server.lobby"),
        "still open"
    );
    assert!(p.allows("Anyone", ALEX, node::GLIST));
    assert!(p.allows("Anyone", ALEX, node::HUB));
}

#[test]
fn the_most_specific_match_of_a_level_wins_and_a_deny_wins_a_tie() {
    let p = parse(
        r#"
[group.a]
allow = ["meridian.*"]
deny = ["meridian.server.secret", "meridian.alert", "meridian.glist"]
[group.b]
allow = ["meridian.alert", "meridian.server.*"]
[[player]]
name = "Alex"
groups = ["a", "b"]
"#,
    );
    assert!(p.allows("Alex", ALEX, node::SEND), "the wide allow");
    // A node beats a pattern of the same group, and the deny of a node beats the
    // allow of a pattern.
    assert!(!p.allows("Alex", ALEX, "meridian.server.secret"));
    assert!(p.allows("Alex", ALEX, "meridian.server.lobby"));
    // The same node allowed in one group of the level and denied in another: deny.
    assert!(!p.allows("Alex", ALEX, node::ALERT));
    assert!(!p.allows("Alex", ALEX, node::GLIST));
}

#[test]
fn a_pattern_covers_what_is_below_it_and_not_itself() {
    let p = parse(
        r#"
[group.g]
allow = ["meridian.server.*"]
deny = ["*"]
[[player]]
name = "Alex"
groups = ["g"]
"#,
    );
    assert!(p.allows("Alex", ALEX, "meridian.server.lobby"));
    assert!(p.allows("Alex", ALEX, "meridian.server.a.b"));
    assert!(
        !p.allows("Alex", ALEX, "meridian.server"),
        "not the bare prefix"
    );
    assert!(!p.allows("Alex", ALEX, node::SEND));
    assert!(!p.allows("Alex", ALEX, "meridian.serverx.y"));
}

#[test]
fn a_server_node_is_in_lower_case() {
    assert_eq!(node::server("Lobby"), "meridian.server.lobby");
}

#[test]
fn what_is_written_is_read_back_the_same() {
    let text = r#"
[group.default]
deny = ["meridian.play"]
[group.admin]
inherits = ["mod"]
allow = ["meridian.send", "meridian.server.*"]
deny = ["meridian.server.secret"]
[group.mod]
inherits = ["default"]
allow = ["meridian.glist"]
[[player]]
name = "Alex"
groups = ["admin", "mod"]
[[player]]
uuid = "069a79f4-44e9-4726-a5be-fca90e38aaf5"
groups = ["mod"]
"#;
    let p = parse(text);
    let written = p.to_toml();
    assert_eq!(Permissions::parse(&written).unwrap(), p, "{written}");
    assert_eq!(p.groups().len(), 3);
    assert_eq!(p.members().len(), 2);
}

#[test]
fn an_empty_file_is_no_groups() {
    assert_eq!(parse(""), Permissions::default());
}

#[test]
fn a_file_that_makes_no_sense_is_refused_and_says_where() {
    for (text, says) in [
        (
            "[group.a]\nallow = [\"bad node\"]",
            "[group.a]: \"bad node\" is not a permission node",
        ),
        ("[group.\"bad name\"]", "not a group name"),
        (
            "[group.a]\nallow = [\"x\"]\ndeny = [\"X\"]",
            "x is both allowed and denied",
        ),
        (
            "[group.a]\ninherits = [\"nope\"]",
            "inherits nope, which is not a group",
        ),
        (
            "[group.a]\ninherits = [\"b\"]\n[group.b]\ninherits = [\"a\"]",
            "inherits it back",
        ),
        ("[group.a]\ninherits = [\"a\"]", "inherits it back"),
        (
            "[[player]]\ngroups = [\"default\"]",
            "needs exactly one of name and uuid",
        ),
        (
            "[[player]]\nname = \"Alex\"\nuuid = \"x\"\ngroups = [\"default\"]",
            "exactly one",
        ),
        ("[[player]]\nname = \"Alex\"", "has no groups"),
        (
            "[[player]]\nname = \"Alex\"\ngroups = [\"ghost\"]",
            "is in ghost, which is not a group",
        ),
        (
            "[[player]]\nname = \"bad name\"\ngroups = [\"default\"]",
            "not a player name",
        ),
        (
            "[[player]]\nuuid = \"nope\"\ngroups = [\"default\"]",
            "not a unique id",
        ),
        ("[group.a]\nallows = []", "allows"),
        ("unknown = 1", "unknown"),
    ] {
        let error = Permissions::parse(text).unwrap_err().to_string();
        assert!(error.contains(says), "{text:?} gave {error:?}");
    }
}

#[test]
fn a_player_may_be_in_the_default_group_written_or_not() {
    let p = parse("[[player]]\nname = \"Alex\"\ngroups = [\"default\"]\n");
    assert_eq!(p.groups_of("Alex", ALEX), ["default"]);
    assert!(p.group(DEFAULT_GROUP).is_some());
    // Inheriting `default` without writing it is allowed too.
    let q = parse("[group.a]\ninherits = [\"default\"]\nallow = [\"x\"]\n");
    assert!(!q.allows("Alex", ALEX, "x"), "nobody is in a");
    let q = parse(
        "[group.a]\ninherits = [\"default\"]\nallow = [\"x\"]\n\
         [[player]]\nname = \"Alex\"\ngroups = [\"a\"]\n",
    );
    assert!(q.allows("Alex", ALEX, "x"));
    assert!(!q.allows("Steve", STEVE, "x"));
}

#[test]
fn the_editing_functions_check_what_they_are_given() {
    let mut p = Permissions::default();
    assert_eq!(p.create_group("staff"), Ok(true));
    assert_eq!(p.create_group("staff"), Ok(false));
    assert!(p.create_group("bad name").is_err());
    assert_eq!(p.set("staff", "meridian.send", Setting::Allow), Ok(true));
    assert_eq!(p.set("staff", "Meridian.Send", Setting::Allow), Ok(false));
    assert_eq!(
        p.set("staff", "meridian.send", Setting::Deny),
        Ok(true),
        "moves it"
    );
    assert_eq!(p.group("staff").unwrap().allow, Vec::<String>::new());
    assert_eq!(p.group("staff").unwrap().deny, ["meridian.send"]);
    assert_eq!(p.set("staff", "meridian.send", Setting::Unset), Ok(true));
    assert_eq!(p.set("staff", "meridian.send", Setting::Unset), Ok(false));
    assert!(p.set("staff", "bad node", Setting::Allow).is_err());
    assert!(
        p.set("ghost", "meridian.send", Setting::Allow)
            .unwrap_err()
            .contains("no group")
    );

    assert_eq!(p.grant(who("Alex"), "staff"), Ok(true));
    assert_eq!(p.grant(who("ALEX"), "staff"), Ok(false));
    assert!(p.grant(who("203.0.113.9"), "staff").is_err());
    assert!(p.grant(who("Steve"), "ghost").is_err());
    assert!(p.revoke(&who("alex"), "staff"));
    assert!(!p.revoke(&who("alex"), "staff"));
    assert!(p.members().is_empty(), "a player in no group is dropped");
}

#[test]
fn inheritance_cannot_loop() {
    let mut p = Permissions::default();
    for g in ["a", "b", "c"] {
        p.create_group(g).unwrap();
    }
    assert_eq!(p.inherit("b", "a"), Ok(true));
    assert_eq!(p.inherit("b", "a"), Ok(false));
    assert_eq!(p.inherit("c", "b"), Ok(true));
    assert!(p.inherit("a", "c").unwrap_err().contains("inherits a"));
    assert!(p.inherit("a", "a").is_err());
    assert!(p.inherit("a", "ghost").is_err());
    assert_eq!(p.uninherit("c", "b"), Ok(true));
    assert_eq!(p.uninherit("c", "b"), Ok(false));
}

#[test]
fn deleting_a_group_takes_it_out_of_the_others_and_the_players() {
    let mut p = Permissions::default();
    p.create_group("a").unwrap();
    p.create_group("b").unwrap();
    p.inherit("b", "a").unwrap();
    p.grant(who("Alex"), "a").unwrap();
    p.grant(who("Steve"), "a").unwrap();
    p.grant(who("Steve"), "b").unwrap();
    assert!(p.delete_group("a"));
    assert!(!p.delete_group("a"));
    assert!(p.group("b").unwrap().inherits.is_empty());
    assert_eq!(p.members().len(), 1);
    assert_eq!(p.groups_of("Steve", STEVE), ["b"]);
}

fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("meridian-perm-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn the_file_is_saved_whole_read_again_and_missing_means_none() {
    let dir = folder("file");
    let path = dir.join("permissions.toml");
    assert_eq!(Permissions::load(&path).unwrap(), Permissions::default());
    let mut p = Permissions::default();
    p.create_group("staff").unwrap();
    p.set("staff", "meridian.*", Setting::Allow).unwrap();
    p.grant(who("Alex"), "staff").unwrap();
    p.save(&path).unwrap();
    assert_eq!(Permissions::load(&path).unwrap(), p);
    let files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(files, ["permissions.toml"]);
    std::fs::write(&path, "[group.a]\nallow = [\"bad node\"]").unwrap();
    let error = Permissions::load(&path).unwrap_err().to_string();
    assert!(
        error.contains("permissions.toml") && error.contains("[group.a]"),
        "{error}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

fn with(extra: &str) -> Config {
    Config::from_toml_str_in(
        &format!(
            "forwarding-secret = \"test-secret\"\n[[backends]]\nname = \"lobby\"\n\
             address = \"127.0.0.1:25566\"\n{extra}"
        ),
        Path::new("conf"),
    )
    .unwrap()
}

#[test]
fn the_file_is_beside_the_configuration_unless_the_table_says_otherwise() {
    assert_eq!(
        with("").permissions.file,
        Some(Path::new("conf").join("permissions.toml"))
    );
    assert_eq!(
        with("[permissions]\nfile = \"staff/perms.toml\"")
            .permissions
            .file,
        Some(Path::new("conf").join("staff/perms.toml"))
    );
    assert_eq!(
        Config::default().permissions,
        PermissionSettings { file: None }
    );
    assert!(
        Config::from_toml_str(
            "[permissions]\nfiles = 1\n[[backends]]\nname = \"a\"\naddress = \"h:1\""
        )
        .is_err()
    );
}

#[test]
fn the_staff_commands_are_known_to_the_commands_table_and_off_by_default() {
    let config = with("[commands]\nenabled = [\"server\", \"send\", \"glist\", \"alert\"]");
    for command in [ProxyCommand::Send, ProxyCommand::Glist, ProxyCommand::Alert] {
        assert!(config.commands.is_enabled(command));
        assert_eq!(command.words(), [command.name()]);
    }
    let defaults = with("");
    assert!(!defaults.commands.is_enabled(ProxyCommand::Send));
    assert!(!defaults.commands.is_enabled(ProxyCommand::Glist));
    assert!(!defaults.commands.is_enabled(ProxyCommand::Alert));
}

// ---- Groups a player is in for one question (the operators of the servers).

#[test]
fn a_group_given_for_one_question_gives_its_nodes_for_that_question_only() {
    let p = parse(
        r#"
[group.operator]
allow = ["meridian.send", "meridian.glist"]
"#,
    );
    assert!(!p.allows("Alex", ALEX, node::SEND));
    assert!(p.allows_in("Alex", ALEX, node::SEND, &["operator"]));
    assert!(p.allows_in("Alex", ALEX, node::GLIST, &["operator"]));
    assert!(
        !p.allows_in("Alex", ALEX, node::ALERT, &["operator"]),
        "not granted"
    );
    // Nothing was written: the player is in no group, and asked again without it they have nothing.
    assert!(p.groups_of("Alex", ALEX).is_empty());
    assert!(p.members().is_empty());
    assert!(!p.allows("Alex", ALEX, node::SEND));
    // Asking with no extra group is asking plainly.
    assert_eq!(
        p.allows_in("Alex", ALEX, node::SEND, &[]),
        p.allows("Alex", ALEX, node::SEND)
    );
}

#[test]
fn a_group_that_is_not_defined_adds_nothing_and_the_open_commands_stay_open() {
    let p = Permissions::default();
    assert!(!p.allows_in("Alex", ALEX, node::SEND, &["operator"]));
    assert!(p.allows_in("Alex", ALEX, node::SERVER, &["operator"]));
}

#[test]
fn the_group_given_inherits_and_a_deny_of_the_players_own_group_still_wins_a_tie() {
    let p = parse(
        r#"
[group.moderator]
allow = ["meridian.glist"]
[group.operator]
inherits = ["moderator"]
allow = ["meridian.send"]
[group.muted]
deny = ["meridian.send"]
[[player]]
name = "Alex"
groups = ["muted"]
"#,
    );
    assert!(
        p.allows_in("Steve", STEVE, node::GLIST, &["operator"]),
        "inherited"
    );
    assert!(p.allows_in("Steve", STEVE, node::SEND, &["operator"]));
    // Alex is in a group that denies the node as precisely as the operator group allows it.
    assert!(!p.allows_in("Alex", ALEX, node::SEND, &["operator"]));
    // A player already in the group is not counted twice.
    let q = parse(
        r#"
[group.operator]
allow = ["meridian.send"]
[[player]]
name = "Alex"
groups = ["operator"]
"#,
    );
    assert!(q.allows_in("Alex", ALEX, node::SEND, &["operator"]));
}
