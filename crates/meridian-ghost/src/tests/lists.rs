//! The player list, boss bars and scoreboard: replaced so that the ghost ends as
//! the target.
use super::*;
use horizon_protocol::packets::play::BossAction;

/// A Tab List packet from its bytes: two NBT string tags (type 8, length, text).
fn tab_lines(header: &str, footer: &str) -> play::TabList {
    let mut payload = Vec::new();
    for text in [header, footer] {
        payload.push(8);
        payload.extend_from_slice(&(text.len() as u16).to_be_bytes());
        payload.extend_from_slice(text.as_bytes());
    }
    match play::Clientbound::decode(ids::TAB_LIST, &payload).unwrap() {
        C::TabList(lines) => lines,
        other => panic!("{other:?}"),
    }
}

/// Bits of `ClientboundPlayerInfoUpdatePacket.Action`.
const ADD: u8 = 1;
const GAME_MODE: u8 = 4;
const LISTED: u8 = 8;
const LATENCY: u8 = 16;
const DISPLAY_NAME: u8 = 32;

fn info(actions: u8, uuid: u128, name: &str, latency: i32) -> Packet {
    let lines = tab_lines(name, "");
    p(C::PlayerInfoUpdate(play::PlayerInfoUpdate {
        actions,
        entries: vec![play::PlayerInfo {
            uuid,
            name: Some(name.into()),
            properties: Vec::new(),
            chat_session: None,
            game_mode: Some(0),
            listed: Some(true),
            latency: Some(latency),
            display_name: Some(Some(lines.header)),
            list_order: Some(0),
            show_hat: Some(true),
        }],
    }))
}

fn bar(id: u128, progress: f32, color: i32) -> Packet {
    p(C::BossEvent(play::BossEvent {
        id,
        action: BossAction::Add {
            name: tab_lines("Boss", "").header,
            progress,
            color,
            overlay: 0,
            darken: false,
            music: false,
            fog: false,
        },
    }))
}

fn objective(name: &str, method: i8) -> Packet {
    let display = (method != 1).then(|| (tab_lines(name, "").header, 0, None));
    p(C::SetObjective(play::SetObjective {
        name: name.into(),
        method,
        display,
    }))
}

fn score(owner: &str, objective: &str, score: i32) -> Packet {
    p(C::SetScore(play::SetScore {
        owner: owner.into(),
        objective: objective.into(),
        score,
        display: None,
        number_format: None,
    }))
}

fn display(slot: i32, objective: &str) -> Packet {
    p(C::SetDisplayObjective(play::SetDisplayObjective {
        slot,
        objective: objective.into(),
    }))
}

fn team(name: &str, method: i8, color: Option<i32>, players: &[&str]) -> Packet {
    let lines = tab_lines(name, "");
    let parameters = matches!(method, 0 | 2).then(|| play::TeamParameters {
        display_name: lines.header.clone(),
        prefix: lines.footer.clone(),
        suffix: lines.footer.clone(),
        name_tag_visibility: 0,
        collision_rule: 0,
        color,
        options: 0,
    });
    p(C::SetPlayerTeam(play::SetPlayerTeam {
        name: name.into(),
        method,
        parameters,
        players: players.iter().map(|&s| s.to_owned()).collect(),
    }))
}

#[test]
fn the_player_list_keeps_who_stays_and_changes_only_what_differs() {
    let all = ADD | GAME_MODE | LISTED | LATENCY;
    let old = vec![
        login(1, 2),
        info(all, 0xa, "Alex", 10),
        info(all, 0xb, "Bea", 10),
    ];
    let new = vec![
        login(1, 2),
        info(all, 0xa, "Alex", 10),
        info(LATENCY, 0xa, "Alex", 80),
        info(all, 0xc, "Cid", 5),
        p(C::TabList(tab_lines("Welcome", "to game-1"))),
    ];
    let (_, delta) = switch(&old, &new);
    let decoded: Vec<C> = delta
        .iter()
        .filter(|p| p.id != ids::BUNDLE_DELIMITER)
        .map(|p| p.decode().unwrap())
        .collect();
    let C::PlayerInfoRemove(removed) = &decoded[0] else {
        panic!("{decoded:?}");
    };
    assert_eq!(removed.uuids, vec![0xb]);
    let updates: Vec<(u8, u128)> = decoded
        .iter()
        .filter_map(|p| match p {
            C::PlayerInfoUpdate(u) => Some((u.actions, u.entries[0].uuid)),
            _ => None,
        })
        .collect();
    // Alex only gets the latency; Cid is added with everything the target sent.
    assert_eq!(updates, vec![(LATENCY, 0xa), (all, 0xc)]);
    assert!(decoded.iter().any(|p| matches!(p, C::TabList(_))));
}

