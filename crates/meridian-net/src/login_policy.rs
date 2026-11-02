//! Whether a player's account is checked at login (DESIGN.md section 15.2).
//!
//! In the online mode every player is checked and becomes *verified*; in the
//! offline mode nobody is, and every player is *unverified*. The hybrid mode
//! decides from the name alone, before anything is asked of the client: a name
//! that belongs to an account is checked like in the online mode, so nobody can
//! take it without the account, and a name that belongs to no account is let in
//! unverified. Names are looked up with the account service's public profile
//! lookup, remembered for a while, and one address can only cause so many
//! lookups a minute.
use crate::client::AuthKind;
use meridian_config::{AuthMode, AuthSettings, Config, OnLookupFailure};
use std::collections::HashMap;
use std::fmt;
use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The account service could not say whether a name is an account's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LookupError;

impl fmt::Display for LookupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the account service did not answer")
    }
}

impl std::error::Error for LookupError {}

/// Tells whether a name belongs to an account.
pub trait NameLookup: Send + Sync {
    fn is_known_account(&self, name: &str) -> Result<bool, LookupError>;
}

/// The account service's public lookup of a profile by name, over HTTPS.
pub struct ProfileLookup;

impl ProfileLookup {
    /// The lookup endpoint; the name follows the last slash.
    const URL: &'static str = "https://api.mojang.com/users/profiles/minecraft/";
    /// A login waits for the answer, so the wait is short.
    const TIMEOUT: Duration = Duration::from_secs(3);

    /// What an answer's status means: 200 is an account, 204 and 404 are no
    /// account, and anything else (too many requests, a server error) is no
    /// answer.
    fn read_status(status: u16) -> Result<bool, LookupError> {
        match status {
            200 => Ok(true),
            204 | 404 => Ok(false),
            _ => Err(LookupError),
        }
    }
}

impl NameLookup for ProfileLookup {
    fn is_known_account(&self, name: &str) -> Result<bool, LookupError> {
        // The login already refused names with other characters; this keeps the
        // URL safe whatever calls it.
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return Ok(false);
        }
        let agent = ureq::AgentBuilder::new()
            .timeout(Self::TIMEOUT)
            .user_agent(concat!("Meridian/", env!("CARGO_PKG_VERSION")))
            .build();
        match agent.get(&format!("{}{name}", Self::URL)).call() {
            Ok(response) => Self::read_status(response.status()),
            Err(ureq::Error::Status(status, _)) => Self::read_status(status),
            Err(ureq::Error::Transport(_)) => Err(LookupError),
        }
    }
}

/// A lookup with a fixed answer, for tests: it never touches the network.
pub struct FakeLookup {
    /// The account names, in lower case.
    accounts: Vec<String>,
    /// Whether every lookup fails.
    down: bool,
    calls: AtomicUsize,
}

impl FakeLookup {
    /// The service knows these names, whatever their case.
    pub fn with_accounts(accounts: &[&str]) -> Self {
        Self {
            accounts: accounts.iter().map(|a| a.to_ascii_lowercase()).collect(),
            down: false,
            calls: AtomicUsize::new(0),
        }
    }

    /// The service never answers.
    pub fn unavailable() -> Self {
        Self {
            accounts: Vec::new(),
            down: true,
            calls: AtomicUsize::new(0),
        }
    }

    /// How many lookups reached it.
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }
}

impl NameLookup for FakeLookup {
    fn is_known_account(&self, name: &str) -> Result<bool, LookupError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if self.down {
            return Err(LookupError);
        }
        let name = name.to_ascii_lowercase();
        Ok(self.accounts.contains(&name))
    }
}

/// Why a player is refused before the login goes on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The account service did not answer and `on-lookup-failure` is `refuse`.
    LookupFailed,
    /// The player's address caused too many lookups this minute.
    TooManyLookups,
}

impl Refusal {
    /// The reason sent in the Disconnect packet, as a chat component in JSON.
    pub fn reason_json(self) -> String {
        match self {
            // Vanilla's own words when the session service is down.
            Refusal::LookupFailed => {
                serde_json::json!({ "translate": "multiplayer.disconnect.authservers_down" })
                    .to_string()
            }
            Refusal::TooManyLookups => serde_json::json!({ "text": self.to_string() }).to_string(),
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::LookupFailed => f.write_str("the account service did not answer"),
            Refusal::TooManyLookups => {
                f.write_str("Too many logins from your address. Try again in a minute.")
            }
        }
    }
}

impl std::error::Error for Refusal {}

/// Above this many entries, forgotten names and old windows are swept out.
const SWEEP_ABOVE: usize = 10_000;
/// The window `lookups-per-minute` counts in.
const WINDOW: Duration = Duration::from_secs(60);

