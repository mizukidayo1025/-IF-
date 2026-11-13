//! Python's text and regular expressions as the offline interpreter uses them (Issue #303): NFKC,
//! `str.casefold`, `str.strip`, `int()` of decimal digits and `re` patterns written as Python wrote
//! them, with Python's meaning of `\b`, `\w`, `\d` and `\s` for str.
//!
//! - `\w` is `[\p{L}\p{N}_]`: CPython 3.12 takes a character as a word character when it is
//!   alphanumeric (`str.isalnum`) or `_`, which is the same set as categories L and N with `_` for
//!   every code point of Unicode 15.0 (checked when this was written). The regex crate's own `\w`
//!   also takes marks, connector punctuation and joiners, so it is not used.
//! - `\b` is the boundary between a `\w` and a character that is not one (or the text's end),
//!   written with lookarounds.
//! - `\d` is a decimal digit (category Nd), from the table `digits.rs` written by CPython
//!   (`native/core/tools/interpret_tables.py`); `int()` reads each digit's value from it.
//! - `\s` is the regex crate's Unicode `\s` with U+001C..U+001F, which `str.isspace` also takes.
//! - NFKC is `unicode-normalization` 0.1.22, of Unicode 15.0.0 as CPython 3.12's `unicodedata`.
//!
//! What may still differ: `\p{L}` and `\p{N}` are of the regex crate's Unicode version, so a
//! character assigned after Unicode 15.0 can be a word character here and not in Python; and
//! `re.IGNORECASE` matches a few characters with several case forms (such as U+0131) differently.
//! The interpreter casefolds the text first, so neither reaches an ASCII keyword.

use std::sync::OnceLock;

use fancy_regex::{Regex, RegexBuilder};
use unicode_normalization::UnicodeNormalization;

use crate::error::{Error, Result};

#[path = "digits.rs"]
mod digits;

const WORD: &str = r"[\p{L}\p{N}_]";
const BOUNDARY: &str = r"(?:(?<=[\p{L}\p{N}_])(?![\p{L}\p{N}_])|(?<![\p{L}\p{N}_])(?=[\p{L}\p{N}_]))";

/// The ranges of `\d` inside a class, as `0-9\x{660}-\x{669}...`.
fn digit_ranges() -> &'static str {
    static RANGES: OnceLock<String> = OnceLock::new();
    RANGES.get_or_init(|| {
        let mut out = String::new();
        for zero in digits::ZEROS {
            out.push_str(&format!("\\x{{{zero:x}}}-\\x{{{:x}}}", zero + 9));
        }
        out
    })
}

/// A Python pattern in the regex crate's syntax, with Python's `\b`, `\w`, `\d` and `\s`.
pub(super) fn translate(python: &str) -> String {
    let mut out = String::with_capacity(python.len() * 2);
    let mut in_class = false;
    let mut chars = python.chars();
    while let Some(char) = chars.next() {
        match char {
            '\\' => {
                let Some(next) = chars.next() else {
                    out.push('\\');
                    break;
                };
                match (next, in_class) {
                    ('b', false) => out.push_str(BOUNDARY),
                    ('w', false) => out.push_str(WORD),
                    ('w', true) => out.push_str(r"\p{L}\p{N}_"),
                    ('d', false) => {
                        out.push('[');
                        out.push_str(digit_ranges());
                        out.push(']');
                    }
                    ('d', true) => out.push_str(digit_ranges()),
                    ('s', false) => out.push_str(r"[\s\x1c-\x1f]"),
                    ('s', true) => out.push_str(r"\s\x1c-\x1f"),
                    _ => {
                        out.push('\\');
                        out.push(next);
                    }
                }
            }
            '[' if !in_class => {
                in_class = true;
                out.push(char);
            }
            ']' if in_class => {
                in_class = false;
                out.push(char);
            }
            _ => out.push(char),
        }
    }
    out
}

/// A compiled Python pattern, with `re.IGNORECASE` when `ignore_case`.
pub(super) fn compile(python: &str, ignore_case: bool) -> Result<Regex> {
    let mut pattern = translate(python);
    if ignore_case {
        pattern = format!("(?i:{pattern})");
    }
    RegexBuilder::new(&pattern)
        .backtrack_limit(100_000_000)
        .build()
        .map_err(|error| Error::unsupported(format!("an interpreter pattern does not compile: {error}")))
}

