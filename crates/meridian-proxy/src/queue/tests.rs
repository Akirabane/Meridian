//! The line of players who wait for a place. The place is a counter here, and the
//! player's connection a pipe.
use super::*;
use horizon_protocol::connection::Pipe;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};

fn settings(wait_seconds: u64, max_length: u32) -> QueueSettings {
    QueueSettings {
        enabled: true,
        wait_seconds,
        max_length,
    }
}

/// The proxy's end of a player's connection, and the player's own.
fn connections() -> (Connection, Connection) {
    let (proxy, player) = Pipe::pair();
    (Connection::local(proxy), Connection::local(player))
}

/// `room` places, taken one at a time.
struct Places {
    free: AtomicUsize,
}

impl Places {
    fn new(free: usize) -> Arc<Self> {
        Arc::new(Places {
            free: AtomicUsize::new(free),
        })
    }

    fn take(&self) -> Option<()> {
        let mut free = self.free.load(Ordering::SeqCst);
        while free > 0 {
            match self
                .free
                .compare_exchange(free, free - 1, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => return Some(()),
                Err(now) => free = now,
            }
        }
        None
    }

    /// Gives a place back and wakes the line, as a session ending does.
    fn give(&self, queue: &LoginQueue) {
        self.free.fetch_add(1, Ordering::SeqCst);
        queue.wake();
    }
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(Instant::now() < until, "{what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A player who waits on a thread of their own: the name goes to `served` when they get
/// the place. Returns when the player is in the line.
fn spawn_player<'s>(
    scope: &'s std::thread::Scope<'s, '_>,
    queue: &'s LoginQueue,
    metrics: &'s Metrics,
    places: &'s Places,
    settings: &'s QueueSettings,
    name: &'static str,
    served: &'s Mutex<Vec<&'static str>>,
) -> Connection {
    let (mut proxy_end, player_end) = connections();
    let before = queue.len();
    scope.spawn(move || {
        let mut line = Line::new(queue, metrics, settings, name);
        let mut who = Who {
            connection: &mut proxy_end,
            stopping: &|| false,
        };
        if line.take(|| places.take(), &mut who).is_ok() {
            served.lock().unwrap().push(name);
            line.served();
        }
    });
    wait_until("the player enters the line", || queue.len() > before);
    player_end
}

#[test]
fn players_are_served_in_the_order_they_came() {
    let (queue, metrics, places) = (LoginQueue::default(), Metrics::default(), Places::new(0));
    let served = Mutex::new(Vec::new());
    let settings = settings(10, 10);
    std::thread::scope(|scope| {
        let _a = spawn_player(scope, &queue, &metrics, &places, &settings, "A", &served);
        let _b = spawn_player(scope, &queue, &metrics, &places, &settings, "B", &served);
        let _c = spawn_player(scope, &queue, &metrics, &places, &settings, "C", &served);
        assert_eq!(
            queue
                .waiting()
                .iter()
                .map(|w| w.0.as_str())
                .collect::<Vec<_>>(),
            ["A", "B", "C"]
        );
        // One place at a time: the first in the line gets it each time.
        for (i, name) in ["A", "B", "C"].iter().enumerate() {
            places.give(&queue);
            wait_until("a player is served", || served.lock().unwrap().len() > i);
            assert_eq!(served.lock().unwrap()[i], *name);
        }
    });
    assert_eq!(queue.len(), 0, "the line is empty");
    assert_eq!(metrics.queue_counts(), (3, 0, [3, 0, 0]));
}

#[test]
fn the_wait_is_woken_by_a_place_and_not_by_the_tick() {
    let (queue, metrics, places) = (LoginQueue::default(), Metrics::default(), Places::new(0));
    let served = Mutex::new(Vec::new());
    let settings = settings(10, 10);
    std::thread::scope(|scope| {
        let _a = spawn_player(scope, &queue, &metrics, &places, &settings, "A", &served);
        let freed = Instant::now();
        places.give(&queue);
        wait_until("A is served", || !served.lock().unwrap().is_empty());
        assert!(
            freed.elapsed() < TICK / 2,
            "served after {:?}, which is the tick",
            freed.elapsed()
        );
    });
}

#[test]
fn nobody_takes_a_place_ahead_of_the_line() {
    let (queue, metrics, places) = (LoginQueue::default(), Metrics::default(), Places::new(0));
    let served = Mutex::new(Vec::new());
    let settings = settings(10, 10);
    std::thread::scope(|scope| {
        let _a = spawn_player(scope, &queue, &metrics, &places, &settings, "A", &served);
        // A place is free, and A has not been woken to take it. A newcomer finds it free
        // all the same, and must go behind A.
        places.free.fetch_add(1, Ordering::SeqCst);
        let _b = spawn_player(scope, &queue, &metrics, &places, &settings, "B", &served);
        assert!(
            served.lock().unwrap().is_empty() || served.lock().unwrap()[0] == "A",
            "B was served first"
        );
        queue.wake();
        wait_until("A is served", || !served.lock().unwrap().is_empty());
        places.give(&queue);
        wait_until("B is served", || served.lock().unwrap().len() == 2);
        assert_eq!(*served.lock().unwrap(), ["A", "B"]);
    });
}

#[test]
fn a_player_who_finds_the_line_empty_and_a_place_free_does_not_wait() {
    let (queue, metrics, places) = (LoginQueue::default(), Metrics::default(), Places::new(1));
    let settings = settings(10, 10);
    let (mut proxy_end, _player) = connections();
    let mut line = Line::new(&queue, &metrics, &settings, "A");
    let mut who = Who {
        connection: &mut proxy_end,
        stopping: &|| false,
    };
    assert_eq!(line.take(|| places.take(), &mut who), Ok(()));
    assert!(!line.was_queued());
    assert_eq!(metrics.queue_counts(), (0, 0, [0, 0, 0]));
}

#[test]
fn a_wait_that_runs_out_is_a_refusal() {
    let (queue, metrics, places) = (LoginQueue::default(), Metrics::default(), Places::new(0));
    let settings = settings(1, 10);
    let (mut proxy_end, _player) = connections();
    let mut line = Line::new(&queue, &metrics, &settings, "A");
    let mut who = Who {
        connection: &mut proxy_end,
        stopping: &|| false,
    };
    let started = Instant::now();
    assert_eq!(
        line.take(|| places.take(), &mut who),
        Err(Refused::TimedOut)
    );
    let took = started.elapsed();
    assert!(
        took >= Duration::from_millis(950) && took < Duration::from_millis(2500),
        "{took:?}"
    );
    assert_eq!(queue.len(), 0);
    assert_eq!(metrics.queue_counts(), (1, 0, [0, 1, 0]));
}

#[test]
fn a_full_line_refuses_at_once() {
    let (queue, metrics, places) = (LoginQueue::default(), Metrics::default(), Places::new(0));
    let served = Mutex::new(Vec::new());
    let settings = settings(10, 2);
    std::thread::scope(|scope| {
        let _a = spawn_player(scope, &queue, &metrics, &places, &settings, "A", &served);
        let _b = spawn_player(scope, &queue, &metrics, &places, &settings, "B", &served);
        let (mut proxy_end, _player) = connections();
        let mut line = Line::new(&queue, &metrics, &settings, "C");
        let mut who = Who {
            connection: &mut proxy_end,
            stopping: &|| false,
        };
        let started = Instant::now();
        assert_eq!(line.take(|| places.take(), &mut who), Err(Refused::Full));
        assert!(started.elapsed() < Duration::from_millis(500));
        assert_eq!(queue.len(), 2, "the two who wait still do");
        assert_eq!(metrics.queue_counts().1, 1, "counted as refused");
        places.give(&queue);
        places.give(&queue);
    });
}

#[test]
fn a_player_who_leaves_while_they_wait_leaves_the_line() {
    let (queue, metrics, places) = (LoginQueue::default(), Metrics::default(), Places::new(0));
    let served = Mutex::new(Vec::new());
    let settings = settings(10, 10);
    std::thread::scope(|scope| {
        let a = spawn_player(scope, &queue, &metrics, &places, &settings, "A", &served);
        let _b = spawn_player(scope, &queue, &metrics, &places, &settings, "B", &served);
        drop(a);
        // Seen at the next tick at the latest.
        wait_until("A leaves the line", || queue.len() == 1);
        assert_eq!(queue.waiting()[0].0, "B");
        // A place goes to B, who is now first.
        places.give(&queue);
        wait_until("B is served", || !served.lock().unwrap().is_empty());
        assert_eq!(*served.lock().unwrap(), ["B"]);
    });
    assert_eq!(queue.len(), 0);
    assert_eq!(metrics.queue_counts(), (2, 0, [1, 0, 1]));
}

#[test]
fn a_line_that_is_dropped_leaves_the_line() {
    let (queue, metrics) = (LoginQueue::default(), Metrics::default());
    let settings = settings(10, 10);
    {
        let mut line = Line::new(&queue, &metrics, &settings, "A");
        let mut state = queue.lock();
        line.enter(&mut state).unwrap();
        assert_eq!(state.line.len(), 1);
    }
    assert_eq!(queue.len(), 0, "no entry is left behind");
    assert_eq!(metrics.queue_counts(), (1, 0, [0, 0, 1]));
}

#[test]
fn the_proxy_stopping_ends_the_wait() {
    let (queue, metrics, places) = (LoginQueue::default(), Metrics::default(), Places::new(0));
    let settings = settings(10, 10);
    let stopping = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let waiter = scope.spawn(|| {
            let (mut proxy_end, _player) = connections();
            let mut line = Line::new(&queue, &metrics, &settings, "A");
            let mut who = Who {
                connection: &mut proxy_end,
                stopping: &|| stopping.load(Ordering::SeqCst),
            };
            line.take(|| places.take(), &mut who)
        });
        wait_until("A is in the line", || queue.len() == 1);
        stopping.store(true, Ordering::SeqCst);
        queue.wake();
        assert_eq!(waiter.join().unwrap(), Err(Refused::Stopping));
    });
    assert_eq!(queue.len(), 0);
}

