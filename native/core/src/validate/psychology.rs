//! The psychological distress block of `WorldState.validate`: `validate_psychological_state`
//! (`domain/psychological_state.py`) where a mean-reverting psychology model is selected, and the
//! state without its model, with Python's errors and texts. It raises the `StateInvariantError`s of
//! `WorldState.validate` itself, its own wrapping included.

use super::catalog::catalog;
use super::py::{equal, finite, unit_interval};
use super::{wrap_value, View};
use crate::config::Selection;
use crate::error::{Error, Result};
use crate::value::V;

const PSYCHOLOGY: &str = "psychological_distress_v1";
const FIELDS: [&str; 6] =
    ["baseline_distress", "long_term_distress", "daily_acute_sum", "daily_high_distress_minutes", "daily_observed_minutes", "completed_days"];

/// The check, as `validate` (`validate/mod.rs`) calls it in Python's order, with the models
/// `selected_models` gave.
pub fn validate(view: &View<'_>, selection: &Selection) -> Result<()> {
    let model = selection.get("psychology").map(|model| &**model).unwrap_or_default();
    if catalog()?.is_mean_reverting(model) {
        let state = view.state(PSYCHOLOGY)?;
        return validate_state(state.as_deref()).map_err(wrap_value("invalid psychological distress state: "));
    }
    if view.contains(PSYCHOLOGY)? {
        return Err(Error::Invariant("psychological distress state requires its registered model".to_owned()));
    }
    Ok(())
}

fn value_error(text: &str) -> Error {
    Error::Value(text.to_owned())
}

/// `type(value) is int` and its value, for an int within i64.
fn int(value: &V) -> Option<i64> {
    match value {
        V::Int(number) => Some(*number),
        _ => None,
    }
}

/// `validate_psychological_state(state)`; None for an absent namespace.
pub fn validate_state(state: Option<&V>) -> Result<()> {
    let state = match state {
        Some(V::Map(state)) if state.len() == 3 && ["schema_version", "last_updated_minute", "agents"].iter().all(|key| state.contains_key(*key)) => state,
        _ => return Err(value_error("psychological distress state fields do not match schema v1")),
    };
    if !equal(&state["schema_version"], &V::Int(1)) {
        return Err(value_error("unsupported psychological distress state schema"));
    }
    let minute_ok = match &state["last_updated_minute"] {
        V::Int(minute) => *minute >= -1,
        V::Big(text) => !text.starts_with('-'),
        _ => false,
    };
    if !minute_ok {
        return Err(value_error("psychological distress last minute is invalid"));
    }
    let agents = match &state["agents"] {
        V::Map(agents) if agents.len() <= 100_000 => agents,
        _ => return Err(value_error("psychological distress agents must be a bounded object")),
    };
    for (agent_id, record) in agents {
        let record = match record {
            V::Map(record) if !agent_id.is_empty() && record.len() == FIELDS.len() && FIELDS.iter().all(|key| record.contains_key(*key)) => record,
            _ => return Err(value_error("psychological distress agent record is invalid")),
        };
        unit_interval("baseline_distress", &record["baseline_distress"])?;
        unit_interval("long_term_distress", &record["long_term_distress"])?;
        let Some(total) = finite(&record["daily_acute_sum"])? else {
            return Err(value_error("psychological distress daily integral must be finite"));
        };
        let observed = int(&record["daily_observed_minutes"]).filter(|observed| (0..1440).contains(observed));
        let Some(observed) = observed else {
            return Err(value_error("psychological distress daily observed minutes are invalid"));
        };
        if !int(&record["daily_high_distress_minutes"]).is_some_and(|high| (0..=observed).contains(&high)) {
            return Err(value_error("psychological distress high minutes are invalid"));
        }
        let days_ok = match &record["completed_days"] {
            V::Int(days) => *days >= 0,
            V::Big(text) => !text.starts_with('-'),
            _ => false,
        };
        if !days_ok || total < 0.0 || total > observed as f64 + 1e-9 {
            return Err(value_error("psychological distress daily accumulation is invalid"));
        }
    }
    Ok(())
}
