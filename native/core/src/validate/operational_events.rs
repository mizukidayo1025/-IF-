//! `validate_operational_events_state(world, in_flight_event=..., validate_event_history=...)` of `domain/operational.py`, with Python's errors and texts.
//!
//! No lane of #250 owns it yet; until then it passes (`STUB(unassigned)`). Raw errors: `validate::world` makes a `ValueError` `invalid operational events state: {exc}`.

use super::View;
use crate::error::Result;

/// The check, as `validate` (`validate/mod.rs`) calls it in Python's order.
pub fn validate(_view: &View<'_>) -> Result<()> {
    // STUB(unassigned)
    Ok(())
}