/// A reload that raises the limit serves the players who wait, without waiting for the
/// tick.
#[test]
fn a_new_limit_serves_the_line() {
    let (queue, metrics) = (LoginQueue::default(), Metrics::default());
    let settings = settings(10, 10);
    let taken = AtomicUsize::new(1);
    let served = Mutex::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let (mut proxy_end, _player) = connections();
            let mut line = Line::new(&queue, &metrics, &settings, "A");
            let mut who = Who {
                connection: &mut proxy_end,
                stopping: &|| false,
            };
            let got = line.take(
                || {
                    let limit = queue.max_players(1) as usize;
                    (taken.load(Ordering::SeqCst) < limit).then(|| {
                        taken.fetch_add(1, Ordering::SeqCst);
                    })
                },
                &mut who,
            );
            *served.lock().unwrap() = got.is_ok();
            line.served();
        });
        wait_until("A is in the line", || queue.len() == 1);
        let raised = Instant::now();
        queue.set_max_players(2);
        wait_until("A is served", || *served.lock().unwrap());
        assert!(raised.elapsed() < TICK / 2, "{:?}", raised.elapsed());
    });
}

/// What the player sends while they wait is not lost: it goes to their backend.
#[test]
fn what_a_waiting_player_sends_goes_to_their_backend() {
    let (queue, metrics, places) = (LoginQueue::default(), Metrics::default(), Places::new(0));
    let settings = settings(10, 10);
    let (mut proxy_end, mut player) = connections();
    let (mut backend, mut backend_far_end) = connections();
    player.send(&[0x00, 7, 7]).unwrap();
    player.send(&[0x02, 1]).unwrap();
    std::thread::scope(|scope| {
        let waiter = scope.spawn(|| {
            let mut line = Line::new(&queue, &metrics, &settings, "A");
            let mut who = Who {
                connection: &mut proxy_end,
                stopping: &|| false,
            };
            let got = line.take(|| places.take(), &mut who);
            line.forward_early(&mut backend);
            got
        });
        wait_until("A is in the line", || queue.len() == 1);
        // Let a tick pass, so that the packets have been read.
        std::thread::sleep(TICK + Duration::from_millis(300));
        places.give(&queue);
        assert_eq!(waiter.join().unwrap(), Ok(()));
    });
    assert_eq!(backend_far_end.recv().unwrap(), (0x00, vec![7, 7]));
    assert_eq!(backend_far_end.recv().unwrap(), (0x02, vec![1]));
}

