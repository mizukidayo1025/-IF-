//! `agent_dynamics_v1`: each agent's decision memory and known facilities. The rules that write and
//! read it are `rules/agent_dynamics.rs`; `domain/agent_dynamics.py` keeps the validation of a saved
//! namespace (docs/rust-port/lanes/f-action-lifecycle.md).

use std::sync::Arc;

use pyo3::intern;
use pyo3::prelude::*;

use pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyList, PyString};

use crate::core::row::{attribute, exact_dict, load_row, namespace_is_absent, state_row, unsupported, IdMap};
use crate::core::world_core::{mirror_of, NamespaceMirror, RowType};
use crate::state::entities::{entities_of, EntityStore, FacilityRow, Table};

pub(crate) const NAME: &str = "agent_dynamics_v1";
pub(crate) const POLICY_V1: &str = "need-led-autonomy-memory-v1";
pub(crate) const POLICY_V2: &str = "need-led-autonomy-memory-v2";
pub(crate) const POLICY_V3: &str = "need-led-autonomy-memory-v3";
pub(crate) const ORIENTATION_SOURCE: &str = "ASM-INTAKE-FACILITY-ORIENTATION-V1";

state_row! {
    pub(crate) struct AgentMemory {
        known_facility_ids: Vec<Arc<str>>,
        last_action: Option<Arc<str>> => set_last_action,
        last_target_id: Option<Arc<str>> => set_last_target_id,
        last_decision_minute: Option<i64> => set_last_decision_minute,
        decision_due_minute: i64 => set_decision_due_minute,
        decision_reason: Option<Arc<str>> => set_decision_reason,
    }
}

state_row! {
    pub(crate) struct AgentDynamicsRow {
        schema_version: i64,
        policy_version: Arc<str>,
        agents: IdMap<AgentMemory>,
    }
}

impl AgentMemory {
    /// The per-record rules of `validate_agent_dynamics_state`, for a record already of its types.
    fn is_valid(&self, facilities: &Table<FacilityRow>) -> bool {
        self.decision_due_minute >= 0
            && self.last_decision_minute.is_none_or(|minute| minute >= 0)
            && self.known_facility_ids.iter().all(|id| facilities.get(id).is_some())
            && has_no_repeats(&self.known_facility_ids)
    }
}

/// Nearly always a sorted list, which needs no set.
fn has_no_repeats(ids: &[Arc<str>]) -> bool {
    if ids.windows(2).all(|pair| pair[0] < pair[1]) {
        return true;
    }
    let mut ordered: Vec<&Arc<str>> = ids.iter().collect();
    ordered.sort_unstable();
    ordered.windows(2).all(|pair| pair[0] != pair[1])
}

/// The texts of `canonical_agent_dynamics_config`'s ValueError, in its order of checks.
pub(crate) const UNSUPPORTED_FIELDS: &str = "agent_dynamics config has unsupported fields";
pub(crate) const UNSUPPORTED_VERSION: &str = "agent_dynamics config has an unsupported version";
pub(crate) const V1_SOURCE: &str = "agent_dynamics v1 has no orientation source";
pub(crate) const MUST_CITE: &str = "agent_dynamics v2/v3 must cite the intake orientation assumption";

/// `value == 1` of Python for a config's schema, or Unsupported for a type whose `==` this can't
/// tell (a subclass, any other object).
fn equals_one(value: &Bound<'_, PyAny>) -> PyResult<bool> {
    if value.is_exact_instance_of::<PyBool>() {
        return value.is_truthy();
    }
    if let Ok(number) = value.downcast_exact::<PyInt>() {
        return Ok(number.extract::<i64>().is_ok_and(|number| number == 1));
    }
    if let Ok(number) = value.downcast_exact::<PyFloat>() {
        return Ok(number.value() == 1.0);
    }
    if value.is_none()
        || value.is_exact_instance_of::<PyString>()
        || value.is_exact_instance_of::<PyList>()
        || value.is_exact_instance_of::<PyDict>()
    {
        return Ok(false);
    }
    unsupported("a schema of another type")
}

