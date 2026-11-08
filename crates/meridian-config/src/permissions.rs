//! Who may do what in game and on the console of the proxy: groups of permission nodes,
//! and the players in them (docs/OPERATING.md, section 5.9).
//!
//! The lists live in a file of their own, `permissions.toml` next to the configuration
//! unless `[permissions] file` says otherwise, for the reasons `access.toml` does: the
//! console changes them while the proxy runs. It is read at the start and at `reload`.
//!
//! ```toml
//! [group.default]            # everyone is in it, and it need not be written
//! deny = ["meridian.play"]
//!
//! [group.moderator]
//! inherits = ["default"]
//! allow = ["meridian.glist", "meridian.alert"]
//!
//! [group.admin]
//! inherits = ["moderator"]
//! allow = ["meridian.send", "meridian.server.*"]
//!
//! [[player]]                 # one of name, uuid
//! name = "Alex"
//! groups = ["admin"]
//! ```
//!
//! A node is lower-case words joined by dots (`meridian.send`), or `*` for every node, or
//! words ending in `.*` for every node below them (`meridian.server.*`; not the words
//! themselves). A group allows and denies nodes. Whether a player has a node is decided by
//! the groups nearest to them: their own groups first, then those the groups inherit,
//! level by level, then `default`. The first level with a node or pattern that matches
//! decides; among the matches of a level the most specific wins (a node over a pattern, a
//! longer pattern over a shorter, `*` last) and a deny wins a tie. A node nothing
//! matches is refused, except the commands that were open before there were permissions
//! ([`Permissions::allows`]).
//!
//! A name is not proof of who is playing unless the proxy checked the account (online
//! mode); a unique id is the account's only then too (see `access.rs`).
use super::*;
use crate::access::{check_name, parse_uuid, quote, write_whole};
use std::collections::BTreeMap;

/// The group everyone is in, whether or not it is written.
pub const DEFAULT_GROUP: &str = "default";

/// The nodes the proxy checks.
pub mod node {
    /// `/send`, and `send` of players by others.
    pub const SEND: &str = "meridian.send";
    /// `/glist`.
    pub const GLIST: &str = "meridian.glist";
    /// `/alert`.
    pub const ALERT: &str = "meridian.alert";
    /// `/server`, to list the servers and to move.
    pub const SERVER: &str = "meridian.server";
    /// `/hub` and `/lobby`.
    pub const HUB: &str = "meridian.hub";
    /// `/play`.
    pub const PLAY: &str = "meridian.play";

    /// What every node of a command of an extension begins with.
    pub const EXTENSION: &str = "ext";

    /// The node for going to the server `name` with `/server <name>`.
    pub fn server(name: &str) -> String {
        format!("{SERVER}.{}", name.to_ascii_lowercase())
    }

    /// The node of the command `command` of the extension `extension`: `ext.<extension>.<command>`.
    pub fn extension(extension: &str, command: &str) -> String {
        format!(
            "{EXTENSION}.{}.{}",
            extension.to_ascii_lowercase(),
            command.to_ascii_lowercase()
        )
    }
}

/// Whether `node`, that no group says anything about, is allowed all the same. The
/// commands of the proxy that were for everyone before permissions existed stay so, until
/// a group denies them. So are the commands of an extension (`ext.<extension>.<command>`): the
/// extension can refuse when it runs one, and an operator who wants one closed denies its node.
fn open_by_default(node: &str) -> bool {
    matches!(node, node::SERVER | node::HUB | node::PLAY)
        || node
            .strip_prefix(node::SERVER)
            .is_some_and(|rest| rest.starts_with('.'))
        || node
            .strip_prefix(node::EXTENSION)
            .is_some_and(|rest| rest.starts_with('.'))
}

/// A group.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PermissionGroup {
    pub name: String,
    /// The groups whose nodes this one has when it says nothing of its own.
    pub inherits: Vec<String>,
    pub allow: Vec<String>,
    pub deny: Vec<String>,
}