/// A turn that finds every lobby full is kept: the player stays at the head and
/// someone who arrives meanwhile goes behind.
#[test]
fn a_player_whose_lobbies_are_full_keeps_the_head_of_the_line() {
    let (queue, metrics, places) = (LoginQueue::default(), Metrics::default(), Places::new(0));
    let settings = settings(10, 10);
    let served = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        // A waits, gets a place, and finds the lobbies full.
        let a = scope.spawn(|| {
            let (mut proxy_end, _player) = connections();
            let mut line = Line::new(&queue, &metrics, &settings, "A");
            let mut who = Who {
                connection: &mut proxy_end,
                stopping: &|| false,
            };
            let place = line.take(|| places.take(), &mut who);
            assert!(place.is_ok());
            let started = Instant::now();
            line.await_room(&mut who).unwrap();
            // It slept until the tick, or a signal, and is still first in the line.
            assert!(queue.len() >= 1);
            assert_eq!(queue.waiting()[0].0, "A");
            line.served();
            started.elapsed()
        });
        wait_until("A is in the line", || queue.len() == 1);
        places.give(&queue);
        wait_until("A holds its turn", || {
            places.free.load(Ordering::SeqCst) == 0
        });
        let _b = spawn_player(scope, &queue, &metrics, &places, &settings, "B", &served);
        assert_eq!(queue.waiting()[0].0, "A");
        assert_eq!(queue.waiting()[1].0, "B");
        a.join().unwrap();
        places.give(&queue);
        wait_until("B is served", || !served.lock().unwrap().is_empty());
    });
}

