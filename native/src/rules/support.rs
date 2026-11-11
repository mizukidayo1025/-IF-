//! `domain/staff.py`'s support requests and the end of a staff member's support: the glue of the
//! core's kernels (`sheltersim_core::rules::support`, lane P8 b4 of Issue #152). Each entry reads
//! what Python holds (whether `model_state` holds `shared_operations_v1` and `body_state_v1`,
//! whether `metric_state` is its slot's tree, the event's IDs, the handler's and the service's
//! values, and the selected care formula where the care needs it), runs the core on the world
//! borrowed whole, and makes the Python values of what it returns.

use std::sync::Arc;

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyString, PyTuple};
use sheltersim_core::config::body::CareOutcome;
use sheltersim_core::error::Error;
use sheltersim_core::rules::kit::{self, Held, PRESENCE};
use sheltersim_core::rules::support::{
    self as core, Completion, Formula, Handler, Inputs, RequestService, SupportEvent, AFTER_UNREAD, COOLDOWN_UNREAD, NAMESPACE_IS_NONE, OTHER_DOCUMENT,
    POLICY_UNREAD,
};

pub(crate) use sheltersim_core::rules::support::PlannedRequest;

use crate::core::configs;
use crate::core::errors::{self, OrPy};
use crate::core::row::{attribute, exact_dict, exact_int, exact_str, namespace_is_absent};
use crate::core::value::{VPy, V};
use crate::core::world_core::{field_held, namespace_held, with_state_value, with_world, NamespaceMirror};
use crate::rules::care_receipts::{transition_record, PlannedTransition};
use crate::rules::service::add_one;
use crate::state::body_state::NAME as BODY;
use crate::state::entities::{entities_of, EntityStore};
use crate::state::events::{write_result, EventRef, EventWriter};
use crate::state::shared_operations::NAME as SHARED;
use crate::Unsupported;

// ---- What the rules read of the Python world -----------------------------------------------------

/// `shared_operations_v1` as the support rules take it: absent from `model_state`, None there
/// (`NAMESPACE_IS_NONE`), the slot's tree, or another document.
fn shared_held(world: &Bound<'_, PyAny>) -> PyResult<Held> {
    let py = world.py();
    let key = PyString::intern(py, SHARED);
    if namespace_is_absent(world, &key)? {
        let table = attribute(world, intern!(py, "model_state"))?;
        return Ok(if exact_dict(&table)?.contains(&key)? { Held::Other(NAMESPACE_IS_NONE) } else { Held::Absent });
    }
    namespace_held(world, &key, OTHER_DOCUMENT)
}

/// Whether `metric_state` is its slot's tree.
fn metric_state_held(world: &Bound<'_, PyAny>) -> PyResult<bool> {
    field_held(world, intern!(world.py(), "metric_state"))
}

/// What the rules of the receipts and of the end of support read.
fn inputs_of(world: &Bound<'_, PyAny>) -> PyResult<Inputs> {
    let py = world.py();
    Ok(Inputs {
        shared: shared_held(world)?,
        body_present: !namespace_is_absent(world, intern!(py, BODY))?,
        metric_state_held: metric_state_held(world)?,
    })
}

/// The event's ID and due minute, and the payload's values of these keys that are str.
fn event_of(event: EventRef<'_, '_>, keys: [bool; 4]) -> PyResult<SupportEvent> {
    let text = |wanted: bool, key: &str| if wanted { event.text(key) } else { Ok(None) };
    Ok(SupportEvent {
        request_id: text(keys[0], "request_id")?,
        agent_id: text(keys[1], "agent_id")?,
        staff_id: text(keys[2], "staff_id")?,
        task_id: text(keys[3], "task_id")?,
        event_id: event.event_id()?,
        due_minute: event.due_minute()?,
    })
}

// ---- A request times out ------------------------------------------------------------------------------

/// `SupportRequestTimeoutHandler.apply` up to the error text: `Some(text)` when Python would suppress
/// the event. `cooldown_minutes` is the handler's.
pub(crate) fn timeout_event<'py>(world: &Bound<'py, PyAny>, event: EventRef<'_, 'py>, cooldown_minutes: i64) -> PyResult<Option<&'static str>> {
    let event = event_of(event, [true, true, false, false])?;
    let inputs = Inputs { shared: Held::Absent, body_present: false, metric_state_held: metric_state_held(world)? };
    with_world(world, |cell| core::timeout(cell, &inputs, &event, cooldown_minutes))?.or_py()
}

