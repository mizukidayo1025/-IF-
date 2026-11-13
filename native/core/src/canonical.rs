//! A World's canonical bytes: what `WorldState.canonical_json()` writes for the same world (Issue
//! #225, lane M1; in the crate since #244, PR 4-3, for a host without Python that reports a Run's
//! digests). Each part is the canonical JSON of one top-level key of the document (the rows sorted
//! by ID, the queue in its order, the log's records as it keeps them, the fields and `model_state`
//! by name); the document joins them in the order of their keys, with `rng_state`, which the core's
//! World does not hold, given by the caller.
//!
//! The digests (`digests`, `run_digests`) read the log a record at a time (`log_digest`) and make
//! no copy of it: after 7 days of 100 people the log's JSON is about 300 MiB, and a copy of it,
//! grown by doubling, took the WASM memory from 500 MiB to 1.59 GiB (#252).

use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::checkpoint::{hex, sha256_hex};
use crate::entities::{EntityRow, Table};
use crate::error::{Error, Result};
use crate::json::Encoder;
use crate::value::V;
use crate::world::{Place, World};

fn table<R: EntityRow>(table: &Table<R>, version: i64) -> Result<Vec<u8>> {
    let mut rows: Vec<&R> = table.iter().map(|(_, row)| row).collect();
    rows.sort_by(|first, second| first.id().as_bytes().cmp(second.id().as_bytes()));
    let mut encoder = Encoder::with_capacity(1024);
    encoder.out.push(b'[');
    for (index, row) in rows.into_iter().enumerate() {
        if index > 0 {
            encoder.out.push(b',');
        }
        row.write_json(&mut encoder, version)?;
    }
    encoder.out.push(b']');
    Ok(encoder.out)
}

fn slot_json(world: &World, name: &str) -> Result<Vec<u8>> {
    let slot = world.slots.get(name).ok_or_else(|| Error::unsupported(format!("the world does not own {name}")))?.try_borrow()?;
    let row = slot.row.as_deref().ok_or_else(|| Error::unsupported(format!("{name} has no row")))?;
    let mut encoder = Encoder::with_capacity(256);
    row.write_json(&mut encoder)?;
    Ok(encoder.out)
}

fn int(value: i64) -> Vec<u8> {
    let mut encoder = Encoder::with_capacity(16);
    encoder.int(value);
    encoder.out
}

/// The canonical JSON of each top-level key of the world's document but `rng_state`, in the order
/// of the keys.
pub fn parts(world: &World) -> Result<Vec<(&'static str, Vec<u8>)>> {
    parts_with(world, true)
}

/// `parts`, but `event_log`'s is empty unless `with_log`: the log is most of a long Run's
/// document, and a digest of it reads it a record at a time instead (`log_digest`, #252).
fn parts_with(world: &World, with_log: bool) -> Result<Vec<(&'static str, Vec<u8>)>> {
    let version = world.schema_version()?;
    let scalars = world.scalars()?;
    let mut scenario = Encoder::with_capacity(64);
    scenario.string(scalars.scenario_id()?);
    let duration = int(scalars.duration_minutes()?);
    let minute = int(scalars.minute_index()?);
    let schema = int(scalars.schema_version()?);
    let seed = scalars.seed()?.text.as_bytes().to_vec();
    drop(scalars);
    let rows = world.rows()?;
    let agents = table(&rows.agents, version)?;
    let zones = table(&rows.zones, version)?;
    let edges = table(&rows.zone_edges, version)?;
    let facilities = table(&rows.facilities, version)?;
    let staff = table(&rows.staff, version)?;
    let requests = table(&rows.support_requests, version)?;
    let tasks = table(&rows.active_tasks, version)?;
    drop(rows);
    let queue = world.queue()?;
    let mut pending = Encoder::with_capacity(1024);
    pending.out.push(b'[');
    for (index, (_, serial)) in queue.ordered().iter().enumerate() {
        if index > 0 {
            pending.out.push(b',');
        }
        let row = queue.slot(*serial).row.as_ref().ok_or_else(|| Error::unsupported("a pending event without its row"))?;
        row.write_json(&mut pending)?;
    }
    pending.out.push(b']');
    drop(queue);
    let mut records = Vec::new();
    if with_log {
        write_log(world, |raw| records.extend_from_slice(raw))?;
    }
    let mut names: Vec<&Arc<str>> = Vec::new();
    for name in world.slots.names() {
        let Some(slot) = world.slots.get(name) else { continue };
        let slot = slot.try_borrow()?;
        if slot.place != Place::Field && slot.row.is_some() {
            names.push(name);
        }
    }
    names.sort_by(|first, second| first.as_bytes().cmp(second.as_bytes()));
    let mut model_state = Encoder::with_capacity(256);
    model_state.out.push(b'{');
    for (index, name) in names.into_iter().enumerate() {
        if index > 0 {
            model_state.out.push(b',');
        }
        model_state.string(name);
        model_state.out.push(b':');
        model_state.out.extend_from_slice(&slot_json(world, name)?);
    }
    model_state.out.push(b'}');
    Ok(vec![
        ("active_tasks", tasks),
        ("agents", agents),
        ("duration_minutes", duration),
        ("event_log", records),
        ("event_queue", pending.out),
        ("facilities", facilities),
        ("id_counters", slot_json(world, "id_counters")?),
        ("metric_state", slot_json(world, "metric_state")?),
        ("minute_index", minute),
        ("model_configs", slot_json(world, "model_configs")?),
        ("model_state", model_state.out),
        ("resources", slot_json(world, "resources")?),
        ("scenario_id", scenario.out),
        ("schema_version", schema),
        ("seed", seed),
        ("staff", staff),
        ("support_requests", requests),
        ("zone_edges", edges),
        ("zones", zones),
    ])
}