#[test]
fn an_entry_with_a_value_the_target_never_sent_is_added_again() {
    let old = vec![
        login(1, 2),
        info(ADD | GAME_MODE | DISPLAY_NAME, 0xa, "Alex", 0),
    ];
    let new = vec![login(1, 2), info(ADD | GAME_MODE, 0xa, "Alex", 0)];
    let (_, delta) = switch(&old, &new);
    let order = ids_of(&delta);
    assert_eq!(
        order,
        vec![ids::PLAYER_INFO_REMOVE, ids::PLAYER_INFO_UPDATE]
    );
}

#[test]
fn boss_bars_are_removed_added_and_updated_by_part() {
    let old = vec![login(1, 2), bar(1, 0.5, 0), bar(2, 1.0, 0)];
    let new = vec![login(1, 2), bar(1, 0.8, 3), bar(3, 1.0, 1)];
    let (_, delta) = switch(&old, &new);
    let actions: Vec<(u128, &'static str)> = delta
        .iter()
        .filter_map(|p| match p.decode() {
            Ok(C::BossEvent(e)) => Some((
                e.id,
                match e.action {
                    BossAction::Add { .. } => "add",
                    BossAction::Remove => "remove",
                    BossAction::UpdateProgress { .. } => "progress",
                    BossAction::UpdateName { .. } => "name",
                    BossAction::UpdateStyle { .. } => "style",
                    BossAction::UpdateProperties { .. } => "properties",
                },
            )),
            _ => None,
        })
        .collect();
    assert_eq!(
        actions,
        vec![(2, "remove"), (1, "progress"), (1, "style"), (3, "add")]
    );
}

#[test]
fn a_removed_objective_takes_its_scores_and_slot_with_it() {
    let old = vec![
        login(1, 2),
        objective("kills", 0),
        score("Alex", "kills", 3),
        display(1, "kills"),
        objective("deaths", 0),
        score("Alex", "deaths", 1),
        score("Bea", "deaths", 2),
    ];
    let new = vec![
        login(1, 2),
        objective("deaths", 0),
        score("Alex", "deaths", 4),
        objective("time", 0),
        score("Bea", "time", 60),
        display(1, "time"),
    ];
    let (after, delta) = switch(&old, &new);
    let order = ids_of(&delta);
    // One removal clears "kills", its score and its slot on the client.
    assert_eq!(
        order.iter().filter(|&&id| id == ids::SET_OBJECTIVE).count(),
        2,
        "remove kills, add time: {order:?}"
    );
    assert_eq!(
        order.iter().filter(|&&id| id == ids::RESET_SCORE).count(),
        1,
        "only Bea's deaths, which the target lacks: {order:?}"
    );
    assert_eq!(
        after.scoreboard.display.get(&1).map(String::as_str),
        Some("time")
    );
}

#[test]
fn a_player_changes_team_and_an_empty_team_goes() {
    let old = vec![
        login(1, 2),
        team("red", 0, Some(12), &["Alex", "Bea"]),
        team("blue", 0, Some(9), &["Cid"]),
        team("old", 0, None, &[]),
    ];
    let new = vec![
        login(1, 2),
        team("red", 0, Some(12), &["Bea"]),
        team("blue", 0, Some(1), &["Cid", "Alex"]),
    ];
    let (after, delta) = switch(&old, &new);
    let methods: Vec<(String, i8)> = delta
        .iter()
        .filter_map(|p| match p.decode() {
            Ok(C::SetPlayerTeam(t)) => Some((t.name, t.method)),
            _ => None,
        })
        .collect();
    assert_eq!(
        methods,
        vec![
            ("old".into(), 1),
            ("red".into(), 4),
            ("blue".into(), 2),
            ("blue".into(), 3),
        ]
    );
    assert!(after.scoreboard.teams["blue"].players.contains("Alex"));
}

#[test]
fn joining_a_team_leaves_the_one_before() {
    let ghost = view(
        View::ghost(),
        &[
            login(1, 2),
            team("red", 0, None, &["Alex"]),
            team("blue", 0, None, &[]),
            team("blue", 3, None, &["Alex"]),
        ],
    );
    assert!(ghost.scoreboard.teams["red"].players.is_empty());
    assert!(ghost.scoreboard.teams["blue"].players.contains("Alex"));
}
