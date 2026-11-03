use super::*;

const MS: fn(u64) -> Duration = Duration::from_millis;

/// A server with `players`, a soft cap, a hard cap, available and not measured.
fn server(backend: &str, players: usize, soft_cap: u32, hard_cap: u32) -> Candidate<'_> {
    Candidate {
        backend,
        players,
        soft_cap,
        hard_cap,
        available: true,
        ping: None,
    }
}

fn placed<'a>(
    candidates: &[Candidate<'a>],
    gates: &mut Gates,
    margin: u32,
) -> Result<&'a str, NoRoom> {
    place(candidates, gates, margin, TieBreak::Order).map(|p| p.backend)
}

#[test]
fn players_are_counted_by_backend() {
    let counts = count_by_backend(["a", "b", "a", "a"]);
    assert_eq!(counts.get("a"), Some(&3));
    assert_eq!(counts.get("b"), Some(&1));
    assert_eq!(counts.get("c"), None);
    assert!(count_by_backend([]).is_empty());
}

/// One server is filled to its soft cap before the next is opened, and a player
/// is never alone on a server while another has room: 10 players on three servers
/// of 4 make 4, 4 and 2.
#[test]
fn a_server_is_filled_before_the_next_is_opened() {
    let mut players = [0usize; 3];
    let names = ["g1", "g2", "g3"];
    let mut gates = Gates::default();
    for _ in 0..10 {
        let candidates: Vec<Candidate<'_>> = names
            .iter()
            .zip(players)
            .map(|(n, p)| server(n, p, 4, 4))
            .collect();
        let name = placed(&candidates, &mut gates, 2).unwrap();
        players[names.iter().position(|n| *n == name).unwrap()] += 1;
    }
    assert_eq!(players, [4, 4, 2]);
}

#[test]
fn the_fullest_open_server_is_chosen_not_the_first_listed() {
    let candidates = [
        server("a", 1, 10, 10),
        server("b", 7, 10, 10),
        server("c", 3, 10, 10),
    ];
    assert_eq!(placed(&candidates, &mut Gates::default(), 2), Ok("b"));
}

#[test]
fn a_server_at_its_soft_cap_is_closed_and_the_next_one_is_opened() {
    let mut gates = Gates::default();
    let candidates = [server("a", 4, 4, 4), server("b", 0, 4, 4)];
    assert_eq!(placed(&candidates, &mut gates, 2), Ok("b"));
    assert!(gates.is_closed("a"));
    assert!(!gates.is_closed("b"));
}

/// The state of one server of soft cap 4 as its player count goes up and down:
/// the counts, the margin, and whether it is closed after each.
#[test]
fn a_closed_server_opens_again_only_at_the_soft_cap_minus_the_margin() {
    type Row = (&'static [usize], u32, &'static [bool]);
    let table: &[Row] = &[
        // Margin 2: closes at 4, still closed at 3, open again at 2.
        (
            &[2, 3, 4, 3, 2, 3],
            2,
            &[false, false, true, true, false, false],
        ),
        // Margin 1: open again at 3.
        (&[3, 4, 3, 4, 3], 1, &[false, true, false, true, false]),
        // Margin 0: open again as soon as it is below the cap.
        (&[4, 3, 4], 0, &[true, false, true]),
        // A margin above the soft cap: open again only when empty.
        (&[4, 3, 1, 0], 9, &[true, true, true, false]),
    ];
    for (counts, margin, expected) in table {
        let mut gates = Gates::default();
        let got: Vec<bool> = counts
            .iter()
            .map(|&n| {
                gates.update(&server("a", n, 4, 4), *margin);
                gates.is_closed("a")
            })
            .collect();
        assert_eq!(&got, expected, "counts {counts:?}, margin {margin}");
    }
}

