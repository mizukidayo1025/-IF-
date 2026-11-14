//! `selected_models(world.model_configs)` of `contracts/policies.py`, with Python's errors and texts:
//! the models the configs select, which `validate` reads (the psychology model) and the extensions'
//! signatures read.
//!
//! Lane B of #250 (#347) ports it; until then it selects nothing (`STUB(lane B #347)`): a reader
//! gets None for every domain.

use super::View;
use crate::config::Selection;
use crate::error::Result;

/// The selection, or the `ValueError` Python raises for the configs.
pub fn selected_models(_view: &View<'_>) -> Result<Selection> {
    // STUB(lane B #347)
    Ok(Selection::default())
}
