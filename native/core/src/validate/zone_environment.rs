//! `validate_zone_environment_runtime_state(world, validate_event_history=...)` of
//! `domain/environment.py`, with Python's errors and texts. `validate::world` makes a `ValueError`
//! (a pydantic `ValidationError` too) `invalid zone environment state: {exc}`.
//!
//! The config and each modifier's parameters go through their pydantic contracts
//! (`contracts::checked`) as Python holds them: the config and the state as its tracked tree
//! (`TrackedDict`), whose class names the texts show.

use std::sync::Arc;

use indexmap::IndexMap;
use rustc_hash::FxHashMap;

use super::effect_program::{unhashable, EventsById, MARKER};
use super::View;
use crate::contracts::{self, Checked, Holder};
use crate::error::{Error, Result};
use crate::events::is_stable_id;
use crate::rules::dispatch::py_eq;
use crate::value::V;

const CONFIG: &str = "zone_environment";
const STATE: &str = "zone_environment_v1";
const POLICY: &str = "zone_environment_v1";
const EVENT_PREFIX: &str = "effect_program_environment";
const START: &str = "effect_program_environment";
const EXPIRY: &str = "effect_program_environment_expire";
/// `LEGACY_ZONE_ENVIRONMENT_FIELDS`: fields an older canonical config lacks.
const LEGACY_FIELDS: [&str; 7] = [
    "lighting_sleep_efficiency_loss",
    "ventilation_hygiene_growth_per_minute",
    "privacy_discontent_per_minute",
    "temperature_stress_per_degree_minute",
    "comfortable_min_temperature_c",
    "comfortable_max_temperature_c",
    "baseline_by_zone",
];
const RECORD_FIELDS: [&str; 12] = [
    "modifier_id",
    "program_id",
    "effect_id",
    "op",
    "zone_id",
    "parameters",
    "start_event_id",
    "end_event_id",
    "start_minute",
    "end_minute",
    "source_id",
    "assumption_ids",
];
const OPERATORS: [&str; 6] = ["set_ambient_noise", "modify_demand_rate", "set_ventilation", "set_lighting", "set_privacy", "set_temperature"];

type Map = IndexMap<Arc<str>, V>;

fn value_error(text: &str) -> Error {
    Error::Value(text.to_owned())
}

fn has_fields(map: &Map, fields: &[&str]) -> bool {
    map.len() == fields.len() && fields.iter().all(|field| map.contains_key(*field))
}

fn text(value: &str) -> V {
    V::Str(Arc::from(value))
}

/// `_has_exact_schema(state)`.
fn exact_schema(state: Option<&V>) -> Option<&Map> {
    match state {
        Some(V::Map(state))
            if has_fields(state, &["schema_version", "modifiers"])
                && py_eq(&state["schema_version"], &V::Int(1))
                && matches!(state["modifiers"], V::Map(_)) =>
        {
            match &state["modifiers"] {
                V::Map(modifiers) => Some(modifiers),
                _ => None,
            }
        }
        _ => None,
    }
}

