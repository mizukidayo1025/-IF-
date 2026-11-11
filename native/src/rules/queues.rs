//! The physical queues of a facility: the glue of the core's kernels (`sheltersim_core::rules::
//! queues`, lane c1 of Issue #152). Each entry reads its arguments and what Python holds (the queue
//! policy and the movement model of the configs, the floor plan, whether the world holds the
//! namespaces and `metric_state`, the `repr` of a kind the rule turns down) in the order the kernel
//! read them before, runs the core on the world borrowed whole (`with_world`), and raises what it
//! returns as the kernel did.

use std::sync::Arc;

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyString};
use sheltersim_core::config::movement::Movement;
use sheltersim_core::config::queues::{QueueConfig, QueuePolicy as Policy};
use sheltersim_core::config::FaultClass;
use sheltersim_core::rules::kit::Held;
use sheltersim_core::rules::queues::{self as core, LeaveInputs, Prepare, Prepared, Walk};


use crate::core::configs;
use crate::core::errors::{self, OrPy};
use crate::core::row::{attribute, exact_dict, exact_int, exact_str, unsupported};
use crate::core::world_core::{field_held, namespace_held, with_world};
use crate::rules::movement::{movement_of, EXACT_INT, KEY};
use crate::rules::movement_geometry::{Geo, GeoPy};
use crate::state::events::write_result;

fn exact_bool(value: &Bound<'_, PyAny>) -> PyResult<bool> {
    match value.downcast_exact::<PyBool>() {
        Ok(flag) => Ok(flag.is_true()),
        Err(_) => unsupported("not a bool"),
    }
}

/// A minute a float holds exactly.
fn plain_minute(value: &Bound<'_, PyAny>) -> PyResult<i64> {
    let minute = exact_int(value)?;
    if !(0..EXACT_INT).contains(&minute) {
        return unsupported("not a plain minute");
    }
    Ok(minute)
}

/// `selected_models(model_configs).get("queue")`, kept for the configs' stamp (`core/configs.rs`).
fn selected_queue_model(world: &Bound<'_, PyAny>) -> PyResult<Option<Arc<str>>> {
    match configs::selection(world)? {
        Ok(selection) => Ok(selection.get("queue").cloned()),
        Err(fault) if fault.is_refusal() => unsupported("not a selection"),
        // What it refuses, it refuses in the Python rule too, with the same error.
        Err(_) => Err(errors::relayed(world.py(), configs::reraise_selection(world))),
    }
}

/// `create_queue_policy` of the configs: the policy of the selected model, from the section of the
/// queue configs (`config::queues`), kept for the stamp of `model_configs`. The constructor's
/// `ValueError` and the kernel's refusals are raised here, as they were when it read the tree.
fn policy_of(world: &Bound<'_, PyAny>) -> PyResult<Policy> {
    let config = configs::kept_value(world, |configs| &configs.facility_queues, QueueConfig::parse)?;
    let selected = selected_queue_model(world)?;
    let need_weighted = match &selected {
        Some(model) => match &**model {
            "need-weighted-physical-queue-v1" => true,
            "strict-fifo-queue-v1" => false,
            _ => return unsupported("another queue model"),
        },
        None => config.has_geometry,
    };
    match config.policy(need_weighted) {
        Ok(policy) => Ok(*policy),
        Err(fault) if fault.class == FaultClass::Value => Err(errors::value_error(&*fault.text)),
        Err(fault) => configs::read(Err(fault.clone())),
    }
}

/// The policy of the configs as `policy_of` reads it, for the tests: its name, then the limits,
/// the threshold and the retry delay of the need-weighted one.
pub(crate) fn policy_text(world: &Bound<'_, PyAny>) -> PyResult<(&'static str, Option<([i64; 3], f64, i64)>)> {
    Ok(match policy_of(world)? {
        Policy::Strict => ("strict", None),
        Policy::NeedWeighted { limits, threshold, retry } => ("need_weighted", Some((limits, threshold, retry))),
    })
}

/// Whether the world holds a namespace, as a kernel of the core takes it.
fn held_of(world: &Bound<'_, PyAny>, name: &'static str, other: &'static str) -> PyResult<Held> {
    namespace_held(world, &PyString::new(world.py(), name), other)
}

fn movement_held(world: &Bound<'_, PyAny>) -> PyResult<Held> {
    held_of(world, KEY, "the world holds another movement state")
}

fn queues_held(world: &Bound<'_, PyAny>) -> PyResult<Held> {
    crate::state::facility_queues::held(world)
}

fn service_held(world: &Bound<'_, PyAny>) -> PyResult<Held> {
    held_of(world, "resource_service_v1", "the world holds another service state")
}

/// `repr(kind)`, where the rule turns the kind down and its error names it.
fn kind_repr(py: Python<'_>, kind: &str) -> PyResult<Option<String>> {
    match core::service_of(kind) {
        Some(_) => Ok(None),
        None => errors::repr_str(py, kind).map(Some),
    }
}

