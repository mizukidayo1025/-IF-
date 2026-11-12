//! The open actions of a controller (`ActiveActions`, the pyclass of
//! `sheltersim_native.rules.action_lifecycle`): the core's rows of the open actions in a controller's
//! Run (`Run::actions`), lent to one core call at a time (lane P10-1c moved it here from
//! `rules/action_lifecycle.rs`, whose entries call it).
//!
//! A Python rule or a test that asks for a plain dict (`as_plain`) gets one, which then holds them
//! until a kernel reads it into rows again; a dict that does not fit stays plain, and the kernels
//! refuse it.

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyString};

use sheltersim_core::error::Result as CoreResult;
use sheltersim_core::rules::action_lifecycle as rules;
use sheltersim_core::rules::action_lifecycle::ActiveAction;
use sheltersim_core::world::World;

use super::errors::OrPy;
use super::row::{py_row, IdMap, PyField};
use super::run::RunCore;
use super::world_core::with_world;
use crate::Unsupported;

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

    /// Run f on the rows, read from a plain dict first: the Run is locked while it runs, so f must
    /// not reach the Run again.
    fn lend<R>(&mut self, py: Python<'_>, f: impl FnOnce(&mut rules::ActiveActions) -> PyResult<R>) -> PyResult<R> {
        self.into_rows(py)?;
        self.rows(f)?
    }

    /// One core call on the world borrowed whole (`with_world`) and the rows, read from a plain
    /// dict first (lane P10-1c: the lending the rules' entries did).
    pub(crate) fn on_world<R>(&mut self, world: &Bound<'_, PyAny>, f: impl FnOnce(&mut World, &mut rules::ActiveActions) -> CoreResult<R>) -> PyResult<R> {
        self.lend(world.py(), |rows| with_world(world, |cell| f(cell, rows))?.or_py())
    }

    /// A core reader of the rows alone, read from a plain dict first: the world is not borrowed.
    pub(crate) fn read<R>(&mut self, py: Python<'_>, f: impl FnOnce(&rules::ActiveActions) -> R) -> PyResult<R> {
        self.lend(py, |rows| Ok(f(rows)))
    }

    /// Whether the actions are rows or a plain dict that fits them (read into them now): the tick's
    /// check before anything changes (policy E10).
    pub(crate) fn fits_rows(&mut self, py: Python<'_>) -> bool {
        self.lend(py, |_| Ok(())).is_ok()
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
