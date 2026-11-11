//! `domain/facility_queues.py`: the glue of the core's kernels (`sheltersim_core::rules::
//! facility_queues`, lane c1 of Issue #152). Each entry reads what Python holds (whether the world
//! holds the namespace, the floor plan, the configs), runs the core on the world borrowed whole
//! (`with_world`), and makes the Python values of what it returns. The core writes the row alone;
//! an attached namespace's tree follows it when the borrow ends (lane P8-0c).
//!
//! The writes the service rules call (`write_*`, `transition_row`, `write_transition`) keep their
//! names and arguments; the readers they call while they hold other borrows read the row through its
//! mirror (`state/facility_queues.rs`).

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PySet, PyTuple};
use sheltersim_core::config::population::AccessibilityNeeds;
use sheltersim_core::rules::facility_queues as core;
use sheltersim_core::rules::kit::Held;

pub(crate) use sheltersim_core::rules::facility_queues::{point_of, position_in, request_of_task};
pub(crate) use crate::state::facility_queues::{
    detached_records, has_independent, independent_heads, with_queues,
};

use crate::core::configs;
use crate::core::errors::{self, OrPy};
use crate::core::row::{attribute, exact_dict, exact_int, exact_str, PyField};
use crate::core::world_core::{namespace_held, with_world};
use crate::rules::movement_geometry::{self, Geo, GeoPy, ServicePoint};
use crate::state::events::{write_result, EventRow, EventStatus, EventWriter};
use crate::state::facility_queues::{held, held_mirror, request_node, QueueRequest};
use crate::Unsupported;

pub(crate) const STATE_KEY: &str = core::STATE_KEY;

/// Whether the world holds `movement_v1`, as `movement::mirror_held` answers.
fn movement_held(world: &Bound<'_, PyAny>) -> PyResult<Held> {
    namespace_held(world, intern!(world.py(), "movement_v1"), "the world holds another movement state")
}

// ---- Writing, for the service rules ------------------------------------------------------------------

/// `enqueue`, after its check.
pub(crate) fn write_enqueue(
    world: &Bound<'_, PyAny>,
    point: &ServicePoint,
    request_id: &str,
    task_id: &str,
    agent_id: &str,
    service_kind: &str,
    requested_minute: i64,
) -> PyResult<()> {
    with_world(world, |world| core::write_enqueue(world, point, request_id, task_id, agent_id, service_kind, requested_minute))?.or_py()
}

/// `start_service`'s writes, the request being one the queue holds as queued.
pub(crate) fn write_start(world: &Bound<'_, PyAny>, request_id: &str, queue_id: &str, active_task_id: &str) -> PyResult<()> {
    with_world(world, |world| core::write_start(world, request_id, queue_id, active_task_id))?.or_py()
}

/// `finish_service`'s writes, for the request `request_of_task` found.
pub(crate) fn write_finish(world: &Bound<'_, PyAny>, request: &QueueRequest) -> PyResult<()> {
    with_world(world, |world| core::write_finish(world, request))?.or_py()
}

/// `queued.remove(request_id)` and `queued.insert(0, request_id)` of a request just queued: it
/// goes back to the head of its queue.
pub(crate) fn write_to_head(world: &Bound<'_, PyAny>, queue_id: &str, request_id: &str) -> PyResult<()> {
    with_world(world, |world| core::write_to_head(world, queue_id, request_id))?.or_py()
}

/// `mark_stall`.
pub(crate) fn write_stall(world: &Bound<'_, PyAny>, queue_id: &str, request_id: &str, reason: &str) -> PyResult<()> {
    with_world(world, |world| core::write_stall(world, queue_id, request_id, reason))?.or_py()
}

/// The record of `record_queue_transition`, changing nothing: `sequence` is the one it will take.
#[allow(clippy::too_many_arguments)]
pub(crate) fn transition_row(
    writer: &EventWriter<'_>,
    request: &QueueRequest,
    transition: &str,
    task_id: &str,
    wait_minutes: Option<i64>,
    causation_id: Option<&str>,
    queue_position: Option<i64>,
    capacity: Option<i64>,
    sequence: i64,
    minute: i64,
) -> PyResult<EventRow> {
    let taken = writer.is_realized(&core::transition_id(sequence), Some(sequence))?;
    let payload =
        core::transition_payload(taken, request, transition, task_id, wait_minutes, causation_id, queue_position, capacity, sequence, minute)
            .or_py()?;
    core::transition_event(payload, causation_id, sequence, minute).or_py()
}

