//! The switch itself: prepare, freeze, hand over, commit (DESIGN.md section 5).
use crate::{
    configure::{Digest, configure},
    control::{
        Arrival, Outcome, PlayerControl, Record, record_dimension, record_handover, uuid_text,
    },
    prepare::Target,
    session::{Lane, SessionHandle, Shared, add, encode, lock, spawn_lane},
};
use meridian_ghost::{View, delta};
use meridian_net::{
    backend::{self, BackendOptions},
    relay::Phase,
};
use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex, atomic::Ordering},
    time::{Duration, Instant},
};

/// The steps of a switch, for the messages that say where one failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// What is checked before anything is asked of a backend.
    Check,
    /// The target holds the player and sends their world; nothing changes for the
    /// client.
    Prepare,
    /// The source freezes the player and gives their record.
    Freeze,
    /// The target takes the record; the client gets the delta and the target's
    /// packets from then on.
    HandOver,
    /// The source lets the player go.
    Commit,
}

impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Step::Check => "check",
            Step::Prepare => "prepare",
            Step::Freeze => "freeze",
            Step::HandOver => "hand over",
            Step::Commit => "commit",
        })
    }
}

/// Where the player is after a failed switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum After {
    /// On the source, as before: the client saw nothing.
    Stayed,
    /// The player left during the switch. Once their record was out of the
    /// source, it was imported into the target as a normal save (decision 10).
    Left,
    /// The client had the target's world, but the player was not placed there (the
    /// target would not, or the freeze left was too short to go on): the player was
    /// disconnected, and the source keeps them.
    Disconnected,
}

/// Why a switch did not happen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub step: Step,
    pub reason: String,
    pub after: After,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.step, self.reason)?;
        match self.after {
            After::Stayed => f.write_str(" (the player stays where they were)"),
            After::Left => f.write_str(" (the player left during the switch)"),
            After::Disconnected => {
                f.write_str(" (the player was disconnected and stays where they were)")
            }
        }
    }
}

impl std::error::Error for Failure {}

/// What a switch did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Done {
    /// The packets and bytes of the delta, bundle delimiters included.
    pub delta_packets: usize,
    pub delta_bytes: u64,
    /// The target's packets sent after the delta (command tree, recipes...).
    pub extras: usize,
    /// The chunks and entities of the target view.
    pub chunks: usize,
    pub entities: usize,
    /// How long each step took: prepare, freeze, hand over, commit.
    pub times: [Duration; 4],
    /// The id of the transfer: the source's hand-over id, which the logs of both
    /// servers carry.
    pub handover: String,
    /// Set when the source refused to let the player go (`player-release moved`)
    /// after the target had placed them, with its reason. The player is on the
    /// target all the same: nothing can undo their placing, and the source keeps its
    /// own copy, which it saves when its connection closes. The two servers' files
    /// for the player differ from then on, which an operator has to know.
    pub source_kept: Option<String>,
}

/// The parts of a record `player-import` knows (Horizon's `docs/server.md`).
pub const PARTS: [&str; 10] = [
    "inventory",
    "ender_chest",
    "experience",
    "health",
    "effects",
    "game_mode",
    "position",
    "spawn_point",
    "recipe_book",
    "advancements",
];

/// Told of each step of a switch as it happens, with the time since the switch
/// began: what the proxy writes to its log.
pub type Trace = Arc<dyn Fn(&str, Duration) + Send + Sync>;

