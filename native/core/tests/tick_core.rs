//! A Run's ticks in the core without Python (Issue #152, lane P10-1b; definition of done 4.1-(4) of
//! design-p8.md, notes for P10 in design-p9.md section 8; #214).
//!
//! The worlds are those of `tests/support/core_tick.py`, read from `tests/data/tick_core.json` by
//! the test-only loader (`support/load.rs`, orchestrator decision (b)):
//! - `small_replay`: h00_small_replay (4 people, 4 zones, 6 edges, 4 toilets, 5 staff, a toilet's
//!   outage at minute 15 and its recovery at 90), with no floor plan and no extensions.
//! - `generated`: a generated Scenario (10 people, the UI's floor plan, every extension, a second
//!   queue at a toilet so that `facility_queues_v1` exists) with an EffectProgram that moves four
//!   facilities while someone walks to them. Its `model_state` has the typed namespaces (each read
//!   into its typed row, `support/typed.rs`), and the update phase's `update_movement` walks people
//!   and replans the trips the moves changed.
//!
//! The Run's policies are what the PyO3 crate read of that controller. Each tick is the core's
//! `run_tick` with the `TestHost` (decision (c)): what the world holds from its slots, the
//! Python-parsed sections of the exported world (`support/sections.rs`), the floor plan of the world
//! as it is at each call (`load::following_host`, as `Geo::of` reads the overlay), contracts that
//! take their input as given, no foreign handler, sinks into its calls, no LLM, `space_refreshed`
//! Ok. The test does what `TickEngine.step` does around the phases: the minute moves on, then the
//! end-of-tick check (`after_tick::check`, with no Python validator to ask): `support/ticks.rs`.
//!
//! After each world's ticks the digests of the world's parts (the rows, the queue, the log, the
//! fields, `model_state`, the scalars) and of the metric samples are the ones the native path gives
//! on the same world through `TickEngine` (frozen in the fixture by `tests/support/core_tick.py`,
//! written by `benchmarks/core_fixtures.py`, and checked against the native path by
//! `tests/integration/test_core_fixtures.py`).

mod support;

use std::collections::HashMap;
use std::rc::Rc;

use serde_json::Value;
use sheltersim_core::canonical;
use sheltersim_core::checkpoint::sha256_hex;
use sheltersim_core::config::SectionId;
use sheltersim_core::engine::StepError;
use sheltersim_core::host::Phase;
use sheltersim_core::plain::PlainRun;
use sheltersim_core::row::ValueRow;
use sheltersim_core::state::movement::MovementRow;
use sheltersim_core::world::{Place, WorldCell};

use support::load::{self, Loaded};
use support::ticks::{metric_line, run as run_ticks};
use support::{Call, TestHost};

// ---- Digests of the world, as `tests/support/core_tick.py` writes them -----------------------------

/// The digests of the world's parts as `core_tick.py` writes them (`canonical::run_digests`).
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

/// The metric samples the host took, each a JSON list of the snapshot's fields in order.
fn metric_lines(host: &TestHost) -> Vec<Vec<u8>> {
    host.calls
        .iter()
        .filter_map(|call| match call {
            Call::Metric(sample, minute) => Some(metric_line(sample, *minute)),
            _ => None,
        })
        .collect()
}

fn metrics_digest(host: &TestHost) -> String {
    sha256_hex(&metric_lines(host).join(&b'\n'))
}

/// Each record of the log, as JSON.
fn records(loaded: &Loaded) -> Vec<Value> {
    loaded
        .cell
        .with(|world| {
            let log = world.log().unwrap();
            (0..log.len()).map(|index| serde_json::from_slice(log.written(index).unwrap()).unwrap()).collect()
        })
        .unwrap()
}

fn event_types(loaded: &Loaded) -> Vec<String> {
    records(loaded).iter().map(|record| record["event"]["event_type"].as_str().unwrap().to_owned()).collect()
}

// ---- The tests ---------------------------------------------------------------------------------------

/// The worlds of `tick_core.json`.
const WORLDS: [&str; 2] = ["small_replay", "generated"];

/// The detail of the record of a trip's event that a replan suppresses (`rules/replan.rs`): only
/// `replan::stop` of the update phase's `update_movement` writes it.
const REPLANNED: &str = "the route changed before travel completion";

