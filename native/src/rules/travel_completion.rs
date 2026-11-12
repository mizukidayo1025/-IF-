//! `AgentTravelCompletionHandler` of `events/handlers.py`: the glue of the core's kernels
//! (`sheltersim_core::rules::travel_completion`, lane c7 of Issue #152). The core reads what the
//! world holds, the floor plan and the movement section through the native host
//! (`Gathered::read`, lane P9-1a); `apply_event` adds the lifecycle's inputs for a relocation and
//! whether the handler has a resource service, and runs the core's checks and writes on the world
//! borrowed whole.
//!
//! What a walk to a toilet or a resource leads to (`_after_arrival`) is the core's `arrive` (lane
//! P9-1a): the walk in the room or its rejection, the world's validation around a toilet request,
//! and the record of a refused resource request. The requests themselves go to the handler's
//! managers here (`toilet::queue_up` and `service::queue_up_or_refusal`, whose inputs this crate
//! reads, or a method a test set on a manager). On the dispatch path, a manager with its own
//! `request` makes the whole event the host's (`Host::apply_foreign`, H4). A rule's text raised
//! before any change is marked (`raised_before_writing`), for the dispatcher.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use pyo3::exceptions::PyRuntimeError;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyString};

use sheltersim_core::host::{Foreign, Host};
use sheltersim_core::rules::kit;
use sheltersim_core::rules::travel_completion::{self as core, Applied, Managers};
use sheltersim_core::value::V;
use sheltersim_core::world::WorldCell;

use crate::core::errors::{self, to_py_err, OrPy};
use crate::core::row::{attribute, exact_dict, exact_int, exact_str, str_value, unsupported};
use crate::core::world_core::{raw_dict_get, PyWorldCell};
use crate::host::{HostError, PyHost};
use crate::rules::population_lifecycle::LifecycleIn;
use crate::rules::{service, toilet};
use crate::state::events::{write_result, EventRef, EventRow, EventRowPy, EventStatus, EventWriter};
use crate::Unsupported;

static ARRIVALS_RAN: AtomicU64 = AtomicU64::new(0);
static ARRIVALS_REFUSED: AtomicU64 = AtomicU64::new(0);
static ARRIVAL_REFUSALS: Mutex<BTreeMap<String, u64>> = Mutex::new(BTreeMap::new());

/// The native host of an entry.
fn host_of<'py>(world: &Bound<'py, PyAny>) -> PyHost<'py> {
    PyHost::of_world(world, PyDict::new(world.py()))
}

/// A method set on the manager itself (a test's), which the rule calls as Python would.
fn own_request<'py>(manager: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyAny>>> {
    let py = manager.py();
    raw_dict_get(&manager.getattr(intern!(py, "__dict__"))?, intern!(py, "request").as_any())
}

/// The handler's managers, as the core's `arrive` asks them: the toilet's queue
/// (`toilet::queue_up`) and the resource service's (`service::queue_up_or_refusal`), whose inputs
/// the PyO3 crate reads, or a manager's own `request`, a test's.
struct HandlerManagers<'a, 'py> {
    handler: &'a Bound<'py, PyAny>,
    world: &'a Bound<'py, PyAny>,
}

impl<'py> HandlerManagers<'_, 'py> {
    fn manager(&self, toilet: bool) -> PyResult<Bound<'py, PyAny>> {
        let py = self.world.py();
        attribute(self.handler, if toilet { intern!(py, "toilet_service") } else { intern!(py, "resource_service") })
    }

    /// The own `request(agent, facility, minute=..., validate_state=...)`: None, or its
    /// `ValueError`'s text.
    fn call_own(&self, request: &Bound<'py, PyAny>, agent_id: &str, facility_id: &str, validate: &Bound<'py, PyAny>) -> PyResult<Option<String>> {
        let py = self.world.py();
        let minute = attribute(self.world, intern!(py, "minute_index"))?;
        let options = PyDict::new(py);
        options.set_item(intern!(py, "minute"), &minute)?;
        options.set_item(intern!(py, "validate_state"), validate)?;
        match request.call((agent_id, facility_id), Some(&options)) {
            Ok(_) => Ok(None),
            Err(error) if error.is_instance_of::<pyo3::exceptions::PyValueError>(py) => Ok(Some(error.value(py).str()?.to_string())),
            Err(error) => Err(error),
        }
    }
}