/// How long each part of a switch may take, and what the others are told.
#[derive(Clone)]
pub struct Settings {
    /// How long the target holds a place for the player (1 to 60 s).
    pub expect_seconds: u64,
    /// How long the target has to configure the player and send their world.
    pub prepare_timeout: Duration,
    /// The target's world is taken as sent once no chunk batch came for this
    /// long: before the freeze, and after the import of a carried position.
    pub settle: Duration,
    /// How long the target has, after the import, to send the player's position
    /// (and with a carried position, the chunks around it).
    pub import_timeout: Duration,
    /// How long the player may stay frozen before the switch is given up. Horizon
    /// lets a freeze go by itself after `max_freeze_seconds`; the switch gives up
    /// before, so that it can still let the source go on (`player-release abort`).
    /// It must stay below that, and the freeze the export says is left is checked as
    /// well (see [`Settings::commit_margin`]).
    pub freeze_limit: Duration,
    /// The time kept in hand against the freeze the source says is left: the
    /// transfer goes on to the placing of the player (`player-confirm`) only while
    /// the freeze left outlasts this margin plus two requests as slow as the
    /// slowest so far, which is what the confirm and the release take. Otherwise the
    /// arrival is dropped and the source goes on.
    pub commit_margin: Duration,
    /// How many times the export is asked in all when its answer is lost (no answer
    /// in time, a connection that broke). The source answers a repeated export of a
    /// frozen player with the same record, and does not freeze again.
    pub export_attempts: u32,
    /// Whether the player keeps the position of their record. Without it they
    /// arrive where the target puts them, in chunks the target already sent;
    /// with it, the target must first send the chunks around the new place.
    pub carry_position: bool,
    /// The target's players are told the player came (join line, list entry).
    pub announce_arrival: bool,
    /// The source's players get the "left the game" line.
    pub announce_departure: bool,
    /// Told of each step.
    pub trace: Option<Trace>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            expect_seconds: 10,
            prepare_timeout: Duration::from_secs(8),
            settle: Duration::from_millis(200),
            import_timeout: Duration::from_secs(3),
            freeze_limit: Duration::from_secs(8),
            commit_margin: Duration::from_secs(2),
            export_attempts: 3,
            carry_position: false,
            announce_arrival: true,
            announce_departure: true,
            trace: None,
        }
    }
}

impl Settings {
    /// The parts the import takes: every one, or every one but the position.
    fn parts(&self) -> Option<Vec<String>> {
        (!self.carry_position).then(|| {
            PARTS
                .iter()
                .filter(|part| **part != "position")
                .map(|part| (*part).to_owned())
                .collect()
        })
    }
}

/// The backend to switch to.
pub struct TargetBackend<'a> {
    pub name: &'a str,
    pub address: &'a str,
    pub options: &'a BackendOptions,
}

/// The configuration digest last seen from each backend, so that a backend known
/// to configure clients differently is refused before it is asked anything.
#[derive(Default)]
pub struct Digests(Mutex<HashMap<String, Digest>>);

impl Digests {
    pub fn get(&self, backend: &str) -> Option<Digest> {
        lock(&self.0).get(backend).copied()
    }

    fn set(&self, backend: &str, digest: Digest) {
        lock(&self.0).insert(backend.to_owned(), digest);
    }
}

/// Clears the session's switching mark when the switch ends, however it ends, and
/// closes what the session left open if it ended meanwhile.
struct Switching<'a>(&'a Shared);

impl Drop for Switching<'_> {
    fn drop(&mut self) {
        self.0.switching.store(false, Ordering::SeqCst);
        if self.0.ended.load(Ordering::SeqCst) {
            self.0.up().lane.closer.close();
        }
    }
}

/// Horizon counts a freeze in server ticks, 20 a second.
const TICK_MS: u64 = 50;

/// How long the player may stay frozen, from the export on.
struct Freeze {
    /// When the switch gives up whatever the source says: the freeze limit, counted
    /// from the end of the prepare step.
    limit_at: Instant,
    /// When the source lets the freeze go by itself, as it said. `None` when it did
    /// not say.
    ends: Option<Instant>,
    /// The time kept in hand (`Settings::commit_margin`).
    margin: Duration,
    /// The slowest request of the hand-over so far.
    slowest: Duration,
}

