//! `WorldState._validate_event_rules`: the ID counters, `EventQueue.validate`, `EventLog.validate` (with the history) and the queue against the log, with Python's errors and texts.
//!
//! Lane B of #250 (#347) ports it; until then it passes (`STUB(lane B #347)`). It raises the `StateInvariantError`s of `WorldState.validate` itself.

use super::View;
use crate::error::Result;

/// The check, as `validate` (`validate/mod.rs`) calls it in Python's order.
pub fn validate(_view: &View<'_>) -> Result<()> {
    // STUB(lane B #347)
    Ok(())
}
