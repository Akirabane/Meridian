//! A quiet player for a real server: it connects over TCP, logs in offline,
//! follows the configuration phase, answers what a server needs answered, stays
//! in the world for a number of seconds, and leaves. It is the client of a real
//! run, and it is tested here against the fake backend over a real socket.
//!
//! When the server sends it back to the configuration phase while it plays (Start
//! Configuration, which a proxy does to move a player whose server was lost), it does as
//! a vanilla client does: it acknowledges, sends its information again, goes through the
//! new configuration phase with the code it uses at login, and is back in the world at
//! Finish Configuration. [`Moves`] counts those returns.
//!
//! A session made with [`link`] can also be spoken to and listened to while it runs. The
//! [`Controller`] types a command (`/server east`) or a chat message once the player is in
//! the world, and the [`Journal`] keeps the chat lines the server sends back, as plain
//! text, so that the caller can tell whether the command worked.
use crate::{
    client::FakeClient,
    trace::{Dir, Recorder},
};
use horizon_protocol::{
    connection::Connection,
    ids,
    packets::{Empty, configuration, play},
};
use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{
    Arc, Condvar, Mutex, PoisonError,
    atomic::{AtomicU32, Ordering},
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How many times the server took the player back to the configuration phase and the
/// player came out of it, in the world again. Clones count together, so a test keeps one
/// and reads it while the session runs.
#[derive(Clone, Debug, Default)]
pub struct Moves(Arc<AtomicU32>);

impl Moves {
    /// The returns to the world so far.
    pub fn done(&self) -> u32 {
        self.0.load(Ordering::SeqCst)
    }
}

/// The longest command or chat message the player types, as in the game's chat box.
pub const MAX_TYPED: usize = 256;

/// The most chat lines a [`Journal`] keeps; the oldest are dropped first.
pub const JOURNAL_LINES: usize = 256;

/// Why a command or a message was not sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotSent {
    /// The session has ended, or was never started: nobody is left to send it.
    SessionOver,
    /// More than [`MAX_TYPED`] characters, which the game's chat box does not take either.
    TooLong,
}

/// What the player is told to type.
#[derive(Debug)]
enum Order {
    /// A command, without its slash.
    Command(String),
    /// A chat message.
    Say(String),
}

impl Order {
    /// The packet that carries it. A command goes unsigned, with no argument signatures, as a
    /// client without chat keys sends one; so does a message, with no signature.
    fn encode(&self) -> Vec<u8> {
        match self {
            Order::Command(command) => play::Serverbound::ChatCommand(play::ChatCommand {
                command: command.clone(),
            })
            .encode(),
            Order::Say(message) => play::Serverbound::Chat(play::Chat {
                message: message.clone(),
                timestamp: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |t| t.as_millis() as i64),
                salt: 0,
                signature: None,
                last_seen: play::LastSeenUpdate {
                    offset: 0,
                    acknowledged: [0, 0, 0],
                    checksum: 0,
                },
            })
            .encode(),
        }
    }
}

/// Types for the player of a running session. Clones type for the same player, from any
/// thread. What is typed is sent in the order it was given, once the player is in the world;
/// while the server has the player in a configuration phase, it waits for the world.
#[derive(Clone, Debug)]
pub struct Controller {
    orders: Sender<Order>,
}

impl Controller {
    /// Types a command. A leading slash is dropped: `command("/server east")` and
    /// `command("server east")` send the same packet.
    pub fn command(&self, text: &str) -> Result<(), NotSent> {
        let text = text.strip_prefix('/').unwrap_or(text);
        self.send(text, Order::Command)
    }

    /// Says something in the chat.
    pub fn say(&self, text: &str) -> Result<(), NotSent> {
        self.send(text, Order::Say)
    }

    fn send(&self, text: &str, order: fn(String) -> Order) -> Result<(), NotSent> {
        if text.chars().count() > MAX_TYPED {
            return Err(NotSent::TooLong);
        }
        self.orders
            .send(order(text.to_owned()))
            .map_err(|_| NotSent::SessionOver)
    }
}

/// A chat line the server sent to the player.
#[derive(Clone, Debug, PartialEq)]
pub struct Said {
    /// The number of the line, from 0, for [`Journal::mark`] and [`Journal::wait_for`].
    pub seq: u64,
    /// The time since the journal was made.
    pub at: Duration,
    /// The text of the line, without colours or formatting. A translated line with no known
    /// translation shows its key.
    pub text: String,
    /// Whether the line is for the action bar above the hotbar rather than the chat.
    pub overlay: bool,
}

#[derive(Debug, Default)]
struct Log {
    lines: VecDeque<Said>,
    next: u64,
}

/// The chat lines the server sent to the player ("system chat", which is what a proxy's and
/// a server's answers to a command are), the last [`JOURNAL_LINES`] of them. Clones read the
/// same lines, from any thread.
#[derive(Clone, Debug)]
pub struct Journal {
    shared: Arc<(Mutex<Log>, Condvar)>,
    made: Instant,
}

impl Default for Journal {
    fn default() -> Self {
        Self {
            shared: Arc::default(),
            made: Instant::now(),
        }
    }
}

impl Journal {
    fn push(&self, text: String, overlay: bool) {
        let (lock, condvar) = &*self.shared;
        let mut log = lock.lock().unwrap_or_else(PoisonError::into_inner);
        let line = Said {
            seq: log.next,
            at: self.made.elapsed(),
            text,
            overlay,
        };
        log.next += 1;
        if log.lines.len() == JOURNAL_LINES {
            log.lines.pop_front();
        }
        log.lines.push_back(line);
        condvar.notify_all();
    }

