//! A World's canonical bytes: what `WorldState.canonical_json()` writes for the same world (Issue
//! #225, lane M1). Each part is the canonical JSON of one top-level key of the document (the rows
//! sorted by ID, the queue in its order, the log's records as it keeps them, the fields and
//! `model_state` by name); the document joins them in the order of their keys, with `rng_state`,
//! which the core's World does not hold, given by the caller.

use std::sync::Arc;

use serde_json::Value;
use sheltersim_core::entities::{EntityRow, Table};
use sheltersim_core::json::Encoder;
use sheltersim_core::world::{Place, World};

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

/// The canonical JSON of each top-level key of the world's document but `rng_state`, in the order
/// of the keys.
pub fn parts(world: &World) -> Vec<(&'static str, Vec<u8>)> {
    let version = world.schema_version().unwrap();
    let scalars = world.scalars().unwrap();
    let int = |value: i64| {
        let mut encoder = Encoder::with_capacity(16);
        encoder.int(value);
        encoder.out
    };
    let mut scenario = Encoder::with_capacity(64);
    scenario.string(scalars.scenario_id().unwrap());
    let duration = int(scalars.duration_minutes().unwrap());
    let minute = int(scalars.minute_index().unwrap());
    let schema = int(scalars.schema_version().unwrap());
    let seed = scalars.seed().unwrap().text.as_bytes().to_vec();
    drop(scalars);
    let rows = world.rows().unwrap();
    let agents = table(&rows.agents, version);
    let zones = table(&rows.zones, version);
    let edges = table(&rows.zone_edges, version);
    let facilities = table(&rows.facilities, version);
    let staff = table(&rows.staff, version);
    let requests = table(&rows.support_requests, version);
    let tasks = table(&rows.active_tasks, version);
    drop(rows);
    let queue = world.queue().unwrap();
    let mut pending = Encoder::with_capacity(1024);
    pending.out.push(b'[');
    for (index, (_, serial)) in queue.ordered().iter().enumerate() {
        if index > 0 {
            pending.out.push(b',');
        }
        queue.slot(*serial).row.as_ref().expect("an event with its row").write_json(&mut pending).unwrap();
    }
    pending.out.push(b']');
    drop(queue);
    let log = world.log().unwrap();
    let mut records = vec![b'['];
    for index in 0..log.len() {
        if index > 0 {
            records.push(b',');
        }
        records.extend_from_slice(log.written(index).expect("a record's bytes"));
    }
    records.push(b']');
    drop(log);
    let mut names: Vec<&Arc<str>> = world
        .slots
        .names()
        .filter(|name| {
            let slot = world.slots.get(name).unwrap().try_borrow().unwrap();
            slot.place != Place::Field && slot.row.is_some()
        })
        .collect();
    names.sort_by(|first, second| first.as_bytes().cmp(second.as_bytes()));
    let mut model_state = Encoder::with_capacity(256);
    model_state.out.push(b'{');
    for (index, name) in names.into_iter().enumerate() {
        if index > 0 {
            model_state.out.push(b',');
        }
        model_state.string(name);
        model_state.out.push(b':');
        model_state.out.extend_from_slice(&slot_json(world, name));
    }
    model_state.out.push(b'}');
    vec![
        ("active_tasks", tasks),
        ("agents", agents),
        ("duration_minutes", duration),
        ("event_log", records),
        ("event_queue", pending.out),
        ("facilities", facilities),
        ("id_counters", slot_json(world, "id_counters")),
        ("metric_state", slot_json(world, "metric_state")),
        ("minute_index", minute),
        ("model_configs", slot_json(world, "model_configs")),
        ("model_state", model_state.out),
        ("resources", slot_json(world, "resources")),
        ("scenario_id", scenario.out),
        ("schema_version", schema),
        ("seed", seed),
        ("staff", staff),
        ("support_requests", requests),
        ("zone_edges", edges),
        ("zones", zones),
    ]
}

/// `WorldState.canonical_json()` of the world, with its `rng_state` (the document's).
pub fn canonical_json(world: &World, rng_state: &Value) -> Vec<u8> {
    let mut rng = Encoder::with_capacity(64);
    super::load::v(rng_state).write_json(&mut rng).unwrap();
    let mut parts = parts(world);
    let at = parts.iter().position(|(name, _)| *name > "rng_state").unwrap();
    parts.insert(at, ("rng_state", rng.out));
    let mut out = vec![b'{'];
    for (index, (name, raw)) in parts.into_iter().enumerate() {
        if index > 0 {
            out.push(b',');
        }
        let mut key = Encoder::with_capacity(32);
        key.string(name);
        out.extend_from_slice(&key.out);
        out.push(b':');
        out.extend_from_slice(&raw);
    }
    out.push(b'}');
    out
}