/// What a run of a world gave besides its world and its host's calls.
struct Ran {
    loaded: Rc<Loaded>,
    host: TestHost,
    asked: Vec<String>,
    /// The ticks that ended with someone on a trip (`movement_v1`'s journeys).
    ticks_with_journeys: i64,
}

/// The world's ticks in the core, which reach what the native path gave: the log's event types, the
/// metric samples, the digests of the world's parts and of the samples. Nothing went outside the
/// core but the sinks, the observer, the contracts (which give their input back), what the host
/// reads for the world it owns (the sections, the plan) and the notices of what the core wrote (a
/// namespace started, `world.resources`).
fn reaches_the_native_path(name: &str) -> (Value, Ran) {
    let fixture = load::entry("tick_core", name);
    let wanted = &fixture["expected"];
    let ticks = wanted["ticks"].as_i64().unwrap();
    let loaded = Rc::new(load::loaded(&fixture));
    let mut host = load::following_host(&fixture, &loaded);
    let mut ticks_with_journeys = 0;
    let asked = run_ticks(&loaded, &mut host, ticks, &mut |loaded, _| {
        loaded
            .cell
            .with(|world| {
                if let Ok(movement) = world.row::<MovementRow>("movement_v1") {
                    ticks_with_journeys += i64::from(!movement.journeys.is_empty());
                }
            })
            .unwrap();
    });
    assert_eq!(asked.iter().filter(|question| *question == "effects").count() as i64, ticks, "{name}");
    for call in &host.calls {
        assert!(
            matches!(call, Call::Holding | Call::FillSection(_) | Call::FloorPlan | Call::Phase(..) | Call::Metric(..) | Call::Validate(..) | Call::Committed(_) | Call::Canonicalized(_) | Call::ValidateWorld | Call::ValidateExtensions | Call::StartNamespace(_) | Call::ResourcesChanged(..) | Call::Contract(..) | Call::SpaceRefreshed),
            "{name}: {call:?}"
        );
    }
    let phases = host.calls.iter().filter(|call| matches!(call, Call::Phase(Phase::CollectMetrics, false))).count();
    assert_eq!(phases as i64, ticks, "{name}");
    let wanted_types: Vec<String> = wanted["event_types"].as_array().unwrap().iter().map(|item| item.as_str().unwrap().to_owned()).collect();
    assert_eq!(event_types(&loaded), wanted_types, "{name}");
    assert_eq!(metric_lines(&host).len() as u64, wanted["metric_count"].as_u64().unwrap(), "{name}");
    assert_eq!(digests(&loaded), expected(&wanted["world"]), "{name}");
    assert_eq!(metrics_digest(&host), wanted["metrics"].as_str().unwrap(), "{name}");
    assert_eq!(ticks_with_journeys, wanted["ticks_with_journeys"].as_i64().unwrap(), "{name}");
    let ran = Ran { loaded, host, asked, ticks_with_journeys };
    (fixture, ran)
}

/// The Python-parsed sections the ticks read, in the order first read.
fn filled(host: &TestHost) -> Vec<SectionId> {
    let mut filled: Vec<SectionId> = Vec::new();
    for call in &host.calls {
        if let Call::FillSection(which) = call {
            if !filled.contains(which) {
                filled.push(*which);
            }
        }
    }
    filled
}

/// The records of the trips' events that a replan suppressed.
fn replanned(loaded: &Loaded) -> i64 {
    records(loaded).iter().filter(|record| record["status"] == "suppressed" && record["detail"] == REPLANNED).count() as i64
}

#[test]
fn the_loaded_worlds_write_the_bytes_of_the_python_worlds() {
    for name in WORLDS {
        let fixture = load::entry("tick_core", name);
        let loaded = load::loaded(&fixture);
        assert_eq!(digests(&loaded), expected(&fixture["initial"]), "{name}");
    }
}

#[test]
fn the_core_runs_the_small_replay_alone_and_reaches_the_native_path_s_world() {
    let (_, ran) = reaches_the_native_path("small_replay");
    // The check asks only the validators the native path registers for namespaces (absent here)
    // and the effect program's check: a world without Python answers yes (policy E6).
    assert!(ran.asked.iter().all(|question| question.starts_with("namespace ") || question == "effects"), "{:?}", ran.asked);
    // The Python-parsed sections the ticks read, each the exported world's: the model selection,
    // whether the extensions are on (no: the configs have none) and the plan of the check.
    assert_eq!(filled(&ran.host), vec![SectionId::Selection, SectionId::ExtensionsEnabled, SectionId::TickPlan]);
    // No movement: nobody walks.
    assert_eq!(ran.ticks_with_journeys, 0);
}