/// What those who give up need: the movement model and the floor plan, read as the kernel read
/// them once someone gave up.
struct Leave {
    movement: Arc<Movement>,
    geo: Option<Arc<Geo>>,
    metric_state_held: bool,
}

impl Leave {
    fn read(world: &Bound<'_, PyAny>, moving: &Held, independent: bool) -> PyResult<Self> {
        let metric_state_held = field_held(world, intern!(world.py(), "metric_state"))?;
        let movement = movement_of(world)?;
        let placed = !matches!(moving, Held::Absent) && movement.has_geometry;
        let geo = if placed || independent { Some(Geo::of(world)?) } else { None };
        Ok(Self { movement, geo, metric_state_held })
    }

    fn inputs(&self) -> LeaveInputs<'_> {
        LeaveInputs {
            has_geometry: self.movement.has_geometry,
            model: &self.movement.model,
            geo: self.geo.as_deref(),
            metric_state_held: self.metric_state_held,
        }
    }
}

/// `prepare_facility_queue`.
#[pyfunction]
#[pyo3(signature = (world, facility_id, *, resource, minute))]
pub(crate) fn prepare_facility_queue<'py>(
    world: &Bound<'py, PyAny>,
    facility_id: &Bound<'py, PyAny>,
    resource: &Bound<'py, PyAny>,
    minute: &Bound<'py, PyAny>,
) -> PyResult<()> {
    let py = world.py();
    let facility = exact_str(facility_id)?;
    let resource = exact_bool(resource)?;
    let Some(kind) = with_world(world, |world| core::queued_kind(world, facility, resource))?.or_py()? else {
        return Ok(());
    };
    let minute = plain_minute(minute)?;
    let policy = policy_of(world)?;
    let movement = movement_held(world)?;
    let repr = kind_repr(py, &kind)?;
    let (queues, service) = match resource {
        true => (queues_held(world)?, service_held(world)?),
        false => (Held::Absent, Held::Absent),
    };
    let prepare = Prepare {
        resource,
        minute,
        policy,
        movement,
        queues,
        service,
        kind_repr: repr.as_deref(),
    };
    let mut result = with_world(world, |world| core::prepare(world, facility, &prepare, None))?;
    if matches!(result, Ok(Prepared::NeedsLeaveInputs)) {
        let leave = Leave::read(world, &prepare.movement, false)?;
        result = with_world(world, |world| core::prepare(world, facility, &prepare, Some(&leave.inputs())))?;
    }
    write_result(py, result).map(|_| ())
}

/// `prepare_independent_facility_queue`: an independent FIFO keeps its order, and only those who
/// give up change anything.
#[pyfunction]
#[pyo3(signature = (world, queue_id, *, minute, resource))]
pub(crate) fn prepare_independent_facility_queue<'py>(
    world: &Bound<'py, PyAny>,
    queue_id: &Bound<'py, PyAny>,
    minute: &Bound<'py, PyAny>,
    resource: &Bound<'py, PyAny>,
) -> PyResult<()> {
    let py = world.py();
    let queue = exact_str(queue_id)?;
    let resource = exact_bool(resource)?;
    let minute = plain_minute(minute)?;
    let queues = queues_held(world)?;
    let movement = movement_held(world)?;
    let found = with_world(world, |world| core::independent_queue(world, queue, &queues, &movement))?.or_py()?;
    let Some((facility, kind)) = found else {
        return Ok(());
    };
    let policy = policy_of(world)?;
    let repr = match &kind {
        Some(kind) => kind_repr(py, kind)?,
        None => None,
    };
    let mut prepare = Prepare {
        resource,
        minute,
        policy,
        movement,
        queues,
        service: Held::Absent,
        kind_repr: repr.as_deref(),
    };
    let first = with_world(world, |world| core::prepare_independent(world, queue, &facility, &prepare, None))?;
    if write_result(py, first)? == Prepared::Done {
        return Ok(());
    }
    // Someone gives up: a resource queue's stall is read once each has left.
    if resource {
        prepare.service = service_held(world)?;
    }
    let leave = Leave::read(world, &prepare.movement, true)?;
    let done = with_world(world, |world| core::prepare_independent(world, queue, &facility, &prepare, Some(&leave.inputs())))?;
    write_result(py, done).map(|_| ())
}

/// The movement section of a placed world (`"facility_geometry"` in the configs and `movement_v1`
/// in the state), or None: the walking kernels do nothing then.
fn placed(world: &Bound<'_, PyAny>) -> PyResult<Option<Arc<Movement>>> {
    let table = attribute(world, intern!(world.py(), "model_state"))?;
    if !exact_dict(&table)?.contains(KEY)? {
        return Ok(None);
    }
    let movement = movement_of(world)?;
    Ok(movement.has_geometry.then_some(movement))
}

