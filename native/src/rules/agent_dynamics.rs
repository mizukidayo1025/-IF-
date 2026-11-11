//! `domain/agent_dynamics.py`: the glue of the kernels of `sheltersim_core::rules::agent_dynamics`
//! (lane P8 a2). Each entry reads what Python holds (the arguments, the config sections it reads
//! through `core/configs.rs`), runs the core's kernel on the world (`with_world`), and makes the
//! Python value of what it returns.

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyString};

use sheltersim_core::config::agent_dynamics::{awareness, knowledge_is_limited, Awareness};
use sheltersim_core::rules::agent_dynamics as rules;
use sheltersim_core::rules::kit::Kept;

use crate::core::configs;
use crate::core::errors::OrPy;
use crate::core::row::{attribute, exact_int, exact_str, str_value, PyField};
use crate::core::world_core::with_world;
use crate::state::agent_dynamics::{canonical_policy_or_text, policy_of, ORIENTATION_SOURCE, POLICY_V1};
use sheltersim_core::config::agent_dynamics::policy as config_policy;

pub(crate) use sheltersim_core::rules::agent_dynamics::Decision;

/// The policy `world.model_configs` selects, or its fault, for the kernel to raise where it reads it.
fn policy_kept(world: &Bound<'_, PyAny>) -> PyResult<Kept<&'static str>> {
    configs::kept_section(world, |configs| &configs.agent_dynamics_policy, config_policy)
}

/// `model_validate` of `model_configs["facility_awareness"]`, or None where it is absent or None,
/// kept for the configs' stamp (`core/configs.rs`). Refused for what the model refuses or coerces.
pub(crate) fn awareness_of(world: &Bound<'_, PyAny>) -> PyResult<std::sync::Arc<Option<Awareness>>> {
    configs::kept_value(world, |configs| &configs.facility_awareness, awareness)
}

/// `model_configs["facility_awareness"]`, or its fault.
fn awareness_kept(world: &Bound<'_, PyAny>) -> PyResult<Kept<Option<Awareness>>> {
    configs::kept_section(world, |configs| &configs.facility_awareness, awareness)
}

/// `record_proposal`, for a Rust caller after `action_lifecycle::commit_action`, as
/// `_apply_proposal` does.
pub(crate) fn remember_decision(world: &Bound<'_, PyAny>, decision: &Decision<'_>) -> PyResult<()> {
    with_world(world, |cell| rules::remember_decision(cell, decision))?.or_py()
}

/// A None or an exact str.
fn optional_str<'a>(value: &'a Bound<'_, PyAny>) -> PyResult<Option<&'a str>> {
    if value.is_none() {
        Ok(None)
    } else {
        exact_str(value).map(Some)
    }
}

/// `record_proposal`.
#[pyfunction]
#[pyo3(signature = (world, proposal, *, target_id))]
pub(crate) fn record_proposal(world: &Bound<'_, PyAny>, proposal: &Bound<'_, PyAny>, target_id: &Bound<'_, PyAny>) -> PyResult<()> {
    let py = world.py();
    let agent_id = attribute(proposal, intern!(py, "agent_id"))?;
    let action = attribute(&attribute(proposal, intern!(py, "action"))?, intern!(py, "value"))?;
    let trigger = attribute(proposal, intern!(py, "trigger"))?;
    remember_decision(
        world,
        &Decision {
            agent_id: exact_str(&agent_id)?,
            action: exact_str(&action)?,
            target_id: optional_str(target_id)?,
            minute: exact_int(&attribute(proposal, intern!(py, "minute_index"))?)?,
            next_decision_at: exact_int(&attribute(proposal, intern!(py, "next_decision_at"))?)?,
            trigger: optional_str(&trigger)?,
        },
    )
}

/// `agent_knows_facility`.
#[pyfunction]
pub(crate) fn agent_knows_facility(
    world: &Bound<'_, PyAny>,
    agent_id: &Bound<'_, PyAny>,
    facility_id: &Bound<'_, PyAny>,
) -> PyResult<bool> {
    let limited = configs::kept_section(world, |configs| &configs.knowledge_is_limited, knowledge_is_limited)?;
    let (agent_id, facility_id) = (str_value(agent_id), str_value(facility_id));
    with_world(world, |cell| rules::knows_facility(cell, &limited, agent_id, facility_id))?.or_py()
}

// ---- What a new agent knows ---------------------------------------------------------------------

/// `create_agent_dynamics_state`.
#[pyfunction]
pub(crate) fn create_state<'py>(world: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let policy = policy_of(world)?;
    let awareness = awareness_of(world)?;
    let row = with_world(world, |cell| rules::create_state(cell, policy, awareness.as_ref().as_ref()))?.or_py()?;
    PyField::to_py(&row, world.py())
}