#[test]
fn the_queue_command_shows_the_line() {
    use crate::console::Commands;
    let config = meridian_config::Config::from_toml_str(
        r#"forwarding-secret = "test-secret"
[[backends]]
name = "lobby"
address = "127.0.0.1:25601"
[queue]
enabled = true
wait-seconds = 10
max-length = 4
"#,
    )
    .unwrap();
    let proxy = crate::state::Proxy::new(
        config,
        "does-not-exist/meridian.toml".into(),
        Arc::new(AtomicBool::new(false)),
    );
    let commands = Commands::builtin();
    assert_eq!(
        commands.execute(&proxy, "queue"),
        ["queue on: 0 waiting (at most 4, each for 10 s at most)"]
    );
    let mut state = proxy.sessions.queue.lock();
    state.line.push_back(Entry {
        id: 0,
        name: "Steve".into(),
        since: Instant::now(),
    });
    drop(state);
    let lines = commands.execute(&proxy, "queue");
    assert_eq!(
        lines[0],
        "queue on: 1 waiting (at most 4, each for 10 s at most)"
    );
    assert_eq!(lines[1], "  1. Steve for 0 s");
    assert!(
        commands
            .execute(&proxy, "queue now")
            .remove(0)
            .starts_with("usage:")
    );
}

/// What a waiting player sends is kept, up to a number of packets and a number of
/// bytes: one who sends large packets while they wait is not waiting quietly either,
/// and leaves the line before the proxy keeps megabytes of them.
#[test]
fn a_waiting_player_who_sends_too_much_leaves_the_line() {
    let (mut proxy, mut player) = connections();
    let mut early = Vec::new();
    // A client's settings and brand: kept.
    player.send(&[0x00, 1, 2, 3]).unwrap();
    player.send(&[0x02, 4, 5]).unwrap();
    assert!(matches!(watch(&mut proxy, &mut early), Watch::Waiting));
    assert_eq!(early.len(), 2);
    // Three packets of 600 KiB each: past what a waiting player may send.
    let large = vec![0x02; 600 << 10];
    for _ in 0..3 {
        player.send(&large).unwrap();
    }
    assert!(matches!(watch(&mut proxy, &mut early), Watch::Gone));
    let kept: usize = early.iter().map(|(_, payload)| payload.len()).sum();
    assert!(kept <= MAX_EARLY_BYTES, "{kept} bytes kept");
}

// ---- What the extensions are told of the line ----

/// A line, its places and a registry whose listener writes down what it is told.
struct Room {
    registry: Registry,
    queue: LoginQueue,
    metrics: Metrics,
    places: Arc<Places>,
    settings: QueueSettings,
}

impl Room {
    fn new(free: usize, wait_seconds: u64, max_length: u32) -> Self {
        Room {
            registry: Registry::default(),
            queue: LoginQueue::default(),
            metrics: Metrics::default(),
            places: Places::new(free),
            settings: settings(wait_seconds, max_length),
        }
    }