impl Freeze {
    /// What the rest of the transfer needs from the freeze left: the margin and the
    /// confirm and the release, each about as slow as the slowest request so far.
    fn need(&self) -> Duration {
        self.margin + self.slowest * 2
    }

    /// The moment from which the hand-over goes no further: the freeze limit, or
    /// the end of the freeze the source said less what the rest needs.
    fn deadline(&self) -> Instant {
        match self.ends.and_then(|ends| ends.checked_sub(self.need())) {
            Some(by) => by.min(self.limit_at),
            None => self.limit_at,
        }
    }

    /// Why the hand-over goes no further, for the one who asked for the switch.
    fn why_late(&self, limit: Duration) -> String {
        match self.ends {
            Some(ends)
                if self.ends.and_then(|e| e.checked_sub(self.need())) < Some(self.limit_at) =>
            {
                format!(
                    "the player's freeze has {} ms left on the old server, and the rest of the \
                     transfer needs about {} ms",
                    ends.saturating_duration_since(Instant::now()).as_millis(),
                    self.need().as_millis()
                )
            }
            _ => format!(
                "the player stayed frozen for {} ms without being handed over",
                limit.as_millis()
            ),
        }
    }
}

/// The two sides' controls, the player's id, and both lanes.
struct Run<'a> {
    shared: &'a Shared,
    source: &'a mut dyn PlayerControl,
    target: &'a mut dyn PlayerControl,
    uuid: String,
    /// The source's lane, as it was when the switch began.
    source_lane: Arc<Lane>,
    /// The target's lane, once it exists.
    lane: Option<Arc<Lane>>,
    /// The parts of the record the target takes.
    parts: Option<Vec<String>>,
    /// Whether the player's place goes to the target.
    carry: bool,
    /// The id of the hand-over, which the export with freeze gave and the requests
    /// that end it name. `None` until the export.
    handover: Option<String>,
    /// Whether the target took the record: its later requests name the hand-over.
    imported: bool,
}

impl Run<'_> {
    fn fail(&self, step: Step, reason: impl Into<String>) -> Failure {
        Failure {
            step,
            reason: reason.into(),
            after: After::Stayed,
        }
    }

    /// The target forgets the held player. Once it holds a record, the drop names the
    /// hand-over, so that it cuts that arrival and no other; before, it goes by the
    /// player's unique id.
    fn drop_target(&mut self) -> Result<(), String> {
        let handover = self.handover.clone().filter(|_| self.imported);
        self.target.drop_player(&self.uuid, handover.as_deref())
    }

    /// The target places the player whose record it holds.
    fn confirm_target(&mut self) -> Result<(), String> {
        match self.handover.clone() {
            Some(handover) => self.target.confirm(&self.uuid, &handover),
            None => Err("the export gave no hand-over id".into()),
        }
    }

    /// The source ends the hand-over: the player is gone for good, or stays.
    fn release_source(&mut self, outcome: Outcome, announce: bool) -> Result<(), String> {
        match self.handover.clone() {
            Some(handover) => self
                .source
                .release(&self.uuid, &handover, outcome, announce),
            None => Err("the export gave no hand-over id".into()),
        }
    }

    /// Lets go of the target: it forgets the held player, and its lane closes.
    fn abandon_target(&mut self) {
        let _ = self.drop_target();
        if let Some(lane) = &self.lane {
            lane.closer.close();
        }
    }

    /// The source lets the frozen player go on, and the target is let go.
    fn abort(&mut self) {
        let _ = self.release_source(Outcome::Abort, false);
        self.abandon_target();
    }

    /// Where the player arrives on the target when their place is carried: where
    /// their client last said it stands and how it looks, standing still, as it
    /// said it stood. `None` without carrying, or when the client never said.
    fn arrival(&self) -> Option<Arrival> {
        if !self.carry {
            return None;
        }
        let pose = self.shared.up().client_pose?;
        Some(Arrival {
            position: pose.at,
            rotation: [pose.yaw, pose.pitch],
            // The client never sends its velocity, and the record's is the export's,
            // which the client has left behind since. A zero velocity is not sent:
            // a server may refuse the field, even as [0, 0, 0].
            velocity: None,
            on_ground: pose.on_ground,
        })
    }
    fn client_gone(&self) -> bool {
        self.shared.client_gone.load(Ordering::SeqCst)
    }

    /// The player left once their record was out of the source (decision 10): the
    /// record goes into the target as a normal save, and the source lets them go.
    /// `record` is the record still to import; `None` when it was imported already,
    /// without placing the player.
    fn left(&mut self, step: Step, record: Option<Record>, announce: bool) -> Failure {
        // A placed player is saved by the target when its connection closes.
        let placed = match record {
            Some(record) => self
                .target
                .import(
                    &self.uuid,
                    record,
                    self.parts.clone(),
                    Some(false),
                    true,
                    self.arrival(),
                )
                .is_ok(),
            None => self.confirm_target().is_ok(),
        };
        if !placed {
            let _ = self.drop_target();
        }
        if let Some(lane) = &self.lane {
            lane.closer.close();
        }
        let reason = if placed {
            // A source that refuses (the freeze ran out) has the player's later
            // state, which it saves when its connection closes just below.
            let _ = self.release_source(Outcome::Moved, announce);
            "the player left during the switch; their record was saved on the target"
        } else {
            let _ = self.release_source(Outcome::Abort, false);
            "the player left during the switch; the target would not take their record, so \
             the source keeps them"
        };
        self.source_lane.closer.close();
        Failure {
            step,
            reason: reason.to_owned(),
            after: After::Left,
        }
    }
}

