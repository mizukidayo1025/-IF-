//! `simulator_signatures(world.model_configs)` of `domain/simulator_state.py`, with Python's errors
//! and texts: the signed extension implementations the configs enable, by key, in the order of
//! the Python dict.
//!
//! `validate::world` asks it whether to validate the extensions at all (a world without any
//! extension key), and raises its error as it is; the head of the extensions
//! (`extension_head.rs`) reads the required states from it.
//!
//! Lane D of #250 (#349) ports it; until then it signs nothing (`STUB(lane D #349)`).

use std::sync::Arc;

use super::View;
use crate::error::Result;

/// The signatures: each key (`"movement_v1"`, `"simulator:..."`, `"operator:..."`) and its value.
pub type Signatures = Vec<(Arc<str>, Arc<str>)>;

/// The signatures, or the `ValueError` Python raises for the configs.
pub fn simulator_signatures(_view: &View<'_>) -> Result<Signatures> {
    // STUB(lane D #349)
    Ok(Vec::new())
}