/// The canonical JSON of the log's records, `[record,record,...]`, given to `out` a piece at a
/// time.
fn write_log(world: &World, mut out: impl FnMut(&[u8])) -> Result<()> {
    let log = world.log()?;
    out(b"[");
    for index in 0..log.len() {
        if index > 0 {
            out(b",");
        }
        out(log.written(index).ok_or_else(|| Error::unsupported("a record without its bytes"))?);
    }
    out(b"]");
    Ok(())
}

/// The sha256 of the `event_log` part, in hex, read a record at a time: no copy of the whole log
/// is made (#252).
pub fn log_digest(world: &World) -> Result<String> {
    let mut hasher = Sha256::new();
    write_log(world, |raw| hasher.update(raw))?;
    Ok(hex(&hasher.finalize()))
}

/// Each part but `event_log` as `parts` gives it, and the digest of `event_log` (`log_digest`) in
/// its place: for the digests, which need no copy of the log.
fn digested_parts(world: &World) -> Result<(Vec<(&'static str, Vec<u8>)>, String)> {
    let mut all = parts_with(world, false)?;
    all.retain(|(name, _)| *name != "event_log");
    Ok((all, log_digest(world)?))
}

/// The sha256 of each part, in hex, by its name (the digests `tests/support/core_tick.py` writes
/// for the parts it covers), in the order of `parts`.
pub fn digests(world: &World) -> Result<Vec<(&'static str, String)>> {
    let (all, log) = digested_parts(world)?;
    let mut digests: Vec<(&'static str, String)> = all.into_iter().map(|(name, raw)| (name, sha256_hex(&raw))).collect();
    let at = digests.iter().position(|(name, _)| *name > "event_log").unwrap_or(digests.len());
    digests.insert(at, ("event_log", log));
    Ok(digests)
}

/// The parts `tests/support/core_tick.py` digests a Run's world by (`PARTS`), in its order; the
/// scalars (`schema_version`, `scenario_id`, `seed`, `duration_minutes`, `minute_index`) are one
/// more part, `scalars`, their JSON list.
pub const RUN_PARTS: [&str; 14] = [
    "agents",
    "zones",
    "zone_edges",
    "facilities",
    "staff",
    "support_requests",
    "active_tasks",
    "resources",
    "event_queue",
    "event_log",
    "id_counters",
    "metric_state",
    "model_configs",
    "model_state",
];

/// The scalars of the `scalars` part, in its order.
const SCALARS: [&str; 5] = ["schema_version", "scenario_id", "seed", "duration_minutes", "minute_index"];

/// The digests of a Run's world as `tests/support/core_tick.py` writes them (`world_digests`):
/// each part of `RUN_PARTS`, then `scalars`.
pub fn run_digests(world: &World) -> Result<Vec<(&'static str, String)>> {
    let (mut all, mut log) = digested_parts(world)?;
    let mut take = |name: &str| -> Result<Vec<u8>> {
        let at = all.iter().position(|(part, _)| *part == name).ok_or_else(|| Error::unsupported(format!("no part {name}")))?;
        Ok(all.remove(at).1)
    };
    let mut digests = Vec::with_capacity(RUN_PARTS.len() + 1);
    for name in RUN_PARTS {
        let digest = if name == "event_log" { std::mem::take(&mut log) } else { sha256_hex(&take(name)?) };
        digests.push((name, digest));
    }
    let mut scalars = vec![b'['];
    for (index, name) in SCALARS.into_iter().enumerate() {
        if index > 0 {
            scalars.push(b',');
        }
        scalars.extend_from_slice(&take(name)?);
    }
    scalars.push(b']');
    digests.push(("scalars", sha256_hex(&scalars)));
    Ok(digests)
}

/// `WorldState.canonical_json()` of the world, with its `rng_state` (the document's).
pub fn canonical_json(world: &World, rng_state: &V) -> Result<Vec<u8>> {
    let mut rng = Encoder::with_capacity(64);
    rng_state.write_json(&mut rng)?;
    let mut parts = parts(world)?;
    let at = parts.iter().position(|(name, _)| *name > "rng_state").unwrap_or(parts.len());
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
    Ok(out)
}
