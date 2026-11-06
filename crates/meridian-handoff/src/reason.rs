//! Why a backend disconnected a player, and the messages that quote it.
//!
//! A backend gives its reason as a text component, and the component may be a translation key
//! (`multiplayer.disconnect.server_shutdown`) that only the client's language file can turn into
//! words. Flattened to text by the proxy, which has no language file, it came out as the key.
//! A [`Reason`] keeps the component as it was sent, next to its flat text (for the logs), and a
//! [`Notice`] puts a cleaned copy of it between two pieces of text of the proxy's own, so that the
//! client does the translating. A reason that is plain text gives the plain text it always gave.
//!
//! The cleaning matters: a chat line shown after the player has gone to another server must not
//! carry what the server they left put in its component. Of a component, only `text`, `translate`
//! (with `fallback` and `with`), `extra` and `color` are kept, each part cleaned the same way;
//! click and hover events, the insertion, the font, scores, selectors, key bindings and anything
//! unknown are dropped, so that a server cannot put a link or a command to run in the chat of a
//! player who is on another one. A reason too large or too deep to be cleaned is shown as text.
use horizon_protocol::{
    buf::{Reader, Writer},
    packets::{configuration::Disconnect, play},
    text,
};
use horizon_world::nbt::Tag;
use std::fmt;

/// The largest reason kept as a component, in bytes of NBT as it was sent. A larger one is shown
/// as text.
const MAX_COMPONENT: usize = 4096;

/// How deep a component may nest, and how many parts it may have, to be cleaned. Beyond that the
/// reason is shown as text.
const MAX_DEPTH: usize = 8;
const MAX_PARTS: usize = 64;

/// The longest colour name kept (`dark_purple`; `#rrggbb` is shorter).
const MAX_COLOR: usize = 16;

/// What a backend said when it disconnected a player. Two reasons are equal when they say the
/// same words, whether or not the component behind them is kept.
#[derive(Clone, Debug)]
pub struct Reason {
    /// The reason flattened to text, without a language file: a translation key shows as itself.
    text: String,
    /// The reason as the backend sent it, if it is a string or a compound and not too large.
    tag: Option<Tag>,
}

impl Reason {
    /// The reason of a disconnect packet.
    pub fn of(disconnect: &Disconnect) -> Self {
        let text = text::plain(&disconnect.reason, &Default::default());
        let mut writer = Writer::new();
        writer.nbt(&disconnect.reason);
        let usable = matches!(disconnect.reason, Tag::String(_) | Tag::Compound(_))
            && writer.data.len() > 1
            && writer.data.len() <= MAX_COMPONENT;
        Reason {
            text,
            tag: usable.then(|| disconnect.reason.clone()),
        }
    }

    /// The reason as text, a translation key as itself.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The disconnect packet with this reason: the component as the backend sent it, or the text.
    /// The disconnect screen runs no click, so the component goes back whole.
    pub(crate) fn disconnect(&self) -> Disconnect {
        Disconnect {
            reason: self
                .tag
                .clone()
                .unwrap_or_else(|| text::literal(&self.text)),
        }
    }

    /// The reason as a clean component, if the client has something to translate in it: a
    /// component with a translation key. `None` for plain text, and for a reason that cannot be
    /// cleaned.
    fn translatable(&self) -> Option<Tag> {
        let mut cleaning = Cleaning::default();
        let clean = cleaning.component(self.tag.as_ref()?, 0)?;
        cleaning.translated.then_some(clean)
    }
}

impl PartialEq for Reason {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text
    }
}

impl Eq for Reason {}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

impl From<&str> for Reason {
    /// A reason that is plain text.
    fn from(text: &str) -> Self {
        Reason {
            text: text.to_owned(),
            tag: None,
        }
    }
}

/// A message to the player: text, or a component that holds a [`Reason`] for the client to
/// translate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice(Body);

#[derive(Clone, Debug, PartialEq, Eq)]
enum Body {
    Text(String),
    /// A component, and the same words as text with a translation key as itself.
    Component {
        tag: Tag,
        flat: String,
    },
}

impl Notice {
    /// `before`, the reason, then `after`. When the reason is plain text, or cannot be cleaned and
    /// written as a component, this is the text `before + reason + after`.
    pub fn around(before: &str, reason: &Reason, after: &str) -> Notice {
        let flat = format!("{before}{reason}{after}");
        if let Some(clean) = reason.translatable() {
            let tag = Tag::compound([
                ("text", text::literal(before)),
                (
                    "extra",
                    Tag::List(vec![clean, Tag::compound([("text", text::literal(after))])]),
                ),
            ]);
            if sendable(&tag) {
                return Notice(Body::Component { tag, flat });
            }
        }
        Notice(Body::Text(flat))
    }

    /// The chat line that says it.
    pub(crate) fn chat(&self) -> play::SystemChat {
        play::SystemChat {
            content: self.tag(),
            overlay: false,
        }
    }

    /// The disconnect packet that says it.
    pub(crate) fn disconnect(&self) -> Disconnect {
        Disconnect { reason: self.tag() }
    }

    fn tag(&self) -> Tag {
        match &self.0 {
            Body::Text(text) => text::literal(text),
            Body::Component { tag, .. } => tag.clone(),
        }
    }
}

