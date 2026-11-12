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
//! end-of-tick check (`after_tick::check`, with no Python validator to ask).
//!
//! After each world's ticks the digests of the world's parts (the rows, the queue, the log, the
//! fields, `model_state`, the scalars) and of the metric samples are the ones the native path gives
//! on the same world through `TickEngine` (frozen in the fixture by `tests/support/core_tick.py`,
//! written by `benchmarks/core_fixtures.py`, and checked against the native path by
//! `tests/integration/test_core_fixtures.py`).

mod support;

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use serde_json::Value;
use sheltersim_core::checkpoint::sha256_hex;
use sheltersim_core::config::movement::RouteCapacities;
use sheltersim_core::config::overlay::overlay_schema;
use sheltersim_core::config::SectionId;
use sheltersim_core::error::{Error, Result};
use sheltersim_core::host::Phase;
use sheltersim_core::json::Encoder;
use sheltersim_core::row::ValueRow;
use sheltersim_core::rules::after_tick::{self, Checks, HeldNamespaces, Inputs, Memo};
use sheltersim_core::rules::kit;
use sheltersim_core::rules::metrics::MetricSample;
use sheltersim_core::rules::tick::{self, Tick};
use sheltersim_core::state::movement::MovementRow;
use sheltersim_core::world::{Fit, Place, World, WorldCell, MODEL_CONFIGS};

use support::canonical;
use support::load::{self, Loaded};
use support::{Call, TestHost};

/// The end-of-tick check's questions to Python: a world without Python has no validator of its
/// own, and every namespace's row is the world's.
#[derive(Default)]
struct NoPython {
    asked: Vec<String>,
}

impl Checks for NoPython {
    fn namespace(&mut self, name: &str) -> Result<Option<bool>> {
        self.asked.push(format!("namespace {name}"));
        Ok(Some(true))
    }

    fn body_rows(&mut self) -> Result<bool> {
        self.asked.push("body_rows".to_owned());
        Ok(true)
    }

    fn shared_operations(&mut self) -> Result<bool> {
        self.asked.push("shared_operations".to_owned());
        Ok(true)
    }

    fn effects(&mut self) -> Result<()> {
        self.asked.push("effects".to_owned());
        Ok(())
    }

    fn body_config_ids(&mut self) -> Result<Vec<Arc<str>>> {
        self.asked.push("body_config_ids".to_owned());
        Ok(Vec::new())
    }
}

/// `WorldState.validate_after_tick`: the core's check, with the plan and the sections it reads.
fn check_after_tick(loaded: &Loaded, host: &mut TestHost, memo: &mut Memo, checks: &mut NoPython) -> std::result::Result<(), Error> {
    let cell = &loaded.cell;
    let holding = kit::holding(cell, host)?;
    let plan = kit::kept_of(kit::section(cell, host, &holding, SectionId::TickPlan, |configs| &configs.tick_plan)?);
    let plan = kit::kept(&plan)?.clone();
    let route_capacities = kit::kept_section_value(cell, host, &holding, SectionId::RouteCapacities, |configs| &configs.route_capacities, RouteCapacities::parse)?;
    let overlay = kit::kept_section_value(cell, host, &holding, SectionId::OverlaySchema, |configs| &configs.overlay_schema, overlay_schema)?;
    let stamp = cell.with(|world| world.slots.get(MODEL_CONFIGS).unwrap().try_borrow().map(|slot| slot.generation))??;
    let mut held = HeldNamespaces::default();
    let names = after_tick::READ.iter().copied().chain(plan.declared.iter().map(|(name, _)| &**name)).chain(plan.required.iter().map(|name| &**name));
    for name in names {
        held.insert(Arc::from(name), holding.held(name, "the world holds another tree than the namespace's"));
    }
    // Rules bound past times by the clock: a memo holds going forward only.
    let minute = cell.with(|world| world.minute_index())??;
    if memo.minute > minute {
        *memo = Memo::default();
    }
    let inputs = Inputs { plan: &plan, configs: stamp, held: &held, route_capacities: &route_capacities, overlay_schema: &overlay };
    after_tick::check(cell, memo, &inputs, checks)
}

/// `TickEngine.step`, `ticks` times: `run_tick` at the world's minute, the minute moved on, the
/// check; `after` sees the world after each tick.
fn run(loaded: &Loaded, host: &mut TestHost, ticks: i64, after: &mut dyn FnMut(&World)) -> Vec<String> {
    let mut memo = Memo::default();
    let mut checks = NoPython::default();
    for _ in 0..ticks {
        let (minute, duration) = loaded.cell.with(|world| (world.minute_index().unwrap(), world.duration_minutes().unwrap())).unwrap();
        assert!(minute < duration, "the clock is complete");
        let mut tick = Tick {
            cell: &loaded.cell,
            host: &mut *host,
            run: &loaded.run,
            policies: loaded.policies.clone(),
            minute,
            seed: loaded.seed.clone(),
            neighbors: loaded.neighbors.clone(),
        };
        if let Err(failed) = tick::run_tick(&mut tick) {
            panic!("minute {minute}: {:?} {:?}", failed.label, failed.error);
        }
        loaded.cell.with(|world| world.scalars.get_mut().minute_index = Fit::Ok(minute + 1)).unwrap();
        if let Err(error) = check_after_tick(loaded, host, &mut memo, &mut checks) {
            panic!("minute {minute}: the end-of-tick check: {error:?}");
        }
        loaded.cell.with(|world| after(world)).unwrap();
    }
    checks.asked
}

