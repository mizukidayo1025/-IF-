//! The sections of `agent_dynamics_v1`: the policy `canonical_agent_dynamics_config` gives, the
//! facility awareness a new agent starts with, and whether knowledge of facilities is limited at all
//! (lane 7-5b of Issue #152).

use std::sync::Arc;

use super::read::{int, map, text, Read};
use super::section::Fault;
use crate::events::is_stable_id;
use crate::state::agent_dynamics::{ORIENTATION_SOURCE, POLICY_V1, POLICY_V2, POLICY_V3};
use crate::value::V;

/// The texts of `canonical_agent_dynamics_config`'s ValueError, in its order of checks.
pub const UNSUPPORTED_FIELDS: &str = "agent_dynamics config has unsupported fields";
pub const UNSUPPORTED_VERSION: &str = "agent_dynamics config has an unsupported version";
pub const V1_SOURCE: &str = "agent_dynamics v1 has no orientation source";
pub const MUST_CITE: &str = "agent_dynamics v2/v3 must cite the intake orientation assumption";

pub const AWARENESS_SOURCE: &str = "ASM-FACILITY-ENTRY-ORIENTATION-V1";
pub const MAX_INITIAL_IDS: usize = 1000;
pub const NOT_A_LIST: &str = "a value is not a plain list";

/// `canonical_agent_dynamics_config(raw)["policy_version"]`, or the text of the ValueError it
/// raises, for a raw config that is JSON. None is a config that is absent or None.
pub fn canonical_policy(raw: Option<&V>) -> Read<Result<&'static str, &'static str>> {
    let Some(raw) = raw.filter(|raw| !matches!(raw, V::Null)) else {
        return Ok(Ok(POLICY_V1));
    };
    let V::Map(raw) = raw else {
        return Ok(Err(UNSUPPORTED_FIELDS));
    };
    if raw.keys().any(|key| !matches!(&**key, "schema_version" | "policy_version" | "source_id")) {
        return Ok(Err(UNSUPPORTED_FIELDS));
    }
    if let Some(schema) = raw.get("schema_version") {
        // `value == 1` of Python.
        let is_one = match schema {
            V::Bool(flag) => *flag,
            V::Int(number) => *number == 1,
            V::Float(number) => *number == 1.0,
            _ => false,
        };
        if !is_one {
            return Ok(Err(UNSUPPORTED_VERSION));
        }
    }
    let policy = match raw.get("policy_version") {
        None => POLICY_V1,
        Some(V::Str(version)) => match &**version {
            POLICY_V1 => POLICY_V1,
            POLICY_V2 => POLICY_V2,
            POLICY_V3 => POLICY_V3,
            _ => return Ok(Err(UNSUPPORTED_VERSION)),
        },
        // `in` a set hashes it: an int, a float, None or a bool is not one of the three.
        Some(V::List(_) | V::Map(_)) => return Err(Fault::refused("an unhashable policy")),
        Some(_) => return Ok(Err(UNSUPPORTED_VERSION)),
    };
    let source = raw.get("source_id");
    if policy == POLICY_V1 {
        return Ok(if source.is_some() { Err(V1_SOURCE) } else { Ok(POLICY_V1) });
    }
    Ok(match source {
        None => Ok(policy),
        Some(V::Str(source)) if &**source == ORIENTATION_SOURCE => Ok(policy),
        Some(_) => Err(MUST_CITE),
    })
}

/// The policy `model_configs` selects, for a rule that reads it: refused where the Python rule
/// raises. A Scenario's config was checked, with its text, when it was loaded.
pub fn policy(configs: &V) -> Read<&'static str> {
    match canonical_policy(map(configs)?.get("agent_dynamics"))? {
        Ok(policy) => Ok(policy),
        Err(_) => Err(Fault::refused("a config the Python rule refuses")),
    }
}

/// `FacilityAwarenessConfig` of a plain document: refused for what the model refuses or coerces.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Awareness {
    /// Sorted, each once.
    pub initial: Vec<Arc<str>>,
    pub discover: bool,
}

/// `model_validate` of `model_configs["facility_awareness"]`, or None where it is absent or None.
pub fn awareness(configs: &V) -> Read<Option<Awareness>> {
    let document = match map(configs)?.get("facility_awareness") {
        None | Some(V::Null) => return Ok(None),
        Some(document) => map(document)?,
    };
    let mut awareness = Awareness { initial: Vec::new(), discover: true };
    for (key, value) in document {
        match &**key {
            "schema_version" => {
                if int(value)? != 1 {
                    return Err(Fault::refused("another awareness schema"));
                }
            }
            "initial_known_facility_ids" => {
                let V::List(ids) = value else {
                    return Err(Fault::refused(NOT_A_LIST));
                };
                if ids.len() > MAX_INITIAL_IDS {
                    return Err(Fault::refused("too many initial facilities"));
                }
                for id in ids {
                    let id = text(id)?;
                    if !is_stable_id(id) {
                        return Err(Fault::refused("an initial facility is not a stable ID"));
                    }
                    awareness.initial.push(id.clone());
                }
                awareness.initial.sort_unstable();
                if awareness.initial.windows(2).any(|pair| pair[0] == pair[1]) {
                    return Err(Fault::refused("an initial facility repeats"));
                }
            }
            "discover_co_located_facilities_on_arrival" => match value {
                V::Bool(flag) => awareness.discover = *flag,
                _ => return Err(Fault::refused("discovery is not a bool")),
            },
            "source_type" => {
                if &**text(value)? != "assumption" {
                    return Err(Fault::refused("another awareness source type"));
                }
            }
            "source_id" => {
                if &**text(value)? != AWARENESS_SOURCE {
                    return Err(Fault::refused("another awareness source"));
                }
            }
            _ => return Err(Fault::refused("the awareness config has other fields")),
        }
    }
    Ok(Some(awareness))
}

/// `agent_knows_facility`'s first check: without `facility_awareness` and `agent_dynamics`, every
/// agent knows every facility.
pub fn knowledge_is_limited(configs: &V) -> Read<bool> {
    let configs = map(configs)?;
    Ok(configs.contains_key("facility_awareness") || configs.contains_key("agent_dynamics"))
}
