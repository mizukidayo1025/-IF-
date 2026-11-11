//! The glue of the steps of the propose phase that make and record a decision
//! (`experiments/phases/propose.py`; the core's `rules::behavior` and `rules::decisions`, lane P8
//! a5), with no Python object for the proposal. They have no Python step (segments.py,
//! KERNEL_ONLY_SEGMENTS).
//!
//! `propose_rows` is `_propose_due`, returning `ProposalRow`s: what the phase reads of a proposal
//! (`agent_id`, `action`, `next_decision_at`, `target_id`, `observed_state_hash`) comes from the
//! core's proposal, and anything else makes the `ActionProposal` of rules/behavior.rs, once.
//! `observed_row` is `_observed_state`, which keeps only where to find the person in the snapshot.
//! `record_outcome` is `_record_outcome`: it reads what Python holds (the values the step was given,
//! the open actions, whether `agent_dynamics_v1` exists) and calls the core's record (through the
//! `PyHost`, which writes to the sink), lifecycle and memory, in that order. What the core does not
//! take goes to the sink's own `record_decision` with the Python proposal (`record_through_python`),
//! and then the same rules. A refusal of the lifecycle or the memory is the KernelRefusal of its
//! Python entry, as when the step called those entries.

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::sync::GILOnceCell;
use pyo3::types::{PyBool, PyDict, PyInt, PyString, PyTuple};

use sheltersim_core::host::{DecisionSink, Host};
use sheltersim_core::rules::behavior::Proposal;
use sheltersim_core::rules::decisions::{self as core, Observed, Recording, Sunk, LIFECYCLE, MEMORY};

use crate::core::errors::{kernel_refusal, OrPy};
use crate::core::row::{attribute, exact_dict, exact_str};
use crate::core::run::{plain_of, policies_of};
use crate::core::world_core::{raw_dict_get, with_world, PyWorldCell};
use crate::host::PyHost;
use crate::rules::action_lifecycle::{record_action_proposal, ActiveActions};
use crate::rules::agent_dynamics::record_proposal;
use crate::rules::behavior::{classes, due_proposals, observed_dict, person_of, proposal_of, snapshot_of};
use crate::rules::world_view::RustView;

const AGENT_DYNAMICS: &str = "agent_dynamics_v1";

// ---- The proposal ------------------------------------------------------------------------------

/// A proposal that has not become a Python object.
#[pyclass(module = "sheltersim_native", frozen)]
pub struct ProposalRow {
    view: Py<RustView>,
    proposal: Proposal,
    made: GILOnceCell<Py<PyAny>>,
}

impl ProposalRow {
    fn made<'py>(&self, py: Python<'py>) -> PyResult<&Bound<'py, PyAny>> {
        let made = self
            .made
            .get_or_try_init(py, || Ok::<_, PyErr>(proposal_of(py, self.view.get(), &self.proposal)?.unbind()))?;
        Ok(made.bind(py))
    }

    /// `proposal.with_outcome(target_id, next_decision_at)`.
    fn recorded<'py>(
        &self,
        py: Python<'py>,
        target_id: &Bound<'py, PyAny>,
        next_decision_at: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        self.made(py)?.call_method1(intern!(py, "with_outcome"), (target_id, next_decision_at))
    }
}

#[pymethods]
impl ProposalRow {
    #[getter]
    fn agent_id<'py>(&self, py: Python<'py>) -> Bound<'py, PyString> {
        self.view.get().key(py, self.proposal.index).clone()
    }

    #[getter]
    fn action<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        Ok(classes(py)?.actions[self.proposal.outcome.action as usize].bind(py).clone())
    }

    #[getter]
    fn next_decision_at(&self) -> i64 {
        self.proposal.outcome.next_decision_at
    }

    #[getter]
    fn target_id<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        match self.proposal.outcome.target {
            Some(target) => self.view.get().key(py, target).clone().into_any(),
            None => py.None().into_bound(py),
        }
    }

    #[getter]
    fn minute_index(&self) -> i64 {
        self.view.get().data().minute_index
    }

    #[getter]
    fn observed_state_hash<'py>(&self, py: Python<'py>) -> Bound<'py, PyString> {
        PyString::new(py, &self.proposal.common.state_hash)
    }

    #[getter]
    fn policy_version<'py>(&self, py: Python<'py>) -> Bound<'py, PyString> {
        PyString::new(py, &self.proposal.common.policy_version)
    }

    /// Whether anything has needed the `ActionProposal` yet.
    #[getter]
    fn has_object(&self, py: Python<'_>) -> bool {
        self.made.get(py).is_some()
    }

    /// The `ActionProposal`, the same object each time.
    fn materialize<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.made(py).cloned()
    }

    fn with_outcome<'py>(
        &self,
        py: Python<'py>,
        target_id: &Bound<'py, PyAny>,
        next_decision_at: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        self.recorded(py, target_id, next_decision_at)
    }

    /// Any other field of the `ActionProposal`.
    fn __getattr__<'py>(&self, py: Python<'py>, name: &Bound<'py, PyString>) -> PyResult<Bound<'py, PyAny>> {
        self.made(py)?.getattr(name)
    }
}

