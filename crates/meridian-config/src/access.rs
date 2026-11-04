//! Who may not join, and who alone may: the bans and the whitelist the proxy applies
//! before any backend is asked (docs/OPERATING.md, "Bans and whitelist").
//!
//! They live in a file of their own, `access.toml` next to the configuration unless
//! `[access] file` says otherwise, because the console changes them while the proxy
//! runs and a configuration written by hand should not be rewritten by it. The file is
//! read at the start and at `reload`, and written whole to a temporary file that then
//! replaces it, so that a crash never leaves half of a list.
//!
//! ```toml
//! whitelist = false      # on or off; off when the file does not say
//!
//! [[ban]]                # one of name, uuid, ip
//! name = "Griefer"
//! reason = "Griefing the spawn"
//!
//! [[ban]]
//! ip = "203.0.113.0/24"  # an address or a range
//!
//! [[allow]]              # one of name, uuid
//! uuid = "069a79f4-44e9-4726-a5be-fca90e38aaf5"
//! ```
//!
//! A name is compared whatever its case. A unique id is the account's when the player
//! was checked, and derived from the name as typed when they were not (offline mode,
//! or a name that belongs to no account in the hybrid mode), so a ban by unique id
//! stops an unverified player only under the spelling it was given.
use super::*;
use std::fmt;
use std::net::IpAddr;

/// The longest player name.
const MAX_NAME: usize = 16;

/// An address range: every address whose first `prefix` bits are those of `network`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Net {
    network: IpAddr,
    prefix: u8,
}

impl Net {
    /// An address (`203.0.113.9`), or a range (`203.0.113.0/24`, `2001:db8::/32`). The
    /// bits after the prefix are cleared, so `10.1.2.3/8` is `10.0.0.0/8`.
    pub fn parse(text: &str) -> Result<Net, String> {
        let (address, prefix) = match text.split_once('/') {
            Some((address, prefix)) => (address, Some(prefix)),
            None => (text, None),
        };
        let address: IpAddr = address
            .parse()
            .map_err(|_| format!("\"{text}\" is not an address or a range"))?;
        let address = address.to_canonical();
        let longest: u8 = if address.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix {
            None => longest,
            Some(p) => p
                .parse::<u8>()
                .ok()
                .filter(|p| *p <= longest)
                .ok_or_else(|| format!("\"{p}\" is not a length from 0 to {longest}"))?,
        };
        Ok(Net {
            network: mask(address, prefix),
            prefix,
        })
    }

    /// Whether `address` is in the range. An IPv4 address written as IPv6
    /// (`::ffff:203.0.113.9`) is the IPv4 one.
    pub fn contains(&self, address: IpAddr) -> bool {
        let address = address.to_canonical();
        address.is_ipv4() == self.network.is_ipv4() && mask(address, self.prefix) == self.network
    }
}

/// `address` with the bits after the first `prefix` cleared.
fn mask(address: IpAddr, prefix: u8) -> IpAddr {
    match address {
        IpAddr::V4(a) => {
            let bits = u32::from(a);
            let keep = u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0);
            IpAddr::V4((bits & keep).into())
        }
        IpAddr::V6(a) => {
            let bits = u128::from(a);
            let keep = u128::MAX.checked_shl(128 - u32::from(prefix)).unwrap_or(0);
            IpAddr::V6((bits & keep).into())
        }
    }
}

impl fmt::Display for Net {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let longest = if self.network.is_ipv4() { 32 } else { 128 };
        if self.prefix == longest {
            write!(f, "{}", self.network)
        } else {
            write!(f, "{}/{}", self.network, self.prefix)
        }
    }
}

/// What a ban or a whitelist entry is about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A player name, whatever its case.
    Name(String),
    /// An account's unique id.
    Uuid([u8; 16]),
    /// An address or a range of them. Not for the whitelist.
    Net(Net),
}

impl Target {
    /// What the console was given: a unique id (32 hexadecimal digits, or hyphenated), an
    /// address or a range, or else a player name (1 to 16 letters, digits and
    /// underscores).
    pub fn parse(text: &str) -> Result<Target, String> {
        if let Some(uuid) = parse_uuid(text) {
            return Ok(Target::Uuid(uuid));
        }
        if text.contains(['.', ':', '/']) {
            return Net::parse(text).map(Target::Net);
        }
        check_name(text)?;
        Ok(Target::Name(text.to_owned()))
    }

