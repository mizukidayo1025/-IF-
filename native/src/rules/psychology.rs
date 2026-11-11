//! The glue of the core's psychology (`sheltersim_core::rules::psychology`, lane P8 a3): the steps
//! of the update phase `_psychology_update`, `_update_long_term_psychology` and
//! `_apply_state_deltas` (experiments/phases/segments.py, KERNEL_ONLY_SEGMENTS).
//!
//! Each step reads what only Python has (the minute and the view's flag as the Python objects they
//! are, the Python deltas the social step and the Python phase path hand on), takes the model from
//! the controller's `Policies` (`run::policies_of`: its Run's, or read now for a holder without a
//! Run), calls the core with the world borrowed, and makes the Python values: the
//! `AgentStateDelta`s, which are only made here at the edge, and `PsychologyUpdateResult`. The core's
//! result is kept in the Run's `Scratch` for the core steps that take it. A read of the policy that
//! raised when the Run read it raises again here by reading it again (policy E14).

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::sync::GILOnceCell;
use pyo3::types::{PyBool, PyFloat, PyInt, PyString, PyTuple, PyType};

use sheltersim_core::config::Fault;
use sheltersim_core::rules::kit::Held;
use sheltersim_core::rules::psychology::{self as core, Applied, Bad, Delta};
use sheltersim_core::state::body_state::NAME as BODY_STATE;

use crate::core::errors::{to_py_err, OrPy};
use crate::core::row::attribute;
use crate::core::run::{policies_of, run_of};
use crate::core::tracked::as_readable_dict;
use crate::core::world_core::{field_held, namespace_held, raw_dict_get, raw_dict_set, with_world};
use crate::host::PyHost;
use crate::rules::trace::TraceRows;
use crate::rules::world_view::view_of;
use crate::Unsupported;

#[track_caller]
fn unsupported<T>(reason: &'static str) -> PyResult<T> {
    Err(Unsupported::new_err(reason))
}

/// The error of a read of the controller's policies at `path` that the Run kept as `fault`: its
/// refusal, or what reading the path again raises (policy E14).
pub(crate) fn policy_error(controller: &Bound<'_, PyAny>, path: &[&str], fault: &Fault) -> PyErr {
    let fault = core::PolicyFault { path: Vec::new(), fault: fault.clone() };
    if let Some(text) = fault.refusal() {
        return Unsupported::new_err(text.to_owned());
    }
    let mut found = controller.clone();
    for name in path {
        match found.getattr(*name) {
            Ok(next) => found = next,
            Err(error) => return error,
        }
    }
    Unsupported::new_err(fault.fault.text.to_string())
}

/// The error of what the core could not make of the policy.
fn bad_error(controller: &Bound<'_, PyAny>, bad: Bad) -> PyErr {
    match bad {
        Bad::Refused(error) => to_py_err(error),
        Bad::Policy(fault) => policy_error(controller, &fault.path, &fault.fault),
    }
}

/// An exact float the Python check `validate_unit_interval` passes.
fn unit_float(value: &Bound<'_, PyAny>) -> PyResult<f64> {
    let Ok(number) = value.downcast_exact::<PyFloat>() else {
        return unsupported("a value is not a float");
    };
    let number = number.value();
    if !(0.0..=1.0).contains(&number) {
        return unsupported("a value is outside 0..1");
    }
    Ok(number)
}

/// The minute as an i64 the steps take: an exact int, not negative.
fn minute_of(minute: &Bound<'_, PyAny>) -> PyResult<i64> {
    let Ok(minute) = minute.downcast_exact::<PyInt>() else {
        return unsupported("the minute is not an int");
    };
    let minute: i64 = minute.extract().map_err(|_| Unsupported::new_err("the minute is too large"))?;
    if minute < 0 || minute == i64::MAX {
        return unsupported("the minute is negative or too large");
    }
    Ok(minute)
}

// ---- The step ----------------------------------------------------------------------------------