/// Switches the player of `session` to `target`. `source` is the control of the
/// backend the player is on, `target_control` that of the target.
///
/// Until the first packet of the delta goes to the client, any failure leaves the
/// player where they were ([`After::Stayed`]): the target forgets them and the
/// source lets them go on.
pub fn switch(
    session: &SessionHandle,
    target: TargetBackend<'_>,
    source: &mut dyn PlayerControl,
    target_control: &mut dyn PlayerControl,
    digests: &Digests,
    settings: &Settings,
) -> Result<Done, Failure> {
    let shared: &Shared = &session.0;
    let mut run = Run {
        shared,
        source,
        target: target_control,
        uuid: uuid_text(shared.profile.uuid),
        source_lane: Arc::clone(&shared.up().lane),
        lane: None,
        parts: settings.parts(),
        carry: settings.carry_position,
        handover: None,
        imported: false,
    };
    if shared.switching.swap(true, Ordering::SeqCst) {
        return Err(run.fail(Step::Check, "a switch of this player is already under way"));
    }
    let _switching = Switching(shared);

    // Check.
    let (client_id, session_digest, dimension, secure_chat) = {
        let down = shared.down();
        if down.backend == target.name {
            return Err(run.fail(
                Step::Check,
                format!("the player is already on {}", target.name),
            ));
        }
        if down.phase != Phase::Play {
            return Err(run.fail(Step::Check, "the player is not in the play phase"));
        }
        match (down.ghost.player_id(), down.config_digest) {
            (Some(id), Some(digest)) => (id, digest, down.dimension.clone(), down.secure_chat),
            _ => return Err(run.fail(Step::Check, "the player has not joined yet")),
        }
    };
    if let Some(kind) = digests
        .get(target.name)
        .and_then(|known| session_digest.differs(&known))
    {
        return Err(run.fail(Step::Check, differs(target.name, kind)));
    }
    let client = shared.up().client.clone();

    // Prepare.
    let started = Instant::now();
    let note = |what: &str| {
        if let Some(trace) = &settings.trace {
            trace(what, started.elapsed());
        }
    };
    run.target
        .expect(&run.uuid, Some(settings.expect_seconds))
        .map_err(|e| {
            run.fail(
                Step::Prepare,
                format!("{} would not expect the player: {e}", target.name),
            )
        })?;
    note("prepare: the target expects the player");
    let mut link = match backend::connect(target.address, &shared.profile, target.options) {
        Ok(link) => link,
        Err(e) => {
            run.abandon_target();
            return Err(run.fail(
                Step::Prepare,
                format!("the player could not log in on {}: {e}", target.name),
            ));
        }
    };
    note("prepare: the player is logged in on the target");
    let configured = link
        .connection
        .reader
        .set_read_timeout(Some(settings.prepare_timeout))
        .map_err(|e| e.to_string())
        .and_then(|()| configure(&mut link.connection, &client))
        .and_then(|digest| {
            link.connection
                .reader
                .set_read_timeout(None)
                .map(|()| digest)
                .map_err(|e| e.to_string())
        });
    let digest = match configured {
        Ok(digest) => digest,
        Err(why) => {
            let _ = run.drop_target();
            return Err(run.fail(Step::Prepare, why));
        }
    };
    digests.set(target.name, digest);
    if let Some(kind) = session_digest.differs(&digest) {
        let _ = run.drop_target();
        return Err(run.fail(Step::Prepare, differs(target.name, kind)));
    }
    note("prepare: the target configures clients as the first server did (digest)");
    let (reader, writer) = link.connection.split();
    let closer = match reader.closer() {
        Ok(closer) => closer,
        Err(e) => {
            let _ = run.drop_target();
            return Err(run.fail(Step::Prepare, e.to_string()));
        }
    };
    let state = Arc::new(Target::new(client_id));
    let lane = Arc::new(Lane {
        id: shared.lane_id(),
        writer: Mutex::new(writer),
        closer,
        target: Some(Arc::clone(&state)),
    });
    run.lane = Some(Arc::clone(&lane));
    if let Err(e) = spawn_lane(&session.0, Arc::clone(&lane), reader) {
        run.abandon_target();
        return Err(run.fail(Step::Prepare, e.to_string()));
    }
    let settle = settings.settle;
    let ready = state.wait(
        started + settings.prepare_timeout,
        "the target did not send the player's world in time",
        |s| {
            let settled = s.last_batch.is_some_and(|at| at.elapsed() >= settle);
            (s.swap.is_some() && s.positions > 0 && s.batches > 0 && settled)
                .then(|| (s.dimension.clone(), s.secure_chat))
        },
    );
    let (target_dimension, target_secure_chat) = match ready {
        Ok(ready) => ready,
        Err(why) => {
            run.abandon_target();
            return Err(run.fail(Step::Prepare, why));
        }
    };
    if target_dimension != dimension {
        run.abandon_target();
        return Err(run.fail(
            Step::Prepare,
            format!(
                "{} puts the player in {}, and they are in {}: only a new login changes the \
                 dimension",
                target.name,
                target_dimension.as_deref().unwrap_or("no dimension"),
                dimension.as_deref().unwrap_or("no dimension"),
            ),
        ));
    }
    if target_secure_chat != secure_chat {
        run.abandon_target();
        return Err(run.fail(
            Step::Prepare,
            format!(
                "{} does not enforce signed chat as the player's first server does",
                target.name
            ),
        ));
    }
    if run.client_gone() {
        run.abandon_target();
        return Err(run.fail(Step::Prepare, "the player left"));
    }
    let (chunks, entities) = state.lock().view.counts();
    note(&format!(
        "prepare: the target's world is here ({chunks} chunks, {entities} entities)"
    ));
    let prepared = Instant::now();

    // Freeze. From the export on, the switch must hand the player over, or give up
    // and let the source go on, before the source lets the freeze go by itself.
    //
    // An answer lost on the way is asked for again, a few times: the source answers
    // an export of a player it froze with the same record and the same id, and does
    // not freeze again, so it is safe whether or not the first request got there.
    let limit_at = prepared + settings.freeze_limit;
    let mut attempt = 0;
    let (exported, sent) = loop {
        attempt += 1;
        let sent = Instant::now();
        let e = match run.source.export(&run.uuid, true) {
            Ok(exported) => break (exported, sent),
            Err(e) => e,
        };
        let again = if attempt < settings.export_attempts && Instant::now() < limit_at {
            run.source.recover()
        } else {
            Ok(false)
        };
        match again {
            Ok(true) => note(&format!(
                "freeze: the export got no answer ({e}); asking again ({} of {})",
                attempt + 1,
                settings.export_attempts
            )),
            Ok(false) | Err(_) => {
                run.abandon_target();
                let tried = if attempt > 1 {
                    format!(" (asked {attempt} times)")
                } else {
                    String::new()
                };
                let more = again.err().map_or(String::new(), |why| format!("; {why}"));
                return Err(run.fail(
                    Step::Freeze,
                    format!("the player's server would not give their record{tried}: {e}{more}"),
                ));
            }
        }
    };
    let record = exported.record;
    // The freeze the server says is left, counted from the moment the export was
    // sent: the server counted from after it, so the end is never later than this.
    let mut freeze = Freeze {
        limit_at,
        ends: exported.freeze_ticks_left.and_then(|ticks| {
            sent.checked_add(Duration::from_millis(ticks.saturating_mul(TICK_MS)))
        }),
        margin: settings.commit_margin,
        slowest: sent.elapsed(),
    };
    if let Some(ticks) = exported.freeze_ticks_left
        && Duration::from_millis(ticks.saturating_mul(TICK_MS)) <= settings.freeze_limit
    {
        note(&format!(
            "freeze: the old server lets the freeze go after {} ms, and the freeze limit is {} ms: \
             the limit must be below the server's max_freeze_seconds",
            ticks.saturating_mul(TICK_MS),
            settings.freeze_limit.as_millis()
        ));
    }
    // The id of the hand-over, which the requests that end it name. A server that
    // freezes the player without giving one cannot be asked to let them go: it does
    // not do the transfers this proxy asks for, and lets the freeze go by itself.
    match record_handover(&record) {
        Some(handover) => run.handover = Some(handover),
        None => {
            run.abandon_target();
            return Err(run.fail(
                Step::Freeze,
                "the player's server gave no hand-over id with their record: it does not take \
                 part in transfers as this proxy asks",
            ));
        }
    }
    if record_dimension(&record) != target_dimension.as_deref() {
        run.abort();
        return Err(run.fail(
            Step::Freeze,
            format!(
                "the player's record is in {}, and {} holds them in {}",
                record_dimension(&record).unwrap_or("no dimension"),
                target.name,
                target_dimension.as_deref().unwrap_or("no dimension"),
            ),
        ));
    }
    if run.client_gone() {
        return Err(run.left(Step::Freeze, Some(record), settings.announce_departure));
    }
    note("freeze: the source froze the player and gave their record");
    let frozen = Instant::now();

    // Hand over: the import, without placing the player. The target sends what it
    // changes to the held player's connection, so the target view is the world
    // after the import before the client is sent anything.
    let positions = {
        let mut s = state.lock();
        s.answer_teleports = false;
        s.positions
    };
    if Instant::now() >= freeze.deadline() {
        run.abort();
        return Err(run.fail(Step::Freeze, freeze.why_late(settings.freeze_limit)));
    }
    // A carried position is where the client stands now, not where the source
    // froze the player: what the client did since the export is kept, so it is
    // not pulled back.
    let arrival = run.arrival();
    // Taken before the request: the target can send the new view's centre and
    // chunks before its answer reaches us, and they count as after the import.
    let import_sent = Instant::now();
    let imported = match run.target.import(
        &run.uuid,
        record,
        run.parts.clone(),
        Some(settings.announce_arrival),
        false,
        arrival,
    ) {
        Ok(imported) => imported,
        Err(e) => {
            run.abort();
            return Err(run.fail(
                Step::HandOver,
                format!("{} would not take the player's record: {e}", target.name),
            ));
        }
    };
    run.imported = true;
    // The confirm and the release take about as long as the slowest request so far.
    freeze.slowest = freeze.slowest.max(import_sent.elapsed());
    note(&match (settings.carry_position, arrival) {
        (false, _) => "hand over: the target took the record, all but the position".to_owned(),
        (true, Some(arrival)) => format!(
            "hand over: the target took the record, at the client's place {:.2} {:.2} {:.2} \
             (recentered: {})",
            arrival.position[0],
            arrival.position[1],
            arrival.position[2],
            imported
                .recentered
                .map_or("not said".to_owned(), |r| r.to_string())
        ),
        (true, None) => {
            "hand over: the target took the record, at the place of the export (the client \
             never said where it stands)"
                .to_owned()
        }
    });
    // The import sends what it changed in one go, a position for the client to
    // confirm among it, whatever the parts: the delta waits for that position. A
    // carried position also moves the player, and the chunks around the new place
    // come over the next ticks: then the delta waits for a chunk batch after the
    // import, and for the batches to stop for the settle time.
    //
    // Only the position and the chunk batches count. A busy target never stops
    // sending (mobs move, time passes), so waiting for it to be quiet would keep
    // the player frozen until the import timeout and abort the switch.
    // Whether the player moved to another chunk: the target says so in its answer,
    // or, when it does not, the centre of the view it sent after the import does.
    let carry = settings.carry_position;
    let settled = |s: &crate::prepare::Preparing| {
        if s.positions <= positions {
            return None;
        }
        let recentered = imported
            .recentered
            .unwrap_or_else(|| s.last_center.is_some_and(|at| at >= import_sent));
        if !carry || !recentered {
            return Some(());
        }
        s.last_batch
            .filter(|at| *at >= import_sent && at.elapsed() >= settle)
            .map(|_| ())
    };
    let late = Instant::now() + settings.import_timeout;
    let thaw = freeze.deadline();
    let imported = if late < thaw {
        state.wait(
            late,
            if carry {
                "the target did not send the player's position and the chunks around it \
                 after the import"
            } else {
                "the target did not send the player's position after the import"
            },
            settled,
        )
    } else {
        state.wait(thaw, &freeze.why_late(settings.freeze_limit), settled)
    };
    if let Err(why) = imported {
        // The player is still held: the target forgets them, the source keeps them.
        run.abort();
        return Err(run.fail(Step::HandOver, why));
    }

    // Hand over: the client. From the first packet of the delta, the player is on
    // the target.
    let source_lane = Arc::clone(&run.source_lane);
    let (delta_packets, delta_bytes, extras, chunks, entities) = {
        let mut s = state.lock();
        let mut down = shared.down();
        if run.client_gone() {
            drop(down);
            drop(s);
            return Err(run.left(Step::HandOver, None, settings.announce_departure));
        }
        if Instant::now() >= freeze.deadline() {
            drop(down);
            drop(s);
            run.abort();
            return Err(run.fail(Step::HandOver, freeze.why_late(settings.freeze_limit)));
        }
        // What the delta goes from and to, for the log: a wrong inventory or
        // experience after a switch shows here first.
        note(&format!(
            "hand over: the client holds {}; the target holds {}",
            down.ghost.player_summary(),
            s.view.player_summary()
        ));
        let unread = [
            ("the client's", std::mem::take(&mut down.unreadable)),
            ("the target's", std::mem::take(&mut s.unreadable)),
        ];
        for (whose, list) in unread {
            if !list.is_empty() {
                note(&format!(
                    "hand over: packets {whose} view could not read, left as they were: {}",
                    list.join("; ")
                ));
            }
        }
        let packets = delta(&down.ghost, &s.view);
        let extras = std::mem::take(&mut s.extras);
        let mut body = Vec::new();
        let mut bytes = 0;
        let mut sent = true;
        for packet in packets.iter().chain(&extras) {
            encode(&mut body, packet.id, &packet.payload);
            if down.writer.send(&body).is_err() {
                sent = false;
                break;
            }
            bytes += body.len() as u64;
            add(&mut down.flow, &body);
            let _ = down.ghost.apply(packet);
        }
        let swap = s.swap.expect("a ready target has sent its Login");
        down.active = lane.id;
        down.swap = swap;
        down.backend = target.name.to_owned();
        down.dimension = s.dimension.clone();
        {
            let mut up = shared.up();
            up.lane = Arc::clone(&lane);
            up.swap = swap;
            up.teleports.switched();
            // The client confirms the position of the delta; if the proxy did
            // already, the target must not get it twice.
            if let Some(teleport) = s.last_teleport
                && s.answered.contains(&teleport)
            {
                up.teleports.answered_by_proxy(teleport);
            }
        }
        let (chunks, entities) = s.view.counts();
        s.preparing = false;
        s.view = View::ghost();
        if !sent {
            drop(down);
            drop(s);
            return Err(run.left(Step::HandOver, None, settings.announce_departure));
        }
        (packets.len(), bytes, extras.len(), chunks, entities)
    };
    note(&format!(
        "hand over: the client has the target's world ({delta_packets} packets, {delta_bytes} \
         bytes; {chunks} chunks, {entities} entities)"
    ));
    // The client has the target's world: the target places the player.
    let disconnect = || {
        session.disconnect(horizon_protocol::packets::configuration::Disconnect {
            reason: horizon_protocol::text::literal(&format!(
                "The switch to {} failed; join again to go on where you were.",
                target.name
            )),
        });
    };
    // Placing the player cannot be undone: only while the freeze left outlasts the
    // confirm and the release does the switch go on to it. Otherwise the arrival is
    // dropped, the source goes on, and the client, which has the target's world, is
    // disconnected.
    let placed = if Instant::now() >= freeze.deadline() {
        Err(format!(
            "{}; the arrival on {} was dropped",
            freeze.why_late(settings.freeze_limit),
            target.name
        ))
    } else {
        run.confirm_target()
            .map_err(|e| format!("{} would not place the player: {e}", target.name))
    };
    if let Err(reason) = placed {
        disconnect();
        run.abort();
        source_lane.closer.close();
        return Err(Failure {
            step: Step::HandOver,
            reason,
            after: After::Disconnected,
        });
    }
    note("hand over: the target placed the player");
    let handed = Instant::now();

    // Commit. The target has the player's file on disk, so the source may let go. A
    // source that refuses has no hand-over any more: the freeze ran out, or a command
    // killed the frozen player. The target has the player placed and nothing undoes
    // that (`player-drop` takes only a held player), so the player stays on the
    // target. The source's connection closes all the same, and the source saves its
    // own copy when it does: the two servers' files differ from then on, and the
    // caller tells the operator ([`Done::source_kept`]).
    let source_kept = match run.release_source(Outcome::Moved, settings.announce_departure) {
        Ok(()) => {
            note("commit: the source let the player go");
            None
        }
        Err(e) => {
            note(&format!(
                "commit: the source would not let the player go: {e}"
            ));
            Some(e)
        }
    };
    source_lane.closer.close();
    let committed = Instant::now();
    Ok(Done {
        delta_packets,
        delta_bytes,
        extras,
        chunks,
        entities,
        times: [
            prepared - started,
            frozen - prepared,
            handed - frozen,
            committed - handed,
        ],
        handover: run.handover.clone().unwrap_or_default(),
        source_kept,
    })
}

fn differs(backend: &str, kind: &str) -> String {
    format!(
        "{backend} configures clients differently ({kind}): the player would need a new \
         configuration phase"
    )
}