    /// A line of the room for a player called `name`, with the extensions told of them.
    fn line(&self, name: &str, uuid: u8) -> Line<'_> {
        Line::new(&self.queue, &self.metrics, &self.settings, name).telling(
            &self.registry,
            [uuid; 16],
            true,
        )
    }
}

/// A player who waits on a thread of their own, served when a place is given. Returns when they are in the line.
fn spawn_told<'s>(
    scope: &'s std::thread::Scope<'s, '_>,
    room: &'s Room,
    name: &'static str,
    uuid: u8,
) -> Connection {
    let (mut proxy_end, player_end) = connections();
    let before = room.queue.len();
    scope.spawn(move || {
        let mut line = room.line(name, uuid);
        let mut who = Who {
            connection: &mut proxy_end,
            stopping: &|| false,
        };
        if line.take(|| room.places.take(), &mut who).is_ok() {
            line.served();
        }
    });
    wait_until("the player enters the line", || room.queue.len() > before);
    player_end
}

/// `(method, params)` of what was told, once there is `count` of it.
fn told_of(told: &Mutex<Vec<String>>, count: usize) -> Vec<(String, serde_json::Value)> {
    crate::extension::events::told_events(told, count)
}

/// A player who waits is told to the extensions that listen when they enter, with their place in the line, and when they are served.
/// A second one goes behind the first, and each is told in order. The players are told with their name, their id and how their
/// account was checked, and nothing else.
#[test]
fn the_extensions_are_told_who_enters_the_line_at_which_place_and_who_is_served() {
    let room = Room::new(0, 10, 10);
    let told = room.registry.bus().recording(false);
    std::thread::scope(|scope| {
        let _a = spawn_told(scope, &room, "A", 1);
        let _b = spawn_told(scope, &room, "B", 2);
        room.places.give(&room.queue);
        wait_until("A is served", || room.queue.len() == 1);
        room.places.give(&room.queue);
        wait_until("B is served", || room.queue.len() == 0);
    });
    let events = told_of(&told, 4);
    for (name, uuid, place) in [("A", 1, 1), ("B", 2, 2)] {
        let of = |method: &str| {
            events
                .iter()
                .position(|(m, p)| m == method && p["player"]["name"] == name)
                .unwrap_or_else(|| panic!("{name}: {method} not told, got {events:?}"))
        };
        let (entered, left) = (
            of("meridian:notification/queue/entered"),
            of("meridian:notification/queue/left"),
        );
        assert!(entered < left, "{name} left before they entered");
        assert_eq!(events[entered].1["position"], place, "{name}");
        assert_eq!(events[left].1["cause"], "served", "{name}");
        assert_eq!(
            events[entered].1["player"],
            serde_json::json!({
                "name": name,
                "uuid": hyphenated([uuid; 16]),
                "auth": "verified"
            })
        );
    }
}

/// A wait that runs out is told as `timeout`.
#[test]
fn a_wait_that_runs_out_is_told_as_a_timeout() {
    let room = Room::new(0, 1, 10);
    let told = room.registry.bus().recording(false);
    let (mut proxy_end, _player) = connections();
    let mut line = room.line("A", 1);
    let mut who = Who {
        connection: &mut proxy_end,
        stopping: &|| false,
    };
    assert_eq!(
        line.take(|| room.places.take(), &mut who),
        Err(Refused::TimedOut)
    );
    let events = told_of(&told, 2);
    assert_eq!(events[0].0, "meridian:notification/queue/entered");
    assert_eq!(events[1].0, "meridian:notification/queue/left");
    assert_eq!(events[1].1["cause"], "timeout");
}

/// A player who closes the connection while they wait is told as `left`.
#[test]
fn a_player_who_leaves_while_they_wait_is_told_as_left() {
    let room = Room::new(0, 10, 10);
    let told = room.registry.bus().recording(false);
    std::thread::scope(|scope| {
        let a = spawn_told(scope, &room, "A", 1);
        drop(a);
        wait_until("A leaves the line", || room.queue.len() == 0);
    });
    let events = told_of(&told, 2);
    assert_eq!(events[0].0, "meridian:notification/queue/entered");
    assert_eq!(events[1].0, "meridian:notification/queue/left");
    assert_eq!(events[1].1["cause"], "left");
}