    /// Whether both are about the same thing: names compare whatever their case.
    pub fn same(&self, other: &Target) -> bool {
        match (self, other) {
            (Target::Name(a), Target::Name(b)) => a.eq_ignore_ascii_case(b),
            _ => self == other,
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Target::Name(_) => "name",
            Target::Uuid(_) => "uuid",
            Target::Net(_) => "ip",
        }
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Target::Name(name) => f.write_str(name),
            Target::Uuid(uuid) => f.write_str(&hyphenated(*uuid)),
            Target::Net(net) => net.fmt(f),
        }
    }
}

pub(crate) fn check_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > MAX_NAME {
        return Err(format!(
            "\"{name}\" is not a player name (1 to {MAX_NAME} characters)"
        ));
    }
    if !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return Err(format!(
            "\"{name}\" is not a player name, an address or a unique id (names have letters, \
             digits and underscores)"
        ));
    }
    Ok(())
}

/// 32 hexadecimal digits, or 8-4-4-4-12 with hyphens.
pub(crate) fn parse_uuid(text: &str) -> Option<[u8; 16]> {
    let digits: String = if text.len() == 36 {
        let groups: Vec<&str> = text.split('-').collect();
        if groups.iter().map(|g| g.len()).collect::<Vec<_>>() != [8, 4, 4, 4, 12] {
            return None;
        }
        groups.concat()
    } else {
        text.to_owned()
    };
    if digits.len() != 32 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u128::from_str_radix(&digits, 16)
        .ok()
        .map(u128::to_be_bytes)
}

/// A unique id as `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`.
fn hyphenated(uuid: [u8; 16]) -> String {
    let hex = format!("{:032x}", u128::from_be_bytes(uuid));
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

/// A ban.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ban {
    pub target: Target,
    /// What the player is told, after "You are banned from this server."
    pub reason: Option<String>,
}

/// Why a player is not let in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    Banned(Option<String>),
    NotWhitelisted,
}

impl Refusal {
    /// What the player is told.
    pub fn message(&self) -> String {
        match self {
            Refusal::Banned(None) => "You are banned from this server.".to_owned(),
            Refusal::Banned(Some(reason)) => {
                format!("You are banned from this server.\nReason: {reason}")
            }
            Refusal::NotWhitelisted => "You are not whitelisted on this server.".to_owned(),
        }
    }
}

/// The bans and the whitelist.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Access {
    whitelist: bool,
    bans: Vec<Ban>,
    allowed: Vec<Target>,
}

impl Access {
    /// Whether only the players of the whitelist are let in.
    pub fn whitelist_on(&self) -> bool {
        self.whitelist
    }

    pub fn set_whitelist(&mut self, on: bool) {
        self.whitelist = on;
    }

    /// The bans, in the order they were made.
    pub fn bans(&self) -> &[Ban] {
        &self.bans
    }

    /// The whitelist, in the order of the entries.
    pub fn allowed(&self) -> &[Target] {
        &self.allowed
    }

    /// The ban of `address`, by its own entry or a range's.
    pub fn address_ban(&self, address: IpAddr) -> Option<&Ban> {
        self.bans
            .iter()
            .find(|b| matches!(&b.target, Target::Net(net) if net.contains(address)))
    }

    /// The ban that holds the player called `name`, with unique id `uuid`, from
    /// `address`: one of their name, their account or their address.
    pub fn ban_of(&self, name: &str, uuid: [u8; 16], address: IpAddr) -> Option<&Ban> {
        self.bans.iter().find(|b| match &b.target {
            Target::Name(n) => n.eq_ignore_ascii_case(name),
            Target::Uuid(u) => *u == uuid,
            Target::Net(net) => net.contains(address),
        })
    }

