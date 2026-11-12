//! `domain/action_lifecycle.py`: the glue of the kernels of `sheltersim_core::rules::
//! action_lifecycle` (lane P8 a2), and the Python object of a controller's open actions.
//!
//! `ActiveActions` holds the core's rows of the open actions, in a controller's Run (`Run::actions`).
//! A Python rule or a test that asks for
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
use crate::core::run::RunCore;
use crate::core::row::{attribute, exact_int, exact_str, py_row, unsupported, IdMap, PyField};
use crate::core::world_core::with_world;
use crate::state::events::{applied_row, EventRow};
use crate::Unsupported;

pub(crate) use sheltersim_core::rules::action_lifecycle::{ends_any_action, Proposal};
use sheltersim_core::rules::action_lifecycle::Ending;


py_row! {
    ActiveAction {
        action_id, agent_id, action, target_id, start_minute, reason_code, start_activity, start_needs,
        model_id, model_source_id, continued,
    }
}

/// The open actions of a controller: the rows of its Run (`Run::actions`, lane P8 a2), or its own
/// rows for a store made without a Run (tests, benchmarks/rule_shadow). A Python rule that runs on
/// them gets a plain dict (`as_plain`), which then is what holds them, until a kernel can read it
/// into rows.
#[pyclass(module = "sheltersim_native.rules.action_lifecycle")]
pub struct ActiveActions {
    /// The Run whose rows these are; None: `own` holds them.
    run: Option<Py<RunCore>>,
    own: rules::ActiveActions,
    plain: Option<Py<PyDict>>,
}

impl ActiveActions {
    /// Run f on the rows, wherever they are.
    fn rows<R>(&mut self, f: impl FnOnce(&mut rules::ActiveActions) -> R) -> PyResult<R> {
        match &self.run {
            Some(run) => run.get().with_actions(f),
            None => Ok(f(&mut self.own)),
        }
    }

    /// Read the plain dict into rows if there is one; Unsupported, keeping it, if it does not fit.
    fn into_rows(&mut self, py: Python<'_>) -> PyResult<()> {
        if let Some(plain) = &self.plain {
            let rows = IdMap::<ActiveAction>::from_py(plain.bind(py))
                .map_err(|_| Unsupported::new_err("an active action does not fit its row"))?;
            let rows = rules::ActiveActions::from_rows(rows).or_py()?;
            self.rows(|held| *held = rows)?;
            self.plain = None;
        }
        Ok(())
    }

    /// Run a kernel on the rows, read from a plain dict first: the Run is locked while it runs, so
    /// the kernel must not reach the Run again.
    pub(crate) fn with_rows<R>(&mut self, py: Python<'_>, f: impl FnOnce(&mut rules::ActiveActions) -> PyResult<R>) -> PyResult<R> {
        self.into_rows(py)?;
        self.rows(f)?
    }

    /// Whether the rows are this Run's (`Run::actions`), which the core's dispatch closes.
    pub(crate) fn is_of(&self, run: &RunCore) -> bool {
        self.run.as_ref().is_some_and(|own| std::ptr::eq(own.get(), run))
    }

    fn rows_as_dict<'py>(&mut self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let plain = self.rows(|rows| PyField::to_py(&rows.rows, py))??;
        Ok(plain.downcast_into::<PyDict>()?)
    }
}

#[pymethods]
impl ActiveActions {
    /// What `active_action_index` gives, with each `start_needs` a dict: the rows of `run`, a
    /// controller's `RunCore`, or the store's own without one.
    #[new]
    #[pyo3(signature = (records, run=None))]
    fn new(py: Python<'_>, records: Bound<'_, PyDict>, run: Option<Py<RunCore>>) -> PyResult<Self> {
        let mut this = Self { run, own: rules::ActiveActions::default(), plain: Some(records.unbind()) };
        // The Run's rows start empty; a store that doesn't fit stays plain, for the Python rules.
        this.rows(|rows| *rows = rules::ActiveActions::default())?;
        let _ = this.into_rows(py);
        Ok(this)
    }

