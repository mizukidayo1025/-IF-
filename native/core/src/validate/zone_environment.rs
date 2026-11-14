//! `validate_zone_environment_runtime_state(world, validate_event_history=...)` of `domain/environment.py`, with Python's errors and texts.
//!
//! Lane F of #250 (#351) ports it; until then it passes (`STUB(lane F #351)`). Raw errors: `validate::world` makes a `ValueError` `invalid zone environment state: {exc}`.

use super::View;
use crate::error::Result;

/// The check, as `validate` (`validate/mod.rs`) calls it in Python's order.
pub fn validate(_view: &View<'_>) -> Result<()> {
    // STUB(lane F #351)
    Ok(())
}
