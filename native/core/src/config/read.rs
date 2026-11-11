//! Reading a section from the configs as a `V`, with the reasons the kernels give today when they
//! read the Python tree (`exact_dict`, `exact_int`, `exact_str` and `number` of the PyO3 crate).
//! A `V` holds exact types only, so each check here is the check the kernel made on the tree.

use std::sync::Arc;

use indexmap::IndexMap;

use super::section::Fault;
use crate::value::V;

pub const NOT_A_DICT: &str = "a value is not a plain dict";
pub const NOT_AN_INT: &str = "a value is not an int";
pub const INT_TOO_LARGE: &str = "an int is too large";
pub const NOT_A_STR: &str = "a value is not a str";
pub const NOT_A_FLOAT: &str = "a number does not fit a float";

/// A value a read site takes, or the refusal it says where it reads it.
pub type Read<T> = Result<T, Fault>;

/// `exact_dict`.
pub fn map(value: &V) -> Read<&IndexMap<Arc<str>, V>> {
    match value {
        V::Map(map) => Ok(map),
        _ => Err(Fault::refused(NOT_A_DICT)),
    }
}

/// `exact_int`.
pub fn int(value: &V) -> Read<i64> {
    match value {
        V::Int(number) => Ok(*number),
        V::Big(_) => Err(Fault::refused(INT_TOO_LARGE)),
        _ => Err(Fault::refused(NOT_AN_INT)),
    }
}

/// `exact_str`.
pub fn text(value: &V) -> Read<&Arc<str>> {
    match value {
        V::Str(text) => Ok(text),
        _ => Err(Fault::refused(NOT_A_STR)),
    }
}

/// `number` of the handlers: a float, or an int that a float holds exactly.
pub fn number(value: &V) -> Read<f64> {
    match value {
        V::Float(number) => Ok(*number),
        V::Int(number) if (-(1 << 53)..=1 << 53).contains(number) => Ok(*number as f64),
        V::Int(_) => Err(Fault::refused(NOT_A_FLOAT)),
        other => int(other).map(|number| number as f64),
    }
}

/// `exact_dict(parent).get_item(key)`: the item, or None for a key the map does not have.
pub fn item<'a>(parent: &'a V, key: &str) -> Read<Option<&'a V>> {
    Ok(map(parent)?.get(key))
}

/// `exact_int` of an item, or `default` where the map has no such key (a `.get` with a default).
pub fn int_or(parent: &V, key: &str, default: i64) -> Read<i64> {
    item(parent, key)?.map_or(Ok(default), int)
}