    /// The dict a Python rule works on. It holds the actions from now on, so what the rule does to
    /// it stays, even if the rule raises.
    fn as_plain<'py>(&mut self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        if let Some(plain) = &self.plain {
            return Ok(plain.bind(py).clone());
        }
        let plain = self.rows_as_dict(py)?;
        self.plain = Some(plain.clone().unbind());
        self.rows(|rows| *rows = rules::ActiveActions::default())?;
        Ok(plain)
    }

    /// A detached copy, changing nothing: for tests and benchmarks/rule_shadow.
    fn snapshot<'py>(&mut self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        match &self.plain {
            Some(plain) => plain.bind(py).copy(),
            None => self.rows_as_dict(py),
        }
    }

    /// Whether the actions are in rows, for tests.
    fn in_rows(&self) -> bool {
        self.plain.is_none()
    }

    /// Whether the rows are the Run's (`Run::actions`), for tests.
    fn in_run(&self) -> bool {
        self.run.is_some()
    }

    fn __contains__(&mut self, py: Python<'_>, agent_id: &Bound<'_, PyAny>) -> PyResult<bool> {
        if let Some(plain) = &self.plain {
            return plain.bind(py).contains(agent_id);
        }
        let Ok(text) = agent_id.downcast::<PyString>() else {
            return Ok(false);
        };
        let text = text.to_str()?;
        self.rows(|rows| rows.contains(text))
    }

    fn __len__(&mut self, py: Python<'_>) -> PyResult<usize> {
        match &self.plain {
            Some(plain) => Ok(plain.bind(py).len()),
            None => self.rows(|rows| rows.len()),
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
    active.try_borrow_mut()?.with_rows(world.py(), |rows| with_world(world, |cell| rules::commit_action(cell, rows, proposal))?.or_py())
}

/// `record_action_proposal`.
#[pyfunction]
#[pyo3(signature = (world, proposal, *, target_id, committed, active_actions))]
pub(crate) fn record_action_proposal(
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
    let Some(row) = applied_row(world, exact_str(&event_id)?, event)? else {
        return Ok(());
    };
    complete_row(world, active_actions, &row, exact_int(minute)?)
}

fn complete_row(world: &Bound<'_, PyAny>, active_actions: &Bound<'_, PyAny>, row: &EventRow, minute: i64) -> PyResult<()> {
    complete(world, active_actions, &Ending::of(row), minute)
}

fn complete(world: &Bound<'_, PyAny>, active_actions: &Bound<'_, PyAny>, event: &Ending<'_>, minute: i64) -> PyResult<()> {
    if !ends_any_action(event.event_type) {
        return Ok(());
    }
    // A plain dict of open actions is the Python rule's.
    let Ok(active) = active_actions.downcast::<ActiveActions>() else {
        return unsupported("active_actions is not the store");
    };
    let mut this = active.try_borrow_mut()?;
    // Most applied events end no open action: the world is not borrowed for them.
    if this.with_rows(world.py(), |rows| Ok(rules::closes(rows, event).is_none()))? {
        return Ok(());
    }
    this.with_rows(world.py(), |rows| with_world(world, |cell| rules::complete_for_event(cell, rows, event, minute))?.or_py())
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
    if applied_row(world, event_id, event)?.is_none() {
        return Ok(());
    }
    let minute = exact_int(minute)?;
    active
        .try_borrow_mut()?
        .with_rows(py, |rows| with_world(world, |cell| rules::population_exit(cell, rows, event_id, minute))?.or_py())
}

/// `finalize_active_actions`.
#[pyfunction]
#[pyo3(signature = (world, minute, *, active_actions))]
fn finalize_active_actions(world: &Bound<'_, PyAny>, minute: &Bound<'_, PyAny>, active_actions: &Bound<'_, PyAny>) -> PyResult<()> {
    let active = store_of(active_actions)?;
    let minute = exact_int(minute)?;
    active.try_borrow_mut()?.with_rows(world.py(), |rows| with_world(world, |cell| rules::finalize(cell, rows, minute))?.or_py())
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<ActiveActions>()?;
    m.add_function(wrap_pyfunction!(record_action_proposal, m)?)?;
    m.add_function(wrap_pyfunction!(record_applied_action_completion, m)?)?;
    m.add_function(wrap_pyfunction!(record_applied_population_exit, m)?)?;
    m.add_function(wrap_pyfunction!(finalize_active_actions, m)?)?;
    Ok(())
}
