//! What Python does with a JSON value where the validators of lane E (#350) read one: `isinstance`,
//! `type(x) is int`, `==`, `in`, `x[key]`, `math.isfinite`, and the `TypeError`s and `KeyError`s
//! CPython 3.12 raises on the way (`unhashable type: 'list'`, `'int' object is not subscriptable`,
//! ...), so that a broken value meets the error Python meets.

use std::cmp::Ordering;

use crate::error::{Error, Result};
use crate::value::V;

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

/// How Python holds the value a rule reads: as the plain document (`read_only_state`), or as the
/// tree of the world's `model_state` (`world.model_state.get(...)`), whose lists and dicts are
/// `TrackedList` and `TrackedDict`, as an error's text names them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

/// `hash(value)`'s error for a list or a dict.
pub fn unhashable(value: &V, held: Held) -> Result<()> {
    match value {
        V::List(_) | V::Map(_) => Err(Error::Type(format!("unhashable type: '{}'", held_type_name(value, held)))),
        _ => Ok(()),
    }
}

/// `value in table`, for a dict or a set whose keys are str (`world.zones`, `{"a", "b"}`): the
/// lookup of a str, False for another hashable value, `TypeError` for a list or a dict.
pub fn in_keys(value: &V, held: Held, has: impl FnOnce(&str) -> bool) -> Result<bool> {
    unhashable(value, held)?;
    Ok(match value {
        V::Str(text) => has(text),
        _ => false,
    })
}

/// `value in {texts...}`.
pub fn in_texts(value: &V, held: Held, texts: &[&str]) -> Result<bool> {
    in_keys(value, held, |text| texts.contains(&text))
}

/// `isinstance(value, int)`: bool is an int.
pub fn is_int(value: &V) -> bool {
    matches!(value, V::Int(_) | V::Big(_) | V::Bool(_))
}

/// `type(value) is int`.
pub fn is_exact_int(value: &V) -> bool {
    matches!(value, V::Int(_) | V::Big(_))
}

/// `isinstance(value, (int, float)) and not isinstance(value, bool)`.
pub fn is_number(value: &V) -> bool {
    matches!(value, V::Int(_) | V::Big(_) | V::Float(_))
}

/// The value of a number as a float (a bool is 0 or 1; an int too large is infinite).
pub fn float_of(value: &V) -> Option<f64> {
    match value {
        V::Bool(flag) => Some(f64::from(u8::from(*flag))),
        V::Int(number) => Some(*number as f64),
        V::Big(text) => text.parse::<f64>().ok(),
        V::Float(number) => Some(*number),
        _ => None,
    }
}

/// `math.isfinite(value)` of a number. An int beyond the floats is infinite here; CPython raises
/// `OverflowError` for it, which no error of the core stands for.
pub fn is_finite(value: &V) -> bool {
    match value {
        V::Int(_) | V::Bool(_) => true,
        other => float_of(other).is_some_and(f64::is_finite),
    }
}

/// The sign and the digits of an int (`Int`, `Big` or a bool).
fn int_parts(value: &V) -> Option<(bool, String)> {
    let text = match value {
        V::Bool(flag) => u8::from(*flag).to_string(),
        V::Int(number) => number.to_string(),
        V::Big(text) => text.to_string(),
        _ => return None,
    };
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest.to_owned()),
        None => (false, text),
    };
    let digits = digits.trim_start_matches('0').to_owned();
    Some((negative && !digits.is_empty(), digits))
}

/// The order of two ints, exactly (`Int`, `Big` or bools).
pub fn int_cmp(left: &V, right: &V) -> Option<Ordering> {
    if let (V::Int(a), V::Int(b)) = (left, right) {
        return Some(a.cmp(b));
    }
    let (left_negative, left_digits) = int_parts(left)?;
    let (right_negative, right_digits) = int_parts(right)?;
    let magnitude = left_digits.len().cmp(&right_digits.len()).then_with(|| left_digits.cmp(&right_digits));
    Some(match (left_negative, right_negative) {
        (false, true) => Ordering::Greater,
        (true, false) => Ordering::Less,
        (false, false) => magnitude,
        (true, true) => magnitude.reverse(),
    })
}

