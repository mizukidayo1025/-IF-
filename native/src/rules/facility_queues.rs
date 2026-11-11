//! `domain/facility_queues.py`: the independent FIFO of each service point.
//!
//! Each kernel reads the row of `facility_queues_v1`, plans, and writes through the tree, whose
//! writes reach the row; while the namespace is detached (`core/state_mirror.py`), it writes the
//! row itself, the same change (`detached_mirror`). Where the Python rule raised its own ValueError, the kernel raises it with
//! the same text, before any write; what else it doesn't take says Unsupported before any write
//! (docs/rust-port/lanes/m6-overlay.md). The pydantic contract of the receipt stays Python's:
//! `transition_row` calls it where the Python rule did.

use std::collections::HashSet;
use std::sync::Arc;

use indexmap::IndexMap;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PySet, PyTuple};
use sheltersim_core::config::population::AccessibilityNeeds;

use crate::core::configs;
use crate::core::errors;
use crate::core::row::{attribute, exact_dict, exact_int, exact_str, unsupported};
use crate::core::value::{V, VPy};
use crate::core::world_core::NamespaceMirror;
use crate::rules::movement::mobility_text;
use crate::rules::movement_geometry::{self, Geo, GeoPy, ServicePoint};
use crate::state::entities::{entities_of, Activity, EntityStore};
use crate::state::events::{EventKind, EventRow, EventStatus, EventWriter};
use crate::state::facility_queues::{held_mirror, QueueRequest, QueueRow, QueuesRow};
use crate::state::movement::MovementRow;
use crate::Unsupported;

pub(crate) const STATE_KEY: &str = "facility_queues_v1";
const LEGACY_KEY: &str = "facility_queues_legacy_mode_v1";
const POLICY_VERSION: &str = "independent-facility-queues-v1";
const SOURCE_ID: &str = "ASM-FACILITY-QUEUES-V1";
const MAX_REQUESTS: usize = 100_000;
const NOT_INSTALLED_OR_TAKEN: &str = "service queue is not installed or request ID is already in use";

// ---- Reading -------------------------------------------------------------------------------------

/// Run f on the row, or give None for a world without the namespace.
pub(crate) fn with_queues<R>(world: &Bound<'_, PyAny>, f: impl FnOnce(&mut QueuesRow) -> PyResult<R>) -> PyResult<Option<R>> {
    match held_mirror(world)? {
        Some(mirror) => Ok(Some(NamespaceMirror::with_row::<QueuesRow, _>(&mirror, f)?)),
        None => Ok(None),
    }
}

/// `facility_has_independent_queues`.
pub(crate) fn has_independent(world: &Bound<'_, PyAny>, facility_id: &str) -> PyResult<bool> {
    Ok(with_queues(world, |row| Ok(row.queues.values().any(|queue| &*queue.facility_id == facility_id)))?.unwrap_or(false))
}

/// `queue_agent_ids`.
pub(crate) fn members_of(row: &QueuesRow, queue_id: &str) -> PyResult<Vec<Arc<str>>> {
    let Some(queue) = row.queues.get(queue_id) else {
        return Ok(Vec::new());
    };
    queue
        .queued_request_ids
        .iter()
        .map(|id| match row.requests.get(&**id) {
            Some(request) => Ok(request.agent_id.clone()),
            None => unsupported("a queue lists an unknown request"),
        })
        .collect()
}

/// `request_for_agent`.
pub(crate) fn request_of<'a>(row: &'a QueuesRow, agent_id: &str, queue_id: Option<&str>) -> Option<&'a QueueRequest> {
    row.requests.values().find(|request| &*request.agent_id == agent_id && queue_id.is_none_or(|queue| &*request.queue_id == queue))
}

/// `request_for_task`.
pub(crate) fn request_of_task<'a>(row: &'a QueuesRow, task_id: &str) -> Option<&'a QueueRequest> {
    row.requests.get(task_id).or_else(|| row.requests.values().find(|request| &*request.active_task_id == task_id))
}

/// `point_for_queue`.
pub(crate) fn point_of<'a>(geo: &'a Geo, queue_id: &str) -> Option<&'a ServicePoint> {
    geo.layout.as_ref()?.signed_service_points.iter().find(|point| &*point.queue_id == queue_id)
}