/// `SupportRequestTimeoutHandler.apply`, called from Python.
#[pyfunction]
fn timeout<'py>(handler: &Bound<'py, PyAny>, world: &Bound<'py, PyAny>, event: &Bound<'py, PyAny>) -> PyResult<()> {
    let py = handler.py();
    let cooldown = exact_int(&attribute(handler, intern!(py, "cooldown_minutes"))?)?;
    if let Some(text) = timeout_event(world, EventRef::Object(event), cooldown)? {
        return errors::suppressed(py, text);
    }
    Ok(())
}

// ---- The care receipts around a request ------------------------------------------------------------

/// `_record_request_interrupted`, of an applied `support_request_timeout`.
pub(crate) fn record_interrupted(world: &Bound<'_, PyAny>, event: EventRef<'_, '_>) -> PyResult<()> {
    let event = event_of(event, [true, false, false, false])?;
    if event.request_id.is_none() {
        return Ok(());
    }
    let inputs = inputs_of(world)?;
    write_result(world.py(), with_world(world, |cell| core::record_interrupted(cell, &inputs, &event))?)
}

/// `_record_staff_support_started`, of an applied `agent_travel_complete` of a staff trip.
pub(crate) fn record_started(world: &Bound<'_, PyAny>, event: EventRef<'_, '_>) -> PyResult<()> {
    let event = event_of(event, [true, false, true, false])?;
    if event.request_id.is_none() {
        return Ok(());
    }
    let inputs = inputs_of(world)?;
    write_result(world.py(), with_world(world, |cell| core::record_started(cell, &inputs, &event))?)
}

/// `_record_request_interrupted`, called from Python.
#[pyfunction]
#[pyo3(name = "record_interrupted")]
fn record_interrupted_step<'py>(_controller: &Bound<'py, PyAny>, world: &Bound<'py, PyAny>, event: &Bound<'py, PyAny>) -> PyResult<()> {
    record_interrupted(world, EventRef::Object(event))
}

/// `_record_staff_support_started`, called from Python.
#[pyfunction]
#[pyo3(name = "record_started")]
fn record_started_step<'py>(_controller: &Bound<'py, PyAny>, world: &Bound<'py, PyAny>, event: &Bound<'py, PyAny>) -> PyResult<()> {
    record_started(world, EventRef::Object(event))
}

/// `_assign_staff_support`: the service's assignment, then the receipts of what it assigned.
#[pyfunction]
fn assign_staff_support<'py>(controller: &Bound<'py, PyAny>, world: &Bound<'py, PyAny>, minute: &Bound<'py, PyAny>) -> PyResult<()> {
    let py = world.py();
    let kwargs = PyDict::new(py);
    kwargs.set_item("validate_state", false)?;
    let assigned = attribute(controller, intern!(py, "staff_service"))?.call_method(intern!(py, "assign_pending"), (minute,), Some(&kwargs))?;
    if assigned.is_empty().unwrap_or(false) {
        return Ok(());
    }
    record_assigned(world, &assigned)
}

/// `_record_assigned_support`: the receipts of the requests just assigned. What it writes is not
/// undone, so what would refuse is an error here (the driver's and `_assign_staff_support`'s callers
/// go on).
pub(crate) fn record_assigned<'py>(world: &Bound<'py, PyAny>, assigned: &Bound<'py, PyAny>) -> PyResult<()> {
    let mut ids = Vec::new();
    for request_id in assigned.try_iter()? {
        let request_id = request_id?;
        let id = exact_str(&request_id).map_err(|_| pyo3::exceptions::PyRuntimeError::new_err("a request ID is not a str"))?;
        ids.push(Arc::<str>::from(id));
    }
    let inputs = inputs_of(world)?;
    write_result(world.py(), with_world(world, |cell| core::record_assigned(cell, &inputs, &ids))?)
}

// ---- A staff member's support ends ------------------------------------------------------------------