/// `_propose_due`, whose proposals the rest of the propose phase reads as rows.
#[pyfunction]
pub(crate) fn propose_rows<'py>(
    controller: &Bound<'py, PyAny>,
    view: &Bound<'py, PyAny>,
    rng: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = controller.py();
    let Some(due) = due_proposals(controller, view, rng)? else {
        return Ok(PyTuple::empty(py).into_any());
    };
    let snapshot = due.rust.unbind();
    let mut rows = Vec::with_capacity(due.proposals.len());
    for proposal in due.proposals {
        rows.push(Bound::new(py, ProposalRow { view: snapshot.clone_ref(py), proposal, made: GILOnceCell::new() })?);
    }
    Ok(PyTuple::new(py, rows)?.into_any())
}

// ---- The observed state ------------------------------------------------------------------------

/// `_agent_view_snapshot`, not yet made. `dict(row)` makes it, as `record_decision` does.
#[pyclass(module = "sheltersim_native", frozen)]
pub struct ObservedRow {
    view: Py<RustView>,
    index: usize,
    made: GILOnceCell<Py<PyDict>>,
}

impl ObservedRow {
    fn made<'py>(&self, py: Python<'py>) -> PyResult<&Bound<'py, PyDict>> {
        let made = self
            .made
            .get_or_try_init(py, || Ok::<_, PyErr>(observed_dict(py, self.view.get(), self.index)?.unbind()))?;
        Ok(made.bind(py))
    }
}

#[pymethods]
impl ObservedRow {
    fn keys<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.made(py)?.call_method0(intern!(py, "keys"))
    }

    fn __getitem__<'py>(&self, py: Python<'py>, key: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        self.made(py)?.as_any().get_item(key)
    }
}

/// The controller's host, with the policies its Run read (or read now for a holder without one).
fn host_of<'py>(controller: &Bound<'py, PyAny>) -> PyResult<PyHost<'py>> {
    Ok(PyHost::of_holder(controller, policies_of(controller)?))
}

/// `_observed_state`. A sink in Python may read the document as a dict, so it gets one.
#[pyfunction]
pub(crate) fn observed_row<'py>(
    controller: &Bound<'py, PyAny>,
    view: &Bound<'py, PyAny>,
    agent_id: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = controller.py();
    let snapshot = snapshot_of(view)?;
    let index = person_of(snapshot.get(), agent_id)?;
    let sink = host_of(controller)?.decision_sink().map_err(|error| error.0)?;
    if matches!(sink, DecisionSink::Python | DecisionSink::Artifacts) {
        return Ok(observed_dict(py, snapshot.get(), index)?.into_any());
    }
    Ok(Bound::new(py, ObservedRow { view: snapshot.unbind(), index, made: GILOnceCell::new() })?.into_any())
}

// ---- The step ----------------------------------------------------------------------------------

