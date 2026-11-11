//! The glue of the core's information model (`sheltersim_core::rules::information`, lane P8 a4):
//! `_sample_information_contacts` of the sample phase (experiments/phases/segments.py,
//! KERNEL_ONLY_SEGMENTS).
//!
//! The step reads what only Python has: the model and its config from the controller's `Policies`
//! (`run::policies_of`), the step's `CounterRNG` and its seed, the minute and the world's minute,
//! and `controller.zone_neighbors`. It calls the core with the world borrowed, which plans, writes
//! the row and schedules the transfers; the trace's document goes through the host (`Host::trace`).
//!
//! The readers of a detached namespace at the end are `proposals.rs`'s (lane e1 moves them).

use std::collections::HashMap;
use std::sync::Arc;

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::sync::GILOnceCell;
use pyo3::types::{PyDict, PyInt, PyList, PyString, PyType};

use sheltersim_core::contacts::Basis;
use sheltersim_core::host::{Host, TraceLevel};
use sheltersim_core::rules::information::{self as core, Inputs, Settings};
use sheltersim_core::run::information::Model;
use sheltersim_core::run::policies::Policies;
use sheltersim_core::value::V;

use crate::contacts::exact_int;
use crate::core::errors::OrPy;
use crate::core::rng::{start_draw_log, take_draw_log};
use crate::core::run::policies_of;
use crate::core::value::VPy;
use crate::core::world_core::{mirror_of, with_world, NamespaceMirror};
use crate::host::PyHost;
use crate::rules::psychology::bad_error;
use crate::state::events::write_result;
use crate::state::information::{InformationRow, NAME};
use crate::Unsupported;

#[track_caller]
fn unsupported<T>(reason: &'static str) -> PyResult<T> {
    Err(Unsupported::new_err(reason))
}

static COUNTER_RNG: GILOnceCell<Py<PyType>> = GILOnceCell::new();

fn counter_rng(py: Python<'_>) -> PyResult<&Py<PyType>> {
    COUNTER_RNG.get_or_try_init(py, || {
        Ok(py.import("sheltersim.core.rng")?.getattr("CounterRNG")?.downcast_into::<PyType>()?.unbind())
    })
}

/// What the step reads besides the rows.
struct Gathered {
    model: Model,
    settings: Settings,
    minute: i64,
    seed: String,
    neighbors: HashMap<String, Vec<String>>,
}

impl Gathered {
    fn inputs(&self, recording: bool) -> Inputs<'_> {
        Inputs {
            model: self.model,
            settings: &self.settings,
            minute: self.minute,
            seed: &self.seed,
            neighbors: &self.neighbors,
            recording,
        }
    }
}

/// What the step reads besides the rows, and the world; None when no model is installed: the step
/// then does nothing.
fn gather<'py>(
    controller: &Bound<'py, PyAny>,
    policies: &Policies,
    minute: &Bound<'py, PyAny>,
    rng: &Bound<'py, PyAny>,
) -> PyResult<Option<(Gathered, Bound<'py, PyAny>)>> {
    let py = controller.py();
    let information = &policies.information;
    let Some(model) = core::model_of(information).map_err(|bad| bad_error(controller, bad))? else {
        return Ok(None);
    };
    if !rng.get_type().is(counter_rng(py)?) {
        return unsupported("the random source is not a CounterRNG");
    }
    let Ok(minute) = minute.downcast_exact::<PyInt>() else {
        return unsupported("the minute is not an int");
    };
    let minute: i64 = minute.extract().map_err(|_| Unsupported::new_err("the minute is too large"))?;
    // `CounterRNG` keys its draws with `str(seed)`: a Scenario's seed is any int from 0, past 64
    // bits too.
    let seed = rng.getattr(intern!(py, "seed"))?;
    let Ok(seed) = seed.downcast_exact::<PyInt>() else {
        return unsupported("the seed is not an int");
    };
    if minute < 0 || minute == i64::MAX || seed.lt(0)? {
        return unsupported("the minute or the seed is out of range");
    }
    let world = controller.getattr(intern!(py, "world"))?;
    // The Python rule read the view's minute.
    if exact_int(&world.getattr(intern!(py, "minute_index"))?)? != minute {
        return unsupported("the world is at another minute");
    }
    let settings = core::settings(information, model).map_err(|bad| bad_error(controller, bad))?;
    let neighbors: HashMap<String, Vec<String>> = controller
        .getattr(intern!(py, "zone_neighbors"))?
        .extract()
        .map_err(|_| Unsupported::new_err("the zone neighbors are not str tuples"))?;
    let seed = seed.str()?.to_str()?.to_owned();
    Ok(Some((Gathered { model, settings, minute, seed, neighbors }, world)))
}

