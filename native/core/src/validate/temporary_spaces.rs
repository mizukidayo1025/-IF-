//! `validate_temporary_spaces_state(world)` of `domain/temporary_spaces.py`, with Python's errors and texts.
//!
//! Lane E of #250 (#350) ports it; until then it passes (`STUB(lane E #350)`). Raw errors.

use super::View;
use crate::error::Result;

/// The check, as `validate` (`validate/mod.rs`) calls it in Python's order.
pub fn validate(_view: &View<'_>) -> Result<()> {
    // STUB(lane E #350)
    Ok(())
}
