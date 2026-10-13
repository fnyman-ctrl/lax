//! The text of one string field of a JSON object, while the model is still
//! writing the object: for Ask, the `answer` of `{"answer": string,
//! "sources": [number]}`, shown as it is written (GUARDRAILS §5).
//!
//! This only decides what to show while the reply streams. It is not
//! validation: the whole reply is parsed and validated afterwards as any
//! other ([`super::structured::parse`]), and if that fails, everything shown
//! is taken back. So when the text looks wrong, this stops early rather than
//! guess.
//!
//! The model's reply arrives in pieces cut anywhere, also inside an escape
//! (`\` in one piece, `n` in the next) or a `\uXXXX` sequence. The reader
//! goes one character at a time and keeps its place between pieces.

use super::structured::is_invisible;

/// Longest key read; a longer one is never the field.
const MAX_KEY_CHARS: usize = 64;
/// Most bytes before the object: room for a code fence and white space.
const MAX_BEFORE: usize = 32;

#[derive(Debug)]
pub(crate) struct FieldText {
    field: &'static str,
    /// What came before the object.
    before: String,
    /// Inside the top-level object (`{` seen).
    started: bool,
    /// The object ended, or the text wasn't the JSON asked for: nothing more
    /// is read.
    stopped: bool,
    /// How deep in objects and arrays: 1 is the top-level object.
    depth: usize,
    expect: Expect,
    /// The string being read, if any.
    string: Option<Str>,
    escape: Escape,
    /// The first half of a character written as two `\u` escapes.
    high: Option<u32>,
    key: String,
    key_too_long: bool,
    /// The last key read in the top-level object was the field.
    at_field: bool,
    /// The field's value has been read (or is being read): a second one
    /// isn't shown.
    found: bool,
}

/// What comes next in the top-level object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    Key,
    Colon,
    Value,
    Comma,
}

/// Which string is being read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Str {
    /// A key of the top-level object.
    Key,
    /// The field's value: its text is shown.
    Field,
    /// Any other string, read only to find its end.
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Escape {
    No,
    Backslash,
    Unicode { digits: u8, value: u32 },
}

impl FieldText {
    pub(crate) fn new(field: &'static str) -> Self {
        FieldText {
            field,
            before: String::new(),
            started: false,
            stopped: false,
            depth: 0,
            expect: Expect::Key,
            string: None,
            escape: Escape::No,
            high: None,
            key: String::new(),
            key_too_long: false,
            at_field: false,
            found: false,
        }
    }

    /// Reads the next piece of the reply. Returns the field's text that it
    /// completes, if any, without control characters (other than line
    /// breaks) or invisible ones, which validation refuses anyway.
    pub(crate) fn push(&mut self, piece: &str) -> String {
        let mut out = String::new();
        for c in piece.chars() {
            if self.stopped {
                break;
            }
            match self.string {
                Some(kind) => self.in_string(kind, c, &mut out),
                None if !self.started => {
                    // Only a code fence may come before the object, as
                    // validation allows; prose means the reply will be
                    // refused, so nothing of it is shown.
                    if c == '{' && matches!(self.before.trim(), "" | "```" | "```json") {
                        self.started = true;
                        self.depth = 1;
                        self.expect = Expect::Key;
                    } else if c == '{' || self.before.len() >= MAX_BEFORE {
                        self.stop();
                    } else {
                        self.before.push(c);
                    }
                }
                None => self.outside_strings(c),
            }
        }
        out
    }

    fn outside_strings(&mut self, c: char) {
        let top = self.depth == 1;
        match c {
            '"' => {
                let kind = match (top, self.expect) {
                    (false, _) => Str::Other,
                    (true, Expect::Key) => {
                        self.key.clear();
                        self.key_too_long = false;
                        Str::Key
                    }
                    (true, Expect::Value) if self.at_field && !self.found => {
                        self.found = true;
                        Str::Field
                    }
                    (true, Expect::Value) => Str::Other,
                    (true, _) => return self.stop(),
                };
                self.string = Some(kind);
            }
            ':' if top => self.then(Expect::Colon, Expect::Value),
            ',' if top => self.then(Expect::Comma, Expect::Key),
            '{' | '[' => {
                if top {
                    self.then(Expect::Value, Expect::Comma);
                }
                self.depth += 1;
            }
            '}' | ']' => {
                self.depth -= 1;
                if self.depth == 0 {
                    // The object is complete; anything after it is ignored.
                    self.stopped = true;
                }
            }
            c if c.is_whitespace() => {}
            // Numbers, true, false, null.
            _ if top => match self.expect {
                Expect::Value => self.expect = Expect::Comma,
                Expect::Comma => {}
                _ => self.stop(),
            },
            _ => {}
        }
    }