/// A player, by name or unique id, and the groups they are in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub who: Target,
    pub groups: Vec<String>,
}

/// What a node is set to in a group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Setting {
    Allow,
    Deny,
    Unset,
}

/// The groups and the players in them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Permissions {
    groups: Vec<PermissionGroup>,
    members: Vec<Member>,
}

/// A node or a pattern, in lower case, or why it is not one.
pub fn check_node(text: &str) -> Result<String, String> {
    let node = text.to_ascii_lowercase();
    let bad = || {
        format!(
            "\"{text}\" is not a permission node (lower-case words joined by dots, \
             `*`, or words ending in `.*`)"
        )
    };
    if node == "*" {
        return Ok(node);
    }
    let words = node.strip_suffix(".*").unwrap_or(&node);
    let word_ok = |w: &str| {
        !w.is_empty()
            && w.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
    };
    if words.split('.').all(word_ok) {
        Ok(node)
    } else {
        Err(bad())
    }
}

/// A group's name: letters, digits, underscores and hyphens.
pub fn check_group_name(name: &str) -> Result<(), String> {
    if !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        Ok(())
    } else {
        Err(format!(
            "\"{name}\" is not a group name (letters, digits, underscores and hyphens)"
        ))
    }
}

/// How well `pattern` matches `node`: how specific it is, or `None` when it does not.
/// More words is more specific, and a node is more specific than a pattern of as many.
fn matches(pattern: &str, node: &str) -> Option<(usize, bool)> {
    if pattern == "*" {
        return Some((0, false));
    }
    match pattern.strip_suffix(".*") {
        Some(prefix) => node
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('.'))
            .then(|| (prefix.split('.').count(), false)),
        None => (pattern == node).then(|| (pattern.split('.').count(), true)),
    }
}

impl Permissions {
    /// The groups, by name.
    pub fn groups(&self) -> &[PermissionGroup] {
        &self.groups
    }

    /// The players with groups.
    pub fn members(&self) -> &[Member] {
        &self.members
    }

    pub fn group(&self, name: &str) -> Option<&PermissionGroup> {
        self.groups.iter().find(|g| g.name == name)
    }

    /// The groups the player is in: those of every entry that is their name or their
    /// unique id, in the order written, each once.
    pub fn groups_of(&self, name: &str, uuid: [u8; 16]) -> Vec<&str> {
        let mut found: Vec<&str> = Vec::new();
        for member in &self.members {
            let is_them = match &member.who {
                Target::Name(n) => n.eq_ignore_ascii_case(name),
                Target::Uuid(u) => *u == uuid,
                Target::Net(_) => false,
            };
            if is_them {
                for group in &member.groups {
                    if !found.contains(&group.as_str()) {
                        found.push(group);
                    }
                }
            }
        }
        found
    }

    /// Whether the player called `name`, with unique id `uuid`, has `node` (lower case,
    /// without a pattern): see the module's description. A node no group says anything of
    /// is refused, unless it is a command that was open before permissions existed
    /// (`meridian.server`, `.hub`, `.play`, and `meridian.server.<name>`).
    pub fn allows(&self, name: &str, uuid: [u8; 16], node: &str) -> bool {
        self.allows_in(name, uuid, node, &[])
    }