// ---- Digests of the world, as `tests/support/core_tick.py` writes them -----------------------------

/// The canonical JSON of each part of the world, by the names of `PARTS` in `core_tick.py`: the
/// document's parts (`support/canonical.rs`), and the scalars as one list.
fn parts(world: &World) -> Vec<(&'static str, Vec<u8>)> {
    let mut all: HashMap<&'static str, Vec<u8>> = canonical::parts(world).into_iter().collect();
    let mut take = |name: &str| all.remove(name).unwrap_or_else(|| panic!("no part {name}"));
    let mut parts: Vec<(&'static str, Vec<u8>)> = [
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
    ]
    .into_iter()
    .map(|name| (name, take(name)))
    .collect();
    let mut scalars = vec![b'['];
    for (index, name) in ["schema_version", "scenario_id", "seed", "duration_minutes", "minute_index"].into_iter().enumerate() {
        if index > 0 {
            scalars.push(b',');
        }
        scalars.extend_from_slice(&take(name));
    }
    scalars.push(b']');
    parts.push(("scalars", scalars));
    parts
}

fn digests(loaded: &Loaded) -> Vec<(&'static str, String)> {
    loaded.cell.with(|world| parts(world).into_iter().map(|(name, raw)| (name, sha256_hex(&raw))).collect()).unwrap()
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

fn metric_line(sample: &MetricSample, minute: i64) -> Vec<u8> {
    let mut encoder = Encoder::with_capacity(256);
    let ints = |encoder: &mut Encoder, value: usize| encoder.int(value as i64);
    encoder.out.push(b'[');
    encoder.int(minute);
    let next = |encoder: &mut Encoder| encoder.out.push(b',');
    next(&mut encoder);
    ints(&mut encoder, sample.toilet_queue_length);
    next(&mut encoder);
    ints(&mut encoder, sample.toilet_active_users);
    next(&mut encoder);
    encoder.int(sample.toilet_working_units);
    for value in [sample.toilet_wait_count, sample.toilet_wait_total_minutes, sample.toilet_wait_mean_minutes, sample.toilet_wait_max_minutes, sample.stress_mean] {
        next(&mut encoder);
        encoder.try_float(value).unwrap();
    }
    for value in [sample.stress_high_count, sample.staff_assigned_task_count, sample.staff_busy_count] {
        next(&mut encoder);
        ints(&mut encoder, value);
    }
    next(&mut encoder);
    encoder.try_float(sample.staff_load_ratio).unwrap();
    next(&mut encoder);
    ints(&mut encoder, sample.unresolved_support_request_count);
    next(&mut encoder);
    encoder.string(&sample.source_ids_json);
    next(&mut encoder);
    ints(&mut encoder, sample.support_no_qualified_worker_count);
    for value in [
        sample.support_no_qualified_worker_request_minutes,
        sample.support_fairness_promoted_assignment_count,
        sample.support_wait_staff_busy_request_minutes,
        sample.support_wait_staff_off_shift_request_minutes,
    ] {
        next(&mut encoder);
        encoder.try_float(value).unwrap();
    }
    encoder.out.push(b']');
    encoder.out
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
/// core but the sinks, the observer, the contracts (which give their input back) and what the host
/// does for the world it owns (the sections, the plan, a namespace started, `world.resources`).
fn reaches_the_native_path(name: &str) -> (Value, Ran) {
    let fixture = load::entry("tick_core", name);
    let wanted = &fixture["expected"];
    let ticks = wanted["ticks"].as_i64().unwrap();
    let loaded = Rc::new(load::loaded(&fixture));
    let mut host = load::following_host(&fixture, &loaded);
    let mut ticks_with_journeys = 0;
    let asked = run(&loaded, &mut host, ticks, &mut |world| {
        if let Ok(movement) = world.row::<MovementRow>("movement_v1") {
            ticks_with_journeys += i64::from(!movement.journeys.is_empty());
        }
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
            run(&loaded, &mut host, 120, &mut |_| {});
            let bytes: Vec<Vec<u8>> = loaded.cell.with(|world| parts(world).into_iter().map(|(_, raw)| raw).collect()).unwrap();
            seen.push((bytes, metric_lines(&host)));
        }
        assert!(seen[0] == seen[1], "{name}");
    }
}

