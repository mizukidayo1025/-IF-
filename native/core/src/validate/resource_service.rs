//! The resource service block of `WorldState.validate` (`core/world.py`), with Python's errors and
//! texts: the config (`ResourceServiceConfig`, a pydantic model of `experiments/scenario.py`) and
//! WorldState v6, the ledger it needs, and `resource_service_v1`: its state, its request and stall
//! histories, and the stalls at the queues' heads. It raises the `StateInvariantError`s of
//! `WorldState.validate` itself.

use std::collections::HashSet;
use std::sync::Arc;

use indexmap::IndexMap;

use super::py::{equal, unhashable};
use super::{wrap_value, View};
use super::models;
use crate::error::{Error, Result};
use crate::rules::facility_queues::independent_heads;
use crate::state::facility_queues::QueuesRow;
use crate::value::V;

const SERVICE: &str = "resource_service_v1";
const LEDGER: &str = "resource_ledger_v1";
const LIFECYCLE: &str = "population_lifecycle_v1";
const QUEUES: &str = "facility_queues_v1";
const ACTIONS: [&str; 2] = ["fetch_water", "collect_meal"];
const OUTCOMES: [&str; 3] = ["queued", "travel_scheduled", "rejected"];
const REQUEST_REASONS: [&str; 9] = [
    "arrived_at_facility",
    "route_available",
    "in_room_route_available",
    "room_route_blocked",
    "no_facility",
    "no_route",
    "request_rejected",
    "travel_rejected",
    "service_disabled",
];
const PAIRS: [(&str, &str); 9] = [
    ("queued", "arrived_at_facility"),
    ("travel_scheduled", "route_available"),
    ("travel_scheduled", "in_room_route_available"),
    ("rejected", "room_route_blocked"),
    ("rejected", "no_facility"),
    ("rejected", "no_route"),
    ("rejected", "request_rejected"),
    ("rejected", "travel_rejected"),
    ("rejected", "service_disabled"),
];
const STALL_REASONS: [&str; 5] = ["capacity", "staff", "stock", "run_end", "water_outage"];

/// `ResourceServiceConfig`: what the entity rules read of it (`resource_config` of
/// `_validate_entity_rules`).
#[derive(Clone, Debug, PartialEq)]
pub struct ServiceConfig {
    pub service_minutes: i64,
    pub water_ml_per_service: i64,
    pub food_meal_units_per_service: i64,
    pub water_staff_skill: Arc<str>,
    pub meal_staff_skill: Arc<str>,
}

impl ServiceConfig {
    /// The fields of the model's dump.
    fn of(dumped: &V) -> Result<Self> {
        let V::Map(map) = dumped else {
            return Err(Error::unsupported("a model dump is not a dict"));
        };
        let int = |name: &str| match map.get(name) {
            Some(V::Int(number)) => Ok(*number),
            _ => Err(Error::unsupported("a model dump lacks an int")),
        };
        let text = |name: &str| match map.get(name) {
            Some(V::Str(text)) => Ok(text.clone()),
            _ => Err(Error::unsupported("a model dump lacks a str")),
        };
        Ok(Self {
            service_minutes: int("service_minutes")?,
            water_ml_per_service: int("water_ml_per_service")?,
            food_meal_units_per_service: int("food_meal_units_per_service")?,
            water_staff_skill: text("water_staff_skill")?,
            meal_staff_skill: text("meal_staff_skill")?,
        })
    }
}

fn invariant(text: &str) -> Error {
    Error::Invariant(text.to_owned())
}

/// The `TypeError` Python raises for `value in a_set` where it cannot hash the value; else whether
/// it is one of the texts.
pub fn is_in(value: &V, known: impl Fn(&str) -> bool) -> Result<bool> {
    if let Some(error) = unhashable(value) {
        return Err(error);
    }
    Ok(matches!(value, V::Str(text) if known(text)))
}

