use super::*;
use meridian_config::{AuthMode, Group, Route};

const BACKENDS: &str = r#"
[[backends]]
name = "lobby"
address = "127.0.0.1:25566"
[[backends]]
name = "east"
address = "127.0.0.1:25567"
[[backends]]
name = "west"
address = "127.0.0.1:25568"
[[backends]]
name = "open"
address = "127.0.0.1:25569"
[[backends]]
name = "open-2"
address = "127.0.0.1:25570"
"#;

/// A configuration with five backends; `top` holds top-level keys and `tables`
/// more tables.
fn config(top: &str, tables: &str) -> Config {
    Config::from_toml_str(&format!(
        "forwarding-secret = \"test-secret\"\ndefault-backend = \"lobby\"\n{top}\n{BACKENDS}{tables}"
    ))
    .unwrap()
}

/// A hybrid configuration: accounts on lobby, east and west, guests on the two
/// open backends.
fn hybrid(top: &str, tables: &str) -> Config {
    config(
        &format!("auth-mode = \"hybrid\"\n{top}"),
        &format!(
            r#"
[[group]]
name = "accounts"
auth = "verified"
backends = ["east", "lobby"]
[[group]]
name = "guests"
auth = "unverified"
backends = ["open", "open-2"]
[[route]]
auth = "verified"
group = "accounts"
[[route]]
auth = "unverified"
group = "guests"
{tables}"#
        ),
    )
}

fn names(list: Result<Vec<String>, RouteError>) -> Vec<String> {
    list.unwrap()
}

const VERIFIED: AuthKind = AuthKind::Verified;
const UNVERIFIED: AuthKind = AuthKind::Unverified;

// Without routes.

#[test]
fn the_default_backend_alone_when_nothing_else_is_configured() {
    let config = config("", "");
    assert_eq!(names(candidates(&config, None, VERIFIED)), ["lobby"]);
    assert_eq!(
        names(candidates(&config, Some("play.example.com"), VERIFIED)),
        ["lobby"]
    );
}

#[test]
fn try_follows_the_default_backend_in_order() {
    let config = config("try = [\"west\", \"east\"]", "");
    assert_eq!(
        names(candidates(&config, None, VERIFIED)),
        ["lobby", "west", "east"]
    );
}

#[test]
fn an_empty_try_adds_nothing() {
    let config = config("try = []", "");
    assert_eq!(names(candidates(&config, None, VERIFIED)), ["lobby"]);
}

#[test]
fn groups_without_routes_choose_nothing() {
    let config = config(
        "",
        "[[group]]\nname = \"g\"\nauth = \"unverified\"\nbackends = [\"lobby\"]",
    );
    // Without a route, a verified player still gets the default backend.
    assert_eq!(names(candidates(&config, None, VERIFIED)), ["lobby"]);
}

// Forced hosts.

const FORCED: &str = r#"
[[forced-host]]
host = "west.example.com"
backend = "west"
"#;

#[test]
fn a_forced_host_comes_first_then_try() {
    let config = config("try = [\"east\"]", FORCED);
    assert_eq!(
        names(candidates(&config, Some("west.example.com"), VERIFIED)),
        ["west", "east"]
    );
}

#[test]
fn a_forced_host_replaces_the_default_backend() {
    let config = config("", FORCED);
    assert_eq!(
        names(candidates(&config, Some("west.example.com"), VERIFIED)),
        ["west"]
    );
}

#[test]
fn a_forced_host_ignores_case_and_a_final_dot() {
    let config = config("", FORCED);
    for host in ["WEST.Example.com", "west.example.com.", "West.Example.Com."] {
        assert_eq!(
            names(candidates(&config, Some(host), VERIFIED)),
            ["west"],
            "{host}"
        );
    }
}

#[test]
fn an_unknown_host_falls_back_to_the_default_backend() {
    let config = config("try = [\"east\"]", FORCED);
    assert_eq!(
        names(candidates(&config, Some("other.example.com"), VERIFIED)),
        ["lobby", "east"]
    );
}

#[test]
fn a_forced_backend_also_in_try_is_listed_once() {
    let config = config("try = [\"west\", \"east\"]", FORCED);
    assert_eq!(
        names(candidates(&config, Some("west.example.com"), VERIFIED)),
        ["west", "east"]
    );
}

// Routes.

#[test]
fn a_verified_player_gets_the_verified_group_in_order() {
    let config = hybrid("", "");
    assert_eq!(
        names(candidates(&config, None, VERIFIED)),
        ["east", "lobby"]
    );
}

#[test]
fn an_unverified_player_gets_the_unverified_group() {
    let config = hybrid("", "");
    assert_eq!(
        names(candidates(&config, None, UNVERIFIED)),
        ["open", "open-2"]
    );
}

