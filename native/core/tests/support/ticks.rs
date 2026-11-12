//! A Run's ticks on a loaded world, as `TickEngine.step` steps it (#214; shared since Issue #225,
//! lane M7): `run_tick` at the world's minute with the host, the minute moved on, then the
//! end-of-tick check (`after_tick::check`, with no Python validator to ask: `NoPython`).
//!
//! `tick_core.rs` checks that these ticks reach the native path's world; the tests of lane M7 run
//! the generated world's ticks with them and read the worlds of the Run (`world_view_run.rs`,
//! `metric_snapshot.rs`). `metric_line` writes a metric sample as the JSON list of the snapshot's
//! fields, as `json.dumps` of `_snapshot_fields` with `separators=(",", ":")` writes it.

use std::sync::Arc;

use sheltersim_core::config::movement::RouteCapacities;
use sheltersim_core::config::overlay::overlay_schema;
use sheltersim_core::config::SectionId;
use sheltersim_core::error::{Error, Result};
use sheltersim_core::json::Encoder;
use sheltersim_core::rules::after_tick::{self, Checks, HeldNamespaces, Inputs, Memo};
use sheltersim_core::rules::kit;
use sheltersim_core::rules::metrics::MetricSample;
use sheltersim_core::rules::tick::{self, Tick};
use sheltersim_core::world::{Fit, World, WorldCell, MODEL_CONFIGS};

use super::load::Loaded;
use super::TestHost;

/// The end-of-tick check's questions to Python: a world without Python has no validator of its
/// own, and every namespace's row is the world's.
#[derive(Default)]
pub struct NoPython {
    pub asked: Vec<String>,
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
pub fn check_after_tick(loaded: &Loaded, host: &mut TestHost, memo: &mut Memo, checks: &mut NoPython) -> std::result::Result<(), Error> {
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

/// A tick of the loaded Run at `minute`, with this host.
pub fn tick_at<'a>(loaded: &'a Loaded, host: &'a mut TestHost, minute: i64) -> Tick<'a, sheltersim_core::cell::Shard<World>, TestHost, std::cell::RefCell<sheltersim_core::run::Run>> {
    Tick {
        cell: &loaded.cell,
        host,
        run: &loaded.run,
        policies: loaded.policies.clone(),
        minute,
        seed: loaded.seed.clone(),
        neighbors: loaded.neighbors.clone(),
    }
}

/// `TickEngine.step`, `ticks` times: `run_tick` at the world's minute, the minute moved on, the
/// check; `after` sees the world after each tick (its minute the next one), with the host. What the
/// check asked of Python.
pub fn run(loaded: &Loaded, host: &mut TestHost, ticks: i64, after: &mut dyn FnMut(&Loaded, &mut TestHost)) -> Vec<String> {
    let mut memo = Memo::default();
    let mut checks = NoPython::default();
    for _ in 0..ticks {
        let (minute, duration) = loaded.cell.with(|world| (world.minute_index().unwrap(), world.duration_minutes().unwrap())).unwrap();
        assert!(minute < duration, "the clock is complete");
        if let Err(failed) = tick::run_tick(&mut tick_at(loaded, host, minute)) {
            panic!("minute {minute}: {:?} {:?}", failed.label, failed.error);
        }
        loaded.cell.with(|world| world.scalars.get_mut().minute_index = Fit::Ok(minute + 1)).unwrap();
        if let Err(error) = check_after_tick(loaded, host, &mut memo, &mut checks) {
            panic!("minute {minute}: the end-of-tick check: {error:?}");
        }
        after(loaded, host);
    }
    checks.asked
}

/// A metric sample as the JSON list of `MetricSnapshot`'s fields in order, the minute first, with
/// the separators `","` and `":"`.
pub fn metric_line(sample: &MetricSample, minute: i64) -> Vec<u8> {
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
