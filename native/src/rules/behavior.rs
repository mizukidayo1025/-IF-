//! The glue of the core's behavior rule (`sheltersim_core::rules::behavior`, lane P8 a5): what the
//! propose phase's `_propose_due` reads of Python, and the Python objects of a proposal.
//!
//! rules/decisions.rs gives these to the steps `_propose_due` and `_observed_state`, which have no
//! Python body (experiments/phases/segments.py, KERNEL_ONLY_SEGMENTS). The policy is the one the
//! controller's Run read (`run::policies_of`); a read that raised then raises again here by reading
//! it again (policy E14). The RNG, its seed and the view's snapshot and hash are read here, in the
//! order the kernel read them. The `ActionProposal`s are the Python classes, without their
//! generated `__init__`, made only when something reads one (rules/decisions.rs keeps the core's
//! proposals). `propose_due` and `observed_state` give the Python objects themselves, for tests.

use std::sync::Arc;

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::sync::GILOnceCell;
use pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyString, PyTuple, PyType};
use pyo3::IntoPyObjectExt;

use sheltersim_core::rules::behavior::{self as core, Proposal, ACTION_VALUES};
use sheltersim_core::rules::psychology::Bad;

use crate::core::errors::to_py_err;
use crate::core::run::policies_of;
use crate::core::value::VPy;
use crate::core::world_core::{raw_dict_get, raw_dict_set};
use crate::rules::psychology::policy_error;
use crate::rules::world_view::RustView;
use crate::state::entities::raw_setattr;
use crate::Unsupported;

#[track_caller]
fn unsupported<T>(reason: &'static str) -> PyResult<T> {
    Err(Unsupported::new_err(reason))
}

// ---- Python's side ---------------------------------------------------------------------------------

pub(crate) struct Classes {
    counter_rng: Py<PyType>,
    candidate: Py<PyType>,
    proposal: Py<PyType>,
    /// The members of `ActionKind`, in the order of `core::Action`.
    pub(crate) actions: Vec<Py<PyAny>>,
    new: Py<PyAny>,
}

static CLASSES: GILOnceCell<Classes> = GILOnceCell::new();

pub(crate) fn classes(py: Python<'_>) -> PyResult<&Classes> {
    CLASSES.get_or_try_init(py, || {
        let module = py.import("sheltersim.policies.behavior")?;
        let kind = |name: &str| -> PyResult<Py<PyType>> { Ok(module.getattr(name)?.downcast_into::<PyType>()?.unbind()) };
        let action_kind = module.getattr("ActionKind")?;
        let mut actions = Vec::with_capacity(ACTION_VALUES.len());
        for value in ACTION_VALUES {
            actions.push(action_kind.call1((value,))?.unbind());
        }
        Ok(Classes {
            counter_rng: py.import("sheltersim.core.rng")?.getattr("CounterRNG")?.downcast_into::<PyType>()?.unbind(),
            candidate: kind("ActionCandidate")?,
            proposal: kind("ActionProposal")?,
            actions,
            new: py.import("builtins")?.getattr("object")?.getattr("__new__")?.unbind(),
        })
    })
}

/// The error of what the core could not make of the policy: its refusal, or what reading the
/// controller's path again raises (policy E14).
fn bad_error(controller: &Bound<'_, PyAny>, bad: Bad) -> PyErr {
    match bad {
        Bad::Refused(error) => to_py_err(error),
        Bad::Policy(fault) => policy_error(controller, &fault.path, &fault.fault),
    }
}

/// The snapshot of a view the phases made. One Python made (`from_python_view`) has zeros for most
/// of what this rule reads, so `partial` is refused, as a view without a snapshot is.
pub(crate) fn snapshot_of<'py>(view: &Bound<'py, PyAny>) -> PyResult<Bound<'py, RustView>> {
    let py = view.py();
    let fields = view.getattr(intern!(py, "__dict__"))?;
    let Some(found) = raw_dict_get(&fields, intern!(py, "_rust_view").as_any())? else {
        return unsupported("the view is Python's");
    };
    let rust = found.downcast_into::<RustView>()?;
    if rust.get().data().partial {
        return unsupported("the view is partial");
    }
    Ok(rust)
}

