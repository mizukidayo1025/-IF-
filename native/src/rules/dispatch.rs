//! The dispatch phase (`experiments/phases/dispatch.py`): the glue of the core's
//! `sheltersim_core::rules::dispatch` (lane P9-1c of Issue #152).
//!
//! `handlers.py` installs the table: the handler classes the core runs, each with its `apply` as
//! defined. The Run reads the dispatcher's handlers by it (`read_dispatch`, `Policies.dispatch`, H3);
//! before each phase the glue checks by identity that the dispatcher, its handlers, their classes'
//! `apply` and the controller's managers are what the Run read, and reads them again where they are
//! not (a test registers or patches a handler after the controller starts). The phase itself is one
//! call of the core with one native host (`PyHost`): the core runs a handler it knows, and gives any
//! other to the host (`Host::apply_foreign`, H4).
//!
//! What the phase turns down before its first event is `Unsupported` (policy E10: the tick goes
//! through `phase_handler`); past that, a refusal is `KernelRefusal` (`errors::kernel_refusal`).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::sync::GILOnceCell;
use pyo3::types::{PyBool, PyDict, PyString, PyTuple, PyType};

use sheltersim_core::rules::dispatch as core;
use sheltersim_core::rules::tick::{Seed, Tick, ZoneNeighbors};

use crate::core::errors;
use crate::core::row::{attribute, exact_int};
use crate::core::run::run_of;
use crate::core::world_core::{raw_dict_get, PyWorldCell};
use crate::host::{HostError, PyHost};
use crate::Unsupported;
use indexmap::IndexMap;

/// What Python gives once: the classes of the table's handlers and their `apply` as defined.
struct Table {
    social: (Py<PyType>, Py<PyAny>),
    travel: (Py<PyType>, Py<PyAny>),
    toilet: (Py<PyType>, Py<PyAny>),
    resource: (Py<PyType>, Py<PyAny>),
    timeout: (Py<PyType>, Py<PyAny>),
    support: (Py<PyType>, Py<PyAny>),
    /// The handlers whose `apply` is a kernel, which the loop calls with the Python dispatcher's steps
    /// around it: the classes, each with its `apply` as defined.
    others: Vec<(Py<PyType>, Py<PyAny>)>,
    /// `AgentTravelCompletionHandler._after_arrival` as defined.
    after_arrival: Py<PyAny>,
    dispatcher: Py<PyType>,
}

static TABLE: GILOnceCell<Table> = GILOnceCell::new();
/// `DispatchPhase._schedule_return_to_initial_zone`, as `phases/dispatch.py` defines it.
static RETURN_RULE: GILOnceCell<Py<PyAny>> = GILOnceCell::new();

/// The controller whose own `_schedule_return_to_initial_zone` this `after_complete` is, if it is.
pub(crate) fn return_rule_owner<'py>(after: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyAny>>> {
    let py = after.py();
    if after.is_none() {
        return Ok(None);
    }
    Ok(match (after.getattr(intern!(py, "__self__")), after.getattr(intern!(py, "__func__")), RETURN_RULE.get(py)) {
        (Ok(owner), Ok(rule), Some(known)) if rule.is(known.bind(py)) => Some(owner),
        _ => None,
    })
}

/// Whether `function` is `DispatchPhase._schedule_return_to_initial_zone` as `phases/dispatch.py`
/// defines it (lane P9-0: `Held::Method` of the Run's policies).
pub(crate) fn is_return_rule(function: &Bound<'_, PyAny>) -> bool {
    RETURN_RULE.get(function.py()).is_some_and(|known| function.is(known.bind(function.py())))
}

