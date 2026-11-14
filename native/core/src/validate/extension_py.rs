//! What the extensions' Python rules do with a value of any type, for their ports (lane D of #250,
//! #349): `isinstance` and `type(...) is` of the JSON types, `==`, `in` of a dict (which hashes the
//! value: a list or a dict is a `TypeError`), `set(...)` of a list, `iter` of what `update` reads,
//! and `json.loads` of a relationship key.
//!
//! A value is what `json.loads` gives (`V`): an int is `Int` (or `Big` past 64 bits), and a bool is
//! not an int to `type(x) is int`, but is one to `isinstance(x, int)`.

use std::collections::HashSet;
use std::sync::Arc;

use crate::error::{Error, Result};
use crate::value::V;

pub use crate::rules::dispatch::py_eq as eq;

/// `type(value).__name__`.
pub fn type_name(value: &V) -> &'static str {
    match value {
        V::Null => "NoneType",
        V::Bool(_) => "bool",
        V::Int(_) | V::Big(_) => "int",
        V::Float(_) => "float",
        V::Str(_) => "str",
        V::List(_) => "list",
        V::Map(_) => "dict",
    }
}

/// How Python holds a namespace's lists and dicts, which names their type in a `TypeError`: as
/// plain ones (a namespace a row holds, read as a new document: `body_state_v1`), or as the
/// mirror's `TrackedList` and `TrackedDict` (a namespace Python holds itself).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Held {
    Plain,
    Tracked,
}

/// `type(value).__name__` of a value held so.
pub fn held_type_name(value: &V, held: Held) -> &'static str {
    match (value, held) {
        (V::List(_), Held::Tracked) => "TrackedList",
        (V::Map(_), Held::Tracked) => "TrackedDict",
        _ => type_name(value),
    }
}

/// The `TypeError` of hashing a list or a dict.
pub fn unhashable(value: &V, held: Held) -> Error {
    Error::Type(format!("unhashable type: '{}'", held_type_name(value, held)))
}

/// `value in table` of a dict keyed by str: a str the table has; a list or a dict raises.
pub fn key_in(value: &V, held: Held, has: impl FnOnce(&str) -> bool) -> Result<bool> {
    match value {
        V::Str(text) => Ok(has(text)),
        V::List(_) | V::Map(_) => Err(unhashable(value, held)),
        _ => Ok(false),
    }
}

/// `value in {texts}` of a set of str.
pub fn in_texts(value: &V, held: Held, texts: &[&str]) -> Result<bool> {
    key_in(value, held, |text| texts.contains(&text))
}

/// `value == number` of an int.
pub fn eq_int(value: Option<&V>, number: i64) -> bool {
    value.is_some_and(|value| eq(value, &V::Int(number)))
}

/// `value == text` (a missing value is None, which is no str).
pub fn eq_str(value: Option<&V>, text: &str) -> bool {
    matches!(value, Some(V::Str(found)) if &**found == text)
}

/// `value == other` of an optional str (None for None).
pub fn eq_opt_str(value: &V, other: Option<&str>) -> bool {
    match (value, other) {
        (V::Null, None) => true,
        (V::Str(found), Some(other)) => &**found == other,
        _ => false,
    }
}

/// `isinstance(value, int)`: a bool is one.
pub fn is_int(value: &V) -> bool {
    matches!(value, V::Int(_) | V::Big(_) | V::Bool(_))
}

/// `type(value) is int`.
pub fn is_exact_int(value: &V) -> bool {
    matches!(value, V::Int(_) | V::Big(_))
}

/// `isinstance(value, (int, float)) and not isinstance(value, bool)`.
pub fn is_real(value: &V) -> bool {
    matches!(value, V::Int(_) | V::Big(_) | V::Float(_))
}

/// The number of an int, a bool or a float, for comparisons with bounds. An int past 64 bits is
/// its float, which compares with the bounds the rules use as the int does.
pub fn number(value: &V) -> Option<f64> {
    match value {
        V::Bool(flag) => Some(f64::from(u8::from(*flag))),
        V::Int(number) => Some(*number as f64),
        V::Big(text) => text.parse().ok(),
        V::Float(number) => Some(*number),
        _ => None,
    }
}

/// An int (or a bool) as a wide int, for sums and comparisons of minutes. An int past 64 bits is
/// kept past every sum of two 64-bit ints, with its sign.
pub fn wide(value: &V) -> Option<i128> {
    match value {
        V::Bool(flag) => Some(i128::from(*flag)),
        V::Int(number) => Some(i128::from(*number)),
        V::Big(text) => Some(text.parse().unwrap_or(if text.starts_with('-') { -(1 << 100) } else { 1 << 100 })),
        _ => None,
    }
}

