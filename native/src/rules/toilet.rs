//! `ToiletServiceManager` of `domain/facilities.py` and `ToiletServiceCompletionHandler` of
//! `events/handlers.py`: the glue of the core's kernels (`sheltersim_core::rules::toilet`, lane c3
//! of Issue #152). Each entry reads what Python holds (the manager's durations and policy, the
//! random source's seed, whether the world holds each namespace, the floor plan, the walk inputs of
//! a placed world), runs the core on the world borrowed whole, and makes the Python values of what
//! it returns. The world's validation and the draw of a random source that is not a `CounterRNG`
//! go through the native host (`Host::validate_world`, `Host::draw`). The walk home after a
//! completion (`after_complete`) is called here, after the core's write (lane c6 moves it).

use std::sync::Arc;

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::sync::GILOnceCell;
use pyo3::types::{PyBool, PyDict, PyList, PyString};

use sheltersim_core::config::population::AccessibilityNeeds;
use sheltersim_core::rules::kit::{Held, Kept};
use sheltersim_core::rules::toilet::{self as core, Request, Rng, ScheduleReturn, Start, Started};

use crate::core::configs;
use crate::core::errors::{self, OrPy};
use crate::core::row::{attribute, exact_dict, exact_int, exact_int_text, exact_str};
use crate::core::world_core::{field_held, namespace_held, with_world, PyWorldCell};
use crate::host::PyHost;
use crate::rules::movement_geometry::{Geo, GeoPy};
use crate::rules::queues::{placement, Walking};
use crate::rules::staff_dynamics::tree_operation_is_idle;
use crate::state::events::{write_result, EventRef, EventRow, EventRowPy};
use crate::Unsupported;

static COUNTER_RNG: GILOnceCell<Py<PyAny>> = GILOnceCell::new();

/// `"toilet" in kind.casefold()`, with the core's fold (`sheltersim_core::rules::kit::is_toilet`).
pub(crate) fn is_toilet(kind: &str) -> PyResult<bool> {
    Ok(sheltersim_core::rules::kit::is_toilet(kind))
}

fn receipt_class(py: Python<'_>) -> PyResult<&Bound<'_, PyAny>> {
    static CLASS: GILOnceCell<Py<PyAny>> = GILOnceCell::new();
    Ok(CLASS
        .get_or_try_init(py, || -> PyResult<Py<PyAny>> { Ok(py.import("sheltersim.domain.facilities")?.getattr("ToiletRequestReceipt")?.unbind()) })?
        .bind(py))
}

/// What `request` reads of the Python world.
struct Inputs {
    queues: Held,
    geo: Arc<Geo>,
    needs: Kept<AccessibilityNeeds>,
}

impl Inputs {
    fn of(world: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            queues: crate::state::facility_queues::held(world)?,
            geo: Geo::of(world)?,
            needs: configs::kept_section(world, |configs| &configs.accessibility_needs, AccessibilityNeeds::parse)?,
        })
    }

    fn request<'a>(&'a self, agent_id: &'a str, facility_id: &'a str, queue_id: Option<&'a str>) -> Request<'a> {
        Request { agent_id, facility_id, queue_id, queues: &self.queues, geo: &self.geo, needs: &self.needs }
    }
}

/// `request`, without the world's validation: the task's ID, the minute and the place in the queue.
pub(crate) fn queue_up<'py>(
    world: &Bound<'py, PyAny>,
    agent_id: &Bound<'py, PyAny>,
    facility_id: &Bound<'py, PyAny>,
    queue_id: Option<&str>,
) -> PyResult<(Arc<str>, i64, i64)> {
    let (agent, facility) = (exact_str(agent_id)?, exact_str(facility_id)?);
    let inputs = Inputs::of(world)?;
    let receipt = write_result(world.py(), with_world(world, |world| core::queue_up(world, &inputs.request(agent, facility, queue_id)))?)?;
    Ok((receipt.queue_task_id, receipt.requested_at, receipt.queue_position))
}