impl<'py> Managers<PyHost<'py>> for HandlerManagers<'_, 'py> {
    fn own(&mut self, toilet: bool) -> Result<bool, HostError> {
        Ok(own_request(&self.manager(toilet)?)?.is_some())
    }

    fn toilet(&mut self, _host: &mut PyHost<'py>, agent_id: &str, facility_id: &str) -> Result<(), HostError> {
        let py = self.world.py();
        let manager = self.manager(true)?;
        if let Some(request) = own_request(&manager)? {
            // `except ValueError as exc: raise EventApplicationError(str(exc))`.
            let validate = attribute(self.handler, intern!(py, "validate_request_state"))?;
            return match self.call_own(&request, agent_id, facility_id, &validate)? {
                None => Ok(()),
                Some(text) => Err(HostError(errors::error(py, errors::Outcome::Failed, text))),
            };
        }
        let (agent, facility) = (PyString::new(py, agent_id), PyString::new(py, facility_id));
        toilet::queue_up(self.world, agent.as_any(), facility.as_any(), None)?;
        Ok(())
    }

    fn resource(&mut self, _host: &mut PyHost<'py>, agent_id: &str, facility_id: &str) -> Result<Option<String>, HostError> {
        let py = self.world.py();
        let manager = self.manager(false)?;
        if let Some(request) = own_request(&manager)? {
            return Ok(self.call_own(&request, agent_id, facility_id, PyBool::new(py, false).as_any())?);
        }
        let (agent, facility) = (PyString::new(py, agent_id), PyString::new(py, facility_id));
        Ok(service::queue_up_or_refusal(&manager, agent.as_any(), facility.as_any())?.err())
    }
}

/// `_after_arrival`: the core's (`core::arrive`, lane P9-1a), with the handler's managers and what
/// it reads of the handler and the configs here.
fn arrive<'py>(
    handler: &Bound<'py, PyAny>,
    world: &Bound<'py, PyAny>,
    agent_id: &str,
    purpose: &str,
    facility_id: &str,
    destination: &str,
    origin: Option<&str>,
) -> PyResult<()> {
    let py = world.py();
    let is_toilet = purpose == "toilet_request";
    if !is_toilet && purpose != "resource_request" {
        return Ok(());
    }
    let configs = attribute(world, intern!(py, "model_configs"))?;
    let has_geometry = exact_dict(&configs)?.contains(intern!(py, "facility_geometry"))?;
    if has_geometry {
        // The approach writes through the world's event stores.
        EventWriter::of(world)?;
    }
    let validates = is_toilet && attribute(handler, intern!(py, "validate_request_state"))?.is_truthy()?;
    let resource_service = !is_toilet && !attribute(handler, intern!(py, "resource_service"))?.is_none();
    let arrival = core::Arrival { agent_id, purpose, facility_id, destination, origin, has_geometry, resource_service, validates };
    let mut managers = HandlerManagers { handler, world };
    let arrived = core::arrive(&PyWorldCell(world), &mut host_of(world), &arrival, &mut managers).map_err(|error| error.0)?;
    write_result(py, arrived)
}

