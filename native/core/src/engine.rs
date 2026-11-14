//! A Run's ticks without Python, as `TickEngine.step` steps a world (#214; in the crate since #244,
//! PR 4-3, for a host without Python such as the WASM prototype): `run_tick` at the world's minute
//! with the host, the minute moved on, then the end-of-tick check (`EndCheck`): the core's whole
//! validation without the event history (`validate::world`, #250), what `validate_after_tick` of
//! Python answers, or for a test the kernel's check alone (`after_tick::check`, with no Python
//! validator to ask: `NoPython`).
//!
//! `metric_line` writes a metric sample as the JSON list of the snapshot's fields, as `json.dumps`
//! of `_snapshot_fields` with `separators=(",", ":")` writes it: the lines a host keeps for the
//! digest of a Run's samples (`tests/support/core_tick.py`'s `metrics_digest`).

use std::cell::RefCell;
use std::sync::Arc;

use crate::cell::Shard;
use crate::config::movement::RouteCapacities;
use crate::config::overlay::overlay_schema;
use crate::config::SectionId;
use crate::error::{Error, Result};
use crate::host::Host;
use crate::json::Encoder;
use crate::load::Loaded;
use crate::rules::after_tick::{self, Checks, HeldNamespaces, Inputs, Memo};
use crate::rules::kit;
use crate::rules::metrics::MetricSample;
use crate::rules::tick::{self, Failed, Tick};
use crate::run::Run;
use crate::world::{Fit, World, WorldCell, MODEL_CONFIGS};

/// The end-of-tick check's questions to Python: a world without Python has no validator of its
/// own, and every namespace's row is the world's (policy E6). It keeps what was asked.
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

/// `WorldState.validate_after_tick`: the core's check, with the plan and the sections it reads
/// through the host.
pub fn check_after_tick<H: Host>(cell: &Shard<World>, host: &mut H, memo: &mut Memo, checks: &mut impl Checks) -> std::result::Result<(), H::Error> {
    let holding = kit::holding(cell, host)?;
    let plan = kit::kept_of(kit::section(cell, host, &holding, SectionId::TickPlan, |configs| &configs.tick_plan)?);
    let plan = kit::kept(&plan)?.clone();
    let route_capacities = kit::kept_section_value(cell, host, &holding, SectionId::RouteCapacities, |configs| &configs.route_capacities, RouteCapacities::parse)?;
    let overlay = kit::kept_section_value(cell, host, &holding, SectionId::OverlaySchema, |configs| &configs.overlay_schema, overlay_schema)?;
    let stamp = cell
        .with(|world| -> Result<_> {
            let slot = world.slots.get(MODEL_CONFIGS).ok_or_else(|| Error::unsupported("the world does not own model_configs"))?;
            Ok(slot.try_borrow()?.generation)
        })
        .and_then(|stamp| stamp)
        .map_err(H::Error::from)?;
    let mut held = HeldNamespaces::default();
    let names = after_tick::READ.iter().copied().chain(plan.declared.iter().map(|(name, _)| &**name)).chain(plan.required.iter().map(|name| &**name));
    for name in names {
        held.insert(Arc::from(name), holding.held(name, "the world holds another tree than the namespace's"));
    }
    // Rules bound past times by the clock: a memo holds going forward only.
    let minute = cell.with(|world| world.minute_index()).and_then(|minute| minute).map_err(H::Error::from)?;
    if memo.minute > minute {
        *memo = Memo::default();
    }
    let inputs = Inputs { plan: &plan, configs: stamp, held: &held, route_capacities: &route_capacities, overlay_schema: &overlay };
    after_tick::check(cell, memo, &inputs, checks).map_err(H::Error::from)
}

/// Which check ends a tick.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EndCheck {
    /// The kernel's check alone (`after_tick::check` with `NoPython`): it takes every namespace as
    /// valid and has no texts. For tests of the kernel's check.
    #[default]
    Kernel,
    /// `WorldState.validate_after_tick`: `validate(validate_event_history=False)`, the core's
    /// (`validate::world`, #250), with Python's class and text of the first error. The native path
    /// asks the kernel's check first and runs Python's validation where it refuses; the kernel says
    /// yes only where every rule passes, so the answer is the validation's.
    Whole,
}

