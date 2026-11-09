//! One player's session: the login to the proxy, the backend chosen for them, the
//! login there with the player information forwarding, then the relay until either
//! side leaves. [`Sessions`] keeps every session being relayed: who the player
//! is, where they are, and the handle that ends the session.
use crate::balance::Balancer;
use crate::commands;
use crate::extension::login;
use crate::health::Health;
use crate::log::{debug, info, warning};
use crate::metrics::{LoginFailure, Metrics};
use crate::queue;
use crate::route::{self, Full};
use crate::status::Statuses;
use crate::switching::{self, Switcher};
use horizon_protocol::{connection::Connection, packets::configuration, text};
use meridian_config::{Config, OnDuplicate, Refusal};
use meridian_handoff::{Session, SessionHandle as Handle};
use meridian_net::{
    Profile,
    backend::{BackendLink, BackendOptions},
    client::{self, ClientSession},
};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

/// How the proxy reaches a backend: [`meridian_net::backend::connect`] when it
/// runs, a pipe to a fake backend in tests.
pub type Connect =
    dyn Fn(&str, &Profile, &BackendOptions) -> meridian_net::Result<BackendLink> + Send + Sync;

/// How long a refused player is given to read the reason and close, before the
/// proxy closes the connection itself.
const REFUSAL_GRACE: Duration = Duration::from_secs(2);

/// A player whose session is being relayed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Player {
    pub name: String,
    pub uuid: [u8; 16],
    /// The name of the backend they are on.
    pub backend: String,
    /// Where they connected from.
    pub address: SocketAddr,
    /// When the relay started.
    pub joined: Instant,
}

/// The sessions being relayed, and the health of the backends players are sent to.
#[derive(Default)]
pub struct Sessions {
    /// Which backends recently failed to take a player.
    pub health: Arc<Health>,
    next: AtomicU64,
    closing: AtomicBool,
    open: Arc<Mutex<HashMap<u64, Entry>>>,
    /// The players let in, from the check of the player limit to the end of
    /// their session, including those still being sent to a backend.
    admitted: AtomicUsize,
    /// What the switches of players to other backends share.
    pub switcher: Arc<Switcher>,
    /// Where logins are placed when balancing is on: the closed servers and the
    /// places reserved for players still being sent to their server.
    pub balancer: Arc<Balancer>,
    /// What the backends last reported of themselves, read by the balancer.
    pub statuses: Arc<Statuses>,
    /// The bans and the whitelist.
    pub access: crate::access::Lists,
    /// The groups of permissions and the players in them.
    pub permissions: Arc<crate::permissions::Lists>,
    /// The operators of the servers, as the servers listed them (`[operators]`).
    pub operators: Arc<crate::operators::Operators>,
    /// The players who wait for a place.
    pub queue: crate::queue::LoginQueue,
    /// The extensions the proxy keeps a connection to, which decide logins (docs/EXTENSIONS.md).
    pub extensions: Arc<crate::extension::Registry>,
    /// The configuration in force, which a reload replaces: set by the proxy that owns these
    /// sessions, for what a session decides long after its login (the fallback).
    live: OnceLock<Arc<RwLock<Arc<Config>>>>,
}

/// What placement reads, held apart from [`Sessions`] so that a command typed in
/// game, which outlives the login that built it, can place its player: the sessions
/// that are counted, the health of the backends, the balancer's state and what the
/// backends reported. Every handle is shared with the [`Sessions`] it came from.
#[derive(Clone)]
pub struct Shared {
    open: Arc<Mutex<HashMap<u64, Entry>>>,
    pub health: Arc<Health>,
    pub balancer: Arc<Balancer>,
    pub statuses: Arc<Statuses>,
    pub metrics: Arc<Metrics>,
    /// What a player is allowed to type in game.
    pub permissions: Arc<crate::permissions::Lists>,
    /// The operators of the servers, who are members of a group of the permissions.
    pub operators: Arc<crate::operators::Operators>,
}

impl Shared {
    /// The players being relayed, in the order they joined.
    pub fn players(&self) -> Vec<Player> {
        let mut players: Vec<Player> = self
            .open
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .map(Entry::player)
            .collect();
        players.sort_by_key(|p| p.joined);
        players
    }

    /// The sessions being relayed, with the players, in the order they joined.
    pub fn sessions(&self) -> Vec<(Player, Handle)> {
        let mut sessions: Vec<(Player, Handle)> = self
            .open
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .map(|e| (e.player(), e.handle.clone()))
            .collect();
        sessions.sort_by_key(|(p, _)| p.joined);
        sessions
    }

    /// The player called `name`, whatever its case, and the handle of their session.
    pub fn session(&self, name: &str) -> Option<(Player, Handle)> {
        self.open
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .find(|e| e.player.name.eq_ignore_ascii_case(name))
            .map(|e| (e.player(), e.handle.clone()))
    }

    /// How many players are on each backend now; see [`Sessions::counts`].
    pub fn counts(&self) -> HashMap<String, usize> {
        let backends: Vec<String> = self
            .open
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .map(|e| e.handle.backend())
            .collect();
        crate::balance::count_by_backend(backends.iter().map(String::as_str))
    }
}

/// Who a player is, for telling a second login of theirs: the unique id of a
/// verified account, or the name of an unverified player whatever its case, since
/// their unique id is derived from the name as typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Identity {
    Verified([u8; 16]),
    Unverified(String),
}

impl Identity {
    pub(crate) fn of(session: &ClientSession) -> Self {
        match session.auth {
            client::AuthKind::Premium => Identity::Verified(session.profile.uuid),
            client::AuthKind::Offline => {
                Identity::Unverified(session.profile.name.to_ascii_lowercase())
            }
        }
    }
}

/// What a second login is told when it is refused.
pub(crate) const ALREADY_CONNECTED: &str = "You are already connected to this proxy";

/// A session in [`Sessions`]. `identity` is `None` for a session opened without
/// one, which no login is checked against.
struct Entry {
    player: Player,
    handle: Handle,
    identity: Option<Identity>,
    /// Why the proxy ended this session, set just before it disconnects the player: what the event `players/left` says. A
    /// session that the player or the connection ended has none.
    ended: Option<EndReason>,
}

/// Why the proxy itself ended a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EndReason {
    /// An operator, the console or an extension kicked the player (a ban ends sessions the same way).
    Kicked,
    /// The same player logged in again and the first session was replaced.
    Replaced,
    /// The proxy was stopping.
    Shutdown,
}

impl Entry {
    /// The player, on the backend their session relays to now: a switch changes
    /// it.
    fn player(&self) -> Player {
        Player {
            backend: self.handle.backend(),
            ..self.player.clone()
        }
    }
}

/// Why [`Sessions::open_as`] did not count a session in.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum NotOpened {
    /// The proxy is stopping.
    Stopping,
    /// The same player has a session and duplicates are refused.
    Duplicate,
}

/// A place taken under the player limit, given back when it is dropped.
pub(crate) struct Admission<'a>(&'a AtomicUsize, &'a crate::queue::LoginQueue);

impl Drop for Admission<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
        // The next player in the line may have a place now.
        self.1.wake();
    }
}

#[cfg(test)]
impl Sessions {
    /// Sessions whose switches go through `switcher`, for tests with fake sidecars.
    pub fn with_switcher(switcher: Arc<Switcher>) -> Self {
        Self {
            switcher,
            ..Self::default()
        }
    }
}