/// `_reject_room_approach`, called from Python.
#[pyfunction]
fn reject_room_approach<'py>(
    handler: &Bound<'py, PyAny>,
    world: &Bound<'py, PyAny>,
    event: &Bound<'py, PyAny>,
    agent_id: &Bound<'py, PyAny>,
    facility_id: &Bound<'py, PyAny>,
    origin_zone_id: &Bound<'py, PyAny>,
) -> PyResult<()> {
    let py = world.py();
    let purpose = crate::rules::handlers::get(&attribute(event, intern!(py, "payload"))?, "purpose")?;
    let resource = str_value(&purpose) == Some("resource_request") && !attribute(handler, intern!(py, "resource_service"))?.is_none();
    let (agent_id, facility_id) = (exact_str(agent_id)?, exact_str(facility_id)?);
    core::reject_room_through(&PyWorldCell(world), &mut host_of(world), resource, agent_id, facility_id, str_value(origin_zone_id)).map_err(|error| error.0)
}

/// `_after_arrival`, called from Python.
#[pyfunction]
#[allow(clippy::too_many_arguments)]
fn after_arrival<'py>(
    handler: &Bound<'py, PyAny>,
    world: &Bound<'py, PyAny>,
    event: &Bound<'py, PyAny>,
    agent_id: &Bound<'py, PyAny>,
    purpose: &Bound<'py, PyAny>,
    facility_id: &Bound<'py, PyAny>,
    destination_zone_id: &Bound<'py, PyAny>,
    origin_zone_id: &Bound<'py, PyAny>,
) -> PyResult<()> {
    let _ = event;
    let purpose = exact_str(purpose)?;
    // Only a walk to a toilet or a resource leads anywhere.
    if !matches!(purpose, "toilet_request" | "resource_request") {
        return Ok(());
    }
    arrive(handler, world, exact_str(agent_id)?, purpose, exact_str(facility_id)?, exact_str(destination_zone_id)?, str_value(origin_zone_id))
}

/// The attribute of an outcome the rule's checks raised: nothing was written.
const UNWRITTEN: &str = "_sheltersim_travel_unwritten";

/// Whether this error is a text of the rule's checks, raised before any change, rather than one
/// raised after the walk's writes (from what the walk leads to).
pub(crate) fn raised_before_writing(py: Python<'_>, error: &PyErr) -> bool {
    error.value(py).hasattr(intern!(py, UNWRITTEN)).unwrap_or(false)
}

/// The payload keys the rule reads, each when a str.
const TEXT_KEYS: [&str; 9] = ["agent_id", "task_id", "destination_zone_id", "purpose", "facility_id", "origin_zone_id", "queue_id", "request_id", "staff_id"];

/// The row of a walk's event object: its own, or for an object no row holds (a payload value the
/// writer refuses), a row of what the rule reads of it, read as the rule reads it: the payload's
/// strs of the keys it reads and the ends of its route.
fn walk_row(object: &Bound<'_, PyAny>) -> PyResult<EventRow> {
    use sheltersim_core::value::V;
    if let Some(row) = EventRow::from_event(object)? {
        return Ok(row);
    }
    let py = object.py();
    let event = EventRef::Object(object);
    let mut payload = indexmap::IndexMap::new();
    for key in TEXT_KEYS {
        if let Some(value) = event.text(key)? {
            payload.insert(Arc::<str>::from(key), V::Str(value));
        }
    }
    let route = attribute(object, intern!(py, "payload"))?.call_method1(intern!(py, "get"), (intern!(py, "route_zone_ids"),))?;
    if let Ok(items) = route.downcast_exact::<pyo3::types::PyTuple>() {
        let end = |item: PyResult<Bound<'_, PyAny>>| -> V {
            match item.ok().and_then(|item| item.downcast_exact::<PyString>().ok().and_then(|text| text.to_str().ok().map(Arc::<str>::from))) {
                Some(text) => V::Str(text),
                None => V::Null,
            }
        };
        let ends = if items.is_empty() { Vec::new() } else { vec![end(items.get_item(0)), end(items.get_item(items.len() - 1))] };
        payload.insert(Arc::from("route_zone_ids"), V::List(ends));
    }
    let text = |name: &Bound<'_, PyString>| -> PyResult<Arc<str>> { Ok(Arc::from(exact_str(&attribute(object, name)?)?)) };
    let optional = |name: &Bound<'_, PyString>| -> PyResult<Option<Arc<str>>> {
        let value = attribute(object, name)?;
        if value.is_none() { Ok(None) } else { Ok(Some(Arc::from(exact_str(&value)?))) }
    };
    let kind = attribute(&attribute(object, intern!(py, "kind"))?, intern!(py, "value"))?;
    let kind = sheltersim_core::events::EventKind::parse(exact_str(&kind)?).ok_or_else(|| Unsupported::new_err("an event of another kind"))?;
    EventRow::new(
        text(intern!(py, "event_id"))?,
        kind,
        exact_int(&attribute(object, intern!(py, "due_minute"))?)?,
        exact_int(&attribute(object, intern!(py, "priority"))?)?,
        exact_int(&attribute(object, intern!(py, "sequence"))?)?,
        text(intern!(py, "event_type"))?,
        payload,
        optional(intern!(py, "source_id"))?,
        optional(intern!(py, "causation_id"))?,
        indexmap::IndexMap::new(),
    )
    .ok_or_else(|| Unsupported::new_err("the event has no row"))
}

