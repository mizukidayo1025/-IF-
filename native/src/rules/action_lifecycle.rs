//! `domain/action_lifecycle.py`: the glue of the kernels of `sheltersim_core::rules::
//! action_lifecycle` (lane P8 a2), and the Python object of a controller's open actions.
//!
//! `ActiveActions` holds the core's rows of the open actions. A Python rule or a test that asks for
//! a plain dict (`as_plain`) gets one, which then holds them until a kernel reads it into rows
//! again; a dict that does not fit stays plain, and the kernels refuse it. Each entry reads what
//! Python holds (the arguments, the event as its row, whether the log holds it applied), runs the
//! core's kernel on the world (`with_world`) and the rows, and returns.

use pyo3::exceptions::PyTypeError;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyString};

use sheltersim_core::rules::action_lifecycle as rules;
use sheltersim_core::rules::action_lifecycle::ActiveAction;

use crate::core::errors::OrPy;
use crate::core::row::{attribute, exact_int, exact_str, py_row, unsupported, IdMap, PyField};
use crate::core::world_core::with_world;
use crate::eventlog::AppliedOutcome;
use crate::state::events::{EventRef, EventRow, EventRowPy, EventWriter};
use crate::Unsupported;

pub(crate) use sheltersim_core::rules::action_lifecycle::{ends_any_action, Proposal};

py_row! {
    ActiveAction {
        action_id, agent_id, action, target_id, start_minute, reason_code, start_activity, start_needs,
        model_id, model_source_id, continued,
    }
}

/// The open actions of a controller. Rows while they fit; a Python rule that runs on them gets a
/// plain dict (`as_plain`), which then is what holds them, until a kernel can read it into rows.
#[pyclass(module = "sheltersim_native.rules.action_lifecycle")]
pub struct ActiveActions {
    rows: rules::ActiveActions,
    plain: Option<Py<PyDict>>,
}

impl ActiveActions {
    /// Read the plain dict into rows if there is one; Unsupported, keeping it, if it does not fit.
    fn into_rows(&mut self, py: Python<'_>) -> PyResult<&mut rules::ActiveActions> {
        if let Some(plain) = &self.plain {
            let rows = IdMap::<ActiveAction>::from_py(plain.bind(py))
                .map_err(|_| Unsupported::new_err("an active action does not fit its row"))?;
            self.rows = rules::ActiveActions::from_rows(rows).or_py()?;
            self.plain = None;
        }
        Ok(&mut self.rows)
    }

    fn rows_as_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let plain = PyField::to_py(&self.rows.rows, py)?;
        Ok(plain.downcast_into::<PyDict>()?)
    }
}

#[pymethods]
impl ActiveActions {
    /// What `active_action_index` gives, with each `start_needs` a dict.
    #[new]
    fn new(py: Python<'_>, records: Bound<'_, PyDict>) -> Self {
        let mut this = Self { rows: rules::ActiveActions::default(), plain: Some(records.unbind()) };
        // A store that doesn't fit stays plain, for the Python rules.
        let _ = this.into_rows(py);
        this
    }

    /// The dict a Python rule works on. It holds the actions from now on, so what the rule does to
    /// it stays, even if the rule raises.
    fn as_plain<'py>(&mut self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        if let Some(plain) = &self.plain {
            return Ok(plain.bind(py).clone());
        }
        let plain = self.rows_as_dict(py)?;
        self.plain = Some(plain.clone().unbind());
        self.rows = rules::ActiveActions::default();
        Ok(plain)
    }

    /// A detached copy, changing nothing: for tests and benchmarks/rule_shadow.
    fn snapshot<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        match &self.plain {
            Some(plain) => plain.bind(py).copy(),
            None => self.rows_as_dict(py),
        }
    }

    /// Whether the actions are in rows, for tests.
    fn in_rows(&self) -> bool {
        self.plain.is_none()
    }

    fn __contains__(&self, py: Python<'_>, agent_id: &Bound<'_, PyAny>) -> PyResult<bool> {
        if let Some(plain) = &self.plain {
            return plain.bind(py).contains(agent_id);
        }
        let Ok(text) = agent_id.downcast::<PyString>() else {
            return Ok(false);
        };
        Ok(self.rows.contains(text.to_str()?))
    }

    fn __len__(&self, py: Python<'_>) -> usize {
        match &self.plain {
            Some(plain) => plain.bind(py).len(),
            None => self.rows.len(),
        }
    }
}

