//! `validate_agent_dynamics_state(world)` of `domain/agent_dynamics.py`, with Python's errors and
//! texts (lane D of #250, #349). Raw errors.

use super::extension_head::py::{self, eq_int, eq_str, get, keys_are, Held};
use super::View;
use crate::config::agent_dynamics::canonical_policy;
use crate::error::{Error, Result};
use crate::state::agent_dynamics::NAME;
use crate::text::py_repr;
use crate::value::V;

const SCHEMA: &str = "agent_dynamics_v1 has an invalid exact schema";
const RECORD_FIELDS: [&str; 6] =
    ["known_facility_ids", "last_action", "last_target_id", "last_decision_minute", "decision_due_minute", "decision_reason"];

/// `canonical_agent_dynamics_config(raw)["policy_version"]`, a kernel of Python's
/// (`kernel_only("agent_dynamics", "canonical_config")`): its ValueError, or its refusal as the
/// `KernelRefusal` Python raises.
pub fn canonical_agent_dynamics_policy(raw: Option<&V>) -> Result<&'static str> {
    match canonical_policy(raw) {
        Ok(Ok(policy)) => Ok(policy),
        Ok(Err(text)) => Err(Error::Value(text.to_owned())),
        Err(fault) => Err(Error::Refused { label: "agent_dynamics.canonical_config".to_owned(), reason: fault.text.to_string().into() }),
    }
}

/// The check, as `validate` (`validate/mod.rs`) calls it in Python's order.
pub fn validate(view: &View<'_>) -> Result<()> {
    let Some(state) = view.state(NAME)? else {
        return Ok(());
    };
    let schema = || Err(Error::Value(SCHEMA.to_owned()));
    let state = match &*state {
        V::Null => return Ok(()),
        V::Map(state) => state,
        _ => return schema(),
    };
    if !keys_are(state, &["schema_version", "policy_version", "agents"]) || !eq_int(state.get("schema_version"), 1) {
        return schema();
    }
    let policy = canonical_agent_dynamics_policy(view.config("agent_dynamics")?.as_ref())?;
    if !eq_str(state.get("policy_version"), policy) {
        return schema();
    }
    let V::Map(agents) = get(state, "agents") else {
        return schema();
    };
    let rows = view.rows()?;
    if agents.len() != rows.agents.len() || !agents.keys().all(|id| rows.agents.contains_key(id)) {
        return schema();
    }
    for (agent_id, record) in agents {
        let invalid = || Err(Error::Value(format!("agent_dynamics_v1 record for {} is invalid", py_repr(agent_id))));
        let V::Map(record) = record else {
            return invalid();
        };
        if !keys_are(record, &RECORD_FIELDS) {
            return invalid();
        }
        let V::List(known) = get(record, "known_facility_ids") else {
            return invalid();
        };
        if known.len() != py::distinct(known, Held::Tracked)? {
            return invalid();
        }
        if known.iter().any(|id| !matches!(id, V::Str(id) if rows.facilities.contains_key(id))) {
            return invalid();
        }
        let due = get(record, "decision_due_minute");
        if !py::is_int(due) || py::negative(due) {
            return invalid();
        }
        let last = get(record, "last_decision_minute");
        if !matches!(last, V::Null) && (!py::is_exact_int(last) || py::negative(last)) {
            return Err(Error::Value(format!("agent_dynamics_v1 decision minute for {} is invalid", py_repr(agent_id))));
        }
    }
    Ok(())
}