/// Which manager of a walk to a toilet or a resource has its own `request` (a test's): the toilet's
/// (true) or the resource service's (false), or None.
fn foreign_request(handler: &Bound<'_, PyAny>, row: &EventRow) -> PyResult<Option<bool>> {
    let py = handler.py();
    let toilet = match kit::payload(row, "purpose") {
        Some(V::Str(purpose)) if &**purpose == "toilet_request" => true,
        Some(V::Str(purpose)) if &**purpose == "resource_request" => false,
        _ => return Ok(None),
    };
    let manager = attribute(handler, if toilet { intern!(py, "toilet_service") } else { intern!(py, "resource_service") })?;
    if manager.is_none() {
        return Ok(None);
    }
    Ok(own_request(&manager)?.map(|_| toilet))
}

/// What the host's run of the handler gave, as the dispatcher takes a direct call's: its outcome
/// raised with its text.
fn foreign_outcome(py: Python<'_>, foreign: Foreign) -> PyResult<()> {
    match foreign.status {
        EventStatus::Applied => Ok(()),
        EventStatus::Suppressed => Err(errors::error(py, errors::Outcome::Suppressed, foreign.detail.unwrap_or_default())),
        _ => Err(errors::error(py, errors::Outcome::Failed, foreign.detail.unwrap_or_default())),
    }
}

