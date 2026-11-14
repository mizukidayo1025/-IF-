//! `WorldState._validate_entity_rules` and `_validate_zone_capacity` (`core/world.py`), with
//! Python's errors and texts: the clock, the seed, the entity maps, the zone edges, the route
//! capacities, the Agents, the zones' capacity, the facilities and their queues, the tasks and the
//! events that serve them, and the stocks.
//!
//! Lane C of #250 (#348) ports it; until then it passes (`STUB(lane C #348)`). It raises the
//! `StateInvariantError`s of `WorldState.validate` itself. `rules/tick_validation.rs` holds the
//! same rules without texts (pass or refuse), which may be a base.

use super::resource_service::ServiceConfig;
use super::View;
use crate::error::Result;

/// The check, as `validate` (`validate/mod.rs`) calls it in Python's order, with the resource
/// service's config where the configs have one (`resource_config`). Python returns the pending
/// events, which the event rules read; here they read the queue again.
pub fn validate(_view: &View<'_>, _service: Option<&ServiceConfig>) -> Result<()> {
    // STUB(lane C #348)
    Ok(())
}