/// The mirror of the namespace while the world holds it detached, its row alone holding it: the
/// writes below then change the row, as the tree's writes would have through their forwarding.
/// None while it has a tree.
fn detached_mirror<'py>(world: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, NamespaceMirror>>> {
    let table = attribute(world, intern!(world.py(), "model_state"))?;
    let held = exact_dict(&table)?.get_item(STATE_KEY)?;
    match held {
        Some(held) if crate::core::tracked::is_detached(&held) => held_mirror(world),
        _ => Ok(None),
    }
}

/// Change the row of a detached namespace: `with_row_tree`, whose tree is None, so that the
/// generation moves as for a write through the tree.
fn edit_row<R>(mirror: &Bound<'_, NamespaceMirror>, f: impl FnOnce(&mut QueuesRow) -> PyResult<R>) -> PyResult<R> {
    NamespaceMirror::with_row_tree::<QueuesRow, _>(mirror, |row, _| f(row))
}

/// The tree's node of a request, for a rule that hands it to Python. A detached namespace gives
/// a new document of the row's record: read it, never write.
fn request_node<'py>(world: &Bound<'py, PyAny>, request_id: &str) -> PyResult<Option<Bound<'py, PyAny>>> {
    if let Some(mirror) = detached_mirror(world)? {
        let py = world.py();
        return NamespaceMirror::with_row::<QueuesRow, _>(&mirror, |row| {
            row.requests.get(request_id).map(|request| crate::core::row::PyField::to_py(request, py)).transpose()
        });
    }
    let table = attribute(world, intern!(world.py(), "model_state"))?;
    let Some(state) = exact_dict(&table)?.get_item(STATE_KEY)? else {
        return Ok(None);
    };
    let Some(requests) = exact_dict(&state)?.get_item(intern!(world.py(), "requests"))? else {
        return unsupported("the queues have no requests");
    };
    exact_dict(&requests)?.get_item(request_id)
}

fn queue_node<'py>(world: &Bound<'py, PyAny>, queue_id: &str) -> PyResult<Bound<'py, PyAny>> {
    let py = world.py();
    let table = attribute(world, intern!(py, "model_state"))?;
    let state = exact_dict(&table)?.get_item(STATE_KEY)?.ok_or_else(|| Unsupported::new_err("no queue state"))?;
    let queues = exact_dict(&state)?.get_item(intern!(py, "queues"))?.ok_or_else(|| Unsupported::new_err("no queues"))?;
    exact_dict(&queues)?.get_item(queue_id)?.ok_or_else(|| Unsupported::new_err("no such queue"))
}

// ---- Writing ------------------------------------------------------------------------------------

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
    let py = world.py();
    if let Some(mirror) = detached_mirror(world)? {
        return edit_row(&mirror, |row| {
            if !row.queues.contains_key(&*point.queue_id) {
                return unsupported("no such queue");
            }
            let id: Arc<str> = Arc::from(request_id);
            let record = QueueRequest {
                request_id: id.clone(),
                queue_id: point.queue_id.clone(),
                service_point_id: point.id.clone(),
                facility_id: point.facility_id.clone(),
                agent_id: Arc::from(agent_id),
                service_kind: Arc::from(service_kind),
                requested_minute,
                status: Arc::from("queued"),
                queue_task_id: Arc::from(task_id),
                active_task_id: Arc::from(task_id),
            };
            match row.requests.get_mut(request_id) {
                Some(slot) => *slot = record,
                None => {
                    row.requests.insert(id.clone(), record);
                }
            }
            row.queues.get_mut(&*point.queue_id).expect("checked above").queued_request_ids.push(id);
            Ok(())
        });
    }
    let table = attribute(world, intern!(py, "model_state"))?;
    let state = exact_dict(&table)?.get_item(STATE_KEY)?.ok_or_else(|| Unsupported::new_err("no queue state"))?;
    let requests = exact_dict(&state)?.get_item(intern!(py, "requests"))?.ok_or_else(|| Unsupported::new_err("no requests"))?;
    let record = PyDict::new(py);
    record.set_item("request_id", request_id)?;
    record.set_item("queue_id", &*point.queue_id)?;
    record.set_item("service_point_id", &*point.id)?;
    record.set_item("facility_id", &*point.facility_id)?;
    record.set_item("agent_id", agent_id)?;
    record.set_item("service_kind", service_kind)?;
    record.set_item("requested_minute", requested_minute)?;
    record.set_item("status", "queued")?;
    record.set_item("queue_task_id", task_id)?;
    record.set_item("active_task_id", task_id)?;
    requests.as_any().set_item(request_id, record)?;
    let queue = queue_node(world, &point.queue_id)?;
    queue.as_any().get_item(intern!(py, "queued_request_ids"))?.call_method1(intern!(py, "append"), (request_id,))?;
    Ok(())
}