/// `str(rng.seed)` of a `CounterRNG`, which keys its draws with it: a Scenario's seed is any int
/// from 0, past 64 bits too.
fn seed_of(classes: &Classes, rng: &Bound<'_, PyAny>) -> PyResult<String> {
    let py = rng.py();
    if !rng.get_type().is(classes.counter_rng.bind(py)) {
        return unsupported("the RNG is not a CounterRNG");
    }
    let seed = rng.getattr(intern!(py, "seed"))?;
    let Ok(seed) = seed.downcast_exact::<PyInt>() else {
        return unsupported("the seed is not an int");
    };
    if seed.lt(0)? {
        return unsupported("the seed is negative");
    }
    Ok(seed.str()?.to_str()?.to_owned())
}

/// The proposals of one `propose_due`: the snapshot they were made from and the core's proposals.
pub(crate) struct Due<'py> {
    pub(crate) rust: Bound<'py, RustView>,
    pub(crate) proposals: Vec<Proposal>,
}

/// `self.decisions.propose_due(view, rng)` of the propose phase, with no Python object for a
/// proposal; None when nobody is due.
pub(crate) fn due_proposals<'py>(
    controller: &Bound<'py, PyAny>,
    view: &Bound<'py, PyAny>,
    rng: &Bound<'py, PyAny>,
) -> PyResult<Option<Due<'py>>> {
    let py = controller.py();
    let classes = classes(py)?;
    let policies = policies_of(controller)?;
    let scoring = core::scoring_of(&policies.behavior).map_err(|bad| bad_error(controller, bad))?;
    let seed = seed_of(classes, rng)?;
    let snapshot = snapshot_of(view)?;
    let state_hash = view.getattr(intern!(py, "state_hash"))?;
    let hash = match state_hash.downcast_exact::<PyString>() {
        Ok(text) if text.len().is_ok_and(|length| length == 64) => text.to_str().ok().map(Arc::<str>::from),
        _ => None,
    };
    let Some(hash) = hash else {
        return unsupported("the view has no hash of a state");
    };
    let proposals = core::due(snapshot.get().data(), &scoring, &seed, hash).map_err(|bad| bad_error(controller, bad))?;
    Ok(proposals.map(|proposals| Due { rust: snapshot, proposals }))
}

fn new_object<'py>(classes: &Classes, kind: &Py<PyType>, fields: &Bound<'py, PyDict>) -> PyResult<Bound<'py, PyAny>> {
    let py = fields.py();
    let object = classes.new.bind(py).call1((kind.bind(py),))?;
    raw_setattr(&object, intern!(py, "__dict__"), fields.as_ptr())?;
    Ok(object)
}

macro_rules! set {
    ($py:expr, $dict:expr, $name:literal, $value:expr) => {
        raw_dict_set($dict.as_any(), intern!($py, $name).as_any(), $value.as_any())?
    };
}

