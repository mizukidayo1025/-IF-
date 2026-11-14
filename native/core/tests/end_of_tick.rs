//! The end of a tick of a Run without Python (`engine::EndCheck::Whole`, #250): what
//! `WorldState.validate_after_tick` answers. The kernel's check (`after_tick::check`) asks the
//! core's validators of the namespaces (`rules::tick_checks::CoreChecks`) where the native path asks
//! the ones Python registers; where it refuses, the whole validation without the event history
//! (`validate::world`) decides, with Python's class and text.
//!
//! - Every broken world Python's validation was recorded on (`validate_*.json`,
//!   `tests/support/core_validate*.py`) ends a tick with the error Python raised for it at
//!   `validate(validate_event_history=False)`: where Python raises, the kernel's check never says
//!   yes.
//! - A namespace that only the validators guard, broken between ticks, stops the check with the
//!   error the whole validation gives, where the kernel's check with every namespace taken as valid
//!   (`NoPython`) passes.
//! - On valid worlds the check says yes at every tick (`tick_core.rs`).

mod support;

use std::sync::Arc;

use serde_json::Value;
use sheltersim_core::cell::Shard;
use sheltersim_core::engine::{self, NoPython, Stepping};
use sheltersim_core::error::Error;
use sheltersim_core::plain::PlainRun;
use sheltersim_core::row::{Row, ValueRow};
use sheltersim_core::state::body_state::BodyRow;
use sheltersim_core::state::resource_ledger::LedgerRow;
use sheltersim_core::state::resource_service::ServiceRow;
use sheltersim_core::state::social_network::SocialNetworkRow;
use sheltersim_core::validate::{self, Options};
use sheltersim_core::world::{World, WorldCell};

use support::load;
use support::validate::{load_entry, outcome_of, same_outcome, Files, Found};

/// The files of recorded cases (all but the file of valid worlds).
const CASE_FILES: [&str; 15] = [
    "validate_effects",
    "validate_entities",
    "validate_entity_rows",
    "validate_entry",
    "validate_events",
    "validate_information",
    "validate_namespaces",
    "validate_operations",
    "validate_places",
    "validate_resource_ledger",
    "validate_selection",
    "validate_service",
    "validate_shared_operations",
    "validate_signatures",
    "validate_staff_tasks",
];

/// The entry a world of a file is a variant of, if any.
fn base_of(files: &mut Files, file: &str, world: &str) -> Option<Value> {
    let found = files.data(file)["worlds"].get(world).cloned()?;
    let base = found.get("base").and_then(Value::as_str)?.to_owned();
    let base_file = found.get("file").and_then(Value::as_str).unwrap_or(file).to_owned();
    Some(files.entry(&base_file, &base))
}

#[test]
fn every_recorded_broken_world_ends_a_tick_with_python_s_error() {
    let mut files = Files::default();
    let (mut checked, mut as_python, mut refused_by_validators, mut skipped) = (0, 0, 0, 0);
    let mut failures = Vec::new();
    for file in CASE_FILES {
        let cases = files.data(file)["cases"].as_array().cloned().unwrap_or_else(|| panic!("{file} has no cases"));
        for case in &cases {
            let name = case["name"].as_str().unwrap();
            let world_name = case["world"].as_str().unwrap();
            let entry = files.entry(file, world_name);
            // The host's sections are the ones Python parsed from the base world's configs: a case
            // that changes the configs would be checked against the sections of other configs.
            if base_of(&mut files, file, world_name).is_some_and(|base| base["world"]["model_configs"] != entry["world"]["model_configs"]) {
                skipped += 1;
                continue;
            }
            let Ok(world) = load_entry(&entry) else {
                skipped += 1;
                continue;
            };
            let wanted: Found = case["expected"]["validate_tick"]
                .as_array()
                .map(|pair| (pair[0].as_str().unwrap().to_owned(), pair[1].as_str().unwrap().to_owned()));
            // What the whole validation gives on the world as loaded: Python's, but where the
            // loader cannot hold the broken value (the case's test changes the loaded world).
            let whole = outcome_of(validate::world(&world, &Options { history: false, in_flight: None }));
            // The host gives the sections of the export and the floor plan of this world.
            let mut host = load::host(&entry);
            let cell = Shard::new(world);
            let mut stepping = Stepping::whole();
            let found = outcome_of(engine::validate_after_tick(&cell, &mut host, &mut stepping));
            if found != whole {
                failures.push(format!("{file} {name}: the whole validation {whole:?}, the end of the tick {found:?}"));
            }
            if same_outcome(&wanted, &found) {
                as_python += 1;
            }
            if stepping.refused.iter().any(|(_, reason)| reason.contains("its kernel did not take it") || reason.contains("are the Python rule's")) {
                refused_by_validators += 1;
            }
            checked += 1;
        }
    }
    eprintln!("{checked} cases checked ({as_python} as Python recorded), {skipped} skipped, {refused_by_validators} refused by a namespace's validator");
    assert!(failures.is_empty(), "{} of {checked} cases differ:\n{}", failures.len(), failures.join("\n"));
    assert!(checked >= 1000 && as_python + 10 >= checked, "{checked} {as_python}");
    assert!(refused_by_validators >= 500, "{refused_by_validators}");
}