/// The floor plan of a placed world, which `physical_service_capacity` reads, or None.
pub(crate) fn placement(world: &Bound<'_, PyAny>) -> PyResult<Option<Arc<Geo>>> {
    match placed(world)? {
        Some(_) => Geo::of(world).map(Some),
        None => Ok(None),
    }
}

/// What the walking kernels read of a placed world, as they read it: the movement section, the floor
/// plan, whether the world holds `movement_v1` (and `facility_queues_v1` for an independent queue),
/// and whether `world.metric_state` is its slot's tree. The service rules read it before they borrow
/// the world, for the head's walk to its service point.
pub(crate) struct Walking {
    movement: Arc<Movement>,
    pub(crate) geo: Arc<Geo>,
    moving: Held,
    queues: Held,
    metric_state_held: bool,
}

impl Walking {
    /// None where the world is not placed: the walking kernels do nothing then.
    pub(crate) fn read(world: &Bound<'_, PyAny>, independent: bool) -> PyResult<Option<Self>> {
        let Some(movement) = placed(world)? else {
            return Ok(None);
        };
        Self::of(world, movement, independent).map(Some)
    }

    fn of(world: &Bound<'_, PyAny>, movement: Arc<Movement>, independent: bool) -> PyResult<Self> {
        let geo = Geo::of(world)?;
        let queues = if independent { queues_held(world)? } else { Held::Absent };
        Ok(Self { movement, geo, moving: movement_held(world)?, queues, metric_state_held: field_held(world, intern!(world.py(), "metric_state"))? })
    }

    pub(crate) fn walk(&self) -> Walk<'_> {
        Walk { geo: &self.geo, model: &self.movement.model, movement: self.moving.clone(), queues: self.queues.clone(), metric_state_held: self.metric_state_held }
    }
}

/// A walking kernel, once the world is placed.
fn walk(
    world: &Bound<'_, PyAny>,
    movement: Arc<Movement>,
    facility_id: &str,
    resource: bool,
    minute: i64,
    queue_id: Option<&str>,
    kernel: fn(&sheltersim_core::world::World, &Walk<'_>, &str, bool, i64, Option<&str>) -> sheltersim_core::events::writer::WriteResult<bool>,
) -> PyResult<bool> {
    let walking = Walking::of(world, movement, queue_id.is_some())?;
    let result = with_world(world, |world| kernel(world, &walking.walk(), facility_id, resource, minute, queue_id))?;
    write_result(world.py(), result)
}

/// `progress_physical_queue_positions`.
#[pyfunction]
#[pyo3(signature = (world, facility_id, *, resource, minute, queue_id=None))]
pub(crate) fn progress_physical_queue_positions<'py>(
    world: &Bound<'py, PyAny>,
    facility_id: &Bound<'py, PyAny>,
    resource: &Bound<'py, PyAny>,
    minute: &Bound<'py, PyAny>,
    queue_id: Option<&Bound<'py, PyAny>>,
) -> PyResult<bool> {
    let Some(movement) = placed(world)? else {
        return Ok(false);
    };
    let facility = exact_str(facility_id)?;
    let (resource, minute) = (exact_bool(resource)?, plain_minute(minute)?);
    let queue = match queue_id {
        Some(queue) if !queue.is_none() => Some(exact_str(queue)?),
        _ => None,
    };
    walk(world, movement, facility, resource, minute, queue, core::positions)
}

/// `progress_physical_queue_head_to_service`.
#[pyfunction]
#[pyo3(signature = (world, facility_id, *, resource, minute, queue_id=None))]
pub(crate) fn progress_physical_queue_head_to_service<'py>(
    world: &Bound<'py, PyAny>,
    facility_id: &Bound<'py, PyAny>,
    resource: &Bound<'py, PyAny>,
    minute: &Bound<'py, PyAny>,
    queue_id: Option<&Bound<'py, PyAny>>,
) -> PyResult<bool> {
    let queue_name = match queue_id {
        Some(queue) if !queue.is_none() => Some(exact_str(queue)?),
        _ => None,
    };
    head_to_service(world, facility_id, exact_bool(resource)?, plain_minute(minute)?, queue_name)
}

/// That rule: false has written nothing, and Unsupported changes nothing.
fn head_to_service<'py>(
    world: &Bound<'py, PyAny>,
    facility_id: &Bound<'py, PyAny>,
    resource: bool,
    minute: i64,
    queue_id: Option<&str>,
) -> PyResult<bool> {
    let Some(movement) = placed(world)? else {
        return Ok(false);
    };
    walk(world, movement, exact_str(facility_id)?, resource, minute, queue_id, core::head_to_service)
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(prepare_facility_queue, m)?)?;
    m.add_function(wrap_pyfunction!(prepare_independent_facility_queue, m)?)?;
    m.add_function(wrap_pyfunction!(progress_physical_queue_positions, m)?)?;
    m.add_function(wrap_pyfunction!(progress_physical_queue_head_to_service, m)?)?;
    Ok(())
}