#[test]
fn the_core_walks_and_replans_the_generated_world_alone_and_reaches_the_native_path_s_world() {
    let (fixture, ran) = reaches_the_native_path("generated");
    let wanted = &fixture["expected"];
    // The extensions `initialize_simulator_extensions` made, each read into its typed row.
    ran.loaded
        .cell
        .with(|world| {
            for name in ["agent_dynamics_v1", "social_network_v1", "staff_dynamics_v1", "staff_operations_v1", "movement_v1", "facility_queues_v1", "effect_program_v1"] {
                let slot = world.slots.get(name).unwrap_or_else(|| panic!("no {name}")).try_borrow().unwrap();
                assert_eq!(slot.place, Place::Namespace, "{name}");
                assert!(slot.row::<ValueRow>().is_err(), "{name} is not a typed row");
            }
            assert!(world.slots.get("population_dynamics_v1").is_some());
        })
        .unwrap();
    // People walked through many ticks, and `update_movement` replanned the trips under way to the
    // facilities that moved.
    assert!(ran.ticks_with_journeys >= 60, "{}", ran.ticks_with_journeys);
    let replans = replanned(&ran.loaded);
    assert!(replans > 0);
    assert_eq!(replans, wanted["replanned"].as_i64().unwrap());
    let types = event_types(&ran.loaded);
    for kind in ["effect_program_geometry", "facility_queue_transition", "agent_travel_complete", "toilet_service_complete", "staff_support_complete"] {
        assert!(types.iter().any(|found| found == kind), "{kind}");
    }
    // The floor plan was built from the export, and again after each move (`load::Following`).
    let moves = records(&ran.loaded).iter().filter(|record| record["event"]["event_type"] == "effect_program_geometry" && record["status"] == "applied").count();
    assert_eq!(moves, 4);
    assert_eq!(ran.host.world.as_ref().unwrap().builds(), 1 + moves);
    // The core started the social model's namespace and wrote `world.resources` itself (Issue #244,
    // decision 2): the host, which writes nothing of the world, was only told.
    let started: Vec<&str> = ran.host.calls.iter().filter_map(|call| if let Call::StartNamespace(name) = call { Some(&**name) } else { None }).collect();
    assert_eq!(started, ["social_propagation_v1"]);
    assert!(ran.host.calls.iter().any(|call| matches!(call, Call::ResourcesChanged(..))));
    // The Python-parsed sections the ticks read through `fill_section`; the floor plan's (the facility
    // geometry and the spatial constraints) are read by `Following::floor_plan`.
    assert_eq!(filled(&ran.host), vec![SectionId::Selection, SectionId::ExtensionsEnabled, SectionId::TickPlan, SectionId::BodyModels]);
}

#[test]
fn two_runs_of_the_core_give_the_same_bytes() {
    for name in WORLDS {
        let fixture = load::entry("tick_core", name);
        let mut seen = Vec::new();
        for _ in 0..2 {
            let loaded = Rc::new(load::loaded(&fixture));
            let mut host = load::following_host(&fixture, &loaded);
            run_ticks(&loaded, &mut host, 120, &mut |_, _| {});
            let bytes: Vec<Vec<u8>> = loaded.cell.with(|world| canonical::parts(world).unwrap().into_iter().map(|(_, raw)| raw).collect()).unwrap();
            seen.push((bytes, metric_lines(&host)));
        }
        assert!(seen[0] == seen[1], "{name}");
    }
}