    /// The lines kept, oldest first.
    pub fn lines(&self) -> Vec<Said> {
        let (lock, _) = &*self.shared;
        let log = lock.lock().unwrap_or_else(PoisonError::into_inner);
        log.lines.iter().cloned().collect()
    }

    /// The number the next line will have. Take it before typing a command, then pass it to
    /// [`Journal::wait_for`] to look only at what came after.
    pub fn mark(&self) -> u64 {
        let (lock, _) = &*self.shared;
        lock.lock().unwrap_or_else(PoisonError::into_inner).next
    }

    /// Waits up to `timeout` for a chat line (not an action bar one) numbered `since` or later
    /// whose text satisfies `wanted`, and returns it. A line already there counts.
    pub fn wait_for(
        &self,
        since: u64,
        timeout: Duration,
        wanted: impl Fn(&str) -> bool,
    ) -> Option<Said> {
        let deadline = Instant::now() + timeout;
        let (lock, condvar) = &*self.shared;
        let mut log = lock.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some(line) = log
                .lines
                .iter()
                .find(|l| l.seq >= since && !l.overlay && wanted(&l.text))
            {
                return Some(line.clone());
            }
            let left = deadline.checked_duration_since(Instant::now())?;
            log = condvar
                .wait_timeout(log, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

/// What the caller keeps of a session made with [`link`].
#[derive(Clone, Debug)]
pub struct Handle {
    /// Types commands and messages for the player.
    pub controller: Controller,
    /// The chat lines the server sent.
    pub journal: Journal,
    /// The returns to the world after a configuration phase the server began.
    pub moves: Moves,
}

/// The session's side of a [`Handle`]: pass it to [`session_linked`].
#[derive(Debug)]
pub struct Link {
    orders: Receiver<Order>,
    journal: Journal,
    moves: Moves,
}

/// Makes the two ends of a controlled session: the [`Handle`] for the caller and the [`Link`]
/// for [`session_linked`].
pub fn link() -> (Handle, Link) {
    let (sender, orders) = channel();
    let (journal, moves) = (Journal::default(), Moves::default());
    (
        Handle {
            controller: Controller { orders: sender },
            journal: journal.clone(),
            moves: moves.clone(),
        },
        Link {
            orders,
            journal,
            moves,
        },
    )
}

/// One thing to type, and when: the time since `started` in [`start_script`].
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub at: Duration,
    /// A command if it starts with a slash, a chat message if not.
    pub text: String,
}

/// Reads a script such as `10:/server game1;190:/hub`: entries separated by semicolons, each
/// the time in seconds (decimals allowed), a colon, and the text. The entries may come in any
/// order; they are sorted by time, and two at the same time stay in the order they were
/// written in. The text of an entry may hold colons but no semicolon.
pub fn parse_script(text: &str) -> Result<Vec<Entry>, String> {
    let mut entries = Vec::new();
    for entry in text.split(';').map(str::trim).filter(|e| !e.is_empty()) {
        let (at, typed) = entry
            .split_once(':')
            .ok_or(format!("script entry {entry:?} is not TIME:TEXT"))?;
        let at: f64 = at
            .trim()
            .parse()
            .ok()
            .filter(|t: &f64| t.is_finite() && (0.0..1e9).contains(t))
            .ok_or(format!("script entry {entry:?} has no valid time"))?;
        let typed = typed.trim();
        if typed.is_empty() || typed == "/" {
            return Err(format!("script entry {entry:?} has nothing to type"));
        }
        entries.push(Entry {
            at: Duration::from_secs_f64(at),
            text: typed.to_owned(),
        });
    }
    if entries.is_empty() {
        return Err("the script has no entry".into());
    }
    entries.sort_by_key(|e| e.at);
    Ok(entries)
}

/// What a [`Typist`] did with an entry.
#[derive(Clone, Debug, PartialEq)]
pub struct Typed {
    /// The time since `started`, when it was handed to the controller.
    pub at: Duration,
    pub text: String,
    pub result: Result<(), NotSent>,
}

/// A thread that hands the entries of a script to a [`Controller`] when they are due.
#[derive(Debug)]
pub struct Typist {
    stop: Sender<()>,
    thread: std::thread::JoinHandle<Vec<Typed>>,
}

/// Starts typing `entries` through `controller`, each when `started` is as old as its time.
/// A text that starts with a slash is typed as a command, any other is said in the chat. The
/// time runs from `started`, not from the world: what is due before the player is in the
/// world waits for it, as the [`Controller`] does.
pub fn start_script(controller: Controller, entries: Vec<Entry>, started: Instant) -> Typist {
    let (stop, stopped) = channel::<()>();
    let thread = std::thread::spawn(move || {
        let mut done = Vec::new();
        for Entry { at, text } in entries {
            let wait = at.saturating_sub(started.elapsed());
            if stopped.recv_timeout(wait) != Err(std::sync::mpsc::RecvTimeoutError::Timeout) {
                break;
            }
            let result = if text.starts_with('/') {
                controller.command(&text)
            } else {
                controller.say(&text)
            };
            done.push(Typed {
                at: started.elapsed(),
                text,
                result,
            });
        }
        done
    });
    Typist { stop, thread }
}

impl Typist {
    /// Stops typing, if the script is not finished, and returns what was typed.
    pub fn finish(self) -> Vec<Typed> {
        drop(self.stop);
        self.thread.join().unwrap_or_default()
    }
}

/// What a script typed and what the server said, as one line each in time order: `SENT +10.0s
/// /server east` and `CHAT +10.2s You are now on east.`, the time being the one since the start.
/// Action bar lines and empty ones are left out. Also returns the number of chat lines.
pub fn transcript(typed: &[Typed], journal: &Journal) -> (Vec<String>, usize) {
    let mut events: Vec<(Duration, String)> = Vec::new();
    for t in typed {
        let how = match t.result {
            Ok(()) => String::new(),
            Err(why) => format!(" (not sent: {why:?})"),
        };
        events.push((
            t.at,
            format!("SENT +{:.1}s {}{how}", t.at.as_secs_f32(), t.text),
        ));
    }
    let said: Vec<Said> = journal
        .lines()
        .into_iter()
        .filter(|l| !l.overlay && !l.text.trim().is_empty())
        .collect();
    for line in &said {
        // On one line, so that the output stays easy to search.
        let text = line.text.replace('\n', " ");
        events.push((
            line.at,
            format!("CHAT +{:.1}s {text}", line.at.as_secs_f32()),
        ));
    }
    events.sort_by_key(|e| e.0);
    (events.into_iter().map(|e| e.1).collect(), said.len())
}

/// Where to connect, who to be, and for how long.
#[derive(Clone, Debug)]
pub struct Options {
    pub host: String,
    pub port: u16,
    pub name: String,
    /// Seconds to stay in the world once the configuration is done.
    pub seconds: u64,
}

/// How a session ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum End {
    /// The client stayed its time and left.
    Clean,
    /// The server sent a disconnect, or refused the login; the text follows.
    Disconnect(String),
    /// The connection was closed with no packet.
    Closed,
    /// Something else went wrong.
    Error(String),
}

/// Runs one session. Every packet the server sent after the login is recorded
/// as going to the client. Three seconds into the world the player types `list`.
pub fn session(o: &Options, recorder: &Recorder) -> End {
    session_counting(o, recorder, &Moves::default())
}

/// As [`session`], and counts in `moves` each return from a configuration phase the server
/// began while the player was in the world.
pub fn session_counting(o: &Options, recorder: &Recorder, moves: &Moves) -> End {
    run(o, recorder, moves, None)
}

/// As [`session`], for a session made with [`link`]: the player types what the
/// [`Controller`] is given, the [`Journal`] gets the chat lines, and the returns to the world
/// are counted in the [`Handle`]'s `moves`. The player types nothing of its own, not even
/// the `list` of the other sessions.
pub fn session_linked(o: &Options, recorder: &Recorder, link: &Link) -> End {
    run(o, recorder, &link.moves, Some(link))
}

fn run(o: &Options, recorder: &Recorder, moves: &Moves, link: Option<&Link>) -> End {
    let mut connection = match Connection::connect(&o.host, o.port, Duration::from_secs(5)) {
        Ok(c) => c,
        Err(e) => return End::Error(format!("cannot connect: {e}")),
    };
    let client = FakeClient {
        host: o.host.clone(),
        port: o.port,
        ..FakeClient::named(&o.name)
    };
    // The login only: the script after it is empty.
    if let Err(e) = client.run(&mut connection, recorder) {
        return End::Disconnect(e);
    }
    let _ = connection
        .reader
        .set_read_timeout(Some(Duration::from_millis(100)));
    let send = |connection: &mut Connection, body: Vec<u8>| connection.send(&body);
    if send(&mut connection, client_information()).is_err() {
        return End::Closed;
    }
    let mut playing = false;
    // The configuration phase the server began while the player played, not the first.
    let mut reconfiguring = false;
    let mut play_started = Instant::now();
    let mut said = false;
    loop {
        if playing {
            let elapsed = play_started.elapsed();
            if elapsed >= Duration::from_secs(o.seconds) {
                connection.writer.shutdown();
                return End::Clean;
            }
            // What the caller asked to type, in order.
            if let Some(link) = link {
                while let Ok(order) = link.orders.try_recv() {
                    if send(&mut connection, order.encode()).is_err() {
                        return End::Closed;
                    }
                }
            }
            if link.is_none() && !said && elapsed >= Duration::from_secs(3) {
                said = true;
                let body = play::Serverbound::ChatCommand(play::ChatCommand {
                    command: "list".into(),
                })
                .encode();
                if send(&mut connection, body).is_err() {
                    return End::Closed;
                }
            }
        }
        let (id, payload) = match connection.recv() {
            Ok(p) => p,
            Err(e) if e.is_timed_out() => continue,
            Err(e) if e.is_end_of_stream() => return End::Closed,
            Err(e) => return End::Error(e.to_string()),
        };
        recorder.record(Dir::ToClient, id, &payload);
        let reply = if playing {
            match id {
                ids::play::clientbound::KEEP_ALIVE => match play::Clientbound::decode(id, &payload)
                {
                    Ok(play::Clientbound::KeepAlive(k)) => Some(
                        play::Serverbound::KeepAlive(configuration::KeepAlive { id: k.id })
                            .encode(),
                    ),
                    _ => None,
                },
                ids::play::clientbound::PING => match play::Clientbound::decode(id, &payload) {
                    Ok(play::Clientbound::Ping(k)) => {
                        Some(play::Serverbound::Pong(configuration::Ping { id: k.id }).encode())
                    }
                    _ => None,
                },
                ids::play::clientbound::CHUNK_BATCH_FINISHED => Some(
                    play::Serverbound::ChunkBatchReceived(play::ChunkBatchReceived {
                        chunks_per_tick: 9.0,
                    })
                    .encode(),
                ),
                ids::play::clientbound::PLAYER_POSITION => {
                    match play::Clientbound::decode(id, &payload) {
                        Ok(play::Clientbound::PlayerPosition(p)) => {
                            let accept =
                                play::Serverbound::AcceptTeleportation(play::AcceptTeleportation {
                                    id: p.teleport,
                                    x: p.position[0],
                                    y: p.position[1],
                                    z: p.position[2],
                                    yaw: p.yaw,
                                    pitch: p.pitch,
                                })
                                .encode();
                            if send(&mut connection, accept).is_err() {
                                return End::Closed;
                            }
                            Some(play::Serverbound::PlayerLoaded(Empty).encode())
                        }
                        _ => None,
                    }
                }
                ids::play::clientbound::SYSTEM_CHAT => {
                    if let (Some(link), Ok(play::Clientbound::SystemChat(chat))) =
                        (link, play::Clientbound::decode(id, &payload))
                    {
                        let text = horizon_protocol::text::plain(&chat.content, &HashMap::new());
                        link.journal.push(text, chat.overlay);
                    }
                    None
                }
                ids::play::clientbound::DISCONNECT => {
                    return End::Disconnect(disconnect_text(&payload));
                }
                // The server takes the player back to the configuration phase: the
                // acknowledgement is the last thing sent as a player, and the information
                // is the first thing sent in the new phase, as a vanilla client does.
                ids::play::clientbound::START_CONFIGURATION => {
                    playing = false;
                    reconfiguring = true;
                    if send(
                        &mut connection,
                        play::Serverbound::ConfigurationAcknowledged(Empty).encode(),
                    )
                    .is_err()
                    {
                        return End::Closed;
                    }
                    Some(client_information())
                }
                _ => None,
            }
        } else {
            match id {
                ids::configuration::clientbound::KEEP_ALIVE => {
                    match configuration::Clientbound::decode(id, &payload) {
                        Ok(configuration::Clientbound::KeepAlive(k)) => Some(
                            configuration::Serverbound::KeepAlive(configuration::KeepAlive {
                                id: k.id,
                            })
                            .encode(),
                        ),
                        _ => None,
                    }
                }
                ids::configuration::clientbound::PING => {
                    match configuration::Clientbound::decode(id, &payload) {
                        Ok(configuration::Clientbound::Ping(k)) => Some(
                            configuration::Serverbound::Pong(configuration::Ping { id: k.id })
                                .encode(),
                        ),
                        _ => None,
                    }
                }
                ids::configuration::clientbound::SELECT_KNOWN_PACKS => Some(
                    configuration::Serverbound::SelectKnownPacks(configuration::SelectKnownPacks {
                        packs: Vec::new(),
                    })
                    .encode(),
                ),
                ids::configuration::clientbound::FINISH_CONFIGURATION => {
                    playing = true;
                    if std::mem::take(&mut reconfiguring) {
                        moves.0.fetch_add(1, Ordering::SeqCst);
                    } else {
                        play_started = Instant::now();
                    }
                    Some(configuration::Serverbound::FinishConfiguration(Empty).encode())
                }
                ids::configuration::clientbound::DISCONNECT => {
                    return End::Disconnect(disconnect_text(&payload));
                }
                _ => None,
            }
        };
        if let Some(body) = reply
            && send(&mut connection, body).is_err()
        {
            return End::Closed;
        }
    }
}

/// The payload of the Client Information the player sends, for a fake server that expects
/// exactly what a proxy replays from the client.
pub fn information_payload() -> Vec<u8> {
    let body = client_information();
    // The id of the packet is the one byte before the payload.
    body[1..].to_vec()
}

/// The client's settings, which it sends at the start of each configuration phase.
fn client_information() -> Vec<u8> {
    configuration::Serverbound::ClientInformation(configuration::ClientInformation::new(4)).encode()
}

/// The text of a disconnect packet's reason.
fn disconnect_text(payload: &[u8]) -> String {
    match configuration::Clientbound::decode(ids::configuration::clientbound::DISCONNECT, payload) {
        Ok(configuration::Clientbound::Disconnect(d)) => {
            horizon_protocol::text::plain(&d.reason, &std::collections::HashMap::new())
        }
        _ => format!("{} undecoded bytes", payload.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::FakeBackend;
    use std::net::TcpListener;

    /// The player logs in to a fake backend over a real socket, answers its
    /// configuration and a keep-alive, and leaves cleanly after its time.
    #[test]
    fn a_quiet_player_stays_and_leaves_over_tcp() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Recorder::new();
        let backend = {
            let seen = seen.clone();
            std::thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                let mut connection = Connection::from_stream(stream).unwrap();
                FakeBackend::default()
                    .run(&mut connection, &seen)
                    .map_err(|e| format!("login: {e}"))?;
                connection
                    .reader
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .map_err(|e| e.to_string())?;
                // The configuration phase: the player's information first.
                let mut ids_seen = Vec::new();
                let (id, _) = connection.recv().map_err(|e| e.to_string())?;
                ids_seen.push(id);
                connection
                    .send(&crate::body(
                        ids::configuration::clientbound::FINISH_CONFIGURATION,
                        &[],
                    ))
                    .map_err(|e| e.to_string())?;
                let (id, _) = connection.recv().map_err(|e| e.to_string())?;
                ids_seen.push(id);
                // The play phase: a keep-alive that must come back.
                let mut keep_alive = Vec::new();
                keep_alive.extend_from_slice(&7_i64.to_be_bytes());
                connection
                    .send(&crate::body(
                        ids::play::clientbound::KEEP_ALIVE,
                        &keep_alive,
                    ))
                    .map_err(|e| e.to_string())?;
                // Everything else until the player closes the connection.
                loop {
                    match connection.recv() {
                        Ok((id, payload)) => {
                            if id == ids::play::serverbound::KEEP_ALIVE {
                                if payload != keep_alive {
                                    return Err("the keep-alive came back changed".to_owned());
                                }
                                ids_seen.push(id);
                            }
                        }
                        Err(e) if e.is_end_of_stream() => return Ok(ids_seen),
                        Err(e) => return Err(e.to_string()),
                    }
                }
            })
        };
        let recorder = Recorder::new();
        let end = session(
            &Options {
                host: "127.0.0.1".into(),
                port,
                name: "Quiet".into(),
                seconds: 1,
            },
            &recorder,
        );
        assert_eq!(end, End::Clean);
        let ids_seen = backend.join().unwrap().unwrap();
        assert_eq!(
            ids_seen,
            vec![
                ids::configuration::serverbound::CLIENT_INFORMATION,
                ids::configuration::serverbound::FINISH_CONFIGURATION,
                ids::play::serverbound::KEEP_ALIVE,
            ]
        );
        assert!(recorder.trace().count(Dir::ToClient) >= 2);
    }

