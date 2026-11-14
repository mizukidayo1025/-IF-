//! `selected_models(world.model_configs)` of `contracts/policies.py`, with Python's errors and texts:
//! the models the configs select, which `validate` reads (the psychology model) and the extensions'
//! signatures read. What it reads of the installed code is the catalog (`catalog.rs`).
//!
//! `proximity_config` is `canonical_proximity_contact_config` of `domain/social_network.py`, which
//! the selection and the signatures read.

use std::sync::Arc;

use indexmap::IndexMap;

use super::catalog::{catalog, Bounds};
use super::py::{equal, finite, number, unhashable};
use super::{repr, View};
use crate::config::Selection;
use crate::contracts;
use crate::error::{Error, Result};
use crate::host::Contract;
use crate::value::V;

const BEHAVIOR_KEYS: [&str; 9] = [
    "mode",
    "source_type",
    "source_id",
    "policy",
    "temperature",
    "decision_interval_minutes",
    "decision_interval_minutes_by_agent",
    "temperature_by_agent",
    "sleep_debt_utility_weight",
];
const DEFAULT_BEHAVIOR: &str = "utility-softmax-v1";
const ENVIRONMENT_DOMAINS: [&str; 4] = ["zone_environment", "zone_rest", "zone_hygiene", "zone_privacy"];
const FORMULA_DOMAINS: [&str; 8] =
    ["body_dynamics", "staff_operations", "staff_fatigue", "zone_environment", "zone_rest", "zone_hygiene", "zone_privacy", "care_outcome"];
const PROXIMITY_POLICY: &str = "distance-sampled-proximity-v1";

fn value_error(text: impl Into<String>) -> Error {
    Error::Value(text.into())
}

fn bound(value: &V) -> Result<f64> {
    number(value)?.ok_or_else(|| Error::unsupported("a catalog bound is not a number"))
}

/// Whether a value is outside a parameter's bounds; `exclusive` reads `lower_exclusive`.
fn outside(value: f64, bounds: &Bounds, exclusive: bool) -> Result<bool> {
    if let Some(lower) = &bounds.lower {
        let lower = bound(lower)?;
        if value < lower || (exclusive && bounds.lower_exclusive && value == lower) {
            return Ok(true);
        }
    }
    match &bounds.upper {
        Some(upper) => Ok(value > bound(upper)?),
        None => Ok(false),
    }
}

/// `canonical_proximity_contact_config(raw)`: whether the config enables the contact sampler
/// (false for no config or a disabled one), or Python's `ValueError`.
pub fn proximity_config(raw: Option<&V>) -> Result<bool> {
    let raw = match raw {
        None | Some(V::Null) => return Ok(false),
        Some(V::Map(raw)) => raw,
        Some(_) => return Err(value_error("social_proximity config fields do not match")),
    };
    let fields = ["schema_version", "enabled", "policy_version", "sample_interval_minutes", "max_distance_m", "max_pairs_per_sample"];
    if raw.len() != fields.len() || fields.iter().any(|field| !raw.contains_key(*field)) {
        return Err(value_error("social_proximity config fields do not match"));
    }
    if !matches!(raw["schema_version"], V::Int(1))
        || !matches!(raw["enabled"], V::Bool(_))
        || !matches!(&raw["policy_version"], V::Str(policy) if &**policy == PROXIMITY_POLICY)
    {
        return Err(value_error("social_proximity config version is unsupported"));
    }
    let interval_ok = matches!(raw["sample_interval_minutes"], V::Int(interval) if (1..=60).contains(&interval));
    let maximum_ok = finite(&raw["max_distance_m"])?.is_some_and(|maximum| (0.25..=10.0).contains(&maximum));
    let pairs_ok = matches!(raw["max_pairs_per_sample"], V::Int(pairs) if (1..=10_000).contains(&pairs));
    if !interval_ok || !maximum_ok || !pairs_ok {
        return Err(value_error("social_proximity bounds are invalid"));
    }
    Ok(matches!(raw["enabled"], V::Bool(true)))
}

/// `selected_models(world.model_configs)`: the selection, or Python's error for the configs.
pub fn selected_models(view: &View<'_>) -> Result<Selection> {
    select(&*view.configs()?)
}

