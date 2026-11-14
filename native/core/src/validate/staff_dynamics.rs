//! `validate_staff_dynamics_state(world)` of `domain/staff_dynamics.py`, and the
//! `canonical_staff_dynamics_config` it calls, with Python's errors and texts (lane D of #250,
//! #349). Raw errors.

use std::sync::Arc;

use indexmap::IndexMap;

use super::extension_head::py::{self, eq, eq_int, get, keys_are, Held};
use super::View;
use crate::error::{Error, Result};
use crate::state::staff_dynamics::{NAME, POLICY_V1, POLICY_V2};
use crate::text::py_repr;
use crate::value::V;

const SOURCE_V1: &str = "ASM-STAFF-DYNAMICS-V1";
const SOURCE_V2: &str = "ASM-STAFF-DYNAMICS-V2";
const CONFIG_FIELDS: [&str; 8] = [
    "schema_version",
    "policy_version",
    "source_id",
    "max_continuous_work_minutes",
    "max_continuous_work_minutes_by_staff",
    "rest_minutes",
    "shift_windows_by_staff",
    "off_shift_fatigue_recovery_per_minute",
];
const RECORD_FIELDS: [&str; 6] =
    ["consecutive_work_minutes", "total_work_minutes", "total_rest_minutes", "fatigue", "rest_until_minute", "last_activity"];

type Map = IndexMap<Arc<str>, V>;

fn value_error<T>(text: &str) -> Result<T> {
    Err(Error::Value(text.to_owned()))
}

fn text(value: &str) -> V {
    V::Str(Arc::from(value))
}

/// `canonical_staff_dynamics_config(raw)`: the config, or its ValueError (or the TypeError of
/// hashing a policy that is a list or a dict).
pub fn canonical_staff_dynamics_config(raw: &V) -> Result<Map> {
    let empty = Map::new();
    let raw = match raw {
        V::Null => &empty,
        V::Map(raw) => raw,
        _ => return value_error("staff_dynamics config has unsupported fields"),
    };
    if raw.keys().any(|key| !CONFIG_FIELDS.contains(&&**key)) {
        return value_error("staff_dynamics config has unsupported fields");
    }
    let legacy = py::eq_str(raw.get("policy_version"), POLICY_V1)
        || (!raw.contains_key("policy_version") && !raw.contains_key("shift_windows_by_staff") && !raw.contains_key("off_shift_fatigue_recovery_per_minute"));
    let mut config = Map::new();
    config.insert("schema_version".into(), V::Int(1));
    config.insert("policy_version".into(), text(if legacy { POLICY_V1 } else { POLICY_V2 }));
    config.insert("source_id".into(), raw.get("source_id").cloned().unwrap_or_else(|| text(if legacy { SOURCE_V1 } else { SOURCE_V2 })));
    config.insert("max_continuous_work_minutes".into(), V::Int(240));
    config.insert("max_continuous_work_minutes_by_staff".into(), V::Map(Map::new()));
    config.insert("rest_minutes".into(), V::Int(30));
    config.insert("shift_windows_by_staff".into(), V::Map(Map::new()));
    config.insert("off_shift_fatigue_recovery_per_minute".into(), V::Float(if legacy { 0.0 } else { 0.005 }));
    for (key, value) in raw {
        config.insert(key.clone(), value.clone());
    }
    if !eq_int(config.get("schema_version"), 1)
        || !py::in_texts(get(&config, "policy_version"), Held::Tracked, &[POLICY_V1, POLICY_V2])?
        || !matches!(get(&config, "source_id"), V::Str(source) if !source.is_empty())
    {
        return value_error("unsupported staff_dynamics policy or source");
    }
    config.insert("schema_version".into(), V::Int(1));
    if py::eq_str(config.get("policy_version"), POLICY_V1)
        && (get(&config, "shift_windows_by_staff").is_truthy() || !eq(get(&config, "off_shift_fatigue_recovery_per_minute"), &V::Float(0.0)))
    {
        return value_error("staff shift and off-shift recovery assumptions require policy v2");
    }
    for (name, lower, upper) in [("max_continuous_work_minutes", 1, 24 * 60), ("rest_minutes", 1, 8 * 60)] {
        let value = get(&config, name);
        if !py::is_exact_int(value) || !py::wide(value).is_some_and(|value| lower <= value && value <= upper) {
            return Err(Error::Value(format!("staff_dynamics {name} is outside its supported range")));
        }
    }
    let V::Map(maxima) = get(&config, "max_continuous_work_minutes_by_staff") else {
        return value_error("staff_dynamics per-staff work limits must be an object");
    };
    for (staff_id, value) in maxima {
        if staff_id.is_empty() || !py::is_exact_int(value) || !py::wide(value).is_some_and(|value| (1..=24 * 60).contains(&value)) {
            return value_error("staff_dynamics per-staff work limits must be integer minutes in 1..1440");
        }
    }
    let V::Map(windows) = get(&config, "shift_windows_by_staff") else {
        return value_error("staff_dynamics shift_windows_by_staff must be an object");
    };
    for (staff_id, rows) in windows {
        let V::List(rows) = rows else {
            return value_error("staff_dynamics shift windows require a staff ID and at most 21 intervals");
        };
        if staff_id.is_empty() || rows.len() > 21 {
            return value_error("staff_dynamics shift windows require a staff ID and at most 21 intervals");
        }
        let mut intervals = Vec::with_capacity(rows.len());
        for row in rows {
            let interval = match row {
                V::Map(row) if keys_are(row, &["start_minute", "end_minute"]) => match (get(row, "start_minute"), get(row, "end_minute")) {
                    (start @ (V::Int(_) | V::Big(_)), end @ (V::Int(_) | V::Big(_))) => {
                        let (start, end) = (py::wide(start).unwrap_or(0), py::wide(end).unwrap_or(0));
                        (0 <= start && start < end && end <= 10_080).then_some((start, end))
                    }
                    _ => None,
                },
                _ => None,
            };
            let Some(interval) = interval else {
                return value_error("staff shift intervals must be non-empty [start,end) weekly minutes");
            };
            intervals.push(interval);
        }
        intervals.sort_unstable();
        if intervals.windows(2).any(|pair| pair[0].1 > pair[1].0) {
            return value_error("staff shift intervals for one person cannot overlap");
        }
    }
    if !py::finite_within(get(&config, "off_shift_fatigue_recovery_per_minute"), 0.0, 1.0) {
        return value_error("staff_dynamics off-shift recovery must be within 0..1 per minute");
    }
    Ok(config)
}

