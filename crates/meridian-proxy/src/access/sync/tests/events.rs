//! What a server's notifications and lists mean for the proxy's.
use super::super::events::*;
use meridian_config::Target;
use serde_json::json;

const ID: &str = "069a79f4-44e9-4726-a5be-fca90e38aaf5";

fn notice(method: &str, params: serde_json::Value) -> String {
    json!({ "jsonrpc": "2.0", "method": format!("minecraft:notification/{method}"), "params": [params] })
        .to_string()
}

fn target(text: &str) -> Target {
    Target::parse(text).unwrap()
}

#[test]
fn a_ban_on_a_server_is_a_ban_by_unique_id_with_the_servers_reason() {
    let line = notice(
        "bans/added",
        json!({ "player": { "id": ID, "name": "Alex" }, "source": "Kim", "reason": "griefing" }),
    );
    assert_eq!(
        parse_notice(&line),
        Some(Event::Ban {
            who: target(ID),
            reason: Some("griefing".into())
        })
    );
    // The id is what a server is sure of; a name alone is taken when there is no id.
    let by_name = notice("bans/added", json!({ "player": { "name": "Alex" } }));
    assert_eq!(
        parse_notice(&by_name),
        Some(Event::Ban {
            who: target("Alex"),
            reason: None
        })
    );
    // The method may come without vanilla's prefix.
    let bare = line.replace("minecraft:", "");
    assert!(parse_notice(&bare).is_some());
}

#[test]
fn a_ban_the_proxy_sent_comes_back_as_nothing() {
    let line = notice(
        "bans/added",
        json!({ "player": { "name": "Alex" }, "source": SOURCE }),
    );
    assert_eq!(parse_notice(&line), None);
    let ip = notice(
        "ip_bans/added",
        json!({ "ip": "203.0.113.9", "source": SOURCE }),
    );
    assert_eq!(parse_notice(&ip), None);
}

#[test]
fn a_ban_for_a_time_is_not_copied_and_says_why() {
    let line = notice(
        "bans/added",
        json!({ "player": { "name": "Alex" }, "expires": "2030-01-01T00:00:00Z" }),
    );
    let Some(Event::Skip(why)) = parse_notice(&line) else {
        panic!("not skipped")
    };
    assert!(
        why.contains("Alex") && why.contains("does not end"),
        "{why}"
    );
    // A ban that never ends has no `expires`, or a null one.
    let forever = notice(
        "bans/added",
        json!({ "player": { "name": "Alex" }, "expires": null }),
    );
    assert!(matches!(parse_notice(&forever), Some(Event::Ban { .. })));
}

#[test]
fn an_address_ban_and_its_lifting() {
    let ban = notice(
        "ip_bans/added",
        json!({ "ip": "203.0.113.9", "source": "Rcon", "reason": "spam" }),
    );
    assert_eq!(
        parse_notice(&ban),
        Some(Event::Ban {
            who: target("203.0.113.9"),
            reason: Some("spam".into())
        })
    );
    // The lifting carries the address alone, as a string.
    let lift = notice("ip_bans/removed", json!("203.0.113.9"));
    assert_eq!(
        parse_notice(&lift),
        Some(Event::Pardon {
            who: vec![target("203.0.113.9")]
        })
    );
    // What is not an address is not taken as one.
    assert_eq!(
        parse_notice(&notice("ip_bans/added", json!({ "ip": "not an address" }))),
        None
    );
    assert_eq!(
        parse_notice(&notice("ip_bans/removed", json!("Alex"))),
        None
    );
}

#[test]
fn a_pardon_names_the_player_by_id_and_by_name() {
    let line = notice("bans/removed", json!({ "id": ID, "name": "Alex" }));
    assert_eq!(
        parse_notice(&line),
        Some(Event::Pardon {
            who: vec![target(ID), target("Alex")]
        })
    );
    assert_eq!(parse_notice(&notice("bans/removed", json!({}))), None);
}