impl Sessions {
    /// Takes a place for one more player under `max_players` (0: no limit). The
    /// check and the count are one step, so two players logging in at once cannot
    /// both take the last place.
    pub(crate) fn admit(&self, max_players: u32) -> Result<Admission<'_>, Full> {
        let mut now = self.admitted.load(Ordering::Relaxed);
        loop {
            route::admit(max_players, now)?;
            match self.admitted.compare_exchange_weak(
                now,
                now + 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Ok(Admission(&self.admitted, &self.queue)),
                // Another login took or gave back a place: check again.
                Err(actual) => now = actual,
            }
        }
    }

    /// Whether the proxy has begun to close: new sessions are refused.
    pub(crate) fn closing(&self) -> bool {
        self.closing.load(Ordering::Relaxed)
    }

    /// How many players hold a place under the limit.
    #[cfg(test)]
    pub fn admitted(&self) -> usize {
        self.admitted.load(Ordering::Relaxed)
    }

    /// Counts a session in, with no identity to check a second login against.
    /// `None` once the proxy is stopping: the session must end at once. Logins go
    /// through [`Sessions::open_as`]; this is for tests that need a session.
    #[cfg(test)]
    pub(crate) fn open(&self, player: Player, handle: Handle) -> Option<Open<'_>> {
        let entry = Entry {
            player,
            handle,
            identity: None,
            ended: None,
        };
        self.insert(entry, OnDuplicate::Replace).ok()
    }

    /// Counts in the session of the player `identity`, unless the same player
    /// already has one: then `on_duplicate` refuses the new session, or ends the
    /// old one and counts the new one in. The check and the count are one step,
    /// so two logins of one player at once cannot both be counted in.
    pub(crate) fn open_as(
        &self,
        player: Player,
        handle: Handle,
        identity: Identity,
        on_duplicate: OnDuplicate,
    ) -> Result<Open<'_>, NotOpened> {
        let entry = Entry {
            player,
            handle,
            identity: Some(identity),
            ended: None,
        };
        self.insert(entry, on_duplicate)
    }

    fn insert(&self, entry: Entry, on_duplicate: OnDuplicate) -> Result<Open<'_>, NotOpened> {
        let mut open = self.lock();
        // Checked under the lock, so a stop cannot miss a session opening now.
        if self.closing.load(Ordering::Relaxed) {
            return Err(NotOpened::Stopping);
        }
        if let Some(identity) = &entry.identity {
            let same: Vec<&mut Entry> = open
                .values_mut()
                .filter(|e| e.identity.as_ref() == Some(identity))
                .collect();
            if !same.is_empty() {
                match on_duplicate {
                    OnDuplicate::Refuse => return Err(NotOpened::Duplicate),
                    OnDuplicate::Replace => {
                        for old in same {
                            old.ended.get_or_insert(EndReason::Replaced);
                            old.handle.disconnect(duplicate_login());
                        }
                    }
                }
            }
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        open.insert(id, entry);
        Ok(Open { sessions: self, id })
    }

    /// Whether the player `identity` has a session.
    pub(crate) fn is_connected(&self, identity: &Identity) -> bool {
        self.lock()
            .values()
            .any(|e| e.identity.as_ref() == Some(identity))
    }

    /// Ends every session of the player `identity`, telling them they logged in
    /// from another location. Returns how many were ended.
    pub(crate) fn replace(&self, identity: &Identity) -> usize {
        let mut open = self.lock();
        let same: Vec<&mut Entry> = open
            .values_mut()
            .filter(|e| e.identity.as_ref() == Some(identity))
            .collect();
        for old in &same {
            old.handle.disconnect(duplicate_login());
        }
        let count = same.len();
        for old in same {
            old.ended.get_or_insert(EndReason::Replaced);
        }
        count
    }

    /// The players being relayed, in the order they joined.
    pub fn players(&self) -> Vec<Player> {
        let mut players: Vec<Player> = self.lock().values().map(Entry::player).collect();
        players.sort_by_key(|p| p.joined);
        players
    }

    /// The players being relayed, in the order they joined, each with whether the session service confirmed their account.
    pub fn listing(&self) -> Vec<(Player, bool)> {
        let mut players: Vec<(Player, bool)> = self
            .lock()
            .values()
            .map(|e| {
                (
                    e.player(),
                    matches!(e.identity, Some(Identity::Verified(_))),
                )
            })
            .collect();
        players.sort_by_key(|(p, _)| p.joined);
        players
    }

    /// How many players are on each backend now; a switch moves a player to the
    /// backend they are on after it. A backend with none is absent.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn counts(&self) -> std::collections::HashMap<String, usize> {
        self.shared().counts()
    }

    /// What placement reads, for a command that outlives this borrow.
    /// Makes `live` the configuration that [`Sessions::current_config`] reads.
    pub fn follow_config(&self, live: Arc<RwLock<Arc<Config>>>) {
        let _ = self.live.set(live);
    }

    /// The configuration in force now, or `login`'s when the sessions follow none (tests).
    pub fn current_config(&self, login: &Config) -> Arc<Config> {
        match self.live.get() {
            Some(live) => Arc::clone(&live.read().unwrap_or_else(|e| e.into_inner())),
            None => Arc::new(login.clone()),
        }
    }

    pub fn shared(&self) -> Shared {
        Shared {
            open: Arc::clone(&self.open),
            health: Arc::clone(&self.health),
            balancer: Arc::clone(&self.balancer),
            statuses: Arc::clone(&self.statuses),
            metrics: Arc::clone(self.metrics()),
            permissions: Arc::clone(&self.permissions),
            operators: Arc::clone(&self.operators),
        }
    }

    /// What the proxy counts, which the switcher holds for everything that moves players.
    pub fn metrics(&self) -> &Arc<Metrics> {
        &self.switcher.metrics
    }

    /// The player called `name`, whatever its case, and the handle of their
    /// session.
    pub fn session(&self, name: &str) -> Option<(Player, Handle)> {
        self.lock()
            .values()
            .find(|e| e.player.name.eq_ignore_ascii_case(name))
            .map(|e| (e.player(), e.handle.clone()))
    }

    /// How many sessions are being relayed.
    #[cfg(test)]
    pub fn count(&self) -> usize {
        self.lock().len()
    }

    /// The player called `name`, whatever its case.
    pub fn find(&self, name: &str) -> Option<Player> {
        self.session(name).map(|(player, _)| player)
    }

    /// Ends the session of the player called `name`, telling them `reason`.
    /// Returns the player, or `None` if no one has that name.
    pub fn kick(&self, name: &str, reason: &str) -> Option<Player> {
        let (player, handle) = {
            let mut open = self.lock();
            let entry = open
                .values_mut()
                .find(|e| e.player.name.eq_ignore_ascii_case(name))?;
            entry.ended.get_or_insert(EndReason::Kicked);
            (entry.player(), entry.handle.clone())
        };
        handle.disconnect(configuration::Disconnect {
            reason: text::literal(reason),
        });
        Some(player)
    }

    /// Ends the session of the one player for whom `is` holds, telling them `reason`. **Never the first of several**: if `is` holds
    /// for two accounts (two names that differ only by case) nobody is kicked ([`Kicked::Several`]), and nobody is kicked either
    /// when `protected` says the player is (it is asked after the lock is let go, and is told how their account was checked). The
    /// sessions of one account (a second login that replaces the first is still listed until it ends) are all ended.
    pub(crate) fn kick_one(
        &self,
        reason: &str,
        is: impl Fn(&Player) -> bool,
        protected: impl Fn(&Player, meridian_config::AuthKind) -> bool,
    ) -> Kicked {
        let found: Vec<(Player, Handle, bool, u64)> = self
            .lock()
            .iter()
            .filter(|(_, e)| is(&e.player))
            .map(|(id, e)| {
                (
                    e.player(),
                    e.handle.clone(),
                    matches!(e.identity, Some(Identity::Verified(_))),
                    *id,
                )
            })
            .collect();
        let Some((player, _, verified, _)) = found.first() else {
            return Kicked::Nobody;
        };
        if found
            .iter()
            .any(|(other, _, _, _)| other.uuid != player.uuid)
        {
            return Kicked::Several;
        }
        let auth = if *verified {
            meridian_config::AuthKind::Verified
        } else {
            meridian_config::AuthKind::Unverified
        };
        if protected(player, auth) {
            return Kicked::Protected;
        }
        {
            let mut open = self.lock();
            for (_, _, _, id) in &found {
                if let Some(entry) = open.get_mut(id) {
                    entry.ended.get_or_insert(EndReason::Kicked);
                }
            }
        }
        for (_, handle, _, _) in &found {
            handle.disconnect(configuration::Disconnect {
                reason: text::literal(reason),
            });
        }
        Kicked::Done(player.clone())
    }

    /// Ends every session, telling each player that the server closed, and refuses
    /// new ones. Returns how many sessions were ended.
    pub fn close_all(&self) -> usize {
        let mut open = self.lock();
        self.closing.store(true, Ordering::Relaxed);
        // Players who wait for a place are told as well.
        self.queue.wake();
        for entry in open.values_mut() {
            entry.ended.get_or_insert(EndReason::Shutdown);
            // `multiplayer.disconnect.server_shutdown`: the "Server closed" of the
            // player's own language.
            entry.handle.disconnect(configuration::Disconnect {
                reason: text::translatable("multiplayer.disconnect.server_shutdown", &[]),
            });
        }
        open.len()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Entry>> {
        self.open.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// What [`Sessions::kick_one`] did.
#[derive(Debug)]
pub(crate) enum Kicked {
    /// Nobody online is that.
    Nobody,
    /// More than one account matched: nobody was kicked.
    Several,
    /// The player may not be kicked this way: nobody was kicked.
    Protected,
    /// This player's session was ended.
    Done(Player),
}

/// A session counted in [`Sessions`] until it is dropped.
pub(crate) struct Open<'a> {
    sessions: &'a Sessions,
    id: u64,
}

impl Open<'_> {
    /// Why the proxy ended this session, if it did: the session is still listed. Read it, drop the `Open`, and only then tell anyone,
    /// so that whoever is told finds the player gone from the list.
    pub(crate) fn ended(&self) -> Option<EndReason> {
        self.sessions
            .lock()
            .get(&self.id)
            .and_then(|entry| entry.ended)
    }
}

impl Drop for Open<'_> {
    fn drop(&mut self) {
        self.sessions.lock().remove(&self.id);
    }
}

/// What the session a second login replaces is told: vanilla's "You logged in
/// from another location".
fn duplicate_login() -> configuration::Disconnect {
    configuration::Disconnect {
        reason: text::translatable("multiplayer.disconnect.duplicate_login", &[]),
    }
}

/// One player's connection, from the handshake to the end of the relay. `pending` is
/// its place among the connections logging in, given back once the login is over.
pub fn handle(
    connection: Connection,
    peer: SocketAddr,
    config: &Config,
    connect: &Connect,
    sessions: &Sessions,
    pending: Option<crate::throttle::Pending>,
) {
    // The address lets the hybrid mode limit name lookups per address.
    let accepted = client::accept_from(connection, config, peer.ip());
    drop(pending);
    match accepted {
        Ok(Some(session)) => serve(session, peer, config, connect, sessions),
        Ok(None) => debug!("{peer}: status answered, or login refused"),
        Err(e) => {
            sessions.metrics().login_failed(LoginFailure::Protocol);
            warning!("{peer}: {e}; closing the connection");
        }
    }
}

/// The kind of player the routes know, from how the login was checked.
pub fn route_auth(auth: client::AuthKind) -> meridian_config::AuthKind {
    match auth {
        client::AuthKind::Premium => meridian_config::AuthKind::Verified,
        client::AuthKind::Offline => meridian_config::AuthKind::Unverified,
    }
}

/// A player who has logged in to the proxy: a place under the player limit, the
/// first backend that answers among the ones the routes allow, then the relay.
pub(crate) fn serve(
    session: ClientSession,
    peer: SocketAddr,
    config: &Config,
    connect: &Connect,
    sessions: &Sessions,
) {
    let name = session.profile.name.clone();
    let uuid = session.profile.uuid;
    // A ban or the whitelist is settled before anything else: a refused player costs no
    // place, and no backend is asked for them.
    if let Some(refusal) = sessions.access.refusal(&name, uuid, peer.ip()) {
        let (why, failure) = match &refusal {
            Refusal::Banned(_) => ("banned", LoginFailure::Banned),
            Refusal::NotWhitelisted => ("not whitelisted", LoginFailure::NotWhitelisted),
        };
        info!("{peer}: {name} ({}) refused: {why}", hyphenated(uuid));
        sessions.metrics().login_failed(failure);
        refuse(session.connection, &refusal.message());
        return;
    }
    // The extension that decides logins comes after the bans and the whitelist, which are local, instant
    // and the operator's own: a ban holds whatever an extension says, and a banned player costs no
    // request. It comes before the second login, so that a login it refuses does not end the session
    // that login would have replaced, and before the queue, so that a refused player never waits. It
    // can only add a refusal (docs/EXTENSIONS.md, 4.2).
    let identity = Identity::of(&session);
    let who = login::Who {
        peer,
        name: &name,
        uuid: hyphenated(uuid),
        verified: session.auth == client::AuthKind::Premium,
        host: session.virtual_host.as_deref(),
        connected: sessions.is_connected(&identity),
    };
    if let login::Outcome::Refuse { by, message } = login::admit(&sessions.extensions, config, &who)
    {
        info!(
            "{peer}: {name} ({}) refused by the extension \"{by}\"",
            hyphenated(uuid)
        );
        sessions.metrics().login_failed(LoginFailure::Extension);
        refuse(session.connection, &message);
        return;
    }
    // A second login of a connected player is settled before anything else, so
    // a refused one costs no place and no backend login.
    if sessions.is_connected(&identity) {
        match config.on_duplicate {
            OnDuplicate::Refuse => {
                info!(
                    "{peer}: {name} ({}) refused: already connected",
                    hyphenated(uuid)
                );
                sessions.metrics().login_failed(LoginFailure::Duplicate);
                refuse(session.connection, ALREADY_CONNECTED);
                return;
            }
            OnDuplicate::Replace => {
                sessions.replace(&identity);
                info!(
                    "{peer}: {name} ({}) logged in again; their other session ends",
                    hyphenated(uuid)
                );
            }
        }
    }
    // The extensions that listen are told when the player enters the line and leaves it, if they wait.
    let line = queue::Line::new(&sessions.queue, sessions.metrics(), &config.queue, &name).telling(
        &sessions.extensions,
        uuid,
        session.auth == client::AuthKind::Premium,
    );
    place(session, peer, config, connect, sessions, identity, line);
}