    /// Moves on to `next` if `now` was expected; stops otherwise.
    fn then(&mut self, now: Expect, next: Expect) {
        if self.expect == now {
            self.expect = next;
        } else {
            self.stop();
        }
    }

    fn stop(&mut self) {
        self.stopped = true;
    }

    fn in_string(&mut self, kind: Str, c: char, out: &mut String) {
        if kind == Str::Other {
            // Only its end matters. An escape is one character, and the
            // digits of a \u escape are never a quote.
            match (self.escape, c) {
                (Escape::Backslash, _) => self.escape = Escape::No,
                (_, '\\') => self.escape = Escape::Backslash,
                (_, '"') => self.end_string(kind),
                _ => {}
            }
            return;
        }
        match self.escape {
            Escape::No => match c {
                '\\' => self.escape = Escape::Backslash,
                // Half a character, not followed by its other half.
                _ if self.high.is_some() => self.stop(),
                '"' => self.end_string(kind),
                c => self.take(kind, c, out),
            },
            Escape::Backslash => {
                self.escape = Escape::No;
                if self.high.is_some() && c != 'u' {
                    return self.stop();
                }
                let decoded = match c {
                    '"' => '"',
                    '\\' => '\\',
                    '/' => '/',
                    'b' => '\u{8}',
                    'f' => '\u{c}',
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    'u' => {
                        self.escape = Escape::Unicode {
                            digits: 0,
                            value: 0,
                        };
                        return;
                    }
                    _ => return self.stop(),
                };
                self.take(kind, decoded, out);
            }
            Escape::Unicode { digits, value } => {
                let Some(digit) = c.to_digit(16) else {
                    return self.stop();
                };
                let value = value * 16 + digit;
                if digits < 3 {
                    self.escape = Escape::Unicode {
                        digits: digits + 1,
                        value,
                    };
                    return;
                }
                self.escape = Escape::No;
                let decoded = match (self.high.take(), value) {
                    (Some(high), 0xDC00..=0xDFFF) => {
                        char::from_u32(0x10000 + ((high - 0xD800) << 10) + (value - 0xDC00))
                    }
                    (None, 0xD800..=0xDBFF) => {
                        self.high = Some(value);
                        return;
                    }
                    (Some(_), _) | (None, 0xDC00..=0xDFFF) => None,
                    (None, value) => char::from_u32(value),
                };
                match decoded {
                    Some(c) => self.take(kind, c, out),
                    None => self.stop(),
                }
            }
        }
    }

    fn take(&mut self, kind: Str, c: char, out: &mut String) {
        match kind {
            Str::Key if self.key.chars().count() < MAX_KEY_CHARS => self.key.push(c),
            Str::Key => self.key_too_long = true,
            Str::Field if (c.is_control() && c != '\n') || is_invisible(c) => {}
            Str::Field => out.push(c),
            Str::Other => {}
        }
    }

