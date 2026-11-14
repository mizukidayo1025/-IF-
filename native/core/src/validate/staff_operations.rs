//! `validate_staff_operations_state(world)` of `domain/staff_operations.py`, and the
//! `canonical_staff_operations_config` it calls, with Python's errors and texts (lane D of #250,
//! #349). Raw errors.

use std::sync::Arc;

use indexmap::IndexMap;

use super::extension_head::py::{self, eq, eq_int, eq_opt_str, eq_str, get, keys_are, Held};
use super::View;
use crate::error::{Error, Result};
use crate::events::EventRow;
use crate::text::py_repr;
use crate::value::V;

const NAME: &str = "staff_operations_v1";
const POLICY: &str = "autonomous-routed-staff-v1";
const SOURCE: &str = "ASM-STAFF-OPERATIONS-V1";
const MAX_STAFF_HISTORY: usize = 20_000;
/// `_DURATION_DEFAULTS`, in its order.
const DURATIONS: [(&str, i64); 5] = [("cleaning", 15), ("maintenance", 20), ("information_outreach", 10), ("intake", 5), ("patrol", 15)];
const TASK_KINDS: [&str; 5] = ["cleaning", "maintenance", "information_outreach", "intake", "patrol"];
const FIELDS: [&str; 7] = ["schema_version", "policy_version", "source_id", "config", "zones", "staff", "history"];
const CONFIG_FIELDS: [&str; 10] = [
    "schema_version",
    "policy_version",
    "source_id",
    "task_duration_minutes",
    "sanitation_use_per_person_minute",
    "cleaning_sanitation_recovery",
    "outreach_uncertainty_relief",
    "patrol_reassurance",
    "thresholds_by_zone",
    "effects_by_zone",
];
const RATES: [&str; 4] = ["sanitation_use_per_person_minute", "cleaning_sanitation_recovery", "outreach_uncertainty_relief", "patrol_reassurance"];
const THRESHOLDS: [&str; 3] = ["cleaning_sanitation_below", "outreach_uncertainty_at_least", "patrol_stress_at_least"];
/// The per-zone effects and their bounds, as `{lower:g}..{upper:g}` writes them.
const EFFECTS: [(&str, f64, f64, &str); 4] = [
    ("sanitation_use_per_person_minute", 0.0, 0.01, "0..0.01"),
    ("cleaning_sanitation_recovery", 0.0, 1.0, "0..1"),
    ("outreach_uncertainty_relief", 0.0, 1.0, "0..1"),
    ("patrol_reassurance", 0.0, 1.0, "0..1"),
];
const RECORD_FIELDS: [&str; 14] = [
    "phase",
    "task_id",
    "task_kind",
    "target_id",
    "target_zone_id",
    "reason",
    "started_minute",
    "travel_due_minute",
    "arrival_minute",
    "due_minute",
    "route_zone_ids",
    "route_edge_ids",
    "travel_minutes",
    "causation_event_ids",
];
const HISTORY_FIELDS: [&str; 12] = [
    "task_id",
    "staff_id",
    "task_kind",
    "target_id",
    "target_zone_id",
    "reason",
    "status",
    "start_minute",
    "arrival_minute",
    "complete_minute",
    "travel_minutes",
    "causation_event_ids",
];

type Map = IndexMap<Arc<str>, V>;

fn value_error<T>(text: &str) -> Result<T> {
    Err(Error::Value(text.to_owned()))
}

fn text(value: &str) -> V {
    V::Str(Arc::from(value))
}

