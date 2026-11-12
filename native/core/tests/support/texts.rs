//! What the core's tests that call a rule directly share (Issue #225, lane M2): the canonical text
//! of a world in a cell, the outcome a rule raised, and events changed as the pytest files changed
//! them.
//!
//! The worlds are read with `cases::entry` (a variant is stored as the changes from another world
//! of its file). `canonical_of` is `WorldState.canonical_json()` of the world in a cell
//! (`canonical::canonical_json`, with the entry's `rng_state`): a test that the pytest file ran
//! "with the world unchanged" compares this text before and after the rule.

use serde_json::Value;
use sheltersim_core::cell::Shard;
use sheltersim_core::checkpoint::sha256_hex;
use sheltersim_core::error::{Error, Outcome};
use sheltersim_core::events::EventRow;
use sheltersim_core::world::{World, WorldCell};

use super::{canonical, load};

/// A value of `changed`'s payload that drops the key (`_DROP` and `...` of the pytest files).
pub const DROP: &str = "\u{0}drop";

/// The World of an entry, in a cell.
pub fn cell(entry: &Value) -> Shard<World> {
    Shard::new(load::world(&entry["world"], &entry["slots"]))
}

/// `WorldState.canonical_json()` of the world in a cell, with the entry's `rng_state`.
pub fn canonical_of(cell: &Shard<World>, entry: &Value) -> String {
    let bytes = cell.with(|world| canonical::canonical_json(world, &entry["world"]["rng_state"])).unwrap();
    String::from_utf8(bytes).unwrap()
}

/// The sha256 of a text, in hex.
pub fn sha(text: &str) -> String {
    sha256_hex(text.as_bytes())
}

/// The class and text of the outcome a rule raised: `EventApplicationError` or `EventSuppressed`.
pub fn outcome<T>(result: Result<T, Error>) -> (&'static str, String) {
    match result {
        Err(Error::Outcome(Outcome::Failed, text)) => ("EventApplicationError", text),
        Err(Error::Outcome(Outcome::Suppressed, text)) => ("EventSuppressed", text),
        Err(other) => panic!("not an outcome of the rule: {other:?}"),
        Ok(_) => panic!("not an outcome of the rule: it passed"),
    }
}

/// The text of an `EventApplicationError` a rule raised.
pub fn failed<T>(result: Result<T, Error>) -> String {
    let (class, text) = outcome(result);
    assert_eq!(class, "EventApplicationError", "{text}");
    text
}

/// The text of an `EventSuppressed` a rule raised.
pub fn suppressed<T>(result: Result<T, Error>) -> String {
    let (class, text) = outcome(result);
    assert_eq!(class, "EventSuppressed", "{text}");
    text
}

/// The event of the world's queue (`ScheduledEvent.to_dict()`) of this type, and of this `op` in
/// its payload where given: the only one.
pub fn pending(entry: &Value, event_type: &str, op: Option<&str>) -> Value {
    let found: Vec<&Value> = entry["world"]["event_queue"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["event_type"] == event_type && op.is_none_or(|op| event["payload"]["op"] == op))
        .collect();
    assert_eq!(found.len(), 1, "{event_type}");
    found[0].clone()
}

/// The event of the world's queue with this ID.
pub fn pending_id(entry: &Value, event_id: &str) -> Value {
    entry["world"]["event_queue"].as_array().unwrap().iter().find(|event| event["event_id"] == event_id).unwrap_or_else(|| panic!("no event {event_id}")).clone()
}

/// The event with its payload's keys set (`DROP` drops one) and its own fields set.
pub fn changed(event: &Value, payload: Value, fields: Value) -> Value {
    let mut event = event.clone();
    for (key, value) in payload.as_object().unwrap() {
        let payload = event["payload"].as_object_mut().unwrap();
        if value.as_str() == Some(DROP) {
            payload.remove(key);
        } else {
            payload.insert(key.clone(), value.clone());
        }
    }
    for (key, value) in fields.as_object().unwrap() {
        if value.as_str() == Some(DROP) {
            event.as_object_mut().unwrap().remove(key);
        } else {
            event[key.as_str()] = value.clone();
        }
    }
    event
}

/// The core's row of an event as `ScheduledEvent.to_dict()` writes it.
pub fn row(event: &Value) -> EventRow {
    load::event(event)
}

/// An int of JSON written out, for one past i64 (`10**30`).
pub fn big(digits: &str) -> Value {
    serde_json::from_str(digits).unwrap()
}
