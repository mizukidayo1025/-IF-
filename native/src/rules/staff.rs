//! `domain/staff.py` and `domain/staff_operations.py`: the glue of the core's kernels
//! (`sheltersim_core::rules::staff`, lane P8 b3 of Issue #152). Each entry reads what Python holds
//! (whether `model_state` holds each namespace and whether `metric_state` is its slot's tree; the
//! floor plan, the configs' sections and the staff operations formula where a kernel asks for them;
//! the service's parameters from the Run's `Policies`, or from the service a Python caller gave; the
//! router a Python caller gave), runs the core on the world borrowed whole, and makes the Python
//! values of what it returns.
//!
//! The kernels plan before they write, and where a plan needs more than the plain tick reads (a
//! request that may be assigned, a free worker with a task to take, a wait whose routes are read),
//! the core says so with nothing written, and the glue reads the rest and calls it again. What a
//! read raised is raised where the core reads it (`PLAN_UNREAD`, `FORMULA_UNREAD`, as the steps'
//! `KernelRefusal` where the controller's `_route` or `_schedule_staff_support_travel` read it).

use std::sync::Arc;

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyString, PyTuple};
use sheltersim_core::error::Error;
use sheltersim_core::events::writer::{WriteError, WriteResult};
use sheltersim_core::rules::overlay::PLAN_UNREAD;
use sheltersim_core::rules::staff::{self as core, Assigned, Full, Inputs, Operations, Routed, Service, Step, Travel, WaitRoutes};
use sheltersim_core::rules::tick::arbitrate::Staff;
use sheltersim_core::config::SectionId;
use sheltersim_core::world::{METRIC_STATE, MODEL_CONFIGS};
use sheltersim_core::rules::staff_operation_events::SNAPSHOT_PLAN_UNREAD;
use sheltersim_core::run::services::{staff_service, STAFF_SPEC};

pub(crate) use sheltersim_core::rules::staff::OpenWait;

use sheltersim_core::config::staff::{known_staff_operations, support_task_minutes};

use crate::core::errors::{self, OrPy};
use crate::core::row::{attribute, exact_int, exact_str, unsupported};
use crate::core::run::{policies_of, read_object};
use crate::core::world_core::{with_world, PyWorldCell};
use crate::host::{absent, PyHost};
use crate::core::geometry::{Geo, GeoPy};
use crate::rules::staff_operation_events::Gathered;
use crate::rules::travel::{self, read_queue_rows, rows_of, staff_router_rows, StaffRouter};
use crate::state::events::{forget_taken, write_result};
use crate::Unsupported;

// ---- What the rules read of the Python world -----------------------------------------------------

/// What the staff rules read of the Python world, read once per entry before the core borrows it:
/// the core's reader of the holding (`tick::arbitrate::Staff`), read now through the native host.
fn staff_of(world: &Bound<'_, PyAny>) -> PyResult<Staff> {
    Staff::of(&*holding_of_staff(&mut PyHost::of_entry(world))?).or_py()
}

/// The namespaces and fields the staff rules read, of the holding read now: what `Staff::of`
/// reads, `metric_state` and the configs.
const STAFF_NAMES: [&str; 4] = ["staff_operations_v1", "staff_dynamics_v1", "body_state_v1", "shared_operations_v1"];

fn holding_of_staff(host: &mut PyHost<'_>) -> PyResult<std::sync::Arc<sheltersim_core::host::Holding>> {
    host.holding_of(&STAFF_NAMES, &[METRIC_STATE, MODEL_CONFIGS])
}

/// The core's result as Python's: a step's refusal where the floor plan could not be read is what
/// `Geo::of` raised, as that step's `KernelRefusal`; the reads of the tasks' inputs are raised where
/// the core read them (`Gathered::finish`).
fn finish<T>(world: &Bound<'_, PyAny>, result: WriteResult<T>, tasks: Option<&Gathered>) -> PyResult<T> {
    let py = world.py();
    let result = match result {
        Err(WriteError::Error(Error::Refused { label, reason })) if reason == PLAN_UNREAD || reason == SNAPSHOT_PLAN_UNREAD => match Geo::of(world) {
            Err(read) => return Err(errors::kernel_refusal(py, &label, read)),
            Ok(_) => Err(WriteError::Error(Error::Refused { label, reason })),
        },
        other => other,
    };
    let result = write_result(py, result);
    match tasks {
        Some(tasks) => tasks.finish(py, result),
        None => result,
    }
}

