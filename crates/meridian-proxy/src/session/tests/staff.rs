//! `/send`, `/glist` and `/alert` typed by a relayed player: refused without the
//! permission, answered with it, and never passed on to the backend.
use super::*;
use horizon_protocol::packets::play;
use ids::{configuration as cfg, play as ids_play};
use meridian_config::{CommandSettings, Permissions, ProxyCommand};

const STAFF: &str = r#"
[group.staff]
allow = ["meridian.send", "meridian.glist", "meridian.alert"]
[[player]]
name = "Alex"
groups = ["staff"]
"#;

fn staff_config() -> Config {
    Config {
        commands: CommandSettings {
            enabled: vec![
                ProxyCommand::Server,
                ProxyCommand::Hub,
                ProxyCommand::Send,
                ProxyCommand::Glist,
                ProxyCommand::Alert,
            ],
            ..CommandSettings::default()
        },
        ..routed()
    }
}

fn payload(body: Vec<u8>) -> Vec<u8> {
    body[1..].to_vec()
}

fn command(text: &str) -> Vec<u8> {
    payload(
        play::Serverbound::ChatCommand(play::ChatCommand {
            command: text.into(),
        })
        .encode(),
    )
}

fn chat(text: &str) -> Vec<u8> {
    payload(
        play::Clientbound::SystemChat(play::SystemChat {
            content: text::literal(text),
            overlay: false,
        })
        .encode(),
    )
}

/// What the player types, and what the proxy says back, line by line.
fn types(text: &str, replies: &[&str]) -> Vec<Step> {
    let mut steps = vec![Step::send(
        ids_play::serverbound::CHAT_COMMAND,
        command(text),
    )];
    steps.extend(
        replies
            .iter()
            .map(|r| Step::expect_exactly(ids_play::clientbound::SYSTEM_CHAT, chat(r))),
    );
    steps
}

/// Alex logs in, with `permissions` (the text of `permissions.toml`) in force, and runs
/// `steps` in play. The backend must see no command: it only waits for the close.
fn alex_types(permissions: &str, steps: Vec<Step>) -> Sessions {
    alex_types_with(permissions, |_| {}, steps)
}

/// As [`alex_types`], after `prepare` has set the sessions up (the operators the servers listed).
fn alex_types_with(
    permissions: &str,
    prepare: impl FnOnce(&Sessions),
    steps: Vec<Step>,
) -> Sessions {
    let backend = FakeBackend {
        forwarding_secret: Some(SECRET.as_bytes().to_vec()),
        after_login: vec![
            finish_configuration(),
            Step::Expect {
                id: cfg::serverbound::FINISH_CONFIGURATION,
            },
            Step::ExpectClose,
        ],
        ..FakeBackend::default()
    };
    let (connect, seen) = connector(backend, Recorder::new());
    let (session, player) = logged_in("Alex", client::AuthKind::Offline, None);
    let mut script = vec![
        Step::Expect {
            id: cfg::clientbound::FINISH_CONFIGURATION,
        },
        acknowledge_configuration(),
    ];
    script.extend(steps);
    script.push(Step::Close);
    let player = meridian_testkit::peer::spawn(player, script, Dir::ToClient, Recorder::new());
    let sessions = Sessions::default();
    sessions
        .permissions
        .install(Permissions::parse(permissions).unwrap(), None);
    prepare(&sessions);
    serve(session, peer(), &staff_config(), &*connect, &sessions);
    player.join().unwrap().unwrap();
    let (_, _, backend) = seen.lock().unwrap().take().unwrap();
    backend.join().unwrap().unwrap();
    sessions
}

#[test]
fn glist_lists_the_players_by_server_for_a_player_who_has_the_node() {
    alex_types(
        STAFF,
        [
            types("glist", &["1 player online:\nlobby (1): Alex"]),
            types("glist lobby", &["lobby (1): Alex"]),
            types("glist east", &["east (0)"]),
            types("glist nowhere", &["There is no server called nowhere."]),
        ]
        .concat(),
    );
}