impl fmt::Display for Notice {
    /// As text; a translation key shows as itself.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Body::Text(text) | Body::Component { flat: text, .. } => f.write_str(text),
        }
    }
}

impl From<String> for Notice {
    fn from(text: String) -> Self {
        Notice(Body::Text(text))
    }
}

impl From<&str> for Notice {
    fn from(text: &str) -> Self {
        Notice(Body::Text(text.to_owned()))
    }
}

/// Whether `tag` is written as network NBT and reads back, whole, as the same tag: what the
/// client will be sent has been read once by the protocol crate's own reader.
fn sendable(tag: &Tag) -> bool {
    let mut writer = Writer::new();
    writer.nbt(tag);
    // An unencodable tag is written as "no tag".
    if writer.data.len() <= 1 {
        return false;
    }
    let mut reader = Reader::new(&writer.data);
    reader.nbt().is_ok_and(|read| &read == tag) && reader.remaining() == 0
}

/// The state of the cleaning of one component.
#[derive(Default)]
struct Cleaning {
    parts: usize,
    /// A translation key was met.
    translated: bool,
}

impl Cleaning {
    /// A clean copy of `tag` as a compound, or `None` when it nests deeper than [`MAX_DEPTH`], has
    /// more than [`MAX_PARTS`] parts, or is not a component.
    fn component(&mut self, tag: &Tag, depth: usize) -> Option<Tag> {
        self.parts += 1;
        if depth > MAX_DEPTH || self.parts > MAX_PARTS {
            return None;
        }
        match tag {
            // A bare string is a component with only text.
            Tag::String(s) => Some(Tag::compound([("text", Tag::String(s.clone()))])),
            // A number, as the text it shows.
            Tag::Byte(_)
            | Tag::Short(_)
            | Tag::Int(_)
            | Tag::Long(_)
            | Tag::Float(_)
            | Tag::Double(_) => Some(Tag::compound([(
                "text",
                text::literal(&text::plain(tag, &Default::default())),
            )])),
            // A list is its first part with the others after it.
            Tag::List(parts) => {
                let extra = self.parts_of(parts, depth)?;
                Some(Tag::compound([
                    ("text", text::literal("")),
                    ("extra", extra),
                ]))
            }
            Tag::Compound(map) => {
                // The wrapper of a value in a list of mixed kinds: `{"": value}`.
                if map.len() == 1
                    && let Some(inner) = map.get(&"".into())
                {
                    return self.component(inner, depth);
                }
                let mut kept: Vec<(&str, Tag)> = Vec::new();
                let get = |key: &str| map.get(&key.into());
                match get("text") {
                    Some(Tag::String(s)) => kept.push(("text", Tag::String(s.clone()))),
                    // A number is shown as the number.
                    Some(
                        number @ (Tag::Byte(_)
                        | Tag::Short(_)
                        | Tag::Int(_)
                        | Tag::Long(_)
                        | Tag::Float(_)
                        | Tag::Double(_)),
                    ) => kept.push((
                        "text",
                        text::literal(&text::plain(number, &Default::default())),
                    )),
                    _ => {}
                }
                if let Some(Tag::String(key)) = get("translate") {
                    self.translated = true;
                    kept.push(("translate", Tag::String(key.clone())));
                    if let Some(Tag::String(s)) = get("fallback") {
                        kept.push(("fallback", Tag::String(s.clone())));
                    }
                    if let Some(Tag::List(args)) = get("with") {
                        kept.push(("with", self.parts_of(args, depth)?));
                    }
                }
                if let Some(Tag::List(parts)) = get("extra") {
                    kept.push(("extra", self.parts_of(parts, depth)?));
                }
                if let Some(Tag::String(color)) = get("color")
                    && color.0.len() <= MAX_COLOR
                {
                    kept.push(("color", Tag::String(color.clone())));
                }
                // A part that says nothing (only a score, a selector, a key binding or an NBT
                // path, which are dropped, or a `text` of another kind) would be `{}`, which a
                // client refuses as a component: it says an empty text.
                if !kept
                    .iter()
                    .any(|(key, _)| matches!(*key, "text" | "translate"))
                {
                    kept.push(("text", text::literal("")));
                }
                Some(Tag::compound(kept))
            }
            Tag::End | Tag::ByteArray(_) | Tag::IntArray(_) | Tag::LongArray(_) => None,
        }
    }

    /// A list of clean components, one level deeper.
    fn parts_of(&mut self, parts: &[Tag], depth: usize) -> Option<Tag> {
        let clean: Option<Vec<Tag>> = parts
            .iter()
            .map(|part| self.component(part, depth + 1))
            .collect();
        clean.map(Tag::List)
    }
}

/// For tests: every key of every compound in `tag`, at any depth.
#[cfg(test)]
pub(crate) fn keys_of(tag: &Tag) -> std::collections::BTreeSet<String> {
    fn walk(tag: &Tag, keys: &mut std::collections::BTreeSet<String>) {
        match tag {
            Tag::Compound(map) => {
                for (key, value) in map {
                    keys.insert(key.to_string_lossy());
                    walk(value, keys);
                }
            }
            Tag::List(items) => items.iter().for_each(|item| walk(item, keys)),
            _ => {}
        }
    }
    let mut keys = std::collections::BTreeSet::new();
    walk(tag, &mut keys);
    keys
}

#[cfg(test)]
mod tests;
