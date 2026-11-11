//! `domain/resource_service.py`: the glue of the core's kernels (`sheltersim_core::rules::service`,
//! lane c3 of Issue #152). Each entry reads what Python holds (the manager's config, whether the
//! world holds each namespace, the floor plan, the pending reserve events, the walk inputs of a
//! placed world), runs the core on the world borrowed whole (`with_world`), and makes the Python
//! values of what it returns: the receipt, the started event's ID, the world's new `resources` dict
//! of an operation the ledger took, and the ledger's error text with Python's `repr` of a value.
//! The world's validation of `request` goes through the native host (`Host::validate_world`).
//!
//! The helpers at the end (`add_one`, `metrics_of`, `set_agent`, `resource_service_of`,
//! `append_outcome`) are for the rules still in this crate.

use std::sync::Arc;

use indexmap::IndexMap;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::sync::GILOnceCell;
use pyo3::types::{PyBool, PyDict, PyList, PyString, PyTuple};

use sheltersim_core::config::population::AccessibilityNeeds;
use sheltersim_core::config::service::ResourceService;
use sheltersim_core::rules::kit::{self, Held, Kept};
use sheltersim_core::rules::resources::Applied;
use sheltersim_core::rules::service::{self as core, Failure, Manager, Minute, Pending, Request, Requested, Serve, Served};


use crate::core::configs;
use crate::core::errors::{self, to_py_err, OrPy};
use crate::core::row::{attribute, exact_dict, exact_int, exact_str, str_value, unsupported, PyField};
use crate::core::value::{VPy, V};
use crate::core::world_core::{field_held, field_mirror_of, namespace_held, raw_dict_get, with_world, NamespaceMirror, PyWorldCell};
use crate::host::PyHost;
use crate::rules::movement_geometry::{Geo, GeoPy};
use crate::rules::queues::Walking;
use crate::rules::resources::{raised, replace_resources};
use crate::rules::social_network::mapping_get;
use crate::state::entities::{Activity, EntityStore};
use crate::state::events::{write_error, EventRef, EventRow, EventRowPy, EventWriter};
use crate::state::metric_state::MetricRow;
use crate::state::resource_service::{RequestRecord, Stall};
use crate::Unsupported;

const SERVICE_KEY: &str = "resource_service_v1";

/// The section `resource_service` of the world's configs (`config::service`), kept for the stamp
/// of `model_configs`; a configs that is not a dict is refused here.
#[track_caller]
pub(crate) fn resource_service_of(world: &Bound<'_, PyAny>) -> PyResult<Arc<ResourceService>> {
    configs::kept_value(world, |configs| &configs.resource_service, ResourceService::parse)
}

/// What a kernel takes of the manager and of the Python world (`core::Manager`).
struct Inputs<'py> {
    world: Bound<'py, PyAny>,
    config: Arc<IndexMap<Arc<str>, V>>,
    service: Held,
    ledger: Held,
    queues: Held,
    geo: Arc<Geo>,
    metric_state_held: bool,
}

/// The manager's config dict, as values.
fn config_of(config: &Bound<'_, PyAny>) -> PyResult<Arc<IndexMap<Arc<str>, V>>> {
    match <V as VPy>::from_py(config)? {
        V::Map(config) => Ok(Arc::new(config)),
        _ => unsupported("the config is not a dict"),
    }
}

impl<'py> Inputs<'py> {
    /// The manager's world and config.
    fn of(manager: &Bound<'py, PyAny>) -> PyResult<Self> {
        let py = manager.py();
        let world = attribute(manager, intern!(py, "world"))?;
        let config = attribute(manager, intern!(py, "config"))?;
        exact_dict(&config)?;
        exact_dict(&attribute(&world, intern!(py, "model_state"))?)?;
        let config = config_of(&config)?;
        Self::read(world, config)
    }