/// The crate's host without Python (`plain::PlainRun`, #244 PR 4-3; the host of the WASM
/// prototype) runs each world to the native path's world and samples, as the `TestHost` does: the
/// same digests of the world's parts and of the samples, tick by tick until the fixture's last.
#[test]
fn the_plain_host_runs_each_world_to_the_native_path_s_world() {
    for name in WORLDS {
        let fixture = load::entry("tick_core", name);
        let wanted = &fixture["expected"];
        let ticks = wanted["ticks"].as_i64().unwrap();
        let mut run = PlainRun::of_entry(&fixture).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(run.minute().unwrap(), 0, "{name}");
        for at in 0..ticks {
            let minute = run.step().unwrap_or_else(|error| panic!("{name}: {error:?}"));
            assert_eq!(minute, at, "{name}");
        }
        assert_eq!(run.minute().unwrap(), ticks, "{name}");
        assert_eq!(digests(&run.loaded), expected(&wanted["world"]), "{name}");
        // The crate's digests of the document's parts are those of the parts `RUN_PARTS` covers.
        let all: HashMap<&str, String> = run.digests().unwrap().into_iter().collect();
        for (part, digest) in digests(&run.loaded) {
            if part != "scalars" {
                assert_eq!(all[part], digest, "{name}: {part}");
            }
        }
        // The digests read the log a record at a time (#252): each is the digest of the part's
        // bytes, in the same order.
        let whole: Vec<(&str, String)> =
            run.loaded.cell.with(|world| canonical::parts(world).unwrap()).unwrap().into_iter().map(|(part, raw)| (part, sha256_hex(&raw))).collect();
        assert_eq!(run.digests().unwrap(), whole, "{name}");
        assert_eq!(run.host.samples.len() as u64, wanted["metric_count"].as_u64().unwrap(), "{name}");
        assert_eq!(run.host.metrics_digest(), wanted["metrics"].as_str().unwrap(), "{name}");
        // The samples are held packed (#252): their lines written again are the native path's.
        let lines: Vec<Vec<u8>> = (0..run.host.samples.len()).map(|index| run.host.samples.line(index).unwrap().unwrap()).collect();
        assert_eq!(sha256_hex(&lines.join(&b'\n')), wanted["metrics"].as_str().unwrap(), "{name}");
        assert!(run.host.samples.line(lines.len()).unwrap().is_none());
        assert_eq!(run.stepping.checks.asked.iter().filter(|question| *question == "effects").count() as i64, ticks, "{name}");
    }
    // The generated world's EffectProgram moved four facilities: the plan was built again after each.
    let fixture = load::entry("tick_core", "generated");
    let mut run = PlainRun::of_entry(&fixture).unwrap();
    for _ in 0..fixture["expected"]["ticks"].as_i64().unwrap() {
        run.step().unwrap();
    }
    assert_eq!(run.host.plan_builds(), 5);
}

/// A world's clock ends the Run: past its last minute a step says so and changes nothing.
#[test]
fn the_plain_run_stops_at_the_end_of_the_clock() {
    let fixture = load::entry("tick_core", "small_replay");
    let mut run = PlainRun::of_entry(&fixture).unwrap();
    let duration = run.duration().unwrap();
    while run.minute().unwrap() < duration {
        run.step().unwrap_or_else(|error| panic!("{error:?}"));
    }
    let before = run.digests().unwrap();
    assert!(matches!(run.step(), Err(StepError::Complete)));
    assert_eq!(run.digests().unwrap(), before);
}

/// The JSON text of an entry that is not an exported world is an error, not a panic.
#[test]
fn the_plain_run_refuses_what_is_not_an_exported_world() {
    for raw in [&b"not json"[..], b"{}", b"[]", br#"{"world": 1, "slots": [], "run": {}}"#] {
        assert!(PlainRun::of_json(raw).is_err(), "{}", String::from_utf8_lossy(raw));
    }
    let fixture = load::entry("tick_core", "small_replay");
    assert!(PlainRun::of_json(fixture.to_string().as_bytes()).is_ok());
}

/// A world of a file of named worlds is read from the file's own bytes, as the WASM crate's Node
/// test reads `tick_core.json`; a name the file does not hold is an error.
#[test]
fn the_plain_run_reads_a_world_of_a_file_of_worlds() {
    let path = format!("{}/tests/data/tick_core.json", env!("CARGO_MANIFEST_DIR"));
    let raw = std::fs::read(&path).unwrap();
    let mut run = PlainRun::of_worlds_json(&raw, "small_replay").unwrap();
    let mut from_entry = PlainRun::of_entry(&load::entry("tick_core", "small_replay")).unwrap();
    for _ in 0..30 {
        run.step().unwrap();
        from_entry.step().unwrap();
    }
    assert_eq!(run.digests().unwrap(), from_entry.digests().unwrap());
    assert!(PlainRun::of_worlds_json(&raw, "no_such_world").is_err());
    assert!(PlainRun::of_worlds_json(b"{\"worlds\": 1}", "small_replay").is_err());
}
