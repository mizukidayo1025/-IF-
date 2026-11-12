//! A Run's ticks in the core without Python (Issue #152, lane P10-1b; definition of done 4.1-(4) of
//! design-p8.md, notes for P10 in design-p9.md section 8).
//!
//! The World is the small world of `tests/support/core_tick.py` (h00_small_replay: 4 people, 4
//! zones, 6 edges, 4 toilets, 5 staff, a toilet's outage at minute 15 and its recovery at 90), read
//! from `tests/data/tick_core.json` by the test-only loader (`support/load.rs`, orchestrator decision
//! (b)). The Run's policies are what the PyO3 crate read of that controller. Each tick is the core's
//! `run_tick` with the `TestHost` (decision (c)): what the world holds from its slots, the
//! Python-parsed sections with fixed test values, contracts that take their input as given, no
//! foreign handler, sinks into its calls, no LLM, `space_refreshed` Ok. The test does what
//! `TickEngine.step` does around the phases: the minute moves on, then the end-of-tick check
//! (`after_tick::check`, with no Python validator to ask).
//!
//! After `TICKS` ticks the digests of the world's parts (the rows, the queue, the log, the fields,
//! `model_state`, the scalars) and of the metric samples are the ones the native path gives on the
//! same world through `TickEngine` (frozen in the fixture by `benchmarks/core_tick_fixture.py`, and
//! checked against the native path by `tests/integration/test_core_tick.py`).
//!
//! The update phase's movement and replanning are the core's since P9-3b (`update_movement`); this
//! world has no `movement_v1`, so neither path walks anyone (extending the fixture is P10 work).

mod support;

use std::sync::Arc;

use serde_json::Value;
use sheltersim_core::checkpoint::sha256_hex;
use sheltersim_core::config::movement::RouteCapacities;
use sheltersim_core::config::overlay::overlay_schema;
use sheltersim_core::config::SectionId;
use sheltersim_core::entities::{EntityRow, Table};
use sheltersim_core::error::{Error, Result};
use sheltersim_core::host::Phase;
use sheltersim_core::json::Encoder;
use sheltersim_core::rules::after_tick::{self, Checks, HeldNamespaces, Inputs, Memo};
use sheltersim_core::rules::kit;
use sheltersim_core::rules::metrics::MetricSample;
use sheltersim_core::rules::tick::{self, Tick};
use sheltersim_core::world::{Fit, Place, World, WorldCell, MODEL_CONFIGS};

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
/// check.
fn run(loaded: &Loaded, host: &mut TestHost, ticks: i64) -> Vec<String> {
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
    }
    checks.asked
}

// ---- Digests of the world, as `tests/support/core_tick.py` writes them -----------------------------

fn table<R: EntityRow>(table: &Table<R>, version: i64) -> Vec<u8> {
    let mut rows: Vec<&R> = table.iter().map(|(_, row)| row).collect();
    rows.sort_by(|first, second| first.id().as_bytes().cmp(second.id().as_bytes()));
    let mut encoder = Encoder::with_capacity(1024);
    encoder.out.push(b'[');
    for (index, row) in rows.into_iter().enumerate() {
        if index > 0 {
            encoder.out.push(b',');
        }
        row.write_json(&mut encoder, version).unwrap();
    }
    encoder.out.push(b']');
    encoder.out
}

fn slot_json(world: &World, name: &str) -> Vec<u8> {
    let slot = world.slots.get(name).unwrap().try_borrow().unwrap();
    let mut encoder = Encoder::with_capacity(256);
    slot.row.as_deref().unwrap_or_else(|| panic!("{name} has no row")).write_json(&mut encoder).unwrap();
    encoder.out
}