    /// `ResourceServiceManager(world)`, which holds nothing else: its config is a copy of
    /// `model_configs["resource_service"]`, from the section kept for the stamp.
    fn of_world(world: &Bound<'py, PyAny>) -> PyResult<Self> {
        let py = world.py();
        let service = resource_service_of(world)?;
        let table = attribute(world, intern!(py, "model_state"))?;
        if !service.present {
            return unsupported("no resource service");
        }
        if exact_int(&attribute(world, intern!(py, "schema_version"))?)? != 6
            || !exact_dict(&table)?.contains(intern!(py, SERVICE_KEY))?
            || !exact_dict(&table)?.contains(intern!(py, "resource_ledger_v1"))?
        {
            return unsupported("not an opted-in world");
        }
        let config = match &service.config {
            Some(config) => config.clone(),
            // Not a dict: `dict(raw)` of the Python value, as before.
            None => {
                let configs = attribute(world, intern!(py, "model_configs"))?;
                let raw = exact_dict(&configs)?.get_item(intern!(py, "resource_service"))?.ok_or_else(|| Unsupported::new_err("no resource service"))?;
                let config = PyDict::new(py);
                config.call_method1(intern!(py, "update"), (raw,))?;
                config_of(config.as_any())?
            }
        };
        Self::read(world.clone(), config)
    }

    fn read(world: Bound<'py, PyAny>, config: Arc<IndexMap<Arc<str>, V>>) -> PyResult<Self> {
        let py = world.py();
        Ok(Self {
            service: namespace_held(&world, intern!(py, SERVICE_KEY), "the world holds another service state")?,
            ledger: namespace_held(&world, intern!(py, "resource_ledger_v1"), "the world holds another ledger")?,
            queues: crate::state::facility_queues::held(&world)?,
            geo: Geo::of(&world)?,
            metric_state_held: field_held(&world, intern!(py, "metric_state"))?,
            config,
            world,
        })
    }

    fn manager(&self) -> Manager<'_> {
        Manager {
            config: &self.config,
            service: self.service.clone(),
            ledger: self.ledger.clone(),
            queues: self.queues.clone(),
            geo: &self.geo,
            metric_state_held: self.metric_state_held,
        }
    }
}

/// The `reservation_id` of each pending `resource_reserve` event, in the queue's order: read through
/// the queue's tuple, the same objects the Python rule reads.
fn pending_reserves(world: &Bound<'_, PyAny>) -> PyResult<Vec<Pending>> {
    let writer = EventWriter::of(world)?;
    let py = world.py();
    let pending = writer.queue().call_method0(intern!(py, "pending"))?;
    let pending = pending.downcast_exact::<PyTuple>().map_err(|_| Unsupported::new_err("the pending events are not a tuple"))?;
    let mut found = Vec::new();
    for event in pending.iter() {
        if exact_str(&attribute(&event, intern!(py, "event_type"))?)? != "resource_reserve" {
            continue;
        }
        let payload = attribute(&event, intern!(py, "payload"))?;
        found.push(match mapping_get(&payload, intern!(py, "reservation_id"))? {
            Some(id) => match str_value(&id) {
                Some(id) => Pending::Id(Arc::from(id)),
                None => Pending::Other,
            },
            None => Pending::Missing,
        });
    }
    Ok(found)
}

/// The error of a kernel: the ledger's text inside its rule's error, or what the world or a write
/// said.
fn failed(py: Python<'_>, failure: Failure) -> PyErr {
    match failure {
        Failure::Write(error) => write_error(py, error),
        Failure::Ledger(fault, wrap) => {
            let error = raised(py, sheltersim_core::rules::resources::Fail::Ledger(fault));
            match error.value(py).str() {
                Ok(text) => to_py_err(wrap.error(&text.to_string_lossy())),
                Err(failure) => failure,
            }
        }
    }
}

/// The world's new `resources` dict of an operation the ledger took.
fn applied(world: &Bound<'_, PyAny>, applied: Option<Applied>) -> PyResult<()> {
    match applied {
        Some(applied) => replace_resources(world, applied),
        None => Ok(()),
    }
}

/// The objects of the world's facilities by these IDs.
fn facility_objects<'py>(world: &Bound<'py, PyAny>, ids: impl Iterator<Item = Arc<str>>) -> PyResult<Vec<Bound<'py, PyAny>>> {
    let py = world.py();
    let facilities = attribute(world, intern!(py, "facilities"))?;
    let mut objects = Vec::new();
    for id in ids {
        let Some(facility) = raw_dict_get(&facilities, PyString::new(py, &id).as_any())? else {
            return unsupported("a row has no object");
        };
        objects.push(facility);
    }
    Ok(objects)
}

// ---- arbitrate ----------------------------------------------------------------------------------