/// One `ActionProposal`, with the fields in the order its `__init__` sets them.
pub(crate) fn proposal_of<'py>(py: Python<'py>, rust: &RustView, proposal: &Proposal) -> PyResult<Bound<'py, PyAny>> {
    let classes = classes(py)?;
    let data = rust.data();
    let outcome = &proposal.outcome;
    let common = &proposal.common;
    let mut candidates = Vec::with_capacity(outcome.candidates.len());
    for item in &outcome.candidates {
        let fields = PyDict::new(py);
        set!(py, fields, "action", classes.actions[item.action as usize].bind(py));
        set!(py, fields, "eligible", PyBool::new(py, item.eligible));
        set!(py, fields, "trigger", PyString::new(py, &proposal.trigger_of(data, item).map_err(to_py_err)?));
        set!(py, fields, "utility", PyFloat::new(py, item.utility));
        set!(py, fields, "urgency_priority", item.priority.into_bound_py_any(py)?);
        set!(py, fields, "probability", PyFloat::new(py, item.probability));
        candidates.push(new_object(classes, &classes.candidate, &fields)?);
    }
    let mut key = Vec::with_capacity(9);
    for part in proposal.rng_key(data) {
        key.push(match part {
            Ok(text) => PyString::new(py, text).into_any(),
            Err(number) => number.into_bound_py_any(py)?,
        });
    }
    let fields = PyDict::new(py);
    set!(py, fields, "agent_id", rust.key(py, proposal.index));
    set!(py, fields, "minute_index", data.minute_index.into_bound_py_any(py)?);
    set!(py, fields, "observed_state_hash", PyString::new(py, &common.state_hash));
    set!(py, fields, "action", classes.actions[outcome.action as usize].bind(py));
    set!(py, fields, "candidates", PyTuple::new(py, candidates)?);
    set!(py, fields, "trigger", intern!(py, "scheduled_decision"));
    set!(py, fields, "choice_value", PyFloat::new(py, outcome.choice_value));
    set!(py, fields, "rng_key", PyTuple::new(py, key)?);
    set!(py, fields, "selection_probability", PyFloat::new(py, outcome.selection_probability));
    set!(py, fields, "policy_version", PyString::new(py, &common.policy_version));
    set!(py, fields, "next_decision_at", outcome.next_decision_at.into_bound_py_any(py)?);
    match outcome.target {
        Some(target) => set!(py, fields, "target_id", rust.key(py, target)),
        None => set!(py, fields, "target_id", py.None().into_bound(py)),
    }
    new_object(classes, &classes.proposal, &fields)
}

/// `self.decisions.propose_due(view, rng)` as Python objects, the comparison base of the rows.
#[pyfunction]
fn propose_due<'py>(
    controller: &Bound<'py, PyAny>,
    view: &Bound<'py, PyAny>,
    rng: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = controller.py();
    let Some(due) = due_proposals(controller, view, rng)? else {
        return Ok(PyTuple::empty(py).into_any());
    };
    let mut proposals = Vec::with_capacity(due.proposals.len());
    for proposal in &due.proposals {
        proposals.push(proposal_of(py, due.rust.get(), proposal)?);
    }
    Ok(PyTuple::new(py, proposals)?.into_any())
}

/// `_agent_view_snapshot(view.agents[agent_id])`, as `_observed_state` of the propose phase.
#[pyfunction]
fn observed_state<'py>(
    controller: &Bound<'py, PyAny>,
    view: &Bound<'py, PyAny>,
    agent_id: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyDict>> {
    let py = controller.py();
    let rust = snapshot_of(view)?;
    let index = person_of(rust.get(), agent_id)?;
    observed_dict(py, rust.get(), index)
}

/// The place in the snapshot of the person with this ID.
pub(crate) fn person_of(rust: &RustView, agent_id: &Bound<'_, PyAny>) -> PyResult<usize> {
    let Ok(id) = agent_id.downcast_exact::<PyString>() else {
        return unsupported("the ID is not a str");
    };
    let id_text = id.to_str().map_err(|_| Unsupported::new_err("the ID is not UTF-8"))?;
    rust.data().index_of(id_text).ok_or_else(|| Unsupported::new_err("no such person"))
}

/// `_agent_view_snapshot` as a dict (the core's `observed_value`).
pub(crate) fn observed_dict<'py>(py: Python<'py>, rust: &RustView, index: usize) -> PyResult<Bound<'py, PyDict>> {
    Ok(core::observed_value(rust.data(), index).to_py(py)?.downcast_into::<PyDict>()?)
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(propose_due, m)?)?;
    m.add_function(wrap_pyfunction!(observed_state, m)?)?;
    Ok(())
}
