//! The bans and the whitelist: the format, the parsing of what the console is given, the
//! matching, and the file.
use super::*;
use std::net::IpAddr;

fn ip(text: &str) -> IpAddr {
    text.parse().unwrap()
}

fn target(text: &str) -> Target {
    Target::parse(text).unwrap()
}

const UUID: [u8; 16] = [
    0x06, 0x9a, 0x79, 0xf4, 0x44, 0xe9, 0x47, 0x26, 0xa5, 0xbe, 0xfc, 0xa9, 0x0e, 0x38, 0xaa, 0xf5,
];
const UUID_TEXT: &str = "069a79f4-44e9-4726-a5be-fca90e38aaf5";
const OTHER: [u8; 16] = [7; 16];

fn nobody() -> IpAddr {
    ip("198.51.100.7")
}

#[test]
fn the_console_is_understood_as_a_name_a_unique_id_or_an_address() {
    assert_eq!(target("Griefer_1"), Target::Name("Griefer_1".into()));
    assert_eq!(target(UUID_TEXT), Target::Uuid(UUID));
    assert_eq!(
        target("069A79F444E94726A5BEFCA90E38AAF5"),
        Target::Uuid(UUID)
    );
    assert_eq!(target("203.0.113.9").to_string(), "203.0.113.9");
    assert_eq!(target("203.0.113.9/24").to_string(), "203.0.113.0/24");
    assert_eq!(target("2001:db8::1").to_string(), "2001:db8::1");
    assert_eq!(target(UUID_TEXT).to_string(), UUID_TEXT);
    for bad in [
        "",
        "a name that is much too long",
        "bad-name",
        "203.0.113.999",
        "203.0.113.0/33",
        "2001:db8::/129",
        "203.0.113.0/x",
        "069a79f4-44e9-4726-a5be-fca90e38aaf",
    ] {
        assert!(Target::parse(bad).is_err(), "{bad:?} was taken");
    }
}

#[test]
fn a_range_holds_the_addresses_of_its_prefix_and_no_others() {
    let range = Net::parse("203.0.113.0/24").unwrap();
    assert!(range.contains(ip("203.0.113.0")));
    assert!(range.contains(ip("203.0.113.255")));
    assert!(!range.contains(ip("203.0.114.0")));
    // IPv4 written as IPv6 is the IPv4 address.
    assert!(range.contains(ip("::ffff:203.0.113.9")));
    // The two families never meet.
    assert!(!range.contains(ip("2001:db8::1")));
    let v6 = Net::parse("2001:db8::/32").unwrap();
    assert!(v6.contains(ip("2001:db8:ffff::1")));
    assert!(!v6.contains(ip("2001:db9::1")));
    assert!(!v6.contains(ip("203.0.113.9")));
    // The edges: a prefix of 0 is every address of its family, the longest only one.
    assert!(Net::parse("0.0.0.0/0").unwrap().contains(ip("8.8.8.8")));
    let one = Net::parse("203.0.113.9").unwrap();
    assert!(one.contains(ip("203.0.113.9")));
    assert!(!one.contains(ip("203.0.113.8")));
    // The bits past the prefix are cleared.
    assert_eq!(Net::parse("10.1.2.3/8"), Net::parse("10.0.0.0/8"));
}

#[test]
fn nothing_is_refused_when_nothing_is_set() {
    let access = Access::default();
    assert!(!access.whitelist_on());
    assert_eq!(access.refusal("Alex", UUID, nobody()), None);
    assert!(access.address_ban(nobody()).is_none());
}

#[test]
fn a_ban_by_name_account_or_address_refuses_with_its_reason() {
    let mut access = Access::default();
    assert!(access.ban(target("Griefer"), Some("griefing".into())));
    assert!(access.ban(Target::Uuid(OTHER), None));
    assert!(access.ban(target("203.0.113.0/24"), Some("a bad range".into())));
    // A name is a name whatever its case.
    assert_eq!(
        access.refusal("gRiEfEr", UUID, nobody()),
        Some(Refusal::Banned(Some("griefing".into())))
    );
    assert_eq!(
        access.refusal("Alex", OTHER, nobody()),
        Some(Refusal::Banned(None))
    );
    assert_eq!(
        access.refusal("Alex", UUID, ip("203.0.113.77")),
        Some(Refusal::Banned(Some("a bad range".into())))
    );
    assert_eq!(access.refusal("Alex", UUID, nobody()), None);
    assert!(access.address_ban(ip("203.0.113.77")).is_some());
    assert!(access.address_ban(nobody()).is_none());
}