/// The facilities `arbitrate` has anything to do at, in ID order, as `(facility, resource_id)`.
#[pyfunction]
pub(crate) fn facilities_to_serve<'py>(manager: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyList>> {
    let py = manager.py();
    let world = attribute(manager, intern!(py, "world"))?;
    exact_dict(&attribute(manager, intern!(py, "config"))?)?;
    exact_dict(&attribute(&world, intern!(py, "model_state"))?)?;
    let queues = crate::state::facility_queues::held(&world)?;
    let due = with_world(&world, |world| core::facilities_to_serve(world, &queues))?.or_py()?;
    let resources: Vec<&'static str> = due.iter().map(|(_, resource)| *resource).collect();
    let objects = facility_objects(&world, due.into_iter().map(|(id, _)| id))?;
    let result = PyList::empty(py);
    for (facility, resource) in objects.into_iter().zip(resources) {
        result.append((facility, resource))?;
    }
    Ok(result)
}

/// One pass of `arbitrate`'s loop after the positions: the started event's ID, or None to stop.
#[pyfunction]
pub(crate) fn serve_head<'py>(
    manager: &Bound<'py, PyAny>,
    facility_id: &Bound<'py, PyAny>,
    resource_id: &Bound<'py, PyAny>,
    minute: &Bound<'py, PyAny>,
) -> PyResult<Option<Bound<'py, PyString>>> {
    Ok(serve_one(manager, facility_id, resource_id, minute, None)?.0)
}

/// One pass of `_arbitrate_independent_facility`'s loop after the positions: the started event's
/// ID or None to stop, and the head's wait as `(agent, reason)`.
#[pyfunction]
pub(crate) fn serve_independent_head<'py>(
    manager: &Bound<'py, PyAny>,
    facility_id: &Bound<'py, PyAny>,
    resource_id: &Bound<'py, PyAny>,
    queue_id: &Bound<'py, PyAny>,
    minute: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyTuple>> {
    let py = manager.py();
    let (started, stall) = serve_one(manager, facility_id, resource_id, minute, Some(exact_str(queue_id)?))?;
    let stall = match stall {
        Some((agent, reason)) => PyTuple::new(py, [PyString::new(py, &agent), PyString::new(py, reason)])?.into_any(),
        None => py.None().into_bound(py),
    };
    let started = match started {
        Some(started) => started.into_any(),
        None => py.None().into_bound(py),
    };
    PyTuple::new(py, [started, stall])
}

#[allow(clippy::type_complexity)]
fn serve_one<'py>(
    manager: &Bound<'py, PyAny>,
    facility_id: &Bound<'py, PyAny>,
    resource_id: &Bound<'py, PyAny>,
    minute: &Bound<'py, PyAny>,
    queue_id: Option<&str>,
) -> PyResult<(Option<Bound<'py, PyString>>, Option<(Arc<str>, &'static str)>)> {
    let py = manager.py();
    let inputs = Inputs::of(manager)?;
    let world = &inputs.world;
    let (facility, resource, minute) = (exact_str(facility_id)?, exact_str(resource_id)?, exact_int(minute)?);
    let pending = pending_reserves(world)?;
    let walking = Walking::read(world, queue_id.is_some())?;
    let walk = walking.as_ref().map(Walking::walk);
    let serve = Serve {
        facility_id: facility,
        resource_id: resource,
        minute,
        queue_id,
        walk: walk.as_ref(),
        placement: walking.as_ref().map(|walking| &*walking.geo),
    };
    match with_world(world, |world| core::serve(world, &inputs.manager(), &serve, &pending))? {
        Ok(Served::Started(event_id, balance)) => {
            applied(world, Some(balance))?;
            Ok((Some(PyString::new(py, &event_id)), None))
        }
        Ok(Served::Stopped(stall)) => Ok((None, stall)),
        Err(failure) => Err(failed(py, failure)),
    }
}

/// `world.model_state["resource_service_v1"]["stall_state"].get(id)`, for the stall a pass of an
/// independent facility starts from: a new record from the row.
pub(crate) fn previous_stall<'py>(world: &Bound<'py, PyAny>, _table: &Bound<'py, PyAny>, id: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = world.py();
    let key = exact_str(id)?;
    let service = namespace_held(world, intern!(py, SERVICE_KEY), "the world holds another service state")?;
    match with_world(world, |world| core::previous_stall(world, &service, key))?.or_py()? {
        Some(stall) => PyField::to_py(&stall, py),
        None => Ok(py.None().into_bound(py)),
    }
}

