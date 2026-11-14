//! `validate_temporary_spaces_state(world)` of `domain/temporary_spaces.py`, with Python's errors
//! and texts (lane E of #250, #350). Raw errors: `validate_simulator_extensions` passes them on.

use std::cmp::Ordering;

use super::movement::py::{cmp_i64, eq, has_exactly, in_keys, in_texts, int_cmp, is_exact_int, is_text, Held};
use super::View;
use crate::error::{Error, Result};
use crate::text::py_repr;
use crate::value::V;

const KEY: &str = "temporary_spaces_v1";
const POLICY: &str = "typed-temporary-capacity-v1";
const MAX_SPACES: usize = 256;
const MAX_HISTORY: usize = 20_000;
const STATE_FIELDS: [&str; 4] = ["schema_version", "policy_version", "spaces", "history"];
const SPACE_FIELDS: [&str; 13] = [
    "space_id",
    "zone_id",
    "people_capacity",
    "sleeping_capacity",
    "start_minute",
    "end_minute",
    "active",
    "start_event_id",
    "end_event_id",
    "program_id",
    "effect_id",
    "source_ids",
    "geometry_status",
];
const HISTORY_FIELDS: [&str; 6] = ["event_id", "space_id", "zone_id", "minute", "transition", "causation_id"];

/// The check, as `validate` (`validate/mod.rs`) calls it in Python's order.
pub fn validate(view: &View<'_>) -> Result<()> {
    let Some(state) = view.state(KEY)? else {
        return Ok(());
    };
    let schema = || Error::Value("temporary_spaces_v1 has an invalid exact schema".to_owned());
    let map = match &*state {
        V::Null => return Ok(()),
        V::Map(map) => map,
        _ => return Err(schema()),
    };
    if !has_exactly(map, &STATE_FIELDS) || !eq(&map["schema_version"], &V::Int(1)) || !is_text(&map["policy_version"], POLICY) {
        return Err(schema());
    }
    let V::Map(spaces) = &map["spaces"] else {
        return Err(schema());
    };
    let V::List(history) = &map["history"] else {
        return Err(schema());
    };
    if spaces.len() > MAX_SPACES || history.len() > MAX_HISTORY {
        return Err(schema());
    }
    let rows = view.rows()?;
    let zone_known = |value: &V| in_keys(value, Held::Plain, |id| rows.zones.contains_key(id));
    for (space_id, row) in spaces {
        if !space_valid(space_id, row, &zone_known)? {
            return Err(Error::Value(format!("temporary space {} is invalid", py_repr(space_id))));
        }
    }
    for row in history {
        if !record_valid(row, &zone_known)? {
            return Err(Error::Value("temporary_spaces_v1 history record is invalid".to_owned()));
        }
    }
    Ok(())
}

/// The conditions of a space, in Python's order: false where one fails, or the error one raises.
fn space_valid(space_id: &str, row: &V, zone_known: &dyn Fn(&V) -> Result<bool>) -> Result<bool> {
    let V::Map(row) = row else {
        return Ok(false);
    };
    if !has_exactly(row, &SPACE_FIELDS) || !is_text(&row["space_id"], space_id) || !zone_known(&row["zone_id"])? {
        return Ok(false);
    }
    let people = &row["people_capacity"];
    if !is_exact_int(people) || cmp_i64(people, 1) == Some(Ordering::Less) || cmp_i64(people, 1000) == Some(Ordering::Greater) {
        return Ok(false);
    }
    let sleeping = &row["sleeping_capacity"];
    if !is_exact_int(sleeping) || cmp_i64(sleeping, 0) == Some(Ordering::Less) || int_cmp(sleeping, people) == Some(Ordering::Greater) {
        return Ok(false);
    }
    let (start, end) = (&row["start_minute"], &row["end_minute"]);
    if !is_exact_int(start) || !is_exact_int(end) || cmp_i64(start, 0) == Some(Ordering::Less) || int_cmp(end, start) != Some(Ordering::Greater) {
        return Ok(false);
    }
    let V::Bool(active) = row["active"] else {
        return Ok(false);
    };
    if !matches!(&row["start_event_id"], V::Str(id) if !id.is_empty()) {
        return Ok(false);
    }
    let end_event = &row["end_event_id"];
    if (active && !matches!(end_event, V::Null)) || (!active && !matches!(end_event, V::Str(id) if !id.is_empty())) {
        return Ok(false);
    }
    if !matches!(row["program_id"], V::Str(_)) || !matches!(row["effect_id"], V::Str(_)) {
        return Ok(false);
    }
    let V::List(sources) = &row["source_ids"] else {
        return Ok(false);
    };
    if sources.iter().any(|item| !matches!(item, V::Str(id) if !id.is_empty())) {
        return Ok(false);
    }
    Ok(is_text(&row["geometry_status"], "zone_only_capacity"))
}

/// The conditions of a history record, in Python's order.
fn record_valid(row: &V, zone_known: &dyn Fn(&V) -> Result<bool>) -> Result<bool> {
    let V::Map(row) = row else {
        return Ok(false);
    };
    if !has_exactly(row, &HISTORY_FIELDS) || !matches!(row["event_id"], V::Str(_)) || !matches!(row["space_id"], V::Str(_)) || !zone_known(&row["zone_id"])? {
        return Ok(false);
    }
    if !is_exact_int(&row["minute"]) || !in_texts(&row["transition"], Held::Plain, &["activated", "expired"])? {
        return Ok(false);
    }
    Ok(matches!(row["causation_id"], V::Null | V::Str(_)))
}