/// A PlainRun of the generated world (`tick_core.json`) after its first tick.
fn generated_after_a_tick() -> PlainRun {
    let mut run = PlainRun::of_entry(&load::entry("tick_core", "generated")).unwrap();
    run.step().unwrap_or_else(|error| panic!("{error:?}"));
    run
}

/// The end of the tick with the world broken by `edit`: it fails with the error of
/// `validate(validate_event_history=False)`, which the kernel's check with every namespace taken as
/// valid does not see. The error, and the reason the core's check refused.
fn end_of_a_broken_tick(edit: impl FnOnce(&mut World)) -> (Error, String) {
    let mut run = generated_after_a_tick();
    run.loaded.cell.with(edit).unwrap();
    let mut memo = run.stepping.memo.clone();
    engine::check_after_tick(&run.loaded.cell, &mut run.host, &mut memo, &mut NoPython::default()).expect("the kernel's own rules pass");
    let whole = run.loaded.cell.with(|world| validate::world(world, &Options { history: false, in_flight: None })).unwrap();
    let wanted = whole.expect_err("the whole validation refuses the world");
    let got = engine::validate_after_tick(&run.loaded.cell, &mut run.host, &mut run.stepping).expect_err("the end of the tick refuses the world");
    assert_eq!((validate::py_str(&got), std::mem::discriminant(&got)), (validate::py_str(&wanted), std::mem::discriminant(&wanted)));
    let reason = run.stepping.refused.last().map(|(_, reason)| reason.clone()).expect("the core's check refused");
    // A failed check drops the memo, as Python drops `_tick_memo`.
    assert_eq!(run.stepping.memo.minute, -1);
    (got, reason)
}

#[test]
fn a_broken_body_record_ends_the_tick_with_the_validation_s_error() {
    let (error, reason) = end_of_a_broken_tick(|world| {
        let mut row = world.row_mut::<BodyRow>("body_state_v1").unwrap();
        let record = row.agents.values_mut().next().unwrap();
        record.uncertainty = 1.5;
    });
    assert!(matches!(&error, Error::Invariant(text) if text == "invalid simulator extension state"), "{error:?}");
    assert_eq!(reason, "the rows of the body state are the Python rule's");
}

#[test]
fn a_broken_social_network_ends_the_tick_with_the_validation_s_error() {
    let (error, reason) = end_of_a_broken_tick(|world| {
        world.row_mut::<SocialNetworkRow>("social_network_v1").unwrap().schema_version = 2;
    });
    assert!(matches!(&error, Error::Invariant(text) if text == "invalid simulator extension state"), "{error:?}");
    assert_eq!(reason, "social_network_v1: its kernel did not take it");
}

#[test]
fn a_broken_resource_service_ends_the_tick_with_the_validation_s_error() {
    let (error, reason) = end_of_a_broken_tick(|world| {
        let mut row = world.row_mut::<ServiceRow>("resource_service_v1").unwrap();
        row.cancelled_task_ids = vec![Arc::from("task-x"), Arc::from("task-x")];
    });
    assert!(matches!(&error, Error::Invariant(text) if text == "resource_service_v1 state is invalid"), "{error:?}");
    assert_eq!(reason, "resource_service_v1: its kernel did not take it");
}

#[test]
fn a_broken_resource_ledger_ends_the_tick_with_the_validation_s_error() {
    let (error, reason) = end_of_a_broken_tick(|world| {
        world.row_mut::<LedgerRow>("resource_ledger_v1").unwrap().schema_version = 2;
    });
    assert!(matches!(&error, Error::Invariant(text) if text.starts_with("invalid resource ledger: ")), "{error:?}");
    assert_eq!(reason, "resource_ledger_v1: its kernel did not take it");
}

/// A namespace held as another row than its own (here a value) is no yes: the whole validation
/// decides, and passes a valid world.
#[test]
fn a_namespace_held_as_a_value_leaves_the_tick_to_the_whole_validation() {
    let mut run = generated_after_a_tick();
    run.loaded
        .cell
        .with(|world| {
            let mut encoder = sheltersim_core::json::Encoder { out: Vec::new() };
            world.row::<sheltersim_core::state::psychological_distress::DistressRow>("psychological_distress_v1").unwrap().write_json(&mut encoder).unwrap();
            let value = load::v(&serde_json::from_slice(&encoder.out).unwrap());
            world.slots.get("psychological_distress_v1").unwrap().try_borrow_mut().unwrap().row = Some(Box::new(ValueRow(value)));
        })
        .unwrap();
    engine::validate_after_tick(&run.loaded.cell, &mut run.host, &mut run.stepping).unwrap_or_else(|error| panic!("{error:?}"));
    assert_eq!(run.stepping.refused.len(), 1, "{:?}", run.stepping.refused);
    assert_eq!(run.stepping.refused[0].1, "the namespace has another row");
}