    /// As [`Permissions::allows`] for a player who is also a member of the groups `extra`, as
    /// the operators of the servers are of `[operators] group`. The membership is for this
    /// question only: it is not written to the file, and a group that is not defined adds
    /// nothing.
    pub fn allows_in(&self, name: &str, uuid: [u8; 16], node: &str, extra: &[&str]) -> bool {
        let node = node.to_ascii_lowercase();
        let mut seen: Vec<&str> = Vec::new();
        let mut own = self.groups_of(name, uuid);
        for wanted in extra {
            // The group as this table names it, so that the list borrows from the table.
            if let Some(group) = self.group(wanted)
                && !own.contains(&group.name.as_str())
            {
                own.push(&group.name);
            }
        }
        // The player's groups level by level, then `default` level by level.
        for start in [own, vec![DEFAULT_GROUP]] {
            let mut level: Vec<&str> = start.into_iter().filter(|g| !seen.contains(g)).collect();
            while !level.is_empty() {
                seen.extend(level.iter().copied());
                let mut best: Option<((usize, bool), bool)> = None;
                let mut next: Vec<&str> = Vec::new();
                for group in level.iter().filter_map(|g| self.group(g)) {
                    for (patterns, allow) in [(&group.allow, true), (&group.deny, false)] {
                        for pattern in patterns {
                            let Some(score) = matches(pattern, &node) else {
                                continue;
                            };
                            // The most specific wins; a deny wins a tie.
                            if best.is_none_or(|(b, a)| score > b || (score == b && a && !allow)) {
                                best = Some((score, allow));
                            }
                        }
                    }
                    for parent in &group.inherits {
                        if !seen.contains(&parent.as_str()) && !next.contains(&parent.as_str()) {
                            next.push(parent);
                        }
                    }
                }
                if let Some((_, allow)) = best {
                    return allow;
                }
                level = next;
            }
        }
        open_by_default(&node)
    }

    /// Creates the group `name`, empty. `false` when it exists.
    pub fn create_group(&mut self, name: &str) -> Result<bool, String> {
        check_group_name(name)?;
        if self.group(name).is_some() {
            return Ok(false);
        }
        self.groups.push(PermissionGroup {
            name: name.to_owned(),
            ..PermissionGroup::default()
        });
        self.groups.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(true)
    }

    /// Deletes the group `name`, and takes it out of what inherits it and of the players
    /// in it. `false` when there is none.
    pub fn delete_group(&mut self, name: &str) -> bool {
        let before = self.groups.len();
        self.groups.retain(|g| g.name != name);
        for group in &mut self.groups {
            group.inherits.retain(|g| g != name);
        }
        for member in &mut self.members {
            member.groups.retain(|g| g != name);
        }
        self.members.retain(|m| !m.groups.is_empty());
        self.groups.len() != before
    }

    fn group_mut(&mut self, name: &str) -> Result<&mut PermissionGroup, String> {
        self.groups
            .iter_mut()
            .find(|g| g.name == name)
            .ok_or_else(|| format!("there is no group called {name}"))
    }

    /// Sets `node` (or a pattern) in the group: allowed, denied, or not mentioned.
    /// `Ok(false)` when that changes nothing.
    pub fn set(&mut self, group: &str, node: &str, setting: Setting) -> Result<bool, String> {
        let node = check_node(node)?;
        let group = self.group_mut(group)?;
        let (was_allow, was_deny) = (group.allow.contains(&node), group.deny.contains(&node));
        let (want_allow, want_deny) = (setting == Setting::Allow, setting == Setting::Deny);
        if (was_allow, was_deny) == (want_allow, want_deny) {
            return Ok(false);
        }
        group.allow.retain(|n| *n != node);
        group.deny.retain(|n| *n != node);
        match setting {
            Setting::Allow => group.allow.push(node),
            Setting::Deny => group.deny.push(node),
            Setting::Unset => {}
        }
        Ok(true)
    }

    /// Makes `group` inherit `parent`. `Ok(false)` when it already does; an error when
    /// that would make a group inherit itself through others.
    pub fn inherit(&mut self, group: &str, parent: &str) -> Result<bool, String> {
        self.group_mut(parent)?;
        if self.group_mut(group)?.inherits.iter().any(|p| p == parent) {
            return Ok(false);
        }
        if group == parent || self.inherited(parent).contains(&group.to_owned()) {
            return Err(format!(
                "{parent} already inherits {group}, directly or not"
            ));
        }
        self.group_mut(group)?.inherits.push(parent.to_owned());
        Ok(true)
    }

    /// Ends the inheritance of `parent` by `group`. `Ok(false)` when there was none.
    pub fn uninherit(&mut self, group: &str, parent: &str) -> Result<bool, String> {
        let group = self.group_mut(group)?;
        let before = group.inherits.len();
        group.inherits.retain(|p| p != parent);
        Ok(group.inherits.len() != before)
    }