/// `canonical_staff_operations_config(raw)`: the config, or its ValueError.
pub fn canonical_staff_operations_config(raw: &V) -> Result<Map> {
    let empty = Map::new();
    let raw = match raw {
        V::Null => &empty,
        V::Map(raw) if raw.keys().all(|key| CONFIG_FIELDS.contains(&&**key)) => raw,
        _ => return value_error("staff_operations config has unsupported fields"),
    };
    let mut config = Map::new();
    config.insert("schema_version".into(), V::Int(1));
    config.insert("policy_version".into(), text(POLICY));
    config.insert("source_id".into(), text(SOURCE));
    config.insert("task_duration_minutes".into(), V::Map(DURATIONS.iter().map(|(kind, minutes)| (Arc::from(*kind), V::Int(*minutes))).collect()));
    config.insert("sanitation_use_per_person_minute".into(), V::Float(0.000005));
    config.insert("cleaning_sanitation_recovery".into(), V::Float(0.15));
    config.insert("outreach_uncertainty_relief".into(), V::Float(0.04));
    config.insert("patrol_reassurance".into(), V::Float(0.01));
    config.insert("thresholds_by_zone".into(), V::Map(Map::new()));
    config.insert("effects_by_zone".into(), V::Map(Map::new()));
    for (key, value) in raw {
        config.insert(key.clone(), value.clone());
    }
    if !eq_int(config.get("schema_version"), 1) || !eq_str(config.get("policy_version"), POLICY) || !eq_str(config.get("source_id"), SOURCE) {
        return value_error("unsupported staff_operations policy or source");
    }
    let durations = match get(&config, "task_duration_minutes") {
        V::Map(durations) if keys_are(durations, &TASK_KINDS) => durations,
        _ => return value_error("staff_operations task durations must define every registered task"),
    };
    for (kind, value) in durations {
        if !py::is_exact_int(value) || !py::wide(value).is_some_and(|value| (1..=24 * 60).contains(&value)) {
            return Err(Error::Value(format!("staff_operations duration for {kind} is outside 1..1440 minutes")));
        }
    }
    for name in RATES {
        if !py::finite_within(get(&config, name), 0.0, 1.0) {
            return Err(Error::Value(format!("staff_operations {name} must be finite and within 0..1")));
        }
    }
    let V::Map(thresholds_by_zone) = get(&config, "thresholds_by_zone") else {
        return value_error("staff_operations thresholds_by_zone must be an object");
    };
    for (zone_id, thresholds) in thresholds_by_zone {
        let thresholds = match thresholds {
            V::Map(thresholds) if !zone_id.is_empty() && !thresholds.is_empty() && thresholds.keys().all(|name| THRESHOLDS.contains(&&**name)) => thresholds,
            _ => return value_error("staff_operations per-zone threshold entry is invalid"),
        };
        for (name, value) in thresholds {
            if !py::finite_within(value, 0.0, 1.0) {
                return Err(Error::Value(format!("staff_operations per-zone {name} must be finite and within 0..1")));
            }
        }
    }
    let V::Map(effects_by_zone) = get(&config, "effects_by_zone") else {
        return value_error("staff_operations effects_by_zone must be an object");
    };
    for (zone_id, effects) in effects_by_zone {
        let effects = match effects {
            V::Map(effects) if !zone_id.is_empty() && !effects.is_empty() && effects.keys().all(|name| EFFECTS.iter().any(|(effect, ..)| *effect == &**name)) => effects,
            _ => return value_error("staff_operations per-zone effect entry is invalid"),
        };
        for (name, value) in effects {
            let Some((_, lower, upper, bounds)) = EFFECTS.iter().find(|(effect, ..)| *effect == &**name) else {
                return value_error("staff_operations per-zone effect entry is invalid");
            };
            if !py::finite_within(value, *lower, *upper) {
                return Err(Error::Value(format!("staff_operations per-zone {name} must be finite and within {bounds}")));
            }
        }
    }
    Ok(config)
}

/// `_idle_record()`.
fn idle_record() -> V {
    let mut record = Map::new();
    for name in RECORD_FIELDS {
        let value = match name {
            "phase" => text("idle"),
            "route_zone_ids" | "route_edge_ids" | "causation_event_ids" => V::List(Vec::new()),
            "travel_minutes" => V::Int(0),
            _ => V::Null,
        };
        record.insert(name.into(), value);
    }
    V::Map(record)
}

/// The pending events in queue order (`world.event_queue.pending()`).
fn pending(view: &View<'_>) -> Result<Vec<Arc<EventRow>>> {
    let queue = view.queue()?;
    queue
        .ordered()
        .iter()
        .map(|(_, serial)| queue.slot(*serial).row.clone().ok_or_else(|| Error::unsupported("a pending event is not in the core")))
        .collect()
}