/// The proxy stopping ends the wait, and it is told as `shutdown`.
#[test]
fn the_proxy_stopping_is_told_as_shutdown() {
    let room = Room::new(0, 10, 10);
    let told = room.registry.bus().recording(false);
    let stopping = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let waiter = scope.spawn(|| {
            let (mut proxy_end, _player) = connections();
            let mut line = room.line("A", 1);
            let mut who = Who {
                connection: &mut proxy_end,
                stopping: &|| stopping.load(Ordering::SeqCst),
            };
            line.take(|| room.places.take(), &mut who)
        });
        wait_until("A is in the line", || room.queue.len() == 1);
        stopping.store(true, Ordering::SeqCst);
        room.queue.wake();
        assert_eq!(waiter.join().unwrap(), Err(Refused::Stopping));
    });
    let events = told_of(&told, 2);
    assert_eq!(events[1].0, "meridian:notification/queue/left");
    assert_eq!(events[1].1["cause"], "shutdown");
}

/// A player whose turn came and who is not served (the login was refused or failed after the wait) is told as `refused`, once: the
/// line that is dropped does not tell it again.
#[test]
fn a_player_whose_turn_came_and_who_was_not_served_is_told_as_refused() {
    let room = Room::new(0, 10, 10);
    let told = room.registry.bus().recording(false);
    {
        let (mut proxy_end, _player) = connections();
        let mut line = room.line("A", 1);
        let mut who = Who {
            connection: &mut proxy_end,
            stopping: &|| false,
        };
        // The place is free at the second try, when the player is at the head of the line.
        let mut tries = 0;
        let got = line.take(
            || {
                tries += 1;
                (tries >= 2).then_some(())
            },
            &mut who,
        );
        assert_eq!(got, Ok(()));
        assert!(line.was_queued());
        // Dropped without `served`.
    }
    room.registry.emit(|| Kind::ServerUp {
        server: "after".into(),
    });
    let events = told_of(&told, 3);
    assert_eq!(events[0].0, "meridian:notification/queue/entered");
    assert_eq!(events[0].1["position"], 1);
    assert_eq!(events[1].0, "meridian:notification/queue/left");
    assert_eq!(events[1].1["cause"], "refused");
    assert_eq!(events[2].0, "meridian:notification/servers/up", "told once");
}

/// A player who finds a place at once never waits and is not told of, and nor is one the line is too long for.
#[test]
fn a_player_who_never_waits_is_not_told_of() {
    let room = Room::new(1, 10, 0);
    let told = room.registry.bus().recording(false);
    {
        let (mut proxy_end, _player) = connections();
        let mut who = Who {
            connection: &mut proxy_end,
            stopping: &|| false,
        };
        let mut line = room.line("A", 1);
        assert_eq!(line.take(|| room.places.take(), &mut who), Ok(()));
        assert!(!line.was_queued());
        // No place, and no room in the line: refused at once, without entering it.
        let mut full = room.line("B", 2);
        assert_eq!(
            full.take(|| room.places.take(), &mut who),
            Err(Refused::Full)
        );
    }
    room.registry.emit(|| Kind::ServerUp {
        server: "after".into(),
    });
    let events = told_of(&told, 1);
    assert_eq!(
        events[0].0, "meridian:notification/servers/up",
        "nothing was told before this"
    );
    assert_eq!(told.lock().unwrap().len(), 1);
}

/// A player who found a place at once and then found every lobby full enters the line then, and is told so, at the first place.
#[test]
fn a_player_whose_lobbies_are_full_is_told_entering_the_line_when_they_do() {
    let room = Room::new(1, 10, 10);
    let told = room.registry.bus().recording(false);
    let (mut proxy_end, _player) = connections();
    let mut who = Who {
        connection: &mut proxy_end,
        stopping: &|| false,
    };
    let mut line = room.line("A", 1);
    assert_eq!(line.take(|| room.places.take(), &mut who), Ok(()));
    assert!(!line.was_queued());
    assert_eq!(line.await_room(&mut who), Ok(()));
    assert!(line.was_queued());
    line.served();
    let events = told_of(&told, 2);
    assert_eq!(events[0].0, "meridian:notification/queue/entered");
    assert_eq!(events[0].1["position"], 1);
    assert_eq!(events[1].0, "meridian:notification/queue/left");
    assert_eq!(events[1].1["cause"], "served");
}