#[test]
fn a_player_allowed_on_a_server_is_allowed_and_one_removed_is_not_removed() {
    let line = notice("allowlist/added", json!({ "id": ID, "name": "Alex" }));
    assert_eq!(parse_notice(&line), Some(Event::Allow { who: target(ID) }));
    let removed = notice("allowlist/removed", json!({ "id": ID, "name": "Alex" }));
    assert_eq!(parse_notice(&removed), None);
}

#[test]
fn what_is_not_a_change_of_the_lists_is_nothing() {
    for line in [
        r#"{"jsonrpc":"2.0","method":"minecraft:notification/server/stopping"}"#,
        r#"{"jsonrpc":"2.0","method":"minecraft:notification/players/joined","params":[{"name":"x"}]}"#,
        r#"{"jsonrpc":"2.0","method":"minecraft:notification/bans/added"}"#,
        r#"{"jsonrpc":"2.0","method":"minecraft:notification/bans/added","params":[]}"#,
        r#"{"jsonrpc":"2.0","id":1,"result":[]}"#,
        "not json",
    ] {
        assert_eq!(parse_notice(line), None, "{line}");
    }
}

#[test]
fn a_ban_taken_from_a_server_is_marked_with_it() {
    assert_eq!(mark("east", None), "banned on east");
    assert_eq!(mark("east", Some("  ")), "banned on east");
    assert_eq!(mark("east", Some("griefing")), "banned on east: griefing");
    assert!(is_marked("east", "banned on east"));
    assert!(is_marked("east", "banned on east: griefing"));
    // Another server, a longer name, or a reason that merely starts alike.
    assert!(!is_marked("east", "banned on west"));
    assert!(!is_marked("east", "banned on east2"));
    assert!(!is_marked("east", "banned on eastern: x"));
    assert!(!is_marked("east", "griefing"));
    assert!(!is_marked("east", "banned on east:griefing"));
}

#[test]
fn the_lists_a_server_holds_are_taken_entry_by_entry() {
    let bans = json!([
        { "player": { "id": ID, "name": "Alex" }, "source": "Kim", "reason": "griefing" },
        { "player": { "name": "Steve" }, "source": SOURCE },
        { "player": { "name": "Notch" }, "expires": "2030-01-01T00:00:00Z" },
        { "nonsense": true },
    ]);
    let ip_bans = json!([{ "ip": "203.0.113.9", "source": "Rcon" }]);
    let allowlist = json!([{ "id": ID, "name": "Alex" }, { "name": "Herobrine" }]);
    let events = state(&bans, &ip_bans, &allowlist);
    assert_eq!(events.len(), 5, "{events:?}");
    assert_eq!(
        events[0],
        Event::Ban {
            who: target(ID),
            reason: Some("griefing".into())
        }
    );
    // The proxy's own ban (Steve) is not taken back; the one for a time is said, not taken.
    assert!(matches!(&events[1], Event::Skip(why) if why.contains("Notch")));
    assert_eq!(
        events[2],
        Event::Ban {
            who: target("203.0.113.9"),
            reason: None
        }
    );
    assert_eq!(events[3], Event::Allow { who: target(ID) });
    assert_eq!(
        events[4],
        Event::Allow {
            who: target("Herobrine")
        }
    );
    // Anything else than a list is an empty one.
    assert!(state(&json!(null), &json!({}), &json!("x")).is_empty());
}

// ---- The operators.

