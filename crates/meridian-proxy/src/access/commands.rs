//! The console's `ban`, `pardon`, `banlist` and `whitelist`.
use super::sync::{self, Change};
use super::{enforce, not_saved, summary};
use crate::console::{Commands, Reply, usage};
use crate::log::info;
use crate::state::Proxy;
use horizon_protocol::crypto::offline_uuid;
use meridian_config::Target;

/// What a list entry is, then what it is about.
fn entry(target: &Target) -> String {
    let kind = match target {
        Target::Name(_) => "name",
        Target::Uuid(_) => "uuid",
        Target::Net(_) => "ip",
    };
    format!("{kind:<5} {target}")
}

/// Adds the line that says the change is not on disk, when it is not.
fn saved(mut lines: Vec<String>, result: Result<(), meridian_config::AccessError>) -> Reply {
    if let Err(e) = result {
        lines.push(not_saved(&e));
    }
    Ok(lines)
}

pub fn ban(proxy: &Proxy, commands: &Commands, args: &[&str]) -> Reply {
    let Some((who, reason)) = args.split_first() else {
        return Err(usage("ban", commands));
    };
    let target = Target::parse(who)?;
    let reason = (!reason.is_empty()).then(|| reason.join(" "));
    let (made, result) = proxy.sessions.access.change(|access| {
        let made = access.ban(target.clone(), reason.clone());
        (made, made)
    });
    if !made {
        return Err(format!("{target} is already banned"));
    }
    let line = match &reason {
        Some(reason) => format!("banned {target}: {reason}"),
        None => format!("banned {target}"),
    };
    info!("{line}");
    let mut lines = vec![line];
    lines.extend(enforce(&proxy.sessions));
    lines.extend(sync::send(
        proxy,
        &Change::Ban {
            who: target,
            reason,
        },
    ));
    saved(lines, result)
}

pub fn pardon(proxy: &Proxy, commands: &Commands, args: &[&str]) -> Reply {
    let [who] = args else {
        return Err(usage("pardon", commands));
    };
    let target = Target::parse(who)?;
    let ids = offline_ids(proxy, &target);
    let servers: Vec<String> = proxy
        .config()
        .backends
        .iter()
        .map(|b| b.name.clone())
        .collect();
    let ((by_name, from_servers), result) = proxy.sessions.access.change(|access| {
        let by_name = access.pardon(&target);
        let mut from_servers = Vec::new();
        for id in ids {
            let taken = access.bans().iter().any(|ban| {
                ban.target.same(&id)
                    && ban
                        .reason
                        .as_deref()
                        .is_some_and(|r| servers.iter().any(|s| sync::events::is_marked(s, r)))
            });
            if taken && access.pardon(&id) {
                from_servers.push(id);
            }
        }
        let lifted = by_name || !from_servers.is_empty();
        ((by_name, from_servers), lifted)
    });
    if !by_name && from_servers.is_empty() {
        return Err(format!("{target} is not banned"));
    }
    let line = format!("pardoned {target}");
    info!("{line}");
    let mut lines = vec![line];
    for id in &from_servers {
        lines.push(format!("also lifted {id}, the ban taken from a server"));
    }
    lines.extend(sync::send(proxy, &Change::Pardon { who: target }));
    saved(lines, result)
}

/// The unique ids under which a ban or a whitelist entry of the player `target` may have been
/// taken from a server (`[access.sync]`, `from-servers`), which files it by the id the server
/// gives. Only where the id follows from the name: in offline mode the proxy itself gives a player
/// the id of their name as typed, and a server may use that or the id of the lower-case name
/// (Horizon `02f05c1` used the first; the code that makes the id is the server's), so both are
/// tried. In online and hybrid
/// mode the account's id cannot be known from a name, so none is guessed and the entry is
/// pardoned by its id. Nothing while the sync is off, so that a pardon is then what it was.
fn offline_ids(proxy: &Proxy, target: &Target) -> Vec<Target> {
    let Target::Name(name) = target else {
        return Vec::new();
    };
    let config = proxy.config();
    let sync = config.access.sync;
    if config.online_mode || !(sync.from_servers || sync.to_servers) {
        return Vec::new();
    }
    let id = |name: &str| Target::Uuid(offline_uuid(name).to_be_bytes());
    let mut ids = vec![id(name)];
    let lower = name.to_ascii_lowercase();
    if lower != *name {
        ids.push(id(&lower));
    }
    ids
}

