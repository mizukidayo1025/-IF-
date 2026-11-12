//! What the core's tests of saved texts share (Issue #225, lane M2): the worlds of
//! `tests/support/core_fixtures_texts.py`, the world's canonical text as Python writes it, the
//! outcome a rule raised, and events changed as the pytest files changed them.
//!
//! A world of those fixtures is a whole entry (`core_fixtures.export_world`) or a variant of
//! another entry of the same file: `{"base": name, "set": [[path, value], ...], "unset": [path,
//! ...]}`, the keys of each path from the entry down. `entry` puts the two together again.
//!
//! `canonical` is `WorldState.canonical_json()` of the world: each part as the core writes it (the
//! bytes `tick_core.rs` checks against Python's), under the sorted keys, with the entry's
//! `rng_state` (the core's World has none). A test that the pytest file ran "with the world
//! unchanged" compares these bytes before and after the rule.

use std::sync::Arc;

use serde_json::Value;
use sheltersim_core::cell::Shard;
use sheltersim_core::checkpoint::sha256_hex;
use sheltersim_core::entities::{EntityRow, Table};
use sheltersim_core::error::{Error, Outcome};
use sheltersim_core::events::EventRow;
use sheltersim_core::json::Encoder;
use sheltersim_core::world::{Place, World, WorldCell};

use super::dumps::Dumps;
use super::load;

/// A value of `changed`'s payload that drops the key (`_DROP` and `...` of the pytest files).
pub const DROP: &str = "\u{0}drop";

/// The named world of a fixture file, its variant put together with its base.
pub fn entry(file: &str, name: &str) -> Value {
    let mut entry = load::entry(file, name);
    let Some(base) = entry.get("base").and_then(Value::as_str).map(str::to_owned) else {
        return entry;
    };
    let mut whole = self::entry(file, &base);
    for change in entry["set"].as_array().unwrap() {
        let path: Vec<&str> = change[0].as_array().unwrap().iter().map(|key| key.as_str().unwrap()).collect();
        let (last, parents) = path.split_last().unwrap();
        let mut at = &mut whole;
        for key in parents {
            at = at.as_object_mut().unwrap().entry(key.to_string()).or_insert_with(|| Value::Object(Default::default()));
        }
        at.as_object_mut().unwrap().insert(last.to_string(), change[1].clone());
    }
    for path in entry["unset"].as_array().unwrap() {
        let path: Vec<&str> = path.as_array().unwrap().iter().map(|key| key.as_str().unwrap()).collect();
        let (last, parents) = path.split_last().unwrap();
        let at = parents.iter().fold(&mut whole, |at, key| &mut at[*key]);
        at.as_object_mut().unwrap().remove(*last);
    }
    for key in ["base", "set", "unset"] {
        entry.as_object_mut().unwrap().remove(key);
    }
    // The variant's own keys (a builder's) over the base's.
    for (key, value) in entry.as_object_mut().unwrap().iter_mut() {
        whole[key.as_str()] = value.take();
    }
    whole
}

/// The World of an entry, in a cell.
pub fn cell(entry: &Value) -> Shard<World> {
    Shard::new(load::world(&entry["world"], &entry["slots"]))
}

fn table<R: EntityRow>(table: &Table<R>, version: i64) -> Vec<u8> {
    let mut rows: Vec<&R> = table.iter().map(|(_, row)| row).collect();
    rows.sort_by(|first, second| first.id().as_bytes().cmp(second.id().as_bytes()));
    let mut encoder = Encoder::with_capacity(1024);
    encoder.out.push(b'[');
    for (index, row) in rows.into_iter().enumerate() {
        if index > 0 {
            encoder.out.push(b',');
        }
        row.write_json(&mut encoder, version).unwrap();
    }
    encoder.out.push(b']');
    encoder.out
}

fn slot_json(world: &World, name: &str) -> Vec<u8> {
    let slot = world.slots.get(name).unwrap().try_borrow().unwrap();
    let mut encoder = Encoder::with_capacity(256);
    slot.row.as_deref().unwrap_or_else(|| panic!("{name} has no row")).write_json(&mut encoder).unwrap();
    encoder.out
}

