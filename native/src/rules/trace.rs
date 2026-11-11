//! The glue of the core's trace documents (`sheltersim_core::rules::trace`, lane P8 a3).
//!
//! The documents are made in the core as `V` with the keys of the Python dicts in their order, and
//! go to the sink as Python dicts: the psychology document through the core's `record_psychology`
//! and the native host (`Host::trace`, which calls `artifacts.record_psychology_trace`), the social
//! contact document through `TraceSink` here, which the social and information kernels use until
//! they move to the core (lane a4). `_record_psychology_trace` of the update phase is
//! `record_psychology_trace`: it reads the result's rows and the final deltas, and calls the core.

use std::sync::Arc;

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyString};

use sheltersim_core::host::TraceLevel;
use sheltersim_core::rules::trace::{self as core, TraceInputs, Traces};
use sheltersim_core::value::V;

pub(crate) use sheltersim_core::rules::trace::SocialContact;

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

/// Where a tick's documents go: the controller's `artifacts`, if its trace is on, read now (the
/// social and information kernels; lane a4 moves them to the host).
pub(crate) struct TraceSink<'py> {
    artifacts: Bound<'py, PyAny>,
    pub(crate) level: Level,
}

impl<'py> TraceSink<'py> {
    pub(crate) fn of(controller: &Bound<'py, PyAny>) -> PyResult<Option<TraceSink<'py>>> {
        let artifacts = controller.getattr(intern!(controller.py(), "artifacts"))?;
        let level = Level::of(&artifacts)?;
        Ok((level != Level::Off).then_some(TraceSink { artifacts, level }))
    }

    /// `maximum_contacts` of the sample phase's document.
    pub(crate) fn information_items(&self, contacts: usize) -> usize {
        if self.level == Level::Full {
            contacts
        } else {
            core::SUMMARY_ITEMS
        }
    }

    pub(crate) fn social(&self, document: &V) -> PyResult<()> {
        let py = self.artifacts.py();
        self.artifacts.call_method1(intern!(py, "record_social_contact_trace"), (document.to_py(py)?,)).map(|_| ())
    }

    pub(crate) fn information(&self, document: &Bound<'py, PyDict>) -> PyResult<()> {
        let py = document.py();
        self.artifacts.call_method1(intern!(py, "record_information_spread_trace"), (document,)).map(|_| ())
    }
}

/// `SocialPropagationResult.trace_document` of the core, with the minute and the model's version as
/// the Python values they are.
pub(crate) fn social_document(
    _py: Python<'_>,
    minute: &Bound<'_, PyAny>,
    policy_version: &Bound<'_, PyAny>,
    ids: &[&str],
    contacts: &[SocialContact],
    contributing: i64,
    total_added: f64,
) -> PyResult<V> {
    Ok(core::social_document(&V::from_py(minute)?, &V::from_py(policy_version)?, ids, contacts, contributing, total_added))
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
