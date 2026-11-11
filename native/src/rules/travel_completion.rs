//! `AgentTravelCompletionHandler` of `events/handlers.py`: the glue of the core's kernels
//! (`sheltersim_core::rules::travel_completion`, lane c7 of Issue #152). `apply_event` reads what
//! Python holds (whether the world holds the namespaces, the floor plan and the movement section
//! for a queue walk, the lifecycle's inputs for a relocation, whether the handler has a resource
//! service), and runs the core's checks and writes on the world borrowed whole.
//!
//! What a walk to a toilet or a resource leads to (`_after_arrival`) stays glue: the core plans the
//! walk in the room or its rejection, and the request goes to the handler's managers as before
//! (`toilet::queue_up` and `service::queue_up_or_refusal`, or a method a test set on a manager),
//! whose refusal the core records. A rule's text raised before any change is marked
//! (`raised_before_writing`), for the dispatcher.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use pyo3::exceptions::PyRuntimeError;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyString};

use sheltersim_core::rules::kit::Held;
use sheltersim_core::rules::travel_completion::{self as core, Applied, Approach};

use crate::core::errors::{self, to_py_err, OrPy};
use crate::core::row::{attribute, exact_dict, exact_int, exact_str, str_value, unsupported};
use crate::core::world_core::{field_held, namespace_held, raw_dict_get, with_world};
use crate::rules::movement_geometry::{Geo, GeoPy};
use crate::rules::population_lifecycle::LifecycleIn;
use crate::rules::{service, toilet};
use crate::state::events::{EventRef, EventRow, EventRowPy, EventWriter};
use crate::Unsupported;

static ARRIVALS_RAN: AtomicU64 = AtomicU64::new(0);
static ARRIVALS_REFUSED: AtomicU64 = AtomicU64::new(0);
static ARRIVAL_REFUSALS: Mutex<BTreeMap<String, u64>> = Mutex::new(BTreeMap::new());

const SERVICE: &str = "resource_service_v1";

fn service_held(world: &Bound<'_, PyAny>) -> PyResult<Held> {
    namespace_held(world, intern!(world.py(), SERVICE), "the world holds another service state")
}

fn metric_state_held(world: &Bound<'_, PyAny>) -> PyResult<bool> {
    field_held(world, intern!(world.py(), "metric_state"))
}

/// `_after_arrival`, where no rejection follows. A rejection reads the event and is the Python
/// rule's, which then runs from the start.
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
        let inputs = crate::rules::travel::Inputs::of(world)?;
        let queues = crate::state::facility_queues::held(world)?;
        let writer = EventWriter::of(world)?;
        let reserved = writer.reserved();
        let (agent, facility, origin_zone): (Arc<str>, Arc<str>, Arc<str>) = (Arc::from(agent_id), Arc::from(facility_id), Arc::from(destination));
        let approach = with_world(world, |cell| core::arrival_approach(cell, inputs.walking(), &inputs.section, &queues, has_geometry, reserved, &agent, purpose, &facility, &origin_zone))?;
        match inputs.finish(py, approach.or_py())? {
            Approach::Request => {}
            Approach::Reject => return reject_room(handler, world, purpose, agent_id, facility_id, origin),
            Approach::Travel(trip) => return crate::rules::travel::write_travel(world, &writer, trip),
        }
    }
    let (agent, facility) = (PyString::new(py, agent_id), PyString::new(py, facility_id));
    let minute = attribute(world, intern!(py, "minute_index"))?;
    // A method set on the manager itself (a test's) is what the rule calls, as Python would.
    let own_request = |manager: &Bound<'py, PyAny>| -> PyResult<Option<Bound<'py, PyAny>>> {
        raw_dict_get(&manager.getattr(intern!(py, "__dict__"))?, intern!(py, "request").as_any())
    };
    let call_own = |request: &Bound<'py, PyAny>, validate: &Bound<'py, PyAny>| -> PyResult<Option<String>> {
        let options = PyDict::new(py);
        options.set_item(intern!(py, "minute"), &minute)?;
        options.set_item(intern!(py, "validate_state"), validate)?;
        match request.call((&agent, &facility), Some(&options)) {
            Ok(_) => Ok(None),
            Err(error) if error.is_instance_of::<pyo3::exceptions::PyValueError>(py) => Ok(Some(error.value(py).str()?.to_string())),
            Err(error) => Err(error),
        }
    };
    if is_toilet {
        // `except ValueError as exc: raise EventApplicationError(str(exc))`, also for the world's
        // validation that `validate_request_state` asks before and after the request.
        let validate = attribute(handler, intern!(py, "validate_request_state"))?;
        let manager = attribute(handler, intern!(py, "toilet_service"))?;
        let refused = match own_request(&manager)? {
            Some(request) => call_own(&request, &validate)?,
            None => {
                let validates = validate.is_truthy()?;
                let requested = (|| -> PyResult<()> {
                    if validates {
                        world.call_method0(intern!(py, "validate"))?;
                    }
                    toilet::queue_up(world, agent.as_any(), facility.as_any(), None)?;
                    if validates {
                        world.call_method0(intern!(py, "validate"))?;
                    }
                    Ok(())
                })();
                match requested {
                    Ok(()) => None,
                    Err(error) if error.is_instance_of::<pyo3::exceptions::PyValueError>(py) => Some(error.value(py).str()?.to_string()),
                    Err(error) => return Err(error),
                }
            }
        };
        if let Some(text) = refused {
            return errors::failed(py, text);
        }
        return Ok(());
    }
    let manager = attribute(handler, intern!(py, "resource_service"))?;
    // Read before the request, which may write the task counter and then turn the person down.
    let held = service_held(world)?;
    let metrics = metric_state_held(world)?;
    with_world(world, |cell| core::check_resource_request(cell, &held, metrics))?.or_py()?;
    let refused = match own_request(&manager)? {
        Some(request) => call_own(&request, PyBool::new(py, false).as_any())?,
        None => service::queue_up_or_refusal(&manager, agent.as_any(), facility.as_any())?.err(),
    };
    let Some(text) = refused else {
        return Ok(());
    };
    // The arrival stands; the record says why the person is not in the queue, with `str(exc)`.
    let Some(origin) = origin else {
        return unsupported("a resource walk's origin is not a str");
    };
    let minute = exact_int(&minute)?;
    let origin: Arc<str> = Arc::from(origin);
    with_world(world, |cell| core::rejected_request(cell, &held, agent_id, facility_id, &origin, minute, &text))?.or_py()
}