/// `_record_independent_queue_stall`.
#[pyfunction]
pub(crate) fn record_independent_stall<'py>(
    manager: &Bound<'py, PyAny>,
    facility_id: &Bound<'py, PyAny>,
    stalls: &Bound<'py, PyAny>,
    previous: &Bound<'py, PyAny>,
) -> PyResult<()> {
    let py = manager.py();
    let inputs = Inputs::of(manager)?;
    let facility = exact_str(facility_id)?;
    let waits = stalls.downcast_exact::<PyList>().map_err(|_| Unsupported::new_err("the stalls are not a list"))?;
    if waits.is_empty() {
        return Ok(());
    }
    let mut pairs: Vec<(Arc<str>, Arc<str>)> = Vec::with_capacity(waits.len());
    for wait in waits.iter() {
        let wait = wait.downcast_exact::<PyTuple>().map_err(|_| Unsupported::new_err("a stall is not a tuple"))?;
        pairs.push((Arc::from(exact_str(&wait.get_item(0)?)?), Arc::from(exact_str(&wait.get_item(1)?)?)));
    }
    let previous = if previous.is_none() {
        None
    } else {
        let held = exact_dict(previous)?;
        let (Some(agent), Some(reason)) = (held.get_item(intern!(py, "agent_id"))?, held.get_item(intern!(py, "reason"))?) else {
            return unsupported("a stall record has another shape");
        };
        Some(Stall { agent_id: Arc::from(exact_str(&agent)?), reason: Arc::from(exact_str(&reason)?) })
    };
    with_world(&inputs.world, |world| core::record_independent_stall(world, &inputs.manager(), facility, &pairs, previous.as_ref()))?.or_py()
}

// ---- complete -----------------------------------------------------------------------------------

/// `ResourceServiceManager.complete`.
#[pyfunction]
fn complete<'py>(manager: &Bound<'py, PyAny>, event: &Bound<'py, PyAny>) -> PyResult<()> {
    complete_event(manager, EventRef::Object(event))
}

/// The row of an event a rule reads: Unsupported for an object a row does not hold.
fn row_of(event: &Bound<'_, PyAny>) -> PyResult<EventRow> {
    EventRow::from_event(event)?.ok_or_else(|| Unsupported::new_err("the event is not a row"))
}

/// `complete` of an event's row or object.
pub(crate) fn complete_event<'py>(manager: &Bound<'py, PyAny>, event: EventRef<'_, 'py>) -> PyResult<()> {
    let py = manager.py();
    let inputs = Inputs::of(manager)?;
    let owned;
    let row = match event {
        EventRef::Row(row) => row,
        EventRef::Object(object) => {
            owned = row_of(object)?;
            &owned
        }
    };
    match with_world(&inputs.world, |world| core::complete(world, &inputs.manager(), row))? {
        Ok(balance) => applied(&inputs.world, Some(balance)),
        Err(failure) => Err(failed(py, failure)),
    }
}

// ---- request ------------------------------------------------------------------------------------

/// `ResourceServiceManager.record_request_outcome`.
#[pyfunction]
#[pyo3(signature = (manager, *, agent_id, action, outcome, reason, facility_id, origin_zone_id, detail=None))]
#[allow(clippy::too_many_arguments)]
fn record_request_outcome<'py>(
    manager: &Bound<'py, PyAny>,
    agent_id: &Bound<'py, PyAny>,
    action: &Bound<'py, PyAny>,
    outcome: &Bound<'py, PyAny>,
    reason: &Bound<'py, PyAny>,
    facility_id: &Bound<'py, PyAny>,
    origin_zone_id: &Bound<'py, PyAny>,
    detail: Option<&Bound<'py, PyAny>>,
) -> PyResult<()> {
    let py = manager.py();
    let world = attribute(manager, intern!(py, "world"))?;
    let optional = |value: Option<&Bound<'py, PyAny>>| -> PyResult<Option<Arc<str>>> {
        match value {
            Some(value) if !value.is_none() => Ok(Some(Arc::from(exact_str(value)?))),
            _ => Ok(None),
        }
    };
    let record = RequestRecord {
        minute: exact_int(&attribute(&world, intern!(py, "minute_index"))?)?,
        agent_id: Arc::from(exact_str(agent_id)?),
        action: Arc::from(exact_str(action)?),
        outcome: Arc::from(exact_str(outcome)?),
        reason: Arc::from(exact_str(reason)?),
        facility_id: optional(Some(facility_id))?,
        origin_zone_id: Arc::from(exact_str(origin_zone_id)?),
        detail: optional(detail)?,
    };
    append_outcome(&world, record)
}