/// `controller.event_dispatcher` and its handlers as the Run reads them (lane P9-0, H3 of
/// design-p9.md): a handler whose class is one the table was installed with, and whose class's
/// `apply` is the one installed, is the core's, with what its kind reads of it; any other is the
/// host's (`Foreign`). Reading never raises: what a read raised is the fault the core says.
pub(crate) fn read_dispatch(controller: &Bound<'_, PyAny>) -> sheltersim_core::run::dispatch::Dispatch {
    use sheltersim_core::run::dispatch::{Dispatch, NO_TABLE};
    let py = controller.py();
    let Some(table) = TABLE.get(py) else {
        return Dispatch::faulted(sheltersim_core::config::Fault::refused(NO_TABLE));
    };
    let read = || -> PyResult<Dispatch> {
        let dispatcher = controller.getattr(intern!(py, "event_dispatcher"))?;
        let known = dispatcher.get_type().is(table.dispatcher.bind(py));
        let handlers = dispatcher.getattr(intern!(py, "handlers"))?;
        let Some(handlers) = crate::core::tracked::as_readable_dict(&handlers) else {
            return Ok(Dispatch { known: Ok(known), handlers: Err(sheltersim_core::config::Fault::refused("a value is not a plain dict")) });
        };
        let mut read = IndexMap::with_capacity(handlers.len());
        for (event_type, handler) in handlers.iter() {
            let Some(event_type) = event_type.downcast::<PyString>().ok().and_then(|text| text.to_str().ok().map(Arc::<str>::from)) else {
                continue;
            };
            // `handlers.get(event_type)` of None is no handler: the event fails with its text.
            if handler.is_none() {
                continue;
            }
            read.insert(event_type, handler_of(table, controller, &handler)?);
        }
        Ok(Dispatch { known: Ok(known), handlers: Ok(Arc::new(read)) })
    };
    read().unwrap_or_else(|error| Dispatch::faulted(crate::core::configs::fault_of(py, &error)))
}

/// A registered handler as the core takes it.
fn handler_of(table: &Table, controller: &Bound<'_, PyAny>, handler: &Bound<'_, PyAny>) -> PyResult<sheltersim_core::run::dispatch::Handler> {
    use sheltersim_core::run::dispatch::{Handler, HandlerKind, Native};
    let py = handler.py();
    let entries = [&table.social, &table.travel, &table.toilet, &table.resource, &table.timeout, &table.support];
    let kind = handler.get_type();
    let Some((class, apply)) = entries.into_iter().chain(table.others.iter()).find(|(class, _)| kind.is(class.bind(py))) else {
        return Ok(Handler::Foreign);
    };
    if !class.bind(py).getattr(intern!(py, "apply"))?.is(apply.bind(py)) {
        return Ok(Handler::Foreign);
    }
    let text = |name: &Bound<'_, PyString>| -> Option<String> { kind.getattr(name).ok()?.extract::<String>().ok() };
    let (Some(module), Some(qualname)) = (text(intern!(py, "__module__")), text(intern!(py, "__qualname__"))) else {
        return Ok(Handler::Foreign);
    };
    let Some(handler_kind) = HandlerKind::of_class(&module, &qualname) else {
        return Ok(Handler::Foreign);
    };
    let same = |own: &str, controllers: &str| -> bool {
        match (handler.getattr(own), controller.getattr(controllers)) {
            (Ok(mine), Ok(theirs)) => mine.is(&theirs),
            _ => false,
        }
    };
    let own_managers = match handler_kind {
        HandlerKind::ResourceServiceCompletion | HandlerKind::ResourceServiceCancellation => same("service", "resource_service"),
        HandlerKind::AgentTravelCompletion => same("toilet_service", "toilet_service") && same("resource_service", "resource_service"),
        _ => true,
    };
    let after_arrival_as_defined = match handler_kind {
        HandlerKind::AgentTravelCompletion => kind.getattr(intern!(py, "_after_arrival")).is_ok_and(|defined| defined.is(table.after_arrival.bind(py))),
        _ => true,
    };
    let read = crate::core::run::read_object(handler, handler_kind.spec(), controller);
    Ok(Handler::Native(Native { kind: handler_kind, handler: Arc::new(read), own_managers, after_arrival_as_defined }))
}