/// `StaffSupportCompletionHandler.apply` up to the receipt: what it returns as a record, planned with
/// its sequence allocated, or None without a receipt. The selected care formula is read where the
/// care needs it.
pub(crate) fn complete_support_event<'py>(
    handler: &Bound<'py, PyAny>,
    world: &Bound<'py, PyAny>,
    writer: &EventWriter<'py>,
    event: EventRef<'_, 'py>,
) -> PyResult<Option<PlannedTransition>> {
    let py = world.py();
    let event = event_of(event, [true, true, true, true])?;
    let handler = Handler {
        cooldown_minutes: exact_int(&attribute(handler, intern!(py, "cooldown_minutes"))?)?,
        require_colocated: attribute(handler, intern!(py, "require_colocated"))?.is_truthy()?,
    };
    let inputs = inputs_of(world)?;
    let reserved = writer.reserved();
    let complete = |formula: Formula<'_>| with_world(world, |cell| core::complete_support(cell, &inputs, &event, handler, formula, reserved))?.or_py();
    match complete(Formula::Unread)? {
        Completion::Done(planned) => Ok(planned),
        Completion::NeedsFormula => {
            let formula = selected_care_formula(world)?;
            match complete(Formula::Read(formula.as_deref()))? {
                Completion::Done(planned) => Ok(planned),
                Completion::NeedsFormula => Err(pyo3::exceptions::PyRuntimeError::new_err("the care asked for its formula again")),
            }
        }
    }
}

/// `StaffSupportCompletionHandler.apply`, called from Python: its records, which the dispatcher
/// appends and projects.
#[pyfunction]
fn complete_support<'py>(handler: &Bound<'py, PyAny>, world: &Bound<'py, PyAny>, event: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyTuple>>> {
    let py = world.py();
    let writer = EventWriter::of(world)?;
    let Some(planned) = complete_support_event(handler, world, &writer, EventRef::Object(event))? else {
        return Ok(None);
    };
    Ok(Some(PyTuple::new(py, [transition_record(py, &planned)?])?))
}

/// `create_care_outcome_formula(configs)`, which the registry makes from a selected model, kept for
/// the configs' stamp as their section `care_outcome` (`core/configs.rs`). The PyO3 crate fills it
/// from the Python factory; a build without Python takes the built-in formulas
/// (`CareOutcome::parse`).
pub(crate) fn selected_care_formula(world: &Bound<'_, PyAny>) -> PyResult<Option<Arc<CareOutcome>>> {
    let py = world.py();
    let selection = match configs::selection(world)? {
        Ok(selection) => selection,
        Err(fault) if fault.is_refusal() => return Err(Unsupported::new_err("not a selection")),
        Err(_) => return Err(Unsupported::new_err("the model selection refuses")),
    };
    if selection.get("care_outcome").is_none() {
        return Ok(None);
    }
    let parse = || -> PyResult<CareOutcome> {
        let configs = attribute(world, intern!(py, "model_configs"))?;
        let formula = py
            .import("sheltersim.policies.registry")?
            .getattr("create_care_outcome_formula")?
            .call1((configs,))
            .map_err(|_| Unsupported::new_err("the formula refuses"))?;
        let text = |name| -> PyResult<Arc<str>> { Ok(Arc::from(exact_str(&attribute(&formula, name)?)?)) };
        let (model_id, source_id) = (text(intern!(py, "model_id"))?, text(intern!(py, "source_id"))?);
        let config = <V as VPy>::from_py(&attribute(&formula, intern!(py, "config"))?)?;
        configs::read(CareOutcome::new(model_id, source_id, config))
    };
    match configs::kept_with_selection(world, |configs| &configs.care_outcome, || Ok(configs::parsed(py, parse())))? {
        Ok(formula) => Ok(Some(formula)),
        Err(fault) => Err(configs::raise(&fault, parse)),
    }
}

// ---- A request, for `seek_help` and `seek_information` -----------------------------------------------

/// `exact_int(service.<name>)` as the rule reads it.
fn service_int(service: &Bound<'_, PyAny>, name: &Bound<'_, PyString>) -> PyResult<i64> {
    exact_int(&attribute(service, name)?)
}