// ---- StaffService.assign_pending -----------------------------------------------------------------

/// The service's parameters as read (`STAFF_SPEC`): a read that raised is raised again by reading
/// the service again (policy E14).
fn service_of<'py>(read: Result<Service, sheltersim_core::config::Fault>, service: impl FnOnce() -> PyResult<Bound<'py, PyAny>>) -> PyResult<Service> {
    match read {
        Ok(parameters) => Ok(parameters),
        Err(fault) if fault.is_refusal() => unsupported_owned(fault.text.to_string()),
        Err(_) => {
            let service = service()?;
            for attr in STAFF_SPEC.attrs {
                attribute(&service, &PyString::intern(service.py(), attr.name))?;
            }
            Err(pyo3::exceptions::PyRuntimeError::new_err("a fault of the staff service did not come again"))
        }
    }
}

fn unsupported_owned<T>(text: String) -> PyResult<T> {
    Err(Unsupported::new_err(text))
}

/// `assign_pending` of the service's parameters on the world: the plain case first, and where a
/// request may be assigned, the queue's rows, the floor plan and the controller's travel. With
/// `validate`, `world.validate()` after (`Host::validate_world`).
fn assign(world: &Bound<'_, PyAny>, controller: Option<&Bound<'_, PyAny>>, service: &Service, minute: i64, validate: bool) -> PyResult<Vec<Arc<str>>> {
    let staff = staff_of(world)?;
    let mut taken = Vec::new();
    let first = with_world(world, |cell| core::assign_pending(cell, &staff.inputs(None), service, minute, None, &mut taken))?;
    let assigned = match finish(world, first, None)? {
        Assigned::Done(assigned) => assigned,
        Assigned::NeedsAll => {
            read_queue_rows(world)?;
            let tasks = Gathered::of(world, true, false)?;
            let task_inputs = tasks.inputs();
            let inputs = staff.inputs(Some(&task_inputs));
            let graph = match (service.resolver, controller) {
                (Step::Own, Some(controller)) => Some(rows_of(controller)?),
                (Step::Own, None) => return unsupported(core::OTHER_RESOLVER),
                _ => None,
            };
            let routing = travel::Inputs::of(world)?;
            let travel = graph.as_ref().map(|graph| Travel { routing: routing.routing(graph), walking: routing.walking() });
            let full = Full { travel };
            let result = with_world(world, |cell| core::assign_pending(cell, &inputs, service, minute, Some(&full), &mut taken))?;
            forget_taken(world, &taken)?;
            match finish(world, result, Some(&tasks))? {
                Assigned::Done(assigned) => assigned,
                Assigned::NeedsAll => return Err(pyo3::exceptions::PyRuntimeError::new_err("the assignment asked for its inputs again")),
            }
        }
    };
    if validate {
        use sheltersim_core::host::Host;
        PyHost::of_entry(world).validate_world().map_err(|error| error.0)?;
    }
    Ok(assigned)
}

/// The controller whose own step this resolver is, if it is one (`Held::Method`): what the Run of
/// a Python caller's service reads.
fn owner_of<'py>(service: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyAny>>> {
    let py = service.py();
    let resolver = attribute(service, intern!(py, "route_resolver"))?;
    let Ok(owner) = resolver.getattr(intern!(py, "__self__")) else {
        return Ok(None);
    };
    Ok(Some(owner))
}

/// `StaffService.assign_pending`, called from Python: its parameters read from the service now, and
/// the controller of its route resolver where that is the controller's own `_route`.
#[pyfunction]
#[pyo3(signature = (service, minute, *, validate_state=true))]
pub(crate) fn assign_pending<'py>(service: &Bound<'py, PyAny>, minute: &Bound<'py, PyAny>, validate_state: bool) -> PyResult<Bound<'py, PyTuple>> {
    let py = service.py();
    let world = attribute(service, intern!(py, "world"))?;
    let at = exact_int(minute)?;
    let owner = owner_of(service)?;
    let root = owner.clone().unwrap_or_else(|| service.clone());
    let parameters = service_of(staff_service(&read_object(service, &STAFF_SPEC, &root)), || Ok(service.clone()))?;
    let controller = match (parameters.resolver, &owner) {
        (Step::Own, Some(owner)) if attribute(owner, intern!(py, "world"))?.is(&world) => Some(owner),
        (Step::Own, _) => return unsupported("the route resolver's controller holds another world"),
        _ => None,
    };
    let assigned = assign(&world, controller, &parameters, at, validate_state)?;
    PyTuple::new(py, assigned.iter().map(|id| &**id))
}