// ---- The phase ---------------------------------------------------------------------------------------

static PHASES_REFUSED: AtomicU64 = AtomicU64::new(0);
static REFUSALS: Mutex<BTreeMap<String, u64>> = Mutex::new(BTreeMap::new());

/// Unsupported before the phase begins, counted by its reason.
fn refuse<T>(reason: &str) -> PyResult<T> {
    PHASES_REFUSED.fetch_add(1, Ordering::Relaxed);
    if let Ok(mut refusals) = REFUSALS.lock() {
        *refusals.entry(format!("phase: {reason}")).or_insert(0) += 1;
    }
    Err(Unsupported::new_err(reason.to_owned()))
}

/// `_run_dispatch_phase` (the E10 phase path, a `KERNEL_ONLY_SEGMENTS` step).
#[pyfunction]
fn run_phase<'py>(controller: &Bound<'py, PyAny>, world: &Bound<'py, PyAny>, minute: &Bound<'py, PyAny>) -> PyResult<()> {
    let py = world.py();
    let minute_value = match ready(controller, world, minute)? {
        Ok(minute_value) => minute_value,
        Err(reason) => return refuse(&reason),
    };
    let run = run_of(controller)?;
    let policies = run.get().policies()?;
    let mut host = PyHost::of_controller(controller)?.on_world(world);
    let cell = PyWorldCell(world);
    let neighbors = Arc::new(ZoneNeighbors { social: Err(UNREAD), information: Err(UNREAD) });
    let mut tick = Tick { cell: &cell, host: &mut host, run: run.get(), policies, minute: minute_value, seed: Seed::Other, neighbors };
    // The phase has begun: a refusal past its checks is KernelRefusal too.
    core::run(&mut tick).map_err(|error: HostError| errors::kernel_refusal(py, core::RUN_PHASE, error.0))
}

/// What the phase checks before its first event, for `run_phase` and the tick driver
/// (`rules/tick.rs`, whose core `run_tick` runs the phase): the minute, or the reason the phase is
/// refused (counted by the caller). The dispatcher's handlers are read again where they changed.
pub(crate) fn ready<'py>(controller: &Bound<'py, PyAny>, world: &Bound<'py, PyAny>, minute: &Bound<'py, PyAny>) -> PyResult<Result<i64, String>> {
    let py = world.py();
    let Some(table) = TABLE.get(py) else {
        return Ok(Err("events/handlers.py has not installed the table".to_owned()));
    };
    let Ok(minute_value) = exact_int(minute) else {
        return Ok(Err("a minute is not an int".to_owned()));
    };
    if minute_value < 0 {
        return Ok(Err("a negative minute".to_owned()));
    }
    let dispatcher = attribute(controller, intern!(py, "event_dispatcher"))?;
    if !dispatcher.get_type().is(table.dispatcher.bind(py)) {
        return Ok(Err("another kind of dispatcher".to_owned()));
    }
    if let Err(error) = crate::state::events::writable(world) {
        return Ok(Err(error.value(py).to_string()));
    }
    let Ok(run) = run_of(controller) else {
        return Ok(Err("the controller has no run core".to_owned()));
    };
    // The open actions the core closes are the Run's rows: a store that holds a plain dict is read
    // into them first.
    let active = attribute(controller, intern!(py, "active_actions"))?;
    let Ok(active) = active.downcast::<crate::rules::action_lifecycle::ActiveActions>() else {
        return Ok(Err("the open actions are not a store of the Run".to_owned()));
    };
    if !active.borrow().is_of(run.get()) {
        return Ok(Err("the open actions are not a store of the Run".to_owned()));
    }
    if !active.try_borrow_mut()?.fits_rows(py) {
        return Ok(Err("the open actions do not fit their rows".to_owned()));
    }
    marks(table, controller, &dispatcher, run.get())?;
    Ok(Ok(minute_value))
}

