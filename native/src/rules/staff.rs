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
use sheltersim_core::rules::kit::Held;
use sheltersim_core::rules::overlay::PLAN_UNREAD;
use sheltersim_core::rules::staff::{self as core, Assigned, Full, Inputs, Operations, Routed, Service, Step, Travel, WaitRoutes};
use sheltersim_core::rules::staff_operation_events::SNAPSHOT_PLAN_UNREAD;
use sheltersim_core::run::services::{staff_service, STAFF_SPEC};

pub(crate) use sheltersim_core::rules::staff::OpenWait;

use sheltersim_core::config::staff::{known_staff_operations, support_task_minutes, StaffOperations};

use crate::core::configs;
use crate::core::errors::{self, OrPy};
use crate::core::row::{attribute, exact_int, exact_str, namespace_is_absent, unsupported};
use crate::core::run::{policies_of, read_object};
use crate::core::world_core::{field_held, namespace_held, with_world, PyWorldCell};
use crate::host::PyHost;
use crate::rules::handlers::get;
use crate::rules::movement_geometry::{Geo, GeoPy};
use crate::rules::staff_operation_events::Gathered;
use crate::rules::travel::{self, read_queue_rows, rows_of, staff_router_rows, StaffRouter};
use crate::state::events::{write_result, EventWriter};
use crate::Unsupported;

const OTHER_DOCUMENT: &str = "the world holds another document than the row's";

// ---- What the rules read of the Python world -----------------------------------------------------

/// A namespace as the staff rules take it: absent where `model_state` has no such key or None
/// there, and `Other` where it holds another document than the row's.
fn held(world: &Bound<'_, PyAny>, name: &'static str, other: &'static str) -> PyResult<Held> {
    let key = PyString::intern(world.py(), name);
    if namespace_is_absent(world, &key)? {
        return Ok(Held::Absent);
    }
    namespace_held(world, &key, other)
}

/// What the staff rules read of the Python world, read once per entry before the core borrows it.
struct Staff {
    operations: Held,
    dynamics: Held,
    body: Held,
    shared: Held,
    metric_state_held: bool,
}

impl Staff {
    fn of(world: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            operations: held(world, "staff_operations_v1", OTHER_DOCUMENT)?,
            dynamics: held(world, "staff_dynamics_v1", OTHER_DOCUMENT)?,
            body: held(world, "body_state_v1", OTHER_DOCUMENT)?,
            shared: held(world, "shared_operations_v1", "a value is not a plain dict")?,
            metric_state_held: field_held(world, intern!(world.py(), "metric_state"))?,
        })
    }

    fn inputs<'a>(&'a self, tasks: Option<&'a sheltersim_core::rules::staff_operation_events::Inputs<'a>>) -> Inputs<'a> {
        Inputs {
            operations: &self.operations,
            dynamics: &self.dynamics,
            body: &self.body,
            shared: &self.shared,
            metric_state_held: self.metric_state_held,
            tasks,
        }
    }
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

