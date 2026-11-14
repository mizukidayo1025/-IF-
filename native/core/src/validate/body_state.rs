//! `validate_body_state(world, validate_event_history=...)` of `domain/body_state.py`, its history
//! included, and the `canonical_body_config` it calls, with Python's errors and texts (lane D of
//! #250, #349). Raw errors.
//!
//! Python skips the rules of the records and relationships where the row's kernel accepted them
//! (`_ROWS_VALID`); that kernel accepts only what the rules accept, so they run here every time.

use std::collections::HashSet;
use std::sync::Arc;

use indexmap::IndexMap;

use super::extension_head::py::{self, eq_int, eq_str, get, keys_are, Held, Json, Key};
use super::{selection, View};
use crate::error::{Error, Result};
use crate::text::py_repr;
use crate::value::V;

const NAME: &str = "body_state_v1";
const POLICY: &str = "life-state-feedback-v1";
const SOURCE: &str = "ASM-LIFE-STATE-FEEDBACK-V1";
const POPULATION: &str = "population_dynamics_v1";
const MAX_SOCIAL_HISTORY: usize = 20_000;
const MAX_SOCIAL_RELATIONSHIPS: usize = 100_000;
const FIELDS: [&str; 7] = ["schema_version", "policy_version", "source_id", "config", "agents", "relationships", "history"];
const RECORD_FIELDS: [&str; 7] =
    ["sleep_debt_minutes", "uncertainty", "reassurance", "discontent", "positive_affect", "negative_affect", "hygiene_need"];
const RELATIONSHIP_FIELDS: [&str; 6] = ["contact_count", "trust", "contact_minutes", "last_contact_minute", "last_interaction", "topic_ids"];
const HISTORY_FIELDS: [&str; 7] = ["event_id", "minute", "kind", "actor_ids", "topic_id", "causation_id", "deltas"];
const AFFECT_MODELS: [&str; 2] = ["linear-positive-negative-v1", "trust-weighted-positive-negative-v1"];

/// `_DEFAULTS`, in its order (only the names are read here).
const DEFAULTS: [&str; 38] = [
    "awake_sleep_debt_minutes_per_minute",
    "rest_sleep_debt_recovery_per_minute",
    "sleep_sleep_debt_recovery_per_minute",
    "sleep_debt_stress_rate",
    "uncertainty_drift_per_minute",
    "uncertainty_decay_per_minute",
    "discontent_stress_rate",
    "discontent_queue_rate",
    "discontent_relief_per_minute",
    "reassurance_decay_per_minute",
    "positive_affect_decay_per_minute",
    "negative_affect_stress_rate",
    "hygiene_growth_per_minute",
    "sanitation_hygiene_growth_per_minute",
    "sanitation_stress_rate",
    "affect_transfer_rate",
    "relationship_trust_gain",
    "relationship_trust_decay_per_day",
    "relationship_repeat_contact_bonus",
    "cooperation_positive_gain",
    "cooperation_negative_reduction",
    "cooperation_reassurance_gain",
    "cooperation_discontent_reduction",
    "social_action_affect_transfer",
    "complaint_reassurance_gain",
    "complaint_discontent_reduction",
    "hygiene_support_relief",
    "caregiver_assistance_stress_relief",
    "caregiver_assistance_reassurance_gain",
    "privacy_accommodation_discontent_relief",
    "mobility_assistance_stress_relief",
    "language_access_uncertainty_relief",
    "medical_assessment_uncertainty_relief",
    "medication_support_uncertainty_relief",
    "dependent_assistance_stress_relief",
    "dependent_assistance_reassurance_gain",
    "correction_uncertainty_reduction",
    "correction_reassurance_gain",
];