/// What the dispatch reads of the controller's neighbors: nothing.
const UNREAD: &str = "the dispatch reads no neighbors";

/// The controller's objects the Run read the dispatcher's handlers from: the dispatcher and its
/// handlers, each handler's class's `apply`, the travel handler's class's `_after_arrival`, and the
/// controller's managers with whether each has a `request` of its own. What they were when the Run
/// last read them is the controller's `_dispatch_marks`; any other is read again.
fn marks(table: &Table, controller: &Bound<'_, PyAny>, dispatcher: &Bound<'_, PyAny>, run: &crate::core::run::RunCore) -> PyResult<()> {
    let py = controller.py();
    let handlers = attribute(dispatcher, intern!(py, "handlers"))?;
    let mut now: Vec<Bound<'_, PyAny>> = vec![dispatcher.clone(), handlers.clone()];
    match crate::core::tracked::as_readable_dict(&handlers) {
        Some(handlers) => {
            for (event_type, handler) in handlers.iter() {
                let apply = handler.get_type().getattr(intern!(py, "apply")).unwrap_or_else(|_| py.None().into_bound(py));
                now.extend([event_type, handler, apply]);
            }
        }
        None => now.push(py.None().into_bound(py)),
    }
    now.push(table.travel.0.bind(py).getattr(intern!(py, "_after_arrival"))?);
    for name in [intern!(py, "toilet_service"), intern!(py, "resource_service")] {
        let manager = attribute(controller, name)?;
        let own = match manager.getattr(intern!(py, "__dict__")) {
            Ok(fields) => raw_dict_get(&fields, intern!(py, "request").as_any())?.is_some(),
            Err(_) => false,
        };
        now.push(manager);
        now.push(PyBool::new(py, own).to_owned().into_any());
    }
    let fields = controller.getattr(intern!(py, "__dict__"))?;
    let kept = raw_dict_get(&fields, intern!(py, "_dispatch_marks").as_any())?;
    let same = kept.as_ref().and_then(|kept| kept.downcast::<PyTuple>().ok()).is_some_and(|kept| {
        kept.len() == now.len() && now.iter().enumerate().all(|(index, item)| kept.get_item(index).is_ok_and(|held| held.is(item)))
    });
    if same {
        return Ok(());
    }
    run.refresh_dispatch_of(controller)?;
    fields.set_item(intern!(py, "_dispatch_marks"), PyTuple::new(py, now)?)
}

/// `_sync_movement_after_events`, a body-less `kernel_only` step of `phases/dispatch.py`.
#[pyfunction]
fn sync_movement(controller: &Bound<'_, PyAny>) -> PyResult<()> {
    let py = controller.py();
    let world = attribute(controller, intern!(py, "world"))?;
    let run = run_of(controller)?;
    let policies = run.get().policies()?;
    let minute = exact_int(&attribute(&world, intern!(py, "minute_index"))?)?;
    let mut host = PyHost::of_controller(controller)?;
    let cell = PyWorldCell(&world);
    let neighbors = Arc::new(ZoneNeighbors { social: Err(UNREAD), information: Err(UNREAD) });
    let mut tick = Tick { cell: &cell, host: &mut host, run: run.get(), policies, minute, seed: Seed::Other, neighbors };
    core::sync_movement(&mut tick).map_err(|error| error.0)
}