fn receipt_class(py: Python<'_>) -> PyResult<&Bound<'_, PyAny>> {
    static CLASS: GILOnceCell<Py<PyAny>> = GILOnceCell::new();
    Ok(CLASS
        .get_or_try_init(py, || -> PyResult<Py<PyAny>> { Ok(py.import("sheltersim.domain.resource_service")?.getattr("ResourceRequestReceipt")?.unbind()) })?
        .bind(py))
}

/// `ResourceRequestReceipt` of the core's receipt, with the caller's objects of the two IDs.
fn receipt<'py>(agent_id: &Bound<'py, PyAny>, facility_id: &Bound<'py, PyAny>, receipt: &core::Receipt) -> PyResult<Bound<'py, PyAny>> {
    let py = agent_id.py();
    receipt_class(py)?.call1((agent_id, facility_id, receipt.resource_id, receipt.requested_at, &*receipt.queue_task_id, receipt.queue_position))
}

/// The population's accessibility needs, kept for the configs' stamp, or the fault the core raises
/// where `choose_queue_id` reads them.
fn needs_of(world: &Bound<'_, PyAny>) -> PyResult<Kept<AccessibilityNeeds>> {
    configs::kept_section(world, |configs| &configs.accessibility_needs, AccessibilityNeeds::parse)
}

/// `ResourceServiceManager.request`, without the world's whole validation: the receipt, or the text
/// of the `ValueError` the Python rule raises when the queue turns the request down, having written
/// what it writes before that (the task counter, when no independent queue takes the person).
pub(crate) fn queue_up_or_refusal<'py>(
    manager: &Bound<'py, PyAny>,
    agent_id: &Bound<'py, PyAny>,
    facility_id: &Bound<'py, PyAny>,
) -> PyResult<Result<Bound<'py, PyAny>, String>> {
    let inputs = Inputs::of(manager)?;
    let world = &inputs.world;
    let needs = needs_of(world)?;
    let pending = pending_reserves(world)?;
    let request = Request { agent_id: exact_str(agent_id)?, facility_id: exact_str(facility_id)?, minute: Minute::Absent, queue_id: None, needs: &needs };
    match with_world(world, |world| core::queue_up(world, &inputs.manager(), &request, &pending))?.or_py()? {
        Requested::Queued(queued) => receipt(agent_id, facility_id, &queued).map(Ok),
        Requested::Refused(text) => Ok(Err(text)),
    }
}

/// `ResourceServiceManager.request`.
#[pyfunction]
#[pyo3(signature = (manager, agent_id, facility_id, *, minute=None, validate_state=None, queue_id=None))]
fn request<'py>(
    manager: &Bound<'py, PyAny>,
    agent_id: &Bound<'py, PyAny>,
    facility_id: &Bound<'py, PyAny>,
    minute: Option<&Bound<'py, PyAny>>,
    validate_state: Option<&Bound<'py, PyAny>>,
    queue_id: Option<&Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = manager.py();
    let validates = !validate_state.is_some_and(|validate| validate.downcast_exact::<PyBool>().is_ok_and(|validate| !validate.is_true()));
    let minute = match minute.filter(|minute| !minute.is_none()) {
        None => Minute::Absent,
        Some(minute) => match errors::int_not_bool(minute)? {
            Some(minute) => Minute::Int(minute),
            None => Minute::NotAnInt,
        },
    };
    let inputs = Inputs::of(manager)?;
    let world = &inputs.world;
    let (agent_name, facility_name) = (exact_str(agent_id)?, exact_str(facility_id)?);
    let queue_name = queue_id.filter(|queue| !queue.is_none()).map(exact_str).transpose()?;
    let needs = needs_of(world)?;
    let pending = pending_reserves(world)?;
    let request = Request { agent_id: agent_name, facility_id: facility_name, minute, queue_id: queue_name, needs: &needs };
    let mut host = PyHost::of_world(world, PyDict::new(py));
    let requested = core::request(&PyWorldCell(world), &mut host, &inputs.manager(), &request, &pending, validates).map_err(|error| error.0)?;
    match requested {
        Requested::Queued(queued) => receipt(agent_id, facility_id, &queued),
        Requested::Refused(text) => Err(errors::marked_value_error(py, &text)),
    }
}

// ---- cancel and interrupt_for_staff -------------------------------------------------------------