/// The per-Agent maps and the label of their errors, in `canonical_body_config`'s order.
const PER_AGENT: [(&str, &str); 14] = [
    ("sleep_debt_stress_rate_by_agent", "sleep debt rates"),
    ("negative_affect_stress_rate_by_agent", "negative-affect stress rates"),
    ("discontent_stress_rate_by_agent", "discontent stress rates"),
    ("complaint_reassurance_gain_by_agent", "reassurance gains"),
    ("hygiene_support_relief_by_agent", "hygiene support relief"),
    ("caregiver_assistance_stress_relief_by_agent", "caregiver stress relief"),
    ("caregiver_assistance_reassurance_gain_by_agent", "caregiver reassurance gains"),
    ("privacy_accommodation_discontent_relief_by_agent", "privacy discontent relief"),
    ("mobility_assistance_stress_relief_by_agent", "mobility stress relief"),
    ("language_access_uncertainty_relief_by_agent", "language-access uncertainty relief"),
    ("medical_assessment_uncertainty_relief_by_agent", "medical-support uncertainty relief"),
    ("medication_support_uncertainty_relief_by_agent", "medication-support uncertainty relief"),
    ("dependent_assistance_stress_relief_by_agent", "dependent-assistance stress relief"),
    ("dependent_assistance_reassurance_gain_by_agent", "dependent-assistance reassurance gains"),
];

/// The fields a saved config may lack (`optional_compatibility_fields`), besides `affect_policy`
/// where the Policy Registry selects no affect model.
const OPTIONAL: [&str; 13] = [
    "social_interaction_minutes",
    "relationship_distance_scale_m",
    "relationship_repeat_contact_bonus",
    "hygiene_support_relief",
    "caregiver_assistance_stress_relief",
    "caregiver_assistance_reassurance_gain",
    "privacy_accommodation_discontent_relief",
    "mobility_assistance_stress_relief",
    "language_access_uncertainty_relief",
    "medical_assessment_uncertainty_relief",
    "medication_support_uncertainty_relief",
    "dependent_assistance_stress_relief",
    "dependent_assistance_reassurance_gain",
];

type Map = IndexMap<Arc<str>, V>;

fn value_error<T>(text: &str) -> Result<T> {
    Err(Error::Value(text.to_owned()))
}