/// The IDs and final stress of `final_deltas`, an exact tuple of deltas with exact str IDs and
/// stress within 0..1.
fn finals_of<'py>(deltas: &Bound<'py, PyAny>) -> PyResult<Vec<(Bound<'py, PyString>, f64)>> {
    let py = deltas.py();
    let Ok(deltas) = deltas.downcast_exact::<PyTuple>() else {
        return unsupported("the deltas are not a tuple");
    };
    let mut found = Vec::with_capacity(deltas.len());
    for delta in deltas.iter_borrowed() {
        let id = delta.getattr(intern!(py, "agent_id"))?;
        let Ok(id) = id.downcast_into_exact::<PyString>() else {
            return unsupported("an id is not a str");
        };
        // A lone surrogate has no UTF-8; after this, to_str does not fail for these ids.
        if id.to_str().is_err() {
            return unsupported("an id is not UTF-8");
        }
        let stress = unit_float(&delta.getattr(intern!(py, "stress"))?)?;
        found.push((id, stress));
    }
    Ok(found)
}

/// `_update_long_term_psychology` of the update phase (experiments/phases/segments.py).
#[pyfunction]
pub(crate) fn update_long_term<'py>(
    controller: &Bound<'py, PyAny>,
    world: &Bound<'py, PyAny>,
    _view: &Bound<'py, PyAny>,
    final_deltas: &Bound<'py, PyAny>,
    minute: &Bound<'py, PyAny>,
) -> PyResult<()> {
    let py = world.py();
    let policies = policies_of(controller)?;
    let psychology = &policies.psychology;
    if !core::keeps_long_term(psychology).map_err(|bad| bad_error(controller, bad))? {
        return Ok(());
    }
    let minute = minute_of(minute)?;
    let long_term = core::long_term_of(psychology).map_err(|bad| bad_error(controller, bad))?;
    let people = finals_of(final_deltas)?;
    let mut finals = Vec::with_capacity(people.len());
    for (id, stress) in &people {
        finals.push((id.to_str()?, *stress));
    }
    let held = field_held(world, intern!(py, "metric_state"))?;
    with_world(world, |world| core::update_long_term(world, &long_term, &finals, minute, held))?.or_py()
}

// ---- A minute of each person's values ----------------------------------------------------------

static DELTA_TYPES: GILOnceCell<(Py<PyType>, Py<PyType>, Py<PyAny>)> = GILOnceCell::new();

/// `AgentStateDelta`, `PsychologyUpdateResult` and `object.__new__`.
fn delta_types(py: Python<'_>) -> PyResult<&(Py<PyType>, Py<PyType>, Py<PyAny>)> {
    DELTA_TYPES.get_or_try_init(py, || {
        let module = py.import("sheltersim.policies.psychology")?;
        let new = py.import("builtins")?.getattr("object")?.getattr("__new__")?;
        Ok((
            module.getattr("AgentStateDelta")?.downcast_into::<PyType>()?.unbind(),
            module.getattr("PsychologyUpdateResult")?.downcast_into::<PyType>()?.unbind(),
            new.unbind(),
        ))
    })
}

/// The names of an `AgentStateDelta`'s floats as Python strings: `NEEDS_AND_TRUST`, then
/// `BODY_NAMES`.
fn float_names(py: Python<'_>) -> [&Bound<'_, PyString>; 13] {
    [
        intern!(py, "toilet_urgency"),
        intern!(py, "hunger"),
        intern!(py, "fatigue"),
        intern!(py, "hydration_need"),
        intern!(py, "stress"),
        intern!(py, "trust"),
        intern!(py, "sleep_debt_minutes"),
        intern!(py, "uncertainty"),
        intern!(py, "reassurance"),
        intern!(py, "discontent"),
        intern!(py, "positive_affect"),
        intern!(py, "negative_affect"),
        intern!(py, "hygiene_need"),
    ]
}