/// A player who passed the ban list and the check for a second login: a place under the
/// player limit (after a wait in the queue when there is none and the queue is on), the
/// first backend that answers among the ones the routes allow, then the relay.
fn place(
    mut session: ClientSession,
    peer: SocketAddr,
    config: &Config,
    connect: &Connect,
    sessions: &Sessions,
    identity: Identity,
    mut line: queue::Line<'_>,
) {
    let name = session.profile.name.clone();
    let uuid = session.profile.uuid;
    // The limit is read at each try: a reload while the player waits changes it.
    let max_players = || sessions.queue.max_players(config.max_players);
    let stopping = || sessions.closing();
    let taken = line.take(
        || sessions.admit(max_players()).ok(),
        &mut queue::Who {
            connection: &mut session.connection,
            stopping: &stopping,
        },
    );
    let _admission = match taken {
        Ok(admission) => admission,
        Err(why) => {
            let reason = format!("the proxy has its {} players", max_players());
            queue::turn_away(
                why,
                session.connection,
                sessions,
                peer,
                &name,
                uuid,
                &reason,
            );
            return;
        }
    };
    // Counted as online in the server list from now until the session ends.
    let _seat = client::Seat::take(&name, uuid);
    let auth = route_auth(session.auth);
    let retry = Duration::from_secs(config.backend_retry_seconds);
    let candidates = match route::candidates(config, session.virtual_host.as_deref(), auth) {
        // A backend that just failed is tried after the others.
        Ok(candidates) => sessions.health.order(candidates, retry, Instant::now()),
        Err(e) => {
            warning!(
                "{peer}: {name} ({}): no backend to send them to: {e}",
                hyphenated(uuid)
            );
            sessions.metrics().login_failed(LoginFailure::NoRoute);
            refuse(session.connection, "There is no server to join right now.");
            return;
        }
    };
    // The extension that decides the choice of server, if there is one, may put the candidates in an
    // order of its own or refuse the player (docs/EXTENSIONS.md, 4.4). It is asked after the player has a
    // place, and again if they wait in the queue and are woken.
    let (candidates, ordered) = match crate::extension::server::choose(
        &sessions.extensions,
        config,
        &sessions.shared(),
        &crate::extension::server::Who {
            name: &name,
            uuid: hyphenated(uuid),
            verified: session.auth == client::AuthKind::Premium,
            host: session.virtual_host.as_deref(),
            lost: None,
        },
        &candidates,
    ) {
        crate::extension::server::Outcome::Default => (candidates, false),
        crate::extension::server::Outcome::Order(servers) => (servers, true),
        crate::extension::server::Outcome::Refuse { by, message } => {
            info!(
                "{peer}: {name} ({}) refused by the extension \"{by}\" at the choice of server",
                hyphenated(uuid)
            );
            sessions.metrics().login_failed(LoginFailure::Extension);
            refuse(session.connection, &message);
            return;
        }
    };
    let options = BackendOptions {
        forwarding_secret: config.forwarding_secret.as_bytes().to_vec(),
        connect_timeout: Duration::from_millis(config.connect_timeout_ms),
        player_address: peer.ip(),
    };
    // With balancing on, the balancer places the player among the lobbies first.
    // Off, nothing here runs and the candidates are tried as they always were. Servers an extension
    // ordered are tried in its order, each held a place by the rule of a server the player named.
    let mut balanced = if ordered {
        crate::placing::ordered(
            config,
            &sessions.shared(),
            connect,
            &session.profile,
            &options,
            &candidates,
            &format!("{peer}: {name} ({})", hyphenated(uuid)),
        )
    } else {
        crate::placing::login(
            config,
            &sessions.shared(),
            connect,
            &session.profile,
            &options,
            &candidates,
            session.virtual_host.as_deref(),
        )
    };
    // Each candidate in turn, with the connect timeout for each.
    let mut joined = balanced.joined.take();
    for candidate in &candidates {
        if joined.is_some() {
            break;
        }
        if balanced.tried.contains(candidate) {
            continue;
        }
        let Some(backend) = config.backend(candidate) else {
            continue;
        };
        match connect(&backend.address, &session.profile, &options) {
            Ok(link) => {
                sessions.health.answered(&backend.name);
                joined = Some((backend, link, None));
                break;
            }
            Err(e) => {
                sessions.health.failed(&backend.name, Instant::now());
                warning!(
                    "{peer}: {name} ({}) cannot join backend \"{}\" at {}: {e}",
                    hyphenated(uuid),
                    backend.name,
                    backend.address
                );
            }
        }
    }
    let Some((backend, mut link, reservation)) = joined else {
        if balanced.full && line.enabled() {
            // Every lobby is at its hard cap. A player who can wait keeps their turn, gives
            // their place back and tries again when something changes.
            drop(_seat);
            drop(_admission);
            let stopping = || sessions.closing();
            let mut who = queue::Who {
                connection: &mut session.connection,
                stopping: &stopping,
            };
            match line.await_room(&mut who) {
                Ok(()) => return place(session, peer, config, connect, sessions, identity, line),
                Err(why) => {
                    let reason = "every lobby is at its hard cap";
                    queue::turn_away(why, session.connection, sessions, peer, &name, uuid, reason);
                    return;
                }
            }
        }
        if balanced.full {
            info!(
                "{peer}: {name} ({}) refused: every lobby is at its hard cap",
                hyphenated(uuid)
            );
            sessions.metrics().login_failed(LoginFailure::Full);
            refuse(session.connection, Full::MESSAGE);
        } else {
            sessions.metrics().login_failed(LoginFailure::Unreachable);
            refuse(session.connection, &cannot_reach(&candidates));
        }
        return;
    };
    // The wait, if there was one, is over: the player leaves the line and the backend
    // gets what they sent meanwhile.
    if line.was_queued() {
        line.forward_early(&mut link.connection);
        let _ = session
            .connection
            .reader
            .set_read_timeout(Some(client::READ_TIMEOUT));
    }
    line.served();
    // What the fallback needs of the login, which the session takes over.
    let profile = session.profile.clone();
    let virtual_host = session.virtual_host.clone();
    let (relay, handle) = match Session::new(session, link, &backend.name) {
        Ok(session) => session,
        Err(e) => {
            warning!("{peer}: {name}: cannot set up the session: {e}");
            return;
        }
    };
    let relay = relay
        .with_compression_level(config.compression_level)
        .with_commands(commands_hook(config, sessions, &handle, &name, auth, peer));
    // The backends' transfers are only looked at when one of them may be made seamless.
    let relay = if config.forced_hosts.iter().any(|f| f.seamless) {
        relay.with_transfers(switching::transfers_hook(
            Arc::new(config.clone()),
            Arc::clone(&sessions.switcher),
            sessions.shared(),
            name.clone(),
            peer.ip(),
        ))
    } else {
        relay
    };
    let player = Player {
        name: name.clone(),
        uuid,
        backend: backend.name.clone(),
        address: peer,
        joined: Instant::now(),
    };
    let open = match sessions.open_as(player, handle.clone(), identity, config.on_duplicate) {
        Ok(open) => open,
        Err(NotOpened::Stopping) => {
            debug!("{peer}: {name}: the proxy is stopping; not relaying");
            handle.disconnect(configuration::Disconnect {
                reason: text::translatable("multiplayer.disconnect.server_shutdown", &[]),
            });
            return;
        }
        Err(NotOpened::Duplicate) => {
            // Another login of the same player got there first.
            info!(
                "{peer}: {name} ({}) refused: already connected",
                hyphenated(uuid)
            );
            sessions.metrics().login_failed(LoginFailure::Duplicate);
            handle.disconnect(configuration::Disconnect {
                reason: text::literal(ALREADY_CONNECTED),
            });
            return;
        }
    };
    // The session counts from now on: the place reserved for it is given back.
    if reservation.as_ref().is_some_and(|r| r.overflow) {
        debug!(
            "{peer}: {name}: every open lobby was full; backend \"{}\" took the overflow",
            backend.name
        );
    }
    drop(reservation);
    sessions.metrics().login();
    info!(
        "{peer}: {name} ({}) joined backend \"{}\"",
        hyphenated(uuid),
        backend.name
    );
    // Told to the extensions that listen, after the session is listed (so that a move or an order that targets the player finds
    // them) and by this thread, which never waits for an extension.
    let verified = auth == meridian_config::AuthKind::Verified;
    sessions
        .extensions
        .emit(|| crate::extension::events::Kind::Joined {
            player: crate::extension::events::Kind::player(&name, uuid, verified),
            server: backend.name.clone(),
            address: peer.ip().to_string(),
        });
    let mut leaving = Leaving {
        sessions,
        open: Some(open),
        name: name.clone(),
        uuid,
        verified,
        handle: handle.clone(),
        broke: false,
    };
    let started = Instant::now();
    let policy = sessions.current_config(config).fallback.clone();
    let mut replace = crate::fallback::replacer(crate::fallback::Context {
        sessions,
        connect,
        config,
        handle: &handle,
        profile: &profile,
        virtual_host: virtual_host.as_deref(),
        auth,
        peer,
    });
    let fallback = meridian_handoff::Fallback {
        on_kick: policy.on_kick,
        on_crash: policy.on_crash,
    };
    match relay.run_with(fallback, &mut replace) {
        Ok(report) => info!(
            "{peer}: {name} ({}) left backend \"{}\" after {:.1} s: \
             {} packets, {} bytes to the player; {} packets, {} bytes to the backend",
            hyphenated(uuid),
            backend.name,
            started.elapsed().as_secs_f64(),
            report.to_client.packets,
            report.to_client.bytes,
            report.to_backend.packets,
            report.to_backend.bytes
        ),
        Err(e) => {
            warning!(
                "{peer}: {name} ({}) on backend \"{}\": the session broke after {:.1} s: {e}",
                hyphenated(uuid),
                backend.name,
                started.elapsed().as_secs_f64()
            );
            leaving.broke = true;
        }
    }
    // `leaving` goes here: the session is let go of, and the extensions are told.
}

/// A session's end, told to the extensions that listen **however it ends**: it is the guard of the session, so that "joined, then left"
/// holds even if the relay panics. The session is let go of first, and only then are the extensions told: one that asks who is online finds
/// the player gone. The reason is the proxy's own when it ended the session (a kick, a replacement, a stop), else `error` if the relay broke
/// or panicked, else `left`.
struct Leaving<'a> {
    sessions: &'a Sessions,
    open: Option<Open<'a>>,
    name: String,
    uuid: [u8; 16],
    verified: bool,
    handle: Handle,
    /// The relay ended with an error.
    broke: bool,
}

impl Drop for Leaving<'_> {
    fn drop(&mut self) {
        let ended = self.open.as_ref().and_then(Open::ended);
        drop(self.open.take());
        let reason = match ended {
            Some(EndReason::Kicked) => meridian_extension::events::LeftReason::Kicked,
            Some(EndReason::Replaced) => meridian_extension::events::LeftReason::Replaced,
            Some(EndReason::Shutdown) => meridian_extension::events::LeftReason::Shutdown,
            None if self.broke || std::thread::panicking() => {
                meridian_extension::events::LeftReason::Error
            }
            None => meridian_extension::events::LeftReason::Left,
        };
        self.sessions
            .extensions
            .emit(|| crate::extension::events::Kind::Left {
                player: crate::extension::events::Kind::player(
                    &self.name,
                    self.uuid,
                    self.verified,
                ),
                server: self.handle.backend(),
                reason,
            });
    }
}