    /// Why the player called `name`, with unique id `uuid`, from `address`, may not
    /// join, if they may not: a ban first, then the whitelist when it is on.
    pub fn refusal(&self, name: &str, uuid: [u8; 16], address: IpAddr) -> Option<Refusal> {
        if let Some(ban) = self.ban_of(name, uuid, address) {
            return Some(Refusal::Banned(ban.reason.clone()));
        }
        let listed = self.allowed.iter().any(|t| match t {
            Target::Name(n) => n.eq_ignore_ascii_case(name),
            Target::Uuid(u) => *u == uuid,
            Target::Net(_) => false,
        });
        (self.whitelist && !listed).then_some(Refusal::NotWhitelisted)
    }

    /// Bans `target`. `false` when it already is banned, and nothing changes.
    pub fn ban(&mut self, target: Target, reason: Option<String>) -> bool {
        if self.bans.iter().any(|b| b.target.same(&target)) {
            return false;
        }
        self.bans.push(Ban { target, reason });
        true
    }

    /// Lifts the ban of `target`; `false` when there was none.
    pub fn pardon(&mut self, target: &Target) -> bool {
        let before = self.bans.len();
        self.bans.retain(|b| !b.target.same(target));
        self.bans.len() != before
    }

    /// Adds `target`, a name or a unique id, to the whitelist. `Ok(false)` when it is
    /// there already.
    pub fn allow(&mut self, target: Target) -> Result<bool, String> {
        if matches!(target, Target::Net(_)) {
            return Err("the whitelist takes player names and unique ids, not addresses".into());
        }
        if self.allowed.iter().any(|t| t.same(&target)) {
            return Ok(false);
        }
        self.allowed.push(target);
        Ok(true)
    }

    /// Takes `target` off the whitelist; `false` when it was not there.
    pub fn disallow(&mut self, target: &Target) -> bool {
        let before = self.allowed.len();
        self.allowed.retain(|t| !t.same(target));
        self.allowed.len() != before
    }

    /// Reads the lists from TOML text (see the module's example).
    pub fn parse(text: &str) -> Result<Access, AccessError> {
        let file: FileLists =
            toml::from_str(text).map_err(|e| AccessError::Parse(e.to_string()))?;
        let mut access = Access {
            whitelist: file.whitelist,
            ..Access::default()
        };
        for (i, entry) in file.ban.into_iter().enumerate() {
            let what = format!("[[ban]] number {}", i + 1);
            let target = pick(&what, entry.name, entry.uuid, entry.ip)?;
            if !access.ban(target.clone(), entry.reason) {
                return Err(AccessError::entry(
                    &what,
                    format!("{target} is banned twice"),
                ));
            }
        }
        for (i, entry) in file.allow.into_iter().enumerate() {
            let what = format!("[[allow]] number {}", i + 1);
            let target = pick(&what, entry.name, entry.uuid, entry.ip)?;
            if !access
                .allow(target.clone())
                .map_err(|why| AccessError::entry(&what, why))?
            {
                return Err(AccessError::entry(
                    &what,
                    format!("{target} is listed twice"),
                ));
            }
        }
        Ok(access)
    }

    /// The lists as the TOML [`Access::parse`] reads back.
    pub fn to_toml(&self) -> String {
        let mut out = String::from(
            "# Bans and whitelist of the proxy. The console changes this file (ban, pardon,\n\
             # whitelist); it is read again at the start and at `reload`.\n",
        );
        out.push_str(&format!("whitelist = {}\n", self.whitelist));
        for ban in &self.bans {
            out.push_str("\n[[ban]]\n");
            out.push_str(&entry_line(&ban.target));
            if let Some(reason) = &ban.reason {
                out.push_str(&format!("reason = {}\n", quote(reason)));
            }
        }
        for target in &self.allowed {
            out.push_str("\n[[allow]]\n");
            out.push_str(&entry_line(target));
        }
        out
    }

    /// Reads the file at `path`. A file that is not there is empty lists: nothing is
    /// banned and the whitelist is off.
    pub fn load(path: &Path) -> Result<Access, AccessError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Access::parse(&text).map_err(|e| e.in_file(path)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Access::default()),
            Err(source) => Err(AccessError::Read {
                path: path.to_owned(),
                source,
            }),
        }
    }

    /// Writes the lists to `path`: the whole file goes to a temporary file beside it,
    /// which then replaces it, so a failure leaves the old file.
    pub fn save(&self, path: &Path) -> Result<(), AccessError> {
        write_whole(path, &self.to_toml())
    }
}