/// `_sample_information_contacts` of the sample phase (experiments/phases/segments.py).
#[pyfunction]
pub(crate) fn sample_contacts<'py>(
    controller: &Bound<'py, PyAny>,
    minute: &Bound<'py, PyAny>,
    rng: &Bound<'py, PyAny>,
) -> PyResult<()> {
    let py = controller.py();
    let policies = policies_of(controller)?;
    let Some((gathered, world)) = gather(controller, &policies, minute, rng)? else {
        return Ok(());
    };
    let mut host = PyHost::of_holder(controller, policies);
    let recording = host.trace_level().map_err(|error| error.0)? != TraceLevel::Off;
    let inputs = gathered.inputs(recording);
    let plan = write_result(py, with_world(&world, |world| core::sample(world, &inputs))?)?;
    if recording {
        core::record_information(&mut host, &plan, gathered.model, &<V as VPy>::from_py(minute)?).map_err(|error| error.0)?;
    }
    Ok(())
}

// ---- For the bit comparisons of benchmarks/rule_shadow and tests/integration ---------------------

fn optional<'py>(py: Python<'py>, text: &Option<Arc<str>>) -> Bound<'py, PyAny> {
    match text {
        Some(text) => PyString::new(py, text).into_any(),
        None => py.None().into_bound(py),
    }
}

/// What this tick would do, changing nothing: the draws in the order they were made, as
/// `(key, value)`, each contact as the Python rule's `InformationContactTrace` fields, the model's
/// version and its forgetting probability, which no contact shows. Says Unsupported where
/// `sample_contacts` would, but for the event queue. The planned draws show in `core/rng.rs`'s log
/// of this thread, which this leaves off.
#[pyfunction]
fn planned_contacts<'py>(
    controller: &Bound<'py, PyAny>,
    minute: &Bound<'py, PyAny>,
    rng: &Bound<'py, PyAny>,
) -> PyResult<Option<Bound<'py, PyDict>>> {
    let py = controller.py();
    let Some((gathered, world)) = gather(controller, &*policies_of(controller)?, minute, rng)? else {
        return Ok(None);
    };
    let inputs = gathered.inputs(true);
    start_draw_log();
    let plan = with_world(&world, |world| core::plan(world, &inputs));
    let made = take_draw_log();
    let plan = plan?.or_py()?;
    let document = PyDict::new(py);
    document.set_item("draws", made)?;
    let contacts = PyList::empty(py);
    for trace in &plan.traces {
        contacts.append((
            &*plan.ids[trace.sender],
            &*plan.ids[trace.recipient],
            trace.basis,
            trace.minutes,
            optional(py, &trace.message),
            optional(py, &trace.belief),
            trace.probability,
            trace.selected,
            optional(py, &trace.source),
            optional(py, &trace.causation),
        ))?;
    }
    document.set_item("contacts", contacts)?;
    document.set_item("policy_version", core::version(gathered.model))?;
    document.set_item("forget_probability", gathered.settings.forget_probability)?;
    Ok(Some(document))
}