/// `exact_str(service.policy_version)` as the rule reads it.
fn service_policy(service: &Bound<'_, PyAny>) -> PyResult<Arc<str>> {
    Ok(Arc::from(exact_str(&attribute(service, intern!(service.py(), "policy_version"))?)?))
}

/// `StaffService.request`, without the world's whole validation, past the sequences the caller's
/// writer reserved. The service's values are read first; one whose read raised is read again where
/// the rule reads it, and raises there.
pub(crate) fn plan_request(service: &Bound<'_, PyAny>, world: &Bound<'_, PyAny>, writer: &EventWriter<'_>, agent_id: &str, minute: i64) -> PyResult<PlannedRequest> {
    let py = world.py();
    let (after, cooldown) = (intern!(py, "unresolved_after_minutes"), intern!(py, "cooldown_minutes"));
    let values = RequestService {
        unresolved_after_minutes: service_int(service, after).ok(),
        cooldown_minutes: service_int(service, cooldown).ok(),
        policy_version: service_policy(service).ok(),
    };
    let reserved = writer.reserved();
    match with_world(world, |cell| core::plan_request(cell, &values, reserved, agent_id, minute))? {
        Err(Error::Unsupported(reason)) if reason == AFTER_UNREAD => service_int(service, after).and(Err(unread())),
        Err(Error::Unsupported(reason)) if reason == COOLDOWN_UNREAD => service_int(service, cooldown).and(Err(unread())),
        Err(Error::Unsupported(reason)) if reason == POLICY_UNREAD => service_policy(service).and(Err(unread())),
        other => other.or_py(),
    }
}

fn unread() -> PyErr {
    pyo3::exceptions::PyRuntimeError::new_err("a value of the service read again did not raise")
}

/// The writes of `StaffService.request` for a request that opens.
pub(crate) fn write_request(
    world: &Bound<'_, PyAny>,
    _writer: &EventWriter<'_>,
    agent_id: &str,
    minute: i64,
    request_id: &Arc<str>,
    timeout_minute: i64,
    event: crate::state::events::EventRow,
) -> PyResult<()> {
    write_result(world.py(), with_world(world, |cell| core::write_request(cell, agent_id, minute, request_id, timeout_minute, event))?)
}

/// The writes of `StaffService.request` for a request closed at once. The person stays as they are.
pub(crate) fn write_closed_request(
    world: &Bound<'_, PyAny>,
    _metrics: &Bound<'_, NamespaceMirror>,
    agent_id: &str,
    minute: i64,
    request_id: &Arc<str>,
    until: i64,
) -> PyResult<()> {
    let held = metric_state_held(world)?;
    with_world(world, |cell| core::write_closed_request(cell, held, agent_id, minute, request_id, until))?.or_py()
}

/// `support_kind_for_agent`.
pub(crate) fn support_kind_of(world: &Bound<'_, PyAny>, _writer: &EventWriter<'_>, agent_id: &str) -> PyResult<Arc<str>> {
    with_world(world, |cell| core::support_kind_of(cell, agent_id))?.or_py()
}

/// `current_household_id` (`sheltersim_core::rules::kit::household_of`): the presence state read
/// as a `V`, a detached one from its row, then the profile's group from the rows.
pub(crate) fn household_of(world: &Bound<'_, PyAny>, agent_id: &str) -> PyResult<Option<Arc<str>>> {
    if let Some(found) = with_state_value(world, PRESENCE, |presence| kit::presence_household(presence, agent_id).or_py())? {
        return Ok(found);
    }
    EntityStore::with_rows(&entities_of(world)?, |rows| Ok(kit::group_of(rows, agent_id)))
}

/// The metric of a request the service turned down.
pub(crate) fn add_rejected(metrics: &Bound<'_, NamespaceMirror>) -> PyResult<()> {
    add_one(metrics, "support_request_rejected_count")
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(assign_staff_support, m)?)?;
    m.add_function(wrap_pyfunction!(timeout, m)?)?;
    m.add_function(wrap_pyfunction!(complete_support, m)?)?;
    m.add_function(wrap_pyfunction!(record_interrupted_step, m)?)?;
    m.add_function(wrap_pyfunction!(record_started_step, m)?)?;
    Ok(())
}