/// The record `transition_row` made, in the log.
pub(crate) fn write_transition(writer: &EventWriter<'_>, row: &EventRow, minute: i64) -> PyResult<()> {
    writer.allocate_sequence()?;
    writer.record(row, minute, EventStatus::Applied, None)
}

// ---- The kernels ---------------------------------------------------------------------------------

/// `independent_queues_required` of a configs dict, for Python. A world's own configs are read
/// through their section (`movement_geometry::independent_queues`).
#[pyfunction]
fn independent_queues_required(configs: &Bound<'_, PyAny>) -> PyResult<bool> {
    movement_geometry::independent_queues_of_configs(configs)
}

/// `is_legacy_queue_checkpoint`.
#[pyfunction]
fn is_legacy_queue_checkpoint(world: &Bound<'_, PyAny>) -> PyResult<bool> {
    let py = world.py();
    if !movement_geometry::independent_queues(world)? {
        return Ok(false);
    }
    let table = attribute(world, intern!(py, "model_state"))?;
    let table = exact_dict(&table)?;
    if table.get_item(STATE_KEY)?.is_some_and(|state| !state.is_none()) {
        return Ok(false);
    }
    let Some(mode) = table.get_item(core::LEGACY_KEY)? else {
        return Ok(false);
    };
    let expected = PyDict::new(py);
    expected.set_item("schema_version", 1)?;
    expected.set_item("mode", "pre_independent_queue_checkpoint")?;
    expected.set_item("policy_version", "facility_fifo_legacy-v1")?;
    mode.eq(expected)
}

/// `facility_has_independent_queues`.
#[pyfunction]
fn facility_has_independent_queues(world: &Bound<'_, PyAny>, facility_id: &Bound<'_, PyAny>) -> PyResult<bool> {
    has_independent(world, exact_str(facility_id)?)
}

/// `create_facility_queue_state`.
#[pyfunction]
fn create_facility_queue_state<'py>(world: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyDict>> {
    if !movement_geometry::independent_queues(world)? {
        return Err(errors::value_error("facility_queues_v1 requires independent explicit service points"));
    }
    let state = core::create_state(&*Geo::of(world)?).or_py()?;
    PyField::to_py(&state, world.py())?.downcast_into::<PyDict>().map_err(PyErr::from)
}

/// `choose_queue_id`.
#[pyfunction]
pub(crate) fn choose_queue_id(world: &Bound<'_, PyAny>, facility_id: &Bound<'_, PyAny>, agent_id: &Bound<'_, PyAny>) -> PyResult<Option<String>> {
    let (facility_id, agent_id) = (exact_str(facility_id)?, exact_str(agent_id)?);
    let held = held(world)?;
    if held == Held::Absent {
        return Ok(None);
    }
    // The population's needs (`config::population`), kept for the stamp of `model_configs`.
    let needs = configs::kept_value(world, |configs| &configs.accessibility_needs, AccessibilityNeeds::parse)?;
    let needs = configs::read(needs.of(agent_id))?;
    let geo = Geo::of(world)?;
    let chosen = with_world(world, |world| core::choose_queue(world, &held, &geo, facility_id, agent_id, needs))?.or_py()?;
    Ok(chosen.map(|id| id.to_string()))
}

/// `queue_ids_for_facility`.
#[pyfunction]
pub(crate) fn queue_ids_for_facility<'py>(world: &Bound<'py, PyAny>, facility_id: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyTuple>> {
    let geo = Geo::of(world)?;
    PyTuple::new(world.py(), core::queue_ids(&geo, exact_str(facility_id)?))
}

/// `queue_head_agent_ids`.
#[pyfunction]
fn queue_head_agent_ids<'py>(world: &Bound<'py, PyAny>, facility_id: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PySet>> {
    let facility_id = exact_str(facility_id)?;
    let held = held(world)?;
    let heads = with_world(world, |world| core::head_agents(world, &held, facility_id))?.or_py()?;
    PySet::new(world.py(), heads.iter().map(|id| &**id))
}

