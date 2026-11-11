//! `domain/care_receipts.py`: the entries of its kernels, whose rules are the core's
//! (`sheltersim_core::rules::care_receipts`, P8b-1). Each asks what only Python can say (whether
//! `shared_operations_v1` is in `model_state`, and its tree is the world's), converts what Python
//! hands in, calls the core, and makes the Python values of what it returns: the applied record of
//! a receipt as Python's `EventLogRecord`, which the dispatcher appends.

use std::sync::Arc;

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyTuple};
use sheltersim_core::events::writer::WriteError;
use sheltersim_core::rules::care_receipts::{self as core, Made};
use sheltersim_core::rules::common_operations::{self as operations, Receipt, RECEIPT_EVENTS};

pub(crate) use sheltersim_core::rules::care_receipts::{PlannedTransition, Transition};

use crate::core::errors::{self, to_py_err, OrPy};
use crate::core::row::{attribute, exact_dict, exact_int, exact_str, namespace_is_absent, unsupported};
use crate::core::world_core::{mirror_of, with_world};
use crate::rules::common_operations::record;
use crate::rules::handlers::status_member;
use crate::state::events::{EventRow, EventRowPy, EventWriter};
use crate::state::shared_operations::NAME;
use crate::Unsupported;

/// A write's error as Python's: a core error's class, or `DuplicateEventError`.
fn write_error(py: Python<'_>, error: WriteError) -> PyErr {
    match error {
        WriteError::Error(error) => to_py_err(error),
        WriteError::Duplicate(message) => match py.import("sheltersim.events.queue").and_then(|module| module.getattr("DuplicateEventError")) {
            Ok(class) => PyErr::from_type(class.downcast_into().expect("an exception class"), message),
            Err(error) => error,
        },
    }
}

/// `EventLogRecord(event=..., realized_minute=..., status=EventStatus.APPLIED, detail=...)`.
fn applied_record<'py>(py: Python<'py>, event: &EventRow, minute: i64, detail: &str) -> PyResult<Bound<'py, PyAny>> {
    let models = py.import(intern!(py, "sheltersim.events.models"))?;
    let kwargs = PyDict::new(py);
    kwargs.set_item(intern!(py, "event"), event.to_py(py)?)?;
    kwargs.set_item(intern!(py, "realized_minute"), minute)?;
    kwargs.set_item(intern!(py, "status"), status_member(py, "APPLIED")?)?;
    kwargs.set_item(intern!(py, "detail"), detail)?;
    models.getattr(intern!(py, "EventLogRecord"))?.call((), Some(&kwargs))
}

pub(crate) fn made_record<'py>(py: Python<'py>, made: Made) -> PyResult<Bound<'py, PyAny>> {
    applied_record(py, &made.event, made.minute, &made.detail)
}

/// The world's `model_state` holds the tree (or the detached row) of `shared_operations_v1`.
fn shared_held(world: &Bound<'_, PyAny>) -> PyResult<()> {
    let py = world.py();
    let mirror = mirror_of(world, NAME)?;
    let table = attribute(world, intern!(py, "model_state"))?;
    match exact_dict(&table)?.get_item(NAME)? {
        Some(state) if mirror.try_borrow()?.holds_state(&state) => Ok(()),
        _ => unsupported("the world holds another document than the row's"),
    }
}

/// `SHARED_OPERATIONS_KEY not in world.model_state`.
fn shared_absent(world: &Bound<'_, PyAny>) -> PyResult<bool> {
    let py = world.py();
    Ok(!attribute(world, intern!(py, "model_state"))?.contains(intern!(py, NAME))?)
}

// ---- A care transition ------------------------------------------------------------------------------

/// `make_care_transition_record` and the checks of `record_shared_operation`, with `sequence` for
/// what `allocate_event_sequence` will give then.
pub(crate) fn plan_transition(world: &Bound<'_, PyAny>, _writer: &EventWriter<'_>, transition: Transition, sequence: i64) -> PyResult<PlannedTransition> {
    shared_held(world)?;
    with_world(world, |world| core::plan_transition(world, transition, sequence))?.or_py()
}

/// The record of an allocated transition, and its receipt.
pub(crate) fn record_transition(world: &Bound<'_, PyAny>, _writer: &EventWriter<'_>, planned: PlannedTransition) -> PyResult<()> {
    with_world(world, |world| core::record_transition(world, planned))?.map_err(|error| write_error(world.py(), error))
}

/// What `plan_transition` planned, with its sequence allocated now.
pub(crate) fn write_transition(world: &Bound<'_, PyAny>, _writer: &EventWriter<'_>, planned: PlannedTransition) -> PyResult<()> {
    with_world(world, |world| core::write_transition(world, planned))?.map_err(|error| write_error(world.py(), error))
}

