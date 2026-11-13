//! The world of a Scenario at minute 0, built in the core (Issue #249): `initial::entry` of a
//! generated Scenario's canonical JSON is the entry Python exports of its world
//! (`export_world`, the sha256 of its bytes), and the world it loads runs to the native path's
//! world (the digests of its parts and of the metric samples after a day of ticks).
//!
//! `initial_worlds.json` holds the Scenarios and what Python gave (`tests/support/core_initial.py`,
//! written by `benchmarks/core_fixtures.py`, checked against Python by
//! `tests/integration/test_core_fixtures.py`); `tests/unit/core/test_world_export.py` compares the
//! bytes for the Scenario screen's requests through the PyO3 crate.

mod support;

use std::rc::Rc;

use serde_json::Value;
use sheltersim_core::canonical;
use sheltersim_core::checkpoint::sha256_hex;
use sheltersim_core::error::Error;
use sheltersim_core::initial;
use sheltersim_core::world::WorldCell;

use support::load::{self, Loaded};
use support::ticks::{metric_line, run as run_ticks};
use support::{Call, TestHost};

const WORLDS: [&str; 2] = ["twenty", "hundred"];

fn world_case(name: &str) -> Value {
    load::data("initial_worlds")["initial_worlds"][name].clone()
}

fn digests(loaded: &Loaded) -> Vec<(&'static str, String)> {
    loaded.cell.with(|world| canonical::run_digests(world)).unwrap().unwrap()
}

fn expected(document: &Value) -> Vec<(&'static str, String)> {
    const NAMES: [&str; 15] = [
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
        "scalars",
    ];
    NAMES.iter().map(|name| (*name, document[*name].as_str().unwrap().to_owned())).collect()
}

fn metrics_digest(host: &TestHost) -> (usize, String) {
    let lines: Vec<Vec<u8>> = host
        .calls
        .iter()
        .filter_map(|call| match call {
            Call::Metric(sample, minute) => Some(metric_line(sample, *minute)),
            _ => None,
        })
        .collect();
    (lines.len(), sha256_hex(&lines.join(&b'\n')))
}

#[test]
fn the_core_builds_the_entry_python_exports_and_runs_it_to_the_native_path_s_world() {
    for name in WORLDS {
        let case = world_case(name);
        let wanted = &case["expected"];
        let made = initial::entry(case["scenario"].as_str().unwrap().as_bytes()).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(made.len() as u64, wanted["entry_bytes"].as_u64().unwrap(), "{name}");
        assert_eq!(sha256_hex(&made), wanted["entry_sha256"].as_str().unwrap(), "{name}");

        let entry: Value = serde_json::from_slice(&made).unwrap();
        let loaded = Rc::new(load::loaded(&entry));
        assert_eq!(digests(&loaded), expected(&wanted["initial"]), "{name}");
        let mut host = load::following_host(&entry, &loaded);
        run_ticks(&loaded, &mut host, wanted["ticks"].as_i64().unwrap(), &mut |_, _| {});
        assert_eq!(digests(&loaded), expected(&wanted["world"]), "{name}");
        let (count, metrics) = metrics_digest(&host);
        assert_eq!(count as u64, wanted["metric_count"].as_u64().unwrap(), "{name}");
        assert_eq!(metrics, wanted["metrics"].as_str().unwrap(), "{name}");
    }
}

#[test]
fn the_core_refuses_configs_its_table_lacks() {
    let case = world_case("twenty");
    let mut scenario: Value = serde_json::from_str(case["scenario"].as_str().unwrap()).unwrap();
    let configs = scenario["model_configs"].as_object().unwrap().keys().cloned().collect::<Vec<_>>();
    assert!(initial::knows(&scenario["model_configs"]).unwrap());
    scenario["model_configs"]["behavior"]["temperature"] = serde_json::json!(0.3);
    let raw = serde_json::to_vec(&scenario).unwrap();
    match initial::entry(&raw) {
        Err(Error::Unsupported(why)) => assert!(why.contains("fingerprint"), "{why}"),
        other => panic!("{:?}", other.map(|bytes| bytes.len())),
    }
    // A data config's contents are not in the fingerprint; that it is there is.
    let mut moved: Value = serde_json::from_str(case["scenario"].as_str().unwrap()).unwrap();
    moved["model_configs"]["population_context"]["evacuation_selection_source_ids"] = serde_json::json!(["SRC-X"]);
    assert!(initial::knows(&moved["model_configs"]).unwrap());
    moved["model_configs"].as_object_mut().unwrap().remove("population_context");
    assert!(!initial::knows(&moved["model_configs"]).unwrap());
    assert!(configs.contains(&"facility_geometry".to_owned()));
}

#[test]
fn what_is_not_a_scenario_is_an_error_not_a_panic() {
    let case = world_case("twenty");
    let text = case["scenario"].as_str().unwrap();
    let mut broken: Vec<Vec<u8>> =
        vec![b"".to_vec(), b"[]".to_vec(), b"{}".to_vec(), b"null".to_vec(), b"\xff".to_vec(), b"{\"seed\": \"1\"}".to_vec()];
    // The Scenario cut short at many places, and each top-level member made a wrong type.
    for end in (0..text.len()).step_by(997) {
        if text.is_char_boundary(end) {
            broken.push(text.as_bytes()[..end].to_vec());
        }
    }
    let scenario: Value = serde_json::from_str(text).unwrap();
    for key in scenario.as_object().unwrap().keys() {
        for wrong in [serde_json::json!(null), serde_json::json!("x"), serde_json::json!([1]), serde_json::json!({"a": 1})] {
            let mut changed = scenario.clone();
            changed[key.as_str()] = wrong;
            broken.push(serde_json::to_vec(&changed).unwrap());
        }
    }
    for raw in broken {
        let _ = initial::entry(&raw);
    }
}