#[test]
fn try_after_a_group_skips_backends_already_listed() {
    // The group already lists lobby, which the file could not also put in try
    // since it is the default backend, so try is set in code.
    let mut config = hybrid("", "");
    config.try_backends = vec!["lobby".into(), "west".into()];
    assert_eq!(
        names(candidates(&config, None, VERIFIED)),
        ["east", "lobby", "west"]
    );
}

#[test]
fn try_never_mixes_the_two_kinds() {
    // west is in no group, so both kinds may use it; open is for guests only.
    let mut config = hybrid("", "");
    config.try_backends = vec!["west".into(), "open".into()];
    assert_eq!(
        names(candidates(&config, None, VERIFIED)),
        ["east", "lobby", "west"]
    );
    assert_eq!(
        names(candidates(&config, None, UNVERIFIED)),
        ["open", "open-2", "west"]
    );
}

#[test]
fn a_forced_host_of_the_other_kind_is_left_out() {
    let config = hybrid(
        "",
        "[[forced-host]]\nhost = \"guests.example.com\"\nbackend = \"open\"",
    );
    // An account that connects through the guests' host name is not sent there.
    let e = candidates(&config, Some("guests.example.com"), VERIFIED).unwrap_err();
    assert_eq!(e, RouteError::NoCandidate(VERIFIED));
    // A guest is.
    assert_eq!(
        names(candidates(&config, Some("guests.example.com"), UNVERIFIED)),
        ["open"]
    );
}

#[test]
fn a_forced_host_of_the_right_kind_comes_before_the_group() {
    let config = hybrid(
        "",
        "[[forced-host]]\nhost = \"west.example.com\"\nbackend = \"west\"",
    );
    // The forced host replaces the group, as it replaces the default backend.
    assert_eq!(
        names(candidates(&config, Some("west.example.com"), VERIFIED)),
        ["west"]
    );
}

#[test]
fn online_mode_with_a_verified_route_only() {
    let config = config(
        "",
        r#"
[[group]]
name = "accounts"
auth = "verified"
backends = ["west", "east"]
[[route]]
auth = "verified"
group = "accounts"
"#,
    );
    assert_eq!(config.auth_mode, AuthMode::Online);
    assert_eq!(names(candidates(&config, None, VERIFIED)), ["west", "east"]);
    // An unverified player cannot happen in online mode; if one did, there is no
    // route for them rather than a guess.
    assert_eq!(
        candidates(&config, None, UNVERIFIED),
        Err(RouteError::NoRoute(UNVERIFIED))
    );
}

// Configurations built in code, which nothing validated.

#[test]
fn a_route_to_a_missing_group_is_an_error() {
    let mut config = config("", "");
    config.routes = vec![Route {
        auth: VERIFIED,
        group: "nowhere".into(),
    }];
    assert_eq!(
        candidates(&config, None, VERIFIED),
        Err(RouteError::NoGroup {
            auth: VERIFIED,
            group: "nowhere".into()
        })
    );
}

#[test]
fn an_unknown_backend_is_an_error_wherever_it_comes_from() {
    let mut config = config("", "");
    config.default_backend = "nether".into();
    assert_eq!(
        candidates(&config, None, VERIFIED),
        Err(RouteError::UnknownBackend("nether".into()))
    );

    let mut config = self::config("", "");
    config.try_backends = vec!["nether".into()];
    assert_eq!(
        candidates(&config, None, VERIFIED),
        Err(RouteError::UnknownBackend("nether".into()))
    );

    let mut config = self::config("", "");
    config.groups = vec![Group {
        name: "g".into(),
        auth: VERIFIED,
        backends: vec!["nether".into()],
    }];
    config.routes = vec![Route {
        auth: VERIFIED,
        group: "g".into(),
    }];
    assert_eq!(
        candidates(&config, None, VERIFIED),
        Err(RouteError::UnknownBackend("nether".into()))
    );
}

#[test]
fn a_group_listing_a_backend_twice_gives_it_once() {
    let mut config = config("", "");
    config.groups = vec![Group {
        name: "g".into(),
        auth: VERIFIED,
        backends: vec!["east".into(), "east".into(), "west".into()],
    }];
    config.routes = vec![Route {
        auth: VERIFIED,
        group: "g".into(),
    }];
    assert_eq!(names(candidates(&config, None, VERIFIED)), ["east", "west"]);
}

#[test]
fn errors_read_well() {
    assert_eq!(
        RouteError::NoRoute(UNVERIFIED).to_string(),
        "no route is configured for unverified players"
    );
    assert_eq!(
        RouteError::UnknownBackend("nether".into()).to_string(),
        "\"nether\" is not a configured backend"
    );
}