/// `start_service`, the request being one the queue holds as queued.
pub(crate) fn write_start<'py>(world: &Bound<'py, PyAny>, request_id: &str, queue_id: &str, active_task_id: &str) -> PyResult<Bound<'py, PyAny>> {
    let py = world.py();
    if let Some(mirror) = detached_mirror(world)? {
        edit_row(&mirror, |row| {
            if !row.requests.contains_key(request_id) {
                return unsupported("no such request");
            }
            let Some(queue) = row.queues.get_mut(queue_id) else {
                return unsupported("no such queue");
            };
            // `list.remove`, which the callers have checked holds it.
            let Some(place) = queue.queued_request_ids.iter().position(|id| &**id == request_id) else {
                return Err(pyo3::exceptions::PyValueError::new_err("list.remove(x): x not in list"));
            };
            let id = queue.queued_request_ids.remove(place);
            queue.active_request_ids.push(id);
            queue.last_stall = None;
            let request = row.requests.get_mut(request_id).expect("checked above");
            request.status = Arc::from("active");
            request.active_task_id = Arc::from(active_task_id);
            Ok(())
        })?;
        return request_node(world, request_id)?.ok_or_else(|| Unsupported::new_err("no such request"));
    }
    let request = request_node(world, request_id)?.ok_or_else(|| Unsupported::new_err("no such request"))?;
    let queue = queue_node(world, queue_id)?;
    queue.get_item(intern!(py, "queued_request_ids"))?.call_method1(intern!(py, "remove"), (request_id,))?;
    queue.get_item(intern!(py, "active_request_ids"))?.call_method1(intern!(py, "append"), (request_id,))?;
    request.set_item(intern!(py, "status"), "active")?;
    request.set_item(intern!(py, "active_task_id"), active_task_id)?;
    queue.set_item(intern!(py, "last_stall"), py.None())?;
    Ok(request)
}

/// `finish_service`'s writes, for the request `request_of_task` found.
pub(crate) fn write_finish(world: &Bound<'_, PyAny>, request: &QueueRequest) -> PyResult<()> {
    let py = world.py();
    if let Some(mirror) = detached_mirror(world)? {
        return edit_row(&mirror, |row| {
            let Some(queue) = row.queues.get_mut(&*request.queue_id) else {
                return unsupported("no such queue");
            };
            let list = if &*request.status == "queued" { &mut queue.queued_request_ids } else { &mut queue.active_request_ids };
            if let Some(place) = list.iter().position(|id| *id == request.request_id) {
                list.remove(place);
            }
            queue.last_stall = None;
            row.requests.shift_remove(&*request.request_id);
            Ok(())
        });
    }
    let queue = queue_node(world, &request.queue_id)?;
    let name = if &*request.status == "queued" { intern!(py, "queued_request_ids") } else { intern!(py, "active_request_ids") };
    let list = queue.get_item(name)?;
    if list.contains(&*request.request_id)? {
        list.call_method1(intern!(py, "remove"), (&*request.request_id,))?;
    }
    let table = attribute(world, intern!(py, "model_state"))?;
    let state = exact_dict(&table)?.get_item(STATE_KEY)?.ok_or_else(|| Unsupported::new_err("no queue state"))?;
    state.get_item(intern!(py, "requests"))?.call_method1(intern!(py, "pop"), (&*request.request_id, py.None()))?;
    queue.set_item(intern!(py, "last_stall"), py.None())?;
    Ok(())
}

/// `queued.remove(request_id)` and `queued.insert(0, request_id)` of a request just queued: it
/// goes back to the head of its queue. `table` is `model_state`.
pub(crate) fn write_to_head(world: &Bound<'_, PyAny>, table: &Bound<'_, PyAny>, queue_id: &str, request_id: &str) -> PyResult<()> {
    let py = world.py();
    if let Some(mirror) = detached_mirror(world)? {
        return edit_row(&mirror, |row| {
            let Some(queue) = row.queues.get_mut(queue_id) else {
                return Err(pyo3::exceptions::PyKeyError::new_err(queue_id.to_owned()));
            };
            let Some(place) = queue.queued_request_ids.iter().position(|id| &**id == request_id) else {
                return Err(pyo3::exceptions::PyValueError::new_err("list.remove(x): x not in list"));
            };
            let id = queue.queued_request_ids.remove(place);
            queue.queued_request_ids.insert(0, id);
            Ok(())
        });
    }
    let table = exact_dict(table)?;
    let queues = table.get_item(intern!(py, "facility_queues_v1"))?.ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("the queues left"))?;
    let queued = queues.get_item("queues")?.get_item(queue_id)?.get_item("queued_request_ids")?;
    queued.call_method1(intern!(py, "remove"), (request_id,))?;
    queued.call_method1(intern!(py, "insert"), (0, request_id))?;
    Ok(())
}