/// The memory of the hybrid mode: names already looked up, and the lookups each
/// address caused in the current minute. The proxy keeps one for its life.
#[derive(Default)]
pub struct LoginPolicy {
    /// Name in lower case → whether it is an account's, and when that was learned.
    names: Mutex<HashMap<String, (bool, Instant)>>,
    /// Address (`None` when it is not known) → start of its window and the
    /// lookups made in it.
    addresses: Mutex<HashMap<Option<IpAddr>, (Instant, u32)>>,
}

impl LoginPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    /// The policy the proxy uses for every login.
    pub fn global() -> &'static LoginPolicy {
        static POLICY: OnceLock<LoginPolicy> = OnceLock::new();
        POLICY.get_or_init(LoginPolicy::new)
    }

    /// How the player called `name`, connecting from `peer`, logs in. `peer` is
    /// `None` when the caller does not know it; such players share one limit.
    pub fn auth_for(
        &self,
        config: &Config,
        name: &str,
        lookup: &dyn NameLookup,
        peer: Option<IpAddr>,
    ) -> Result<AuthKind, Refusal> {
        self.auth_for_at(config, name, lookup, peer, Instant::now())
    }

    fn auth_for_at(
        &self,
        config: &Config,
        name: &str,
        lookup: &dyn NameLookup,
        peer: Option<IpAddr>,
        now: Instant,
    ) -> Result<AuthKind, Refusal> {
        match config.auth_mode {
            AuthMode::Online => Ok(AuthKind::Premium),
            AuthMode::Offline => Ok(AuthKind::Offline),
            AuthMode::Hybrid => {
                let known = self.is_known_account(&config.auth, name, lookup, peer, now)?;
                Ok(if known {
                    AuthKind::Premium
                } else {
                    AuthKind::Offline
                })
            }
        }
    }

    /// Whether `name` is an account's: remembered, or looked up within the
    /// address's limit. A failed lookup is not remembered.
    fn is_known_account(
        &self,
        settings: &AuthSettings,
        name: &str,
        lookup: &dyn NameLookup,
        peer: Option<IpAddr>,
        now: Instant,
    ) -> Result<bool, Refusal> {
        let key = name.to_ascii_lowercase();
        if let Some(known) = self.remembered(settings, &key, now) {
            return Ok(known);
        }
        self.count_lookup(settings, peer, now)?;
        match lookup.is_known_account(name) {
            Ok(known) => {
                self.remember(settings, key, known, now);
                Ok(known)
            }
            Err(LookupError) => match settings.on_lookup_failure {
                OnLookupFailure::Refuse => Err(Refusal::LookupFailed),
                OnLookupFailure::Unverified => Ok(false),
            },
        }
    }

    fn remembered(&self, settings: &AuthSettings, key: &str, now: Instant) -> Option<bool> {
        let names = self.names.lock().unwrap_or_else(|e| e.into_inner());
        let &(known, at) = names.get(key)?;
        (now.saturating_duration_since(at) < keep_for(settings, known)).then_some(known)
    }

    fn remember(&self, settings: &AuthSettings, key: String, known: bool, now: Instant) {
        if keep_for(settings, known).is_zero() {
            return;
        }
        let mut names = self.names.lock().unwrap_or_else(|e| e.into_inner());
        if names.len() >= SWEEP_ABOVE {
            names.retain(|_, &mut (known, at)| {
                now.saturating_duration_since(at) < keep_for(settings, known)
            });
        }
        names.insert(key, (known, now));
    }

    /// Counts one lookup for `peer`, or refuses it when the address has used its
    /// lookups for this minute.
    fn count_lookup(
        &self,
        settings: &AuthSettings,
        peer: Option<IpAddr>,
        now: Instant,
    ) -> Result<(), Refusal> {
        let mut addresses = self.addresses.lock().unwrap_or_else(|e| e.into_inner());
        if addresses.len() >= SWEEP_ABOVE {
            addresses.retain(|_, &mut (start, _)| now.saturating_duration_since(start) < WINDOW);
        }
        let (start, count) = addresses.entry(peer).or_insert((now, 0));
        if now.saturating_duration_since(*start) >= WINDOW {
            *start = now;
            *count = 0;
        }
        if *count >= settings.lookups_per_minute {
            return Err(Refusal::TooManyLookups);
        }
        *count += 1;
        Ok(())
    }
}

/// How long an answer is remembered.
fn keep_for(settings: &AuthSettings, known: bool) -> Duration {
    Duration::from_secs(if known {
        settings.lookup_cache_seconds
    } else {
        settings.negative_cache_seconds
    })
}

/// How the player called `name`, connecting from `peer`, logs in, with the
/// proxy's [`LoginPolicy::global`]: online gives a verified login, offline an
/// unverified one, and hybrid looks the name up.
pub fn auth_for(
    config: &Config,
    name: &str,
    lookup: &dyn NameLookup,
    peer: IpAddr,
) -> Result<AuthKind, Refusal> {
    LoginPolicy::global().auth_for(config, name, lookup, Some(peer))
}

#[cfg(test)]
mod tests;