/// `StaffService.assign_pending` when every pending request stays pending, for the tests: its
/// writes and (), or None with nothing changed where the step validates, the minute is not the
/// world's, or a request may be assigned.
#[pyfunction]
#[pyo3(signature = (service, minute, *, validate_state=true))]
fn assign_none<'py>(service: &Bound<'py, PyAny>, minute: &Bound<'py, PyAny>, validate_state: bool) -> PyResult<Option<Bound<'py, PyTuple>>> {
    let py = service.py();
    if validate_state {
        return Ok(None);
    }
    let world = attribute(service, intern!(py, "world"))?;
    let at = exact_int(minute)?;
    let staff = staff_of(&world)?;
    let plain = with_world(&world, |cell| core::assign_plain(cell, &staff.inputs(None), at))?.or_py()?;
    Ok(plain.then(|| PyTuple::empty(py)))
}

// ---- _consider_due_complaints --------------------------------------------------------------------

/// `exact_int(controller.social_policy.limits.complaint_wait_minutes)`, read again for its error.
fn read_threshold(controller: &Bound<'_, PyAny>) -> PyResult<()> {
    let py = controller.py();
    let policy = attribute(controller, intern!(py, "social_policy"))?;
    if !policy.is_none() {
        exact_int(&attribute(&attribute(&policy, intern!(py, "limits"))?, intern!(py, "complaint_wait_minutes"))?)?;
    }
    Ok(())
}

/// `_consider_due_complaints`: the policy's threshold from the Run's `Policies`, and its decision
/// through the host (`social_policy.consider_complaint_response`).
#[pyfunction]
pub(crate) fn consider_due_complaints<'py>(controller: &Bound<'py, PyAny>, minute: &Bound<'py, PyAny>) -> PyResult<()> {
    let py = controller.py();
    let policies = policies_of(controller)?;
    let threshold = match policies.social.complaint_wait_minutes() {
        Ok(threshold) => threshold,
        Err(fault) if fault.is_refusal() => return unsupported_owned(fault.text.to_string()),
        Err(_) => {
            read_threshold(controller)?;
            return Err(pyo3::exceptions::PyRuntimeError::new_err("a fault of the social policy did not come again"));
        }
    };
    let Some(threshold) = threshold else {
        return Ok(());
    };
    let at = exact_int(minute)?;
    let world = attribute(controller, intern!(py, "world"))?;
    let mut host = PyHost::of_controller(controller)?;
    core::consider_due_complaints(&PyWorldCell(&world), &mut host, Some(threshold), at).map_err(|error| error.0)
}

// ---- What the metrics read of each open request ----------------------------------------------------

/// The requests of `_support_wait_snapshot`, in the order given, with the Python IDs.
pub(crate) struct OpenWaits<'py> {
    pub(crate) requests: Vec<OpenWait>,
    ids: Vec<Bound<'py, PyAny>>,
    waits: core::OpenWaits,
}

impl<'py> OpenWaits<'py> {
    pub(crate) fn id_of(&self, request: &OpenWait) -> &Bound<'py, PyAny> {
        &self.ids[request.index]
    }

    pub(crate) fn reasons_of<'a>(&'a self, request: &'a OpenWait) -> Vec<&'a str> {
        self.waits.reasons_of(request)
    }

    pub(crate) fn has_no_qualified_staff(&self, request: &OpenWait) -> bool {
        self.waits.has_no_qualified_staff(request)
    }
}

/// `support_waited_minutes` and `support_wait_reason_codes` of each request of the list, or with
/// `only_open` of each one that is pending or assigned.
pub(crate) fn open_waits<'py>(world: &Bound<'py, PyAny>, list: &Bound<'py, PyList>, sample: i64, only_open: bool) -> PyResult<OpenWaits<'py>> {
    let py = world.py();
    let id_name = intern!(py, "id");
    let mut ids = Vec::with_capacity(list.len());
    let mut keys = Vec::with_capacity(list.len());
    for request in list.iter() {
        let id = attribute(&request, id_name)?;
        keys.push(Arc::<str>::from(exact_str(&id)?));
        ids.push(id);
    }
    let mut waits = with_wait_routes(world, |cell, inputs, routes| core::open_waits(cell, inputs, routes, &keys, sample, only_open))?;
    let requests = std::mem::take(&mut waits.requests);
    Ok(OpenWaits { requests, ids, waits })
}