/// `_reject_room_approach`. `origin` is the event's origin when a str.
fn reject_room<'py>(handler: &Bound<'py, PyAny>, world: &Bound<'py, PyAny>, purpose: &str, agent_id: &str, facility_id: &str, origin: Option<&str>) -> PyResult<()> {
    let py = world.py();
    let resource = purpose == "resource_request" && !attribute(handler, intern!(py, "resource_service"))?.is_none();
    let held = service_held(world)?;
    let metrics = metric_state_held(world)?;
    let origin: Option<Arc<str>> = origin.map(Arc::from);
    with_world(world, |cell| core::reject_room(cell, &held, metrics, resource, agent_id, facility_id, origin.as_ref()))?.or_py()
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
    let purpose = crate::rules::handlers::get(&attribute(event, intern!(world.py(), "payload"))?, "purpose")?;
    let purpose = if str_value(&purpose) == Some("resource_request") { "resource_request" } else { "other" };
    reject_room(handler, world, purpose, exact_str(agent_id)?, exact_str(facility_id)?, str_value(origin_zone_id))
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

/// What the core reads of the Python world for this walk.
struct TravelIn {
    movement: Held,
    queues: Held,
    overlay: Held,
    geo: Option<Arc<Geo>>,
    section: Option<Arc<sheltersim_core::config::movement::Movement>>,
    resource_service: bool,
    lifecycle: Option<LifecycleIn>,
}

impl TravelIn {
    fn of(handler: &Bound<'_, PyAny>, world: &Bound<'_, PyAny>, row: &EventRow) -> PyResult<Self> {
        let py = world.py();
        let purpose = match sheltersim_core::rules::kit::payload(row, "purpose") {
            Some(sheltersim_core::value::V::Str(purpose)) => purpose.clone(),
            _ => Arc::from(""),
        };
        let (geo, section) = if core::is_queue_walk(&purpose) {
            let geo = Geo::of(world)?;
            let section = if geo.layout.is_some() { Some(crate::rules::movement::movement_of(world)?) } else { None };
            (Some(geo), section)
        } else {
            (None, None)
        };
        let lifecycle = if &*purpose == "presence_relocation" { Some(LifecycleIn::of(world, false)?) } else { None };
        Ok(Self {
            movement: namespace_held(world, intern!(py, "movement_v1"), "the world holds another movement state")?,
            queues: crate::state::facility_queues::held(world)?,
            overlay: namespace_held(world, intern!(py, "geometry_overlay_v1"), "the world holds another overlay state")?,
            geo,
            section,
            resource_service: !attribute(handler, intern!(py, "resource_service"))?.is_none(),
            lifecycle,
        })
    }
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
    let read = TravelIn::of(handler, world, &row)?;
    if read.lifecycle.is_some() {
        // A relocation's completion reads the pending events as rows.
        crate::rules::travel::read_queue_rows(world)?;
    }
    let lifecycle = read.lifecycle.as_ref().map(LifecycleIn::inputs);
    let inputs = core::Inputs {
        movement: read.movement.clone(),
        queues: read.queues.clone(),
        overlay: read.overlay.clone(),
        geo: read.geo.as_deref(),
        section: read.section.as_deref(),
        resource_service: read.resource_service,
        lifecycle: lifecycle.as_ref(),
    };
    let finish = |error: PyErr| match &read.lifecycle {
        Some(lifecycle) => lifecycle.finish(py, Err::<(), _>(error)).unwrap_err(),
        None => error,
    };
    let applied = crate::rules::population::with_rows(world, || match with_world(world, |cell| core::apply(cell, &inputs, &row))? {
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
