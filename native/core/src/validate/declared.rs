//! `validate_declared_states(world)` of `contracts/runtime.py`, with Python's errors and texts: the
//! declaration of `model_configs["state_contracts"]` (`declared_states`), and each declared
//! namespace by its installed validator. `validate` (`validate/mod.rs`) makes a `ValueError` a
//! `StateInvariantError(str(exc))`.
//!
//! The one installed contract is `population_presence_v1` (`contracts/presence.py`): its validator's
//! errors all become `declared model-state validation failed`, so only whether it passes counts.

use super::catalog::{catalog, StateContract};
use super::py::equal;
use super::selection::select;
use super::View;
use super::models;
use crate::error::{Error, Result};
use crate::value::V;

fn value_error(text: &str) -> Error {
    Error::Value(text.to_owned())
}

/// `declared_states(configs)`: the installed contracts the configs declare, in their order.
pub fn declared_states(configs: &V) -> Result<Vec<&'static StateContract>> {
    let catalog = catalog()?;
    let V::Map(map) = configs else {
        return Err(Error::unsupported("model_configs is not a dict"));
    };
    let declaration = match map.get("state_contracts") {
        None | Some(V::Null) => return Ok(Vec::new()),
        Some(V::Map(declaration)) => declaration,
        Some(_) => return Err(Error::unsupported("state_contracts is not a dict")),
    };
    let fields_ok = declaration.len() == 2 && declaration.contains_key("schema_version") && declaration.contains_key("namespaces");
    if !fields_ok || !matches!(declaration["schema_version"], V::Int(1)) {
        return Err(value_error("unsupported state_contracts declaration"));
    }
    let V::Map(namespaces) = &declaration["namespaces"] else {
        return Err(value_error("state_contracts namespaces must be an object"));
    };
    let mut out = Vec::new();
    for (namespace, requested) in namespaces {
        let Some(contract) = catalog.state_contracts.get(namespace) else {
            return Err(value_error("requested model-state contract is not installed"));
        };
        let signature = V::Map(
            [
                ("implementation_id", V::Str(contract.implementation_id.clone())),
                ("implementation_version", V::Str(contract.implementation_version.clone())),
                ("schema_version", V::Int(contract.schema_version)),
            ]
            .into_iter()
            .map(|(key, value)| (key.into(), value))
            .collect(),
        );
        let requested_ok = matches!(requested, V::Map(requested) if matches!(requested.get("schema_version"), Some(V::Int(_))))
            && equal(requested, &signature);
        if !requested_ok {
            return Err(value_error("model-state implementation/schema mismatch; migration required"));
        }
        if let Some(domain) = &contract.policy_domain {
            let selection = select(configs)?;
            let fits = selection
                .get(domain)
                .and_then(|model| catalog.models.get(&**model))
                .is_some_and(|model| model.state_schema_version == contract.schema_version);
            if !fits {
                return Err(value_error("selected policy cannot use this installed state schema"));
            }
        }
        out.push(contract);
    }
    Ok(out)
}

/// The check, as `validate` (`validate/mod.rs`) calls it in Python's order.
pub fn validate(view: &View<'_>) -> Result<()> {
    let declared = declared_states(&*view.configs()?)?;
    for contract in declared {
        let state = view.state(&contract.namespace)?;
        let schema_ok = match state.as_deref() {
            Some(V::Map(state)) => matches!(state.get("schema_version"), Some(V::Int(version)) if *version == contract.schema_version),
            _ => false,
        };
        if !schema_ok {
            return Err(value_error("missing or incompatible required model-state namespace"));
        }
        let passes = match &*contract.namespace {
            "population_presence_v1" => presence_passes(view, state.as_deref().unwrap_or(&V::Null))?,
            _ => return Err(Error::unsupported("a declared namespace has no validator in the core")),
        };
        if !passes {
            return Err(value_error("declared model-state validation failed"));
        }
    }
    Ok(())
}

/// Whether `validate_presence_state(world, raw)` passes.
fn presence_passes(view: &View<'_>, raw: &V) -> Result<bool> {
    let state = match models::presence_state(raw) {
        Ok(V::Map(state)) => state,
        Ok(_) => return Err(Error::unsupported("a model dump is not a dict")),
        Err(error) if super::is_value_error(&error) => return Ok(false),
        Err(error) => return Err(error),
    };
    let rows = view.rows()?;
    let minute_index = view.minute_index()?;
    let text = |value: Option<&V>| match value {
        Some(V::Str(text)) => Some(text.clone()),
        _ => None,
    };
    let Some(V::Map(people)) = state.get("people") else {
        return Err(Error::unsupported("a presence dump lacks its people"));
    };
    for (key, person) in people {
        let V::Map(person) = person else {
            return Err(Error::unsupported("a presence dump's person is not a dict"));
        };
        let since = matches!(person.get("since_minute"), Some(V::Int(since)) if *since <= minute_index);
        if text(person.get("agent_id")).as_deref() != Some(&**key) || !since {
            return Ok(false);
        }
        let admitted = rows.agents.contains_key(key);
        if (text(person.get("status")).as_deref() == Some("admitted")) != admitted {
            return Ok(false);
        }
        for name in ["transition_zone_id", "return_zone_id"] {
            if let Some(zone) = text(person.get(name)) {
                if !rows.zones.contains_key(&zone) {
                    return Ok(false);
                }
            }
        }
        if let Some(V::List(path)) = person.get("archive_path") {
            let mut held: Option<V> = None;
            for (index, part) in path.iter().enumerate() {
                let V::Str(part) = part else {
                    return Ok(false);
                };
                if index == 0 {
                    if !view.contains(part)? {
                        return Ok(false);
                    }
                    held = view.state(part)?.map(|state| state.clone());
                    continue;
                }
                match held {
                    Some(V::Map(map)) if map.contains_key(&**part) => held = map.get(&**part).cloned(),
                    _ => return Ok(false),
                }
            }
        }
    }
    if rows.agents.iter().any(|(key, _)| !people.contains_key(key)) {
        return Ok(false);
    }
    let Some(V::List(history)) = state.get("history") else {
        return Err(Error::unsupported("a presence dump lacks its history"));
    };
    let mut ids = std::collections::HashSet::new();
    let mut previous = -1;
    for receipt in history {
        let V::Map(receipt) = receipt else {
            return Err(Error::unsupported("a presence receipt is not a dict"));
        };
        let Some(V::Int(minute)) = receipt.get("minute") else {
            return Err(Error::unsupported("a presence receipt lacks its minute"));
        };
        let id = text(receipt.get("transition_id")).unwrap_or_default();
        let agent_known = text(receipt.get("agent_id")).is_some_and(|agent| people.contains_key(&agent));
        if ids.contains(&id) || !agent_known || *minute < previous || *minute > minute_index {
            return Ok(false);
        }
        for name in ["from_zone_id", "to_zone_id"] {
            if let Some(zone) = text(receipt.get(name)) {
                if !rows.zones.contains_key(&zone) {
                    return Ok(false);
                }
            }
        }
        ids.insert(id);
        previous = *minute;
    }
    Ok(true)
}
