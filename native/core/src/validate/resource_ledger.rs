//! the resource ledger block of `WorldState.validate`: the identity history of `population_dynamics_v1`, then `validate_resource_ledger` (with the history: the whole recount) or `validate_resource_ledger_state` (without) of `domain/resources.py`. `validate::world` calls it only where `resource_ledger_v1` is in `model_state`, with Python's errors and texts.
//!
//! Lane F of #250 (#351) ports it; until then it passes (`STUB(lane F #351)`). Raw errors: `validate::world` makes a `ValueError` (a `ResourceLedgerError` too) `invalid resource ledger: {exc}`.

use super::View;
use crate::error::Result;

/// The check, as `validate` (`validate/mod.rs`) calls it in Python's order.
pub fn validate(_view: &View<'_>) -> Result<()> {
    // STUB(lane F #351)
    Ok(())
}