    /// The server takes the player back to the configuration phase while they play, as a
    /// proxy does to move a player whose server was lost: the player acknowledges, sends
    /// its information, answers the new configuration, and plays again, still answering
    /// keep-alives.
    #[test]
    fn a_player_goes_back_to_configuration_when_the_server_says_so_and_plays_again() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let backend = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut connection = Connection::from_stream(stream).unwrap();
            FakeBackend::default()
                .run(&mut connection, &Recorder::new())
                .map_err(|e| format!("login: {e}"))?;
            connection
                .reader
                .set_read_timeout(Some(Duration::from_secs(5)))
                .map_err(|e| e.to_string())?;
            let send = |connection: &mut Connection, id: i32, payload: &[u8]| {
                connection
                    .send(&crate::body(id, payload))
                    .map_err(|e| e.to_string())
            };
            let mut seen = Vec::new();
            let mut next = |connection: &mut Connection| -> Result<i32, String> {
                let (id, _) = connection.recv().map_err(|e| e.to_string())?;
                seen.push(id);
                Ok(id)
            };
            // The first configuration phase and the world.
            next(&mut connection)?;
            send(
                &mut connection,
                ids::configuration::clientbound::FINISH_CONFIGURATION,
                &[],
            )?;
            next(&mut connection)?;
            // The server takes the player back to the configuration phase.
            send(
                &mut connection,
                ids::play::clientbound::START_CONFIGURATION,
                &[],
            )?;
            next(&mut connection)?; // the acknowledgement
            next(&mut connection)?; // the information
            send(
                &mut connection,
                ids::configuration::clientbound::SELECT_KNOWN_PACKS,
                &[0],
            )?;
            next(&mut connection)?; // the packs
            send(
                &mut connection,
                ids::configuration::clientbound::FINISH_CONFIGURATION,
                &[],
            )?;
            next(&mut connection)?;
            // The world again: a keep-alive must come back.
            send(
                &mut connection,
                ids::play::clientbound::KEEP_ALIVE,
                &8_i64.to_be_bytes(),
            )?;
            next(&mut connection)?;
            loop {
                match connection.recv() {
                    Err(e) if e.is_end_of_stream() => return Ok(seen),
                    Err(e) => return Err(e.to_string()),
                    Ok(_) => {}
                }
            }
        });
        let moves = Moves::default();
        let end = session_counting(
            &Options {
                host: "127.0.0.1".into(),
                port,
                name: "Quiet".into(),
                seconds: 1,
            },
            &Recorder::new(),
            &moves,
        );
        assert_eq!(end, End::Clean);
        assert_eq!(moves.done(), 1);
        assert_eq!(
            backend.join().unwrap().unwrap(),
            vec![
                ids::configuration::serverbound::CLIENT_INFORMATION,
                ids::configuration::serverbound::FINISH_CONFIGURATION,
                ids::play::serverbound::CONFIGURATION_ACKNOWLEDGED,
                ids::configuration::serverbound::CLIENT_INFORMATION,
                ids::configuration::serverbound::SELECT_KNOWN_PACKS,
                ids::configuration::serverbound::FINISH_CONFIGURATION,
                ids::play::serverbound::KEEP_ALIVE,
            ]
        );
    }

    /// A server that is not there is reported, not waited for. Port 0 is one
    /// nothing can listen on, so no other test can take it in the meantime, as it
    /// could a port freed for the occasion; the system refuses it at once.
    #[test]
    fn a_closed_port_is_an_error() {
        let port = 0;
        let end = session(
            &Options {
                host: "127.0.0.1".into(),
                port,
                name: "Quiet".into(),
                seconds: 1,
            },
            &Recorder::new(),
        );
        assert!(
            matches!(&end, End::Error(why) if why.starts_with("cannot connect")),
            "{end:?}"
        );
    }

    // ---- The controlled player.

    type Seen = Vec<(i32, Vec<u8>)>;

    fn options(port: u16) -> Options {
        Options {
            host: "127.0.0.1".into(),
            port,
            name: "Bot".into(),
            seconds: 1,
        }
    }

    fn chat_line(text: &str, overlay: bool) -> Vec<u8> {
        play::Clientbound::SystemChat(play::SystemChat {
            content: horizon_protocol::text::literal(text),
            overlay,
        })
        .encode()
    }

    /// The payload of an encoded packet: its body without the packet id, which is one byte
    /// for the ids used here.
    fn payload_of(body: Vec<u8>) -> Vec<u8> {
        body[1..].to_vec()
    }

    /// A fake server, over a real socket, that logs the player in, ends the configuration,
    /// says "Welcome" in the chat and a line in the action bar, answers every command with
    /// the chat line "ran /<command>", and returns every packet it received after the login.
    fn server(listener: TcpListener) -> std::thread::JoinHandle<Result<Seen, String>> {
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut connection = Connection::from_stream(stream).unwrap();
            FakeBackend::default()
                .run(&mut connection, &Recorder::new())
                .map_err(|e| format!("login: {e}"))?;
            connection
                .reader
                .set_read_timeout(Some(Duration::from_secs(10)))
                .map_err(|e| e.to_string())?;
            let mut seen = Seen::new();
            let mut next = |connection: &mut Connection| -> Result<i32, String> {
                let (id, payload) = connection.recv().map_err(|e| e.to_string())?;
                seen.push((id, payload));
                Ok(id)
            };
            next(&mut connection)?;
            connection
                .send(&crate::body(
                    ids::configuration::clientbound::FINISH_CONFIGURATION,
                    &[],
                ))
                .map_err(|e| e.to_string())?;
            next(&mut connection)?;
            for (text, overlay) in [("Welcome", false), ("Action bar", true)] {
                connection
                    .send(&chat_line(text, overlay))
                    .map_err(|e| e.to_string())?;
            }
            loop {
                match connection.recv() {
                    Ok((id, payload)) => {
                        if id == ids::play::serverbound::CHAT_COMMAND
                            && let Ok(play::Serverbound::ChatCommand(c)) =
                                play::Serverbound::decode(id, &payload)
                        {
                            connection
                                .send(&chat_line(&format!("ran /{}", c.command), false))
                                .map_err(|e| e.to_string())?;
                        }
                        seen.push((id, payload));
                    }
                    Err(e) if e.is_end_of_stream() => return Ok(seen),
                    Err(e) => return Err(e.to_string()),
                }
            }
        })
    }

    /// Commands and a message typed by the caller reach the server as the packets of a
    /// client without chat keys, in the order given and only once the player is in the
    /// world, and the server's answers come back as lines of text.
    #[test]
    fn typed_commands_and_messages_reach_the_server_in_order_and_its_lines_come_back() {
        use ids::play::serverbound as sb;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = server(listener);
        let (handle, link) = link();
        // Typed before the session starts: it waits for the world.
        let typed = &handle.controller;
        assert_eq!(typed.command("/server game1"), Ok(()));
        assert_eq!(typed.say("hello there"), Ok(()));
        assert_eq!(typed.command("hub"), Ok(()));
        let player =
            std::thread::spawn(move || session_linked(&options(port), &Recorder::new(), &link));

        let wait = |text: &str| {
            handle
                .journal
                .wait_for(0, Duration::from_secs(10), |line| line == text)
        };
        assert!(
            wait("ran /server game1").is_some(),
            "{:?}",
            handle.journal.lines()
        );
        assert!(wait("ran /hub").is_some(), "{:?}", handle.journal.lines());
        assert_eq!(player.join().unwrap(), End::Clean);

        let seen = server.join().unwrap().unwrap();
        // The information, the end of the configuration, then what was typed, in order,
        // and nothing of the player's own.
        let ids_seen: Vec<i32> = seen.iter().map(|(id, _)| *id).collect();
        assert_eq!(
            ids_seen,
            [
                ids::configuration::serverbound::CLIENT_INFORMATION,
                ids::configuration::serverbound::FINISH_CONFIGURATION,
                sb::CHAT_COMMAND,
                sb::CHAT,
                sb::CHAT_COMMAND,
            ]
        );
        // The command: the unsigned packet, the text without its slash, to the byte.
        let command = |text: &str| {
            payload_of(
                play::Serverbound::ChatCommand(play::ChatCommand {
                    command: text.into(),
                })
                .encode(),
            )
        };
        assert_eq!(seen[2].1, command("server game1"));
        assert_eq!(seen[4].1, command("hub"));
        // The message: unsigned, with the text as given.
        match play::Serverbound::decode(sb::CHAT, &seen[3].1) {
            Ok(play::Serverbound::Chat(chat)) => {
                assert_eq!(chat.message, "hello there");
                assert_eq!(chat.signature, None);
                assert!(chat.timestamp > 1_700_000_000_000, "{}", chat.timestamp);
            }
            other => panic!("{other:?}"),
        }
        // What the server said, as text, with the action bar line marked.
        let lines: Vec<(String, bool)> = handle
            .journal
            .lines()
            .into_iter()
            .map(|l| (l.text, l.overlay))
            .collect();
        assert_eq!(
            lines,
            [
                ("Welcome".to_owned(), false),
                ("Action bar".to_owned(), true),
                ("ran /server game1".to_owned(), false),
                ("ran /hub".to_owned(), false),
            ]
        );
    }

    /// Something typed while the server has the player in a configuration phase waits for
    /// the world: a client in that phase cannot send a command.
    #[test]
    fn what_is_typed_during_a_configuration_phase_waits_for_the_world() {
        use ids::play::serverbound as sb;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (handle, link) = link();
        let (back_in_configuration, told) = channel();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut connection = Connection::from_stream(stream).unwrap();
            FakeBackend::default()
                .run(&mut connection, &Recorder::new())
                .map_err(|e| format!("login: {e}"))?;
            connection
                .reader
                .set_read_timeout(Some(Duration::from_secs(10)))
                .map_err(|e| e.to_string())?;
            let mut seen = Vec::new();
            let mut next = |connection: &mut Connection| -> Result<i32, String> {
                let (id, _) = connection.recv().map_err(|e| e.to_string())?;
                seen.push(id);
                Ok(id)
            };
            next(&mut connection)?;
            connection
                .send(&crate::body(
                    ids::configuration::clientbound::FINISH_CONFIGURATION,
                    &[],
                ))
                .map_err(|e| e.to_string())?;
            next(&mut connection)?;
            connection
                .send(&crate::body(
                    ids::play::clientbound::START_CONFIGURATION,
                    &[],
                ))
                .map_err(|e| e.to_string())?;
            next(&mut connection)?; // the acknowledgement
            next(&mut connection)?; // the information
            // The player is in the new configuration phase: the caller types now.
            back_in_configuration.send(()).unwrap();
            connection
                .reader
                .set_read_timeout(Some(Duration::from_millis(600)))
                .map_err(|e| e.to_string())?;
            match connection.recv() {
                Err(e) if e.is_timed_out() => {}
                Ok((id, _)) => return Err(format!("the player sent {id} in the configuration")),
                Err(e) => return Err(e.to_string()),
            }
            connection
                .reader
                .set_read_timeout(Some(Duration::from_secs(10)))
                .map_err(|e| e.to_string())?;
            connection
                .send(&crate::body(
                    ids::configuration::clientbound::FINISH_CONFIGURATION,
                    &[],
                ))
                .map_err(|e| e.to_string())?;
            next(&mut connection)?; // the end of the configuration
            next(&mut connection)?; // what was typed
            Ok(seen)
        });
        let player =
            std::thread::spawn(move || session_linked(&options(port), &Recorder::new(), &link));
        told.recv_timeout(Duration::from_secs(10)).unwrap();
        handle.controller.command("late").unwrap();
        let seen = server.join().unwrap().unwrap();
        assert_eq!(
            seen,
            [
                ids::configuration::serverbound::CLIENT_INFORMATION,
                ids::configuration::serverbound::FINISH_CONFIGURATION,
                sb::CONFIGURATION_ACKNOWLEDGED,
                ids::configuration::serverbound::CLIENT_INFORMATION,
                ids::configuration::serverbound::FINISH_CONFIGURATION,
                sb::CHAT_COMMAND,
            ]
        );
        assert_eq!(handle.moves.done(), 1);
        let _ = player.join();
    }

    #[test]
    fn a_slash_is_dropped_the_length_is_bounded_and_a_finished_session_refuses() {
        let (handle, link) = link();
        let typed = &handle.controller;
        assert_eq!(typed.command("/a b"), Ok(()));
        assert_eq!(typed.command("a b"), Ok(()));
        assert_eq!(typed.say("/not a command"), Ok(()));
        let sent: Vec<String> = std::iter::from_fn(|| link.orders.try_recv().ok())
            .map(|o| format!("{o:?}"))
            .collect();
        assert_eq!(
            sent,
            [
                r#"Command("a b")"#,
                r#"Command("a b")"#,
                r#"Say("/not a command")"#
            ]
        );
        assert_eq!(typed.say(&"x".repeat(MAX_TYPED)), Ok(()));
        assert_eq!(typed.say(&"x".repeat(MAX_TYPED + 1)), Err(NotSent::TooLong));
        assert_eq!(
            typed.command(&"é".repeat(MAX_TYPED + 1)),
            Err(NotSent::TooLong)
        );
        drop(link);
        assert_eq!(typed.command("list"), Err(NotSent::SessionOver));
        assert_eq!(typed.say("hi"), Err(NotSent::SessionOver));
    }

    #[test]
    fn the_journal_keeps_the_last_lines_and_waits_for_a_new_one() {
        let journal = Journal::default();
        for n in 0..JOURNAL_LINES + 44 {
            journal.push(format!("line {n}"), false);
        }
        let lines = journal.lines();
        assert_eq!(lines.len(), JOURNAL_LINES);
        assert_eq!(lines[0].seq, 44);
        assert_eq!(lines[0].text, "line 44");
        assert_eq!(journal.mark(), (JOURNAL_LINES + 44) as u64);

        // Only what came after the mark counts, and an action bar line does not.
        let mark = journal.mark();
        assert_eq!(
            journal.wait_for(mark, Duration::from_millis(20), |t| t.starts_with("line")),
            None
        );
        journal.push("bar".into(), true);
        assert_eq!(
            journal.wait_for(mark, Duration::from_millis(20), |t| t == "bar"),
            None
        );
        let late = {
            let journal = journal.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(50));
                journal.push("You are now on east.".into(), false);
            })
        };
        let said = journal
            .wait_for(mark, Duration::from_secs(10), |t| t.contains("now on"))
            .unwrap();
        assert_eq!(said.text, "You are now on east.");
        assert!(!said.overlay);
        late.join().unwrap();
        // A line already there is found at once.
        assert!(
            journal
                .wait_for(0, Duration::ZERO, |t| t == "line 299")
                .is_some()
        );
    }

    #[test]
    fn a_script_is_read_sorted_and_checked() {
        let entry = |at: f64, text: &str| Entry {
            at: Duration::from_secs_f64(at),
            text: text.into(),
        };
        assert_eq!(
            parse_script("190:/hub; 10:/server game1 ;10.5:hello: there;10:/second at 10").unwrap(),
            [
                entry(10.0, "/server game1"),
                entry(10.0, "/second at 10"),
                entry(10.5, "hello: there"),
                entry(190.0, "/hub"),
            ]
        );
        assert_eq!(parse_script("0:/list;").unwrap(), [entry(0.0, "/list")]);
        for (bad, why) in [
            ("", "no entry"),
            (" ; ", "no entry"),
            ("/hub", "TIME:TEXT"),
            ("soon:/hub", "no valid time"),
            ("-1:/hub", "no valid time"),
            ("NaN:/hub", "no valid time"),
            ("inf:/hub", "no valid time"),
            ("5:", "nothing to type"),
            ("5:/", "nothing to type"),
        ] {
            let error = parse_script(bad).unwrap_err();
            assert!(error.contains(why), "{bad:?}: {error}");
        }
    }

    /// The typist hands over each entry when it is due, a command or a message by its first
    /// character, and stops when told to, leaving what is not due.
    #[test]
    fn the_typist_types_each_entry_when_due_and_stops_when_told() {
        let (handle, link) = link();
        let entries = parse_script("0:/first;0.05:second;0.1:/third;3600:/never").unwrap();
        let typist = start_script(handle.controller.clone(), entries, Instant::now());
        // The three due ones arrive in order; the fourth is an hour away.
        let mut sent = Vec::new();
        while sent.len() < 3 {
            sent.push(format!(
                "{:?}",
                link.orders.recv_timeout(Duration::from_secs(10)).unwrap()
            ));
        }
        assert_eq!(
            sent,
            [
                r#"Command("first")"#,
                r#"Say("second")"#,
                r#"Command("third")"#
            ]
        );
        let typed = typist.finish();
        assert_eq!(typed.len(), 3);
        assert!(typed.iter().all(|t| t.result == Ok(())));
        assert_eq!(
            typed.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(),
            ["/first", "second", "/third"]
        );
        assert!(typed[2].at >= Duration::from_millis(100));
        assert!(
            link.orders.try_recv().is_err(),
            "the last entry was not due"
        );
    }

    #[test]
    fn the_transcript_puts_what_was_typed_and_what_was_said_in_time_order() {
        let journal = Journal::default();
        std::thread::sleep(Duration::from_millis(60));
        journal.push("Moving you to east...".into(), false);
        journal.push("bar".into(), true);
        journal.push("  ".into(), false);
        journal.push("two\nlines".into(), false);
        let typed = [
            Typed {
                at: Duration::from_millis(10),
                text: "/server east".into(),
                result: Ok(()),
            },
            Typed {
                at: Duration::from_secs(3600),
                text: "x".repeat(300),
                result: Err(NotSent::TooLong),
            },
        ];
        let (lines, said) = transcript(&typed, &journal);
        assert_eq!(said, 2);
        assert_eq!(lines.len(), 4, "{lines:?}");
        assert_eq!(lines[0], "SENT +0.0s /server east");
        assert!(lines[1].starts_with("CHAT +0.0") || lines[1].starts_with("CHAT +0.1"));
        assert!(lines[1].ends_with(" Moving you to east..."), "{}", lines[1]);
        assert!(lines[2].ends_with(" two lines"), "{}", lines[2]);
        assert!(lines[3].starts_with("SENT +3600.0s xxx"));
        assert!(lines[3].ends_with(" (not sent: TooLong)"));
    }

    /// A line that is a translated component shows its key when no translation is known,
    /// and the pieces of a line with siblings are joined.
    #[test]
    fn a_line_is_flattened_to_plain_text() {
        use horizon_protocol::text;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut connection = Connection::from_stream(stream).unwrap();
            FakeBackend::default()
                .run(&mut connection, &Recorder::new())
                .map_err(|e| format!("login: {e}"))?;
            connection
                .reader
                .set_read_timeout(Some(Duration::from_secs(10)))
                .map_err(|e| e.to_string())?;
            connection.recv().map_err(|e| e.to_string())?;
            connection
                .send(&crate::body(
                    ids::configuration::clientbound::FINISH_CONFIGURATION,
                    &[],
                ))
                .map_err(|e| e.to_string())?;
            connection.recv().map_err(|e| e.to_string())?;
            let line = play::Clientbound::SystemChat(play::SystemChat {
                content: text::translatable("commands.example", &["a"]),
                overlay: false,
            })
            .encode();
            connection.send(&line).map_err(|e| e.to_string())?;
            loop {
                match connection.recv() {
                    Err(e) if e.is_end_of_stream() => return Ok(()),
                    Err(e) => return Err::<(), String>(e.to_string()),
                    Ok(_) => {}
                }
            }
        });
        let (handle, link) = link();
        let end = session_linked(&options(port), &Recorder::new(), &link);
        assert_eq!(end, End::Clean);
        server.join().unwrap().unwrap();
        let lines = handle.journal.lines();
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(lines[0].text, "commands.example");
    }
}