/// The record of an allocated transition as Python's object, which the dispatcher then appends and
/// projects. Nothing is written.
pub(crate) fn transition_record<'py>(py: Python<'py>, planned: &PlannedTransition) -> PyResult<Bound<'py, PyAny>> {
    applied_record(py, planned.row(), planned.minute(), planned.detail())
}

/// `append_applied_care_transition` once the namespace is known to be there.
pub(crate) fn append(world: &Bound<'_, PyAny>, transition: Transition) -> PyResult<()> {
    shared_held(world)?;
    with_world(world, |world| core::append(world, transition))?.map_err(|error| write_error(world.py(), error))
}

/// Whether `append_applied_care_transition` has nothing to append to: no namespace. One that is None
/// is refused.
fn absent(world: &Bound<'_, PyAny>) -> PyResult<bool> {
    let py = world.py();
    if !namespace_is_absent(world, intern!(py, NAME))? {
        return Ok(false);
    }
    let table = attribute(world, intern!(py, "model_state"))?;
    if exact_dict(&table)?.contains(intern!(py, NAME))? {
        return unsupported("the namespace is None");
    }
    Ok(true)
}

/// `append_applied_care_transition` of a step past its first change, as its Python entry does it:
/// nothing without the namespace, and a refusal is that entry's KernelRefusal.
pub(crate) fn append_applied(world: &Bound<'_, PyAny>, transition: Transition) -> PyResult<()> {
    let py = world.py();
    let done = match absent(world) {
        Ok(true) => Ok(()),
        Ok(false) => append(world, transition),
        Err(error) => Err(error),
    };
    done.map_err(|error| errors::kernel_refusal(py, "care_receipts.append_transition", error))
}

fn optional_text(value: Option<&Bound<'_, PyAny>>) -> PyResult<Option<Arc<str>>> {
    match value {
        Some(value) if !value.is_none() => Ok(Some(Arc::from(exact_str(value)?))),
        _ => Ok(None),
    }
}

/// `append_applied_care_transition`. It returns nothing, as nobody reads its receipt.
#[pyfunction]
#[pyo3(signature = (world, *, request_id, agent_id, household_id, support_kind, status, staff_id=None, causation_id=None, source_ids=None))]
#[allow(clippy::too_many_arguments)]
fn append_transition<'py>(
    world: &Bound<'py, PyAny>,
    request_id: &Bound<'py, PyAny>,
    agent_id: &Bound<'py, PyAny>,
    household_id: Option<&Bound<'py, PyAny>>,
    support_kind: &Bound<'py, PyAny>,
    status: &Bound<'py, PyAny>,
    staff_id: Option<&Bound<'py, PyAny>>,
    causation_id: Option<&Bound<'py, PyAny>>,
    source_ids: Option<&Bound<'py, PyAny>>,
) -> PyResult<()> {
    if absent(world)? {
        return Ok(());
    }
    let mut sources = Vec::new();
    if let Some(source_ids) = source_ids {
        let source_ids = source_ids.downcast_exact::<PyTuple>().map_err(|_| Unsupported::new_err("the sources are not a tuple"))?;
        sources.reserve(source_ids.len() + 1);
        for source in source_ids.iter() {
            sources.push(Arc::from(exact_str(&source)?));
        }
    }
    let (status_text, _) = core::status_of(exact_str(status)?).or_py()?;
    append(
        world,
        Transition {
            request_id: Arc::from(exact_str(request_id)?),
            agent_id: Arc::from(exact_str(agent_id)?),
            household_id: optional_text(household_id)?,
            support_kind: Arc::from(exact_str(support_kind)?),
            status: status_text,
            staff_id: optional_text(staff_id)?,
            causation_id: optional_text(causation_id)?,
            source_ids: sources,
            outcome: None,
        },
    )
}

/// `care_source_ids_for_agent`.
pub(crate) fn source_ids_of(world: &Bound<'_, PyAny>, agent_id: &str) -> PyResult<Vec<Arc<str>>> {
    with_world(world, |world| core::source_ids_of(world, agent_id))?.or_py()
}

// ---- The other receipts: applied records the dispatcher appends ----------------------------------------

fn arcs(items: Vec<String>) -> Vec<Arc<str>> {
    items.into_iter().map(Arc::from).collect()
}

/// `make_facility_recovery_record(...)[0]`, or None without the shared contract.
#[allow(clippy::too_many_arguments)]
pub(crate) fn facility_recovery_record<'py>(
    world: &Bound<'py, PyAny>,
    facility_id: &str,
    component_id: &str,
    status: &str,
    previous: i64,
    units: i64,
    causation: &str,
    source_ids: Vec<String>,
) -> PyResult<Option<Bound<'py, PyAny>>> {
    if shared_absent(world)? {
        return Ok(None);
    }
    let sources = arcs(source_ids);
    let made = with_world(world, |world| core::facility_recovery_record(world, facility_id, component_id, status, previous, units, causation, sources))?.or_py()?;
    made_record(world.py(), made).map(Some)
}

