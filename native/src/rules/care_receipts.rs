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
use sheltersim_core::rules::kit;

pub(crate) use sheltersim_core::rules::care_receipts::{PlannedTransition, Transition};

use crate::core::errors::{to_py_err, OrPy};
use crate::core::row::{attribute, exact_int, exact_str, unsupported};
use crate::core::world_core::{with_world, PyWorldCell};
use crate::host::PyHost;
use crate::rules::common_operations::record;
use crate::rules::handlers::status_member;
use crate::state::events::{EventRow, EventRowPy, EventWriter};
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

/// What the world holds, read by the core's host (lane P9-1a).
fn holding(world: &Bound<'_, PyAny>) -> PyResult<std::sync::Arc<sheltersim_core::host::Holding>> {
    kit::holding(&PyWorldCell(world), &mut PyHost::of_world(world, PyDict::new(world.py()))).map_err(|error| error.0)
}

// ---- A care transition ------------------------------------------------------------------------------

/// The record of an allocated transition, and its receipt.
pub(crate) fn record_transition(world: &Bound<'_, PyAny>, _writer: &EventWriter<'_>, planned: PlannedTransition) -> PyResult<()> {
    with_world(world, |world| core::record_transition(world, planned))?.map_err(|error| write_error(world.py(), error))
}

/// The record of an allocated transition as Python's object, which the dispatcher then appends and
/// projects. Nothing is written.
pub(crate) fn transition_record<'py>(py: Python<'py>, planned: &PlannedTransition) -> PyResult<Bound<'py, PyAny>> {
    applied_record(py, planned.row(), planned.minute(), planned.detail())
}

/// `append_applied_care_transition` once the namespace is known to be there.
pub(crate) fn append(world: &Bound<'_, PyAny>, transition: Transition) -> PyResult<()> {
    core::check_tree(&*holding(world)?).or_py()?;
    with_world(world, |world| core::append(world, transition))?.map_err(|error| write_error(world.py(), error))
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
    if core::nothing_to_append(&*holding(world)?).or_py()? {
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

// ---- ingest_applied_shared_operation_records ---------------------------------------------------------

/// `ingest_applied_shared_operation_records`.
#[pyfunction]
fn ingest<'py>(world: &Bound<'py, PyAny>, outcomes: &Bound<'py, PyAny>) -> PyResult<()> {
    let py = world.py();
    // A detached namespace holds the row's placeholder, which the host's `Holding` reads as held.
    if !core::has_namespace(&*holding(world)?).or_py()? {
        return Ok(());
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

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(append_transition, m)?)?;
    m.add_function(wrap_pyfunction!(ingest, m)?)?;
    Ok(())
}
