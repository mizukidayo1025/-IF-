//! What one controller keeps beside its World: one per Run, continuation or Branch (Issue #152,
//! lane 7-3; policy E7 of docs/rust-port/step5-plan.md, design-p7.md principle 1).
//!
//! `RunConfig` is read once, when the controller starts, from what it read of `model_configs`; the
//! route graph (`Routes`) is built from the world's tables at each of the controller's
//! `_refresh_space`. Neither is state: nothing
//! saves, hashes or compares them, and a new controller over the same world builds them again.

use std::collections::HashMap;
use std::sync::Arc;

use crate::error::{Error, Result};
use crate::routes::Routes;

/// One value of `max_agents_in_transit_by_edge`, as a rule reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Capacity {
    /// None: the edge has no limit.
    Unlimited,
    Max(i64),
    /// A value the kernel turns down when it reads it, with the refusal's text.
    Refused(&'static str),
}

/// `controller.route_capacities`: `layout_routing.max_agents_in_transit_by_edge`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RouteCapacities {
    /// By edge ID; keys that are not text are left out, as no edge ID finds them.
    pub by_edge: HashMap<Arc<str>, Capacity>,
    /// Whether the table has any key at all: then every route counts the trips under way.
    pub any: bool,
    /// Whether a key was left out: one no edge of `world.zone_edges` can be, which `ZoneGraph`
    /// turns down (`Router::of_tables`).
    pub unmatched_keys: bool,
}

impl RouteCapacities {
    /// The capacity of an edge, None where the table has none.
    pub fn of(&self, edge: &str) -> Result<Option<i64>> {
        match self.by_edge.get(edge) {
            None | Some(Capacity::Unlimited) => Ok(None),
            Some(Capacity::Max(capacity)) => Ok(Some(*capacity)),
            Some(Capacity::Refused(reason)) => Err(Error::unsupported(*reason)),
        }
    }
}

/// The configs of a Run, read when its controller starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunConfig {
    /// The table, or the refusal of a value that is not a table, raised where a route is asked.
    pub route_capacities: std::result::Result<Arc<RouteCapacities>, &'static str>,
}

impl RunConfig {
    /// The route capacities, or the refusal they were read with.
    pub fn route_capacities(&self) -> Result<&Arc<RouteCapacities>> {
        self.route_capacities.as_ref().map_err(|reason| Error::unsupported(*reason))
    }
}

/// What a controller keeps beside its World.
pub struct Run {
    pub config: RunConfig,
    pub routes: Routes,
}

impl Run {
    pub fn new(config: RunConfig) -> Self {
        Self { config, routes: Routes::default() }
    }
}