/// `_record_outcome`.
#[pyfunction]
#[pyo3(signature = (controller, proposal, *, target_id, next_decision_at, committed, observed_state))]
pub(crate) fn record_outcome<'py>(
    controller: &Bound<'py, PyAny>,
    proposal: &Bound<'py, PyAny>,
    target_id: &Bound<'py, PyAny>,
    next_decision_at: &Bound<'py, PyAny>,
    committed: &Bound<'py, PyAny>,
    observed_state: &Bound<'py, PyAny>,
) -> PyResult<()> {
    let py = controller.py();
    let through_python = || record_through_python(controller, proposal, target_id, next_decision_at, committed, observed_state);
    let Ok(row) = proposal.downcast_exact::<ProposalRow>() else {
        return through_python();
    };
    let row = row.get();
    let Ok(committed_flag) = committed.downcast_exact::<PyBool>() else {
        return through_python();
    };
    let data = row.view.get().data();
    // `with_outcome` refuses a next decision that is not after the minute, with its own text.
    let next = match next_decision_at.downcast_exact::<PyInt>().map(|next| next.extract::<i64>()) {
        Ok(Ok(next)) if next > data.minute_index => next,
        _ => return through_python(),
    };
    let target = if target_id.is_none() { None } else { Some(exact_str(target_id)?) };
    let world = attribute(controller, intern!(py, "world"))?;
    let active = attribute(controller, intern!(py, "active_actions"))?;
    let Ok(active) = active.downcast::<ActiveActions>() else {
        return through_python();
    };
    let table = attribute(&world, intern!(py, "model_state"))?;
    let remembers = raw_dict_get(exact_dict(&table)?.as_any(), intern!(py, AGENT_DYNAMICS).as_any())?.is_some();
    let observed = match observed_state.downcast_exact::<ObservedRow>() {
        Ok(observed) => {
            let observed = observed.get();
            Observed::Row { data: observed.view.get().data(), index: observed.index }
        }
        Err(_) => plain_of(observed_state, 0).map_or(Observed::Other, Observed::Value),
    };
    let recording = Recording {
        data,
        proposal: &row.proposal,
        target_id: target,
        next_decision_at: next,
        committed: committed_flag.is_true(),
        observed: &observed,
        remembers,
    };

    let mut host = host_of(controller)?;
    match core::record(&PyWorldCell(&world), &mut host, &recording) {
        Ok(Sunk::Recorded) => {}
        Ok(Sunk::Python) => return through_python(),
        Err(error) => return Err(error.0),
    }
    if recording.committed {
        let committed = active
            .try_borrow_mut()
            .map_err(PyErr::from)
            .and_then(|mut active| active.with_rows(py, |rows| with_world(&world, |cell| core::commit(cell, rows, &recording))?.or_py()));
        committed.map_err(|error| kernel_refusal(py, LIFECYCLE, error))?;
    }
    with_world(&world, |cell| core::remember(cell, &recording))?.or_py().map_err(|error| kernel_refusal(py, MEMORY, error))
}

/// The step as the phase wrote it, for what the core does not take: a proposal Python made, a sink
/// without `decision_sink` (or a trace's writer before its start or after its end), a long observed
/// state, which the sink measures, values `with_outcome` or the rules raise on. The sink's
/// `record_decision` is the same one, with its own checks and texts; the rules after it are the
/// glue of their Python entries, called directly.
fn record_through_python<'py>(
    controller: &Bound<'py, PyAny>,
    proposal: &Bound<'py, PyAny>,
    target_id: &Bound<'py, PyAny>,
    next_decision_at: &Bound<'py, PyAny>,
    committed: &Bound<'py, PyAny>,
    observed_state: &Bound<'py, PyAny>,
) -> PyResult<()> {
    let py = controller.py();
    let recorded = proposal.call_method1(intern!(py, "with_outcome"), (target_id, next_decision_at))?;
    let options = PyDict::new(py);
    options.set_item(intern!(py, "observed_state"), observed_state)?;
    controller
        .getattr(intern!(py, "artifacts"))?
        .call_method(intern!(py, "record_decision"), (&recorded,), Some(&options))?;
    let world = controller.getattr(intern!(py, "world"))?;
    let active = controller.getattr(intern!(py, "active_actions"))?;
    record_action_proposal(&world, &recorded, target_id, committed, &active).map_err(|error| kernel_refusal(py, LIFECYCLE, error))?;
    if world.getattr(intern!(py, "model_state"))?.contains(intern!(py, AGENT_DYNAMICS))? {
        record_proposal(&world, &recorded, target_id).map_err(|error| kernel_refusal(py, MEMORY, error))?;
    }
    Ok(())
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<ProposalRow>()?;
    m.add_class::<ObservedRow>()?;
    m.add_function(wrap_pyfunction!(propose_rows, m)?)?;
    m.add_function(wrap_pyfunction!(observed_row, m)?)?;
    m.add_function(wrap_pyfunction!(record_outcome, m)?)?;
    Ok(())
}