/// A pattern compiled once.
pub(super) struct Pattern {
    python: &'static str,
    ignore_case: bool,
    cell: OnceLock<std::result::Result<Regex, String>>,
}

impl Pattern {
    pub(super) const fn new(python: &'static str, ignore_case: bool) -> Self {
        Self { python, ignore_case, cell: OnceLock::new() }
    }

    pub(super) fn get(&self) -> Result<&Regex> {
        self.cell
            .get_or_init(|| compile(self.python, self.ignore_case).map_err(|error| error.to_string()))
            .as_ref()
            .map_err(|error| Error::unsupported(error.clone()))
    }
}

fn run<T>(value: std::result::Result<T, fancy_regex::Error>) -> Result<T> {
    value.map_err(|error| Error::unsupported(format!("an interpreter pattern failed: {error}")))
}

/// `bool(re.search(pattern, text))`.
pub(super) fn found(regex: &Regex, text: &str) -> Result<bool> {
    run(regex.is_match(text))
}

/// `re.search(pattern, text)`.
pub(super) fn search<'t>(regex: &Regex, text: &'t str) -> Result<Option<fancy_regex::Captures<'t, str>>> {
    run(regex.captures(text))
}

/// The (start, end) of each match of `re.finditer(pattern, text)`, in bytes.
pub(super) fn all_captures<'t>(regex: &Regex, text: &'t str) -> Result<Vec<fancy_regex::Captures<'t, str>>> {
    regex.captures_iter(text).map(run).collect()
}

/// `re.split(pattern, text)` of a pattern that never matches empty text.
pub(super) fn split<'t>(regex: &Regex, text: &'t str) -> Result<Vec<&'t str>> {
    let mut pieces = Vec::new();
    let mut last = 0;
    for found in regex.find_iter(text) {
        let found = run(found)?;
        pieces.push(&text[last..found.start()]);
        last = found.end();
    }
    pieces.push(&text[last..]);
    Ok(pieces)
}

/// `re.sub(pattern, " ", text)` of a pattern that never matches empty text.
pub(super) fn blank(regex: &Regex, text: &str, with: &str) -> Result<String> {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for found in regex.find_iter(text) {
        let found = run(found)?;
        out.push_str(&text[last..found.start()]);
        out.push_str(with);
        last = found.end();
    }
    out.push_str(&text[last..]);
    Ok(out)
}

/// `len(re.findall(pattern, text))` of a pattern that never matches empty text.
pub(super) fn count(regex: &Regex, text: &str) -> Result<usize> {
    let mut total = 0;
    for found in regex.find_iter(text) {
        run(found)?;
        total += 1;
    }
    Ok(total)
}

/// `str.isspace` of one character: Unicode White_Space, and U+001C..U+001F.
pub(super) fn is_space(char: char) -> bool {
    char.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&char)
}

/// `text.strip()`.
pub(super) fn strip(text: &str) -> &str {
    text.trim_matches(is_space)
}

/// `unicodedata.normalize("NFKC", text)`.
pub(super) fn nfkc(text: &str) -> String {
    text.nfkc().collect()
}

/// `unicodedata.normalize("NFKC", text).casefold().strip()`.
pub(super) fn normalize(text: &str) -> String {
    let folded = crate::text::casefold(&nfkc(text)).into_owned();
    strip(&folded).to_owned()
}

/// The value of a decimal digit (category Nd), or None.
fn digit_value(char: char) -> Option<u32> {
    let point = char as u32;
    let index = digits::ZEROS.partition_point(|zero| *zero <= point);
    let zero = *digits::ZEROS.get(index.checked_sub(1)?)?;
    (point - zero < 10).then_some(point - zero)
}

/// `int(text)` of text the pattern `[+-]?\d+` matched; None past i64 (no count gets there).
pub(super) fn int(text: &str) -> Option<i64> {
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let mut value: i64 = 0;
    for char in digits.chars() {
        value = value.checked_mul(10)?.checked_add(i64::from(digit_value(char)?))?;
    }
    Some(if negative { -value } else { value })
}

/// The number of characters (code points) of text, as Python's `len`.
pub(super) fn chars(text: &str) -> usize {
    text.chars().count()
}

/// `text[:limit]` in code points.
pub(super) fn head(text: &str, limit: usize) -> &str {
    match text.char_indices().nth(limit) {
        Some((index, _)) => &text[..index],
        None => text,
    }
}
