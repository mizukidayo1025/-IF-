//! An EffectProgram marker's guard in the dispatch phase: the Python dispatcher's outcome (Issue
//! #225, lane M7; moved from `tests/integration/test_effect_guard_texts.py`).
//!
//! `EventDispatcher.dispatch_one` asked the guard (`should_apply_effect_guard`) before an event's
//! handler; the core's loop (`rules::dispatch::run`, the dispatch phase `_run_dispatch_phase`
//! runs) asks it for every handler (lane P9-1c): a false guard is a suppressed event, a guard the
//! contract turns down or with no target a failed one, each with the Python rule's text (saved as
//! the record's `detail`); a guard that holds goes on to the handler.
//!
//! The world is the gate's generated Scenario at minute 0 with its controller
//! (`resource_service_texts.json`'s `gate`, `tests/support/core_fixtures_kernels.py`), with a marked
//! event scheduled as `WorldState.schedule_event` schedules it, or a guarded program scheduled by
//! `schedule_effect_program` in Python (`gate_<name>`). The contract's text of a guard it turns down
//! is pydantic's, as the fixture recorded it (`support/contracts.rs`). Each case runs the phase at
//! minute 0 with the host of a Run's world, and compares all that was saved with what the Python
//! step saved from it: the first 16 hex of the sha256 of `json.dumps({"records", "pending",
//! "counters", "world"}, sort_keys=True)` (the log's records as text, each pending event's
//! `to_dict()`, `id_counters`, the world's canonical text), a digest taken once from the Python step
//! (45a9bc9) that `Dumps` writes again, so it stays. The record's status and detail are the outcome
//! the guard's Python rule gave, and the dispatch's counts moved as the pytest file checked.

mod support;

use std::rc::Rc;
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use serde_json::{json, Value};
use sheltersim_core::events::EventStatus;
use sheltersim_core::rules::dispatch::{self, COUNTS};
use sheltersim_core::rules::{effects, kit};
use sheltersim_core::value::V;
use sheltersim_core::world::WorldCell;

use support::canonical::{canonical_json, parts};
use support::dumps::Dumps;
use support::load::{self, Loaded};
use support::texts::{big, sha};
use support::ticks::tick_at;
use support::{cases, contracts, TestHost};

const FILE: &str = "resource_service_texts";
const KEY: &str = "effect_program_v1";
const EVALUATED_FALSE: &str = "EffectProgram guard evaluated false";

/// The dispatch's counts are the process's: one case at a time.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// The counts of the dispatch, with the guard's refusals (`effects::COUNTS`) as `guard_refused`.
fn counts() -> IndexMap<String, u64> {
    let mut counts: IndexMap<String, u64> = COUNTS.all().into_iter().map(|(name, times)| (name.to_owned(), times)).collect();
    let refused = effects::COUNTS.refusal_counts().into_iter().filter(|(reason, _)| reason.starts_with("dispatch_guard:")).map(|(_, times)| times).sum();
    counts.insert("guard_refused".to_owned(), refused);
    counts
}

/// What a case's event is: one marked by hand, or the start of the program a world scheduled.
enum Scheduled {
    Marked(Value),
    Program(&'static str),
}

/// `_marked`: an event whose marker has the guard, of no program.
fn marked(event_type: &str, payload: Value, guard: Value) -> Scheduled {
    Scheduled::Marked(json!({
        "event_id": "guarded-1",
        "kind": "intervention",
        "due_minute": 0,
        "priority": 0,
        "sequence": 900_001,
        "event_type": event_type,
        "payload": payload,
        "source_id": "TEST-GUARD",
        "causation_id": null,
        "provenance": {KEY: {"guard": guard, "program_id": "no-such-program"}},
    }))
}

/// What a case gave: the record of its event and the counts before and after.
struct Dispatched {
    status: EventStatus,
    detail: Option<String>,
    before: IndexMap<String, u64>,
    after: IndexMap<String, u64>,
}

impl Dispatched {
    fn grew(&self, names: &[&str]) {
        for name in names {
            assert_eq!(self.after[*name], self.before[*name] + 1, "{name}");
        }
    }