/// `re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._:-]{0,63}", text)`.
fn is_plain_id(text: &str) -> bool {
    let bytes = text.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && bytes[0].is_ascii_alphanumeric()
        && bytes[1..].iter().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

/// `canonical_body_config(raw)`: the config (the defaults' values are not read, only which names it
/// has), or its ValueError.
fn canonical_body_config(raw: &V) -> Result<Map> {
    let empty = Map::new();
    let raw = match raw {
        V::Null => &empty,
        V::Map(raw) => raw,
        _ => return value_error("body_dynamics config must be an object"),
    };
    let allowed = |key: &str| {
        ["schema_version", "policy_version", "source_id", "social_interaction_minutes", "affect_policy", "relationship_distance_scale_m"].contains(&key)
            || DEFAULTS.contains(&key)
            || PER_AGENT.iter().any(|(field, _)| *field == key)
    };
    if raw.keys().any(|key| !allowed(key)) {
        return value_error("body_dynamics config contains unsupported fields");
    }
    let mut config = Map::new();
    config.insert("schema_version".into(), V::Int(1));
    config.insert("policy_version".into(), V::Str(POLICY.into()));
    config.insert("source_id".into(), V::Str(SOURCE.into()));
    for name in DEFAULTS {
        // A default is within 0..1; its value is not read where the config has none.
        config.insert(name.into(), V::Float(0.0));
    }
    config.insert("social_interaction_minutes".into(), V::Int(5));
    config.insert("affect_policy".into(), V::Str(AFFECT_MODELS[0].into()));
    config.insert("relationship_distance_scale_m".into(), V::Float(2.0));
    for (key, value) in raw {
        config.insert(key.clone(), value.clone());
    }
    if !eq_int(config.get("schema_version"), 1) || !eq_str(config.get("policy_version"), POLICY) || !eq_str(config.get("source_id"), SOURCE) {
        return value_error("unsupported body_dynamics policy or source");
    }
    for name in DEFAULTS {
        if !py::finite_within(get(&config, name), 0.0, 1.0) {
            return Err(Error::Value(format!("body_dynamics {name} must be finite and within 0..1")));
        }
    }
    let minutes = get(&config, "social_interaction_minutes");
    if !py::is_exact_int(minutes) || !py::wide(minutes).is_some_and(|minutes| (1..=1440).contains(&minutes)) {
        return value_error("body_dynamics social_interaction_minutes must be within 1..1440");
    }
    let V::Str(affect) = get(&config, "affect_policy") else {
        return value_error("body_dynamics affect_policy must be an installed model ID");
    };
    if !AFFECT_MODELS.contains(&&**affect) {
        return value_error("body affect policy has no installed implementation");
    }
    if !py::finite_within(get(&config, "relationship_distance_scale_m"), 0.1, 100.0) {
        return value_error("body_dynamics relationship_distance_scale_m must be within 0.1..100 metres");
    }
    let rate = |name: &str| py::number(get(&config, name)).unwrap_or(0.0);
    if rate("awake_sleep_debt_minutes_per_minute") > 10.0 {
        return value_error("awake sleep debt rate cannot exceed 10 minutes per minute");
    }
    if rate("rest_sleep_debt_recovery_per_minute") > 10.0 || rate("sleep_sleep_debt_recovery_per_minute") > 10.0 {
        return value_error("sleep debt recovery cannot exceed 10 minutes per minute");
    }
    for (field, label) in PER_AGENT {
        let per_agent = get(&config, field);
        if matches!(per_agent, V::Null) {
            continue;
        }
        let V::Map(per_agent) = per_agent else {
            return Err(Error::Value(format!("body_dynamics per-Agent {label} must be a non-empty ID map")));
        };
        if per_agent.is_empty() {
            return Err(Error::Value(format!("body_dynamics per-Agent {label} must be a non-empty ID map")));
        }
        for (agent_id, rate) in per_agent {
            // `type(rate) not in (int, float)`: a bool is not one.
            if !is_plain_id(agent_id) || !py::finite_within(rate, 0.0, 1.0) {
                return Err(Error::Value(format!("body_dynamics per-Agent {label} are invalid")));
            }
        }
    }
    Ok(config)
}

/// `read_only_state(world, "population_dynamics_v1", {})`, as the history reads it: the IDs of
/// `world.agents` and of `initial_agent_ids` and `profile_traits`, with the departures' when
/// `departures` is true.
fn historical_ids(view: &View<'_>, departures: bool) -> Result<HashSet<Key>> {
    let rows = view.rows()?;
    let mut ids: HashSet<Key> = rows.agents.iter().map(|(id, _)| Key::Str(id.clone())).collect();
    drop(rows);
    let population = if view.contains(POPULATION)? { view.state(POPULATION)? } else { None };
    let Some(population) = population else {
        return Ok(ids);
    };
    let V::Map(population) = &*population else {
        return Ok(ids);
    };
    let empty_list = V::List(Vec::new());
    let empty_map = V::Map(Map::new());
    py::update(&mut ids, population.get("initial_agent_ids").unwrap_or(&empty_list), Held::Plain)?;
    py::update(&mut ids, population.get("profile_traits").unwrap_or(&empty_map), Held::Plain)?;
    if departures {
        for item in py::iterate(population.get("departures").unwrap_or(&empty_list))? {
            if let V::Map(item) = &item {
                if let Some(V::Str(id)) = item.get("agent_id") {
                    ids.insert(Key::Str(id.clone()));
                }
            }
        }
    }
    Ok(ids)
}

/// `validate_body_config(world, state)`: the people the per-person parameters name.
fn validate_body_config(view: &View<'_>, state: &Map) -> Result<HashSet<Key>> {
    let saved = get(state, "config");
    let config = canonical_body_config(saved)?;
    let mut configured: HashSet<Key> = HashSet::new();
    for (field, _) in PER_AGENT {
        if let Some(per_agent) = config.get(field) {
            py::update(&mut configured, per_agent, Held::Plain)?;
        }
    }
    let rows = view.rows()?;
    let past_residents = configured.iter().any(|id| !matches!(id, Key::Str(id) if rows.agents.contains_key(id)));
    drop(rows);
    if past_residents {
        let historical = historical_ids(view, false)?;
        if configured.iter().any(|id| !historical.contains(id)) {
            return value_error("body_state_v1 per-Agent parameter references an unknown resident");
        }
    }
    let selected = selection::selected_models(view)?.get("affect").cloned();
    if let Some(selected) = &selected {
        if !eq_str(config.get("affect_policy"), selected) {
            return value_error("body_state_v1 affect model does not match the shared Policy Registry");
        }
    }
    // The saved config is the canonical one, which keeps every saved key and value, less some
    // optional fields older versions did not write.
    let fits = match saved {
        V::Map(saved) => config.keys().all(|key| saved.contains_key(key) || OPTIONAL.contains(&&**key) || (selected.is_none() && &**key == "affect_policy")),
        _ => false,
    };
    if !fits {
        return value_error("body_state_v1 config is not canonical");
    }
    Ok(configured)
}

/// `body_config_ids(world)` of `core/tick_validation.py`: `sorted(validate_body_config(world,
/// {"config": config}))` of the saved config, the people its per-person parameters name, or what
/// `validate_body_config` raises. For the end-of-tick check (`rules::tick_checks`), which then
/// reads whether each is here or in the population state's history. A name that is not a str
/// (Python's `sorted` would compare it with the str ones) is a refusal.
pub(crate) fn body_config_ids(view: &View<'_>, config: &V) -> Result<Vec<Arc<str>>> {
    let mut state = Map::new();
    state.insert(Arc::from("config"), config.clone());
    let configured = validate_body_config(view, &state)?;
    if configured.iter().any(|id| !matches!(id, Key::Str(_))) {
        return Err(Error::unsupported("a per-person parameter names a value that is not a str"));
    }
    let mut named: Vec<Arc<str>> = configured.into_iter().filter_map(|id| if let Key::Str(id) = id { Some(id) } else { None }).collect();
    named.sort_unstable();
    Ok(named)
}

/// `_validate_body_rows(world, state)`.
fn validate_rows(view: &View<'_>, state: &Map) -> Result<()> {
    let V::Map(agents) = get(state, "agents") else {
        return Ok(());
    };
    for (agent_id, record) in agents {
        let record = match record {
            V::Map(record) if keys_are(record, &RECORD_FIELDS) => record,
            _ => return Err(Error::Value(format!("body_state_v1 record for {} is invalid", py_repr(agent_id)))),
        };
        for (name, value) in record {
            let upper = if &**name == "sleep_debt_minutes" { 1440.0 } else { 1.0 };
            if !py::finite_within(value, 0.0, upper) {
                return Err(Error::Value(format!("body_state_v1 {name} for {} is invalid", py_repr(agent_id))));
            }
        }
    }
    let V::Map(relationships) = get(state, "relationships") else {
        return Ok(());
    };
    let rows = view.rows()?;
    let is_agent = |text: &str| rows.agents.contains_key(text);
    for (key, record) in relationships {
        let Some(parsed) = py::loads(key)? else {
            return value_error("body_state_v1 relationship key is invalid");
        };
        let fits = || -> Result<bool> {
            let Json::List(pair) = &parsed else {
                return Ok(false);
            };
            let [first, second] = pair.as_slice() else {
                return Ok(false);
            };
            if !first.key_in(is_agent)? || !second.key_in(is_agent)? {
                return Ok(false);
            }
            let (Json::Str(first), Json::Str(second)) = (first, second) else {
                return Ok(false);
            };
            if first == second || py::dumps_texts(&[first, second]) != **key {
                return Ok(false);
            }
            let V::Map(record) = record else {
                return Ok(false);
            };
            let count = |name: &str| py::is_exact_int(get(record, name)) && !py::negative(get(record, name));
            Ok(keys_are(record, &RELATIONSHIP_FIELDS)
                && count("contact_count")
                && count("contact_minutes")
                && matches!(get(record, "topic_ids"), V::List(topics) if topics.len() <= 32
                    && topics.iter().all(|topic| matches!(topic, V::Str(topic) if topic.chars().count() <= 64))))
        };
        if !fits()? {
            return value_error("body_state_v1 relationship record is invalid");
        }
        let V::Map(record) = record else {
            return value_error("body_state_v1 relationship record is invalid");
        };
        let trust = get(record, "trust");
        if !matches!(trust, V::Null) && !(py::is_real(trust) && py::number(trust).is_some_and(|trust| (0.0..=1.0).contains(&trust))) {
            return value_error("body_state_v1 trust must be within 0..1");
        }
        let minute = get(record, "last_contact_minute");
        if !matches!(minute, V::Null) && (!py::is_exact_int(minute) || py::negative(minute)) {
            return value_error("body_state_v1 contact minute is invalid");
        }
    }
    Ok(())
}

/// `records_since_previous_tick(history, minute_index)`: the items appended in the last tick, and
/// the one before them.
pub fn since_previous_tick(history: &[V], minute_index: i64) -> &[V] {
    let mut start = history.len();
    while start > 0 {
        let minute = match &history[start - 1] {
            V::Map(item) => item.get("minute"),
            _ => None,
        };
        if matches!(minute, Some(V::Int(minute)) if i128::from(*minute) < i128::from(minute_index) - 1)
            || matches!(minute, Some(V::Big(text)) if text.starts_with('-'))
        {
            break;
        }
        start -= 1;
    }
    &history[start.saturating_sub(1)..]
}

/// The check, as `validate` (`validate/mod.rs`) calls it in Python's order.
pub fn validate(view: &View<'_>) -> Result<()> {
    let Some(state) = view.state(NAME)? else {
        return Ok(());
    };
    let schema = || value_error("body_state_v1 has an invalid exact schema");
    let state = match &*state {
        V::Null => return Ok(()),
        V::Map(state) => state,
        _ => return schema(),
    };
    if !keys_are(state, &FIELDS)
        || !eq_int(state.get("schema_version"), 1)
        || !eq_str(state.get("policy_version"), POLICY)
        || !eq_str(state.get("source_id"), SOURCE)
    {
        return schema();
    }
    {
        let rows = view.rows()?;
        let V::Map(agents) = get(state, "agents") else {
            return schema();
        };
        if agents.len() != rows.agents.len() || !agents.keys().all(|id| rows.agents.contains_key(id)) {
            return schema();
        }
    }
    if !matches!(get(state, "relationships"), V::Map(relationships) if relationships.len() <= MAX_SOCIAL_RELATIONSHIPS)
        || !matches!(get(state, "history"), V::List(history) if history.len() <= MAX_SOCIAL_HISTORY)
    {
        return schema();
    }
    validate_body_config(view, state)?;
    validate_rows(view, state)?;
    let historical = historical_ids(view, true)?;
    let V::List(history) = get(state, "history") else {
        return schema();
    };
    let history = if view.history { history.as_slice() } else { since_previous_tick(history, view.minute_index()?) };
    for item in history {
        let fits = || -> Result<bool> {
            let V::Map(item) = item else {
                return Ok(false);
            };
            if !keys_are(item, &HISTORY_FIELDS)
                || !matches!(get(item, "event_id"), V::Str(_))
                || !py::is_exact_int(get(item, "minute"))
                || py::negative(get(item, "minute"))
                || !matches!(get(item, "kind"), V::Str(_))
            {
                return Ok(false);
            }
            let V::List(actors) = get(item, "actor_ids") else {
                return Ok(false);
            };
            for actor in actors {
                if !historical.contains(&py::key(actor, Held::Plain)?) {
                    return Ok(false);
                }
            }
            Ok(matches!(get(item, "deltas"), V::Map(_)))
        };
        if !fits()? {
            return value_error("body_state_v1 social history item is invalid");
        }
    }
    Ok(())
}
