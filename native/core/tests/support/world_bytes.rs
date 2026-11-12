//! The whole world as bytes, and a digest of a test's answers over it (Issue #225, lane M3).
//!
//! A test that records what a rule answered and left (a pytest's `repr` digest, recorded again from
//! the core: decision 1 of #225) hashes `world_bytes` after each call: the canonical JSON of each part
//! of the world (`tests/tick_core.rs` digests the same parts one by one), the entity tables in the
//! order of their IDs, the queue's events in their order, the log's records as written, and each
//! slot's row. `Answers` chains them, one line a call, as the pytests chained theirs.

use std::sync::Arc;

use sheltersim_core::checkpoint::sha256_hex;
use sheltersim_core::entities::{EntityRow, Table};
use sheltersim_core::json::Encoder;
use sheltersim_core::world::{Place, World};

fn table<R: EntityRow>(encoder: &mut Encoder, table: &Table<R>, version: i64) {
    let mut rows: Vec<&R> = table.iter().map(|(_, row)| row).collect();
    rows.sort_by(|first, second| first.id().as_bytes().cmp(second.id().as_bytes()));
    encoder.out.push(b'[');
    for (index, row) in rows.into_iter().enumerate() {
        if index > 0 {
            encoder.out.push(b',');
        }
        row.write_json(encoder, version).unwrap();
    }
    encoder.out.push(b']');
}

fn slot(encoder: &mut Encoder, world: &World, name: &str) {
    let slot = world.slots.get(name).unwrap().try_borrow().unwrap();
    match slot.row.as_deref() {
        Some(row) => row.write_json(encoder).unwrap(),
        None => encoder.out.extend_from_slice(b"null"),
    }
}

/// The world as bytes: each part's name and canonical JSON, a line each.
pub fn world_bytes(world: &World) -> Vec<u8> {
    let version = world.schema_version().unwrap();
    let mut encoder = Encoder::with_capacity(1 << 16);
    let part = |encoder: &mut Encoder, name: &str| {
        if !encoder.out.is_empty() {
            encoder.out.push(b'\n');
        }
        encoder.out.extend_from_slice(name.as_bytes());
        encoder.out.push(b' ');
    };
    {
        let rows = world.rows().unwrap();
        part(&mut encoder, "agents");
        table(&mut encoder, &rows.agents, version);
        part(&mut encoder, "zones");
        table(&mut encoder, &rows.zones, version);
        part(&mut encoder, "zone_edges");
        table(&mut encoder, &rows.zone_edges, version);
        part(&mut encoder, "facilities");
        table(&mut encoder, &rows.facilities, version);
        part(&mut encoder, "staff");
        table(&mut encoder, &rows.staff, version);
        part(&mut encoder, "support_requests");
        table(&mut encoder, &rows.support_requests, version);
        part(&mut encoder, "active_tasks");
        table(&mut encoder, &rows.active_tasks, version);
    }
    part(&mut encoder, "event_queue");
    {
        let queue = world.queue().unwrap();
        encoder.out.push(b'[');
        for (index, (_, serial)) in queue.ordered().iter().enumerate() {
            if index > 0 {
                encoder.out.push(b',');
            }
            queue.slot(*serial).row.as_ref().expect("an event with its row").write_json(&mut encoder).unwrap();
        }
        encoder.out.push(b']');
    }
    part(&mut encoder, "event_log");
    {
        let log = world.log().unwrap();
        encoder.out.push(b'[');
        for index in 0..log.len() {
            if index > 0 {
                encoder.out.push(b',');
            }
            encoder.out.extend_from_slice(log.written(index).expect("a record's bytes"));
        }
        encoder.out.push(b']');
    }
    let mut names: Vec<&Arc<str>> = world.slots.names().collect();
    names.sort_by(|first, second| first.as_bytes().cmp(second.as_bytes()));
    for name in names {
        let place = world.slots.get(name).unwrap().try_borrow().unwrap().place;
        part(&mut encoder, if place == Place::Field { "field" } else { "model_state" });
        encoder.string(name);
        encoder.out.push(b' ');
        slot(&mut encoder, world, name);
    }
    let scalars = world.scalars().unwrap();
    part(&mut encoder, "scalars");
    encoder.out.push(b'[');
    encoder.int(scalars.schema_version().unwrap());
    encoder.out.push(b',');
    encoder.string(scalars.scenario_id().unwrap());
    encoder.out.push(b',');
    encoder.out.extend_from_slice(scalars.seed().unwrap().text.as_bytes());
    encoder.out.push(b',');
    encoder.int(scalars.duration_minutes().unwrap());
    encoder.out.push(b',');
    encoder.int(scalars.minute_index().unwrap());
    encoder.out.push(b']');
    encoder.out
}

/// A test's answers, chained in the order of its calls: what each call returned (as text) and the
/// world after it.
#[derive(Default)]
pub struct Answers {
    raw: Vec<u8>,
}

impl Answers {
    pub fn add(&mut self, result: &str, world: &World) {
        self.raw.extend_from_slice(result.as_bytes());
        self.raw.push(b'\n');
        self.raw.extend_from_slice(&world_bytes(world));
        self.raw.push(b'\n');
    }

    /// An answer that leaves no world behind (a read).
    pub fn text(&mut self, result: &str) {
        self.raw.extend_from_slice(result.as_bytes());
        self.raw.push(b'\n');
    }

    /// The sha256 of the answers so far.
    pub fn hex(&self) -> String {
        sha256_hex(&self.raw)
    }
}
