//! Python's text operations the rules use, with CPython's results: `str.casefold` and
//! `format(value, "g")` (Issue #152, P8-0a).

use std::borrow::Cow;

use crate::error::{Error, Result};

mod casefold;

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