/// What `add_agent` refuses for a person about to be added, for a plan that adds one.
pub(crate) fn check_add_agent(world: &Bound<'_, PyAny>) -> PyResult<()> {
    let (policy, awareness) = (policy_kept(world)?, awareness_kept(world)?);
    with_world(world, |cell| rules::check_add_agent(cell, &policy, &awareness))?.or_py()
}

/// `add_agent`: one record.
#[pyfunction]
pub(crate) fn add_agent(world: &Bound<'_, PyAny>, agent_id: &Bound<'_, PyAny>) -> PyResult<()> {
    let (policy, awareness) = (policy_kept(world)?, awareness_kept(world)?);
    let agent_id = str_value(agent_id);
    with_world(world, |cell| rules::add_agent(cell, agent_id, &policy, &awareness))?.or_py()
}

// ---- What an agent learns -------------------------------------------------------------------------

/// `learn_facility`.
#[pyfunction]
fn learn_facility(world: &Bound<'_, PyAny>, agent_id: &Bound<'_, PyAny>, facility_id: &Bound<'_, PyAny>) -> PyResult<()> {
    let awareness = awareness_kept(world)?;
    let (agent_id, facility_id) = (str_value(agent_id), str_value(facility_id));
    with_world(world, |cell| rules::learn_facility(cell, agent_id, facility_id, &awareness))?.or_py()
}

/// The rule, for IDs a row holds.
pub(crate) fn learn_facility_of(world: &Bound<'_, PyAny>, agent_id: &str, facility_id: &str) -> PyResult<()> {
    let awareness = awareness_kept(world)?;
    with_world(world, |cell| rules::learn_facility(cell, Some(agent_id), Some(facility_id), &awareness))?.or_py()
}

/// `announce_installed_facility`.
#[pyfunction]
fn announce_installed_facility(world: &Bound<'_, PyAny>, facility_id: &Bound<'_, PyAny>) -> PyResult<()> {
    let policy = policy_kept(world)?;
    let facility_id = str_value(facility_id);
    with_world(world, |cell| rules::announce_installed_facility(cell, facility_id, &policy))?.or_py()
}

/// `canonical_agent_dynamics_config`: a new dict, or its ValueError in its words.
#[pyfunction]
fn canonical_config<'py>(raw: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyDict>> {
    let py = raw.py();
    let policy = canonical_policy_or_text(Some(raw))?.map_err(crate::core::errors::value_error)?;
    let config = PyDict::new(py);
    config.set_item(intern!(py, "schema_version"), 1_i64)?;
    config.set_item(intern!(py, "policy_version"), PyString::new(py, policy))?;
    if policy != POLICY_V1 {
        config.set_item(intern!(py, "source_id"), PyString::new(py, ORIENTATION_SOURCE))?;
    }
    Ok(config)
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(record_proposal, m)?)?;
    m.add_function(wrap_pyfunction!(agent_knows_facility, m)?)?;
    m.add_function(wrap_pyfunction!(create_state, m)?)?;
    m.add_function(wrap_pyfunction!(add_agent, m)?)?;
    m.add_function(wrap_pyfunction!(learn_facility, m)?)?;
    m.add_function(wrap_pyfunction!(announce_installed_facility, m)?)?;
    m.add_function(wrap_pyfunction!(canonical_config, m)?)?;
    Ok(())
}