/// `_consider_social_contact_event(event)`, a body-less `kernel_only` step of `phases/dispatch.py`:
/// nothing for another type of event.
#[pyfunction]
fn consider_contact(controller: &Bound<'_, PyAny>, event: &Bound<'_, PyAny>) -> PyResult<()> {
    let py = controller.py();
    let row = crate::rules::information_events::event_row(event)?;
    if &*row.event_type != "information_contact_update" {
        return Ok(());
    }
    let world = attribute(controller, intern!(py, "world"))?;
    let run = run_of(controller)?;
    let policies = run.get().policies()?;
    let minute = exact_int(&attribute(&world, intern!(py, "minute_index"))?)?;
    let mut host = PyHost::of_controller(controller)?;
    let cell = PyWorldCell(&world);
    let neighbors = Arc::new(ZoneNeighbors { social: Err(UNREAD), information: Err(UNREAD) });
    let mut tick = Tick { cell: &cell, host: &mut host, run: run.get(), policies, minute, seed: Seed::Other, neighbors };
    core::consider_contact(&mut tick, &row).map_err(|error| error.0)
}

/// What `events/handlers.py` gives once: the handlers the table stands for, as defined.
#[pyfunction]
#[allow(clippy::too_many_arguments)]
fn install_table<'py>(
    py: Python<'py>,
    social: (Bound<'py, PyType>, Bound<'py, PyAny>),
    travel: (Bound<'py, PyType>, Bound<'py, PyAny>),
    toilet: (Bound<'py, PyType>, Bound<'py, PyAny>),
    resource: (Bound<'py, PyType>, Bound<'py, PyAny>),
    timeout: (Bound<'py, PyType>, Bound<'py, PyAny>),
    support: (Bound<'py, PyType>, Bound<'py, PyAny>),
    others: Vec<(Bound<'py, PyType>, Bound<'py, PyAny>)>,
    after_arrival: Bound<'py, PyAny>,
    dispatcher: Bound<'py, PyType>,
) -> PyResult<()> {
    let table = Table {
        social: (social.0.unbind(), social.1.unbind()),
        travel: (travel.0.unbind(), travel.1.unbind()),
        toilet: (toilet.0.unbind(), toilet.1.unbind()),
        resource: (resource.0.unbind(), resource.1.unbind()),
        timeout: (timeout.0.unbind(), timeout.1.unbind()),
        support: (support.0.unbind(), support.1.unbind()),
        others: others.into_iter().map(|(class, apply)| (class.unbind(), apply.unbind())).collect(),
        after_arrival: after_arrival.unbind(),
        dispatcher: dispatcher.unbind(),
    };
    // A second import of the module, as `importlib.reload` makes, keeps the first table: the
    // classes then differ from the handlers', which go to the host.
    let _ = TABLE.set(py, table);
    Ok(())
}

/// What `phases/dispatch.py` gives once: its step that a toilet's completion leads to.
#[pyfunction]
fn install_return_rule(py: Python<'_>, rule: Bound<'_, PyAny>) -> PyResult<()> {
    let _ = RETURN_RULE.set(py, rule.unbind());
    Ok(())
}

/// What has run in this process: `(counts, refusals by reason)`.
#[pyfunction]
fn stats<'py>(py: Python<'py>) -> PyResult<(Bound<'py, PyDict>, Bound<'py, PyDict>)> {
    let counts = PyDict::new(py);
    for (name, times) in core::COUNTS.all() {
        counts.set_item(name, times)?;
    }
    counts.set_item("phases_refused", PHASES_REFUSED.load(Ordering::Relaxed))?;
    for (label, times) in core::KINDS.ran_counts() {
        counts.set_item(format!("ran.{label}"), times)?;
    }
    let reasons = PyDict::new(py);
    if let Ok(refusals) = REFUSALS.lock() {
        for (reason, times) in refusals.iter() {
            reasons.set_item(reason, *times)?;
        }
    }
    for (reason, times) in core::KINDS.refusal_counts() {
        reasons.set_item(reason, times)?;
    }
    Ok((counts, reasons))
}

// ---- A dispatcher a caller holds alone (decision D3, lane P9-3b) -------------------------------------