fn store_of<'a, 'py>(value: &'a Bound<'py, PyAny>) -> PyResult<&'a Bound<'py, ActiveActions>> {
    value.downcast::<ActiveActions>().map_err(|_| PyTypeError::new_err("active_actions is not the store"))
}

// ---- record_action_proposal --------------------------------------------------------------------------

/// `record_action_proposal` of a committed proposal, for a Rust caller too. Unsupported, with
/// nothing changed, for the Python rule on `active.as_plain()`.
pub(crate) fn commit_action(world: &Bound<'_, PyAny>, active: &Bound<'_, ActiveActions>, proposal: &Proposal<'_>) -> PyResult<()> {
    let mut this = active.try_borrow_mut()?;
    let rows = this.into_rows(world.py())?;
    with_world(world, |cell| rules::commit_action(cell, rows, proposal))?.or_py()
}

/// `record_action_proposal`.
#[pyfunction]
#[pyo3(signature = (world, proposal, *, target_id, committed, active_actions))]
fn record_action_proposal(
    world: &Bound<'_, PyAny>,
    proposal: &Bound<'_, PyAny>,
    target_id: &Bound<'_, PyAny>,
    committed: &Bound<'_, PyAny>,
    active_actions: &Bound<'_, PyAny>,
) -> PyResult<()> {
    let py = world.py();
    let Ok(committed) = committed.downcast_exact::<PyBool>() else {
        return unsupported("committed is not a bool");
    };
    if !committed.is_true() {
        return Ok(());
    }
    let active = store_of(active_actions)?;
    let agent_id = attribute(proposal, intern!(py, "agent_id"))?;
    let action = attribute(&attribute(proposal, intern!(py, "action"))?, intern!(py, "value"))?;
    let trigger = attribute(proposal, intern!(py, "trigger"))?;
    commit_action(
        world,
        active,
        &Proposal {
            agent_id: exact_str(&agent_id)?,
            action: exact_str(&action)?,
            target_id: if target_id.is_none() { None } else { Some(exact_str(target_id)?) },
            minute: exact_int(&attribute(proposal, intern!(py, "minute_index"))?)?,
            trigger: exact_str(&trigger)?,
        },
    )
}

// ---- The applied events ------------------------------------------------------------------------------

/// The event's row: Unsupported for an event a row does not hold.
fn row_of(event: &Bound<'_, PyAny>) -> PyResult<EventRow> {
    EventRow::from_event(event)?.ok_or_else(|| Unsupported::new_err("the event is not a row"))
}

/// The row of the event if the log holds it applied, as the Python rules ask. The event must be the
/// object the record holds, or one whose row is what the record's bytes say: a value a row cannot
/// tell from another (`1` and `1.0`) is the Python rule's to compare.
fn applied_row(writer: &EventWriter<'_>, event_id: &str, event: &Bound<'_, PyAny>) -> PyResult<Option<EventRow>> {
    match writer.log().borrow().applied_outcome(event_id, event)? {
        AppliedOutcome::No => Ok(None),
        AppliedOutcome::ThisEvent => row_of(event).map(Some),
        AppliedOutcome::AnotherObject => unsupported("the record holds another object of the event"),
        AppliedOutcome::SavedRow(saved) => match EventRow::from_event(event)? {
            Some(row) if row == saved => Ok(Some(row)),
            _ => unsupported("the record is of another row of the event"),
        },
    }
}