/// `records_since_previous_tick(history, minute_index)` of `core/history.py`: the records of the
/// tick that just ended, with the last earlier one.
pub fn since_previous_tick(history: &[V], minute_index: i64) -> &[V] {
    let mut start = history.len();
    while start > 0 {
        if let V::Map(item) = &history[start - 1] {
            if let Some(V::Int(minute)) = item.get("minute") {
                if *minute < minute_index - 1 {
                    break;
                }
            }
        }
        start -= 1;
    }
    &history[start.saturating_sub(1)..]
}

/// A list of non-empty, distinct str.
fn distinct_texts(value: Option<&V>) -> bool {
    let Some(V::List(items)) = value else {
        return false;
    };
    let mut seen = HashSet::new();
    items.iter().all(|item| matches!(item, V::Str(text) if !text.is_empty() && seen.insert(text.clone())))
}

fn has_fields(map: &IndexMap<Arc<str>, V>, fields: &[&str]) -> bool {
    map.len() == fields.len() && fields.iter().all(|field| map.contains_key(*field))
}

/// `ResourceServiceConfig.model_validate(config).model_dump(mode="json")`, or its error.
fn config_model(config: &V) -> Result<V> {
    models::resource_service_config(config)
}

/// The check, as `validate` (`validate/mod.rs`) calls it in Python's order: the config where the
/// configs have one (`resource_config`), or None.
pub fn validate(view: &View<'_>) -> Result<Option<ServiceConfig>> {
    let raw = view.config("resource_service")?;
    if raw.is_some() != (view.schema_version()? == 6) {
        return Err(invariant("resource_service requires WorldState v6; v6 requires resource_service"));
    }
    let Some(raw) = raw else {
        if view.contains(SERVICE)? {
            return Err(invariant("resource_service state requires WorldState v6"));
        }
        return Ok(None);
    };
    let dumped = config_model(&raw).map_err(wrap_value("invalid resource_service config: "))?;
    if !equal(&raw, &dumped) {
        return Err(invariant("resource_service config must contain the complete canonical v1 values"));
    }
    if !view.contains(LEDGER)? {
        return Err(invariant("WorldState v6 requires a resource ledger"));
    }
    let state = view.state(SERVICE)?;
    let fields = ["schema_version", "cancelled_task_ids", "active_reservation_ids", "request_history", "queue_stall_history", "stall_state"];
    let state = match state.as_deref() {
        Some(V::Map(state))
            if has_fields(state, &fields)
                && equal(&state["schema_version"], &V::Int(1))
                && distinct_texts(state.get("cancelled_task_ids"))
                && distinct_texts(state.get("active_reservation_ids"))
                && matches!(state["request_history"], V::List(_))
                && matches!(state["queue_stall_history"], V::List(_))
                && matches!(state["stall_state"], V::Map(_)) =>
        {
            state.clone()
        }
        _ => return Err(invariant("resource_service_v1 state is invalid")),
    };
    let (V::List(requests), V::List(stalls), V::Map(stall_state)) = (&state["request_history"], &state["queue_stall_history"], &state["stall_state"]) else {
        return Err(invariant("resource_service_v1 state is invalid"));
    };
    let lifecycle = view.state(LIFECYCLE)?;
    let retained: HashSet<Arc<str>> = match lifecycle.as_deref() {
        Some(V::Map(lifecycle)) => match lifecycle.get("people") {
            Some(V::Map(people)) => people.keys().cloned().collect(),
            _ => HashSet::new(),
        },
        _ => HashSet::new(),
    };
    let rows = view.rows()?;
    let minute_index = view.minute_index()?;
    let known_agent = |id: &str| rows.agents.contains_key(id) || retained.contains(id);
    let (requests, stalls) = if view.history {
        (&requests[..], &stalls[..])
    } else {
        (since_previous_tick(requests, minute_index), since_previous_tick(stalls, minute_index))
    };
    let request_fields = ["minute", "agent_id", "action", "outcome", "reason", "facility_id", "origin_zone_id", "detail"];
    for record in requests {
        let bad = || invariant("resource service request history is invalid");
        let V::Map(record) = record else {
            return Err(bad());
        };
        if !has_fields(record, &request_fields) || !matches!(record["minute"], V::Int(minute) if (0..=minute_index).contains(&minute)) {
            return Err(bad());
        }
        if !is_in(&record["agent_id"], known_agent)?
            || !is_in(&record["action"], |action| ACTIONS.contains(&action))?
            || !is_in(&record["outcome"], |outcome| OUTCOMES.contains(&outcome))?
            || !is_in(&record["reason"], |reason| REQUEST_REASONS.contains(&reason))?
            || !is_in(&record["origin_zone_id"], |zone| rows.zones.contains_key(zone))?
        {
            return Err(bad());
        }
        let facility = &record["facility_id"];
        if !matches!(facility, V::Null) && !is_in(facility, |id| rows.facilities.contains_key(id))? {
            return Err(bad());
        }
        if !matches!(record["detail"], V::Null | V::Str(_)) {
            return Err(bad());
        }
        let expected = if matches!(&record["action"], V::Str(action) if &**action == "fetch_water") { "water_station" } else { "meal_distribution" };
        if let V::Str(facility) = facility {
            if rows.facilities.get(facility).is_some_and(|facility| &*facility.kind != expected) {
                return Err(invariant("resource service request history has wrong facility"));
            }
        }
        let (V::Str(outcome), V::Str(reason)) = (&record["outcome"], &record["reason"]) else {
            return Err(bad());
        };
        if !PAIRS.contains(&(&**outcome, &**reason)) {
            return Err(invariant("resource service request outcome is inconsistent"));
        }
    }
    let mut previous = -1;
    for record in stalls {
        let bad = || invariant("resource queue stall history is invalid");
        let V::Map(record) = record else {
            return Err(bad());
        };
        if !has_fields(record, &["minute", "facility_id", "agent_id", "reason"]) {
            return Err(bad());
        }
        let V::Int(minute) = record["minute"] else {
            return Err(bad());
        };
        if minute < 0 || minute < previous || minute > minute_index {
            return Err(bad());
        }
        let facility_ok = matches!(&record["facility_id"], V::Str(id) if rows.facilities.contains_key(id));
        let agent_ok = matches!(&record["agent_id"], V::Str(id) if known_agent(id));
        if !facility_ok || !agent_ok || !is_in(&record["reason"], |reason| STALL_REASONS.contains(&reason))? {
            return Err(bad());
        }
        previous = minute;
    }
    for (facility_id, record) in stall_state {
        let bad = || invariant("resource queue stall state is invalid");
        if !rows.facilities.contains_key(facility_id) {
            return Err(bad());
        }
        let record = match record {
            V::Map(record) if has_fields(record, &["agent_id", "reason"]) => record,
            _ => return Err(bad()),
        };
        if !is_in(&record["agent_id"], |id| rows.agents.contains_key(id))? || !is_in(&record["reason"], |reason| STALL_REASONS.contains(&reason))? {
            return Err(bad());
        }
        let V::Str(agent_id) = &record["agent_id"] else {
            return Err(bad());
        };
        let facility = rows.facilities.get(facility_id).ok_or_else(bad)?;
        let resource = matches!(&*facility.kind, "water_station" | "meal_distribution");
        let heads = match view.row::<QueuesRow>(QUEUES)? {
            Some(row) => independent_heads(&row, facility_id)?,
            None if view.contains(QUEUES)? => return Err(Error::unsupported("facility_queues_v1 is not its row")),
            None => None,
        };
        let heads = heads.unwrap_or_else(|| facility.resource_queue_agent_ids.first().cloned().into_iter().collect());
        if !resource || !heads.iter().any(|head| head == agent_id) {
            return Err(invariant("resource queue stall must describe the current facility queue head"));
        }
    }
    ServiceConfig::of(&dumped).map(Some)
}
