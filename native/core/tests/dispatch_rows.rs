//! The dispatch phase (`rules::dispatch::run`, the kernel of `_run_dispatch_phase`) and the travel
//! completion's handler alone, on the worlds of `integration/test_dispatch_rows.py`, without Python
//! (Issue #225, lane M10b).
//!
//! `data/dispatch_rows.json` (`tests/support/core_fixtures_reads.py`) holds the Scenario of 100
//! people at minute 0 with its controller's table of handlers, and the worlds that file arranged
//! before the phase: an event of a type no handler takes, a social action (its people together,
//! apart, or with a reason the rule does not take), and a walk home or to a toilet scheduled by the
//! controller, at the walk's due minute. After the phase, everything the world saved (its records,
//! its pending events, its ID counters and its canonical JSON) is what the deleted Python step saved
//! from the same events: the digest that file took of `json.dumps(..., sort_keys=True)` of them
//! (45a9bc9), written again here with `Dumps`. The cases of the handler alone (`support/cases.rs`)
//! are a walk home spoiled, and one named for another purpose, with the deleted Python rule's texts.

mod support;

use std::rc::Rc;
use std::sync::Arc;

use serde_json::Value;
use sheltersim_core::checkpoint::sha256_hex;
use sheltersim_core::events::{EventRow, EventStatus};
use sheltersim_core::row::ValueRow;
use sheltersim_core::rules::common_operations::Logged;
use sheltersim_core::rules::dispatch;
use sheltersim_core::rules::tick::{Seed, Tick};
use sheltersim_core::value::V;
use sheltersim_core::world::{World, WorldCell, ID_COUNTERS};

use support::cases::{self, check_group};
use support::dumps::Dumps;
use support::load::{self, Loaded};
use support::{canonical, Call, TestHost};

const FILE: &str = "dispatch_rows";

fn text(value: &str) -> V {
    V::Str(Arc::from(value))
}

fn map(entries: &[(&str, V)]) -> V {
    V::Map(entries.iter().map(|(key, value)| (Arc::from(*key), value.clone())).collect())
}

/// `ScheduledEvent.to_dict()`.
fn to_dict(row: &EventRow) -> V {
    let optional = |value: &Option<Arc<str>>| value.as_ref().map_or(V::Null, |id| V::Str(id.clone()));
    map(&[
        ("event_id", V::Str(row.event_id.clone())),
        ("kind", text(row.kind.as_str())),
        ("due_minute", V::Int(row.due_minute)),
        ("priority", V::Int(row.priority)),
        ("sequence", V::Int(row.sequence)),
        ("event_type", V::Str(row.event_type.clone())),
        ("payload", row.payload.clone()),
        ("source_id", optional(&row.source_id)),
        ("causation_id", optional(&row.causation_id)),
        ("provenance", row.provenance.clone()),
    ])
}

/// `_saved`: the records' bytes, the pending events, the ID counters and the canonical JSON; the
/// first 16 hex of the sha256 of `json.dumps` of them with `sort_keys=True` (`_is_python_s`).
fn saved(entry: &Value, world: &World) -> String {
    let log = world.log().unwrap();
    let records: Vec<V> = (0..log.len()).map(|index| text(std::str::from_utf8(log.written(index).unwrap()).unwrap())).collect();
    drop(log);
    let queue = world.queue().unwrap();
    let pending: Vec<V> = queue.ordered().iter().map(|(_, serial)| to_dict(queue.slot(*serial).row.as_ref().expect("a row"))).collect();
    drop(queue);
    let counters = world.row::<ValueRow>(ID_COUNTERS).unwrap().0.clone();
    let canonical = String::from_utf8(canonical::canonical_json(world, &entry["world"]["rng_state"])).unwrap();
    let document = map(&[("records", V::List(records)), ("pending", V::List(pending)), ("counters", counters), ("world", text(&canonical))]);
    sha256_hex(Dumps::default().sorted().text(&document).as_bytes())[..16].to_owned()
}

/// The host of a phase on a loaded world: what the PyO3 crate's host does to the Python world
/// (`following_host`), and `find_record` finding the world's records.
fn host_of(entry: &Value, loaded: &Rc<Loaded>) -> TestHost {
    let mut host = load::following_host(entry, loaded);
    for record in entry["world"]["event_log"].as_array().unwrap() {
        let event = load::event(&record["event"]);
        let status = EventStatus::parse(record["status"].as_str().unwrap()).unwrap();
        let logged = Logged { status, realized_minute: record["realized_minute"].as_i64().unwrap(), event };
        host.records.entry(logged.event.event_id.clone()).or_insert(logged);
    }
    host
}

