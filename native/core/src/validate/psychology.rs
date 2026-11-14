//! The psychological distress block of `WorldState.validate`: `validate_psychological_state`
//! (`domain/psychological_state.py`) where a mean-reverting psychology model is selected, and the
//! state without its model, with Python's errors and texts.
//!
//! Lane B of #250 (#347) ports it; until then it passes (`STUB(lane B #347)`). It raises the
//! `StateInvariantError`s of `WorldState.validate` itself, its own wrapping included.

use super::View;
use crate::config::Selection;
use crate::error::Result;

/// The check, as `validate` (`validate/mod.rs`) calls it in Python's order, with the models
/// `selected_models` gave.
pub fn validate(_view: &View<'_>, _selection: &Selection) -> Result<()> {
    // STUB(lane B #347)
    Ok(())
}
