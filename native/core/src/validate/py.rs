//! Python's reading of JSON values, for the validators: numbers (`number`, `finite`), equality
//! (`equal`) and `validate_unit_interval` (`unit_interval`).

use crate::error::{Error, Result};
use crate::value::V;

/// A number as Python compares it: an int (`V::Int`, `V::Big`) or a float; None for any other
/// value, a bool too. An int too large for a float is left to Python (its `OverflowError`).
pub fn number(value: &V) -> Result<Option<f64>> {
    match value {
        V::Int(number) => Ok(Some(*number as f64)),
        V::Float(number) => Ok(Some(*number)),
        V::Big(text) => match text.parse::<f64>() {
            Ok(number) if number.is_finite() => Ok(Some(number)),
            _ => Err(Error::unsupported("an int too large for a float")),
        },
        _ => Ok(None),
    }
}

/// A finite int or float (`type(value) in (int, float) and math.isfinite(value)`).
pub fn finite(value: &V) -> Result<Option<f64>> {
    Ok(number(value)?.filter(|number| number.is_finite()))
}

/// `first == second` of Python for JSON values: an int equals a float of its value, a bool its
/// int, and a dict's order does not count.
pub fn equal(first: &V, second: &V) -> bool {
    match (first, second) {
        (V::Bool(a), V::Bool(b)) => a == b,
        (V::Bool(a), other) | (other, V::Bool(a)) => matches!(other, V::Int(_) | V::Float(_)) && equal(&V::Int(i64::from(*a)), other),
        (V::Int(a), V::Float(b)) | (V::Float(b), V::Int(a)) => (*a as f64) == *b && (*b as i64) == *a,
        (V::List(a), V::List(b)) => a.len() == b.len() && a.iter().zip(b).all(|(a, b)| equal(a, b)),
        (V::Map(a), V::Map(b)) => a.len() == b.len() && a.iter().all(|(key, value)| b.get(key).is_some_and(|other| equal(value, other))),
        (a, b) => a == b,
    }
}

/// `validate_unit_interval(name, value)` of `domain/models.py`.
pub fn unit_interval(name: &str, value: &V) -> Result<()> {
    match finite(value)? {
        Some(number) if (0.0..=1.0).contains(&number) => Ok(()),
        _ => Err(Error::Value(format!("{name} must be finite and between 0 and 1"))),
    }
}

/// `^[A-Za-z0-9][A-Za-z0-9._:-]{0,63}$` (`Identifier`, `_INFORMATION_ID`), fully matched.
pub fn is_stable_id(text: &str) -> bool {
    let bytes = text.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && bytes[0].is_ascii_alphanumeric()
        && bytes[1..].iter().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

/// The name of the Python type of a value the world holds, as an error names it (`'int'`,
/// `'NoneType'`): a list or a dict of the world is one of its tracked trees (`TrackedList`,
/// `TrackedDict` of `core/state_mirror.py`), as the native path's texts show.
pub fn type_name(value: &V) -> &'static str {
    match value {
        V::Null => "NoneType",
        V::Bool(_) => "bool",
        V::Int(_) | V::Big(_) => "int",
        V::Float(_) => "float",
        V::Str(_) => "str",
        V::List(_) => "TrackedList",
        V::Map(_) => "TrackedDict",
    }
}

/// The `TypeError` of `value in a_set` (a lookup that hashes the value) where Python cannot hash
/// it: `unhashable type: 'TrackedList'`.
pub fn unhashable(value: &V) -> Option<Error> {
    match value {
        V::List(_) | V::Map(_) => Some(Error::Type(format!("unhashable type: '{}'", type_name(value)))),
        _ => None,
    }
}

/// `json.dumps(value, separators=(",", ":"))` of a list of str (`ensure_ascii`): what Python
/// writes of an Agent pair's key.
pub fn dumps_texts(texts: &[&str]) -> String {
    let mut out = String::from("[");
    for (index, text) in texts.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push('"');
        for char in text.chars() {
            match char {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                '\u{8}' => out.push_str("\\b"),
                '\u{c}' => out.push_str("\\f"),
                ' '..='~' => out.push(char),
                other => {
                    let mut units = [0u16; 2];
                    for unit in other.encode_utf16(&mut units) {
                        out.push_str(&format!("\\u{unit:04x}"));
                    }
                }
            }
        }
        out.push('"');
    }
    out.push(']');
    out
}
