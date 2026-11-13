//! Python's text operations the rules use, with CPython's results: `str.casefold`,
//! `format(value, "g")` (Issue #152, P8-0a) and `repr` of a str (P8 c3).

use std::borrow::Cow;

use crate::error::{Error, Result};

mod casefold;
mod classes;
mod printable;

/// What `str.casefold` gives one character: its full case folding, or None for itself. The table
/// (`text/casefold.rs`) is generated from CPython 3.12, Unicode 15.0.0, whose fold maps each
/// character on its own (`native/core/tools/casefold_table.py`).
pub fn fold_char(char: char) -> Option<&'static str> {
    let point = char as u32;
    casefold::FOLDS
        .binary_search_by_key(&point, |(from, _)| *from)
        .ok()
        .map(|index| casefold::FOLDS[index].1)
}

fn in_ranges(ranges: &[(u32, u32)], char: char) -> bool {
    let point = char as u32;
    ranges.binary_search_by(|(first, last)| if *last < point { std::cmp::Ordering::Less } else if *first > point { std::cmp::Ordering::Greater } else { std::cmp::Ordering::Equal }).is_ok()
}

/// Whether `\d` of a str pattern of Python's `re` matches the character: a Unicode decimal
/// digit. The tables (`text/classes.rs`) are generated from CPython 3.12, Unicode 15.0.0
/// (`native/core/tools/regex_classes_table.py`).
pub fn re_decimal(char: char) -> bool {
    in_ranges(&classes::DECIMAL, char)
}

/// Whether `\s` of a str pattern matches the character (`str.isspace`).
pub fn re_space(char: char) -> bool {
    in_ranges(&classes::SPACE, char)
}

/// Whether `\w` of a str pattern matches the character (`str.isalnum`, and `_`).
pub fn re_word(char: char) -> bool {
    in_ranges(&classes::WORD, char)
}

/// `text.casefold()`. Borrowed when no character folds, as for ASCII text without capitals.
pub fn casefold(text: &str) -> Cow<'_, str> {
    if text.is_ascii() {
        return if text.bytes().any(|byte| byte.is_ascii_uppercase()) {
            Cow::Owned(text.to_ascii_lowercase())
        } else {
            Cow::Borrowed(text)
        };
    }
    let Some(first) = text.char_indices().find(|(_, char)| fold_char(*char).is_some()) else {
        return Cow::Borrowed(text);
    };
    let mut folded = String::with_capacity(text.len() + 8);
    folded.push_str(&text[..first.0]);
    for char in text[first.0..].chars() {
        match fold_char(char) {
            Some(fold) => folded.push_str(fold),
            None => folded.push(char),
        }
    }
    Cow::Owned(folded)
}

/// `format(value, "g")` of a float, or of an int as a float: six significant digits, the
/// exponent written with a sign and at least two digits. Unsupported for a value that is not
/// finite, which the callers never format.
pub fn g_text(value: f64) -> Result<String> {
    if !value.is_finite() {
        return Err(Error::unsupported("a threshold is not finite"));
    }
    if value == 0.0 {
        return Ok(if value.is_sign_negative() { "-0" } else { "0" }.to_owned());
    }
    // Both round the exact value of the double half to even, as CPython's `%g` does.
    let scientific = format!("{value:.5e}");
    let Some((mantissa, exponent)) = scientific.split_once('e') else {
        return Err(Error::unsupported("a float with no exponent"));
    };
    let exponent: i32 = exponent.parse().map_err(|_| Error::unsupported("a float with no exponent"))?;
    fn trimmed(text: &str) -> &str {
        if text.contains('.') {
            text.trim_end_matches('0').trim_end_matches('.')
        } else {
            text
        }
    }
    if (-4..6).contains(&exponent) {
        let fixed = format!("{:.*}", (5 - exponent) as usize, value);
        return Ok(trimmed(&fixed).to_owned());
    }
    let sign = if exponent < 0 { '-' } else { '+' };
    Ok(format!("{}e{sign}{:02}", trimmed(mantissa), exponent.abs()))
}

/// Whether `repr` escapes a code point past ASCII: `str.isprintable` says no to it. The table
/// (`text/printable.rs`) is generated from CPython 3.12, Unicode 15.0.0
/// (`native/core/tools/repr_table.py`).
fn escapes(point: u32) -> bool {
    printable::ESCAPED
        .binary_search_by(|(first, last)| {
            if *last < point {
                std::cmp::Ordering::Less
            } else if *first > point {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// `repr(text)` of a str, as CPython writes it: in single quotes, or double quotes when the text
/// has a single quote and no double one; the quote, the backslash, `\t`, `\n`, `\r` and the
/// characters `str.isprintable` says no to escaped. The `{x!r}` of a Python rule's f-string.
pub fn py_repr(text: &str) -> String {
    use std::fmt::Write;
    let quote = if text.contains('\'') && !text.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    for char in text.chars() {
        let point = char as u32;
        match char {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ if char == quote => {
                out.push('\\');
                out.push(char);
            }
            _ if point < 0x20 || point == 0x7f => {
                let _ = write!(out, "\\x{point:02x}");
            }
            _ if point < 0x7f || !escapes(point) => out.push(char),
            _ if point <= 0xff => {
                let _ = write!(out, "\\x{point:02x}");
            }
            _ if point <= 0xffff => {
                let _ = write!(out, "\\u{point:04x}");
            }
            _ => {
                let _ = write!(out, "\\U{point:08x}");
            }
        }
    }
    out.push(quote);
    out
}