/// What the end-of-tick check keeps from one tick to the next, and what it asked.
#[derive(Default)]
pub struct Stepping {
    pub memo: Memo,
    pub checks: NoPython,
    pub end: EndCheck,
}

impl Stepping {
    /// The stepping of a Run without Python (`plain::PlainRun`): each tick ends with the whole
    /// validation (`EndCheck::Whole`).
    pub fn whole() -> Self {
        Self { end: EndCheck::Whole, ..Self::default() }
    }
}

/// `WorldState.validate_after_tick` (`EndCheck::Whole`): `validate(validate_event_history=False)`.
pub fn validate_after_tick(cell: &Shard<World>) -> Result<()> {
    cell.with(|world| crate::validate::world(world, &crate::validate::Options { history: false, in_flight: None }))?
}

/// Why a step stopped: the tick's failure (the phase's label and the host's error), or the check's.
#[derive(Debug)]
pub enum StepError<E> {
    /// The world's clock is complete: no tick is left.
    Complete,
    Tick { minute: i64, label: Option<&'static str>, error: E },
    Check { minute: i64, error: E },
}

/// A tick of the loaded Run at `minute`, with this host: the world, the Run, its policies, the
/// zones' neighbors and the random source (`CounterRNG(world.seed)`).
pub fn tick_at<'a, H: Host>(loaded: &'a Loaded, host: &'a mut H, minute: i64) -> Tick<'a, Shard<World>, H, RefCell<Run>> {
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

/// `TickEngine.step`: `run_tick` at the world's minute, the minute moved on, the check. The minute
/// the tick ran at.
pub fn step<H: Host>(loaded: &Loaded, host: &mut H, stepping: &mut Stepping) -> std::result::Result<i64, StepError<H::Error>> {
    let cell = &loaded.cell;
    let clock = cell.with(|world| -> Result<(i64, i64)> { Ok((world.minute_index()?, world.duration_minutes()?)) }).and_then(|clock| clock);
    let (minute, duration) = clock.map_err(|error| StepError::Check { minute: -1, error: <H::Error as From<Error>>::from(error) })?;
    if minute >= duration {
        return Err(StepError::Complete);
    }
    if let Err(Failed { label, error }) = tick::run_tick(&mut tick_at(loaded, host, minute)) {
        return Err(StepError::Tick { minute, label, error });
    }
    cell.with(|world| world.scalars.get_mut().minute_index = Fit::Ok(minute + 1))
        .map_err(|error| StepError::Check { minute, error: <H::Error as From<Error>>::from(error) })?;
    match stepping.end {
        EndCheck::Kernel => check_after_tick(cell, host, &mut stepping.memo, &mut stepping.checks),
        EndCheck::Whole => validate_after_tick(cell).map_err(<H::Error as From<Error>>::from),
    }
    .map_err(|error| StepError::Check { minute, error })?;
    Ok(minute)
}

/// A metric sample as the JSON list of `MetricSnapshot`'s fields in order, the minute first, with
/// the separators `","` and `":"`. A float JSON cannot hold (NaN, an infinity) is an error.
pub fn metric_line(sample: &MetricSample, minute: i64) -> Result<Vec<u8>> {
    let mut encoder = Encoder::with_capacity(256);
    let ints = |encoder: &mut Encoder, value: usize| encoder.int(value as i64);
    let next = |encoder: &mut Encoder| encoder.out.push(b',');
    encoder.out.push(b'[');
    encoder.int(minute);
    next(&mut encoder);
    ints(&mut encoder, sample.toilet_queue_length);
    next(&mut encoder);
    ints(&mut encoder, sample.toilet_active_users);
    next(&mut encoder);
    encoder.int(sample.toilet_working_units);
    for value in [sample.toilet_wait_count, sample.toilet_wait_total_minutes, sample.toilet_wait_mean_minutes, sample.toilet_wait_max_minutes, sample.stress_mean] {
        next(&mut encoder);
        encoder.try_float(value)?;
    }
    for value in [sample.stress_high_count, sample.staff_assigned_task_count, sample.staff_busy_count] {
        next(&mut encoder);
        ints(&mut encoder, value);
    }
    next(&mut encoder);
    encoder.try_float(sample.staff_load_ratio)?;
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
        encoder.try_float(value)?;
    }
    encoder.out.push(b']');
    Ok(encoder.out)
}
