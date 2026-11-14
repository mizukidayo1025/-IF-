//! `validate_declared_states(world)` of `contracts/runtime.py`, with Python's errors and texts.
//!
//! Lane B of #250 (#347) ports it; until then it passes (`STUB(lane B #347)`). `validate::world` makes its `ValueError` a `StateInvariantError(str(exc))`.

use super::View;
use crate::error::Result;

/// The check, as `validate` (`validate/mod.rs`) calls it in Python's order.
pub fn validate(_view: &View<'_>) -> Result<()> {
    // STUB(lane B #347)
    Ok(())
}
