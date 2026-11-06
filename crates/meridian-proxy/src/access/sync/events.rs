//! What a server's management protocol says about its lists, as changes of the proxy's
//! (docs/OPERATING.md, section 5.12). Nothing here touches the lists: a notification, or
//! an entry of a list the server holds, becomes an [`Event`], and `hear.rs` applies it.
//!
//! The shapes are those of Horizon's management protocol, which follows vanilla's:
//!
//! | Notification (one element in `params`) | Element |
//! | --- | --- |
//! | `notification/bans/added` | `{"player": {"id", "name"}, "source", "reason", "expires"}` |
//! | `notification/bans/removed` | `{"id", "name"}` |
//! | `notification/ip_bans/added` | `{"ip", "source", "reason", "expires"}` |
//! | `notification/ip_bans/removed` | the address, as a string |
//! | `notification/allowlist/added` | `{"id", "name"}` |
//! | `notification/allowlist/removed` | `{"id", "name"}`, not applied |
//!
//! `bans`, `ip_bans` and `allowlist` give the same elements in a list.
//!
//! The operators (docs/OPERATING.md, section 5.9) are read, not followed: a notification that
//! one was added or removed ([`is_operator_notice`]) only tells the proxy to read the list again,
//! which is `operators`, a list of `{"player": {"id", "name"}, "permissionLevel",
//! "bypassesPlayerLimit"}`.
use crate::operators::Op;
use meridian_config::Target;
use serde_json::Value;

/// The `source` of a ban the proxy sends to a server. A ban that comes back with it is the
/// proxy's own, which the proxy's lists have already: it is not taken again.
pub const SOURCE: &str = "Meridian";

/// One change of a server's lists, as it would be made on the proxy's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// A ban. The reason is the server's own, when it gave one.
    Ban { who: Target, reason: Option<String> },
    /// A ban lifted: the player by their unique id and by their name, since the proxy's
    /// ban may be by either.
    Pardon { who: Vec<Target> },
    /// A player added to the allowlist.
    Allow { who: Target },
    /// Something that is not copied, and why.
    Skip(String),
}

/// The reason the proxy gives a ban it took from `server`, which also marks the ban as that
/// server's: `banned on east`, `banned on east: griefing`.
pub fn mark(server: &str, reason: Option<&str>) -> String {
    match reason.map(str::trim).filter(|r| !r.is_empty()) {
        Some(reason) => format!("banned on {server}: {reason}"),
        None => format!("banned on {server}"),
    }
}

/// Whether `reason` is that of a ban taken from `server`: only such a ban is lifted when
/// the server pardons the player.
pub fn is_marked(server: &str, reason: &str) -> bool {
    let mark = format!("banned on {server}");
    reason == mark
        || reason
            .strip_prefix(&mark)
            .is_some_and(|rest| rest.starts_with(": "))
}

/// The event a notification is, if it is one the proxy follows.
pub fn parse_notice(line: &str) -> Option<Event> {
    let message: Value = serde_json::from_str(line).ok()?;
    let method = message.get("method")?.as_str()?;
    let method = method.strip_prefix("minecraft:").unwrap_or(method);
    // Vanilla puts the one parameter in an array.
    let element = message.get("params")?.as_array()?.first()?;
    match method {
        "notification/bans/added" => ban(element),
        "notification/ip_bans/added" => ip_ban(element),
        "notification/bans/removed" => {
            let who = players(element);
            (!who.is_empty()).then_some(Event::Pardon { who })
        }
        "notification/ip_bans/removed" => {
            address(element.as_str()?).map(|who| Event::Pardon { who: vec![who] })
        }
        "notification/allowlist/added" => allow(element),
        // A player taken off a server's allowlist may be allowed on the others: the
        // proxy's whitelist is not shortened by it.
        _ => None,
    }
}

/// Whether `line` tells that an operator was added or removed on a server: the proxy then reads
/// the list of operators again, which is the whole truth, whatever the notice carried.
pub fn is_operator_notice(line: &str) -> bool {
    let Ok(message) = serde_json::from_str::<Value>(line) else {
        return false;
    };
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return false;
    };
    matches!(
        method.strip_prefix("minecraft:").unwrap_or(method),
        "notification/operators/added" | "notification/operators/removed"
    )
}