#[test]
fn a_notice_that_an_operator_was_added_or_removed_is_a_reason_to_read_the_list_again() {
    for method in ["operators/added", "operators/removed"] {
        let line = notice(
            method,
            json!({ "player": { "id": ID, "name": "Alex" }, "permissionLevel": 4, "bypassesPlayerLimit": false }),
        );
        assert!(is_operator_notice(&line), "{method}");
        // The proxy does not follow the notice itself.
        assert_eq!(parse_notice(&line), None, "{method}");
        let bare = line.replace("minecraft:", "");
        assert!(is_operator_notice(&bare), "{method} without the prefix");
    }
    for other in [
        "bans/added",
        "allowlist/added",
        "gamerules/updated",
        "server/status",
    ] {
        assert!(!is_operator_notice(&notice(other, json!({}))), "{other}");
    }
    assert!(!is_operator_notice("not json"));
    assert!(!is_operator_notice(
        r#"{"jsonrpc":"2.0","id":1,"result":[]}"#
    ));
}

#[test]
fn the_operators_of_a_server_are_its_listed_players_with_their_level() {
    let list = json!([
        { "player": { "id": ID, "name": "Alex" }, "permissionLevel": 4, "bypassesPlayerLimit": true },
        { "player": { "id": null, "name": "Steve" }, "permissionLevel": 2, "bypassesPlayerLimit": false },
        { "player": { "name": "Kim" }, "permissionLevel": 0 },
    ]);
    let ops = operators(&list).unwrap();
    assert_eq!(ops.len(), 3);
    assert_eq!(ops[0].name, "Alex");
    assert_eq!(ops[0].level, 4);
    assert_eq!(
        ops[0].id,
        match target(ID) {
            Target::Uuid(id) => Some(id),
            _ => None,
        }
    );
    // No id when the server gives none (or null).
    assert_eq!(
        (ops[1].name.as_str(), ops[1].level, ops[1].id),
        ("Steve", 2, None)
    );
    assert_eq!(
        (ops[2].name.as_str(), ops[2].level, ops[2].id),
        ("Kim", 0, None)
    );
    assert_eq!(operators(&json!([])).unwrap(), vec![]);
}

/// An id that is there but cannot be read is not "no id": the entry would be matched by name,
/// which a unique id was there to prevent.
#[test]
fn an_entry_with_an_id_that_cannot_be_read_is_not_taken_but_a_null_id_is() {
    let list = json!([
        { "player": { "id": "not-a-uuid", "name": "A" }, "permissionLevel": 4 },
        { "player": { "id": 12, "name": "B" }, "permissionLevel": 4 },
        { "player": { "id": "", "name": "C" }, "permissionLevel": 4 },
        { "player": { "id": ["x"], "name": "D" }, "permissionLevel": 4 },
        { "player": { "id": null, "name": "NoId" }, "permissionLevel": 4 },
        { "player": { "name": "Absent" }, "permissionLevel": 4 },
        { "player": { "id": ID, "name": "Fine" }, "permissionLevel": 4 },
    ]);
    let names: Vec<_> = operators(&list)
        .unwrap()
        .into_iter()
        .map(|op| op.name)
        .collect();
    assert_eq!(names, ["NoId", "Absent", "Fine"]);
}

#[test]
fn an_entry_the_proxy_cannot_name_or_whose_level_it_cannot_read_is_not_taken() {
    let list = json!([
        { "player": { "id": ID }, "permissionLevel": 4 },
        { "player": { "name": "  " }, "permissionLevel": 4 },
        { "player": { "name": "A" }, "permissionLevel": 5 },
        { "player": { "name": "B" }, "permissionLevel": -1 },
        { "player": { "name": "C" }, "permissionLevel": "4" },
        { "player": { "name": "D" } },
        { "permissionLevel": 4 },
        "Alex",
        { "player": { "name": "Fine" }, "permissionLevel": 3 },
    ]);
    let ops = operators(&list).unwrap();
    assert_eq!(ops.len(), 1, "{ops:?}");
    assert_eq!(ops[0].name, "Fine");
    // An answer that is not a list is not an answer.
    assert_eq!(operators(&json!({ "operators": [] })), None);
    assert_eq!(operators(&json!(null)), None);
    assert_eq!(operators(&json!("none")), None);
}
