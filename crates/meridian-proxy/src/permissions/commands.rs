//! The console's `perm`.
use super::{not_saved, summary};
use crate::console::{Commands, Reply, usage};
use crate::log::info;
use crate::state::Proxy;
use meridian_config::{AccessError, Permissions, Setting, Target, check_node};

fn saved(mut lines: Vec<String>, result: Result<(), AccessError>) -> Reply {
    if let Err(e) = result {
        lines.push(not_saved(&e));
    }
    Ok(lines)
}

/// A player as the console names them: a name, or a unique id.
fn player(text: &str) -> Result<Target, String> {
    match Target::parse(text)? {
        Target::Net(_) => Err("permissions are for player names and unique ids".to_owned()),
        target => Ok(target),
    }
}

fn nodes(list: &[String]) -> String {
    if list.is_empty() {
        "-".to_owned()
    } else {
        list.join(", ")
    }
}

pub fn perm(proxy: &Proxy, commands: &Commands, args: &[&str]) -> Reply {
    let wrong = || usage("perm", commands);
    let lists = &proxy.sessions.permissions;
    match args {
        ["list"] | [] => {
            let p = lists.snapshot();
            let mut lines = vec![format!("permissions: {}", summary(&p))];
            for group in p.groups() {
                let parents = if group.inherits.is_empty() {
                    String::new()
                } else {
                    format!(" (inherits {})", group.inherits.join(", "))
                };
                lines.push(format!(
                    "  group {}{parents}: allow {}; deny {}",
                    group.name,
                    nodes(&group.allow),
                    nodes(&group.deny)
                ));
            }
            for member in p.members() {
                lines.push(format!("  {}: {}", member.who, member.groups.join(", ")));
            }
            Ok(lines)
        }
        ["list", who] => {
            let who = player(who)?;
            let p = lists.snapshot();
            let (name, uuid) = identity(proxy, &who);
            let groups = p.groups_of(&name, uuid);
            Ok(vec![if groups.is_empty() {
                format!("{who} is in no group (only default)")
            } else {
                format!("{who} is in {}", groups.join(", "))
            }])
        }
        ["check", who, node] => {
            let who = player(who)?;
            let node = check_node(node)?;
            if node.contains('*') {
                return Err("check takes a node, not a pattern".to_owned());
            }
            let (name, uuid) = identity(proxy, &who);
            let allowed = lists.snapshot().allows(&name, uuid, &node);
            let mut line = format!(
                "{who} {} {node}",
                if allowed { "has" } else { "does not have" }
            );
            if matches!(who, Target::Name(_)) && proxy.sessions.find(&name).is_none() {
                line.push_str(" (by name: a rank given to their unique id is not seen)");
            }
            Ok(vec![line])
        }
        ["grant", who, group] => {
            let who = player(who)?;
            let (done, result) = lists.change(|p| {
                let done = p.grant(who.clone(), group);
                let changed = done == Ok(true);
                (done, changed)
            });
            if !done? {
                return Err(format!("{who} is already in {group}"));
            }
            let line = format!("{who} is now in {group}");
            info!("permissions: {line}");
            saved(vec![line], result)
        }
        ["revoke", who, group] => {
            let who = player(who)?;
            let (done, result) = lists.change(|p| {
                let done = p.revoke(&who, group);
                (done, done)
            });
            if !done {
                return Err(format!("{who} is not in {group}"));
            }
            let line = format!("{who} is no longer in {group}");
            info!("permissions: {line}");
            saved(vec![line], result)
        }
        ["group", group, action @ ("create" | "delete")] => {
            let create = *action == "create";
            let (done, result) = lists.change(|p| {
                let done = if create {
                    p.create_group(group)
                } else {
                    Ok(p.delete_group(group))
                };
                let changed = done == Ok(true);
                (done, changed)
            });
            let line = match (create, done?) {
                (true, true) => format!("group {group} created"),
                (true, false) => return Err(format!("group {group} exists")),
                (false, true) => format!("group {group} deleted"),
                (false, false) => return Err(format!("there is no group called {group}")),
            };
            info!("permissions: {line}");
            saved(vec![line], result)
        }
        ["group", group, action @ ("allow" | "deny" | "unset"), node] => {
            let setting = match *action {
                "allow" => Setting::Allow,
                "deny" => Setting::Deny,
                _ => Setting::Unset,
            };
            let node = check_node(node)?;
            let (done, result) = lists.change(|p| {
                let done = p.set(group, &node, setting);
                let changed = done == Ok(true);
                (done, changed)
            });
            if !done? {
                return Err(format!("{node} is already so in {group}"));
            }
            let line = match setting {
                Setting::Allow => format!("{group} allows {node}"),
                Setting::Deny => format!("{group} denies {node}"),
                Setting::Unset => format!("{group} no longer says anything of {node}"),
            };
            info!("permissions: {line}");
            saved(vec![line], result)
        }
        ["group", group, action @ ("inherit" | "uninherit"), parent] => {
            let inherit = *action == "inherit";
            let (done, result) = lists.change(|p: &mut Permissions| {
                let done = if inherit {
                    p.inherit(group, parent)
                } else {
                    p.uninherit(group, parent)
                };
                let changed = done == Ok(true);
                (done, changed)
            });
            let line = match (inherit, done?) {
                (true, true) => format!("{group} inherits {parent}"),
                (true, false) => return Err(format!("{group} already inherits {parent}")),
                (false, true) => format!("{group} no longer inherits {parent}"),
                (false, false) => return Err(format!("{group} does not inherit {parent}")),
            };
            info!("permissions: {line}");
            saved(vec![line], result)
        }
        _ => Err(wrong()),
    }
}

/// The name and unique id to check a player by: those of the player online, else the
/// name alone or the unique id alone, which is all the console knows.
fn identity(proxy: &Proxy, who: &Target) -> (String, [u8; 16]) {
    match who {
        Target::Name(name) => match proxy.sessions.find(name) {
            Some(online) => (online.name, online.uuid),
            None => (name.clone(), [0; 16]),
        },
        Target::Uuid(uuid) => match proxy
            .sessions
            .players()
            .into_iter()
            .find(|p| p.uuid == *uuid)
        {
            Some(online) => (online.name, *uuid),
            None => (String::new(), *uuid),
        },
        Target::Net(_) => (String::new(), [0; 16]),
    }
}

/// What a reload says of the permissions: what they hold when they changed.
pub fn reloaded(proxy: &Proxy, new: Permissions, file: Option<std::path::PathBuf>) -> Vec<String> {
    let changed = proxy.sessions.permissions.snapshot() != new;
    let line = format!("permissions read again: {}", summary(&new));
    proxy.sessions.permissions.install(new, file);
    if !changed {
        return Vec::new();
    }
    info!("reload: {line}");
    vec![line]
}