/// A server that fell from its cap but not to the reopening level is not preferred
/// to one that is open, however full it is.
#[test]
fn a_closed_server_that_has_not_fallen_far_enough_takes_no_new_player() {
    let mut gates = Gates::default();
    assert_eq!(
        placed(&[server("a", 4, 4, 4), server("b", 0, 4, 4)], &mut gates, 2),
        Ok("b")
    );
    // a lost one player (3), b has 1: a is still closed, the player goes to b.
    let now = [server("a", 3, 4, 4), server("b", 1, 4, 4)];
    assert_eq!(placed(&now, &mut gates, 2), Ok("b"));
    // a fell to 2: it is open again and, being the fullest, takes the player.
    let later = [server("a", 2, 4, 4), server("b", 1, 4, 4)];
    assert_eq!(placed(&later, &mut gates, 2), Ok("a"));
}

#[test]
fn when_every_server_is_closed_the_emptiest_below_its_hard_cap_takes_the_overflow() {
    let candidates = [
        server("a", 4, 4, 6),
        server("b", 5, 4, 6),
        server("c", 4, 4, 6),
    ];
    let p = place(&candidates, &mut Gates::default(), 2, TieBreak::Order).unwrap();
    // a and c tie on 4 players: the first listed.
    assert_eq!((p.backend, p.overflow), ("a", true));
}

#[test]
fn an_open_server_is_not_an_overflow() {
    let p = place(
        &[server("a", 1, 4, 6)],
        &mut Gates::default(),
        2,
        TieBreak::Order,
    )
    .unwrap();
    assert!(!p.overflow);
}

#[test]
fn a_server_never_takes_more_than_its_hard_cap() {
    // The hard cap is the soft cap by default in the configuration: no overflow.
    let full = [server("a", 4, 4, 4), server("b", 4, 4, 4)];
    assert_eq!(placed(&full, &mut Gates::default(), 2), Err(NoRoom));
    // With a hard cap, the overflow goes up to it and stops.
    let over = [server("a", 5, 4, 6), server("b", 6, 4, 6)];
    assert_eq!(placed(&over, &mut Gates::default(), 2), Ok("a"));
    let capped = [server("a", 6, 4, 6), server("b", 6, 4, 6)];
    assert_eq!(placed(&capped, &mut Gates::default(), 2), Err(NoRoom));
}

#[test]
fn no_server_means_no_room() {
    assert_eq!(placed(&[], &mut Gates::default(), 2), Err(NoRoom));
    assert_eq!(NoRoom.to_string(), "no server of this role has room");
}

#[test]
fn an_unavailable_server_is_left_out_and_its_players_stay_counted() {
    let mut a = server("a", 3, 4, 4);
    a.available = false;
    let candidates = [a, server("b", 0, 4, 4)];
    let mut gates = Gates::default();
    // Without the draining server, a player goes to b even though a is fuller.
    assert_eq!(placed(&candidates, &mut gates, 2), Ok("b"));
    // Only unavailable servers: no room.
    assert_eq!(placed(&[a], &mut gates, 2), Err(NoRoom));
    // The gates still follow the counts of a server that is passed over.
    a.players = 4;
    placed(&[a], &mut gates, 2).unwrap_err();
    assert!(gates.is_closed("a"));
}

#[test]
fn a_tie_is_broken_by_the_quickest_server_or_by_the_order() {
    let mut slow = server("slow", 2, 8, 8);
    slow.ping = Some(MS(80));
    let mut quick = server("quick", 2, 8, 8);
    quick.ping = Some(MS(5));
    let unmeasured = server("unmeasured", 2, 8, 8);
    let by = |tie_break, list: &[Candidate<'static>]| {
        place(list, &mut Gates::default(), 2, tie_break)
            .unwrap()
            .backend
    };
    let list = [slow, unmeasured, quick];
    assert_eq!(by(TieBreak::Ping, &list), "quick");
    assert_eq!(by(TieBreak::Order, &list), "slow");
    // A measured server comes before one that is not; two unmeasured ones keep
    // their order.
    let list = [unmeasured, server("other", 2, 8, 8), slow];
    assert_eq!(by(TieBreak::Ping, &list), "slow");
    let list = [unmeasured, server("other", 2, 8, 8)];
    assert_eq!(by(TieBreak::Ping, &list), "unmeasured");
}

