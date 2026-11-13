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
    parts_with(world, true, true)
}

/// Each part of `parts` but `event_log`, whose bytes are most of a long Run's document: the
/// world a browser's Run stores beside its log (`archive::world_parts`, #252).
pub fn parts_without_log(world: &World) -> Result<Vec<(&'static str, Vec<u8>)>> {
    let mut all = parts_with(world, false, true)?;
    all.retain(|(name, _)| *name != "event_log");
    Ok(all)
}

/// `parts`, but `event_log`'s is empty unless `with_log`: the log is most of a long Run's
/// document, and a digest of it reads it a record at a time instead (`log_digest`, #252); and
/// `model_state`'s is empty unless `with_model_state`, for the tree digest, which reads each
/// namespace on its own (`state_digest`).
fn parts_with(world: &World, with_log: bool, with_model_state: bool) -> Result<Vec<(&'static str, Vec<u8>)>> {
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
    let names = if with_model_state { namespace_names(world)? } else { Vec::new() };
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

/// The names of the world's namespaces (the keys of `model_state`), sorted by their bytes.
fn namespace_names(world: &World) -> Result<Vec<&Arc<str>>> {
    let mut names: Vec<&Arc<str>> = Vec::new();
    for name in world.slots.names() {
        let Some(slot) = world.slots.get(name) else { continue };
        let slot = slot.try_borrow()?;
        if slot.place != Place::Field && slot.row.is_some() {
            names.push(name);
        }
    }
    names.sort_by(|first, second| first.as_bytes().cmp(second.as_bytes()));
    Ok(names)
}

/// The canonical JSON of the log's records, `[record,record,...]`, given to `out` a piece at a
/// time.
fn write_log(world: &World, mut out: impl FnMut(&[u8])) -> Result<()> {
    let log = world.log()?;
    let mut record = Vec::with_capacity(4096);
    out(b"[");
    for index in 0..log.len() {
        if index > 0 {
            out(b",");
        }
        // From its bytes, or from its packed form (the browser's log, #252).
        record.clear();
        if !log.write_record(index, &mut record)? {
            return Err(Error::unsupported("a record without its bytes"));
        }
        out(&record);
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
    let mut all = parts_with(world, false, true)?;
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

// ---- The tree digest of a world (`state_hash`, #264) ---------------------------------------------

/// How many items each hash of a history list covers (`TREE_CHUNK_ITEMS` of
/// `core/checkpoint_cache.py`, a rule of the Checkpoint v2 hash).
pub const TREE_CHUNK_ITEMS: usize = 128;
/// The inner nodes start with NUL, which canonical JSON (the bytes of a leaf) never does.
const TREE_LIST: &[u8] = b"\0sheltersim-tree-list\0";
const TREE_OBJECT: &[u8] = b"\0sheltersim-tree-object\0";
/// The namespaces of `model_state` whose lists the tree splits into chunks, and those lists
/// (`_WORLD_TREE`); every other value is a leaf.
const TREE_NAMESPACES: [(&str, &[&str]); 4] = [
    ("body_state_v1", &["history"]),
    ("shared_operations_v1", &["history"]),
    ("resource_ledger_v1", &["audit"]),
    ("resource_service_v1", &["request_history", "queue_stall_history"]),
];

/// `world.state_digest()`: the hash tree of the world's canonical document, with its `rng_state`
/// (`CanonicalEncoder.world_tree_digest`, the `state_hash` of a Checkpoint v2). The bytes are
/// streamed into the hashers: the log a record at a time, `model_state` a namespace at a time, so
/// no copy of the whole document is made (#252: the copy of a day-7 world took 1.59 GiB).
pub fn state_digest(world: &World, rng_state: &V) -> Result<String> {
    let log = log_node(world)?;
    tree_digest(world, rng_state, log)
}

/// `state_digest`, with the log's node given (`ListNode` of its records): for a reader whose host
/// holds records the core's log no longer keeps.
pub fn tree_digest(world: &World, rng_state: &V, log: String) -> Result<String> {
    let mut log = Some(log);
    let mut nodes: Vec<(&str, String)> = Vec::with_capacity(20);
    for (name, raw) in parts_with(world, false, false)? {
        let node = match name {
            "event_log" => log.take().unwrap_or_default(),
            "model_state" => model_state_node(world)?,
            _ => sha256_hex(&raw),
        };
        nodes.push((name, node));
    }
    nodes.push(("rng_state", sha256_hex(&rng_state.to_json()?)));
    Ok(object_node(nodes))
}

/// An object's node: the hash of its fields' hashes, as a canonical JSON object.
fn object_node(mut nodes: Vec<(&str, String)>) -> String {
    nodes.sort_by(|first, second| first.0.as_bytes().cmp(second.0.as_bytes()));
    let mut encoder = Encoder::with_capacity(TREE_OBJECT.len() + 80 * nodes.len());
    encoder.out.extend_from_slice(TREE_OBJECT);
    encoder.out.push(b'{');
    for (index, (name, node)) in nodes.iter().enumerate() {
        if index > 0 {
            encoder.out.push(b',');
        }
        encoder.string(name);
        encoder.out.push(b':');
        encoder.string(node);
    }
    encoder.out.push(b'}');
    sha256_hex(&encoder.out)
}

/// A list's node, its items given one at a time: its count and the hash of each run of
/// `TREE_CHUNK_ITEMS` items (the canonical JSON of that slice of the list).
pub struct ListNode {
    hasher: Sha256,
    in_chunk: usize,
    count: usize,
    keys: Vec<String>,
}

impl Default for ListNode {
    fn default() -> Self {
        Self::new()
    }
}

impl ListNode {
    pub fn new() -> Self {
        Self { hasher: Sha256::new(), in_chunk: 0, count: 0, keys: Vec::new() }
    }

    pub fn push(&mut self, item: &[u8]) {
        self.hasher.update(if self.in_chunk == 0 { b"[" } else { b"," });
        self.hasher.update(item);
        self.in_chunk += 1;
        self.count += 1;
        if self.in_chunk == TREE_CHUNK_ITEMS {
            self.seal();
        }
    }

    fn seal(&mut self) {
        let mut hasher = std::mem::take(&mut self.hasher);
        hasher.update(b"]");
        self.keys.push(hex(&hasher.finalize()));
        self.in_chunk = 0;
    }

    pub fn node(mut self) -> String {
        if self.in_chunk > 0 {
            self.seal();
        }
        let mut encoder = Encoder::with_capacity(TREE_LIST.len() + 70 * self.keys.len() + 24);
        encoder.out.extend_from_slice(TREE_LIST);
        encoder.out.push(b'[');
        encoder.int(self.count as i64);
        encoder.out.extend_from_slice(b",[");
        for (index, key) in self.keys.iter().enumerate() {
            if index > 0 {
                encoder.out.push(b',');
            }
            encoder.string(key);
        }
        encoder.out.extend_from_slice(b"]]");
        sha256_hex(&encoder.out)
    }
}

fn log_node(world: &World) -> Result<String> {
    let log = world.log()?;
    let mut chunks = ListNode::new();
    let mut record = Vec::with_capacity(4096);
    for index in 0..log.len() {
        record.clear();
        if !log.write_record(index, &mut record)? {
            return Err(Error::unsupported("a record without its bytes"));
        }
        chunks.push(&record);
    }
    Ok(chunks.node())
}

fn model_state_node(world: &World) -> Result<String> {
    let mut nodes: Vec<(&str, String)> = Vec::new();
    for name in namespace_names(world)? {
        let raw = slot_json(world, name)?;
        let node = match TREE_NAMESPACES.iter().find(|(namespace, _)| *namespace == &**name) {
            Some((_, lists)) => namespace_node(&raw, lists)?,
            None => sha256_hex(&raw),
        };
        nodes.push((&**name, node));
    }
    Ok(object_node(nodes))
}

/// The node of a namespace whose `lists` are chunked: an object of its fields' hashes, where it is
/// a dict, else a leaf.
fn namespace_node(raw: &[u8], lists: &[&str]) -> Result<String> {
    if raw.first() != Some(&b'{') {
        return Ok(sha256_hex(raw));
    }
    let members = json_members(raw)?;
    let mut nodes: Vec<(&str, String)> = Vec::with_capacity(members.len());
    for (key, span) in &members {
        let value = &raw[span.clone()];
        let node = if lists.contains(&key.as_str()) && value.first() == Some(&b'[') {
            let mut chunks = ListNode::new();
            for item in json_items(value)? {
                chunks.push(&value[item]);
            }
            chunks.node()
        } else {
            sha256_hex(value)
        };
        nodes.push((key.as_str(), node));
    }
    Ok(object_node(nodes))
}

// ---- Spans of compact JSON -------------------------------------------------------------------------

fn not_json() -> Error {
    Error::Runtime("canonical JSON that does not split".to_owned())
}

/// Where the string starting at `start` ends: past its closing quote.
fn string_end(raw: &[u8], start: usize) -> Result<usize> {
    let mut at = start + 1;
    while at < raw.len() {
        match raw[at] {
            b'\\' => at += 2,
            b'"' => return Ok(at + 1),
            _ => at += 1,
        }
    }
    Err(not_json())
}

/// Where the value starting at `start` ends.
fn value_end(raw: &[u8], start: usize) -> Result<usize> {
    let mut depth = 0_usize;
    let mut at = start;
    loop {
        match *raw.get(at).ok_or_else(not_json)? {
            b'"' => {
                at = string_end(raw, at)?;
                if depth == 0 {
                    return Ok(at);
                }
                continue;
            }
            b'{' | b'[' => depth += 1,
            b'}' | b']' => {
                depth = depth.checked_sub(1).ok_or_else(not_json)?;
                if depth == 0 {
                    return Ok(at + 1);
                }
            }
            _ if depth == 0 => {
                while at < raw.len() && !matches!(raw[at], b',' | b'}' | b']') {
                    at += 1;
                }
                return Ok(at);
            }
            _ => {}
        }
        at += 1;
    }
}

/// The keys of a compact JSON object and where each value is.
fn json_members(raw: &[u8]) -> Result<Vec<(String, std::ops::Range<usize>)>> {
    let mut members = Vec::new();
    if raw.get(1) == Some(&b'}') {
        return Ok(members);
    }
    let mut at = 1;
    loop {
        let key_end = string_end(raw, at)?;
        let key: String = serde_json::from_slice(&raw[at..key_end]).map_err(|_| not_json())?;
        if raw.get(key_end) != Some(&b':') {
            return Err(not_json());
        }
        let end = value_end(raw, key_end + 1)?;
        members.push((key, key_end + 1..end));
        match raw.get(end) {
            Some(b',') => at = end + 1,
            Some(b'}') => return Ok(members),
            _ => return Err(not_json()),
        }
    }
}

/// Where each item of a compact JSON list is.
fn json_items(raw: &[u8]) -> Result<Vec<std::ops::Range<usize>>> {
    let mut items = Vec::new();
    if raw.get(1) == Some(&b']') {
        return Ok(items);
    }
    let mut at = 1;
    loop {
        let end = value_end(raw, at)?;
        items.push(at..end);
        match raw.get(end) {
            Some(b',') => at = end + 1,
            Some(b']') => return Ok(items),
            _ => return Err(not_json()),
        }
    }
}
