//! The console's `ban`, `pardon`, `banlist` and `whitelist`.
use super::sync::{self, Change};
use super::{enforce, not_saved, summary};
use crate::console::{Commands, Reply, usage};
use crate::log::info;
use crate::state::Proxy;
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
    let (lifted, result) = proxy.sessions.access.change(|access| {
        let lifted = access.pardon(&target);
        (lifted, lifted)
    });
    if !lifted {
        return Err(format!("{target} is not banned"));
    }
    let line = format!("pardoned {target}");
    info!("{line}");
    let mut lines = vec![line];
    lines.extend(sync::send(proxy, &Change::Pardon { who: target }));
    saved(lines, result)
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
            let (done, result) = proxy.sessions.access.change(|access| {
                let done = access.disallow(&target);
                (done, done)
            });
            if !done {
                return Err(format!("{target} is not on the whitelist"));
            }
            let line = format!("{target} removed from the whitelist");
            info!("{line}");
            let mut lines = vec![line];
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
