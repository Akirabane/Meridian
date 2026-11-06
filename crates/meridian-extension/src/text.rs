//! Text that comes from the other side of a connection, made safe to write in a log line.

/// `text` with no control character, no line or paragraph separator (U+2028 and U+2029, which end a
/// line for some viewers) and no character that sets or changes the direction of the text (which could
/// make a log line read differently than it is), cut to `max` characters. The proxy cleans what an
/// extension says of itself with it, and the SDK what the proxy says.
pub fn clean(text: &str, max: usize) -> String {
    text.chars()
        .filter(|c| {
            !c.is_control()
                && !matches!(
                    c,
                    '\u{200e}'
                        | '\u{200f}'
                        | '\u{2028}'
                        | '\u{2029}'
                        | '\u{202a}'..='\u{202e}'
                        | '\u{2066}'..='\u{2069}'
                )
        })
        .take(max)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::clean;

    #[test]
    fn control_characters_and_direction_marks_are_dropped() {
        assert_eq!(clean("a\r\nb\u{1b}[31mc\0", 64), "ab[31mc");
        assert_eq!(clean("x\u{202e}evil\u{2066}y", 64), "xevily");
        // The marks that set the direction without overriding it, and the separators of lines.
        assert_eq!(clean("a\u{200e}b\u{200f}c", 64), "abc");
        assert_eq!(clean("one\u{2028}two\u{2029}three", 64), "onetwothree");
        assert_eq!(clean("plain 1.2.3", 64), "plain 1.2.3");
    }

    #[test]
    fn the_text_is_cut_in_characters_not_bytes() {
        assert_eq!(clean("caf\u{e9}s", 4), "caf\u{e9}");
        assert_eq!(clean("abcdef", 3), "abc");
        assert_eq!(clean("", 3), "");
    }
}