#[test]
fn the_ping_never_beats_the_fill_rule() {
    let mut quick = server("quick", 1, 8, 8);
    quick.ping = Some(MS(1));
    let mut slow = server("slow", 5, 8, 8);
    slow.ping = Some(MS(200));
    let p = place(&[quick, slow], &mut Gates::default(), 2, TieBreak::Ping).unwrap();
    assert_eq!(p.backend, "slow");
}

#[test]
fn the_ping_breaks_a_tie_among_the_overflow_too() {
    let mut a = server("a", 4, 4, 6);
    a.ping = Some(MS(50));
    let mut b = server("b", 4, 4, 6);
    b.ping = Some(MS(10));
    let p = place(&[a, b], &mut Gates::default(), 2, TieBreak::Ping).unwrap();
    assert_eq!((p.backend, p.overflow), ("b", true));
}

#[test]
fn a_server_named_on_purpose_may_pass_the_soft_cap_but_not_the_hard_cap() {
    assert_eq!(allow_named(&server("a", 3, 4, 6)), Ok(()));
    assert_eq!(allow_named(&server("a", 4, 4, 6)), Ok(()));
    assert_eq!(allow_named(&server("a", 5, 4, 6)), Ok(()));
    assert_eq!(allow_named(&server("a", 6, 4, 6)), Err(Refused::AtHardCap));
    // With no hard cap written it is the soft cap: nothing beyond it.
    assert_eq!(allow_named(&server("a", 4, 4, 4)), Err(Refused::AtHardCap));
    let mut draining = server("a", 0, 4, 6);
    draining.available = false;
    assert_eq!(allow_named(&draining), Err(Refused::Unavailable));
    assert_eq!(Refused::AtHardCap.to_string(), "the server is full");
    assert_eq!(
        Refused::Unavailable.to_string(),
        "the server is not taking players"
    );
}

#[test]
fn the_hub_takes_a_lobby_first_and_a_sublobby_only_when_no_lobby_can() {
    let lobbies = [server("lobby-1", 3, 10, 10), server("lobby-2", 0, 10, 10)];
    let queues = [server("queue-1", 5, 10, 10)];
    let hub = |lobbies: &[Candidate<'static>], queues: &[Candidate<'static>]| {
        place_first(
            &[lobbies, queues],
            &mut Gates::default(),
            2,
            TieBreak::Order,
        )
        .map(|p| p.backend)
    };
    assert_eq!(hub(&lobbies, &queues), Ok("lobby-1"));
    let full = [server("lobby-1", 10, 10, 10)];
    assert_eq!(hub(&full, &queues), Ok("queue-1"));
    let none = [server("queue-1", 10, 10, 10)];
    assert_eq!(hub(&full, &none), Err(NoRoom));
    assert_eq!(hub(&[], &[]), Err(NoRoom));
    // A lobby past its soft cap but below its hard cap still beats a sublobby.
    let overflow = [server("lobby-1", 10, 10, 12)];
    assert_eq!(hub(&overflow, &queues), Ok("lobby-1"));
}

#[test]
fn every_tier_keeps_its_gates_up_to_date() {
    let mut gates = Gates::default();
    let lobby = [server("lobby", 0, 10, 10)];
    let queue = [server("queue", 10, 10, 10)];
    place_first(&[&lobby, &queue], &mut gates, 2, TieBreak::Order).unwrap();
    assert!(gates.is_closed("queue"));
}

#[test]
fn a_server_removed_from_the_configuration_is_forgotten() {
    let mut gates = Gates::default();
    gates.update(&server("a", 4, 4, 4), 2);
    gates.update(&server("b", 4, 4, 4), 2);
    gates.retain(|name| name == "b");
    assert!(!gates.is_closed("a"));
    assert!(gates.is_closed("b"));
}