    /// Every group `group` inherits, directly or not.
    fn inherited(&self, group: &str) -> Vec<String> {
        let mut found: Vec<String> = Vec::new();
        let mut todo = vec![group.to_owned()];
        while let Some(next) = todo.pop() {
            for parent in self.group(&next).map_or(&[][..], |g| &g.inherits) {
                if !found.contains(parent) {
                    found.push(parent.clone());
                    todo.push(parent.clone());
                }
            }
        }
        found
    }

    /// Puts the player in `group`. `Ok(false)` when they are.
    pub fn grant(&mut self, who: Target, group: &str) -> Result<bool, String> {
        if matches!(who, Target::Net(_)) {
            return Err("permissions are for player names and unique ids, not addresses".into());
        }
        self.group_mut(group)?;
        let member = match self.members.iter_mut().find(|m| m.who.same(&who)) {
            Some(member) => member,
            None => {
                self.members.push(Member {
                    who,
                    groups: Vec::new(),
                });
                self.members.last_mut().expect("just pushed")
            }
        };
        if member.groups.iter().any(|g| g == group) {
            return Ok(false);
        }
        member.groups.push(group.to_owned());
        Ok(true)
    }

    /// Takes the player out of `group`. `false` when they were not in it.
    pub fn revoke(&mut self, who: &Target, group: &str) -> bool {
        let mut done = false;
        for member in self.members.iter_mut().filter(|m| m.who.same(who)) {
            let before = member.groups.len();
            member.groups.retain(|g| g != group);
            done |= member.groups.len() != before;
        }
        self.members.retain(|m| !m.groups.is_empty());
        done
    }

    /// Reads the lists from TOML text (see the module's example).
    pub fn parse(text: &str) -> Result<Permissions, AccessError> {
        let file: FilePermissions =
            toml::from_str(text).map_err(|e| AccessError::Parse(e.to_string()))?;
        let mut permissions = Permissions::default();
        for (name, group) in file.group {
            let what = format!("[group.{name}]");
            let bad = |why: String| AccessError::entry(&what, why);
            check_group_name(&name).map_err(bad)?;
            let nodes = |list: Vec<String>| -> Result<Vec<String>, AccessError> {
                let mut out: Vec<String> = Vec::new();
                for node in list {
                    let node = check_node(&node).map_err(|why| AccessError::entry(&what, why))?;
                    if !out.contains(&node) {
                        out.push(node);
                    }
                }
                Ok(out)
            };
            let (allow, deny) = (nodes(group.allow)?, nodes(group.deny)?);
            if let Some(both) = allow.iter().find(|n| deny.contains(n)) {
                return Err(AccessError::entry(
                    &what,
                    format!("{both} is both allowed and denied"),
                ));
            }
            permissions.groups.push(PermissionGroup {
                name,
                inherits: group.inherits,
                allow,
                deny,
            });
        }
        for group in &permissions.groups {
            for parent in &group.inherits {
                let what = format!("[group.{}]", group.name);
                if permissions.group(parent).is_none() && parent != DEFAULT_GROUP {
                    return Err(AccessError::entry(
                        &what,
                        format!("inherits {parent}, which is not a group"),
                    ));
                }
                if *parent == group.name || permissions.inherited(parent).contains(&group.name) {
                    return Err(AccessError::entry(
                        &what,
                        format!("inherits {parent}, which inherits it back"),
                    ));
                }
            }
        }
        for (i, entry) in file.player.into_iter().enumerate() {
            let what = format!("[[player]] number {}", i + 1);
            let bad = |why: String| AccessError::entry(&what, why);
            let who = match (entry.name, entry.uuid) {
                (Some(name), None) => check_name(&name)
                    .map(|()| Target::Name(name))
                    .map_err(bad)?,
                (None, Some(uuid)) => parse_uuid(&uuid)
                    .map(Target::Uuid)
                    .ok_or_else(|| bad(format!("\"{uuid}\" is not a unique id")))?,
                _ => return Err(bad("needs exactly one of name and uuid".into())),
            };
            for group in &entry.groups {
                if permissions.group(group).is_none() && group != DEFAULT_GROUP {
                    return Err(AccessError::entry(
                        &what,
                        format!("is in {group}, which is not a group"),
                    ));
                }
            }
            if entry.groups.is_empty() {
                return Err(bad("has no groups".into()));
            }
            for group in entry.groups {
                if permissions.group(&group).is_none() {
                    // `default` is implicit; written down it is a group like another.
                    permissions
                        .create_group(&group)
                        .map_err(|e| AccessError::entry(&what, e))?;
                }
                permissions
                    .grant(who.clone(), &group)
                    .map_err(|e| AccessError::entry(&what, e))?;
            }
        }
        Ok(permissions)
    }