#[test]
fn a_player_without_the_node_is_refused_and_the_backend_never_hears_of_it() {
    alex_types(
        "",
        [
            types("glist", &["You do not have permission to use /glist."]),
            types("alert hi", &["You do not have permission to use /alert."]),
            types(
                "send all east",
                &["You do not have permission to use /send."],
            ),
        ]
        .concat(),
    );
}

#[test]
fn an_alert_is_shown_to_the_players_and_counted() {
    alex_types(
        STAFF,
        types(
            "alert maintenance in five minutes",
            &[
                "[Alert] maintenance in five minutes",
                "Alert shown to 1 player.",
            ],
        ),
    );
    // To a server where no one is: nobody is told.
    alex_types(
        STAFF,
        types("alert server:east hello", &["Alert shown to 0 players."]),
    );
    alex_types(
        STAFF,
        types(
            "alert hello server:nowhere",
            &["There is no server called nowhere."],
        ),
    );
}

#[test]
fn send_says_what_is_wrong_before_it_moves_anyone() {
    alex_types(
        STAFF,
        [
            types("send Nobody east", &["Nobody is not online."]),
            types("send Alex nowhere", &["There is no server called nowhere."]),
            types("send Alex lobby", &["Already on lobby."]),
            types("send server:east lobby", &["There is no one to send."]),
            types(
                "send server:nowhere lobby",
                &["There is no server called nowhere."],
            ),
            types(
                "send Alex",
                &["Usage: /send <player|all|server:name> <server>"],
            ),
        ]
        .concat(),
    );
}

#[test]
fn send_starts_the_move_and_the_player_is_told_how_it_ended() {
    // No sidecar is configured, so the move fails at its first check: the player is told
    // at once that it started, then that it could not be done, then how many failed.
    let mut steps = types("send Alex east", &["Sending 1 player to east..."]);
    steps.push(Step::Expect {
        id: ids_play::clientbound::SYSTEM_CHAT,
    });
    steps.push(Step::expect_exactly(
        ids_play::clientbound::SYSTEM_CHAT,
        chat("Sent to east, 1 failed; the log says why."),
    ));
    let sessions = alex_types(STAFF, steps);
    assert_eq!(sessions.count(), 0);
}

// ---- The operators of the servers (`[operators]`).

const OPERATOR_GROUP: &str = r#"
[group.operator]
allow = ["meridian.glist"]
"#;

/// Alex is listed as an operator of `backend`, with `level`, by a server read through its
/// sidecar, and the table is on.
fn operator_of(backend: &'static str, level: u8) -> impl FnOnce(&Sessions) {
    move |sessions: &Sessions| {
        sessions
            .operators
            .configure(&meridian_config::OperatorSettings {
                from_servers: true,
                ..meridian_config::OperatorSettings::default()
            });
        sessions.operators.replace(
            backend,
            vec![crate::operators::Op {
                id: Some(horizon_protocol::crypto::offline_uuid("Alex").to_be_bytes()),
                name: "Alex".into(),
                level,
            }],
            std::time::Instant::now(),
        );
    }
}

/// A player who is an operator of the server they are on has the nodes of the operator group
/// when they type a command in game, without being in any group of the file.
#[test]
fn an_operator_of_the_server_they_are_on_may_use_what_the_operator_group_may() {
    alex_types_with(
        OPERATOR_GROUP,
        operator_of("lobby", 4),
        types("glist lobby", &["lobby (1): Alex"]),
    );
}

/// Not an operator of the server they are on, or not at the level asked, or with the table off:
/// refused, as any player.
#[test]
fn the_operator_of_another_server_or_of_too_low_a_level_is_refused() {
    let refused = || types("glist", &["You do not have permission to use /glist."]);
    alex_types_with(OPERATOR_GROUP, operator_of("east", 4), refused());
    alex_types_with(OPERATOR_GROUP, operator_of("lobby", 3), refused());
    alex_types_with(
        OPERATOR_GROUP,
        |sessions| {
            operator_of("lobby", 4)(sessions);
            sessions
                .operators
                .configure(&meridian_config::OperatorSettings::default());
        },
        refused(),
    );
}