/// `record_applied_action_completion`.
#[pyfunction]
#[pyo3(signature = (world, event, minute, *, active_actions))]
fn record_applied_action_completion(
    world: &Bound<'_, PyAny>,
    event: &Bound<'_, PyAny>,
    minute: &Bound<'_, PyAny>,
    active_actions: &Bound<'_, PyAny>,
) -> PyResult<()> {
    let py = world.py();
    let event_type = attribute(event, intern!(py, "event_type"))?;
    // Nothing below changes anything for another type of event.
    if !ends_any_action(exact_str(&event_type)?) {
        return Ok(());
    }
    store_of(active_actions)?;
    let event_id = attribute(event, intern!(py, "event_id"))?;
    let writer = EventWriter::of(world)?;
    let Some(row) = applied_row(&writer, exact_str(&event_id)?, event)? else {
        return Ok(());
    };
    complete_row(world, active_actions, &row, exact_int(minute)?)
}

/// `record_applied_action_completion` of an event the caller knows the log holds applied. Says
/// Unsupported before any change. The writer is the caller's check that the world's events are
/// Rust's.
pub(crate) fn complete_for_event(
    world: &Bound<'_, PyAny>,
    _writer: &EventWriter<'_>,
    active_actions: &Bound<'_, PyAny>,
    event: EventRef<'_, '_>,
    minute: i64,
) -> PyResult<()> {
    match event {
        EventRef::Row(row) => complete_row(world, active_actions, row, minute),
        EventRef::Object(object) => {
            if !ends_any_action(&EventRef::Object(object).event_type()?) {
                return Ok(());
            }
            complete_row(world, active_actions, &row_of(object)?, minute)
        }
    }
}

fn complete_row(world: &Bound<'_, PyAny>, active_actions: &Bound<'_, PyAny>, row: &EventRow, minute: i64) -> PyResult<()> {
    if !ends_any_action(&row.event_type) {
        return Ok(());
    }
    // A plain dict of open actions is the Python rule's.
    let Ok(active) = active_actions.downcast::<ActiveActions>() else {
        return unsupported("active_actions is not the store");
    };
    let mut this = active.try_borrow_mut()?;
    let rows = this.into_rows(world.py())?;
    with_world(world, |cell| rules::complete_for_event(cell, rows, row, minute))?.or_py()
}

/// `record_applied_population_exit`.
#[pyfunction]
#[pyo3(signature = (world, event, minute, *, active_actions))]
fn record_applied_population_exit(
    world: &Bound<'_, PyAny>,
    event: &Bound<'_, PyAny>,
    minute: &Bound<'_, PyAny>,
    active_actions: &Bound<'_, PyAny>,
) -> PyResult<()> {
    let py = world.py();
    let event_type = attribute(event, intern!(py, "event_type"))?;
    if exact_str(&event_type)? != "effect_program_population" {
        return Ok(());
    }
    let active = store_of(active_actions)?;
    let event_id = attribute(event, intern!(py, "event_id"))?;
    let event_id = exact_str(&event_id)?;
    let writer = EventWriter::of(world)?;
    if applied_row(&writer, event_id, event)?.is_none() {
        return Ok(());
    }
    let minute = exact_int(minute)?;
    let mut this = active.try_borrow_mut()?;
    let rows = this.into_rows(py)?;
    with_world(world, |cell| rules::population_exit(cell, rows, event_id, minute))?.or_py()
}

/// `finalize_active_actions`.
#[pyfunction]
#[pyo3(signature = (world, minute, *, active_actions))]
fn finalize_active_actions(world: &Bound<'_, PyAny>, minute: &Bound<'_, PyAny>, active_actions: &Bound<'_, PyAny>) -> PyResult<()> {
    let active = store_of(active_actions)?;
    let minute = exact_int(minute)?;
    let mut this = active.try_borrow_mut()?;
    let rows = this.into_rows(world.py())?;
    with_world(world, |cell| rules::finalize(cell, rows, minute))?.or_py()
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<ActiveActions>()?;
    m.add_function(wrap_pyfunction!(record_action_proposal, m)?)?;
    m.add_function(wrap_pyfunction!(record_applied_action_completion, m)?)?;
    m.add_function(wrap_pyfunction!(record_applied_population_exit, m)?)?;
    m.add_function(wrap_pyfunction!(finalize_active_actions, m)?)?;
    Ok(())
}