/// `left < right`, `left <= right`, ... of two numbers, as Python orders an int and a float.
pub fn num_cmp(left: &V, right: &V) -> Option<Ordering> {
    if is_int(left) && is_int(right) {
        return int_cmp(left, right);
    }
    float_of(left)?.partial_cmp(&float_of(right)?)
}

/// The order of an int and an i64.
pub fn cmp_i64(value: &V, other: i64) -> Option<Ordering> {
    int_cmp(value, &V::Int(other))
}

/// `left == right` in Python: `1 == 1.0 == True`, lists item by item, dicts by their keys.
pub fn eq(left: &V, right: &V) -> bool {
    match (left, right) {
        (V::Null, V::Null) => true,
        (V::Str(a), V::Str(b)) => a == b,
        (V::List(a), V::List(b)) => a.len() == b.len() && a.iter().zip(b).all(|(x, y)| eq(x, y)),
        (V::Map(a), V::Map(b)) => a.len() == b.len() && a.iter().all(|(key, x)| b.get(key).is_some_and(|y| eq(x, y))),
        (a, b) if float_of(a).is_some() && float_of(b).is_some() => num_cmp(a, b) == Some(Ordering::Equal),
        _ => false,
    }
}

/// `left == "text"`.
pub fn is_text(value: &V, text: &str) -> bool {
    matches!(value, V::Str(found) if &**found == text)
}

/// `needle in container`.
pub fn contains(container: &V, needle: &V, held: Held) -> Result<bool> {
    match container {
        V::Map(map) => in_keys(needle, held, |key| map.contains_key(key)),
        V::List(items) => Ok(items.iter().any(|item| eq(item, needle))),
        V::Str(text) => match needle {
            V::Str(part) => Ok(text.contains(&**part)),
            other => Err(Error::Type(format!("'in <string>' requires string as left operand, not {}", held_type_name(other, held)))),
        },
        other => Err(Error::Type(format!("argument of type '{}' is not iterable", type_name(other)))),
    }
}

/// `container[key]` for a str key.
pub fn item<'a>(container: &'a V, key: &str) -> Result<&'a V> {
    match container {
        V::Map(map) => map.get(key).ok_or_else(|| Error::Key(key.to_owned())),
        V::List(_) => Err(Error::Type("list indices must be integers or slices, not str".to_owned())),
        V::Str(_) => Err(Error::Type("string indices must be integers, not 'str'".to_owned())),
        other => Err(Error::Type(format!("'{}' object is not subscriptable", type_name(other)))),
    }
}

/// `set(map) == {names...}`: the keys of a dict are exactly these.
pub fn has_exactly(map: &indexmap::IndexMap<std::sync::Arc<str>, V>, names: &[&str]) -> bool {
    map.len() == names.len() && names.iter().all(|name| map.contains_key(*name))
}

/// What a hashable value is equal to, as a key: `1`, `1.0` and `True` are one key.
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum Key {
    None,
    Text(std::sync::Arc<str>),
    Int(String),
    Float(u64),
}

/// The key of a hashable value, or the `TypeError` of `hash(value)`.
pub fn key_of(value: &V, held: Held) -> Result<Key> {
    unhashable(value, held)?;
    Ok(match value {
        V::Null => Key::None,
        V::Str(text) => Key::Text(text.clone()),
        V::Float(number) if number.is_finite() && number.fract() == 0.0 && number.abs() < 1e300 => Key::Int(format!("{number:.0}")),
        V::Float(number) => Key::Float(if *number == 0.0 { 0 } else { number.to_bits() }),
        other => {
            let (negative, digits) = int_parts(other).unwrap_or_default();
            let digits = if digits.is_empty() { "0".to_owned() } else { digits };
            Key::Int(if negative { format!("-{digits}") } else { digits })
        }
    })
}

/// `len(items) != len(set(items))` for a list: whether an item repeats, or the `TypeError` of an
/// unhashable item. Python's `set(...)` hashes the items in order and stops at the first it cannot.
pub fn repeats(items: &[V], held: Held) -> Result<bool> {
    let mut seen = std::collections::HashSet::with_capacity(items.len());
    let mut repeated = false;
    for item in items {
        repeated |= !seen.insert(key_of(item, held)?);
    }
    Ok(repeated)
}