/// The check, as `validate` (`validate/mod.rs`) calls it in Python's order.
pub fn validate(view: &View<'_>) -> Result<()> {
    let Some(state) = view.state(NAME)? else {
        return Ok(());
    };
    let schema = || value_error("staff_operations_v1 has an invalid exact schema");
    let state = match &*state {
        V::Null => return Ok(()),
        V::Map(state) => state,
        _ => return schema(),
    };
    let rows = view.rows()?;
    if !keys_are(state, &FIELDS)
        || !eq_int(state.get("schema_version"), 1)
        || !eq_str(state.get("policy_version"), POLICY)
        || !eq_str(state.get("source_id"), SOURCE)
        || !matches!(get(state, "zones"), V::Map(zones) if zones.len() == rows.zones.len() && zones.keys().all(|id| rows.zones.contains_key(id)))
        || !matches!(get(state, "staff"), V::Map(staff) if staff.len() == rows.staff.len() && staff.keys().all(|id| rows.staff.contains_key(id)))
        || !matches!(get(state, "history"), V::List(history) if history.len() <= MAX_STAFF_HISTORY)
    {
        return schema();
    }
    let saved = get(state, "config");
    let config = canonical_staff_operations_config(saved)?;
    if !eq(&V::Map(config.clone()), saved) {
        let mut legacy = config.clone();
        for additive in ["thresholds_by_zone", "effects_by_zone"] {
            // `additive not in state["config"]`: a saved None is the only other value that reaches here.
            let absent = match saved {
                V::Map(saved) => !saved.contains_key(additive),
                other => return Err(Error::Type(format!("argument of type '{}' is not iterable", py::type_name(other)))),
            };
            if absent && !get(&config, additive).is_truthy() {
                legacy.shift_remove(additive);
            }
        }
        if !eq(&V::Map(legacy), saved) {
            return value_error("staff_operations_v1 config is not canonical");
        }
    }
    let zones_known = |names: &V| matches!(names, V::Map(names) if names.keys().all(|id| rows.zones.contains_key(id)));
    if !zones_known(get(&config, "thresholds_by_zone")) {
        return value_error("staff_operations_v1 per-zone threshold references an unknown zone");
    }
    if !zones_known(get(&config, "effects_by_zone")) {
        return value_error("staff_operations_v1 per-zone effect references an unknown zone");
    }
    if let V::Map(zones) = get(state, "zones") {
        for (zone_id, zone) in zones {
            let fits = matches!(zone, V::Map(zone) if keys_are(zone, &["sanitation", "updated_minute"])
                && py::finite_within(get(zone, "sanitation"), 0.0, 1.0)
                && py::is_exact_int(get(zone, "updated_minute"))
                && !py::negative(get(zone, "updated_minute")));
            if !fits {
                return Err(Error::Value(format!("staff_operations_v1 zone state for {} is invalid", py_repr(zone_id))));
            }
        }
    }
    let pending = pending(view)?;
    let durations = get(&config, "task_duration_minutes");
    if let V::Map(staff) = get(state, "staff") {
        for (staff_id, record) in staff {
            check_record(&rows, &pending, durations, staff_id, record)?;
        }
    }
    if let V::List(history) = get(state, "history") {
        for item in history {
            if !history_item_fits(&rows, item)? {
                return value_error("staff_operations_v1 history item is invalid");
            }
        }
    }
    Ok(())
}