/// `value < 0` of an int.
pub fn negative(value: &V) -> bool {
    wide(value).is_some_and(|number| number < 0)
}

/// `isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value) and
/// lower <= value <= upper`.
pub fn finite_within(value: &V, lower: f64, upper: f64) -> bool {
    is_real(value) && number(value).is_some_and(|number| number.is_finite() && lower <= number && number <= upper)
}

/// `set(map) == {names}`.
pub fn keys_are(map: &indexmap::IndexMap<Arc<str>, V>, names: &[&str]) -> bool {
    map.len() == names.len() && names.iter().all(|name| map.contains_key(*name))
}

/// A value as Python hashes it: equal values (`1`, `1.0`, `True`) are one key.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Key {
    Null,
    Int(i128),
    Big(Box<str>),
    Float(u64),
    Str(Arc<str>),
}

/// The key of a hashable value, or the `TypeError` of hashing a list or a dict.
pub fn key(value: &V, held: Held) -> Result<Key> {
    Ok(match value {
        V::Null => Key::Null,
        V::Bool(flag) => Key::Int(i128::from(*flag)),
        V::Int(number) => Key::Int(i128::from(*number)),
        V::Big(text) => Key::Big(text.clone()),
        V::Float(number) if number.fract() == 0.0 && number.abs() < 1e30 => Key::Int(*number as i128),
        V::Float(number) => Key::Float(number.to_bits()),
        V::Str(text) => Key::Str(text.clone()),
        V::List(_) | V::Map(_) => return Err(unhashable(value, held)),
    })
}

/// `len(set(items))`.
pub fn distinct(items: &[V], held: Held) -> Result<usize> {
    let mut seen = HashSet::with_capacity(items.len());
    for item in items {
        seen.insert(key(item, held)?);
    }
    Ok(seen.len())
}

/// The items `iter(value)` gives, as `set.update(value)` reads them: a list's items, a dict's keys,
/// a str's characters; anything else is the `TypeError` of `iter`.
pub fn iterate(value: &V) -> Result<Vec<V>> {
    match value {
        V::List(items) => Ok(items.clone()),
        V::Map(map) => Ok(map.keys().map(|key| V::Str(key.clone())).collect()),
        V::Str(text) => Ok(text.chars().map(|char| V::Str(Arc::from(char.to_string().as_str()))).collect()),
        other => Err(Error::Type(format!("'{}' object is not iterable", type_name(other)))),
    }
}

/// `set.update(value)`.
pub fn update(set: &mut HashSet<Key>, value: &V, held: Held) -> Result<()> {
    for item in iterate(value)? {
        set.insert(key(&item, held)?);
    }
    Ok(())
}

/// `dict.get(name)` of a value read as a dict.
pub fn get<'a>(map: &'a indexmap::IndexMap<Arc<str>, V>, name: &str) -> &'a V {
    map.get(name).unwrap_or(&V::Null)
}

/// `json.dumps(value, sort_keys=True, separators=(",", ":"))` (`ensure_ascii`, floats as their
/// `repr`), or the `ValueError` of `allow_nan=False` for a float that is not finite.
pub fn dumps_sorted(value: &V) -> Result<String> {
    let mut out = String::new();
    write_sorted(&mut out, value)?;
    Ok(out)
}

fn write_sorted(out: &mut String, value: &V) -> Result<()> {
    match value {
        V::Null => out.push_str("null"),
        V::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        V::Int(number) => out.push_str(&number.to_string()),
        V::Big(text) => out.push_str(text),
        V::Float(number) if !number.is_finite() => {
            let repr = if number.is_nan() { "nan" } else if *number > 0.0 { "inf" } else { "-inf" };
            return Err(Error::Value(format!("Out of range float values are not JSON compliant: {repr}")));
        }
        V::Float(number) => out.push_str(&crate::json::python_float_repr(*number)),
        V::Str(text) => write_ascii(out, text),
        V::List(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_sorted(out, item)?;
            }
            out.push(']');
        }
        V::Map(map) => {
            let mut keys: Vec<&Arc<str>> = map.keys().collect();
            keys.sort_unstable();
            out.push('{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_ascii(out, key);
                out.push(':');
                write_sorted(out, &map[key])?;
            }
            out.push('}');
        }
    }
    Ok(())
}