/// `ToiletServiceManager.request`.
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
    let queue_name = queue_id.filter(|queue| !queue.is_none()).map(exact_str).transpose()?;
    let world = attribute(manager, intern!(py, "world"))?;
    if let Some(minute) = minute.filter(|minute| !minute.is_none()) {
        let now = exact_int(&attribute(&world, intern!(py, "minute_index"))?)?;
        let Some(minute) = errors::int_not_bool(minute)? else {
            return Err(errors::value_error("toilet request minute must be an integer"));
        };
        if minute != now {
            return Err(errors::value_error("toilet request minute must match WorldState"));
        }
    }
    let (agent, facility) = (exact_str(agent_id)?, exact_str(facility_id)?);
    let inputs = Inputs::of(&world)?;
    let mut host = PyHost::of_world(&world, PyDict::new(py));
    let receipt = core::request(&PyWorldCell(&world), &mut host, &inputs.request(agent, facility, queue_name), validates).map_err(|error| error.0)?;
    receipt_class(py)?.call1((agent_id, facility_id, receipt.requested_at, &*receipt.queue_task_id, receipt.queue_position))
}

/// `arbitrate`'s facilities with anyone queued, in ID order. Those with none do nothing there.
#[pyfunction]
pub(crate) fn facilities_to_serve<'py>(manager: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyList>> {
    let py = manager.py();
    let world = attribute(manager, intern!(py, "world"))?;
    let queues = crate::state::facility_queues::held(&world)?;
    let due = with_world(&world, |world| core::facilities_to_serve(world, &queues))?.or_py()?;
    let result = PyList::empty(py);
    if !due.is_empty() {
        let facilities = attribute(&world, intern!(py, "facilities"))?;
        for id in due {
            let Some(facility) = crate::core::world_core::raw_dict_get(&facilities, PyString::new(py, &id).as_any())? else {
                return Err(Unsupported::new_err("a row has no object"));
            };
            result.append(facility)?;
        }
    }
    Ok(result)
}

/// `arbitrate`'s condition of a shared queue: someone in it and a free place.
#[pyfunction]
pub(crate) fn has_room<'py>(manager: &Bound<'py, PyAny>, facility_id: &Bound<'py, PyAny>) -> PyResult<bool> {
    let py = manager.py();
    let world = attribute(manager, intern!(py, "world"))?;
    exact_dict(&attribute(&world, intern!(py, "model_state"))?)?;
    let name = exact_str(facility_id)?;
    let queues = crate::state::facility_queues::held(&world)?;
    let placement = placement(&world)?;
    with_world(&world, |world| core::has_room(world, &queues, placement.as_deref(), name))?.or_py()
}

/// The rest of a pass of `arbitrate`'s loop: the head starts its service, and this says the started
/// event's ID, or None when the service could not finish before the Run ends.
#[pyfunction]
pub(crate) fn start_head<'py>(
    manager: &Bound<'py, PyAny>,
    facility_id: &Bound<'py, PyAny>,
    minute: &Bound<'py, PyAny>,
    rng: &Bound<'py, PyAny>,
) -> PyResult<Option<Bound<'py, PyString>>> {
    start_one(manager, facility_id, minute, rng, None)
}

/// A pass of `_arbitrate_independent_facility`'s loop after the positions: the started event's ID,
/// or None to stop the queue, with what stopped it marked on the queue.
#[pyfunction]
pub(crate) fn serve_independent_head<'py>(
    manager: &Bound<'py, PyAny>,
    facility_id: &Bound<'py, PyAny>,
    queue_id: &Bound<'py, PyAny>,
    minute: &Bound<'py, PyAny>,
    rng: &Bound<'py, PyAny>,
) -> PyResult<Option<Bound<'py, PyString>>> {
    start_one(manager, facility_id, minute, rng, Some(exact_str(queue_id)?))
}