/// A tick of `minute` on `world` for the dispatcher alone: its host and policies
/// (`PyHost::of_dispatcher`), and a Run of its own, which nothing reads.
fn alone<'py, T>(
    dispatcher: &Bound<'py, PyAny>,
    world: &Bound<'py, PyAny>,
    event: &Bound<'py, PyAny>,
    minute: &Bound<'py, PyAny>,
    f: impl FnOnce(&mut Tick<'_, PyWorldCell<'_, 'py>, PyHost<'py>, std::cell::RefCell<sheltersim_core::run::Run>>, &sheltersim_core::events::EventRow) -> Result<T, HostError>,
) -> PyResult<(T, PyHost<'py>)> {
    use crate::state::events::EventRowPy;
    let minute = exact_int(minute)?;
    let row = sheltersim_core::events::EventRow::from_event(event)?.ok_or_else(|| Unsupported::new_err(core::NO_ROW))?;
    let (mut host, policies) = PyHost::of_dispatcher(dispatcher, world, event)?;
    let cell = PyWorldCell(world);
    let run = std::cell::RefCell::new(sheltersim_core::run::Run::new(sheltersim_core::run::RunConfig { route_capacities: Err("the dispatcher has no run") }));
    let neighbors = Arc::new(ZoneNeighbors { social: Ok(Default::default()), information: Ok(Default::default()) });
    let found = {
        let mut tick = Tick { cell: &cell, host: &mut host, run: &run, policies, minute, seed: Seed::Other, neighbors };
        f(&mut tick, &row)
    };
    Ok((found.map_err(|error| error.0)?, host))
}

/// `EventDispatcher.dispatch_one` without its `after_event`: the core's (`one_alone`). The records of
/// the event, the event's first, or none where it left the queue or was replaced.
#[pyfunction]
fn dispatch_one<'py>(
    dispatcher: &Bound<'py, PyAny>,
    world: &Bound<'py, PyAny>,
    event: &Bound<'py, PyAny>,
    minute: &Bound<'py, PyAny>,
) -> PyResult<Vec<Bound<'py, PyAny>>> {
    let (ids, host) = alone(dispatcher, world, event, minute, |tick, row| core::one_alone(tick, row))?;
    match ids {
        Some(ids) => host.records_of(&ids),
        None => Ok(Vec::new()),
    }
}

/// `EventDispatcher.settle`: the core's (`settle_alone`). The log's record of the event.
#[pyfunction]
fn settle<'py>(
    dispatcher: &Bound<'py, PyAny>,
    world: &Bound<'py, PyAny>,
    event: &Bound<'py, PyAny>,
    minute: &Bound<'py, PyAny>,
    status: &Bound<'py, PyAny>,
    detail: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = dispatcher.py();
    let status = crate::core::row::exact_str(&attribute(status, intern!(py, "value"))?)
        .ok()
        .and_then(sheltersim_core::events::EventStatus::parse)
        .ok_or_else(|| Unsupported::new_err("a status is not an EventStatus"))?;
    let detail = if detail.is_none() { None } else { Some(crate::core::row::exact_str(detail)?.to_owned()) };
    let ((), host) = alone(dispatcher, world, event, minute, |tick, row| core::settle_alone(tick, row, status, detail.as_deref()))?;
    let id: Arc<str> = Arc::from(crate::core::row::exact_str(&attribute(event, intern!(py, "event_id"))?)?);
    let mut found = host.records_of(&[id])?;
    found.pop().ok_or_else(|| Unsupported::new_err("the record went"))
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(run_phase, m)?)?;
    m.add_function(wrap_pyfunction!(dispatch_one, m)?)?;
    m.add_function(wrap_pyfunction!(settle, m)?)?;
    m.add_function(wrap_pyfunction!(sync_movement, m)?)?;
    m.add_function(wrap_pyfunction!(consider_contact, m)?)?;
    m.add_function(wrap_pyfunction!(install_table, m)?)?;
    m.add_function(wrap_pyfunction!(install_return_rule, m)?)?;
    m.add_function(wrap_pyfunction!(stats, m)?)?;
    Ok(())
}