/// `request_for_task`: the tree's own record.
#[pyfunction]
fn request_for_task<'py>(world: &Bound<'py, PyAny>, task_id: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyAny>>> {
    let task_id = exact_str(task_id)?;
    let found = with_queues(world, |row| Ok(request_of_task(row, task_id).map(|request| request.request_id.clone())))?.flatten();
    match found {
        Some(id) => request_node(world, &id),
        None => Ok(None),
    }
}

/// `request_for_agent`: the tree's own record.
#[pyfunction]
#[pyo3(signature = (world, agent_id, *, queue_id=None))]
fn request_for_agent<'py>(world: &Bound<'py, PyAny>, agent_id: &Bound<'py, PyAny>, queue_id: Option<&Bound<'py, PyAny>>) -> PyResult<Option<Bound<'py, PyAny>>> {
    let agent_id = exact_str(agent_id)?;
    let queue_id = match queue_id {
        Some(queue) if !queue.is_none() => Some(exact_str(queue)?),
        _ => None,
    };
    let found = with_queues(world, |row| Ok(core::request_of(row, agent_id, queue_id).map(|request| request.request_id.clone())))?.flatten();
    match found {
        Some(id) => request_node(world, &id),
        None => Ok(None),
    }
}

/// `queue_agent_ids`.
#[pyfunction]
fn queue_agent_ids<'py>(world: &Bound<'py, PyAny>, queue_id: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyList>> {
    let queue_id = exact_str(queue_id)?;
    let members = with_queues(world, |row| core::members_of(row, queue_id).or_py())?.unwrap_or_default();
    PyList::new(world.py(), members.iter().map(|id| &**id))
}

/// `enqueue`.
#[pyfunction]
#[pyo3(signature = (world, *, queue_id, request_id, task_id, agent_id, service_kind, requested_minute))]
#[allow(clippy::too_many_arguments)]
fn enqueue(
    world: &Bound<'_, PyAny>,
    queue_id: &Bound<'_, PyAny>,
    request_id: &Bound<'_, PyAny>,
    task_id: &Bound<'_, PyAny>,
    agent_id: &Bound<'_, PyAny>,
    service_kind: &Bound<'_, PyAny>,
    requested_minute: &Bound<'_, PyAny>,
) -> PyResult<()> {
    let (queue_id, request_id) = (exact_str(queue_id)?, exact_str(request_id)?);
    let (task_id, agent_id, service_kind) = (exact_str(task_id)?, exact_str(agent_id)?, exact_str(service_kind)?);
    let requested_minute = exact_int(requested_minute)?;
    let geo = Geo::of(world)?;
    let held = held(world)?;
    with_world(world, |world| core::enqueue(world, &held, &geo, queue_id, request_id, task_id, agent_id, service_kind, requested_minute))?
        .or_py()
}

/// `start_service`.
#[pyfunction]
fn start_service<'py>(world: &Bound<'py, PyAny>, request_id: &Bound<'py, PyAny>, active_task_id: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let (request_text, active_task_id) = (exact_str(request_id)?, exact_str(active_task_id)?);
    let held = held(world)?;
    with_world(world, |world| core::start_service(world, &held, request_text, active_task_id))?.or_py()?;
    request_node(world, request_text)?.ok_or_else(|| Unsupported::new_err("no such request"))
}

/// `finish_service`: a copy of the request with its last status, or None.
#[pyfunction]
#[pyo3(signature = (world, task_id, *, status))]
fn finish_service<'py>(world: &Bound<'py, PyAny>, task_id: &Bound<'py, PyAny>, status: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyAny>>> {
    let py = world.py();
    let task_text = exact_str(task_id)?;
    let found = with_queues(world, |row| Ok(request_of_task(row, task_text).cloned()))?.flatten();
    let Some(request) = found else {
        return Ok(None);
    };
    let node = request_node(world, &request.request_id)?.ok_or_else(|| Unsupported::new_err("no such request"))?;
    let copy = PyDict::new(py);
    copy.call_method1(intern!(py, "update"), (&node,))?;
    copy.set_item(intern!(py, "status"), status)?;
    write_finish(world, &request)?;
    Ok(Some(copy.into_any()))
}