/// `-expm1(-rate)`, the forgetting probability, and `_transmission_probability` for the tests.
#[pyfunction]
#[allow(clippy::too_many_arguments)]
fn transmission_probability_of(
    hazard: bool,
    trust: f64,
    family: bool,
    adjacent: bool,
    contact_minutes: i64,
    correction: bool,
    duration_scale: f64,
    base_probability: f64,
    correction_multiplier: f64,
    trust_sensitivity: f64,
    family_multiplier: f64,
    adjacent_multiplier: f64,
) -> PyResult<f64> {
    let settings = Settings {
        maximum_contacts: 0,
        minimum_contact_minutes: 1,
        duration_scale,
        base_probability,
        correction_multiplier,
        trust_sensitivity,
        family_multiplier,
        adjacent_multiplier,
        ..Settings::default()
    };
    let basis = if family {
        Basis::Family
    } else if adjacent {
        Basis::Nearby
    } else {
        Basis::SameZone
    };
    let model = if hazard { Model::Hazard } else { Model::Sparse };
    core::transmission_probability(&settings, model, trust, basis, contact_minutes, correction).or_py()
}

// ---- A detached namespace, for proposals.rs ------------------------------------------------------

/// The mirror of the namespace while the world holds it detached, its row alone holding it: the
/// rules that read or write its tree read and write the row then. None while it has a tree, or
/// is absent.
pub(crate) fn detached_mirror<'py>(world: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, NamespaceMirror>>> {
    let table = world.getattr(intern!(world.py(), "model_state"))?;
    let Some(table) = crate::core::tracked::as_readable_dict(&table) else {
        return Ok(None);
    };
    let Some(held) = table.get_item(NAME)? else {
        return Ok(None);
    };
    if !crate::core::tracked::is_detached(&held) {
        return Ok(None);
    }
    let mirror = mirror_of(world, NAME)?;
    if !mirror.try_borrow()?.holds_state(&held) {
        return Err(Unsupported::new_err("the world holds another document than the row's"));
    }
    Ok(Some(mirror))
}

/// `state["query_pressure"].get(agent_id)` of a detached namespace's row.
pub(crate) fn row_query_pressure(mirror: &Bound<'_, NamespaceMirror>, agent_id: &str) -> PyResult<Option<f64>> {
    NamespaceMirror::with_row::<InformationRow, _>(mirror, |row| Ok(row.query_pressure.get(agent_id).copied()))
}

/// `InformationSpreadPolicy.record_support_request` on a detached namespace's row: false, having
/// written nothing, where the row has no beliefs of the person, whose `KeyError` is the Python
/// rule's to raise on the tree.
pub(crate) fn record_support_request(mirror: &Bound<'_, NamespaceMirror>, agent_id: &str, request_id: &str, minute: i64) -> PyResult<bool> {
    NamespaceMirror::with_row_tree::<InformationRow, _>(mirror, |row, _| {
        let Some((agent_key, beliefs)) = row.beliefs.get_key_value(agent_id) else {
            return Ok(false);
        };
        let agent_key = agent_key.clone();
        // `sorted` of str: bytes of UTF-8 sort as code points do.
        let mut message_ids: Vec<Arc<str>> =
            beliefs.iter().filter(|(_, belief)| &*belief.belief == "heard").map(|(id, _)| id.clone()).collect();
        if message_ids.is_empty() {
            return Ok(true);
        }
        message_ids.sort_unstable();
        row.support_requests.push(crate::state::information::SupportRequestRow {
            request_id: Arc::from(request_id),
            agent_id: agent_key.clone(),
            minute,
            message_ids,
        });
        row.counters.support_requests += 1;
        match row.query_pressure.get_mut(&agent_key) {
            Some(pressure) => *pressure = 0.0,
            None => {
                row.query_pressure.insert(agent_key, 0.0);
            }
        }
        Ok(true)
    })
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(sample_contacts, m)?)?;
    m.add_function(wrap_pyfunction!(planned_contacts, m)?)?;
    m.add_function(wrap_pyfunction!(transmission_probability_of, m)?)?;
    Ok(())
}