/// A core function that reads the open requests' waits (`core::open_waits`), called on the world
/// with what the staff rules read of the Python world, and again with the routes' inputs where it
/// answers None (`with_wait_inputs`).
pub(crate) fn with_wait_routes<R>(
    world: &Bound<'_, PyAny>,
    call: impl Fn(&mut sheltersim_core::world::World, &Inputs<'_>, Option<&WaitRoutes<'_>>) -> Result<Option<R>, Error>,
) -> PyResult<R> {
    with_wait_inputs(world, |inputs, routes| with_world(world, |cell| call(cell, inputs, routes))?.or_py())
}

/// What a reader of the open requests' waits takes, given to `call`, which borrows the world itself
/// (one that calls the host gives it back first): first without the routes, and again with the
/// routes' inputs where it answers None. Those are the queue's rows, the floor plan and the
/// sections, read where a request's routes are; a floor plan whose read raised is raised where the
/// core needed it.
pub(crate) fn with_wait_inputs<R>(
    world: &Bound<'_, PyAny>,
    mut call: impl FnMut(&Inputs<'_>, Option<&WaitRoutes<'_>>) -> PyResult<Option<R>>,
) -> PyResult<R> {
    let py = world.py();
    let staff = staff_of(world)?;
    let inputs = staff.inputs(None);
    if let Some(found) = call(&inputs, None)? {
        return Ok(found);
    }
    read_queue_rows(world)?;
    let mut host = PyHost::of_entry(world);
    let holding = holding_of_staff(&mut host)?;
    let loads_staff = travel::staff_held(&holding);
    let geo = Geo::of(world);
    let movement = travel::movement_section(&mut host, &holding)?;
    let support_minutes = host.value_section(&holding, SectionId::SupportTaskMinutes, |configs| &configs.support_task_minutes, support_task_minutes)?;
    let routes = WaitRoutes { loads_staff: &loads_staff, geo: geo.as_ref().ok(), movement: &movement, support_minutes: &support_minutes };
    let found = match (call(&inputs, Some(&routes)), &geo) {
        (Err(error), Err(read)) if error.is_instance_of::<Unsupported>(py) && error.value(py).to_string() == PLAN_UNREAD => return Err(read.clone_ref(py)),
        (found, _) => found?,
    };
    found.ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("the waits asked for their routes again"))
}

/// `_support_wait_snapshot` in `experiments/metrics.py`, for the snapshots `rules/metrics.rs`
/// leaves to the Python rule.
#[pyfunction]
fn wait_snapshot<'py>(world: &Bound<'py, PyAny>, active_support: &Bound<'py, PyAny>, sample_minute: &Bound<'py, PyAny>) -> PyResult<(Bound<'py, PyDict>, Bound<'py, PyList>)> {
    let py = world.py();
    let sample = exact_int(sample_minute)?;
    let list = active_support.downcast_exact::<PyList>().map_err(|_| Unsupported::new_err("the requests are not a list"))?;
    let open = open_waits(world, list, sample, false)?;
    let output = &open.requests;
    // `sorted(active_support, key=agent_id)`, stable.
    let mut order: Vec<usize> = (0..output.len()).collect();
    order.sort_by(|first, second| output[*first].agent.as_bytes().cmp(output[*second].agent.as_bytes()));
    let details = PyDict::new(py);
    for index in &order {
        let row = &output[*index];
        let detail = PyDict::new(py);
        detail.set_item(intern!(py, "request_id"), open.id_of(row))?;
        detail.set_item(intern!(py, "waited_minutes"), row.waited)?;
        detail.set_item(intern!(py, "reason_codes"), PyList::new(py, open.reasons_of(row))?)?;
        details.set_item(&*row.agent, detail)?;
    }
    let mut no_qualified: Vec<&str> = output.iter().filter(|row| open.has_no_qualified_staff(row)).map(|row| &*row.agent).collect();
    no_qualified.sort_unstable();
    Ok((details, PyList::new(py, no_qualified)?))
}

// ---- domain/staff_operations.py --------------------------------------------------------------------