/// The phase run on a world of the file at its minute (`_dispatched`): the world loaded and the
/// host's calls; what it saved is the Python step's (`python_saved`).
fn dispatched(name: &str, python_saved: &str) -> (Rc<Loaded>, Vec<Call>) {
    let entry = cases::entry(FILE, name);
    let loaded = Rc::new(load::loaded(&entry));
    let mut host = host_of(&entry, &loaded);
    let minute = loaded.cell.with(|world| world.minute_index().unwrap()).unwrap();
    let mut tick = Tick {
        cell: &loaded.cell,
        host: &mut host,
        run: &loaded.run,
        policies: loaded.policies.clone(),
        minute,
        seed: Seed::Other,
        neighbors: loaded.neighbors.clone(),
    };
    dispatch::run(&mut tick).unwrap_or_else(|error| panic!("{name}: {error:?}"));
    assert_eq!(loaded.cell.with(|world| saved(&entry, world)).unwrap(), python_saved, "{name}");
    let calls = std::mem::take(&mut host.calls);
    drop(host);
    (loaded, calls)
}

/// The record of an event, and its status and detail.
fn record(loaded: &Loaded, event_id: &str) -> (String, Option<String>) {
    loaded
        .cell
        .with(|world| {
            let log = world.log().unwrap();
            let found = (0..log.len()).map(|index| serde_json::from_slice::<Value>(log.written(index).unwrap()).unwrap()).find(|record| record["event"]["event_id"] == event_id);
            let found = found.unwrap_or_else(|| panic!("no record of {event_id}"));
            (found["status"].as_str().unwrap().to_owned(), found["detail"].as_str().map(str::to_owned))
        })
        .unwrap()
}

fn foreign(calls: &[Call]) -> usize {
    calls.iter().filter(|call| matches!(call, Call::Foreign(_))).count()
}

#[test]
fn an_event_type_without_a_handler_fails_with_python_s_text() {
    let (loaded, _) = dispatched("no_handler", "3363a241c396b9b0");
    assert_eq!(record(&loaded, "x"), ("failed".to_owned(), Some("no handler registered for event type 'probe'".to_owned())));
}

#[test]
fn a_social_action_runs_in_the_core_with_python_s_results() {
    let (loaded, calls) = dispatched("social", "db670ea5268dbb27");
    assert_eq!(foreign(&calls), 0);
    assert_eq!(record(&loaded, "social-901"), ("applied".to_owned(), None));
}

#[test]
fn a_suppressed_social_action_has_python_s_detail_in_its_record() {
    let (loaded, calls) = dispatched("social_apart", "f1b5249381ba1d0c");
    assert_eq!(foreign(&calls), 0);
    let detail = "social action participants are no longer co-located";
    assert_eq!(record(&loaded, "social-901"), ("suppressed".to_owned(), Some(detail.to_owned())));
}

#[test]
fn a_failed_social_action_has_python_s_detail_from_the_kernel() {
    let (loaded, calls) = dispatched("social_other_reason", "b0d258951174919c");
    assert_eq!(foreign(&calls), 0);
    assert_eq!(record(&loaded, "social-901"), ("failed".to_owned(), Some("social action reason code is not allowed".to_owned())));
}

#[test]
fn a_walk_ends_in_the_core_with_python_s_results() {
    for (name, python_saved) in [("return_walk", "e7cc4860a63dc8ea"), ("toilet_walk", "d071ac8765838790")] {
        let (loaded, calls) = dispatched(name, python_saved);
        assert_eq!(foreign(&calls), 0, "{name}");
        let target = loaded.cell.with(|world| world.rows().unwrap().agents.get("A001").unwrap().target_id.clone()).unwrap();
        assert!(target.is_none() || name == "toilet_walk", "{name}");
    }
}

#[test]
fn a_walk_that_does_not_fit_changes_nothing_and_says_the_rule_s_text() {
    check_group(FILE, "a_walk_that_does_not_fit", 3);
}

#[test]
fn a_walk_home_named_for_another_purpose_fails_with_the_rule_s_text_before_any_change() {
    check_group(FILE, "a_walk_named_for_another_purpose", 3);
}