    fn same(&self, name: &str) {
        assert_eq!(self.after[name], self.before[name], "{name}");
    }
}

/// `_saved`: what the step saved, as the pytest file wrote it.
fn saved(loaded: &Loaded, entry: &Value) -> V {
    let text = |value: &[u8]| V::Str(Arc::from(std::str::from_utf8(value).unwrap()));
    let read = |raw: &[u8]| load::v(&serde_json::from_slice(raw).unwrap());
    loaded
        .cell
        .with(|world| {
            let log = world.log().unwrap();
            let records: Vec<V> = (0..log.len()).map(|index| text(log.written(index).unwrap())).collect();
            drop(log);
            let all: IndexMap<&str, Vec<u8>> = parts(world).into_iter().collect();
            let pending = match read(&all["event_queue"]) {
                V::List(items) => items,
                other => panic!("the queue: {other:?}"),
            };
            V::Map(IndexMap::from([
                (Arc::from("records"), V::List(records)),
                (Arc::from("pending"), V::List(pending)),
                (Arc::from("counters"), read(&all["id_counters"])),
                (Arc::from("world"), text(&canonical_json(world, &entry["world"]["rng_state"]))),
            ]))
        })
        .unwrap()
}

/// `_dispatched`: the event's record from the dispatch phase at minute 0, which saves what the
/// Python step saved (`python_saved`).
fn dispatched(scheduled: Scheduled, python_saved: &str) -> Dispatched {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let world = match &scheduled {
        Scheduled::Marked(_) => "gate".to_owned(),
        Scheduled::Program(name) => format!("gate_{name}"),
    };
    let entry = cases::entry(FILE, &world);
    let loaded = Rc::new(load::loaded(&entry));
    let mut host: TestHost = load::following_host(&entry, &loaded);
    host.contracts = Some(contracts::recorded(&load::data(FILE)["worlds"]["gate"]["contracts"]));
    let event_id = match &scheduled {
        Scheduled::Marked(event) => {
            let row = load::event(event);
            loaded.cell.with(|world| kit::Events::of(world).unwrap().schedule(row).unwrap()).unwrap();
            event["event_id"].as_str().unwrap().to_owned()
        }
        Scheduled::Program(_) => {
            let queue = entry["world"]["event_queue"].as_array().unwrap();
            let start = queue.iter().find(|event| event["event_type"] == "effect_program_apply").unwrap();
            start["event_id"].as_str().unwrap().to_owned()
        }
    };
    let before = counts();
    dispatch::run(&mut tick_at(&loaded, &mut host, 0)).unwrap_or_else(|error| panic!("{world}: {error:?}"));
    let after = counts();
    let text = Dumps::default().sorted().text(&saved(&loaded, &entry));
    assert_eq!(&sha(&text)[..16], python_saved, "{world}");
    let record = loaded
        .cell
        .with(|world| {
            let log = world.log().unwrap();
            let index = log.index_of(&event_id).unwrap_or_else(|| panic!("no record of {event_id}"));
            let (_, _, status, detail) = sheltersim_core::events::EventRow::from_saved_record(log.written(index).unwrap()).unwrap();
            (status, detail.map(|detail| detail.to_string()))
        })
        .unwrap();
    Dispatched { status: record.0, detail: record.1, before, after }
}

fn facility() -> Value {
    json!({"facility_id": "toilet_01", "working_units": 0})
}

#[test]
fn a_false_guard_suppresses_the_event() {
    let guard = json!({"operator": "lt", "field": "simulation.minute", "value": 0});
    let found = dispatched(marked("effect_program_apply", facility(), guard), "4e2ff7aa6f0fc988");
    assert_eq!((found.status, found.detail.as_deref()), (EventStatus::Suppressed, Some(EVALUATED_FALSE)));
    found.grew(&["guard_stopped"]);
    found.same("events_foreign");
}

#[test]
fn a_guard_the_contract_turns_down_fails_with_its_text() {
    let cases = [
        (json!({"operator": "eq"}), "85aca6254326f09a"),
        (json!({"operator": "eq", "field": "no.such.field", "value": 1}), "5e0f37e2c43462d3"),
        (json!({"operator": "gt", "field": "simulation.minute", "value": "x"}), "27570f25deea21d6"),
        (json!({"operator": "and", "operands": [{"operator": "eq", "field": "simulation.minute", "value": 0}]}), "e7283102ac28014d"),
        (json!({"operator": "eq", "field": "simulation.minute", "value": 0, "other": 1}), "dc0e06448ea5b32f"),
    ];
    for (guard, python_saved) in cases {
        let found = dispatched(marked("effect_program_apply", facility(), guard.clone()), python_saved);
        assert_eq!(found.status, EventStatus::Failed, "{guard}");
        assert!(found.detail.as_deref().unwrap().starts_with("EffectProgram guard is invalid: "), "{guard}");
        found.grew(&["guard_stopped", "outcomes_failed"]);
    }
}

#[test]
fn a_guard_without_its_target_fails_with_python_s_text() {
    let cases = [
        ("target.capacity", json!({}), "guard field 'target.capacity' has no facility target", "0c5b9a97be5e1e4b"),
        ("target.working_units", json!({"facility_id": "no-such-facility"}), "guard field 'target.working_units' has no facility target", "8ae7aec5b1626e62"),
        ("target.queue_length", json!({"facility_id": ""}), "guard field 'target.queue_length' has no facility target", "6f9d918df8bceef5"),
        ("zone.occupancy", json!({}), "guard zone.occupancy has no known zone target", "e8e7ce315ac96ba7"),
        ("zone.occupancy", json!({"zone_id": "nowhere"}), "guard zone.occupancy has no known zone target", "149aeef2dba32364"),
        ("zone.occupancy", json!({"target_id": "nowhere"}), "guard zone.occupancy has no known zone target", "9955e50f1b9ce2ee"),
        ("resource.available_quantity", json!({"resource_id": "other"}), "guard resource.available_quantity has no ledger target", "0ee0ab9b611a9831"),
        ("resource.available_quantity", json!({}), "guard resource.available_quantity has no ledger target", "b6928ccb8daaaf5c"),
        ("staff.available", json!({"staff_id": "nobody"}), "guard staff.available has no known staff target", "9bfbcfbdfdeb96a5"),
        ("staff.available", json!({}), "guard staff.available has no known staff target", "78eb1a948feea5d7"),
        ("staff.available", json!({"staff_id": ""}), "guard staff.available has no known staff target", "02cc177610302683"),
    ];
    for (field, payload, text, python_saved) in cases {
        let guard = if field == "staff.available" {
            json!({"operator": "eq", "field": field, "value": true})
        } else {
            json!({"operator": "gte", "field": field, "value": 0})
        };
        let found = dispatched(marked("effect_program_apply", payload, guard), python_saved);
        assert_eq!((found.status, found.detail.as_deref()), (EventStatus::Failed, Some(text)), "{field}");
        found.grew(&["guard_stopped", "outcomes_failed"]);
    }
}

#[test]
fn a_later_operand_is_read_only_after_those_before_it() {
    // `and` stops at a false operand, so the missing target after it is never read.
    let first_false = json!({
        "operator": "and",
        "operands": [
            {"operator": "lt", "field": "simulation.minute", "value": 0},
            {"operator": "gte", "field": "target.capacity", "value": 0},
        ],
    });
    let found = dispatched(marked("effect_program_apply", json!({}), first_false), "899ecabfe3dd6d97");
    assert_eq!(found.detail.as_deref(), Some(EVALUATED_FALSE));
    let first_true = json!({
        "operator": "or",
        "operands": [
            {"operator": "gte", "field": "target.capacity", "value": 0},
            {"operator": "gte", "field": "simulation.minute", "value": 0},
        ],
    });
    let found = dispatched(marked("effect_program_apply", json!({}), first_true), "3a129a58d52ea5b8");
    assert_eq!(found.detail.as_deref(), Some("guard field 'target.capacity' has no facility target"));
}

#[test]
fn a_guard_that_holds_goes_on_to_the_handler() {
    let found = dispatched(Scheduled::Program("holds"), "af71b64a3089f6b2");
    assert_eq!((found.status, found.detail.as_deref()), (EventStatus::Applied, None));
    found.grew(&["guard_passed", "outcomes_applied", "events_core"]);
    found.same("events_foreign");
}

#[test]
fn a_false_guard_of_a_scheduled_program_suppresses_its_start() {
    let found = dispatched(Scheduled::Program("false"), "6a554483ede70f5b");
    assert_eq!((found.status, found.detail.as_deref()), (EventStatus::Suppressed, Some(EVALUATED_FALSE)));
    found.grew(&["guard_stopped"]);
}

#[test]
fn an_event_without_a_handler_fails_before_any_guard() {
    let guard = json!({"operator": "lt", "field": "simulation.minute", "value": 0});
    let found = dispatched(marked("no_such_event", json!({}), guard), "8a9bb286b8074ae8");
    assert_eq!((found.status, found.detail.as_deref()), (EventStatus::Failed, Some("no handler registered for event type 'no_such_event'")));
    found.grew(&["events_unregistered"]);
    found.same("guard_stopped");
}

#[test]
fn a_marked_event_of_the_table_is_guarded() {
    let guard = json!({"operator": "lt", "field": "simulation.minute", "value": 0});
    let found = dispatched(marked("support_request_timeout", json!({}), guard), "5141dc1f7e535ad5");
    assert_eq!((found.status, found.detail.as_deref()), (EventStatus::Suppressed, Some(EVALUATED_FALSE)));
    found.grew(&["guard_stopped"]);
    found.same("events_foreign");
}

#[test]
fn a_guard_value_past_64_bits_compares_by_value() {
    // An int past 64 bits is the contract's to take (StrictInt), and the core compares it as Python.
    let cases = [
        ("past_64_bits", big("1180591620717411303424"), true, "7570f31750059065"),
        ("below_64_bits", big("-1180591620717411303424"), false, "d520f89364bda8e5"),
        ("at_2_63", big("9223372036854775808"), true, "9b401386111f6f68"),
        ("below_minus_2_63", big("-9223372036854775809"), false, "1678a1db6ae904e1"),
    ];
    for (name, value, applies, python_saved) in cases {
        let found = dispatched(Scheduled::Program(name), python_saved);
        // The guard the program holds is this one.
        let entry = cases::entry(FILE, &format!("gate_{name}"));
        let start = entry["world"]["event_queue"].as_array().unwrap().iter().find(|event| event["event_type"] == "effect_program_apply").cloned().unwrap();
        assert_eq!(start["provenance"][KEY]["guard"]["value"], value, "{name}");
        if applies {
            assert_eq!((found.status, found.detail.as_deref()), (EventStatus::Applied, None), "{name}");
            found.grew(&["guard_passed", "events_core"]);
        } else {
            assert_eq!((found.status, found.detail.as_deref()), (EventStatus::Suppressed, Some(EVALUATED_FALSE)), "{name}");
            found.grew(&["guard_stopped"]);
        }
        found.same("guard_refused");
    }
}
