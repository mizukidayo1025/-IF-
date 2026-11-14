//! `validate_body_state(world, validate_event_history=...)` of `domain/body_state.py`, its history included, with Python's errors and texts.
//!
//! Lane D of #250 (#349) ports it; until then it passes (`STUB(lane D #349)`). Raw errors.

use super::View;
use crate::error::Result;

/// The check, as `validate` (`validate/mod.rs`) calls it in Python's order.
pub fn validate(_view: &View<'_>) -> Result<()> {
    // STUB(lane D #349)
    Ok(())
}