/// The facility of a queue, read from a detached namespace's row: None while the namespace has
/// a tree (the caller reads it), Some(None) without the queue.
pub(crate) fn detached_queue_facility(world: &Bound<'_, PyAny>, queue_id: &str) -> PyResult<Option<Option<Arc<str>>>> {
    let Some(mirror) = detached_mirror(world)? else {
        return Ok(None);
    };
    NamespaceMirror::with_row::<QueuesRow, _>(&mirror, |row| Ok(Some(row.queues.get(queue_id).map(|queue| queue.facility_id.clone()))))
}

/// `{"requests": {request_id: ...}, "queues": {queue_id: ...}}` of a detached namespace's row,
/// with only these records, as new documents: for a rule that reads them with `get`. None while
/// the namespace has a tree.
pub(crate) fn detached_records<'py>(world: &Bound<'py, PyAny>, request_id: Option<&str>, queue_id: Option<&str>) -> PyResult<Option<Bound<'py, PyAny>>> {
    let Some(mirror) = detached_mirror(world)? else {
        return Ok(None);
    };
    let py = world.py();
    NamespaceMirror::with_row::<QueuesRow, _>(&mirror, |row| {
        let requests = PyDict::new(py);
        if let Some((id, request)) = request_id.and_then(|id| row.requests.get_key_value(id)) {
            requests.set_item(&**id, crate::core::row::PyField::to_py(request, py)?)?;
        }
        let queues = PyDict::new(py);
        if let Some((id, queue)) = queue_id.and_then(|id| row.queues.get_key_value(id)) {
            queues.set_item(&**id, crate::core::row::PyField::to_py(queue, py)?)?;
        }
        let state = PyDict::new(py);
        state.set_item("requests", requests)?;
        state.set_item("queues", queues)?;
        Ok(Some(state.into_any()))
    })
}

/// `mark_stall`.
pub(crate) fn write_stall(world: &Bound<'_, PyAny>, queue_id: &str, request_id: &str, reason: &str) -> PyResult<()> {
    let py = world.py();
    let stall = PyDict::new(py);
    stall.set_item("minute", attribute(world, intern!(py, "minute_index"))?)?;
    stall.set_item("request_id", request_id)?;
    stall.set_item("reason", reason)?;
    if let Some(mirror) = detached_mirror(world)? {
        let stall: crate::state::facility_queues::QueueStall =
            crate::core::row::PyField::from_py(stall.as_any()).map_err(|_| Unsupported::new_err("the stall does not fit the row"))?;
        return edit_row(&mirror, |row| match row.queues.get_mut(queue_id) {
            Some(queue) => {
                queue.last_stall = Some(stall);
                Ok(())
            }
            None => unsupported("no such queue"),
        });
    }
    queue_node(world, queue_id)?.set_item(intern!(py, "last_stall"), stall)
}

fn text(value: &str) -> V {
    V::Str(Arc::from(value))
}

