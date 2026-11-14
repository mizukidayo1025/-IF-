//! `WorldState._validate_event_rules` (`core/world.py`), with Python's errors and texts: the ID
//! counters, `EventQueue.validate`, `EventLog.validate` (the audit of the realization times, with
//! the history), and the pending events against the log and the clock. It raises the
//! `StateInvariantError`s of `WorldState.validate` itself.
//!
//! The queue and the log of the core cannot hold an ID or a sequence twice (the loader refuses
//! such a document), so `EventQueue.validate`'s refusals and the typed containers' check are
//! unreachable here; they are kept as Python has them.

use super::py::type_name;
use super::View;
use crate::error::{Error, Result};
use crate::row::ValueRow;
use crate::value::V;
use crate::world::ID_COUNTERS;

fn invariant(text: impl Into<String>) -> Error {
    Error::Invariant(text.into())
}

/// `value < 0` (or `<=` with `operator`) of an ID counter: its sign, or Python's `TypeError` for a
/// value that does not compare with an int.
fn compare_error(operator: &str, value: &V) -> Error {
    Error::Type(format!("'{operator}' not supported between instances of '{}' and 'int'", type_name(value)))
}

fn below_zero(value: &V) -> Result<bool> {
    match value {
        V::Int(number) => Ok(*number < 0),
        V::Big(text) => Ok(text.starts_with('-')),
        V::Float(number) => Ok(*number < 0.0),
        V::Bool(_) => Ok(false),
        other => Err(compare_error("<", other)),
    }
}

/// The check, as `validate` (`validate/mod.rs`) calls it in Python's order.
pub fn validate(view: &View<'_>) -> Result<()> {
    let counters = view.world.slots.try_row::<ValueRow>(ID_COUNTERS)?;
    let V::Map(counters) = &counters.0 else {
        return Err(Error::unsupported("id_counters is not a dict"));
    };
    for value in counters.values() {
        if below_zero(value)? {
            return Err(invariant("ID counters cannot be negative"));
        }
    }
    let minute_index = view.minute_index()?;
    let duration = view.duration_minutes()?;
    {
        let queue = view.queue()?;
        match queue.validate() {
            1 => return Err(invariant("event queue contains duplicate event IDs")),
            2 => return Err(invariant("event queue contains duplicate event sequences")),
            _ => {}
        }
    }
    if view.history {
        match view.world.log_mut()?.audit(Some(minute_index), Some(duration)) {
            3 => return Err(invariant("event log cannot contain future realization times")),
            4 => return Err(invariant("event log is outside the scenario duration")),
            _ => {}
        }
    }
    let queue = view.queue()?;
    let log = view.log()?;
    let pending: Vec<_> = queue.ordered().iter().map(|(key, serial)| (*key, queue.slot(*serial).event_id.clone())).collect();
    if pending.iter().any(|(_, event_id)| log.index_of(event_id).is_some()) {
        return Err(invariant("an event cannot be both pending and realized"));
    }
    if pending.iter().any(|(key, _)| log.has_sequence(key.2)) {
        return Err(invariant("event sequences must be unique across queue and log"));
    }
    for (key, event_id) in &pending {
        if key.0 >= duration {
            return Err(invariant(format!("event {} is outside the scenario", crate::text::py_repr(event_id))));
        }
    }
    let highest = pending.iter().map(|(key, _)| key.2).max().unwrap_or(-1).max(log.max_sequence());
    let at_most = match counters.get("event").unwrap_or(&V::Int(0)) {
        V::Int(counter) => *counter <= highest,
        V::Big(text) => text.starts_with('-'),
        V::Float(counter) => *counter <= highest as f64,
        V::Bool(flag) => i64::from(*flag) <= highest,
        other => return Err(compare_error("<=", other)),
    };
    if at_most {
        return Err(invariant("event ID counter must be greater than scheduled sequences"));
    }
    Ok(())
}