/// The canonical JSON of each part of the world, by the names of `PARTS` in `core_tick.py`.
fn parts(world: &World) -> Vec<(&'static str, Vec<u8>)> {
    let version = world.schema_version().unwrap();
    let rows = world.rows().unwrap();
    let mut parts = vec![
        ("agents", table(&rows.agents, version)),
        ("zones", table(&rows.zones, version)),
        ("zone_edges", table(&rows.zone_edges, version)),
        ("facilities", table(&rows.facilities, version)),
        ("staff", table(&rows.staff, version)),
        ("support_requests", table(&rows.support_requests, version)),
        ("active_tasks", table(&rows.active_tasks, version)),
        ("resources", slot_json(world, "resources")),
    ];
    drop(rows);
    let queue = world.queue().unwrap();
    let mut encoder = Encoder::with_capacity(1024);
    encoder.out.push(b'[');
    for (index, (_, serial)) in queue.ordered().iter().enumerate() {
        if index > 0 {
            encoder.out.push(b',');
        }
        queue.slot(*serial).row.as_ref().expect("an event with its row").write_json(&mut encoder).unwrap();
    }
    encoder.out.push(b']');
    parts.push(("event_queue", encoder.out));
    let log = world.log().unwrap();
    let mut written = vec![b'['];
    for index in 0..log.len() {
        if index > 0 {
            written.push(b',');
        }
        written.extend_from_slice(log.written(index).expect("a record's bytes"));
    }
    written.push(b']');
    parts.push(("event_log", written));
    parts.push(("id_counters", slot_json(world, "id_counters")));
    parts.push(("metric_state", slot_json(world, "metric_state")));
    parts.push(("model_configs", slot_json(world, "model_configs")));
    let mut names: Vec<&Arc<str>> = world
        .slots
        .names()
        .filter(|name| {
            let slot = world.slots.get(name).unwrap().try_borrow().unwrap();
            slot.place != Place::Field && slot.row.is_some()
        })
        .collect();
    names.sort_by(|first, second| first.as_bytes().cmp(second.as_bytes()));
    let mut encoder = Encoder::with_capacity(256);
    encoder.out.push(b'{');
    for (index, name) in names.into_iter().enumerate() {
        if index > 0 {
            encoder.out.push(b',');
        }
        encoder.string(name);
        encoder.out.push(b':');
        encoder.out.extend_from_slice(&slot_json(world, name));
    }
    encoder.out.push(b'}');
    parts.push(("model_state", encoder.out));
    let scalars = world.scalars().unwrap();
    let mut encoder = Encoder::with_capacity(64);
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
    parts.push(("scalars", encoder.out));
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

fn event_types(loaded: &Loaded) -> Vec<String> {
    loaded
        .cell
        .with(|world| {
            let log = world.log().unwrap();
            (0..log.len())
                .map(|index| {
                    let record: Value = serde_json::from_slice(log.written(index).unwrap()).unwrap();
                    record["event"]["event_type"].as_str().unwrap().to_owned()
                })
                .collect()
        })
        .unwrap()
}

// ---- The tests ---------------------------------------------------------------------------------------

#[test]
fn the_loaded_world_writes_the_bytes_of_the_python_world() {
    let fixture = load::fixture();
    let loaded = load::loaded(&fixture);
    assert_eq!(digests(&loaded), expected(&fixture["initial"]));
}

#[test]
fn the_core_runs_the_ticks_alone_and_reaches_the_native_path_s_world() {
    let fixture = load::fixture();
    let wanted = &fixture["expected"];
    let ticks = wanted["ticks"].as_i64().unwrap();
    let loaded = load::loaded(&fixture);
    let mut host = load::host(&fixture);
    let asked = run(&loaded, &mut host, ticks);
    // The check asks only the validators the native path registers for namespaces (absent here)
    // and the effect program's check: a world without Python answers yes (policy E6).
    assert!(asked.iter().all(|question| question.starts_with("namespace ") || question == "effects"), "{asked:?}");
    assert_eq!(asked.iter().filter(|question| *question == "effects").count() as i64, ticks);
    // Nothing went outside the core but the sinks and the observer.
    for call in &host.calls {
        assert!(
            matches!(call, Call::Holding | Call::FillSection(_) | Call::FloorPlan | Call::Phase(..) | Call::Metric(..) | Call::Validate(..) | Call::Committed(_) | Call::Canonicalized(_) | Call::ValidateWorld | Call::ValidateExtensions),
            "{call:?}"
        );
    }
    let mut filled: Vec<SectionId> = Vec::new();
    for call in &host.calls {
        if let Call::FillSection(which) = call {
            if !filled.contains(which) {
                filled.push(*which);
            }
        }
    }
    // The Python-parsed sections the ticks read: the model selection (the fixture's), whether the
    // extensions are on (no: the configs have none) and the plan of the check (the test's).
    assert_eq!(filled, vec![SectionId::Selection, SectionId::ExtensionsEnabled, SectionId::TickPlan]);
    let phases = host.calls.iter().filter(|call| matches!(call, Call::Phase(Phase::CollectMetrics, false))).count();
    assert_eq!(phases as i64, ticks);
    let wanted_types: Vec<String> = wanted["event_types"].as_array().unwrap().iter().map(|item| item.as_str().unwrap().to_owned()).collect();
    assert_eq!(event_types(&loaded), wanted_types);
    assert_eq!(metric_lines(&host).len() as u64, wanted["metric_count"].as_u64().unwrap());
    assert_eq!(digests(&loaded), expected(&wanted["world"]));
    assert_eq!(metrics_digest(&host), wanted["metrics"].as_str().unwrap());
}

#[test]
fn two_runs_of_the_core_give_the_same_bytes() {
    let fixture = load::fixture();
    let mut seen = Vec::new();
    for _ in 0..2 {
        let loaded = load::loaded(&fixture);
        let mut host = load::host(&fixture);
        run(&loaded, &mut host, 120);
        let bytes: Vec<Vec<u8>> = loaded.cell.with(|world| parts(world).into_iter().map(|(_, raw)| raw).collect()).unwrap();
        seen.push((bytes, metric_lines(&host)));
    }
    assert!(seen[0] == seen[1]);
}