/// The check, as `validate` (`validate/mod.rs`) calls it in Python's order.
pub fn validate(view: &View<'_>) -> Result<()> {
    let Some(state) = view.state(NAME)? else {
        return Ok(());
    };
    let schema = || value_error("staff_dynamics_v1 has an invalid exact schema");
    let state = match &*state {
        V::Null => return Ok(()),
        V::Map(state) => state,
        _ => return schema(),
    };
    let rows = view.rows()?;
    if !keys_are(state, &["schema_version", "policy_version", "source_id", "config", "staff"]) || !eq_int(state.get("schema_version"), 1) {
        return schema();
    }
    let V::Map(staff) = get(state, "staff") else {
        return schema();
    };
    if staff.len() != rows.staff.len() || !staff.keys().all(|id| rows.staff.contains_key(id)) {
        return schema();
    }
    let config = canonical_staff_dynamics_config(get(state, "config"))?;
    if !eq(get(state, "policy_version"), get(&config, "policy_version")) || !eq(get(state, "source_id"), get(&config, "source_id")) {
        return value_error("staff_dynamics_v1 model identity does not match its config");
    }
    let saved = get(state, "config");
    let without = |names: &[&str]| -> V { V::Map(config.iter().filter(|(key, _)| !names.contains(&&***key)).map(|(key, value)| (key.clone(), value.clone())).collect()) };
    if !eq(&V::Map(config.clone()), saved)
        && !(eq(&without(&["max_continuous_work_minutes_by_staff"]), saved)
            || py::eq_str(config.get("policy_version"), POLICY_V1)
                && eq(&without(&["shift_windows_by_staff", "off_shift_fatigue_recovery_per_minute", "max_continuous_work_minutes_by_staff"]), saved))
    {
        return value_error("staff_dynamics_v1 config is not canonical");
    }
    let known = |names: &V| matches!(names, V::Map(names) if names.keys().all(|id| rows.staff.contains_key(id)));
    if !known(get(&config, "shift_windows_by_staff")) {
        return value_error("staff_dynamics shift roster references unknown staff");
    }
    if !known(get(&config, "max_continuous_work_minutes_by_staff")) {
        return value_error("staff_dynamics per-staff work limits reference unknown staff");
    }
    for (staff_id, record) in staff {
        let V::Map(record) = record else {
            return Err(Error::Value(format!("staff_dynamics_v1 record for {} is invalid", py_repr(staff_id))));
        };
        if !keys_are(record, &RECORD_FIELDS) {
            return Err(Error::Value(format!("staff_dynamics_v1 record for {} is invalid", py_repr(staff_id))));
        }
        for name in ["consecutive_work_minutes", "total_work_minutes", "total_rest_minutes", "rest_until_minute"] {
            let value = get(record, name);
            if !py::is_exact_int(value) || py::negative(value) {
                return Err(Error::Value(format!("staff_dynamics_v1 {name} for {} is invalid", py_repr(staff_id))));
            }
        }
        if !py::finite_within(get(record, "fatigue"), 0.0, 1.0) {
            return Err(Error::Value(format!("staff_dynamics_v1 fatigue for {} is invalid", py_repr(staff_id))));
        }
        if !py::in_texts(get(record, "last_activity"), Held::Tracked, &["working", "resting", "off_duty"])? {
            return Err(Error::Value(format!("staff_dynamics_v1 activity for {} is invalid", py_repr(staff_id))));
        }
    }
    Ok(())
}