/// The rules of one staff member's record.
fn check_record(rows: &crate::entities::Entities, pending: &[Arc<EventRow>], durations: &V, staff_id: &Arc<str>, record: &V) -> Result<()> {
    let id = py_repr(staff_id);
    let record = match record {
        V::Map(record) if keys_are(record, &RECORD_FIELDS) && py::in_texts(get(record, "phase"), Held::Tracked, &["idle", "travel", "working"])? => record,
        V::Map(record) if keys_are(record, &RECORD_FIELDS) => return Err(Error::Value(format!("staff_operations_v1 record for {id} is invalid"))),
        _ => return Err(Error::Value(format!("staff_operations_v1 record for {id} is invalid"))),
    };
    let phase = get(record, "phase");
    if eq_str(Some(phase), "idle") {
        if !eq(&V::Map(record.clone()), &idle_record()) {
            return Err(Error::Value(format!("idle staff_operations_v1 record for {id} retains a task")));
        }
        return Ok(());
    }
    let Some(member) = rows.staff.get(staff_id) else {
        return Err(Error::Key(staff_id.to_string()));
    };
    let field = |name: &str| get(record, name);
    let task_id = field("task_id");
    let started = field("started_minute");
    let due = field("due_minute");
    let route_zones = field("route_zone_ids");
    let route_edges = field("route_edge_ids");
    let travel = field("travel_minutes");
    let target_zone = field("target_zone_id");
    let active = || -> Result<bool> {
        if !py::key_in(task_id, Held::Tracked, |id| rows.active_tasks.contains_key(id))? {
            return Ok(false);
        }
        let V::Str(task) = task_id else {
            return Ok(false);
        };
        let Some(row) = rows.active_tasks.get(task) else {
            return Ok(false);
        };
        if row.staff_id.as_deref() != Some(&**staff_id)
            || member.active_task_id.as_deref() != Some(&**task)
            || member.work_queue.len() != 1
            || member.work_queue[0] != *task
            || !eq(&V::Int(member.busy_until), due)
            || !py::in_texts(field("task_kind"), Held::Tracked, &TASK_KINDS)?
            || !py::key_in(target_zone, Held::Tracked, |id| rows.zones.contains_key(id))?
            || !py::is_exact_int(started)
            || !py::is_exact_int(due)
            || py::wide(due) <= py::wide(started)
        {
            return Ok(false);
        }
        let (V::List(zones), V::List(edges)) = (route_zones, route_edges) else {
            return Ok(false);
        };
        for zone in zones {
            if !py::key_in(zone, Held::Tracked, |id| rows.zones.contains_key(id))? {
                return Ok(false);
            }
        }
        for edge in edges {
            if !py::key_in(edge, Held::Tracked, |id| rows.zone_edges.contains_key(id))? {
                return Ok(false);
            }
        }
        if !py::is_exact_int(travel) || py::negative(travel) || !matches!(field("causation_event_ids"), V::List(_)) || py::negative(started) {
            return Ok(false);
        }
        if !matches!(field("reason"), V::Str(reason) if !reason.is_empty()) {
            return Ok(false);
        }
        let target = field("target_id");
        Ok(matches!(target, V::Null) || py::key_in(target, Held::Tracked, |id| rows.facilities.contains_key(id))?)
    };
    if !active()? {
        return Err(Error::Value(format!("active staff_operations_v1 record for {id} is invalid")));
    }
    // Every ID of the route is known and every minute an int now.
    let (V::List(zones), V::List(edges)) = (route_zones, route_edges) else {
        return Err(Error::Value(format!("active staff_operations_v1 record for {id} is invalid")));
    };
    let wide = |value: &V| py::wide(value).unwrap_or(0);
    let zone_at = |index: usize| match zones.get(index) {
        Some(V::Str(zone)) => Some(&**zone),
        _ => None,
    };
    let route_is_valid = zones.len() == edges.len() + 1
        && !edges.is_empty()
        && edges.iter().enumerate().all(|(index, edge)| {
            let edge = match edge {
                V::Str(edge) => rows.zone_edges.get(edge),
                _ => None,
            };
            edge.is_some_and(|edge| zone_at(index) == Some(&*edge.from_zone_id) && zone_at(index + 1) == Some(&*edge.to_zone_id))
        })
        && zone_at(0) == Some(&*member.zone_id)
        && zones.last().is_some_and(|last| eq(last, target_zone))
        && wide(travel)
            == edges
                .iter()
                .map(|edge| match edge {
                    V::Str(edge) => rows.zone_edges.get(edge).map_or(0, |edge| i128::from(edge.travel_minutes)),
                    _ => 0,
                })
                .sum::<i128>();
    let travel_due = field("travel_due_minute");
    let arrival = field("arrival_minute");
    let arrives_at = wide(started) + wide(travel);
    let is_travel = eq_str(Some(phase), "travel");
    if is_travel {
        if !route_is_valid || !py::is_exact_int(travel_due) || py::wide(travel_due) != Some(arrives_at) || !matches!(arrival, V::Null) {
            return Err(Error::Value(format!("staff_operations_v1 travel record for {id} is invalid")));
        }
    } else if !zones.is_empty()
        || !edges.is_empty()
        || !matches!(travel_due, V::Null)
        || !py::is_exact_int(arrival)
        || py::wide(arrival) != Some(arrives_at)
        || !eq_opt_str(target_zone, Some(&member.zone_id))
    {
        return Err(Error::Value(format!("staff_operations_v1 working record for {id} is invalid")));
    }
    let V::Str(kind) = field("task_kind") else {
        return Err(Error::Value(format!("active staff_operations_v1 record for {id} is invalid")));
    };
    let duration = match durations {
        V::Map(durations) => durations.get(kind).and_then(py::wide).unwrap_or(0),
        _ => 0,
    };
    if py::wide(due) != Some(arrives_at + duration) {
        return Err(Error::Value(format!("staff_operations_v1 due minute for {id} is invalid")));
    }
    let payload = |event: &EventRow, name: &str| -> V {
        match &event.payload {
            V::Map(payload) => payload.get(name).cloned().unwrap_or(V::Null),
            _ => V::Null,
        }
    };
    let staff_value = V::Str(staff_id.clone());
    let completions: Vec<&Arc<EventRow>> = pending
        .iter()
        .filter(|event| {
            &*event.event_type == "staff_operation_complete"
                && eq(&payload(event, "task_id"), task_id)
                && eq(&payload(event, "staff_id"), &staff_value)
                && eq(&V::Int(event.due_minute), due)
        })
        .collect();
    let arrivals: Vec<&Arc<EventRow>> = pending
        .iter()
        .filter(|event| &*event.event_type == "staff_operation_arrive" && eq(&payload(event, "task_id"), task_id) && eq(&payload(event, "staff_id"), &staff_value))
        .collect();
    let consistent = completions.len() == 1
        && eq(&payload(completions[0], "task_kind"), field("task_kind"))
        && eq(&payload(completions[0], "target_id"), field("target_id"))
        && arrivals.len() == usize::from(is_travel)
        && arrivals.first().is_none_or(|arrival| eq(&V::Int(arrival.due_minute), travel_due) && eq(&payload(arrival, "target_zone_id"), target_zone));
    if !consistent {
        return Err(Error::Value(format!("staff_operations_v1 event references for {id} are inconsistent")));
    }
    Ok(())
}