/// The commands the proxy answers in game for the player of `handle`: a move
/// starts a switch on a thread of its own, and the player is told at once.
fn commands_hook(
    config: &Config,
    sessions: &Sessions,
    handle: &Handle,
    name: &str,
    auth: meridian_config::AuthKind,
    peer: SocketAddr,
) -> Arc<meridian_net::relay::CommandHook> {
    let config = Arc::new(config.clone());
    let shared = sessions.shared();
    // The extension that decides the permission of a command, if there is one, is asked with the
    // configuration in force when the command is typed. What it keeps of the answers belongs to this
    // session; the session holds the registry, and the registry holds nothing of the session.
    let current = {
        let (live, login) = (sessions.live.get().cloned(), Arc::clone(&config));
        move || match &live {
            Some(live) => Arc::clone(&live.read().unwrap_or_else(|e| e.into_inner())),
            None => Arc::clone(&login),
        }
    };
    let permit = Arc::new(crate::extension::command::Gate::new(
        Arc::clone(&sessions.extensions),
        current.clone(),
    ));
    // The commands an extension declared are the same player's: one runs at a time, and its lines are told
    // through the session's handle when they come.
    let own = {
        let handle = handle.clone();
        Arc::new(crate::extension::own::Own::new(
            Arc::clone(&sessions.extensions),
            current,
            move |line: &str| handle.tell(line),
        ))
    };
    let typist = {
        let (handle, permissions, operators) = (
            handle.clone(),
            Arc::clone(&shared.permissions),
            Arc::clone(&shared.operators),
        );
        move || {
            let backend = handle.backend();
            let (name, uuid) = (handle.profile().name.clone(), handle.profile().uuid);
            commands::Typist {
                auth,
                operator_groups: operators.groups_for(&backend, &name, uuid, auth),
                backend,
                name,
                uuid,
                permissions: Arc::new(permissions.snapshot()),
            }
        }
    };
    // A move to a backend the player named: with balancing on, it may pass the soft
    // cap, never the hard cap, and a place is kept on it until the switch ends.
    let mover = {
        let (config, switcher) = (Arc::clone(&config), Arc::clone(&sessions.switcher));
        let (handle, name, shared) = (handle.clone(), name.to_owned(), shared.clone());
        move |target: &str| {
            let place = match crate::placing::named(&config, &shared, target) {
                Ok(place) => place,
                Err(line) => return line,
            };
            switching::in_game(
                Arc::clone(&config),
                Arc::clone(&switcher),
                handle.clone(),
                name.clone(),
                peer.ip(),
                target.to_owned(),
                place,
            );
            format!("Moving you to {target}...")
        }
    };
    // `/send`, `/glist` and `/alert`, for the players who have the permission.
    let staff = {
        let context = crate::staff::Context {
            config: Arc::clone(&config),
            shared: shared.clone(),
            switcher: Arc::clone(&sessions.switcher),
            handle: handle.clone(),
        };
        move |typist: &commands::Typist, answer| crate::staff::run(&context, typist, answer)
    };
    // `/hub` and `/play` with balancing on: the lobby or game server the balancer finds
    // for the player.
    let placer = {
        let (config, switcher) = (Arc::clone(&config), Arc::clone(&sessions.switcher));
        let (handle, name) = (handle.clone(), name.to_owned());
        move |role: meridian_config::Role, typist: &commands::Typist| {
            let open_to = |backend: &str| commands::open_to(&config, backend, typist);
            let found = match role {
                meridian_config::Role::Game => {
                    crate::placing::game(&config, &shared, open_to, &typist.backend)
                }
                _ => crate::placing::hub(&config, &shared, open_to, &typist.backend),
            };
            match found {
                Ok((target, place)) => {
                    switching::in_game(
                        Arc::clone(&config),
                        Arc::clone(&switcher),
                        handle.clone(),
                        name.clone(),
                        peer.ip(),
                        target.clone(),
                        Some(place),
                    );
                    format!("Moving you to {target}...")
                }
                Err(line) => line,
            }
        }
    };
    commands::hook_with(
        config,
        typist,
        mover,
        placer,
        staff,
        Some(permit),
        Some(own),
    )
}

/// What a player is told when none of the backends tried answers.
fn cannot_reach(tried: &[String]) -> String {
    let names: Vec<String> = tried.iter().map(|n| format!("\"{n}\"")).collect();
    match names.as_slice() {
        [one] => format!("Cannot reach the server {one} right now. Try again in a moment."),
        many => format!(
            "Cannot reach the servers {} right now. Try again in a moment.",
            many.join(", ")
        ),
    }
}

/// Tells a player in the configuration phase why the session ends, then gives
/// them a moment to read it and close, so that the reason is not lost to a reset
/// connection.
pub(crate) fn refuse(mut connection: Connection, reason: &str) {
    let packet = configuration::Clientbound::Disconnect(configuration::Disconnect {
        reason: text::literal(reason),
    });
    if connection.send(&packet.encode()).is_err() {
        return;
    }
    if connection
        .reader
        .set_read_timeout(Some(REFUSAL_GRACE))
        .is_err()
    {
        return;
    }
    let until = Instant::now() + REFUSAL_GRACE;
    // Whatever the client still sends is read and dropped until it closes.
    while Instant::now() < until && connection.recv().is_ok() {}
}

