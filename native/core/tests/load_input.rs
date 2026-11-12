//! The crate's loader (`sheltersim_core::load`, #244) on untrusted JSON: an exported world loads,
//! and a value that is not what the exporter writes is an error, never a panic, since the browser
//! feeds it a file. `find_record` reads a record back from the log's bytes, and `Sections::fill`
//! leaves the sections the core parses itself to the core.

mod support;

use serde_json::{json, Value};
use sheltersim_core::config::{Configs, Parser, Section, SectionId};
use sheltersim_core::error::Error;
use sheltersim_core::events::EventStatus;
use sheltersim_core::load::{self, FloorPlans, Sections};
use sheltersim_core::world::WorldCell;
use support::load as fixtures;

/// The entry with the value at a path of keys replaced.
fn with(entry: &Value, path: &[&str], value: Value) -> Value {
    let mut changed = entry.clone();
    let mut node = &mut changed;
    for key in path {
        node = &mut node[*key];
    }
    *node = value;
    changed
}

fn is_input_error(result: Result<impl Sized, Error>) -> bool {
    matches!(result, Err(Error::Value(text)) if text.starts_with("not an exported world"))
}

#[test]
fn an_exported_world_loads_with_its_sections_and_floor_plan() {
    let entry = fixtures::entry("sections", "generated_moved");
    let loaded = load::loaded(&entry).unwrap();
    let sections = Sections::of_entry(&entry).unwrap();
    let mut plans = FloorPlans::default();
    let overlay = loaded.cell.with(|world| load::overlay(world)).unwrap().unwrap();
    assert!(overlay.is_some());
    let first = plans.plan(&sections, overlay.as_ref()).unwrap();
    let again = plans.plan(&sections, overlay.as_ref()).unwrap();
    assert!(std::sync::Arc::ptr_eq(&first, &again), "the same overlay builds no new plan");
    assert_eq!(plans.builds(), 1);
}

#[test]
fn a_shape_the_exporter_never_writes_is_an_error() {
    let entry = fixtures::entry("sections", "generated_moved");
    // Each part of the world's document, the slots and the Run, replaced with what no part is.
    for (key, _) in entry["world"].as_object().unwrap() {
        if let Err(error) = load::loaded(&with(&entry, &["world", key], json!([[]]))) {
            assert!(matches!(error, Error::Value(_)), "{key}: {error:?}");
        }
    }
    for key in ["slots", "run"] {
        assert!(is_input_error(load::loaded(&with(&entry, &[key], json!(7)))), "{key}");
    }
    for key in ["policies", "route_capacities", "zone_neighbors"] {
        assert!(is_input_error(load::loaded(&with(&entry, &["run", key], json!("x")))), "{key}");
    }
    assert!(is_input_error(load::loaded(&with(&entry, &["world", "seed"], json!("seed")))));
    assert!(is_input_error(load::loaded(&with(&entry, &["world", "agents"], json!([{"profile": {}}])))));
    let mut twice = entry.clone();
    let records = twice["world"]["event_log"].as_array_mut().unwrap();
    records.push(records[0].clone());
    assert!(is_input_error(load::loaded(&twice)));
    // A typed namespace that does not fit its row.
    assert!(is_input_error(load::loaded(&with(&entry, &["world", "model_state", "movement_v1"], json!({"journeys": 3})))));
    assert!(is_input_error(load::event(&json!({"kind": "nothing"}))));
    assert!(is_input_error(load::world(&json!(null), &json!(null))));
    assert!(is_input_error(load::obj(&json!({"class": [null, null, true], "attrs": {"x": {"two": 1, "entries": 2}}, "class_own": {}, "own": {}}))));
}

#[test]
fn a_section_the_exporter_never_writes_is_an_error() {
    let entry = fixtures::entry("sections", "generated_moved");
    assert!(is_input_error(Sections::of_entry(&with(&entry, &["sections"], json!(null)))));
    for &which in SectionId::ALL {
        if which.parser() != Parser::Host {
            continue;
        }
        let name = which.name();
        let mut missing = entry.clone();
        missing["sections"].as_object_mut().unwrap().remove(name);
        assert!(is_input_error(Sections::of_entry(&missing)), "{name}");
        assert!(is_input_error(Sections::of_entry(&with(&entry, &["sections", name], json!({"other": 1})))), "{name}");
        if let Err(error) = Sections::of_entry(&with(&entry, &["sections", name, "value"], json!([[7]]))) {
            assert!(matches!(error, Error::Value(_)), "{name}: {error:?}");
        }
    }
}

#[test]
fn a_host_error_is_raised_where_a_rule_asks_for_the_section() {
    let entry = fixtures::entry("sections", "generated_moved");
    let raising = with(&entry, &["sections", "body_models"], json!({"host_error": "the model selection raises"}));
    let sections = Sections::of_entry(&raising).unwrap();
    let configs = Configs::new(None);
    assert!(sections.fill(&configs, SectionId::Selection).unwrap());
    let error = sections.fill(&configs, SectionId::BodyModels).unwrap_err();
    assert!(matches!(&error, Error::Runtime(text) if text.ends_with("the model selection raises")), "{error:?}");
    assert!(configs.body_models.get().is_none());
}

#[test]
fn the_core_parses_its_own_sections() {
    let sections = Sections::of_entry(&fixtures::fixture()).unwrap();
    let configs = Configs::new(None);
    for &which in SectionId::ALL {
        assert_eq!(sections.fill(&configs, which).unwrap(), which.parser() == Parser::Host, "{which:?}");
        assert_eq!(which.is_filled(&configs), which.parser() == Parser::Host, "{which:?}");
    }
    assert!(matches!(configs.facility_geometry.get(), Some(Section::Absent)));
}

#[test]
fn find_record_reads_a_record_back_from_the_log() {
    let entry = fixtures::entry("event_texts", "small");
    let loaded = load::loaded(&entry).unwrap();
    let records = entry["world"]["event_log"].as_array().unwrap();
    assert!(!records.is_empty());
    for record in records {
        let wanted = load::event(&record["event"]).unwrap();
        let found = loaded.cell.with(|world| load::find_record(world, &wanted.event_id)).unwrap().unwrap().unwrap();
        assert_eq!(found.event, wanted);
        assert_eq!(found.realized_minute, record["realized_minute"].as_i64().unwrap());
        assert_eq!(Some(found.status), EventStatus::parse(record["status"].as_str().unwrap()));
    }
    assert!(loaded.cell.with(|world| load::find_record(world, "no-such-event")).unwrap().unwrap().is_none());
}
