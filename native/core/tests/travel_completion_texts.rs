//! A walk that does not fit fails with the travel rule's own text, before any change (Issue #225,
//! lane M1; moved from `tests/integration/test_travel_completion_texts.py`).
//!
//! `AgentTravelCompletionHandler.apply` raises `EventApplicationError` when a walk's event does not
//! match its task, its person, or what its purpose needs; the dispatcher saves the text as the
//! event's `detail`. Every check comes before the rule's first write, so a failed walk changes
//! nothing. The core's handler (`dispatch::apply_alone` with the controller's handler of walks)
//! must raise each text itself, before any change (`support/cases.rs`). The worlds are the 100
//! people's Scenario and the generated Scenario of the Project screen, each with a walk scheduled
//! as the controller schedules one, then edited as each case needs (`tests/support/core_texts.py`).
//! The Python rule is gone (lane L10, `kernel_only`): each text is written down from it, in its
//! order of checks. The walks in a queue stay in the pytest file, as the test of the kernel's entry:
//! their worlds are runs of the generated Scenario until a queue walk, too large to keep here.

mod support;

use serde_json::Value;
use sheltersim_core::checkpoint::sha256_hex;
use sheltersim_core::rules::dispatch;
use sheltersim_core::rules::tick::Tick;
use sheltersim_core::world::WorldCell;

use support::canonical::canonical_json;
use support::cases::{check_group, host, resolve};
use support::load;

const FILE: &str = "travel_texts";

#[test]
fn a_walk_s_minute_person_and_destination_are_checked_in_the_rule_s_order() {
    check_group(FILE, "a_walk_s_minute_person_and_destination_are_checked_in_the_rule_s_order", 5);
}

#[test]
fn a_walk_home_that_does_not_fit_its_purpose_fails_with_the_purpose_s_text() {
    check_group(FILE, "a_walk_home_that_does_not_fit_its_purpose_fails_with_the_purpose_s_text", 7);
}

#[test]
fn a_sleep_bed_walk_without_its_journey_fails_after_its_reservation_is_read() {
    check_group(FILE, "a_sleep_bed_walk_without_its_journey_fails_after_its_reservation_is_read", 1);
}

#[test]
fn a_relocation_walk_that_does_not_match_its_pending_relocation_fails() {
    check_group(FILE, "a_relocation_walk_that_does_not_match_its_pending_relocation_fails", 3);
}

#[test]
fn a_toilet_walk_needs_its_facility_there_and_its_target() {
    check_group(FILE, "a_toilet_walk_needs_its_facility_there_and_its_target", 6);
}

/// With the controller's handler, and with one made without the resource service
/// (`AgentTravelCompletionHandler(controller.toilet_service, validate_request_state=False)`).
#[test]
fn a_resource_walk_needs_the_service_its_facility_and_its_route() {
    check_group(FILE, "a_resource_walk_needs_the_service_its_facility_and_its_route", 10);
}

#[test]
fn a_queue_walk_on_a_walk_home_fails_its_fifo_check() {
    check_group(FILE, "a_queue_walk_on_a_walk_home_fails_its_fifo_check", 4);
}

/// A walk event of another purpose, before the real one in the same minute, fails alone: the
/// core's dispatch phase saves it with its text and applies the real one.
///
/// The world's digest (16 hex digits of the sha256 of `canonical_json()`) is the pytest's,
/// `4ffcda3a4d1d53c0`, written down from the Python step (45a9bc9) before it went (lane L10); the
/// pytest `test_travel_completion_texts.py::test_a_walk_that_does_not_fit_is_saved_with_its_text_as_the_python_phase_saves_it`
/// passed with it on the same kernel in #225 lane M1, and the core writes the same bytes
/// (`support/canonical.rs`), so the value is kept.
#[test]
fn a_walk_that_does_not_fit_is_saved_with_its_text_as_the_python_phase_saves_it() {
    let data = load::data(FILE);
    let dispatched = &data["dispatched"];
    let entry = resolve(&data["worlds"], dispatched["world"].as_str().unwrap());
    let loaded = load::loaded(&entry);
    let mut host = host(&entry);
    let minute = loaded.cell.with(|world| world.minute_index().unwrap()).unwrap();
    let mut tick = Tick {
        cell: &loaded.cell,
        host: &mut host,
        run: &loaded.run,
        policies: loaded.policies.clone(),
        minute,
        seed: loaded.seed.clone(),
        neighbors: loaded.neighbors.clone(),
    };
    dispatch::run(&mut tick).unwrap();
    let (records, written) = loaded
        .cell
        .with(|world| {
            let log = world.log().unwrap();
            let records: Vec<Value> = (0..log.len())
                .map(|index| serde_json::from_slice::<Value>(log.written(index).unwrap()).unwrap())
                .filter(|record| record["event"]["event_type"] == "agent_travel_complete")
                .map(|record| serde_json::json!([record["event"]["event_id"], record["status"], record["detail"]]))
                .collect();
            drop(log);
            (records, canonical_json(world, &entry["world"]["rng_state"]))
        })
        .unwrap();
    assert_eq!(Value::Array(records), dispatched["records"]);
    assert_eq!(&sha256_hex(&written)[..16], "4ffcda3a4d1d53c0");
}