/// A unique id as `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`.
pub fn hyphenated(uuid: [u8; 16]) -> String {
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use horizon_protocol::{crypto::offline_uuid, ids};
    use meridian_config::Backend;
    use meridian_net::backend;
    use meridian_testkit::{
        backend::{FakeBackend, Forwarded, Login, acknowledge_configuration, finish_configuration},
        client::FakeClient,
        peer::Step,
        pipe,
        trace::{Dir, Recorder, Trace},
    };
    use std::sync::Arc;
    use std::thread::JoinHandle;

    mod access;
    mod balancing;
    mod extension;
    mod fallback;
    mod staff;

    const SECRET: &str = "a shared secret";

    fn config() -> Config {
        Config {
            backends: vec![Backend {
                name: "lobby".into(),
                address: "127.0.0.1:25601".into(),
            }],
            default_backend: "lobby".into(),
            online_mode: false,
            compression_threshold: 256,
            forwarding_secret: SECRET.into(),
            // These tests relay a session to the backend's close; the fallback has its own.
            fallback: meridian_config::Fallback {
                on_kick: false,
                on_crash: false,
                ..meridian_config::Fallback::default()
            },
            ..Config::default()
        }
    }

    fn peer() -> SocketAddr {
        "198.51.100.7:50123".parse().unwrap()
    }

    type BackendThread = JoinHandle<Result<(Login, Connection), String>>;

    /// What the connector was asked for: the address, the options, and the fake
    /// backend's thread.
    type Seen = Arc<Mutex<Option<(String, BackendOptions, BackendThread)>>>;

    /// A connector that joins the proxy to `backend` through a pipe instead of a
    /// socket, keeps what it was asked for, and hands the backend's thread back.
    fn connector(backend: FakeBackend, recorder: Recorder) -> (Arc<Connect>, Seen) {
        let seen = Arc::new(Mutex::new(None));
        let kept = Arc::clone(&seen);
        let backend = Mutex::new(Some(backend));
        let connect: Arc<Connect> = Arc::new(move |address, profile, options| {
            let (proxy_end, server_end) = pipe();
            let fake = backend.lock().unwrap().take().expect("one connection");
            let thread = fake.spawn(server_end, recorder.clone());
            *kept.lock().unwrap() = Some((address.to_owned(), options.clone(), thread));
            backend::login(proxy_end, "127.0.0.1", 25601, profile, options)
        });
        (connect, seen)
    }

    /// The configuration phase, keep-alives both ways, the play phase, and the
    /// backend closing: the two scripts mirror each other.
    fn scripts() -> (Vec<Step>, Vec<Step>) {
        use ids::{configuration as cfg, play};
        let backend = vec![
            Step::send(cfg::clientbound::REGISTRY_DATA, vec![5; 700]),
            Step::send(cfg::clientbound::KEEP_ALIVE, 11u64.to_be_bytes()),
            Step::expect_exactly(cfg::serverbound::KEEP_ALIVE, 11u64.to_be_bytes()),
            finish_configuration(),
            Step::Expect {
                id: cfg::serverbound::FINISH_CONFIGURATION,
            },
            Step::send(play::clientbound::LOGIN, vec![6; 60]),
            Step::send(play::clientbound::KEEP_ALIVE, 12u64.to_be_bytes()),
            Step::expect_exactly(play::serverbound::KEEP_ALIVE, 12u64.to_be_bytes()),
            Step::Close,
        ];
        let client = vec![
            Step::expect_exactly(cfg::clientbound::REGISTRY_DATA, vec![5; 700]),
            Step::expect_exactly(cfg::clientbound::KEEP_ALIVE, 11u64.to_be_bytes()),
            Step::send(cfg::serverbound::KEEP_ALIVE, 11u64.to_be_bytes()),
            Step::Expect {
                id: cfg::clientbound::FINISH_CONFIGURATION,
            },
            acknowledge_configuration(),
            Step::expect_exactly(play::clientbound::LOGIN, vec![6; 60]),
            Step::expect_exactly(play::clientbound::KEEP_ALIVE, 12u64.to_be_bytes()),
            Step::send(play::serverbound::KEEP_ALIVE, 12u64.to_be_bytes()),
            Step::ExpectClose,
        ];
        (backend, client)
    }

    /// The packets after the login, which is where a direct session and a
    /// relayed one must agree: what the client received, then what the backend
    /// received once its `login_packets` login packets are left out.
    fn after_login(trace: &Trace, login_packets: usize) -> Trace {
        let to_client = trace.packets(Dir::ToClient).cloned();
        let to_server = trace.packets(Dir::ToServer).skip(login_packets).cloned();
        Trace {
            packets: to_client.chain(to_server).collect(),
        }
    }

    /// J1's proof in memory: a player logs in to the proxy, which logs them in to
    /// the backend with the forwarding and relays the session. Both sides see
    /// exactly what they see when the player joins the backend directly.
    #[test]
    fn a_player_is_relayed_to_the_backend_as_if_joined_directly() {
        let (backend_script, client_script) = scripts();

        // Directly: the client logs in to the backend itself.
        let direct = Recorder::new();
        let (client_end, server_end) = pipe();
        let server = FakeBackend {
            compression: Some(64),
            after_login: backend_script.clone(),
            ..FakeBackend::default()
        }
        .spawn(server_end, direct.clone());
        let client = FakeClient {
            after_login: client_script.clone(),
            ..FakeClient::named("Alex")
        }
        .spawn(client_end, direct.clone());
        client.join().unwrap().unwrap();
        server.join().unwrap().unwrap();

        // Through the proxy.
        let relayed = Recorder::new();
        let (connect, seen) = connector(
            FakeBackend {
                compression: Some(64),
                forwarding_secret: Some(SECRET.as_bytes().to_vec()),
                after_login: backend_script,
                ..FakeBackend::default()
            },
            relayed.clone(),
        );
        let (player_end, proxy_end) = pipe();
        let sessions = Arc::new(Sessions::default());
        let proxy = {
            let sessions = Arc::clone(&sessions);
            std::thread::spawn(move || {
                handle(proxy_end, peer(), &config(), &*connect, &sessions, None)
            })
        };
        let client = FakeClient {
            after_login: client_script,
            ..FakeClient::named("Alex")
        }
        .spawn(player_end, relayed.clone());
        let (welcome, _) = client.join().unwrap().unwrap();
        proxy.join().unwrap();
        let (address, options, server) = seen.lock().unwrap().take().unwrap();
        let (login, _) = server.join().unwrap().unwrap();

        // Direct: handshake, Login Start and Login Acknowledged before the script.
        // Relayed: the forwarding answer too.
        assert_eq!(
            after_login(&relayed.trace(), 4),
            after_login(&direct.trace(), 3)
        );
        assert_eq!(welcome.compression, Some(256), "the proxy's own threshold");
        assert_eq!(address, "127.0.0.1:25601");
        assert_eq!(
            options,
            BackendOptions {
                forwarding_secret: SECRET.as_bytes().to_vec(),
                connect_timeout: Duration::from_millis(5_000),
                player_address: peer().ip(),
            }
        );
        assert_eq!(
            login.forwarded,
            Some(Forwarded {
                version: 1,
                address: "198.51.100.7".into(),
                uuid: offline_uuid("Alex"),
                name: "Alex".into(),
                properties: Vec::new(),
            })
        );
        assert_eq!(sessions.count(), 0, "the session is counted out at the end");
    }

    /// `compression-level` reaches the player: the session is set up with the level of the configuration the
    /// connection is handled with (each connection takes the configuration of the moment, so a reload changes it for
    /// the connections after). The scripts' registry data is the only packet at the threshold or above: the bytes the
    /// player reads change with the level, and are those of level 6 when it is not set.
    #[test]
    fn the_compression_level_of_the_configuration_reaches_the_player() {
        use ids::configuration as cfg;
        let registry: Vec<u8> = (0..20_000u32).map(|i| ((i / 7) % 13) as u8).collect();
        let received = |level: Option<u32>| -> i64 {
            let (mut backend_script, mut client_script) = scripts();
            backend_script[0] = Step::send(cfg::clientbound::REGISTRY_DATA, registry.clone());
            client_script[0] =
                Step::expect_exactly(cfg::clientbound::REGISTRY_DATA, registry.clone());
            let (connect, _seen) = connector(
                FakeBackend {
                    compression: Some(64),
                    forwarding_secret: Some(SECRET.as_bytes().to_vec()),
                    after_login: backend_script,
                    ..FakeBackend::default()
                },
                Recorder::new(),
            );
            let mut config = config();
            if let Some(level) = level {
                config.compression_level = level;
            }
            let (player_end, proxy_end) = pipe();
            let sessions = Arc::new(Sessions::default());
            let proxy = std::thread::spawn(move || {
                handle(proxy_end, peer(), &config, &*connect, &sessions, None)
            });
            let client = FakeClient {
                after_login: client_script,
                ..FakeClient::named("Alex")
            }
            .spawn(player_end, Recorder::new());
            let (_, connection) = client.join().unwrap().unwrap();
            proxy.join().unwrap();
            connection
                .reader
                .counters
                .bytes_in
                .load(std::sync::atomic::Ordering::Relaxed)
        };
        let (unset, one, six, nine) = (
            received(None),
            received(Some(1)),
            received(Some(6)),
            received(Some(9)),
        );
        assert_eq!(unset, six, "without the setting, level 6");
        assert!(
            one > six && one > nine && nine != six,
            "level 1 {one}, 6 {six}, 9 {nine} bytes"
        );
    }

    /// A backend that cannot be reached: the player is told so in the
    /// configuration phase, and nothing hangs.
    #[test]
    fn a_player_is_told_when_the_backend_cannot_be_reached() {
        let connect: Arc<Connect> = Arc::new(|_, _, _| {
            Err(meridian_net::Error::Io(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                "refused",
            )))
        });
        let (player_end, proxy_end) = pipe();
        let proxy = std::thread::spawn(move || {
            handle(
                proxy_end,
                peer(),
                &config(),
                &*connect,
                &Sessions::default(),
                None,
            )
        });
        let recorder = Recorder::new();
        let client = FakeClient {
            after_login: vec![
                Step::Expect {
                    id: ids::configuration::clientbound::DISCONNECT,
                },
                Step::Close,
            ],
            ..FakeClient::named("Alex")
        }
        .spawn(player_end, recorder.clone());
        client.join().unwrap().unwrap();
        proxy.join().unwrap();
        let trace = recorder.trace();
        let packet = &trace.packets[0];
        let configuration::Clientbound::Disconnect(disconnect) =
            configuration::Clientbound::decode(packet.id, &packet.payload).unwrap()
        else {
            panic!("not a disconnect");
        };
        let reason = text::plain(&disconnect.reason, &Default::default());
        assert!(reason.contains("\"lobby\""), "{reason}");
    }

    /// A stop closes a session being relayed on both sides.
    #[test]
    fn a_stop_closes_the_sessions_being_relayed() {
        let recorder = Recorder::new();
        let (connect, seen) = connector(
            FakeBackend {
                forwarding_secret: Some(SECRET.as_bytes().to_vec()),
                after_login: vec![Step::ExpectClose],
                ..FakeBackend::default()
            },
            recorder.clone(),
        );
        let (player_end, proxy_end) = pipe();
        let sessions = Arc::new(Sessions::default());
        let proxy = {
            let sessions = Arc::clone(&sessions);
            std::thread::spawn(move || {
                handle(proxy_end, peer(), &config(), &*connect, &sessions, None)
            })
        };
        let player_trace = Recorder::new();
        let client = FakeClient {
            after_login: vec![
                Step::Expect {
                    id: ids::configuration::clientbound::DISCONNECT,
                },
                Step::ExpectClose,
            ],
            ..FakeClient::named("Alex")
        }
        .spawn(player_end, player_trace.clone());
        let deadline = Instant::now() + Duration::from_secs(5);
        while sessions.count() == 0 {
            assert!(Instant::now() < deadline, "the session never started");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(sessions.close_all(), 1);
        client.join().unwrap().unwrap();
        proxy.join().unwrap();
        let (_, _, server) = seen.lock().unwrap().take().unwrap();
        server.join().unwrap().unwrap();
        assert_eq!(sessions.count(), 0);
        // The player is told the server closed, in their own language.
        let trace = player_trace.trace();
        let configuration::Clientbound::Disconnect(disconnect) =
            configuration::Clientbound::decode(trace.packets[0].id, &trace.packets[0].payload)
                .unwrap()
        else {
            panic!("not a disconnect");
        };
        assert_eq!(
            disconnect.reason,
            text::translatable("multiplayer.disconnect.server_shutdown", &[])
        );
    }

    /// A session relayed between two pipes whose far ends do nothing, with the
    /// player's and the backend's ends handed back.
    pub fn idle_session(name: &str) -> (Session, Handle, Connection, Connection) {
        idle_session_on(name, "lobby")
    }

    /// The same, on the backend `backend`.
    pub fn idle_session_on(name: &str, backend: &str) -> (Session, Handle, Connection, Connection) {
        let (proxy_client, player) = pipe();
        let (proxy_backend, server) = pipe();
        let client = client::ClientSession {
            profile: Profile {
                name: name.to_owned(),
                uuid: offline_uuid(name).to_be_bytes(),
                properties: Vec::new(),
            },
            auth: client::AuthKind::Offline,
            virtual_host: None,
            connection: proxy_client,
        };
        let link = BackendLink {
            connection: proxy_backend,
        };
        let (relay, handle) = Session::new(client, link, backend).unwrap();
        (relay, handle, player, server)
    }

    #[test]
    fn a_unique_id_is_written_with_hyphens() {
        let uuid = 0x0123456789abcdef_fedcba9876543210_u128.to_be_bytes();
        assert_eq!(hyphenated(uuid), "01234567-89ab-cdef-fedc-ba9876543210");
    }

    #[test]
    fn a_stopping_proxy_refuses_new_sessions() {
        let sessions = Sessions::default();
        assert_eq!(sessions.close_all(), 0);
        let (_relay, handle, _player, _server) = idle_session("Alex");
        let player = Player {
            name: "Alex".into(),
            uuid: [0; 16],
            backend: "lobby".into(),
            address: peer(),
            joined: Instant::now(),
        };
        assert!(sessions.open(player, handle).is_none());
        assert_eq!(sessions.count(), 0);
    }

    /// The proxy marks why it ends a session, just before it disconnects the player: that is what the event `players/left` says. A
    /// session that nobody ended has no mark.
    #[test]
    fn the_proxy_marks_why_it_ends_a_session() {
        let sessions = Sessions::default();
        let alex = connected(&sessions, "Alex", Identity::Unverified("alex".into()));
        let steve = connected(&sessions, "Steve", Identity::Unverified("steve".into()));
        let notch = connected(
            &sessions,
            "Notch",
            Identity::Verified(offline_uuid("Notch").to_be_bytes()),
        );
        let herobrine = connected(
            &sessions,
            "Herobrine",
            Identity::Unverified("herobrine".into()),
        );
        for open in [&alex.0, &steve.0, &notch.0, &herobrine.0] {
            assert_eq!(open.ended(), None);
        }
        // The console's kick, and an extension's.
        assert!(sessions.kick("steve", "bye").is_some());
        assert_eq!(steve.0.ended(), Some(EndReason::Kicked));
        assert!(matches!(
            sessions.kick_one("bye", |p| p.name == "Alex", |_, _| false),
            Kicked::Done(_)
        ));
        assert_eq!(alex.0.ended(), Some(EndReason::Kicked));
        // A kick that is refused marks nothing.
        assert!(matches!(
            sessions.kick_one("bye", |p| p.name == "Notch", |_, _| true),
            Kicked::Protected
        ));
        assert_eq!(notch.0.ended(), None);
        // A second login that replaces the first.
        assert_eq!(
            sessions.replace(&Identity::Verified(offline_uuid("Notch").to_be_bytes())),
            1
        );
        assert_eq!(notch.0.ended(), Some(EndReason::Replaced));
        // A stop marks the others, and does not change what was already said.
        assert_eq!(sessions.close_all(), 4);
        assert_eq!(herobrine.0.ended(), Some(EndReason::Shutdown));
        assert_eq!(alex.0.ended(), Some(EndReason::Kicked));
        assert_eq!(notch.0.ended(), Some(EndReason::Replaced));
    }

    /// "Joined, then left" holds even when the relay panics: the guard of the session tells the end when it is dropped by the unwinding, once,
    /// with the reason `error`, and the player is out of the list by then.
    #[test]
    fn a_session_whose_relay_panics_is_still_told_to_the_extensions_as_left_with_error() {
        use meridian_extension::events::{Event, LeftReason};
        let sessions = Sessions::default();
        let told_list = sessions.extensions.bus().recording(false);
        let told = &told_list;
        let open = connected(&sessions, "Alex", Identity::Unverified("alex".into()));
        let (_, handle) = sessions.session("Alex").unwrap();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _leaving = Leaving {
                sessions: &sessions,
                open: Some(open.0),
                name: "Alex".into(),
                uuid: offline_uuid("Alex").to_be_bytes(),
                verified: false,
                handle,
                broke: false,
            };
            panic!("a relay that fails");
        }));
        assert!(outcome.is_err());
        let limit = Instant::now() + Duration::from_secs(5);
        while told.lock().unwrap().is_empty() {
            assert!(Instant::now() < limit, "the end was never told");
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(50));
        let told = told.lock().unwrap().clone();
        assert_eq!(told.len(), 1, "once: {told:?}");
        let message: serde_json::Value = serde_json::from_str(&told[0]).unwrap();
        let Event::Left(left) = Event::parse(
            message["method"].as_str().unwrap(),
            Some(&message["params"]),
        ) else {
            panic!("{told:?}");
        };
        assert_eq!(
            (left.player.name.as_str(), left.reason),
            ("Alex", LeftReason::Error)
        );
        assert!(
            sessions.find("Alex").is_none(),
            "the player is out of the list when it is told"
        );
        // Without a panic: `error` when the relay broke, `left` when it did not.
        for (name, broke, want) in [
            ("Steve", true, LeftReason::Error),
            ("Notch", false, LeftReason::Left),
        ] {
            let open = connected(&sessions, name, Identity::Unverified(name.to_lowercase()));
            let (_, handle) = sessions.session(name).unwrap();
            drop(Leaving {
                sessions: &sessions,
                open: Some(open.0),
                name: name.into(),
                uuid: offline_uuid(name).to_be_bytes(),
                verified: false,
                handle,
                broke,
            });
            let limit = Instant::now() + Duration::from_secs(5);
            let mut reason = None;
            while reason.is_none() {
                assert!(Instant::now() < limit, "{name}'s end was never told");
                std::thread::sleep(Duration::from_millis(5));
                reason = told_list.lock().unwrap().iter().find_map(|text| {
                    let message: serde_json::Value = serde_json::from_str(text).ok()?;
                    let Event::Left(left) =
                        Event::parse(message["method"].as_str()?, Some(&message["params"]))
                    else {
                        return None;
                    };
                    (left.player.name == name).then_some(left.reason)
                });
            }
            assert_eq!(reason, Some(want), "{name}");
        }
    }

    /// The registry lists players in the order they joined, finds them whatever
    /// the case of the name, and a kick ends only that player's session.
    #[test]
    fn the_registry_lists_finds_and_kicks_players() {
        let sessions = Sessions::default();
        let mut ends = Vec::new();
        let mut opened = Vec::new();
        let start = Instant::now();
        for (i, name) in ["Alex", "Steve"].into_iter().enumerate() {
            let (relay, handle, player, server) = idle_session(name);
            let player_info = Player {
                name: name.into(),
                uuid: offline_uuid(name).to_be_bytes(),
                backend: "lobby".into(),
                address: peer(),
                joined: start + Duration::from_millis(i as u64),
            };
            opened.push(sessions.open(player_info, handle).unwrap());
            ends.push((std::thread::spawn(move || relay.run()), player, server));
        }
        let names: Vec<String> = sessions.players().into_iter().map(|p| p.name).collect();
        assert_eq!(names, ["Alex", "Steve"]);
        assert_eq!(sessions.counts().get("lobby"), Some(&2));
        assert_eq!(sessions.counts().len(), 1);
        assert_eq!(sessions.find("steve").unwrap().name, "Steve");
        assert!(sessions.find("Herobrine").is_none());
        assert!(sessions.kick("Herobrine", "bye").is_none());
        assert_eq!(sessions.kick("ALEX", "bye").unwrap().name, "Alex");
        let (relay, mut player, _server) = ends.remove(0);
        assert!(relay.join().unwrap().is_ok());
        let (id, payload) = player.recv().unwrap();
        let configuration::Clientbound::Disconnect(d) =
            configuration::Clientbound::decode(id, &payload).unwrap()
        else {
            panic!("not a disconnect");
        };
        assert_eq!(text::plain(&d.reason, &Default::default()), "bye");
        // Steve is still relayed.
        drop(opened.remove(0));
        assert_eq!(sessions.count(), 1);
        drop(opened);
        for (relay, player, server) in ends {
            drop((player, server));
            assert!(relay.join().unwrap().is_ok());
        }
    }

    /// A player already logged in to the proxy: the session the proxy holds, and
    /// the player's own end, in the configuration phase.
    fn logged_in(
        name: &str,
        auth: client::AuthKind,
        host: Option<&str>,
    ) -> (ClientSession, Connection) {
        let (proxy_end, player) = pipe();
        let session = ClientSession {
            profile: Profile {
                name: name.to_owned(),
                uuid: offline_uuid(name).to_be_bytes(),
                properties: Vec::new(),
            },
            auth,
            virtual_host: host.map(str::to_owned),
            connection: proxy_end,
        };
        (session, player)
    }

    /// The backends of the routing tests: lobby, east and west, an offline proxy.
    fn routed() -> Config {
        let backend = |name: &str, port: u16| Backend {
            name: name.into(),
            address: format!("127.0.0.1:{port}"),
        };
        Config {
            backends: vec![
                backend("lobby", 25601),
                backend("east", 25602),
                backend("west", 25603),
            ],
            default_backend: "lobby".into(),
            online_mode: false,
            auth_mode: meridian_config::AuthMode::Offline,
            forwarding_secret: SECRET.into(),
            ..Config::default()
        }
    }

    /// Backends reached through pipes, by address: `None` is one that does not
    /// answer. Keeps the addresses tried, in order, and the backends' threads.
    struct Reachable {
        tried: Arc<Mutex<Vec<String>>>,
        threads: Arc<Mutex<Vec<BackendThread>>>,
    }

    fn reachable(backends: Vec<(&str, bool)>) -> (Arc<Connect>, Reachable) {
        reachable_after(backends, vec![Step::Close])
    }

    /// [`reachable`], with a backend that does `after_login` once a player is in: waiting to be closed keeps the session open.
    fn reachable_after(
        backends: Vec<(&str, bool)>,
        after_login: Vec<Step>,
    ) -> (Arc<Connect>, Reachable) {
        let answering: HashMap<String, bool> = backends
            .into_iter()
            .map(|(address, answers)| (address.to_owned(), answers))
            .collect();
        let kept = Reachable {
            tried: Arc::new(Mutex::new(Vec::new())),
            threads: Arc::new(Mutex::new(Vec::new())),
        };
        let (tried, threads) = (Arc::clone(&kept.tried), Arc::clone(&kept.threads));
        let connect: Arc<Connect> = Arc::new(move |address, profile, options| {
            tried.lock().unwrap().push(address.to_owned());
            // A backend that answers takes every player sent to it.
            let fake = answering
                .get(address)
                .copied()
                .unwrap_or(false)
                .then(|| FakeBackend {
                    forwarding_secret: Some(SECRET.as_bytes().to_vec()),
                    after_login: after_login.clone(),
                    ..FakeBackend::default()
                });
            let Some(fake) = fake else {
                return Err(meridian_net::Error::Io(std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    "refused",
                )));
            };
            let (proxy_end, server_end) = pipe();
            threads
                .lock()
                .unwrap()
                .push(fake.spawn(server_end, Recorder::new()));
            backend::login(proxy_end, "127.0.0.1", 25601, profile, options)
        });
        (connect, kept)
    }

    /// Runs one logged-in player through the routing. The player's end runs
    /// `script`; returns the addresses tried and what the player received.
    fn route_one(
        config: &Config,
        sessions: &Sessions,
        connect: &Connect,
        reachable: &Reachable,
        session: (ClientSession, Connection),
        script: Vec<Step>,
    ) -> (Vec<String>, Trace) {
        let (session, player) = session;
        let recorder = Recorder::new();
        let player = meridian_testkit::peer::spawn(player, script, Dir::ToClient, recorder.clone());
        serve(session, peer(), config, connect, sessions);
        player.join().unwrap().unwrap();
        for thread in reachable.threads.lock().unwrap().drain(..) {
            thread.join().unwrap().unwrap();
        }
        let tried = std::mem::take(&mut *reachable.tried.lock().unwrap());
        (tried, recorder.trace())
    }

    /// The reason of the configuration-phase Disconnect a player received.
    fn refusal(trace: &Trace) -> String {
        let packet = &trace.packets[0];
        let configuration::Clientbound::Disconnect(d) =
            configuration::Clientbound::decode(packet.id, &packet.payload).unwrap()
        else {
            panic!("not a disconnect");
        };
        text::plain(&d.reason, &Default::default())
    }

    fn refused() -> Vec<Step> {
        vec![
            Step::Expect {
                id: ids::configuration::clientbound::DISCONNECT,
            },
            Step::Close,
        ]
    }

    #[test]
    fn the_route_knows_both_kinds_of_login() {
        use meridian_config::AuthKind;
        assert_eq!(route_auth(client::AuthKind::Premium), AuthKind::Verified);
        assert_eq!(route_auth(client::AuthKind::Offline), AuthKind::Unverified);
    }

    /// A host name forced to east sends the player there, not to the default.
    #[test]
    fn a_forced_host_chooses_the_backend() {
        let config = Config {
            forced_hosts: vec![meridian_config::ForcedHost {
                host: "east.example.com".into(),
                backend: "east".into(),
                motd: None,
                seamless: false,
            }],
            ..routed()
        };
        let sessions = Sessions::default();
        let (connect, backends) =
            reachable(vec![("127.0.0.1:25601", true), ("127.0.0.1:25602", true)]);
        let (tried, _) = route_one(
            &config,
            &sessions,
            &*connect,
            &backends,
            logged_in("Alex", client::AuthKind::Offline, Some("east.example.com")),
            vec![Step::ExpectClose],
        );
        assert_eq!(tried, ["127.0.0.1:25602"]);
        assert_eq!(sessions.admitted(), 0, "the place is given back at the end");
    }

    /// The default backend is down: the player goes to the next in `try`.
    #[test]
    fn a_player_falls_back_when_the_first_backend_is_down() {
        let config = Config {
            try_backends: vec!["east".into()],
            ..routed()
        };
        let sessions = Sessions::default();
        let (connect, backends) =
            reachable(vec![("127.0.0.1:25601", false), ("127.0.0.1:25602", true)]);
        let (tried, _) = route_one(
            &config,
            &sessions,
            &*connect,
            &backends,
            logged_in("Alex", client::AuthKind::Offline, None),
            vec![Step::ExpectClose],
        );
        assert_eq!(tried, ["127.0.0.1:25601", "127.0.0.1:25602"]);
    }

    /// The default backend failed for one player: the next goes straight to the
    /// fallback, and the default is tried again once the retry time is over.
    #[test]
    fn a_backend_that_failed_is_passed_over_for_the_next_player() {
        let config = Config {
            try_backends: vec!["east".into()],
            ..routed()
        };
        let sessions = Sessions::default();
        let (connect, backends) =
            reachable(vec![("127.0.0.1:25601", false), ("127.0.0.1:25602", true)]);
        let first = route_one(
            &config,
            &sessions,
            &*connect,
            &backends,
            logged_in("Alex", client::AuthKind::Offline, None),
            vec![Step::ExpectClose],
        );
        assert_eq!(first.0, ["127.0.0.1:25601", "127.0.0.1:25602"]);
        let second = route_one(
            &config,
            &sessions,
            &*connect,
            &backends,
            logged_in("Steve", client::AuthKind::Offline, None),
            vec![Step::ExpectClose],
        );
        assert_eq!(
            second.0,
            ["127.0.0.1:25602"],
            "lobby is not waited for again"
        );
        assert!(
            sessions
                .health
                .describe("lobby", Duration::from_secs(10), Instant::now())
                .is_some()
        );

        // With no retry time, every player tries the default first.
        let config = Config {
            backend_retry_seconds: 0,
            ..config
        };
        let third = route_one(
            &config,
            &sessions,
            &*connect,
            &backends,
            logged_in("Notch", client::AuthKind::Offline, None),
            vec![Step::ExpectClose],
        );
        assert_eq!(third.0, ["127.0.0.1:25601", "127.0.0.1:25602"]);
    }

    /// A relayed session of `name` with `identity`, kept until the returned
    /// values are dropped: the session, the relay's thread and both far ends.
    fn connected<'a>(
        sessions: &'a Sessions,
        name: &str,
        identity: Identity,
    ) -> (
        Open<'a>,
        std::thread::JoinHandle<meridian_net::Result<meridian_net::relay::Report>>,
        Connection,
        Connection,
    ) {
        let (relay, handle, player, server) = idle_session(name);
        let info = Player {
            name: name.into(),
            uuid: offline_uuid(name).to_be_bytes(),
            backend: "lobby".into(),
            address: peer(),
            joined: Instant::now(),
        };
        let open = sessions
            .open_as(info, handle, identity, OnDuplicate::Refuse)
            .unwrap();
        (
            open,
            std::thread::spawn(move || relay.run()),
            player,
            server,
        )
    }

    /// Whether the Disconnect a relayed player received says they logged in
    /// from another location.
    fn disconnect_reason(player: &mut Connection) -> bool {
        let (id, payload) = player.recv().unwrap();
        let configuration::Clientbound::Disconnect(d) =
            configuration::Clientbound::decode(id, &payload).unwrap()
        else {
            panic!("not a disconnect");
        };
        d == duplicate_login()
    }

    #[test]
    fn identities_follow_how_the_player_logged_in() {
        let (verified, _) = logged_in("Notch", client::AuthKind::Premium, None);
        assert_eq!(
            Identity::of(&verified),
            Identity::Verified(offline_uuid("Notch").to_be_bytes())
        );
        // An unverified name is the same player whatever its case.
        let (a, _) = logged_in("Alex", client::AuthKind::Offline, None);
        let (b, _) = logged_in("aLEX", client::AuthKind::Offline, None);
        assert_eq!(Identity::of(&a), Identity::of(&b));
        assert_eq!(Identity::of(&a), Identity::Unverified("alex".into()));
    }

    #[test]
    fn a_second_session_of_one_player_is_refused_or_replaces_the_first() {
        let sessions = Sessions::default();
        let alex = Identity::Unverified("alex".into());
        let (first, relay, mut player, _server) = connected(&sessions, "Alex", alex.clone());

        // Refused: the first session goes on.
        let (_r, handle, _p, _s) = idle_session("ALEX");
        let info = sessions.players()[0].clone();
        assert_eq!(
            sessions
                .open_as(info.clone(), handle, alex.clone(), OnDuplicate::Refuse)
                .err(),
            Some(NotOpened::Duplicate)
        );
        assert_eq!(sessions.count(), 1);

        // Another player, or the same name verified, is not a duplicate.
        let (_r2, handle, _p2, _s2) = idle_session("Steve");
        let steve = sessions.open_as(
            info.clone(),
            handle,
            Identity::Unverified("steve".into()),
            OnDuplicate::Refuse,
        );
        assert!(steve.is_ok());
        let (_r3, handle, _p3, _s3) = idle_session("Alex");
        let verified = sessions.open_as(
            info.clone(),
            handle,
            Identity::Verified([7; 16]),
            OnDuplicate::Refuse,
        );
        assert!(verified.is_ok());

        // Replaced: the first session is told it logged in elsewhere.
        let (_r4, handle, _p4, _s4) = idle_session("alex");
        let replacing = sessions.open_as(info, handle, alex, OnDuplicate::Replace);
        assert!(replacing.is_ok());
        assert!(
            disconnect_reason(&mut player),
            "told of the login elsewhere"
        );
        assert!(relay.join().unwrap().is_ok());
        drop(first);
    }

    /// With duplicates refused, a second login of a connected player is told so
    /// before any backend is asked, and takes no place.
    #[test]
    fn a_player_already_connected_is_refused_before_any_backend() {
        let config = routed();
        let sessions = Sessions::default();
        let (_open, _relay, _player, _server) =
            connected(&sessions, "Alex", Identity::Unverified("alex".into()));
        let (connect, backends) = reachable(vec![("127.0.0.1:25601", true)]);
        let (tried, trace) = route_one(
            &config,
            &sessions,
            &*connect,
            &backends,
            logged_in("ALEX", client::AuthKind::Offline, None),
            refused(),
        );
        assert!(tried.is_empty(), "no backend login for a refused duplicate");
        assert_eq!(refusal(&trace), ALREADY_CONNECTED);
        assert_eq!(sessions.admitted(), 0);
        assert_eq!(sessions.count(), 1);
    }

    /// With duplicates replaced, the second login ends the first session and is
    /// sent to a backend.
    #[test]
    fn a_second_login_replaces_the_first_when_set_so() {
        let config = Config {
            on_duplicate: OnDuplicate::Replace,
            ..routed()
        };
        let sessions = Sessions::default();
        let (open, relay, mut player, _server) =
            connected(&sessions, "Alex", Identity::Unverified("alex".into()));
        let (connect, backends) = reachable(vec![("127.0.0.1:25601", true)]);
        let ended = std::thread::spawn(move || {
            let reason = disconnect_reason(&mut player);
            let report = relay.join().unwrap();
            (reason, report.is_ok())
        });
        let (tried, _) = route_one(
            &config,
            &sessions,
            &*connect,
            &backends,
            logged_in("alex", client::AuthKind::Offline, None),
            vec![Step::ExpectClose],
        );
        assert_eq!(tried, ["127.0.0.1:25601"]);
        let (reason, relay_ended) = ended.join().unwrap();
        assert!(reason, "told of the login elsewhere");
        assert!(relay_ended);
        drop(open);
    }

    /// A relayed player types `/server`: the proxy answers in the chat and the
    /// backend never sees it. A command of the backend's reaches it unchanged.
    #[test]
    fn the_proxy_answers_server_in_game_and_passes_other_commands_on() {
        use horizon_protocol::packets::play;
        use ids::{configuration as cfg, play as ids_play};
        let payload = |body: Vec<u8>| body[1..].to_vec();
        let command = |text: &str| {
            payload(
                play::Serverbound::ChatCommand(play::ChatCommand {
                    command: text.into(),
                })
                .encode(),
            )
        };
        let chat = |text: &str| {
            payload(
                play::Clientbound::SystemChat(play::SystemChat {
                    content: text::literal(text),
                    overlay: false,
                })
                .encode(),
            )
        };
        let backend = FakeBackend {
            forwarding_secret: Some(SECRET.as_bytes().to_vec()),
            after_login: vec![
                finish_configuration(),
                Step::Expect {
                    id: cfg::serverbound::FINISH_CONFIGURATION,
                },
                Step::expect_exactly(ids_play::serverbound::CHAT_COMMAND, command("tell Bob hi")),
                Step::Close,
            ],
            ..FakeBackend::default()
        };
        let (connect, seen) = connector(backend, Recorder::new());
        let (session, player) = logged_in("Alex", client::AuthKind::Offline, None);
        let player = meridian_testkit::peer::spawn(
            player,
            vec![
                Step::Expect {
                    id: cfg::clientbound::FINISH_CONFIGURATION,
                },
                acknowledge_configuration(),
                Step::send(ids_play::serverbound::CHAT_COMMAND, command("server")),
                Step::expect_exactly(
                    ids_play::clientbound::SYSTEM_CHAT,
                    chat("Servers: lobby (default). You are on lobby."),
                ),
                Step::send(ids_play::serverbound::CHAT_COMMAND, command("tell Bob hi")),
                Step::ExpectClose,
            ],
            Dir::ToClient,
            Recorder::new(),
        );
        let sessions = Sessions::default();
        serve(session, peer(), &config(), &*connect, &sessions);
        player.join().unwrap().unwrap();
        let (_, _, backend) = seen.lock().unwrap().take().unwrap();
        backend.join().unwrap().unwrap();
    }

    /// None of the candidates answers: the player is told which were tried.
    #[test]
    fn a_player_is_told_when_no_candidate_answers() {
        let config = Config {
            try_backends: vec!["east".into()],
            ..routed()
        };
        let sessions = Sessions::default();
        let (connect, backends) =
            reachable(vec![("127.0.0.1:25601", false), ("127.0.0.1:25602", false)]);
        let (tried, trace) = route_one(
            &config,
            &sessions,
            &*connect,
            &backends,
            logged_in("Alex", client::AuthKind::Offline, None),
            refused(),
        );
        assert_eq!(tried, ["127.0.0.1:25601", "127.0.0.1:25602"]);
        assert_eq!(
            refusal(&trace),
            "Cannot reach the servers \"lobby\", \"east\" right now. Try again in a moment."
        );
        assert_eq!(sessions.admitted(), 0);
    }

    /// At the player limit, a player is refused before any backend is tried.
    #[test]
    fn a_full_proxy_refuses_the_player() {
        let config = Config {
            max_players: 1,
            ..routed()
        };
        let sessions = Sessions::default();
        let held = sessions.admit(1).unwrap();
        let (connect, backends) = reachable(vec![("127.0.0.1:25601", true)]);
        let (tried, trace) = route_one(
            &config,
            &sessions,
            &*connect,
            &backends,
            logged_in("Steve", client::AuthKind::Offline, None),
            refused(),
        );
        assert!(tried.is_empty());
        assert_eq!(refusal(&trace), "The server is full");
        assert_eq!(
            sessions.metrics().failures_of(LoginFailure::Full),
            1,
            "counted as a login that failed for being full"
        );
        assert_eq!(sessions.admitted(), 1, "only the place already held");
        drop(held);
        assert_eq!(sessions.admitted(), 0);
    }

    /// A proxy with room for one player, whose queue is on.
    fn queued(wait_seconds: u64, max_length: u32) -> Config {
        Config {
            max_players: 1,
            queue: meridian_config::QueueSettings {
                enabled: true,
                wait_seconds,
                max_length,
            },
            ..routed()
        }
    }

    /// With the queue on, a player who finds the proxy full waits, and gets the place
    /// that is given back, then goes to their backend.
    #[test]
    fn a_player_waits_for_a_place_and_gets_it() {
        let config = queued(5, 3);
        let sessions = Sessions::default();
        let held = sessions.admit(1).unwrap();
        let (connect, backends) = reachable(vec![("127.0.0.1:25601", true)]);
        let started = Instant::now();
        let (tried, _) = std::thread::scope(|scope| {
            let sessions = &sessions;
            scope.spawn(move || {
                while sessions.queue.len() == 0 {
                    std::thread::sleep(Duration::from_millis(5));
                }
                std::thread::sleep(Duration::from_millis(200));
                drop(held);
            });
            route_one(
                &config,
                sessions,
                &*connect,
                &backends,
                logged_in("Steve", client::AuthKind::Offline, None),
                vec![Step::ExpectClose],
            )
        });
        assert!(started.elapsed() >= Duration::from_millis(200));
        assert_eq!(tried, ["127.0.0.1:25601"]);
        assert_eq!(sessions.queue.len(), 0);
        assert_eq!(sessions.admitted(), 0);
        assert_eq!(sessions.metrics().queue_counts(), (1, 0, [1, 0, 0]));
        assert_eq!(sessions.metrics().failures_of(LoginFailure::Full), 0);
    }

    /// A wait that runs out is the refusal a full proxy always gave.
    #[test]
    fn a_wait_that_runs_out_refuses_the_player() {
        let config = queued(1, 3);
        let sessions = Sessions::default();
        let _held = sessions.admit(1).unwrap();
        let (connect, backends) = reachable(vec![("127.0.0.1:25601", true)]);
        let started = Instant::now();
        let (tried, trace) = route_one(
            &config,
            &sessions,
            &*connect,
            &backends,
            logged_in("Steve", client::AuthKind::Offline, None),
            refused(),
        );
        assert!(started.elapsed() >= Duration::from_millis(950));
        assert!(tried.is_empty());
        assert_eq!(refusal(&trace), "The server is full");
        assert_eq!(sessions.queue.len(), 0);
        assert_eq!(sessions.admitted(), 1, "only the place already held");
        assert_eq!(sessions.metrics().queue_counts(), (1, 0, [0, 1, 0]));
        assert_eq!(sessions.metrics().failures_of(LoginFailure::Full), 1);
    }

    /// A player who closes the connection while they wait is out of the line, and
    /// costs no place and no backend.
    #[test]
    fn a_player_who_leaves_the_queue_costs_nothing() {
        let config = queued(10, 3);
        let sessions = Sessions::default();
        let _held = sessions.admit(1).unwrap();
        let (connect, backends) = reachable(vec![("127.0.0.1:25601", true)]);
        let started = Instant::now();
        let (tried, _) = route_one(
            &config,
            &sessions,
            &*connect,
            &backends,
            logged_in("Steve", client::AuthKind::Offline, None),
            vec![Step::Close],
        );
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "{:?}",
            started.elapsed()
        );
        assert!(tried.is_empty());
        assert_eq!(sessions.queue.len(), 0);
        assert_eq!(sessions.admitted(), 1);
        assert_eq!(sessions.metrics().queue_counts(), (1, 0, [0, 0, 1]));
    }

    /// With the line at its longest, the next player is refused at once, and the one
    /// who waits is served when the place comes.
    #[test]
    fn a_full_queue_refuses_at_once() {
        let config = queued(10, 1);
        let sessions = Sessions::default();
        let held = sessions.admit(1).unwrap();
        let (connect, backends) = reachable(vec![("127.0.0.1:25601", true)]);
        std::thread::scope(|scope| {
            let (alex, alex_end) = logged_in("Alex", client::AuthKind::Offline, None);
            let alex_peer = meridian_testkit::peer::spawn(
                alex_end,
                vec![Step::ExpectClose],
                Dir::ToClient,
                Recorder::new(),
            );
            let serving = scope.spawn(|| serve(alex, peer(), &config, &*connect, &sessions));
            while sessions.queue.len() == 0 {
                std::thread::sleep(Duration::from_millis(5));
            }
            let started = Instant::now();
            let (tried, trace) = route_one(
                &config,
                &sessions,
                &*connect,
                &backends,
                logged_in("Bob", client::AuthKind::Offline, None),
                refused(),
            );
            assert!(started.elapsed() < Duration::from_secs(1));
            assert!(tried.is_empty());
            assert_eq!(refusal(&trace), "The server is full");
            assert_eq!(sessions.queue.len(), 1, "Alex still waits");
            drop(held);
            serving.join().unwrap();
            alex_peer.join().unwrap().unwrap();
        });
        let (entered, refused_at_once, left) = sessions.metrics().queue_counts();
        assert_eq!((entered, refused_at_once, left), (1, 1, [1, 0, 0]));
        for thread in backends.threads.lock().unwrap().drain(..) {
            thread.join().unwrap().unwrap();
        }
    }

    /// What the extensions that listen are told of a player who waits at a real login: they enter the line at its first place and, when
    /// the place comes, leave it served.
    #[test]
    fn a_player_who_waits_at_a_login_is_told_entering_the_line_and_served() {
        let config = queued(5, 3);
        let sessions = Sessions::default();
        let told = sessions.extensions.bus().recording(false);
        let held = sessions.admit(1).unwrap();
        let (connect, backends) = reachable(vec![("127.0.0.1:25601", true)]);
        std::thread::scope(|scope| {
            let sessions = &sessions;
            scope.spawn(move || {
                while sessions.queue.len() == 0 {
                    std::thread::sleep(Duration::from_millis(5));
                }
                std::thread::sleep(Duration::from_millis(100));
                drop(held);
            });
            route_one(
                &config,
                sessions,
                &*connect,
                &backends,
                logged_in("Steve", client::AuthKind::Offline, None),
                vec![Step::ExpectClose],
            )
        });
        let events =
            crate::extension::events::told_matching(&told, "meridian:notification/queue/", 2);
        assert_eq!(events[0].0, "meridian:notification/queue/entered");
        assert_eq!(events[0].1["position"], 1);
        assert_eq!(
            events[0].1["player"],
            serde_json::json!({
                "name": "Steve",
                "uuid": hyphenated(offline_uuid("Steve").to_be_bytes()),
                "auth": "unverified"
            })
        );
        assert_eq!(events[1].0, "meridian:notification/queue/left");
        assert_eq!(events[1].1["cause"], "served");
    }

    /// A wait that runs out, and a player who closes the connection while they wait, are told with their own word.
    #[test]
    fn a_wait_that_ends_without_a_place_is_told_with_why() {
        for (wait, steps, cause) in [(1, refused(), "timeout"), (10, vec![Step::Close], "left")] {
            let config = queued(wait, 3);
            let sessions = Sessions::default();
            let told = sessions.extensions.bus().recording(false);
            let _held = sessions.admit(1).unwrap();
            let (connect, backends) = reachable(vec![("127.0.0.1:25601", true)]);
            route_one(
                &config,
                &sessions,
                &*connect,
                &backends,
                logged_in("Notch", client::AuthKind::Premium, None),
                steps,
            );
            let events =
                crate::extension::events::told_matching(&told, "meridian:notification/queue/", 2);
            assert_eq!(
                events[0].0, "meridian:notification/queue/entered",
                "{cause}"
            );
            assert_eq!(events[1].0, "meridian:notification/queue/left", "{cause}");
            assert_eq!(events[1].1["cause"], cause);
            assert_eq!(
                events[0].1["player"],
                serde_json::json!({
                    "name": "Notch",
                    "uuid": hyphenated(offline_uuid("Notch").to_be_bytes()),
                    "auth": "verified"
                }),
                "{cause}"
            );
        }
    }

    /// A reload that raises `max-players` serves the player who waits.
    #[test]
    fn a_reload_that_raises_the_limit_serves_the_queue() {
        let config = queued(10, 3);
        let sessions = Sessions::default();
        let _held = sessions.admit(1).unwrap();
        let (connect, backends) = reachable(vec![("127.0.0.1:25601", true)]);
        let (tried, _) = std::thread::scope(|scope| {
            let sessions = &sessions;
            scope.spawn(move || {
                while sessions.queue.len() == 0 {
                    std::thread::sleep(Duration::from_millis(5));
                }
                sessions.queue.set_max_players(2);
            });
            route_one(
                &config,
                sessions,
                &*connect,
                &backends,
                logged_in("Steve", client::AuthKind::Offline, None),
                vec![Step::ExpectClose],
            )
        });
        assert_eq!(tried, ["127.0.0.1:25601"]);
        assert_eq!(sessions.queue.len(), 0);
    }

    /// The limit counts places as they are taken, not sessions as they open.
    #[test]
    fn the_last_place_is_taken_once() {
        let sessions = Sessions::default();
        let first = sessions.admit(2).unwrap();
        let second = sessions.admit(2).unwrap();
        assert!(sessions.admit(2).is_err());
        drop(first);
        let third = sessions.admit(2).unwrap();
        assert!(sessions.admit(0).is_ok(), "0 is no limit");
        drop((second, third));
    }

    /// The hybrid mode: a verified player goes to the verified group's backend,
    /// an unverified one to the other group's, each never to the other's.
    #[test]
    fn the_hybrid_mode_sends_each_kind_to_its_group() {
        use meridian_config::{AuthKind, AuthMode, Group, Route};
        let config = Config {
            online_mode: true,
            auth_mode: AuthMode::Hybrid,
            groups: vec![
                Group {
                    name: "premium".into(),
                    auth: AuthKind::Verified,
                    backends: vec!["lobby".into()],
                },
                Group {
                    name: "cracked".into(),
                    auth: AuthKind::Unverified,
                    backends: vec!["east".into()],
                },
            ],
            routes: vec![
                Route {
                    auth: AuthKind::Verified,
                    group: "premium".into(),
                },
                Route {
                    auth: AuthKind::Unverified,
                    group: "cracked".into(),
                },
            ],
            ..routed()
        };
        config.validate().unwrap();
        let sessions = Sessions::default();
        let (connect, backends) =
            reachable(vec![("127.0.0.1:25601", true), ("127.0.0.1:25602", true)]);
        let (tried, _) = route_one(
            &config,
            &sessions,
            &*connect,
            &backends,
            logged_in("Notch", client::AuthKind::Premium, None),
            vec![Step::ExpectClose],
        );
        assert_eq!(tried, ["127.0.0.1:25601"]);
        let (tried, _) = route_one(
            &config,
            &sessions,
            &*connect,
            &backends,
            logged_in("guest", client::AuthKind::Offline, None),
            vec![Step::ExpectClose],
        );
        assert_eq!(tried, ["127.0.0.1:25602"]);
    }
}