/// A str as `json.dumps` writes it with `ensure_ascii`.
fn write_ascii(out: &mut String, text: &str) {
    let units: Vec<u16> = text.encode_utf16().collect();
    let written = dumps_texts(&[&units]);
    // `dumps_texts` writes a list of one str: keep the str.
    out.push_str(&written[1..written.len() - 1]);
}

// ---- json.loads and json.dumps of a relationship key ------------------------------------------------

/// What `json.loads` gives, as far as the relationship keys are read: a str (as its UTF-16 code
/// units, since an escape may give a lone surrogate), a list, or any other value.
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Str(Vec<u16>),
    List(Vec<Json>),
    Dict,
    Other,
}

impl Json {
    /// `value in table` of a dict keyed by str: a list or a dict raises its `TypeError`.
    pub fn key_in(&self, has: impl FnOnce(&str) -> bool) -> Result<bool> {
        match self {
            Json::Str(units) => Ok(String::from_utf16(units).is_ok_and(|text| has(&text))),
            Json::List(_) => Err(Error::Type("unhashable type: 'list'".to_owned())),
            Json::Dict => Err(Error::Type("unhashable type: 'dict'".to_owned())),
            Json::Other => Ok(false),
        }
    }
}

/// `json.loads(text)`: the value, None for a `JSONDecodeError`, or the `ValueError` of an int of more
/// than 4300 digits.
pub fn loads(text: &str) -> Result<Option<Json>> {
    let mut parser = Parser { units: text.encode_utf16().collect(), at: 0 };
    parser.space();
    let Some(value) = parser.value(0)? else {
        return Ok(None);
    };
    parser.space();
    Ok((parser.at == parser.units.len()).then_some(value))
}

/// `json.dumps(items, separators=(",", ":"))` of a list of str (ASCII, escapes as `ensure_ascii`).
pub fn dumps_texts(items: &[&[u16]]) -> String {
    let mut out = String::from("[");
    for (index, units) in items.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push('"');
        for &unit in *units {
            match unit {
                0x22 => out.push_str("\\\""),
                0x5c => out.push_str("\\\\"),
                0x0a => out.push_str("\\n"),
                0x0d => out.push_str("\\r"),
                0x09 => out.push_str("\\t"),
                0x08 => out.push_str("\\b"),
                0x0c => out.push_str("\\f"),
                0x20..=0x7e => out.push(char::from(unit as u8)),
                _ => out.push_str(&format!("\\u{unit:04x}")),
            }
        }
        out.push('"');
    }
    out.push(']');
    out
}

/// Python's `json` scanner (`strict=True`), on the UTF-16 units of the text.
struct Parser {
    units: Vec<u16>,
    at: usize,
}

/// How deep the scanner goes before Python's `RecursionError`; a key that deep is not read here.
const MAX_JSON_DEPTH: usize = 900;

impl Parser {
    fn peek(&self) -> Option<u16> {
        self.units.get(self.at).copied()
    }

    fn space(&mut self) {
        while matches!(self.peek(), Some(0x20 | 0x09 | 0x0a | 0x0d)) {
            self.at += 1;
        }
    }