/// `AgentTravelCompletionHandler.apply` of a walk. `object` gives the event's object, for Python.
/// `direct` asks `_after_arrival`'s kernel with no Python between.
pub(crate) fn apply_event<'py>(
    handler: &Bound<'py, PyAny>,
    world: &Bound<'py, PyAny>,
    event: EventRef<'_, 'py>,
    object: &dyn Fn() -> PyResult<Bound<'py, PyAny>>,
    direct: bool,
) -> PyResult<()> {
    let py = world.py();
    let row = match event {
        EventRef::Row(row) => row.clone(),
        EventRef::Object(object) => walk_row(object)?,
    };
    let cell = PyWorldCell(world);
    let mut host = host_of(world);
    // A manager's own `request` (a test's): the handler is the host's to run whole (H4), which
    // gives it the event's object; nothing is written here.
    if direct {
        if foreign_request(handler, &row)?.is_some() {
            let foreign = host.with_handler(handler).with_in_flight(&object()?).apply_foreign(&row).map_err(|error| error.0)?;
            return foreign_outcome(py, foreign);
        }
    }
    let read = core::Gathered::read(&cell, &mut host, &row).map_err(|error| error.0)?;
    let lifecycle_in = match kit::payload(&row, "purpose") {
        Some(V::Str(purpose)) if &**purpose == "presence_relocation" => Some(LifecycleIn::of(world, false)?),
        _ => None,
    };
    if lifecycle_in.is_some() {
        // A relocation's completion reads the pending events as rows.
        crate::rules::travel::read_queue_rows(world)?;
    }
    let lifecycle = lifecycle_in.as_ref().map(LifecycleIn::inputs);
    let resource_service = !attribute(handler, intern!(py, "resource_service"))?.is_none();
    let inputs = read.inputs(resource_service, lifecycle.as_ref());
    let finish = |error: PyErr| match &lifecycle_in {
        Some(lifecycle) => lifecycle.finish(py, Err::<(), _>(error)).unwrap_err(),
        None => error,
    };
    let applied = crate::rules::population::with_rows(world, || match cell.with(|world| core::apply(world, &inputs, &row)).or_py()? {
        Ok(walk) => Ok(Ok(walk)),
        Err(Applied::After(error)) if matches!(&error, sheltersim_core::error::Error::Unsupported(reason) if reason == sheltersim_core::rules::population::NO_ROW) => {
            Err(Unsupported::new_err(sheltersim_core::rules::population::NO_ROW))
        }
        Err(applied) => Ok(Err(applied)),
    })?;
    let walk = match applied {
        Ok(Some(walk)) => walk,
        Ok(None) => return Ok(()),
        Err(Applied::Checks(error)) => {
            let error = finish(to_py_err(error));
            if errors::made_here(py, &error) {
                error.value(py).setattr(intern!(py, UNWRITTEN), true)?;
            }
            return Err(error);
        }
        Err(Applied::After(error)) => return Err(finish(to_py_err(error))),
    };
    if matches!(&*walk.purpose, "toilet_request" | "resource_request") {
        let facility = walk.facility_id.as_deref().unwrap_or_default();
        // A resource walk got here with its origin a str; a toilet walk's `_after_arrival` doesn't
        // read it.
        let origin = walk.origin.zone().map(|zone| &**zone);
        if direct {
            match arrive(handler, world, &walk.agent_id, &walk.purpose, facility, &walk.destination, origin) {
                Ok(()) => {
                    ARRIVALS_RAN.fetch_add(1, Ordering::Relaxed);
                    return Ok(());
                }
                Err(error) if error.is_instance_of::<Unsupported>(py) => {
                    ARRIVALS_REFUSED.fetch_add(1, Ordering::Relaxed);
                    if let Ok(mut refusals) = ARRIVAL_REFUSALS.lock() {
                        *refusals.entry(error.value(py).to_string()).or_insert(0) += 1;
                    }
                }
                Err(error) => return Err(error),
            }
        }
        handler
            .call_method1(intern!(py, "_after_arrival"), (world, object()?, &*walk.agent_id, &*walk.purpose, facility, &*walk.destination, origin))
            .map_err(|error| if error.is_instance_of::<Unsupported>(py) { PyRuntimeError::new_err("a kernel said Unsupported after the walk was written") } else { error })?;
    }
    Ok(())
}

/// `AgentTravelCompletionHandler.apply`, called from Python.
#[pyfunction]
fn apply<'py>(handler: &Bound<'py, PyAny>, world: &Bound<'py, PyAny>, event: &Bound<'py, PyAny>) -> PyResult<()> {
    if EventRow::from_event(event)?.is_none() {
        return unsupported("the event has no row");
    }
    apply_event(handler, world, EventRef::Object(event), &|| Ok(event.clone()), false)
}

/// How often `direct` found its way: `(ran, refused, reasons)`.
pub(crate) fn arrival_counts() -> (u64, u64, BTreeMap<String, u64>) {
    let reasons = ARRIVAL_REFUSALS.lock().map(|refusals| refusals.clone()).unwrap_or_default();
    (ARRIVALS_RAN.load(Ordering::Relaxed), ARRIVALS_REFUSED.load(Ordering::Relaxed), reasons)
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(apply, m)?)?;
    m.add_function(wrap_pyfunction!(after_arrival, m)?)?;
    m.add_function(wrap_pyfunction!(reject_room_approach, m)?)?;
    Ok(())
}