/// `WorldState.canonical_json()` of the world, with `rng_state` as the entry has it.
pub fn canonical(world: &World, entry: &Value) -> String {
    let version = world.schema_version().unwrap();
    let mut parts: Vec<(&str, Vec<u8>)> = Vec::new();
    {
        let rows = world.rows().unwrap();
        parts.push(("active_tasks", table(&rows.active_tasks, version)));
        parts.push(("agents", table(&rows.agents, version)));
        parts.push(("facilities", table(&rows.facilities, version)));
        parts.push(("staff", table(&rows.staff, version)));
        parts.push(("support_requests", table(&rows.support_requests, version)));
        parts.push(("zone_edges", table(&rows.zone_edges, version)));
        parts.push(("zones", table(&rows.zones, version)));
    }
    let queue = world.queue().unwrap();
    let mut encoder = Encoder::with_capacity(1024);
    encoder.out.push(b'[');
    for (index, (_, serial)) in queue.ordered().iter().enumerate() {
        if index > 0 {
            encoder.out.push(b',');
        }
        queue.slot(*serial).row.as_ref().expect("an event with its row").write_json(&mut encoder).unwrap();
    }
    encoder.out.push(b']');
    parts.push(("event_queue", encoder.out));
    drop(queue);
    let log = world.log().unwrap();
    let mut written = vec![b'['];
    for index in 0..log.len() {
        if index > 0 {
            written.push(b',');
        }
        written.extend_from_slice(log.written(index).expect("a record's bytes"));
    }
    written.push(b']');
    parts.push(("event_log", written));
    drop(log);
    for name in ["resources", "id_counters", "metric_state", "model_configs"] {
        parts.push((name, slot_json(world, name)));
    }
    let mut names: Vec<&Arc<str>> = world
        .slots
        .names()
        .filter(|name| {
            let slot = world.slots.get(name).unwrap().try_borrow().unwrap();
            slot.place != Place::Field && slot.row.is_some()
        })
        .collect();
    names.sort_by(|first, second| first.as_bytes().cmp(second.as_bytes()));
    let mut encoder = Encoder::with_capacity(256);
    encoder.out.push(b'{');
    for (index, name) in names.into_iter().enumerate() {
        if index > 0 {
            encoder.out.push(b',');
        }
        encoder.string(name);
        encoder.out.push(b':');
        encoder.out.extend_from_slice(&slot_json(world, name));
    }
    encoder.out.push(b'}');
    parts.push(("model_state", encoder.out));
    let scalars = world.scalars().unwrap();
    let mut scalar = |name: &'static str, write: &dyn Fn(&mut Encoder)| {
        let mut encoder = Encoder::with_capacity(32);
        write(&mut encoder);
        parts.push((name, encoder.out));
    };
    scalar("schema_version", &|encoder| encoder.int(scalars.schema_version().unwrap()));
    scalar("scenario_id", &|encoder| encoder.string(scalars.scenario_id().unwrap()));
    scalar("seed", &|encoder| encoder.out.extend_from_slice(scalars.seed().unwrap().text.as_bytes()));
    scalar("duration_minutes", &|encoder| encoder.int(scalars.duration_minutes().unwrap()));
    scalar("minute_index", &|encoder| encoder.int(scalars.minute_index().unwrap()));
    let rng_state = Dumps::default().unicode().compact().sorted().text(&load::v(&entry["world"]["rng_state"]));
    parts.push(("rng_state", rng_state.into_bytes()));
    parts.sort_by(|first, second| first.0.cmp(second.0));
    let mut out = vec![b'{'];
    for (index, (name, raw)) in parts.into_iter().enumerate() {
        if index > 0 {
            out.push(b',');
        }
        out.extend_from_slice(format!("\"{name}\":").as_bytes());
        out.extend_from_slice(&raw);
    }
    out.push(b'}');
    String::from_utf8(out).unwrap()
}

/// `canonical` of the world in a cell.
pub fn canonical_of(cell: &Shard<World>, entry: &Value) -> String {
    cell.with(|world| canonical(world, entry)).unwrap()
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
