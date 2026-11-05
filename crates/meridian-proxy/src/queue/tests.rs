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
