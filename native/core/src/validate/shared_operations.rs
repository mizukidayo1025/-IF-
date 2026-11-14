//! `validate_shared_operations_state(world, validate_event_history=...)` of `domain/common_operations.py`, its history included, with Python's errors and texts.
//!
//! Lane E of #250 (#350) ports it; until then it passes (`STUB(lane E #350)`). Raw errors. The check before it (the namespace without its signed contract) is `validate::extensions`'s.

use super::View;
use crate::error::Result;

/// The check, as `validate` (`validate/mod.rs`) calls it in Python's order.
pub fn validate(_view: &View<'_>) -> Result<()> {
    // STUB(lane E #350)
    Ok(())
}
