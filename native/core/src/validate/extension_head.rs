//! the head of `validate_simulator_extensions` (`domain/simulator_state.py`): `temporary_spaces_v1` without its contract, the legacy queue marker, and the required states that are missing, with Python's errors and texts.
//!
//! Lane D of #250 (#349) ports it; until then it passes (`STUB(lane D #349)`). Raw errors: `validate::extensions` calls it first.

use super::View;
use crate::error::Result;

/// The check, as `validate` (`validate/mod.rs`) calls it in Python's order.
pub fn validate(_view: &View<'_>) -> Result<()> {
    // STUB(lane D #349)
    Ok(())
}