#[test]
fn what_a_player_is_told_says_the_reason_when_there_is_one() {
    assert_eq!(
        Refusal::Banned(None).message(),
        "You are banned from this server."
    );
    assert_eq!(
        Refusal::Banned(Some("griefing".into())).message(),
        "You are banned from this server.\nReason: griefing"
    );
    assert_eq!(
        Refusal::NotWhitelisted.message(),
        "You are not whitelisted on this server."
    );
}

#[test]
fn a_ban_is_made_once_and_lifted_by_what_made_it() {
    let mut access = Access::default();
    assert!(access.ban(target("Griefer"), None));
    assert!(!access.ban(target("GRIEFER"), Some("again".into())));
    assert_eq!(access.bans().len(), 1);
    assert_eq!(access.bans()[0].reason, None, "the first ban stands");
    assert!(!access.pardon(&target("Someone")));
    assert!(access.pardon(&target("griefer")));
    assert!(access.bans().is_empty());
    // A range is lifted as the same range, not by an address inside it.
    assert!(access.ban(target("203.0.113.0/24"), None));
    assert!(!access.pardon(&target("203.0.113.9")));
    assert!(access.pardon(&target("203.0.113.77/24")));
}

#[test]
fn the_whitelist_lets_in_only_who_is_on_it_and_only_while_it_is_on() {
    let mut access = Access::default();
    assert_eq!(access.allow(target("Alex")), Ok(true));
    assert_eq!(access.allow(target("ALEX")), Ok(false));
    assert_eq!(access.allow(Target::Uuid(UUID)), Ok(true));
    // Off: it changes nothing.
    assert_eq!(access.refusal("Steve", OTHER, nobody()), None);
    access.set_whitelist(true);
    assert_eq!(access.refusal("alex", OTHER, nobody()), None);
    assert_eq!(access.refusal("Steve", UUID, nobody()), None);
    assert_eq!(
        access.refusal("Steve", OTHER, nobody()),
        Some(Refusal::NotWhitelisted)
    );
    assert!(access.disallow(&target("alex")));
    assert!(!access.disallow(&target("alex")));
    assert_eq!(
        access.refusal("Alex", OTHER, nobody()),
        Some(Refusal::NotWhitelisted)
    );
}

#[test]
fn an_address_is_not_a_whitelist_entry() {
    let mut access = Access::default();
    assert!(access.allow(target("203.0.113.9")).is_err());
    assert!(access.allowed().is_empty());
}

#[test]
fn a_ban_beats_the_whitelist() {
    let mut access = Access::default();
    access.set_whitelist(true);
    access.allow(target("Alex")).unwrap();
    access.ban(target("Alex"), Some("later".into()));
    assert_eq!(
        access.refusal("Alex", UUID, nobody()),
        Some(Refusal::Banned(Some("later".into())))
    );
}

fn busy() -> Access {
    let mut access = Access::default();
    access.set_whitelist(true);
    access.ban(
        target("Griefer"),
        Some("a \"quoted\" reason, with a \\ and é".into()),
    );
    access.ban(target(UUID_TEXT), None);
    access.ban(target("203.0.113.0/24"), Some("range".into()));
    access.ban(target("2001:db8::/32"), None);
    access.allow(target("Alex")).unwrap();
    access.allow(target(UUID_TEXT)).unwrap();
    access
}

#[test]
fn what_is_written_is_read_back_the_same() {
    let access = busy();
    let text = access.to_toml();
    assert_eq!(Access::parse(&text).unwrap(), access, "{text}");
}

#[test]
fn the_file_is_read_as_the_module_shows_it() {
    let access = Access::parse(
        r#"
whitelist = true

[[ban]]
name = "Griefer"
reason = "Griefing the spawn"

[[ban]]
ip = "203.0.113.0/24"

[[allow]]
uuid = "069a79f4-44e9-4726-a5be-fca90e38aaf5"
"#,
    )
    .unwrap();
    assert!(access.whitelist_on());
    assert_eq!(access.bans().len(), 2);
    assert_eq!(
        access.refusal("Griefer", UUID, nobody()),
        Some(Refusal::Banned(Some("Griefing the spawn".into())))
    );
    assert_eq!(access.refusal("Anyone", UUID, nobody()), None);
    assert_eq!(
        access.refusal("Anyone", OTHER, nobody()),
        Some(Refusal::NotWhitelisted)
    );
}

