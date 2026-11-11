//! The glue of the core's trace documents (`sheltersim_core::rules::trace`, lane P8 a3).
//!
//! The documents are made in the core as `V` with the keys of the Python dicts in their order, and
//! go to the sink as Python dicts through the native host (`Host::trace`, which calls
//! `artifacts.record_*_trace`): the psychology document here, through the core's
//! `record_psychology`, and the social and information documents from their steps (lane a4).
//! `_record_psychology_trace` of the update phase is `record_psychology_trace`: it reads the
//! result's rows and the final deltas, and calls the core.

use std::sync::Arc;

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::PyString;

use sheltersim_core::host::TraceLevel;
use sheltersim_core::rules::trace::{self as core, TraceInputs, Traces};
use sheltersim_core::value::V;

use crate::core::row::{exact_float, exact_str, unsupported};
use crate::core::run::policies_of;
use crate::core::value::VPy;
use crate::host::PyHost;
use crate::rules::psychology::policy_error;
use crate::rules::world_view::RustView;

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Level {
    Off,
    Summary,
    Full,
}

impl Level {
    pub(crate) fn of(artifacts: &Bound<'_, PyAny>) -> PyResult<Level> {
        let py = artifacts.py();
        let level = artifacts.getattr(intern!(py, "trace_level"))?;
        Ok(match level.downcast::<PyString>()?.to_str()? {
            "off" => Level::Off,
            "summary" => Level::Summary,
            "full" => Level::Full,
            _ => return unsupported("a trace level this kernel does not know"),
        })
    }
}

// ---- A minute's psychology -----------------------------------------------------------------------

/// `PsychologyUpdateResult.traces` of the psychology kernel: the core's rows, and the view they
/// index, which `record_psychology_trace` makes the document of.
#[pyclass(module = "sheltersim_native", frozen)]
pub struct TraceRows {
    view: Py<RustView>,
    traces: Arc<Traces>,
}

impl TraceRows {
    pub(crate) fn new(view: Py<RustView>, traces: Arc<Traces>) -> TraceRows {
        TraceRows { view, traces }
    }
}

#[pymethods]
impl TraceRows {
    fn __len__(&self) -> usize {
        self.traces.rows.len()
    }
}

/// `_record_psychology_trace` of the update phase. It changes nothing but what the sink keeps.
#[pyfunction]
pub(crate) fn record_psychology_trace<'py>(
    controller: &Bound<'py, PyAny>,
    result: &Bound<'py, PyAny>,
    final_deltas: &Bound<'py, PyAny>,
    minute: &Bound<'py, PyAny>,
) -> PyResult<()> {
    let py = controller.py();
    let policies = policies_of(controller)?;
    let mut host = PyHost::of_holder(controller, policies.clone());
    if sheltersim_core::host::Host::trace_level(&host).map_err(|error| error.0)? == TraceLevel::Off {
        return Ok(());
    }
    let traces = result.getattr(intern!(py, "traces"))?;
    let Ok(rows) = traces.downcast_exact::<TraceRows>() else {
        return unsupported("the traces are not the psychology kernel's rows");
    };
    let rows = rows.get();
    let view = rows.view.get();

    // `final_by_agent`, as the Python step read it.
    let deltas: Vec<Bound<'py, PyAny>> = final_deltas.try_iter()?.collect::<PyResult<_>>()?;
    let mut finals = Vec::with_capacity(deltas.len());
    let mut ids = Vec::with_capacity(deltas.len());
    for delta in &deltas {
        let id = delta.getattr(intern!(py, "agent_id"))?;
        exact_str(&id)?;
        finals.push(exact_float(&delta.getattr(intern!(py, "stress"))?)?);
        ids.push(id);
    }
    let mut by_agent = Vec::with_capacity(deltas.len());
    for (id, stress) in ids.iter().zip(finals) {
        by_agent.push((exact_str(id)?, stress));
    }

    let source_id = match policies.psychology.policy.as_ref() {
        Ok(policy) => match policy.get("source_id") {
            Ok(held) => held_value(held)?,
            Err(fault) => return Err(policy_error(controller, &["psychology", "source_id"], &fault)),
        },
        Err(fault) => return Err(policy_error(controller, &["psychology"], fault)),
    };
    let inputs = TraceInputs {
        source_id: &source_id,
        policy_version: &V::from_py(&result.getattr(intern!(py, "policy_version"))?)?,
        minute: &V::from_py(minute)?,
    };
    core::record_psychology(&mut host, view.data(), &rows.traces, &by_agent, &inputs).map_err(|error| error.0)
}

/// A value the Run read as the document holds it: a str subclass by its text.
fn held_value(held: &sheltersim_core::run::held::Held) -> PyResult<V> {
    use sheltersim_core::run::held::Held;
    match held {
        Held::Value(value) => Ok(value.clone()),
        Held::Text(text) => Ok(V::Str(text.clone())),
        _ => unsupported("a source id is not a plain value"),
    }
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<TraceRows>()?;
    m.add_function(wrap_pyfunction!(record_psychology_trace, m)?)?;
    Ok(())
}