/// `mark_stall`.
#[pyfunction]
fn mark_stall(world: &Bound<'_, PyAny>, queue_id: &Bound<'_, PyAny>, request_id: &Bound<'_, PyAny>, reason: &Bound<'_, PyAny>) -> PyResult<()> {
    let (queue_id, request_id, reason) = (exact_str(queue_id)?, exact_str(request_id)?, exact_str(reason)?);
    let held = held(world)?;
    with_world(world, |world| core::mark_stall(world, &held, queue_id, request_id, reason))?.or_py()
}

/// `record_queue_transition`.
#[pyfunction]
#[pyo3(signature = (world, request, *, transition, task_id, wait_minutes=None, causation_id=None))]
fn record_queue_transition(
    world: &Bound<'_, PyAny>,
    request: &Bound<'_, PyAny>,
    transition: &Bound<'_, PyAny>,
    task_id: &Bound<'_, PyAny>,
    wait_minutes: Option<&Bound<'_, PyAny>>,
    causation_id: Option<&Bound<'_, PyAny>>,
) -> PyResult<()> {
    let request: QueueRequest = PyField::from_py(request).map_err(|_| Unsupported::new_err("not a queue request"))?;
    // The refusals of a world whose events are not Rust's, as before.
    EventWriter::of(world)?;
    exact_int(&attribute(world, intern!(world.py(), "minute_index"))?)?;
    let wait = match wait_minutes {
        Some(wait) if !wait.is_none() => Some(exact_int(wait)?),
        _ => None,
    };
    let cause = match causation_id {
        Some(cause) if !cause.is_none() => Some(exact_str(cause)?),
        _ => None,
    };
    let (transition, task_id) = (exact_str(transition)?, exact_str(task_id)?);
    let held = held(world)?;
    let geo = Geo::of(world)?;
    let written = with_world(world, |world| {
        let recording = core::plan_record(world, &held, &geo, &request, transition, task_id, wait, cause)?;
        core::write_record(world, recording, cause)
    })?;
    write_result(world.py(), written)
}

/// `validate_facility_queue_state`: true only for what the Python rule passes.
#[pyfunction]
fn validate(world: &Bound<'_, PyAny>) -> PyResult<bool> {
    let py = world.py();
    let required = movement_geometry::independent_queues(world)?;
    let table = attribute(world, intern!(py, "model_state"))?;
    let table_dict = exact_dict(&table)?;
    if held_mirror(world)?.is_none() {
        return Ok(!required);
    }
    if !required || table_dict.contains(core::LEGACY_KEY)? {
        return Ok(false);
    }
    let geo = Geo::of(world)?;
    let movement = movement_held(world)?;
    with_world(world, |world| core::validate(world, &Held::Held, &geo, &movement))?.or_py()
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(independent_queues_required, m)?)?;
    m.add_function(wrap_pyfunction!(is_legacy_queue_checkpoint, m)?)?;
    m.add_function(wrap_pyfunction!(facility_has_independent_queues, m)?)?;
    m.add_function(wrap_pyfunction!(create_facility_queue_state, m)?)?;
    m.add_function(wrap_pyfunction!(choose_queue_id, m)?)?;
    m.add_function(wrap_pyfunction!(queue_ids_for_facility, m)?)?;
    m.add_function(wrap_pyfunction!(queue_head_agent_ids, m)?)?;
    m.add_function(wrap_pyfunction!(request_for_task, m)?)?;
    m.add_function(wrap_pyfunction!(request_for_agent, m)?)?;
    m.add_function(wrap_pyfunction!(queue_agent_ids, m)?)?;
    m.add_function(wrap_pyfunction!(enqueue, m)?)?;
    m.add_function(wrap_pyfunction!(start_service, m)?)?;
    m.add_function(wrap_pyfunction!(finish_service, m)?)?;
    m.add_function(wrap_pyfunction!(mark_stall, m)?)?;
    m.add_function(wrap_pyfunction!(record_queue_transition, m)?)?;
    m.add_function(wrap_pyfunction!(validate, m)?)?;
    Ok(())
}