/// Whether a history item fits its rules (or the TypeError of hashing one of its values).
fn history_item_fits(rows: &crate::entities::Entities, item: &V) -> Result<bool> {
    let V::Map(item) = item else {
        return Ok(false);
    };
    if !keys_are(item, &HISTORY_FIELDS)
        || !py::key_in(get(item, "staff_id"), Held::Tracked, |id| rows.staff.contains_key(id))?
        || !py::in_texts(get(item, "task_kind"), Held::Tracked, &TASK_KINDS)?
        || !py::in_texts(get(item, "status"), Held::Tracked, &["completed", "blocked", "interrupted"])?
        || !py::key_in(get(item, "target_zone_id"), Held::Tracked, |id| rows.zones.contains_key(id))?
    {
        return Ok(false);
    }
    let target = get(item, "target_id");
    if !matches!(target, V::Null) && !py::key_in(target, Held::Tracked, |id| rows.facilities.contains_key(id))? {
        return Ok(false);
    }
    if !matches!(get(item, "task_id"), V::Str(_)) || !matches!(get(item, "reason"), V::Str(_)) {
        return Ok(false);
    }
    let minutes = ["start_minute", "complete_minute", "travel_minutes"];
    if minutes.iter().any(|name| !py::is_exact_int(get(item, name)) || py::negative(get(item, name))) {
        return Ok(false);
    }
    let start = py::wide(get(item, "start_minute"));
    let complete = py::wide(get(item, "complete_minute"));
    if complete < start {
        return Ok(false);
    }
    let arrival = get(item, "arrival_minute");
    if matches!(arrival, V::Null) && !(eq_str(Some(get(item, "status")), "interrupted") && py::wide(get(item, "travel_minutes")).is_some_and(|travel| travel > 0)) {
        return Ok(false);
    }
    if !matches!(arrival, V::Null) && (!py::is_exact_int(arrival) || !(start <= py::wide(arrival) && py::wide(arrival) <= complete)) {
        return Ok(false);
    }
    Ok(matches!(get(item, "causation_event_ids"), V::List(ids) if ids.iter().all(|id| matches!(id, V::Str(id) if !id.is_empty()))))
}