pub fn banlist(proxy: &Proxy, commands: &Commands, args: &[&str]) -> Reply {
    if !args.is_empty() {
        return Err(usage("banlist", commands));
    }
    let access = proxy.sessions.access.snapshot();
    if access.bans().is_empty() {
        return Ok(vec!["no bans".to_owned()]);
    }
    let mut lines = vec![format!("{}:", summary_bans(access.bans().len()))];
    for ban in access.bans() {
        lines.push(match &ban.reason {
            Some(reason) => format!("  {}: {reason}", entry(&ban.target)),
            None => format!("  {}", entry(&ban.target)),
        });
    }
    Ok(lines)
}

fn summary_bans(n: usize) -> String {
    match n {
        1 => "1 ban".to_owned(),
        n => format!("{n} bans"),
    }
}

pub fn whitelist(proxy: &Proxy, commands: &Commands, args: &[&str]) -> Reply {
    let wrong = || usage("whitelist", commands);
    match args {
        ["add", who] => {
            let target = Target::parse(who)?;
            let (done, result) = proxy.sessions.access.change(|access| {
                let done = access.allow(target.clone());
                let changed = done == Ok(true);
                (done, changed)
            });
            if !done? {
                return Err(format!("{target} is already on the whitelist"));
            }
            let line = format!("{target} added to the whitelist");
            info!("{line}");
            let mut lines = vec![line];
            lines.extend(sync::send(proxy, &Change::Allow { who: target }));
            saved(lines, result)
        }
        ["remove", who] => {
            let target = Target::parse(who)?;
            let ids = offline_ids(proxy, &target);
            let ((by_name, by_id), result) = proxy.sessions.access.change(|access| {
                let by_name = access.disallow(&target);
                // An entry a server's allowlist gave is by unique id.
                let mut by_id = Vec::new();
                for id in ids {
                    if access.disallow(&id) {
                        by_id.push(id);
                    }
                }
                let done = by_name || !by_id.is_empty();
                ((by_name, by_id), done)
            });
            if !by_name && by_id.is_empty() {
                return Err(format!("{target} is not on the whitelist"));
            }
            let line = format!("{target} removed from the whitelist");
            info!("{line}");
            let mut lines = vec![line];
            for id in &by_id {
                lines.push(format!("also removed {id}, the entry taken from a server"));
            }
            lines.extend(sync::send(proxy, &Change::Disallow { who: target }));
            saved(lines, result)
        }
        ["list"] | [] => {
            let access = proxy.sessions.access.snapshot();
            let state = if access.whitelist_on() { "on" } else { "off" };
            let mut lines = vec![format!(
                "whitelist {state}, {} listed{}",
                access.allowed().len(),
                if access.allowed().is_empty() { "" } else { ":" }
            )];
            lines.extend(access.allowed().iter().map(|t| format!("  {}", entry(t))));
            Ok(lines)
        }
        [switch @ ("on" | "off")] => {
            let on = *switch == "on";
            let ((was, listed), result) = proxy.sessions.access.change(|access| {
                let was = access.whitelist_on();
                access.set_whitelist(on);
                ((was, access.allowed().len()), was != on)
            });
            let line = format!(
                "whitelist {}{}",
                switch,
                if was == on { " already" } else { "" }
            );
            info!("{line}");
            let mut lines = vec![line];
            if on && listed == 0 {
                lines
                    .push("the whitelist is empty: no one can join until players are added".into());
            }
            if on && was != on {
                lines
                    .push("players already connected stay; kick ends a session by hand".to_owned());
            }
            saved(lines, result)
        }
        _ => Err(wrong()),
    }
}

/// What a reload says of the lists: what they hold when they changed.
pub fn reloaded(
    proxy: &Proxy,
    new: meridian_config::Access,
    file: Option<std::path::PathBuf>,
) -> Vec<String> {
    let was = proxy.sessions.access.snapshot();
    if was == new {
        proxy.sessions.access.install(new, file);
        return Vec::new();
    }
    let line = format!("bans and whitelist read again: {}", summary(&new));
    proxy.sessions.access.install(new, file);
    info!("reload: {line}");
    let mut lines = vec![line];
    lines.extend(enforce(&proxy.sessions));
    lines
}
