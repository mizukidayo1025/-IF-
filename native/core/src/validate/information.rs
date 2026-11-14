//! the `information_spread` block of `WorldState.validate`: `InformationSpreadConfig.from_mapping`, its canonical form, and `validate_information_runtime_state` (`domain/information.py`), with Python's errors and texts.
//!
//! Lane B of #250 (#347) ports it; until then it passes (`STUB(lane B #347)`). It raises the `StateInvariantError`s of `WorldState.validate` itself, its own wrapping included.

use super::View;
use crate::error::Result;

/// The check, as `validate` (`validate/mod.rs`) calls it in Python's order.
pub fn validate(_view: &View<'_>) -> Result<()> {
    // STUB(lane B #347)
    Ok(())
}