    /// The lists as the TOML [`Permissions::parse`] reads back.
    pub fn to_toml(&self) -> String {
        let list = |nodes: &[String]| {
            let quoted: Vec<String> = nodes.iter().map(|n| quote(n)).collect();
            format!("[{}]", quoted.join(", "))
        };
        let mut out = String::from(
            "# Permissions of the proxy. The console changes this file (perm); it is read again\n\
             # at the start and at `reload`.\n",
        );
        for group in &self.groups {
            out.push_str(&format!("\n[group.{}]\n", group.name));
            if !group.inherits.is_empty() {
                out.push_str(&format!("inherits = {}\n", list(&group.inherits)));
            }
            if !group.allow.is_empty() {
                out.push_str(&format!("allow = {}\n", list(&group.allow)));
            }
            if !group.deny.is_empty() {
                out.push_str(&format!("deny = {}\n", list(&group.deny)));
            }
        }
        for member in &self.members {
            out.push_str("\n[[player]]\n");
            let (key, value) = match &member.who {
                Target::Uuid(_) => ("uuid", member.who.to_string()),
                other => ("name", other.to_string()),
            };
            out.push_str(&format!("{key} = {}\n", quote(&value)));
            out.push_str(&format!("groups = {}\n", list(&member.groups)));
        }
        out
    }

    /// Reads the file at `path`. A file that is not there is no groups and no players.
    pub fn load(path: &Path) -> Result<Permissions, AccessError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Permissions::parse(&text).map_err(|e| e.in_file(path)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Permissions::default()),
            Err(source) => Err(AccessError::Read {
                path: path.to_owned(),
                source,
            }),
        }
    }

    /// Writes the lists to `path`, through a temporary file beside it.
    pub fn save(&self, path: &Path) -> Result<(), AccessError> {
        write_whole(path, &self.to_toml())
    }
}

/// The file as written.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FilePermissions {
    #[serde(default)]
    group: BTreeMap<String, FileGroup>,
    #[serde(default)]
    player: Vec<FilePlayer>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileGroup {
    #[serde(default)]
    inherits: Vec<String>,
    #[serde(default)]
    allow: Vec<String>,
    #[serde(default)]
    deny: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FilePlayer {
    name: Option<String>,
    uuid: Option<String>,
    #[serde(default)]
    groups: Vec<String>,
}

/// The `[permissions]` table: where the groups and the players are kept.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PermissionSettings {
    /// The file. A configuration read from a file has one, `permissions.toml` beside it
    /// unless the table says otherwise; without one (a configuration built in code) the
    /// lists are kept in memory only.
    pub file: Option<PathBuf>,
}

/// The `[permissions]` table as written.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct FilePermissionSettings {
    file: Option<PathBuf>,
}

impl FilePermissionSettings {
    pub(crate) fn into_settings(self, folder: &Path) -> PermissionSettings {
        let file = self
            .file
            .unwrap_or_else(|| PathBuf::from("permissions.toml"));
        PermissionSettings {
            file: Some(sidecar::resolve(folder, &file)),
        }
    }
}