/// The operators in the list a server holds (`operators`), or `None` when the answer is not a
/// list. An entry with no player name, with a level that is not 0 to 4, or with an id that is
/// present but is not a unique id, is not taken: a player the proxy cannot name or whose level or
/// id it cannot read has no standing.
pub fn operators(list: &Value) -> Option<Vec<Op>> {
    let entries = list.as_array()?;
    Some(
        entries
            .iter()
            .filter_map(|entry| {
                let player = entry.get("player")?;
                let name = player.get("name")?.as_str()?.trim();
                let level = u8::try_from(entry.get("permissionLevel")?.as_u64()?)
                    .ok()
                    .filter(|level| *level <= meridian_config::MAX_OPERATOR_LEVEL)?;
                // No id (absent or null) is an entry the server could not give one for. An id that
                // is there but cannot be read is not that: the entry is dropped, not matched by
                // name.
                let id = match player.get("id") {
                    None | Some(Value::Null) => None,
                    Some(id) => match id.as_str().map(Target::parse) {
                        Some(Ok(Target::Uuid(id))) => Some(id),
                        _ => return None,
                    },
                };
                (!name.is_empty()).then(|| Op {
                    id,
                    name: name.to_owned(),
                    level,
                })
            })
            .collect(),
    )
}

/// A ban of a player (a `UserBanDto`).
pub fn ban(entry: &Value) -> Option<Event> {
    if entry.get("source").and_then(Value::as_str) == Some(SOURCE) {
        return None;
    }
    let who = players(entry.get("player")?).into_iter().next()?;
    if expires(entry) {
        return Some(skip(&who));
    }
    Some(Event::Ban {
        who,
        reason: reason(entry),
    })
}

/// A ban of an address (an `IpBanDto`).
pub fn ip_ban(entry: &Value) -> Option<Event> {
    if entry.get("source").and_then(Value::as_str) == Some(SOURCE) {
        return None;
    }
    let who = address(entry.get("ip")?.as_str()?)?;
    if expires(entry) {
        return Some(skip(&who));
    }
    Some(Event::Ban {
        who,
        reason: reason(entry),
    })
}

/// A player of the allowlist (a `PlayerDto`).
pub fn allow(entry: &Value) -> Option<Event> {
    let who = players(entry).into_iter().next()?;
    Some(Event::Allow { who })
}

/// The changes the three lists a server holds amount to, for the proxy to take what it does
/// not have. Nothing in them removes anything.
pub fn state(bans: &Value, ip_bans: &Value, allowlist: &Value) -> Vec<Event> {
    let each = |list: &Value, entry: fn(&Value) -> Option<Event>| -> Vec<Event> {
        list.as_array()
            .into_iter()
            .flatten()
            .filter_map(entry)
            .collect()
    };
    let mut events = each(bans, ban);
    events.extend(each(ip_bans, ip_ban));
    events.extend(each(allowlist, allow));
    events
}

/// Whether the ban ends some day: the proxy's bans never do.
fn expires(entry: &Value) -> bool {
    entry.get("expires").is_some_and(|e| !e.is_null())
}

fn skip(who: &Target) -> Event {
    Event::Skip(format!(
        "{who} is banned for a time on the server; a ban of the proxy does not end, so it is not copied"
    ))
}

fn reason(entry: &Value) -> Option<String> {
    entry
        .get("reason")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// A single address, from the text a server writes.
fn address(text: &str) -> Option<Target> {
    match Target::parse(text) {
        Ok(target @ Target::Net(_)) => Some(target),
        _ => None,
    }
}

/// The ways to name a player (`{"id", "name"}`) that the proxy's lists understand: the
/// unique id first, then the name.
fn players(player: &Value) -> Vec<Target> {
    let mut targets = Vec::new();
    if let Some(id) = player.get("id").and_then(Value::as_str)
        && let Ok(target @ Target::Uuid(_)) = Target::parse(id)
    {
        targets.push(target);
    }
    if let Some(name) = player.get("name").and_then(Value::as_str)
        && let Ok(target @ Target::Name(_)) = Target::parse(name)
    {
        targets.push(target);
    }
    targets
}