    fn literal(&mut self, word: &str) -> bool {
        let units: Vec<u16> = word.encode_utf16().collect();
        if self.units[self.at..].starts_with(&units) {
            self.at += units.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self, depth: usize) -> Result<Option<Json>> {
        if depth > MAX_JSON_DEPTH {
            return Err(Error::unsupported("a relationship key nests too deep"));
        }
        let Some(unit) = self.peek() else {
            return Ok(None);
        };
        match unit {
            0x22 => {
                self.at += 1;
                Ok(self.string().map(Json::Str))
            }
            0x7b => {
                self.at += 1;
                self.space();
                if self.peek() == Some(0x7d) {
                    self.at += 1;
                    return Ok(Some(Json::Dict));
                }
                loop {
                    if self.peek() != Some(0x22) {
                        return Ok(None);
                    }
                    self.at += 1;
                    if self.string().is_none() {
                        return Ok(None);
                    }
                    self.space();
                    if self.peek() != Some(0x3a) {
                        return Ok(None);
                    }
                    self.at += 1;
                    self.space();
                    if self.value(depth + 1)?.is_none() {
                        return Ok(None);
                    }
                    self.space();
                    match self.peek() {
                        Some(0x7d) => {
                            self.at += 1;
                            return Ok(Some(Json::Dict));
                        }
                        Some(0x2c) => {
                            self.at += 1;
                            self.space();
                        }
                        _ => return Ok(None),
                    }
                }
            }
            0x5b => {
                self.at += 1;
                self.space();
                let mut items = Vec::new();
                if self.peek() == Some(0x5d) {
                    self.at += 1;
                    return Ok(Some(Json::List(items)));
                }
                loop {
                    let Some(item) = self.value(depth + 1)? else {
                        return Ok(None);
                    };
                    items.push(item);
                    self.space();
                    match self.peek() {
                        Some(0x5d) => {
                            self.at += 1;
                            return Ok(Some(Json::List(items)));
                        }
                        Some(0x2c) => {
                            self.at += 1;
                            self.space();
                        }
                        _ => return Ok(None),
                    }
                }
            }
            _ if self.literal("null") || self.literal("true") || self.literal("false") => Ok(Some(Json::Other)),
            _ if self.literal("NaN") || self.literal("Infinity") || self.literal("-Infinity") => Ok(Some(Json::Other)),
            _ => self.number(),
        }
    }

    /// `-?(0|[1-9]\d*)(\.\d+)?([eE][-+]?\d+)?`; an int of more than 4300 digits raises as `int()`.
    fn number(&mut self) -> Result<Option<Json>> {
        let digit = |unit: Option<u16>| matches!(unit, Some(0x30..=0x39));
        let start = self.at;
        if self.peek() == Some(0x2d) {
            self.at += 1;
        }
        match self.peek() {
            Some(0x30) => self.at += 1,
            Some(0x31..=0x39) => {
                while digit(self.peek()) {
                    self.at += 1;
                }
            }
            _ => {
                self.at = start;
                return Ok(None);
            }
        }
        let integer_end = self.at;
        let mut is_float = false;
        if self.peek() == Some(0x2e) && digit(self.units.get(self.at + 1).copied()) {
            self.at += 1;
            while digit(self.peek()) {
                self.at += 1;
            }
            is_float = true;
        }
        if matches!(self.peek(), Some(0x65 | 0x45)) {
            let mark = self.at;
            self.at += 1;
            if matches!(self.peek(), Some(0x2b | 0x2d)) {
                self.at += 1;
            }
            if digit(self.peek()) {
                while digit(self.peek()) {
                    self.at += 1;
                }
                is_float = true;
            } else {
                self.at = mark;
            }
        }
        if !is_float {
            let digits = self.units[start..integer_end].iter().filter(|unit| **unit != 0x2d).count();
            if digits > 4300 {
                return Err(Error::Value(format!(
                    "Exceeds the limit (4300 digits) for integer string conversion: value has {digits} digits; use sys.set_int_max_str_digits() to increase the limit"
                )));
            }
        }
        Ok(Some(Json::Other))
    }

    /// The rest of a string after its opening quote, or None where the scanner raises.
    fn string(&mut self) -> Option<Vec<u16>> {
        let mut out = Vec::new();
        loop {
            let unit = self.peek()?;
            self.at += 1;
            match unit {
                0x22 => return Some(out),
                0x5c => {
                    let escape = self.peek()?;
                    self.at += 1;
                    match escape {
                        0x22 | 0x5c | 0x2f => out.push(escape),
                        0x62 => out.push(0x08),
                        0x66 => out.push(0x0c),
                        0x6e => out.push(0x0a),
                        0x72 => out.push(0x0d),
                        0x74 => out.push(0x09),
                        0x75 => {
                            let first = self.hex4()?;
                            // A high surrogate followed by an escaped low one is one character.
                            if (0xd800..0xdc00).contains(&first) && self.units.get(self.at..self.at + 2) == Some(&[0x5c, 0x75]) {
                                let mark = self.at;
                                self.at += 2;
                                match self.hex4() {
                                    Some(second) if (0xdc00..0xe000).contains(&second) => {
                                        out.push(first);
                                        out.push(second);
                                        continue;
                                    }
                                    _ => self.at = mark,
                                }
                            }
                            out.push(first);
                        }
                        _ => return None,
                    }
                }
                0x00..=0x1f => return None,
                _ => out.push(unit),
            }
        }
    }

    fn hex4(&mut self) -> Option<u16> {
        let units = self.units.get(self.at..self.at + 4)?;
        let text = String::from_utf16(units).ok()?;
        if !text.chars().all(|char| char.is_ascii_hexdigit()) {
            return None;
        }
        self.at += 4;
        u16::from_str_radix(&text, 16).ok()
    }
}