/// `_select_models(configs)`.
pub fn select(configs: &V) -> Result<Selection> {
    let catalog = catalog()?;
    let empty = IndexMap::new();
    let V::Map(configs) = configs else {
        return Err(value_error("model configs must map names to configuration objects"));
    };
    let mut maps: IndexMap<&str, &IndexMap<Arc<str>, V>> = IndexMap::new();
    for (name, value) in configs {
        let V::Map(map) = value else {
            return Err(value_error("model configs must map names to configuration objects"));
        };
        maps.insert(name, map);
    }
    let behavior = maps.get("behavior").copied().unwrap_or(&empty);
    if behavior.keys().any(|key| !BEHAVIOR_KEYS.contains(&&**key)) {
        return Err(value_error("unknown behavior coefficient; explicit registry support required"));
    }
    let interval_ok = match behavior.get("decision_interval_minutes") {
        None => true,
        Some(V::Int(interval)) => *interval > 0,
        Some(V::Big(text)) => !text.starts_with('-'),
        Some(_) => false,
    };
    if !interval_ok {
        return Err(value_error("behavior decision interval must be a positive integer"));
    }
    let temperature_ok = match behavior.get("temperature") {
        None => true,
        Some(other) => finite(other)?.is_some_and(|temperature| temperature > 0.0),
    };
    if !temperature_ok {
        return Err(value_error("behavior temperature must be finite and positive"));
    }
    let selection = maps.get("policy_registry").copied().unwrap_or(&empty);
    let known = |key: &str| key == "schema_version" || key == "suite_id" || catalog.has_domain(key);
    if selection.keys().any(|key| !known(key)) {
        return Err(value_error("unknown policy_registry setting"));
    }
    let schema = selection.get("schema_version");
    if !selection.is_empty() && !matches!(schema, Some(V::Int(1..=3))) {
        return Err(value_error("unsupported policy_registry schema"));
    }
    let policy = behavior.get("policy").cloned().unwrap_or_else(|| V::Str(Arc::from(DEFAULT_BEHAVIOR)));
    let mut suite_models = None;
    if let Some(suite_id) = selection.get("suite_id").filter(|suite| !matches!(suite, V::Null)) {
        if let Some(error) = unhashable(suite_id) {
            return Err(error);
        }
        suite_models = match suite_id {
            V::Str(suite) => catalog.suites.get(&**suite),
            _ => None,
        };
        let Some(members) = suite_models.filter(|_| matches!(schema, Some(V::Int(3)))) else {
            return Err(value_error("unsupported or incorrectly versioned model suite"));
        };
        let conflicting = members
            .iter()
            .any(|(domain, model)| selection.get(&**domain).is_some_and(|chosen| !equal(chosen, &V::Str(model.clone()))));
        if conflicting {
            return Err(value_error("model suite selectors cannot be mixed with per-domain overrides"));
        }
        let suite_behavior = members.iter().find(|(domain, _)| &**domain == "behavior").map(|(_, model)| V::Str(model.clone()));
        if !suite_behavior.is_some_and(|model| equal(&policy, &model)) {
            return Err(value_error("behavior policy does not match the selected model suite"));
        }
    }
    let mut models: IndexMap<Arc<str>, V> = IndexMap::new();
    for (domain, model) in catalog.default_models.iter().chain(suite_models.into_iter().flatten()) {
        models.insert(domain.clone(), V::Str(model.clone()));
    }
    models.insert(Arc::from("behavior"), policy.clone());
    let mut domains: Vec<Arc<str>> = models.keys().cloned().collect();
    domains.extend(selection.keys().filter(|key| &***key != "schema_version" && &***key != "suite_id").cloned());
    domains.sort();
    domains.dedup();
    for domain in &domains {
        if let Some(chosen) = selection.get(&**domain) {
            models.insert(domain.clone(), chosen.clone());
        }
        let model_id = &models[&**domain];
        let installed = match model_id {
            V::Str(model) => catalog.models.get(&**model).is_some_and(|definition| definition.domain == *domain),
            _ => false,
        };
        if !installed {
            return Err(value_error(format!("unsupported installed {domain} model: {}", repr(model_id))));
        }
    }
    // Every model is a str of the catalog now.
    let selected: IndexMap<Arc<str>, Arc<str>> = models
        .iter()
        .map(|(domain, model)| match model {
            V::Str(model) => Ok((domain.clone(), model.clone())),
            _ => Err(Error::unsupported("a selected model is not a str")),
        })
        .collect::<Result<_>>()?;
    let behavior_model = selected["behavior"].clone();
    if !equal(&V::Str(behavior_model.clone()), &policy) {
        return Err(value_error("behavior selector must agree with model_configs.behavior.policy"));
    }
    let parameters = &catalog.models[&*behavior_model].behavior_parameters;
    if let Some(value) = behavior.get("sleep_debt_utility_weight") {
        let Some(bounds) = parameters.get("behavior.sleep_debt_utility_weight") else {
            return Err(value_error("behavior.sleep_debt_utility_weight is not supported by the selected behavior model"));
        };
        let Some(number) = finite(value)? else {
            return Err(value_error("behavior.sleep_debt_utility_weight must be finite"));
        };
        if outside(number, bounds, false)? {
            return Err(value_error("behavior.sleep_debt_utility_weight is outside its Model Catalog bounds"));
        }
    }
    for (field, minutes) in [("decision_interval_minutes_by_agent", true), ("temperature_by_agent", false)] {
        let values = match behavior.get(field) {
            None => &empty,
            Some(V::Map(values)) if values.len() <= 100 => values,
            Some(_) => return Err(value_error(format!("behavior.{field} must be a bounded Agent ID map"))),
        };
        let metadata = parameters.get(format!("behavior.{field}.*").as_str());
        if !values.is_empty() && metadata.is_none() {
            return Err(value_error(format!("behavior.{field} is not supported by selected {behavior_model}")));
        }
        for (agent_id, value) in values {
            if agent_id.is_empty() || agent_id.chars().count() > 64 {
                return Err(value_error(format!("behavior.{field} contains an invalid Agent ID")));
            }
            let Some(number) = finite(value)? else {
                return Err(value_error(format!("behavior.{field} values must be finite numbers")));
            };
            if minutes && !(matches!(value, V::Int(_) | V::Big(_)) && number >= 1.0) {
                return Err(value_error("behavior decision intervals must be positive integer minutes"));
            }
            let Some(bounds) = metadata else {
                return Err(value_error(format!("behavior.{field} is not supported by selected {behavior_model}")));
            };
            if outside(number, bounds, true)? {
                return Err(value_error(format!("behavior.{field} value is outside its Model Catalog bounds")));
            }
        }
    }
    let life_state = selected.get("psychology").is_some_and(|model| catalog.is_life_state(model));
    let has = |name: &str| maps.contains_key(name);
    if selected.contains_key("affect") && !life_state {
        return Err(value_error("affect model requires the installed life-state psychology operator"));
    }
    if selected.contains_key("body_dynamics") && (!life_state || !has("body_dynamics")) {
        return Err(value_error("body_dynamics formula selection requires body_dynamics config and life-state psychology"));
    }
    if selected.contains_key("staff_operations") && !has("staff_operations") {
        return Err(value_error("staff_operations formula selection requires staff_operations config"));
    }
    if selected.contains_key("staff_fatigue") && !has("staff_dynamics") {
        return Err(value_error("staff_fatigue formula selection requires staff_dynamics config"));
    }
    if selected.contains_key("care_outcome") && !has("body_dynamics") {
        return Err(value_error("care outcome selection requires body_dynamics config"));
    }
    if ENVIRONMENT_DOMAINS.iter().any(|domain| selected.contains_key(*domain)) {
        let Some(environment) = configs.get("zone_environment") else {
            return Err(value_error("environment formula selection requires zone_environment config"));
        };
        let dumped = contracts::validate(&Contract::ZoneEnvironment, environment).map_err(super::models::tracked)?;
        let v2 = matches!(&dumped, V::Map(dumped) if matches!(dumped.get("policy_version"), Some(V::Str(policy)) if &**policy == "zone-environment-v2"));
        if !v2 {
            return Err(value_error("environment formula selection requires zone-environment-v2"));
        }
    }
    if FORMULA_DOMAINS.iter().any(|domain| selected.contains_key(*domain)) && !matches!(schema, Some(V::Int(2 | 3))) {
        return Err(value_error("replaceable formula factories require policy_registry schema_version 2 or 3"));
    }
    if selected.values().any(|model| !catalog.models[&**model].factory) {
        return Err(value_error("selected model metadata has no installed Policy factory"));
    }
    if let Some(proximity) = selected.get("social_proximity") {
        let enabled = proximity_config(configs.get("social_proximity"))?;
        if &**proximity == PROXIMITY_POLICY && !enabled {
            return Err(value_error("distance-sampled-proximity-v1 requires an enabled social_proximity config"));
        }
        if &**proximity == "disabled-proximity-v1" && enabled {
            return Err(value_error("disabled-proximity-v1 conflicts with an enabled social_proximity config"));
        }
    }
    for model in selected.values() {
        let required = match &catalog.models[&**model].required_state_contracts {
            V::Map(required) => required,
            _ => return Err(value_error("invalid installed model state requirements")),
        };
        if required.is_empty() {
            continue;
        }
        let declared = match maps.get("state_contracts").and_then(|contracts| contracts.get("namespaces")) {
            None => None,
            Some(V::Map(declared)) => Some(declared),
            Some(_) => return Err(Error::unsupported("state_contracts.namespaces is not a dict (AttributeError)")),
        };
        for (namespace, signature) in required {
            let actual = catalog.state_contracts.get(namespace).map(|contract| {
                V::Map(IndexMap::from([
                    (Arc::from("implementation_id"), V::Str(contract.implementation_id.clone())),
                    (Arc::from("implementation_version"), V::Str(contract.implementation_version.clone())),
                    (Arc::from("schema_version"), V::Int(contract.schema_version)),
                ]))
            });
            let declared = declared.and_then(|declared| declared.get(namespace));
            if !declared.is_some_and(|declared| equal(declared, signature)) || !actual.is_some_and(|actual| equal(&actual, signature)) {
                return Err(value_error("selected model requires an installed, explicitly versioned state contract; migration required"));
            }
        }
    }
    Ok(Selection::new(selected.into_iter().collect()))
}