/// `make_staff_handoff_record(...)[0]`, or None without the shared contract.
pub(crate) fn handoff_record<'py>(
    world: &Bound<'py, PyAny>,
    task_id: &str,
    previous_staff: &str,
    replacement: &str,
    reason_code: &str,
    causation: &str,
    source_ids: Vec<String>,
) -> PyResult<Option<Bound<'py, PyAny>>> {
    if shared_absent(world)? {
        return Ok(None);
    }
    let sources = arcs(source_ids);
    let made = with_world(world, |world| core::handoff_record(world, task_id, previous_staff, replacement, reason_code, causation, sources))?.or_py()?;
    made_record(world.py(), made).map(Some)
}

// ---- ingest_applied_shared_operation_records ---------------------------------------------------------

/// `ingest_applied_shared_operation_records`.
#[pyfunction]
fn ingest<'py>(world: &Bound<'py, PyAny>, outcomes: &Bound<'py, PyAny>) -> PyResult<()> {
    let py = world.py();
    let table = attribute(world, intern!(py, "model_state"))?;
    // Past `_DetachingTable.__getitem__`: a detached namespace holds a dict, its row's.
    let Some(held) = exact_dict(&table)?.get_item(intern!(py, NAME))? else {
        return Ok(());
    };
    if !crate::core::tracked::is_detached(&held) && !held.is_instance_of::<PyDict>() {
        return unsupported("the namespace is another value");
    }
    let applied = status_member(py, "APPLIED")?;
    let mut first = true;
    for outcome in outcomes.try_iter()? {
        let outcome = outcome?;
        let Ok(event) = outcome.getattr(intern!(py, "event")) else {
            continue;
        };
        let status = outcome.getattr(intern!(py, "status")).ok();
        if !status.is_some_and(|status| status.is(&applied)) || event.is_none() {
            continue;
        }
        let event_type = attribute(&event, intern!(py, "event_type"))?;
        if !RECEIPT_EVENTS.contains(&exact_str(&event_type)?) {
            continue;
        }
        let event_id = attribute(&event, intern!(py, "event_id"))?;
        let minute = attribute(world, intern!(py, "minute_index"))?;
        let realized = outcome.getattr(intern!(py, "realized_minute")).ok();
        let recent = realized.is_some_and(|realized| realized.eq(&minute).unwrap_or(false));
        let (id, minute) = (exact_str(&event_id)?, exact_int(&minute)?);
        let recorded = with_world(world, |world| operations::is_recorded(world, id, recent, minute))?.or_py();
        // Only the first can be refused: nothing has changed yet.
        let recorded = match recorded {
            Err(error) if !first && error.is_instance_of::<Unsupported>(py) => {
                return Err(pyo3::exceptions::PyRuntimeError::new_err(format!("the shared operations left their row: {}", error.value(py))));
            }
            other => other?,
        };
        if recorded {
            continue;
        }
        // `_operation_record_adapter().validate_python(event.payload)`, as the core's check.
        let Some(row) = EventRow::from_event(&event)? else {
            return unsupported("the event has no row");
        };
        let receipt = Receipt::of(&row.payload).or_py()?;
        // A refusal past the first receipt leaves the earlier ones written, as the Python rule's
        // error did.
        record(world, &receipt)?;
        first = false;
    }
    Ok(())
}

/// `ingest_applied_shared_operation_records(world, (record,))` of one APPLIED record a kernel of the
/// core wrote at `realized_minute`, of an event it made (lane c6: the continuity receipt of a
/// replanned support).
pub(crate) fn ingest_applied(world: &Bound<'_, PyAny>, event: &EventRow, realized_minute: i64) -> PyResult<()> {
    let py = world.py();
    let table = attribute(world, intern!(py, "model_state"))?;
    let Some(held) = exact_dict(&table)?.get_item(intern!(py, NAME))? else {
        return Ok(());
    };
    if !crate::core::tracked::is_detached(&held) && !held.is_instance_of::<PyDict>() {
        return unsupported("the namespace is another value");
    }
    if !RECEIPT_EVENTS.contains(&&*event.event_type) {
        return Ok(());
    }
    let minute = exact_int(&attribute(world, intern!(py, "minute_index"))?)?;
    let recent = realized_minute == minute;
    let id = &*event.event_id;
    if with_world(world, |world| operations::is_recorded(world, id, recent, minute))?.or_py()? {
        return Ok(());
    }
    let receipt = Receipt::of(&event.payload).or_py()?;
    record(world, &receipt)
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(append_transition, m)?)?;
    m.add_function(wrap_pyfunction!(ingest, m)?)?;
    Ok(())
}