/// The Python `AgentStateDelta` of a core delta, whose values are known to pass its checks.
fn new_delta<'py>(
    types: &(Py<PyType>, Py<PyType>, Py<PyAny>),
    key: &Bound<'py, PyString>,
    minute: &Bound<'py, PyAny>,
    delta: &Delta,
) -> PyResult<Bound<'py, PyAny>> {
    let py = key.py();
    let object = types.2.bind(py).call1((types.0.bind(py),))?;
    let fields = object.getattr(intern!(py, "__dict__"))?;
    let names = float_names(py);
    raw_dict_set(&fields, intern!(py, "agent_id").as_any(), key.as_any())?;
    raw_dict_set(&fields, intern!(py, "minute_index").as_any(), minute)?;
    let values = [delta.toilet_urgency, delta.hunger, delta.fatigue, delta.hydration_need, delta.stress, delta.trust];
    for (name, value) in names[..6].iter().zip(values) {
        raw_dict_set(&fields, name.as_any(), PyFloat::new(py, value).as_any())?;
    }
    match &delta.body {
        Some(body) => {
            for (name, value) in names[6..].iter().zip(body) {
                raw_dict_set(&fields, name.as_any(), PyFloat::new(py, *value).as_any())?;
            }
        }
        None => {
            let none = py.None().into_bound(py);
            for name in &names[6..] {
                raw_dict_set(&fields, name.as_any(), &none)?;
            }
        }
    }
    Ok(object)
}

/// `_psychology_update` of the update phase. With a trace the result's `traces` are `TraceRows`, which
/// `record_psychology_trace` of rules/trace.rs makes the document of.
#[pyfunction]
pub(crate) fn update_psychology<'py>(
    controller: &Bound<'py, PyAny>,
    view: &Bound<'py, PyAny>,
    minute: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = controller.py();
    let policies = policies_of(controller)?;
    let host = PyHost::of_holder(controller, policies.clone());
    let traced = sheltersim_core::host::Host::trace_level(&host).map_err(|error| error.0)?
        != sheltersim_core::host::TraceLevel::Off;
    let psychology = &policies.psychology;
    let model = core::model_of(psychology).map_err(|bad| bad_error(controller, bad))?;
    let world = controller.getattr(intern!(py, "world"))?;
    if !minute.is_exact_instance_of::<PyInt>() || !minute.eq(view.getattr(intern!(py, "minute_index"))?)? {
        return unsupported("the minute is not the view's");
    }
    let Ok(minute_value) = minute.extract::<i64>() else {
        return unsupported("the minute is negative or too large");
    };
    if minute_value < 0 {
        return unsupported("the minute is negative or too large");
    }
    let flag = view.getattr(intern!(py, "resource_service_enabled"))?;
    let flag = flag.downcast_exact::<PyBool>().ok().map(|flag| flag.is_true());
    let params = core::params(psychology, model, flag, traced).map_err(|bad| bad_error(controller, bad))?;
    let rust_view = view_of(view)?;
    let data = rust_view.get().data();
    let result = with_world(&world, |world| core::update(world, data, &params, minute_value, traced))?.or_py()?;
    if let Ok(run) = run_of(controller) {
        run.get().with_scratch(|scratch| scratch.put(result.clone()))?;
    }

    let types = delta_types(py)?;
    let mut deltas = Vec::with_capacity(result.deltas.len());
    for (index, delta) in result.deltas.iter().enumerate() {
        deltas.push(new_delta(types, rust_view.get().key(py, index), minute, delta)?);
    }
    let deltas = PyTuple::new(py, deltas)?;
    let traces = match &result.traces {
        Some(traces) => Bound::new(py, TraceRows::new(rust_view.clone().unbind(), traces.clone()))?.into_any(),
        None => PyTuple::empty(py).into_any(),
    };
    types.1.bind(py).call1((deltas, traces, PyString::new(py, &result.policy_version)))
}

// ---- Writing the new values ---------------------------------------------------------------------

/// Whether `model_state` holds `body_state_v1`; anything else it holds there is refused.
fn body_state_of(world: &Bound<'_, PyAny>) -> PyResult<bool> {
    let py = world.py();
    if as_readable_dict(&attribute(world, intern!(py, "model_state"))?).is_none() {
        return unsupported("model_state is not readable");
    }
    match namespace_held(world, intern!(py, BODY_STATE), "body_state_v1 is not the tree of its row")? {
        Held::Absent => Ok(false),
        Held::Held => Ok(true),
        Held::Other(reason) => unsupported(reason),
    }
}