/// Let go the objects of the queue's slots the core took out (`QueueStore::forget`).
fn forget(world: &Bound<'_, PyAny>, taken: &[u64]) -> PyResult<()> {
    if taken.is_empty() {
        return Ok(());
    }
    EventWriter::of(world)?.queue().get().forget(taken)
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
    let py = world.py();
    let staff = Staff::of(world)?;
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
            forget(world, &taken)?;
            match finish(world, result, Some(&tasks))? {
                Assigned::Done(assigned) => assigned,
                Assigned::NeedsAll => return Err(pyo3::exceptions::PyRuntimeError::new_err("the assignment asked for its inputs again")),
            }
        }
    };
    if validate {
        use sheltersim_core::host::Host;
        PyHost::of_world(world, PyDict::new(py)).validate_world().map_err(|error| error.0)?;
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

/// `_assign_staff_support` of the tick, with the controller's `staff_service` as the Run read it
/// (`Policies`).
pub(crate) fn assign_pending_of_run<'py>(controller: &Bound<'py, PyAny>, world: &Bound<'py, PyAny>, minute: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyTuple>> {
    let py = controller.py();
    let at = exact_int(minute)?;
    let policies = policies_of(controller)?;
    let read = policies.services.staff.object().and_then(|service| staff_service(service));
    let parameters = service_of(read, || controller.getattr(intern!(py, "staff_service")))?;
    let assigned = assign(world, Some(controller), &parameters, at, false)?;
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
    let staff = Staff::of(&world)?;
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
/// `only_open` of each one that is pending or assigned. The routes' inputs (the queue's rows, the
/// floor plan, the sections) are read where a request's routes are.
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
    let staff = Staff::of(world)?;
    let inputs = staff.inputs(None);
    let found = with_world(world, |cell| core::open_waits(cell, &inputs, None, &keys, sample, only_open))?.or_py()?;
    let waits = match found {
        Some(waits) => waits,
        None => {
            read_queue_rows(world)?;
            let loads_staff = travel::staff_held(world)?;
            let geo = Geo::of(world);
            let movement = travel::movement_section(world)?;
            let support_minutes = configs::kept_section(world, |configs| &configs.support_task_minutes, support_task_minutes)?;
            let routes = WaitRoutes { loads_staff: &loads_staff, geo: geo.as_ref().ok(), movement: &movement, support_minutes: &support_minutes };
            let found = with_world(world, |cell| core::open_waits(cell, &inputs, Some(&routes), &keys, sample, only_open))?;
            let found = match (found, &geo) {
                (Err(Error::Unsupported(reason)), Err(read)) if reason == PLAN_UNREAD => return Err(read.clone_ref(py)),
                (found, _) => found.or_py()?,
            };
            found.ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("the waits asked for their routes again"))?
        }
    };
    let mut waits = waits;
    let requests = std::mem::take(&mut waits.requests);
    Ok(OpenWaits { requests, ids, waits })
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
    if namespace_is_absent(world, intern!(world.py(), "staff_operations_v1"))? {
        return Ok(());
    }
    let at = exact_int(minute)?;
    let staff = Staff::of(world)?;
    with_world(world, |cell| core::update_operations(cell, &staff.inputs(None), at))?.or_py()
}

/// `format(value, "g")`, to compare with Python's in the tests.
#[pyfunction]
fn format_g(value: f64) -> PyResult<String> {
    sheltersim_core::text::g_text(value).or_py()
}

/// `create_staff_operations_formula(model_configs)` and whether `policy_registry` selects it, kept
/// for the configs' stamp as their section (`core/configs.rs`). What the factory raises is relayed
/// as it is. A build without Python takes the built-in formulas (`StaffOperations::parse`).
pub(crate) fn staff_operations(world: &Bound<'_, PyAny>) -> PyResult<Arc<StaffOperations>> {
    let py = world.py();
    let parse = || -> PyResult<StaffOperations> {
        let configs = attribute(world, intern!(py, "model_configs"))?;
        let formula = py.import("sheltersim.policies.registry")?.getattr("create_staff_operations_formula")?.call1((&configs,))?;
        let selected = get(&configs, "policy_registry")?;
        let selected = !selected.is_none() && selected.contains("staff_operations")?;
        let model_id = Arc::from(exact_str(&attribute(&formula, intern!(py, "model_id"))?)?);
        let selected_source = match selected {
            true => Some(Arc::from(exact_str(&attribute(&formula, intern!(py, "source_id"))?)?)),
            false => None,
        };
        Ok(StaffOperations { model_id, selected_source })
    };
    match configs::kept(world, |configs| &configs.staff_operations, || Ok(configs::parsed(py, parse())))? {
        Ok(formula) => Ok(formula),
        Err(fault) => Err(configs::raise(&fault, parse)),
    }
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
    if namespace_is_absent(world, intern!(py, "staff_operations_v1"))? {
        return Ok(PyTuple::empty(py));
    }
    let at = exact_int(minute)?;
    let known = configs::kept_section(world, |configs| &configs.staff_operations_known, known_staff_operations)?;
    let staff = Staff::of(world)?;
    let first = with_world(world, |cell| core::assign_operations(cell, &staff.inputs(None), &known, at, None))?;
    let started = match finish(world, first, None)? {
        Operations::Done(started) => started,
        Operations::NeedsRoutes => {
            read_queue_rows(world)?;
            let tasks = Gathered::of(world, true, true)?;
            let graph = staff_router_rows(router)?;
            let (movement, loads_staff) = (travel::movement_held(world)?, travel::staff_held(world)?);
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
    if namespace_is_absent(world, intern!(py, "staff_operations_v1"))? {
        return Ok(PyTuple::empty(py));
    }
    let at = exact_int(minute)?;
    read_queue_rows(world)?;
    let staff = Staff::of(world)?;
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