/// `re.fullmatch(r"modifier-[0-9a-f]{32}", value)`.
fn is_modifier_id(value: &str) -> bool {
    value.len() == 41 && value.starts_with("modifier-") && value.as_bytes()[9..].iter().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn float_of(value: &V) -> f64 {
    match value {
        V::Float(number) => *number,
        V::Int(number) => *number as f64,
        _ => f64::NAN,
    }
}

/// The check, as `validate` (`validate/mod.rs`) calls it in Python's order.
pub fn validate(view: &View<'_>) -> Result<()> {
    let config = view.config(CONFIG)?.filter(|config| !matches!(config, V::Null));
    let state = view.state(STATE)?;
    let state = state.as_deref().filter(|state| !matches!(state, V::Null));
    let Some(config_data) = config else {
        if state.is_some() {
            return Err(value_error("zone environment state requires a zone_environment config"));
        }
        let queue = view.queue()?;
        for (_, serial) in queue.ordered() {
            let row = queue.slot(*serial).row.as_ref().ok_or_else(|| Error::unsupported("a pending event has no row"))?;
            if row.event_type.starts_with(EVENT_PREFIX) {
                return Err(value_error("zone environment events require the zone_environment policy"));
            }
        }
        if view.history && view.log()?.event_types().iter().any(|event_type| event_type.starts_with(EVENT_PREFIX)) {
            return Err(value_error("zone environment events require the zone_environment policy"));
        }
        return Ok(());
    };
    let canonical = contracts::checked(&Checked::ZoneEnvironment, &config_data, Holder::Tracked)?;
    let V::Map(canonical_map) = &canonical else {
        return Err(Error::unsupported("a config's dump is no dict"));
    };
    let legacy: Map = canonical_map.iter().filter(|(key, _)| !LEGACY_FIELDS.contains(&&***key)).map(|(key, value)| (key.clone(), value.clone())).collect();
    if !py_eq(&config_data, &canonical) && !py_eq(&config_data, &V::Map(legacy)) {
        return Err(value_error("zone_environment config must contain canonical v1 values"));
    }
    let rows = view.rows()?;
    if let Some(V::Map(baseline)) = canonical_map.get("baseline_by_zone") {
        if baseline.keys().any(|zone| !rows.zones.contains_key(zone)) {
            return Err(value_error("zone environment baseline references an unknown zone"));
        }
    }
    let Some(modifiers) = exact_schema(state) else {
        return Err(value_error("zone_environment_v1 has an invalid exact schema"));
    };
    if !view.history {
        return Ok(());
    }
    let policy_version = canonical_map.get("policy_version");
    let config_number = |name: &str| canonical_map.get(name).map_or(f64::NAN, float_of);
    let max_per_zone = match canonical_map.get("max_active_modifiers_per_zone") {
        Some(V::Int(limit)) => *limit,
        _ => return Err(Error::unsupported("a config's dump without its limit")),
    };
    let duration = view.duration_minutes()?;
    let by_id = EventsById::new(view)?;
    let mut counts: FxHashMap<Arc<str>, i64> = FxHashMap::default();
    let invalid = || value_error("zone environment modifier record is invalid");
    for (modifier_id, record) in modifiers {
        if !is_modifier_id(modifier_id) {
            return Err(invalid());
        }
        let record = match record {
            V::Map(record) if has_fields(record, &RECORD_FIELDS) => record,
            _ => return Err(invalid()),
        };
        if !matches!(&record["modifier_id"], V::Str(id) if id == modifier_id) {
            return Err(invalid());
        }
        // `record.get("op") not in {...}` and `record.get("zone_id") not in world.zones` hash them.
        unhashable(&record["op"])?;
        let op = match &record["op"] {
            V::Str(op) if OPERATORS.contains(&&**op) => op.clone(),
            _ => return Err(invalid()),
        };
        unhashable(&record["zone_id"])?;
        let zone_id = match &record["zone_id"] {
            V::Str(zone) if rows.zones.contains_key(zone) => zone.clone(),
            _ => return Err(invalid()),
        };
        let stable = |name: &str| match &record[name] {
            V::Str(id) if is_stable_id(id) => Some(id.clone()),
            _ => None,
        };
        let (Some(program_id), Some(effect_id)) = (stable("program_id"), stable("effect_id")) else {
            return Err(invalid());
        };
        let (V::Str(start_event_id), V::Str(end_event_id), V::Str(source_id), V::List(assumption_ids)) =
            (&record["start_event_id"], &record["end_event_id"], &record["source_id"], &record["assumption_ids"])
        else {
            return Err(invalid());
        };
        let mut assumptions: Vec<&str> = Vec::with_capacity(assumption_ids.len());
        for item in assumption_ids {
            match item {
                V::Str(id) if !id.is_empty() => assumptions.push(id),
                _ => return Err(invalid()),
            }
        }
        let mut unique = assumptions.clone();
        unique.sort_unstable();
        unique.dedup();
        if unique.len() != assumptions.len() {
            return Err(invalid());
        }
        let (V::Int(start_minute), V::Int(end_minute)) = (&record["start_minute"], &record["end_minute"]) else {
            return Err(invalid());
        };
        let (start_minute, end_minute) = (*start_minute, *end_minute);
        if !(0 <= start_minute && start_minute < end_minute && end_minute < duration) {
            return Err(invalid());
        }
        let parameters = contracts::checked(&Checked::Parameters(op.clone()), &record["parameters"], Holder::Tracked)?;
        if matches!(&*op, "set_ventilation" | "set_lighting" | "set_privacy" | "set_temperature")
            && !policy_version.is_some_and(|version| py_eq(version, &text("zone-environment-v2")))
        {
            return Err(value_error("condition modifiers require zone-environment-v2"));
        }
        if &*op == "modify_demand_rate" {
            let V::Map(dumped) = &parameters else {
                return Err(Error::unsupported("a parameters' dump is no dict"));
            };
            let action = match dumped.get("action") {
                Some(V::Str(action)) => action.clone(),
                _ => return Err(Error::unsupported("a demand modifier's dump without its action")),
            };
            if matches!(&*action, "fetch_water" | "collect_meal") && !view.contains("resource_service_v1")? {
                return Err(value_error("zone demand modifier requires the resource-service policy"));
            }
            let probability = dumped.get("probability_multiplier").map_or(f64::NAN, float_of);
            let duration_multiplier = dumped.get("duration_multiplier").map_or(f64::NAN, float_of);
            if probability < config_number("min_probability_multiplier")
                || probability > config_number("max_probability_multiplier")
                || (matches!(&*action, "rest" | "sleep") && !(0.5..=2.0).contains(&duration_multiplier))
            {
                return Err(value_error("zone demand modifier exceeds configured limits"));
            }
        }
        if !canonical_map.get("source_id").is_some_and(|source| py_eq(source, &V::Str(source_id.clone()))) {
            return Err(value_error("zone environment modifier source does not match its assumption config"));
        }
        let links = || value_error("zone environment modifier has invalid start/expiry event links");
        let (Some(start), Some(end)) = (by_id.get(start_event_id)?, by_id.get(end_event_id)?) else {
            return Err(links());
        };
        let mut start_payload = Map::with_capacity(7);
        start_payload.insert(Arc::from("program_id"), V::Str(program_id.clone()));
        start_payload.insert(Arc::from("effect_id"), V::Str(effect_id.clone()));
        start_payload.insert(Arc::from("modifier_id"), V::Str(modifier_id.clone()));
        start_payload.insert(Arc::from("zone_id"), V::Str(zone_id.clone()));
        let end_payload = start_payload.clone();
        start_payload.insert(Arc::from("op"), V::Str(op.clone()));
        start_payload.insert(Arc::from("parameters"), parameters);
        start_payload.insert(Arc::from("end_minute"), V::Int(end_minute));
        let start_marker = match &start.provenance {
            V::Map(provenance) => match provenance.get(MARKER) {
                Some(V::Map(marker)) => Some(marker),
                _ => None,
            },
            _ => None,
        };
        if &*start.event_type != START
            || &*end.event_type != EXPIRY
            || !py_eq(&start.payload, &V::Map(start_payload))
            || !py_eq(&end.payload, &V::Map(end_payload))
            || start.due_minute != start_minute
            || end.due_minute != end_minute
            || end.causation_id.as_deref() != Some(&*start.event_id)
            || start_marker.is_none()
        {
            return Err(links());
        }
        let marker = start_marker.unwrap_or_else(|| unreachable!("checked above"));
        let get = |name: &str| marker.get(name).unwrap_or(&V::Null);
        let dependencies_match = matches!(get("policy_dependencies"), V::List(items) if items.len() == 1 && py_eq(&items[0], &text(POLICY)));
        let assumptions_match = match get("assumption_ids") {
            V::List(items) => items.len() == assumption_ids.len() && items.iter().zip(assumption_ids).all(|(a, b)| py_eq(a, b)),
            _ => false,
        };
        if !py_eq(get("phase"), &text("start"))
            || !py_eq(get("program_id"), &V::Str(program_id.clone()))
            || !py_eq(get("effect_id"), &V::Str(effect_id.clone()))
            || !py_eq(get("op"), &V::Str(op.clone()))
            || !dependencies_match
            || !assumptions_match
            || assumptions != unique
        {
            return Err(value_error("zone environment modifier provenance does not match its start event"));
        }
        *counts.entry(zone_id).or_default() += 1;
    }
    if counts.values().any(|count| *count > max_per_zone) {
        return Err(value_error("zone environment runtime exceeds its per-zone modifier limit"));
    }
    Ok(())
}