#[test]
fn an_empty_file_is_empty_lists() {
    assert_eq!(Access::parse("").unwrap(), Access::default());
}

#[test]
fn a_file_that_makes_no_sense_is_refused_and_says_where() {
    for (text, says) in [
        ("whitelist = 3", "whitelist"),
        (
            "[[ban]]\nreason = \"x\"",
            "[[ban]] number 1: needs exactly one",
        ),
        (
            "[[ban]]\nname = \"a\"\nip = \"1.2.3.4\"",
            "needs exactly one",
        ),
        ("[[ban]]\nname = \"bad name\"", "not a player name"),
        ("[[ban]]\nuuid = \"nope\"", "not a unique id"),
        ("[[ban]]\nip = \"1.2.3.4/40\"", "not a length"),
        (
            "[[ban]]\nname = \"a\"\n[[ban]]\nname = \"A\"",
            "[[ban]] number 2: A is banned twice",
        ),
        (
            "[[allow]]\nname = \"a\"\n[[allow]]\nname = \"A\"",
            "listed twice",
        ),
        ("[[allow]]\nip = \"1.2.3.4\"", "not addresses"),
        ("[[ban]]\nname = \"a\"\nnick = \"b\"", "nick"),
        ("unknown = true", "unknown"),
    ] {
        let error = Access::parse(text).unwrap_err().to_string();
        assert!(error.contains(says), "{text:?} gave {error:?}");
    }
}

fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("meridian-access-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_file_that_is_not_there_is_empty_lists() {
    let dir = folder("missing");
    assert_eq!(
        Access::load(&dir.join("access.toml")).unwrap(),
        Access::default()
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_lists_are_saved_whole_and_read_again() {
    let dir = folder("save");
    let path = dir.join("access.toml");
    let access = busy();
    access.save(&path).unwrap();
    assert_eq!(Access::load(&path).unwrap(), access);
    // A second save replaces the first, and leaves no temporary file.
    let mut smaller = Access::default();
    smaller.ban(target("Only"), None);
    smaller.save(&path).unwrap();
    assert_eq!(Access::load(&path).unwrap(), smaller);
    let files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(files, ["access.toml"]);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_file_that_cannot_be_read_names_itself() {
    let dir = folder("broken");
    let path = dir.join("access.toml");
    std::fs::write(&path, "[[ban]]\nname = \"bad name\"\n").unwrap();
    let error = Access::load(&path).unwrap_err().to_string();
    assert!(error.contains("access.toml"), "{error}");
    assert!(error.contains("[[ban]] number 1"), "{error}");
    // A folder in its place cannot be read.
    let in_place = dir.join("folder");
    std::fs::create_dir(&in_place).unwrap();
    let error = Access::load(&in_place).unwrap_err().to_string();
    assert!(error.starts_with("cannot read"), "{error}");
    // Nor written to a place that is not there.
    let error = Access::default()
        .save(&dir.join("no").join("access.toml"))
        .unwrap_err()
        .to_string();
    assert!(error.starts_with("cannot write"), "{error}");
    let _ = std::fs::remove_dir_all(dir);
}

fn with(access: &str) -> Config {
    Config::from_toml_str_in(
        &format!(
            "forwarding-secret = \"test-secret\"\n[[backends]]\nname = \"lobby\"\n\
             address = \"127.0.0.1:25566\"\n{access}"
        ),
        Path::new("conf"),
    )
    .unwrap()
}

#[test]
fn the_file_is_beside_the_configuration_unless_the_table_says_otherwise() {
    assert_eq!(
        with("").access.file,
        Some(Path::new("conf").join("access.toml"))
    );
    assert_eq!(
        with("[access]\nfile = \"lists/bans.toml\"").access.file,
        Some(Path::new("conf").join("lists/bans.toml"))
    );
    assert_eq!(Config::default().access, AccessSettings { file: None });
    let error =
        Config::from_toml_str("[access]\nfiles = 1\n[[backends]]\nname = \"a\"\naddress = \"h:1\"")
            .unwrap_err()
            .to_string();
    assert!(error.contains("files"), "{error}");
}
