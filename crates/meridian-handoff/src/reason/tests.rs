use super::*;
use horizon_protocol::{ids, text::Language};
use std::collections::BTreeSet;

const SHUTDOWN: &str = "multiplayer.disconnect.server_shutdown";

/// A disconnect whose reason is `tag`.
fn disconnect_of(tag: Tag) -> Disconnect {
    Disconnect { reason: tag }
}

fn string(text: &str) -> Tag {
    text::literal(text)
}

fn compound<const N: usize>(entries: [(&str, Tag); N]) -> Tag {
    Tag::compound(entries)
}

fn english(pairs: &[(&str, &str)]) -> Language {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

/// The line a client with `language` shows for the notice's chat packet, read back from the
/// packet as it goes on the wire.
fn shown(notice: &Notice, language: &Language) -> String {
    let body = play::Clientbound::SystemChat(notice.chat()).encode();
    let mut reader = Reader::new(&body);
    let id = reader.varint().unwrap();
    assert_eq!(id, ids::play::clientbound::SYSTEM_CHAT);
    match play::Clientbound::decode(id, reader.rest()).unwrap() {
        play::Clientbound::SystemChat(chat) => {
            assert!(!chat.overlay);
            text::plain(&chat.content, language)
        }
        other => panic!("{other:?}"),
    }
}

/// The tag a notice sends.
fn sent(notice: &Notice) -> Tag {
    notice.chat().content
}

const BEFORE: &str = "You were disconnected from real: ";
const AFTER: &str = ". You were moved to real2.";

#[test]
fn a_translation_key_is_left_for_the_client_to_translate() {
    let disconnect = disconnect_of(text::translatable(SHUTDOWN, &[]));
    let reason = Reason::of(&disconnect);
    // For the logs, the key is what there is to say.
    assert_eq!(reason.text(), SHUTDOWN);
    let notice = Notice::around(BEFORE, &reason, AFTER);
    assert!(matches!(notice.0, Body::Component { .. }), "{notice:?}");
    // A client that has the key shows its words, not the key.
    let language = english(&[(SHUTDOWN, "Server closed")]);
    assert_eq!(
        shown(&notice, &language),
        "You were disconnected from real: Server closed. You were moved to real2."
    );
    // One that does not has nothing better than the key, as before.
    assert_eq!(
        shown(&notice, &Language::new()),
        format!("{BEFORE}{SHUTDOWN}{AFTER}")
    );
    assert_eq!(notice.to_string(), format!("{BEFORE}{SHUTDOWN}{AFTER}"));
}

#[test]
fn a_plain_text_reason_gives_the_text_it_always_gave_byte_for_byte() {
    let disconnect = disconnect_of(string("You have been kicked"));
    let reason = Reason::of(&disconnect);
    let notice = Notice::around(BEFORE, &reason, AFTER);
    let line = "You were disconnected from real: You have been kicked. You were moved to real2.";
    assert_eq!(notice, Notice::from(line));
    // The chat packet is the one the proxy always sent.
    let old = play::Clientbound::SystemChat(play::SystemChat {
        content: text::literal(line),
        overlay: false,
    })
    .encode();
    assert_eq!(play::Clientbound::SystemChat(notice.chat()).encode(), old);
    assert_eq!(
        play::Clientbound::Disconnect(notice.disconnect()).encode(),
        play::Clientbound::Disconnect(disconnect_of(text::literal(line))).encode()
    );
    // A reason that was only ever text.
    assert_eq!(
        Notice::around(BEFORE, &Reason::from("You have been kicked"), AFTER),
        Notice::from(line)
    );
}

#[test]
fn a_component_with_styling_and_no_key_is_text_too() {
    let styled = compound([("text", string("Bye")), ("color", string("red"))]);
    let reason = Reason::of(&disconnect_of(styled));
    assert_eq!(reason.text(), "Bye");
    assert_eq!(Notice::around("a: ", &reason, "."), Notice::from("a: Bye."));
}

#[test]
fn arguments_and_a_nested_component_are_kept() {
    // A translation with an argument.
    let disconnect = disconnect_of(text::translatable(
        "multiplayer.disconnect.kicked_by",
        &["Alex"],
    ));
    let notice = Notice::around(BEFORE, &Reason::of(&disconnect), AFTER);
    assert!(matches!(notice.0, Body::Component { .. }));
    let language = english(&[("multiplayer.disconnect.kicked_by", "Kicked by %s")]);
    assert_eq!(
        shown(&notice, &language),
        "You were disconnected from real: Kicked by Alex. You were moved to real2."
    );
    // A translation inside the extra of another component.
    let nested = compound([
        ("text", string("")),
        (
            "extra",
            Tag::List(vec![compound([("translate", string("a.b"))])]),
        ),
    ]);
    let notice = Notice::around("pre ", &Reason::of(&disconnect_of(nested)), " post");
    assert!(matches!(notice.0, Body::Component { .. }));
    assert_eq!(shown(&notice, &english(&[("a.b", "Hi")])), "pre Hi post");
    // A component as an argument, and an argument that is a number, wrapped as a list of mixed
    // kinds wraps it (`{"": value}`).
    let mixed = compound([
        ("translate", string("a.c")),
        (
            "with",
            Tag::List(vec![
                compound([("text", string("Alex"))]),
                compound([("", Tag::Int(3))]),
            ]),
        ),
    ]);
    let notice = Notice::around("", &Reason::of(&disconnect_of(mixed)), "");
    assert_eq!(
        shown(&notice, &english(&[("a.c", "%s has %s")])),
        "Alex has 3"
    );
}

/// What a compromised server could put in its reason is not carried to the chat of a player who
/// has gone to another server: a click that runs a command as the player, a link, a hover, an
/// insertion into the chat box, a font, anything unknown.
#[test]
fn what_could_run_or_open_something_is_left_out_of_the_chat() {
    let click = || {
        compound([
            ("action", string("run_command")),
            ("value", string("/op Mallory")),
        ])
    };
    let hostile = || {
        compound([
            ("translate", string("multiplayer.disconnect.kicked")),
            ("clickEvent", click()),
            ("hoverEvent", compound([("action", string("show_text"))])),
            ("insertion", string("/stop")),
            ("font", string("minecraft:illageralt")),
            ("bogus", string("x")),
            ("color", string("red")),
            (
                "with",
                Tag::List(vec![compound([
                    ("text", string("Alex")),
                    ("clickEvent", click()),
                ])]),
            ),
            (
                "extra",
                Tag::List(vec![compound([
                    ("text", string("more")),
                    ("clickEvent", click()),
                    ("score", compound([("name", string("@p"))])),
                    ("selector", string("@a")),
                    ("keybind", string("key.inventory")),
                ])]),
            ),
        ])
    };
    let reason = Reason::of(&disconnect_of(hostile()));
    let notice = Notice::around(BEFORE, &reason, AFTER);
    assert!(matches!(notice.0, Body::Component { .. }));
    let allowed: BTreeSet<String> = ["text", "translate", "fallback", "with", "extra", "color"]
        .iter()
        .map(|k| (*k).to_owned())
        .collect();
    let keys = keys_of(&sent(&notice));
    assert!(keys.is_subset(&allowed), "{keys:?}");
    // What is kept is what the client needs to say the same words.
    let language = english(&[("multiplayer.disconnect.kicked", "Kicked by %s")]);
    assert_eq!(
        shown(&notice, &language),
        "You were disconnected from real: Kicked by Alexmore. You were moved to real2."
    );
    // The disconnect screen runs no click, and that message goes back as the server sent it.
    assert_eq!(
        play::Clientbound::Disconnect(reason.disconnect()).encode(),
        play::Clientbound::Disconnect(disconnect_of(hostile())).encode()
    );
}

#[test]
fn the_component_is_whole_network_nbt() {
    let disconnect = disconnect_of(text::translatable(SHUTDOWN, &[]));
    let notice = Notice::around(BEFORE, &Reason::of(&disconnect), AFTER);
    let Body::Component { tag, .. } = &notice.0 else {
        panic!("{notice:?}");
    };
    // Written as network NBT and read back by the protocol crate's own reader, down to the last
    // byte, as the same tag.
    let mut writer = Writer::new();
    writer.nbt(tag);
    assert_eq!(writer.data[0], 10, "a root compound");
    let mut reader = Reader::new(&writer.data);
    assert_eq!(&reader.nbt().unwrap(), tag);
    assert_eq!(reader.remaining(), 0);
    assert!(sendable(tag));
}

#[test]
fn the_text_around_may_hold_any_character() {
    let disconnect = disconnect_of(text::translatable(SHUTDOWN, &[]));
    let before = "Du serveur « é » 😀 : ";
    let notice = Notice::around(before, &Reason::of(&disconnect), " – fin");
    assert!(matches!(notice.0, Body::Component { .. }));
    assert_eq!(
        shown(&notice, &english(&[(SHUTDOWN, "Arrêt")])),
        "Du serveur « é » 😀 : Arrêt – fin"
    );
}

/// A component nested `levels` deep, with a translation at the bottom.
fn deep(levels: usize) -> Tag {
    let mut tag = compound([("translate", string(SHUTDOWN))]);
    for _ in 0..levels {
        tag = compound([("text", string("")), ("extra", Tag::List(vec![tag]))]);
    }
    tag
}

#[test]
fn an_empty_odd_huge_or_deep_reason_falls_back_to_text_and_never_panics() {
    // Nothing to say.
    let empty = Reason::of(&disconnect_of(string("")));
    assert_eq!(Notice::around("a", &empty, "b"), Notice::from("ab"));
    assert_eq!(
        Notice::around("a", &Reason::from(""), "b"),
        Notice::from("ab")
    );
    // A tag that is neither a string nor a compound.
    for odd in [Tag::Int(7), Tag::End, Tag::List(vec![string("x")])] {
        let reason = Reason::of(&disconnect_of(odd));
        assert!(reason.tag.is_none());
        assert_eq!(
            Notice::around("a ", &reason, " b"),
            Notice::from(format!("a {reason} b"))
        );
    }
    // A key too long to be kept as a component is shown as text.
    let key = "k".repeat(MAX_COMPONENT + 10);
    let huge = Reason::of(&disconnect_of(text::translatable(&key, &[])));
    assert!(huge.tag.is_none());
    assert_eq!(
        Notice::around("a ", &huge, " b"),
        Notice::from(format!("a {key} b"))
    );
    // A component with more levels than are cleaned is shown as text, not cleaned in part. Up to
    // the limit it is a component.
    let shallow = Reason::of(&disconnect_of(deep(MAX_DEPTH / 2)));
    assert!(matches!(
        Notice::around("a ", &shallow, " b").0,
        Body::Component { .. }
    ));
    // Past the limit, and past the 512 levels a container may nest (the writer refuses those).
    for levels in [MAX_DEPTH + 1, 100, 280, 510, 600] {
        let reason = Reason::of(&disconnect_of(deep(levels)));
        let notice = Notice::around("a ", &reason, " b");
        assert_eq!(
            notice,
            Notice::from(format!("a {reason} b")),
            "{levels} levels"
        );
    }
    // More parts than are cleaned.
    let wide = compound([
        ("translate", string("a.b")),
        (
            "extra",
            Tag::List(
                (0..=MAX_PARTS)
                    .map(|_| compound([("text", string("x"))]))
                    .collect(),
            ),
        ),
    ]);
    let reason = Reason::of(&disconnect_of(wide));
    assert!(matches!(
        Notice::around("a ", &reason, " b").0,
        Body::Text(_)
    ));
}

#[test]
fn a_notice_is_always_shown_as_its_words() {
    let component = Notice::around(
        BEFORE,
        &Reason::of(&disconnect_of(text::translatable(SHUTDOWN, &[]))),
        AFTER,
    );
    assert_eq!(component.to_string(), format!("{BEFORE}{SHUTDOWN}{AFTER}"));
    assert_eq!(Notice::from("x").to_string(), "x");
}

#[test]
fn the_reason_goes_back_to_the_client_as_the_backend_sent_it() {
    let original = disconnect_of(text::translatable(SHUTDOWN, &[]));
    let reason = Reason::of(&original);
    assert_eq!(
        play::Clientbound::Disconnect(reason.disconnect()).encode(),
        play::Clientbound::Disconnect(original).encode()
    );
    // A reason that is only text.
    assert_eq!(
        play::Clientbound::Disconnect(Reason::from("Bye").disconnect()).encode(),
        play::Clientbound::Disconnect(disconnect_of(text::literal("Bye"))).encode()
    );
}

/// Every compound of `tag` says something: it has `text` or `translate`. A compound with neither
/// is `{}` once it is a component, and a client refuses it.
fn every_part_says_something(tag: &Tag) {
    match tag {
        Tag::Compound(map) => {
            assert!(
                map.contains_key(&"text".into()) || map.contains_key(&"translate".into()),
                "a part says nothing: {tag:?}"
            );
            map.values().for_each(every_part_says_something);
        }
        Tag::List(items) => items.iter().for_each(every_part_says_something),
        _ => {}
    }
}

/// What the filter drops can leave a part with nothing: a part made only of a score, a selector,
/// a key binding or an NBT path, or whose `text` is not a string. It comes out with an empty text,
/// not as `{}`.
#[test]
fn a_part_left_with_nothing_says_an_empty_text() {
    let language = english(&[("a.b", "[%s]")]);
    for reason in [
        // A key binding as an argument.
        compound([
            ("translate", string("a.b")),
            (
                "with",
                Tag::List(vec![compound([("keybind", string("key.jump"))])]),
            ),
        ]),
        // A selector in the extra.
        compound([
            ("translate", string("a.b")),
            (
                "extra",
                Tag::List(vec![compound([("selector", string("@a"))])]),
            ),
        ]),
        // A score and an NBT path, a text that is a list.
        compound([
            ("translate", string("a.b")),
            (
                "extra",
                Tag::List(vec![
                    compound([("score", compound([("name", string("@p"))]))]),
                    compound([("nbt", string("Health"))]),
                    compound([("text", Tag::List(vec![string("x")]))]),
                    compound([]),
                ]),
            ),
        ]),
    ] {
        let notice = Notice::around("", &Reason::of(&disconnect_of(reason)), "");
        assert!(matches!(notice.0, Body::Component { .. }));
        every_part_says_something(&sent(&notice));
        // And nothing of what was dropped shows.
        assert_eq!(shown(&notice, &language), "[]");
    }
}

/// A `text` that is a number is shown as that number, as a game shows it; one that is another
/// kind of tag is empty.
#[test]
fn a_text_that_is_a_number_is_shown_and_any_other_is_empty() {
    let reason = compound([
        ("translate", string("a.b")),
        (
            "with",
            Tag::List(vec![
                compound([("text", Tag::Int(5))]),
                compound([("text", Tag::List(vec![string("x")]))]),
            ]),
        ),
    ]);
    let notice = Notice::around("", &Reason::of(&disconnect_of(reason)), "");
    every_part_says_something(&sent(&notice));
    assert_eq!(shown(&notice, &english(&[("a.b", "%s|%s")])), "5|");
}

/// The deepest a component may nest is `MAX_DEPTH` levels below the first: that one is a
/// component, one more is text.
#[test]
fn the_depth_limit_is_exactly_the_one_written() {
    let at_the_limit = Reason::of(&disconnect_of(deep(MAX_DEPTH)));
    let notice = Notice::around("a ", &at_the_limit, " b");
    assert!(matches!(notice.0, Body::Component { .. }), "{notice:?}");
    every_part_says_something(&sent(&notice));
    assert_eq!(shown(&notice, &english(&[(SHUTDOWN, "x")])), "a x b");
    let past = Reason::of(&disconnect_of(deep(MAX_DEPTH + 1)));
    assert!(matches!(Notice::around("a ", &past, " b").0, Body::Text(_)));
}