/// `update_staff_operations`: each zone's sanitation falls with who is in it.
#[pyfunction]
pub(crate) fn update_operations(world: &Bound<'_, PyAny>, minute: &Bound<'_, PyAny>) -> PyResult<()> {
    let holding = holding_of_staff(&mut PyHost::of_entry(world))?;
    if absent(&holding, "staff_operations_v1")? {
        return Ok(());
    }
    let at = exact_int(minute)?;
    let staff = Staff::of(&holding).or_py()?;
    with_world(world, |cell| core::update_operations(cell, &staff.inputs(None), at))?.or_py()
}

/// `format(value, "g")`, to compare with Python's in the tests.
#[pyfunction]
fn format_g(value: f64) -> PyResult<String> {
    sheltersim_core::text::g_text(value).or_py()
}

/// `assign_staff_operations`, called from Python with a `SpaceRouter`: that caller's input, whose
/// graph is read as it is (`StaffRouter::Given`).
#[pyfunction]
pub(crate) fn assign_operations<'py>(world: &Bound<'py, PyAny>, minute: &Bound<'py, PyAny>, router: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyTuple>> {
    assign_operations_with(world, minute, StaffRouter::Given(router))
}

/// `assign_staff_operations` with the staff's router of a Run or of a router a caller gave. The
/// plain tick, where no free worker has a task to take, reads neither the router nor the formula.
pub(crate) fn assign_operations_with<'py>(world: &Bound<'py, PyAny>, minute: &Bound<'py, PyAny>, router: StaffRouter<'_, 'py>) -> PyResult<Bound<'py, PyTuple>> {
    let py = world.py();
    let mut host = PyHost::of_entry(world);
    let holding = holding_of_staff(&mut host)?;
    if absent(&holding, "staff_operations_v1")? {
        return Ok(PyTuple::empty(py));
    }
    let at = exact_int(minute)?;
    let known = host.value_section(&holding, SectionId::StaffOperationsKnown, |configs| &configs.staff_operations_known, known_staff_operations)?;
    let staff = Staff::of(&holding).or_py()?;
    let first = with_world(world, |cell| core::assign_operations(cell, &staff.inputs(None), &known, at, None))?;
    let started = match finish(world, first, None)? {
        Operations::Done(started) => started,
        Operations::NeedsRoutes => {
            read_queue_rows(world)?;
            let tasks = Gathered::of(world, true, true)?;
            let graph = staff_router_rows(router)?;
            let holding = host.holding_of(&["movement_v1", "staff_operations_v1"], &[])?;
            let (movement, loads_staff) = (travel::movement_held(&holding), travel::staff_held(&holding));
            let routed = Routed { router: &graph, movement: &movement, loads_staff: &loads_staff };
            let task_inputs = tasks.inputs();
            let inputs = staff.inputs(Some(&task_inputs));
            let result = with_world(world, |cell| core::assign_operations(cell, &inputs, &known, at, Some(&routed)))?;
            match finish(world, result, Some(&tasks))? {
                Operations::Done(started) => started,
                Operations::NeedsRoutes => return Err(pyo3::exceptions::PyRuntimeError::new_err("the tasks asked for their routes again")),
            }
        }
    };
    PyTuple::new(py, started.iter().map(|id| &**id))
}

/// `schedule_staff_operation_relief`: a replacement already at the task takes it over at the shift's
/// boundary.
#[pyfunction]
pub(crate) fn relief<'py>(world: &Bound<'py, PyAny>, minute: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyTuple>> {
    let py = world.py();
    let holding = holding_of_staff(&mut PyHost::of_entry(world))?;
    if absent(&holding, "staff_operations_v1")? {
        return Ok(PyTuple::empty(py));
    }
    let at = exact_int(minute)?;
    read_queue_rows(world)?;
    let staff = Staff::of(&holding).or_py()?;
    let handoffs = with_world(world, |cell| core::relief(cell, &staff.inputs(None), at))?;
    let handoffs = write_result(py, handoffs)?;
    PyTuple::new(py, handoffs.iter().map(|id| &**id))
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(assign_pending, m)?)?;
    m.add_function(wrap_pyfunction!(consider_due_complaints, m)?)?;
    m.add_function(wrap_pyfunction!(assign_none, m)?)?;
    m.add_function(wrap_pyfunction!(wait_snapshot, m)?)?;
    m.add_function(wrap_pyfunction!(update_operations, m)?)?;
    m.add_function(wrap_pyfunction!(assign_operations, m)?)?;
    m.add_function(wrap_pyfunction!(relief, m)?)?;
    m.add_function(wrap_pyfunction!(format_g, m)?)?;
    Ok(())
}