/// The record of `record_queue_transition`, changing nothing: `sequence` is the one it will take.
/// The contract of the receipt is Python's, and what it refuses it refuses there.
#[allow(clippy::too_many_arguments)]
pub(crate) fn transition_row(
    py: Python<'_>,
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
    let event_id: Arc<str> = Arc::from(format!("facility-queue-transition-{sequence}"));
    {
        if writer.is_realized(&event_id, Some(sequence))? {
            return unsupported("the record's identity is taken");
        }
    }
    let option_int = |value: Option<i64>| value.map_or(V::Null, V::Int);
    let mut payload: IndexMap<Arc<str>, V> = IndexMap::with_capacity(17);
    payload.insert(Arc::from("schema_version"), V::Int(1));
    payload.insert(Arc::from("record_type"), text("operations"));
    payload.insert(Arc::from("event_type"), text("facility_queue_transition"));
    payload.insert(Arc::from("event_id"), V::Str(event_id.clone()));
    payload.insert(Arc::from("request_id"), V::Str(request.request_id.clone()));
    payload.insert(Arc::from("queue_id"), V::Str(request.queue_id.clone()));
    payload.insert(Arc::from("service_point_id"), V::Str(request.service_point_id.clone()));
    payload.insert(Arc::from("facility_id"), V::Str(request.facility_id.clone()));
    payload.insert(Arc::from("agent_id"), V::Str(request.agent_id.clone()));
    payload.insert(Arc::from("service_kind"), V::Str(request.service_kind.clone()));
    payload.insert(Arc::from("transition"), text(transition));
    payload.insert(Arc::from("minute"), V::Int(minute));
    payload.insert(Arc::from("requested_minute"), V::Int(request.requested_minute));
    payload.insert(Arc::from("task_id"), text(task_id));
    payload.insert(Arc::from("queue_position"), option_int(queue_position));
    payload.insert(Arc::from("service_capacity_concurrent"), option_int(capacity));
    payload.insert(Arc::from("wait_minutes"), option_int(wait_minutes));
    payload.insert(Arc::from("causation_id"), causation_id.map_or(V::Null, text));
    py.import(intern!(py, "sheltersim.contracts.operations"))?
        .getattr(intern!(py, "FacilityQueueTransitionReceipt"))?
        .call_method1(intern!(py, "model_validate"), (V::Map(payload.clone()).to_py(py)?,))
        .map_err(|_| Unsupported::new_err("the receipt's contract refuses it"))?;
    let mut provenance: IndexMap<Arc<str>, V> = IndexMap::with_capacity(3);
    provenance.insert(Arc::from("source_type"), text("assumption"));
    provenance.insert(Arc::from("source_id"), text(SOURCE_ID));
    provenance.insert(Arc::from("policy_version"), text(POLICY_VERSION));
    EventRow::new(
        event_id,
        EventKind::Endogenous,
        minute,
        0,
        sequence,
        Arc::from("facility_queue_transition"),
        payload,
        Some(Arc::from(POLICY_VERSION)),
        causation_id.map(Arc::from),
        provenance,
    )
    .ok_or_else(|| Unsupported::new_err("the record's values are not what an event takes"))
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
    let Some(mode) = table.get_item(LEGACY_KEY)? else {
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
    let py = world.py();
    if !movement_geometry::independent_queues(world)? {
        return Err(errors::value_error("facility_queues_v1 requires independent explicit service points"));
    }
    let geo = Geo::of(world)?;
    let Some(layout) = &geo.layout else {
        return unsupported("no floor plan");
    };
    let mut points: Vec<&ServicePoint> = layout.signed_service_points.iter().collect();
    points.sort_by(|first, second| first.queue_id.as_bytes().cmp(second.queue_id.as_bytes()));
    let queues = PyDict::new(py);
    for point in points {
        let queue = PyDict::new(py);
        queue.set_item("facility_id", &*point.facility_id)?;
        queue.set_item("service_point_id", &*point.id)?;
        queue.set_item("queued_request_ids", PyList::empty(py))?;
        queue.set_item("active_request_ids", PyList::empty(py))?;
        queue.set_item("last_stall", py.None())?;
        queues.set_item(&*point.queue_id, queue)?;
    }
    let state = PyDict::new(py);
    state.set_item("schema_version", 1)?;
    state.set_item("policy_version", POLICY_VERSION)?;
    state.set_item("queues", queues)?;
    state.set_item("requests", PyDict::new(py))?;
    Ok(state)
}

/// `choose_queue_id`.
#[pyfunction]
pub(crate) fn choose_queue_id(world: &Bound<'_, PyAny>, facility_id: &Bound<'_, PyAny>, agent_id: &Bound<'_, PyAny>) -> PyResult<Option<String>> {
    let (facility_id, agent_id) = (exact_str(facility_id)?, exact_str(agent_id)?);
    let Some(loads) = with_queues(world, |row| {
        Ok(row.queues.iter().map(|(id, queue)| (id.clone(), (queue.queued_request_ids.len() + queue.active_request_ids.len()) as i64)).collect::<Vec<_>>())
    })?
    else {
        return Ok(None);
    };
    // The population's needs (`config::population`), kept for the stamp of `model_configs`.
    let needs = configs::kept_value(world, |configs| &configs.accessibility_needs, AccessibilityNeeds::parse)?;
    let mut tags: HashSet<String> = configs::read(needs.of(agent_id))?.iter().map(|need| need.to_string()).collect();
    let store = entities_of(world)?;
    EntityStore::with_rows(&store, |rows| {
        if let Some(agent) = rows.agents.get(agent_id) {
            tags.insert(mobility_text(agent).to_owned());
        }
        Ok(())
    })?;
    let geo = Geo::of(world)?;
    let Some(layout) = &geo.layout else {
        return Ok(None);
    };
    let mut best: Option<(i64, &Arc<str>)> = None;
    for point in &layout.signed_service_points {
        if &*point.facility_id != facility_id {
            continue;
        }
        if &*point.accessibility == "restricted" && !point.required_access_tags.iter().all(|tag| tags.contains(&**tag)) {
            continue;
        }
        let Some((_, load)) = loads.iter().find(|(id, _)| *id == point.queue_id) else {
            return unsupported("a signed point has no queue");
        };
        if best.is_none_or(|(known, known_id)| (*load, point.queue_id.as_bytes()) < (known, known_id.as_bytes())) {
            best = Some((*load, &point.queue_id));
        }
    }
    Ok(best.map(|(_, id)| id.to_string()))
}

/// `queue_ids_for_facility`.
#[pyfunction]
pub(crate) fn queue_ids_for_facility<'py>(world: &Bound<'py, PyAny>, facility_id: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyTuple>> {
    let facility_id = exact_str(facility_id)?;
    let geo = Geo::of(world)?;
    let mut ids: Vec<&str> = geo
        .layout
        .iter()
        .flat_map(|layout| layout.signed_service_points.iter())
        .filter(|point| &*point.facility_id == facility_id)
        .map(|point| &*point.queue_id)
        .collect();
    ids.sort_unstable_by(|first, second| first.as_bytes().cmp(second.as_bytes()));
    PyTuple::new(world.py(), ids)
}

/// `queue_head_agent_ids`.
#[pyfunction]
fn queue_head_agent_ids<'py>(world: &Bound<'py, PyAny>, facility_id: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PySet>> {
    let py = world.py();
    let facility_id = exact_str(facility_id)?;
    let heads = independent_heads(world, facility_id)?;
    let heads = match heads {
        Some(heads) => heads,
        None => {
            let store = entities_of(world)?;
            EntityStore::with_rows(&store, |rows| {
                let Some(facility) = rows.facilities.get(facility_id) else {
                    return unsupported("no such facility");
                };
                Ok(facility.resource_queue_agent_ids.first().cloned().into_iter().collect())
            })?
        }
    };
    PySet::new(py, heads.iter().map(|id| &**id))
}

/// The heads of the facility's independent queues, or None for a facility without any.
pub(crate) fn independent_heads(world: &Bound<'_, PyAny>, facility_id: &str) -> PyResult<Option<Vec<Arc<str>>>> {
    let found = with_queues(world, |row| {
        if !row.queues.values().any(|queue| &*queue.facility_id == facility_id) {
            return Ok(None);
        }
        let mut heads = Vec::new();
        for queue in row.queues.values() {
            if &*queue.facility_id != facility_id {
                continue;
            }
            if let Some(head) = queue.queued_request_ids.first() {
                let Some(request) = row.requests.get(&**head) else {
                    return unsupported("a queue lists an unknown request");
                };
                heads.push(request.agent_id.clone());
            }
        }
        Ok(Some(heads))
    })?;
    Ok(found.flatten())
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
    let found = with_queues(world, |row| Ok(request_of(row, agent_id, queue_id).map(|request| request.request_id.clone())))?.flatten();
    match found {
        Some(id) => request_node(world, &id),
        None => Ok(None),
    }
}

/// `queue_agent_ids`.
#[pyfunction]
fn queue_agent_ids<'py>(world: &Bound<'py, PyAny>, queue_id: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyList>> {
    let queue_id = exact_str(queue_id)?;
    let members = with_queues(world, |row| members_of(row, queue_id))?.unwrap_or_default();
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
    let geo = Geo::of(world)?;
    let found = with_queues(world, |row| Ok((row.requests.contains_key(request_id), row.queues.contains_key(queue_id))))?;
    // Not installed, no signed point of that queue, or the ID taken: the Python rule's one text.
    let (Some((false, has_queue)), Some(point)) = (found, point_of(&geo, queue_id)) else {
        return Err(errors::value_error(NOT_INSTALLED_OR_TAKEN));
    };
    if !has_queue {
        // A signed point whose queue the state lacks, which its check turns down.
        return unsupported("a signed point has no queue");
    }
    write_enqueue(world, point, request_id, exact_str(task_id)?, exact_str(agent_id)?, exact_str(service_kind)?, exact_int(requested_minute)?)
}

/// `start_service`.
#[pyfunction]
fn start_service<'py>(world: &Bound<'py, PyAny>, request_id: &Bound<'py, PyAny>, active_task_id: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let request_text = exact_str(request_id)?;
    let queue_id = with_queues(world, |row| {
        let Some(request) = row.requests.get(request_text) else {
            return unsupported("no such request");
        };
        let Some(queue) = row.queues.get(&*request.queue_id) else {
            return unsupported("no such queue");
        };
        if !queue.queued_request_ids.iter().any(|id| &**id == request_text) {
            return unsupported("the request is not queued");
        }
        Ok(request.queue_id.clone())
    })?;
    let Some(queue_id) = queue_id else {
        return unsupported("no queue state");
    };
    write_start(world, request_text, &queue_id, exact_str(active_task_id)?)
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
    let queue_text = exact_str(queue_id)?;
    let known = with_queues(world, |row| Ok(row.queues.contains_key(queue_text)))?;
    if known != Some(true) {
        return Ok(());
    }
    write_stall(world, queue_text, exact_str(request_id)?, exact_str(reason)?)
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
    let py = world.py();
    let request: QueueRequest = crate::core::row::PyField::from_py(request).map_err(|_| Unsupported::new_err("not a queue request"))?;
    let writer = EventWriter::of(world)?;
    let minute = exact_int(&attribute(world, intern!(py, "minute_index"))?)?;
    let wait = match wait_minutes {
        Some(wait) if !wait.is_none() => Some(exact_int(wait)?),
        _ => None,
    };
    let cause = match causation_id {
        Some(cause) if !cause.is_none() => Some(exact_str(cause)?),
        _ => None,
    };
    let position = with_queues(world, |row| {
        let Some(queue) = row.queues.get(&*request.queue_id) else {
            return unsupported("no such queue");
        };
        Ok(position_in(queue, &request.request_id))
    })?
    .ok_or_else(|| Unsupported::new_err("no queue state"))?;
    let geo = Geo::of(world)?;
    let capacity = point_of(&geo, &request.queue_id).map(|point| point.service_capacity);
    let sequence = writer.next_sequence()?;
    let row = transition_row(py, &writer, &request, exact_str(transition)?, exact_str(task_id)?, wait, cause, position, capacity, sequence, minute)?;
    write_transition(&writer, &row, minute)
}