fn start_one<'py>(
    manager: &Bound<'py, PyAny>,
    facility_id: &Bound<'py, PyAny>,
    minute: &Bound<'py, PyAny>,
    rng: &Bound<'py, PyAny>,
    queue_id: Option<&str>,
) -> PyResult<Option<Bound<'py, PyString>>> {
    let py = manager.py();
    let world = attribute(manager, intern!(py, "world"))?;
    let facility = exact_str(facility_id)?;
    let minute = exact_int(minute)?;
    let counter_rng = COUNTER_RNG
        .get_or_try_init(py, || -> PyResult<Py<PyAny>> { Ok(py.import("sheltersim.core.rng")?.getattr("CounterRNG")?.unbind()) })?
        .bind(py);
    // The world's CounterRNG draws in the core, keyed by `str(seed)` of a seed of any size; another
    // KeyedRandom (a test's) is asked through the host, as the rule asks it.
    let random = if rng.get_type().is(counter_rng) { Rng::Seed(exact_int_text(&attribute(rng, intern!(py, "seed"))?)?) } else { Rng::Host };
    let lowest = exact_int(&attribute(manager, intern!(py, "min_service_minutes"))?)?;
    let highest = exact_int(&attribute(manager, intern!(py, "max_service_minutes"))?)?;
    let policy: Arc<str> = Arc::from(exact_str(&attribute(manager, intern!(py, "policy_version"))?)?);
    let queues = crate::state::facility_queues::held(&world)?;
    let geo = Geo::of(&world)?;
    let walking = match queue_id {
        Some(_) => Walking::read(&world, true)?,
        None => None,
    };
    let walk = walking.as_ref().map(Walking::walk);
    let metric_state_held = field_held(&world, intern!(py, "metric_state"))?;
    let mut host = PyHost::of_world(&world, PyDict::new(py)).with_rng(rng);
    let mut idle = Vec::new();
    loop {
        let start = Start {
            facility_id: facility,
            minute,
            queue_id,
            lowest,
            highest,
            policy: policy.clone(),
            rng: &random,
            queues: &queues,
            geo: &geo,
            walk: walk.as_ref(),
            metric_state_held,
            idle: &idle,
        };
        match core::start(&PyWorldCell(&world), &mut host, &start).map_err(|error| error.0)? {
            Started::Event(event_id) => return Ok(Some(PyString::new(py, &event_id))),
            Started::Stopped => return Ok(None),
            Started::NeedsIdle(staff_id) => {
                let answer = tree_operation_is_idle(&world, &PyString::new(py, &staff_id))?;
                idle.push((staff_id, answer));
            }
        }
    }
}

/// `ToiletServiceCompletionHandler.apply` up to `after_complete`, with `complete_toilet_service`:
/// the caller then schedules the walk home.
pub(crate) fn complete_event<'py>(world: &Bound<'py, PyAny>, event: EventRef<'_, 'py>) -> PyResult<()> {
    let py = world.py();
    let owned;
    let row = match event {
        EventRef::Row(row) => row,
        EventRef::Object(object) => {
            owned = EventRow::from_event(object)?.ok_or_else(|| Unsupported::new_err("the event is not a row"))?;
            &owned
        }
    };
    let queues = crate::state::facility_queues::held(world)?;
    let geo = Geo::of(world)?;
    let body = namespace_held(world, intern!(py, "body_state_v1"), "the world holds another body state")?;
    let metric_state_held = field_held(world, intern!(py, "metric_state"))?;
    let ScheduleReturn = write_result(py, with_world(world, |world| core::complete(world, row, &queues, &geo, &body, metric_state_held))?)?;
    Ok(())
}

/// `ToiletServiceCompletionHandler.apply`, called from Python: then its `after_complete`.
#[pyfunction]
fn complete<'py>(handler: &Bound<'py, PyAny>, world: &Bound<'py, PyAny>, event: &Bound<'py, PyAny>) -> PyResult<()> {
    let py = handler.py();
    complete_event(world, EventRef::Object(event))?;
    let after = attribute(handler, intern!(py, "after_complete"))?;
    if !after.is_none() {
        after.call1((world, event))?;
    }
    Ok(())
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(request, m)?)?;
    m.add_function(wrap_pyfunction!(facilities_to_serve, m)?)?;
    m.add_function(wrap_pyfunction!(has_room, m)?)?;
    m.add_function(wrap_pyfunction!(start_head, m)?)?;
    m.add_function(wrap_pyfunction!(serve_independent_head, m)?)?;
    m.add_function(wrap_pyfunction!(complete, m)?)?;
    Ok(())
}