/// `ResourceServiceManager.cancel`.
pub(crate) fn cancel_service<'py>(manager: &Bound<'py, PyAny>, event: &Bound<'py, PyAny>) -> PyResult<()> {
    let py = manager.py();
    let inputs = Inputs::of(manager)?;
    let row = row_of(event)?;
    match with_world(&inputs.world, |world| core::cancel(world, &inputs.manager(), &row))? {
        Ok(balance) => applied(&inputs.world, balance),
        Err(failure) => Err(failed(py, failure)),
    }
}

/// `interrupt_for_staff` of the manager's world.
pub(crate) fn interrupt_service<'py>(manager: &Bound<'py, PyAny>, staff_id: &Bound<'py, PyAny>, causation_event: &Bound<'py, PyAny>) -> PyResult<()> {
    interrupt_with(&Inputs::of(manager)?, staff_id, causation_event)
}

/// `ResourceServiceManager(world).interrupt_for_staff(staff_id, causation_event=event)`.
pub(crate) fn interrupt_service_of_world<'py>(world: &Bound<'py, PyAny>, staff_id: &Bound<'py, PyAny>, causation_event: &Bound<'py, PyAny>) -> PyResult<()> {
    interrupt_with(&Inputs::of_world(world)?, staff_id, causation_event)
}

fn interrupt_with<'py>(inputs: &Inputs<'py>, staff_id: &Bound<'py, PyAny>, causation_event: &Bound<'py, PyAny>) -> PyResult<()> {
    let world = &inputs.world;
    let py = world.py();
    let staff = exact_str(staff_id)?;
    let cause: Option<Arc<str>> = str_value(&attribute(causation_event, intern!(py, "event_id"))?).map(Arc::from);
    let pending = pending_reserves(world)?;
    match with_world(world, |world| core::interrupt(world, &inputs.manager(), &pending, staff, cause.as_ref()))? {
        Ok(balance) => applied(world, balance),
        Err(failure) => Err(failed(py, failure)),
    }
}

// ---- Helpers for the rules still in this crate ----------------------------------------------------

/// `metric_state[name] = metric_state.get(name, 0.0) + 1.0`, on the row and the tree
/// (`sheltersim_core::rules::kit::add_one`).
pub(crate) fn add_one(metrics: &Bound<'_, NamespaceMirror>, name: &str) -> PyResult<()> {
    NamespaceMirror::with_row_tree::<MetricRow, _>(metrics, |row, tree| kit::add_one(row, tree, name))
}

pub(crate) fn metrics_of<'py>(world: &Bound<'py, PyAny>) -> PyResult<Bound<'py, NamespaceMirror>> {
    let metrics = field_mirror_of(world, intern!(world.py(), "metric_state"))?;
    NamespaceMirror::with_row::<MetricRow, _>(&metrics, |_| Ok(()))?;
    Ok(metrics)
}

/// A person's activity and target, on the row: the object follows when the borrow ends
/// (`sheltersim_core::rules::kit::set_agent`).
pub(crate) fn set_agent<'py>(
    _world: &Bound<'py, PyAny>,
    store: &Bound<'py, EntityStore>,
    agent_id: &Bound<'py, PyAny>,
    activity: Activity,
    target: Option<&Bound<'py, PyAny>>,
) -> PyResult<()> {
    let id = exact_str(agent_id)?;
    let target_text = target.map(exact_str).transpose()?;
    EntityStore::with_rows(store, |rows| kit::set_agent(rows, id, activity, target_text).or_py())
}

/// `record_request_outcome`: the record at the end of the service's `request_history`, on the row
/// (an attached tree follows it).
pub(crate) fn append_outcome(world: &Bound<'_, PyAny>, record: RequestRecord) -> PyResult<()> {
    let service = namespace_held(world, intern!(world.py(), SERVICE_KEY), "the world holds another service state")?;
    with_world(world, |world| core::append_outcome(world, &service, record))?.or_py()
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(facilities_to_serve, m)?)?;
    m.add_function(wrap_pyfunction!(serve_head, m)?)?;
    m.add_function(wrap_pyfunction!(serve_independent_head, m)?)?;
    m.add_function(wrap_pyfunction!(record_independent_stall, m)?)?;
    m.add_function(wrap_pyfunction!(complete, m)?)?;
    m.add_function(wrap_pyfunction!(record_request_outcome, m)?)?;
    m.add_function(wrap_pyfunction!(request, m)?)?;
    Ok(())
}