    fn end_string(&mut self, kind: Str) {
        self.string = None;
        if self.depth != 1 {
            return;
        }
        if kind == Str::Key {
            self.at_field = !self.key_too_long && self.key == self.field;
            self.expect = Expect::Colon;
        } else {
            self.expect = Expect::Comma;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(pieces: &[&str]) -> String {
        let mut field = FieldText::new("answer");
        pieces.iter().map(|p| field.push(p)).collect()
    }

    /// The text read from `json` cut at every place, and one character at a
    /// time, is always the same.
    fn read_cut_anywhere(json: &str) -> String {
        let whole = read(&[json]);
        for (i, _) in json.char_indices() {
            assert_eq!(read(&[&json[..i], &json[i..]]), whole, "cut at {i}: {json}");
        }
        let chars: Vec<String> = json.chars().map(String::from).collect();
        let chars: Vec<&str> = chars.iter().map(String::as_str).collect();
        assert_eq!(read(&chars), whole, "one at a time: {json}");
        whole
    }

    #[test]
    fn the_answer_is_read_as_it_comes() {
        let mut field = FieldText::new("answer");
        assert_eq!(field.push("{\"ans"), "");
        assert_eq!(field.push("wer\": \"Du mai"), "Du mai");
        assert_eq!(field.push("lade Anna"), "lade Anna");
        assert_eq!(field.push(".\", \"sources\": [1"), ".");
        assert_eq!(field.push("]}"), "");
    }

    #[test]
    fn it_matches_what_the_json_says_wherever_it_is_cut() {
        for json in [
            r#"{"answer": "Hej Anna Example", "sources": [1, 2]}"#,
            r#"{"sources": [3], "answer": "Rad ett\n- rad två\n- \"citat\" och \\ och \/"}"#,
            r#"{"answer":"\u00c5sa och \u00e5\u00e4\u00f6 och \ud83d\ude00 och \u20ac"}"#,
            r#"{"answer": "tab\tslut", "sources": []}"#,
            r#"{"answer": "kod: {x} [y] \"}\" , ", "sources": [2]}"#,
            "```json\n{\"answer\": \"I ett staket\", \"sources\": [1]}\n```",
            r#"{"\u0061nswer": "escaped key"}"#,
        ] {
            let expected: String = serde_json::from_str::<serde_json::Value>(
                json.trim_start_matches("```json\n")
                    .trim_end_matches("\n```"),
            )
            .unwrap()["answer"]
                .as_str()
                .unwrap()
                .chars()
                .filter(|&c| !(c.is_control() && c != '\n'))
                .collect();
            assert_eq!(read_cut_anywhere(json), expected, "{json}");
        }
    }

    #[test]
    fn only_the_top_level_field_is_read() {
        // Nested objects and arrays, and strings with brackets and quotes.
        assert_eq!(
            read_cut_anywhere(
                r#"{"meta": {"answer": "no", "x": ["}", "\"{"]}, "sources": ["]"], "answer": "yes"}"#
            ),
            "yes"
        );
        // "answer" as a value isn't the key.
        assert_eq!(
            read_cut_anywhere(r#"{"note": "answer", "answer": "ja"}"#),
            "ja"
        );
        // A key with an escaped quote in it.
        assert_eq!(
            read_cut_anywhere(r#"{"an\"swer": "x", "answer": "y"}"#),
            "y"
        );
        assert_eq!(read_cut_anywhere(r#"{"answers": "x"}"#), "");
        // A second answer field isn't shown (validation refuses the reply).
        assert_eq!(read_cut_anywhere(r#"{"answer": "a", "answer": "b"}"#), "a");
    }

    #[test]
    fn nothing_is_read_after_the_object() {
        assert_eq!(read_cut_anywhere(r#"{"answer": "a"} {"answer": "b"}"#), "a");
    }

    #[test]
    fn something_that_isnt_the_json_asked_for_stops_it() {
        // Not a string.
        assert_eq!(read_cut_anywhere(r#"{"answer": 5, "x": "y"}"#), "");
        assert_eq!(read_cut_anywhere(r#"{"answer": ["a"]}"#), "");
        // Half a character, alone or followed by something else.
        assert_eq!(read_cut_anywhere(r#"{"answer": "a\ud800b"}"#), "a");
        assert_eq!(read_cut_anywhere(r#"{"answer": "a\ud800\n"}"#), "a");
        assert_eq!(read_cut_anywhere(r#"{"answer": "a\ud800\u0041"}"#), "a");
        assert_eq!(read_cut_anywhere(r#"{"answer": "a\udc00b"}"#), "a");
        // Escapes that don't exist, and \u without four hex digits.
        assert_eq!(read_cut_anywhere(r#"{"answer": "a\qb"}"#), "a");
        assert_eq!(read_cut_anywhere(r#"{"answer": "a\u00zzb"}"#), "a");
        // Structure that isn't an object's.
        assert_eq!(read_cut_anywhere(r#"{"answer" "a"}"#), "");
        assert_eq!(read_cut_anywhere(r#"{"x": 1 "answer": "a"}"#), "");
        assert_eq!(read_cut_anywhere(r#"{: "answer": "a"}"#), "");
    }

    #[test]
    fn control_and_invisible_characters_are_not_shown() {
        assert_eq!(
            read_cut_anywhere(r#"{"answer": "a\u202eb\u0007c\r\nd\u200be"}"#),
            "abc\nde"
        );
        assert_eq!(read_cut_anywhere("{\"answer\": \"a\u{2066}b\"}"), "ab");
    }

    #[test]
    fn only_a_code_fence_may_come_before_the_object() {
        // Validation refuses anything else, so nothing of it is shown.
        assert_eq!(read_cut_anywhere(r#"Sure! {"answer": "Hej"}"#), "");
        assert_eq!(read_cut_anywhere(r#"x{"answer": "Hej"}"#), "");
        assert_eq!(read_cut_anywhere("No JSON at all"), "");
        assert_eq!(
            read_cut_anywhere(&format!("{}{{\"answer\": \"Hej\"}}", " ".repeat(40))),
            ""
        );
        for before in ["", "  \n", "```", "```json\n", "\n```json\n  "] {
            let reply = format!("{before}{{\"answer\": \"Hej\"}}");
            assert_eq!(read_cut_anywhere(&reply), "Hej", "{reply:?}");
        }
    }

    #[test]
    fn long_keys_are_not_the_field() {
        let key = format!("answer{}", " ".repeat(100));
        assert_eq!(read(&[&format!(r#"{{"{key}": "x"}}"#)]), "");
    }
}