/// `queued_ids.index(request_id) + 1`, or None.
pub(crate) fn position_in(queue: &QueueRow, request_id: &str) -> Option<i64> {
    queue.queued_request_ids.iter().position(|id| &**id == request_id).map(|place| place as i64 + 1)
}

/// `validate_facility_queue_state`: true only for what the Python rule passes.
#[pyfunction]
fn validate(world: &Bound<'_, PyAny>) -> PyResult<bool> {
    let py = world.py();
    let required = movement_geometry::independent_queues(world)?;
    let table = attribute(world, intern!(py, "model_state"))?;
    let table_dict = exact_dict(&table)?;
    let Some(mirror) = held_mirror(world)? else {
        return Ok(!required);
    };
    if !required || table_dict.contains(LEGACY_KEY)? {
        return Ok(false);
    }
    let geo = Geo::of(world)?;
    let Some(layout) = &geo.layout else {
        return Ok(false);
    };
    let minute = exact_int(&attribute(world, intern!(py, "minute_index"))?)?;
    let store = entities_of(world)?;
    let passes = NamespaceMirror::with_row::<QueuesRow, _>(&mirror, |row| {
        if row.schema_version != 1
            || &*row.policy_version != POLICY_VERSION
            || row.requests.len() > MAX_REQUESTS
            || row.queues.len() != layout.signed_service_points.len()
        {
            return Ok(false);
        }
        EntityStore::with_rows(&store, |rows| {
            let mut people: HashSet<&str> = HashSet::new();
            let mut listed = 0;
            for (queue_id, queue) in row.queues.iter() {
                let Some(point) = layout.signed_service_points.iter().find(|point| point.queue_id == *queue_id) else {
                    return Ok(false);
                };
                let queued: HashSet<&str> = queue.queued_request_ids.iter().map(|id| &**id).collect();
                let active: HashSet<&str> = queue.active_request_ids.iter().map(|id| &**id).collect();
                if queue.facility_id != point.facility_id
                    || queue.service_point_id != point.id
                    || queued.len() != queue.queued_request_ids.len()
                    || active.len() != queue.active_request_ids.len()
                    || !queued.is_disjoint(&active)
                    || queue.active_request_ids.len() as i64 > point.service_capacity
                {
                    return Ok(false);
                }
                if let Some(stall) = &queue.last_stall {
                    if stall.minute < 0 || !matches!(&*stall.reason, "capacity" | "staff" | "stock" | "outage" | "route" | "run_end") {
                        return Ok(false);
                    }
                }
                for request_id in queue.queued_request_ids.iter().chain(&queue.active_request_ids) {
                    listed += 1;
                    let is_queued = queued.contains(&**request_id);
                    let Some(request) = row.requests.get(&**request_id) else {
                        return Ok(false);
                    };
                    let (Some(agent), Some(facility)) = (rows.agents.get(&*request.agent_id), rows.facilities.get(&*point.facility_id)) else {
                        return Ok(false);
                    };
                    if request.request_id != *request_id
                        || request.queue_id != *queue_id
                        || request.facility_id != point.facility_id
                        || request.service_point_id != point.id
                        || !matches!(&*request.service_kind, "toilet" | "water" | "meal")
                        || !(0..=minute).contains(&request.requested_minute)
                        || &*request.status != (if is_queued { "queued" } else { "active" })
                        || !people.insert(&request.agent_id)
                    {
                        return Ok(false);
                    }
                    let resource = matches!(&*request.service_kind, "water" | "meal");
                    match &*request.service_kind {
                        "water" if &*facility.kind != "water_station" => return Ok(false),
                        "meal" if &*facility.kind != "meal_distribution" => return Ok(false),
                        _ => {}
                    }
                    let agent_at = agent.target_id.as_deref() == Some(&*point.facility_id);
                    if is_queued {
                        let Some(task) = rows.active_tasks.get(&*request.queue_task_id) else {
                            return Ok(false);
                        };
                        let mirror_list = if resource { &facility.resource_queue_agent_ids } else { &facility.queue_agent_ids };
                        if !matches!(&*task.kind, "toilet_queue" | "resource_service_queue")
                            || task.agent_id != Some(request.agent_id.clone())
                            || task.resource_id.as_deref() != Some(&*point.facility_id)
                            || agent.activity != Activity::Queued
                            || !agent_at
                            || !mirror_list.contains(&request.agent_id)
                        {
                            return Ok(false);
                        }
                    } else {
                        let Some(task) = rows.active_tasks.get(&*request.active_task_id) else {
                            return Ok(false);
                        };
                        let mirror_list = if resource { &facility.resource_active_agent_ids } else { &facility.active_agent_ids };
                        if !matches!(&*task.kind, "toilet_service" | "resource_service_water" | "resource_service_food")
                            || task.agent_id != Some(request.agent_id.clone())
                            || task.resource_id.as_deref() != Some(&*point.facility_id)
                            || agent.activity != Activity::InService
                            || !agent_at
                            || !mirror_list.contains(&request.agent_id)
                        {
                            return Ok(false);
                        }
                        if &*request.service_kind == "toilet" && (!point.required_staff_skills.is_empty() || task.staff_id.is_some()) {
                            let staff = task.staff_id.as_deref().and_then(|id| rows.staff.get(id));
                            let Some(staff) = staff else {
                                return Ok(false);
                            };
                            if point.required_staff_skills.is_empty()
                                || !point.required_staff_skills.iter().all(|skill| staff.skills.0.contains(skill))
                                || staff.zone_id != facility.zone_id
                                || staff.active_task_id.as_deref() != Some(&*task.id)
                                || staff.work_queue.len() != 1
                                || staff.work_queue[0] != task.id
                                || staff.busy_until != task.due_minute
                            {
                                return Ok(false);
                            }
                        }
                    }
                }
            }
            Ok(listed == row.requests.len())
        })
    })?;
    if !passes {
        return Ok(false);
    }
    // The journeys: each with a queue walks for a queued request of that queue.
    if table_dict.contains(crate::rules::movement::KEY)? {
        let movement = crate::rules::movement::mirror_held(world)?;
        return NamespaceMirror::with_row::<MovementRow, _>(&movement, |moving| {
            NamespaceMirror::with_row::<QueuesRow, _>(&mirror, |row| {
                for (agent_id, journey) in moving.journeys.iter() {
                    let Some(queue_id) = &journey.queue_id else {
                        continue;
                    };
                    if request_of(row, agent_id, Some(queue_id)).is_none_or(|request| &*request.status != "queued") {
                        return Ok(false);
                    }
                }
                Ok(true)
            })
        });
    }
    Ok(true)
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