/// Writes `text` to a temporary file beside `path`, which then replaces it.
pub(crate) fn write_whole(path: &Path, text: &str) -> Result<(), AccessError> {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    let done = (|| {
        use std::io::Write;
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)
    })();
    if let Err(source) = done {
        let _ = std::fs::remove_file(&temporary);
        return Err(AccessError::Write {
            path: path.to_owned(),
            source,
        });
    }
    Ok(())
}

fn entry_line(target: &Target) -> String {
    format!("{} = {}\n", target.kind(), quote(&target.to_string()))
}

/// `text` as a TOML basic string.
pub(crate) fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The file as written.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileLists {
    #[serde(default)]
    whitelist: bool,
    #[serde(default)]
    ban: Vec<FileBan>,
    #[serde(default)]
    allow: Vec<FileAllow>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileBan {
    name: Option<String>,
    uuid: Option<String>,
    ip: Option<String>,
    reason: Option<String>,
}

/// An entry of the whitelist. `ip` is read only to say that it does not belong there.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileAllow {
    name: Option<String>,
    uuid: Option<String>,
    ip: Option<String>,
}

/// The one of `name`, `uuid` and `ip` an entry has.
fn pick(
    what: &str,
    name: Option<String>,
    uuid: Option<String>,
    ip: Option<String>,
) -> Result<Target, AccessError> {
    let bad = |why: String| AccessError::entry(what, why);
    match (name, uuid, ip) {
        (Some(name), None, None) => check_name(&name).map(|()| Target::Name(name)).map_err(bad),
        (None, Some(uuid), None) => parse_uuid(&uuid)
            .map(Target::Uuid)
            .ok_or_else(|| bad(format!("\"{uuid}\" is not a unique id"))),
        (None, None, Some(ip)) => Net::parse(&ip).map(Target::Net).map_err(bad),
        _ => Err(bad("needs exactly one of name, uuid and ip".into())),
    }
}

/// Why the bans and the whitelist could not be read or written.
#[derive(Debug)]
pub enum AccessError {
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    /// Not valid TOML, or a key of the wrong type or an unknown one.
    Parse(String),
    /// An entry that makes no sense.
    Entry { what: String, why: String },
}

impl AccessError {
    pub(crate) fn entry(what: &str, why: String) -> Self {
        AccessError::Entry {
            what: what.to_owned(),
            why,
        }
    }

    /// The same, with the file named in the message.
    pub(crate) fn in_file(self, path: &Path) -> Self {
        let named = |text: String| format!("{}: {text}", path.display());
        match self {
            AccessError::Parse(text) => AccessError::Parse(named(text)),
            AccessError::Entry { what, why } => AccessError::Entry {
                what: named(what),
                why,
            },
            other => other,
        }
    }
}

impl fmt::Display for AccessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AccessError::Read { path, source } => {
                write!(f, "cannot read {}: {source}", path.display())
            }
            AccessError::Write { path, source } => {
                write!(f, "cannot write {}: {source}", path.display())
            }
            AccessError::Parse(text) => f.write_str(text),
            AccessError::Entry { what, why } => write!(f, "{what}: {why}"),
        }
    }
}

impl std::error::Error for AccessError {}

/// The `[access]` table: where the bans and the whitelist are kept.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccessSettings {
    /// The file. A configuration read from a file has one, `access.toml` beside it unless
    /// the table says otherwise; without one (a configuration built in code) the lists
    /// are kept in memory only.
    pub file: Option<PathBuf>,
}

/// The `[access]` table as written.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct FileAccess {
    file: Option<PathBuf>,
}

impl FileAccess {
    pub(crate) fn into_settings(self, folder: &Path) -> AccessSettings {
        let file = self.file.unwrap_or_else(|| PathBuf::from("access.toml"));
        AccessSettings {
            file: Some(sidecar::resolve(folder, &file)),
        }
    }
}