/// `canonical_agent_dynamics_config(raw)["policy_version"]`, or the text of the ValueError it
/// raises. Unsupported for what Python would decide by a subclass's `==`, or raise TypeError for (an
/// unhashable policy).
pub(crate) fn canonical_policy_or_text(raw: Option<&Bound<'_, PyAny>>) -> PyResult<Result<&'static str, &'static str>> {
    let Some(raw) = raw.filter(|raw| !raw.is_none()) else {
        return Ok(Ok(POLICY_V1));
    };
    let py = raw.py();
    if !raw.is_instance_of::<PyDict>() {
        return Ok(Err(UNSUPPORTED_FIELDS));
    }
    let raw = exact_dict(raw)?;
    for (key, _) in raw.iter() {
        match key.downcast_exact::<PyString>() {
            Ok(key) => {
                if !matches!(key.to_str()?, "schema_version" | "policy_version" | "source_id") {
                    return Ok(Err(UNSUPPORTED_FIELDS));
                }
            }
            Err(_) if key.is_instance_of::<PyString>() => return unsupported("a key is a str of another class"),
            Err(_) => return Ok(Err(UNSUPPORTED_FIELDS)),
        }
    }
    if let Some(schema) = raw.get_item(intern!(py, "schema_version"))? {
        if !equals_one(&schema)? {
            return Ok(Err(UNSUPPORTED_VERSION));
        }
    }
    let policy = match raw.get_item(intern!(py, "policy_version"))? {
        None => POLICY_V1,
        Some(version) => match version.downcast_exact::<PyString>() {
            Ok(version) => match version.to_str()? {
                POLICY_V1 => POLICY_V1,
                POLICY_V2 => POLICY_V2,
                POLICY_V3 => POLICY_V3,
                _ => return Ok(Err(UNSUPPORTED_VERSION)),
            },
            Err(_) if version.is_instance_of::<PyString>() => return unsupported("a policy is a str of another class"),
            // `in` a set hashes it: an int, a float, None or a bool is not one of the three.
            Err(_) if version.hash().is_ok() => return Ok(Err(UNSUPPORTED_VERSION)),
            Err(_) => return unsupported("an unhashable policy"),
        },
    };
    let source = raw.get_item(intern!(py, "source_id"))?;
    if policy == POLICY_V1 {
        return Ok(if source.is_some() { Err(V1_SOURCE) } else { Ok(POLICY_V1) });
    }
    match source {
        None => Ok(Ok(policy)),
        Some(source) => match source.downcast_exact::<PyString>() {
            Ok(source) if source.to_str()? == ORIENTATION_SOURCE => Ok(Ok(policy)),
            Ok(_) => Ok(Err(MUST_CITE)),
            Err(_) if source.is_instance_of::<PyString>() => unsupported("a source is a str of another class"),
            Err(_) => Ok(Err(MUST_CITE)),
        },
    }
}

/// `canonical_agent_dynamics_config(raw)["policy_version"]` for a rule that reads it: Unsupported
/// where the Python rule raises. A Scenario's config was checked, with its text, when it was loaded.
pub(crate) fn canonical_policy(raw: Option<&Bound<'_, PyAny>>) -> PyResult<&'static str> {
    match canonical_policy_or_text(raw)? {
        Ok(policy) => Ok(policy),
        Err(_) => unsupported("a config the Python rule refuses"),
    }
}

/// The policy `world.model_configs` selects.
pub(crate) fn policy_of(world: &Bound<'_, PyAny>) -> PyResult<&'static str> {
    let py = world.py();
    let configs = attribute(world, intern!(py, "model_configs"))?;
    canonical_policy(exact_dict(&configs)?.get_item(intern!(py, "agent_dynamics"))?.as_ref())
}

/// `validate_agent_dynamics_state`: accepts, or leaves it to the Python rule and its error text.
#[pyfunction]
fn validate(world: &Bound<'_, PyAny>) -> PyResult<bool> {
    if namespace_is_absent(world, intern!(world.py(), NAME))? {
        return Ok(true);
    }
    let policy = policy_of(world)?;
    let mirror = mirror_of(world, NAME)?;
    let store = entities_of(world)?;
    NamespaceMirror::with_row::<AgentDynamicsRow, _>(&mirror, |row| {
        if row.schema_version != 1 || &*row.policy_version != policy {
            return Ok(false);
        }
        EntityStore::with_rows(&store, |rows| {
            Ok(row.agents.len() == rows.agents.len()
                && row
                    .agents
                    .iter()
                    .all(|(id, record)| rows.agents.get(id).is_some() && record.is_valid(&rows.facilities)))
        })
    })
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    m.add("ROW", Py::new(py, RowType::new("agent_dynamics", load_row::<AgentDynamicsRow>))?)?;
    m.add_function(wrap_pyfunction!(validate, m)?)?;
    Ok(())
}