/// `apply_state_deltas`. Every delta is read and checked, by what Python's checks accept without
/// their slow path, before anything changes; the core checks the rest and writes.
#[pyfunction]
pub(crate) fn apply_deltas<'py>(
    _controller: &Bound<'py, PyAny>,
    world: &Bound<'py, PyAny>,
    deltas: &Bound<'py, PyAny>,
) -> PyResult<()> {
    let py = world.py();
    let Ok(deltas) = deltas.downcast_exact::<PyTuple>() else {
        return unsupported("the deltas are not a tuple");
    };
    let world_minute = world.getattr(intern!(py, "minute_index"))?;
    let delta_class = &delta_types(py)?.0;
    let body_state = body_state_of(world)?;
    let names = float_names(py);
    let mut read = Vec::with_capacity(deltas.len());
    for delta in deltas.iter_borrowed() {
        if !delta.get_type().is(delta_class) {
            return unsupported("a delta is not an AgentStateDelta");
        }
        let fields = delta.getattr(intern!(py, "__dict__"))?;
        let item = |name: &Bound<'py, PyString>| -> PyResult<Bound<'py, PyAny>> {
            raw_dict_get(&fields, name.as_any())?.ok_or_else(|| Unsupported::new_err("a field is missing"))
        };
        let id = item(intern!(py, "agent_id"))?;
        let Ok(id) = id.downcast_into_exact::<PyString>() else {
            return unsupported("an id is not a str");
        };
        if id.to_str().is_err() {
            return unsupported("an id is not UTF-8");
        }
        let minute = item(intern!(py, "minute_index"))?;
        if !minute.is_exact_instance_of::<PyInt>() || !minute.eq(&world_minute)? {
            return unsupported("a delta is not of this minute");
        }
        let objects = [item(names[0])?, item(names[1])?, item(names[2])?, item(names[3])?, item(names[4])?, item(names[5])?];
        let mut values = [0.0; 6];
        for (place, value) in objects.iter().enumerate() {
            values[place] = unit_float(value)?;
        }
        let mut body: [Option<f64>; 7] = Default::default();
        for (place, name) in names[6..].iter().enumerate() {
            let value = item(name)?;
            if !value.is_none() {
                let Ok(value) = value.downcast_exact::<PyFloat>() else {
                    return unsupported("a body value is not a float");
                };
                body[place] = Some(value.value());
            }
        }
        read.push((id, values, body));
    }
    let mut applied = Vec::with_capacity(read.len());
    for (id, values, body) in &read {
        applied.push(Applied { agent_id: id.to_str()?, values: *values, body: *body });
    }
    with_world(world, |world| core::apply(world, &applied, body_state))?.or_py()
}

// ---- For the bit comparisons of tests/integration/test_psychology_row.py -----------------------

/// `next_long_term_distress` of `domain/psychological_state.py`, from a record's fields.
#[pyfunction]
#[allow(clippy::too_many_arguments)]
fn next_long_term_distress_of(
    baseline: f64,
    long_term: f64,
    daily_acute_sum: f64,
    daily_high_distress_minutes: i64,
    daily_observed_minutes: i64,
    retention: f64,
    acute_weight: f64,
    high_distress_weight: f64,
) -> f64 {
    let record = sheltersim_core::state::psychological_distress::AgentRecord {
        baseline_distress: baseline,
        long_term_distress: long_term,
        daily_acute_sum,
        daily_high_distress_minutes,
        daily_observed_minutes,
        completed_days: 0,
    };
    let parameters =
        core::LongTermParameters { retention, acute_weight, high_distress_weight, high_distress_threshold: 1.0 };
    core::next_long_term_distress(&record, &parameters)
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(update_psychology, m)?)?;
    m.add_function(wrap_pyfunction!(update_long_term, m)?)?;
    m.add_function(wrap_pyfunction!(apply_deltas, m)?)?;
    m.add_function(wrap_pyfunction!(next_long_term_distress_of, m)?)?;
    Ok(())
}
