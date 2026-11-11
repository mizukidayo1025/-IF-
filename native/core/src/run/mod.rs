//! What one controller keeps beside its World: one per Run, continuation or Branch (Issue #152,
//! lane 7-3; policy E7 of docs/rust-port/step5-plan.md, design-p7.md principle 1).
//!
//! `RunConfig` is read once, when the controller starts, from what it read of `model_configs`; the
//! route graph (`Routes`) is built from the world's tables at each of the controller's
//! `_refresh_space`. `Policies` (lane P8-0b) is what the rules read of the controller's policies and
//! managers, read once when the controller starts (`policies.rs`); `Scratch` holds what one tick's
//! phases hand on (`scratch.rs`); `actions` the open action of each agent (lane P8 a2), which the
//! controller keeps from tick to tick. None of them is state: nothing saves, hashes or compares them,
//! and a new controller over the same world builds them again.
//!
//! Each part of `Policies` has its own file here, so that lanes add theirs without touching
//! another's.

pub mod behavior;
pub mod dispatch;
pub mod held;
pub mod information;
pub mod policies;
pub mod proximity;
pub mod psychology;
pub mod scratch;
pub mod services;
pub mod sinks;
pub mod social;
pub mod stress;

pub use policies::Policies;
pub use scratch::Scratch;

use std::collections::HashMap;
use std::sync::Arc;

use crate::error::{Error, Result};
use crate::routes::Routes;
use crate::rules::action_lifecycle::ActiveActions;
use crate::rules::world_view::ViewCache;

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
    /// Shared with the native `Host`, which answers from it without the Run's lock.
    pub policies: Arc<Policies>,
    /// Not state: the values of the tick under way.
    pub scratch: Scratch,
    /// The open action of each agent (lane P8 a2). Not saved: a restored Run makes it again from
    /// the event log (`active_action_index`), and the PyO3 crate's `ActiveActions` loads it.
    pub actions: ActiveActions,
    /// Not state: what the view keeps from one view to the next (lane P8 a1), by the stamps of what
    /// it was read from.
    pub views: ViewCache,
}

impl Run {
    /// A Run without a controller's policies: every read of them refuses.
    pub fn new(config: RunConfig) -> Self {
        Self::with_policies(config, Policies::absent())
    }

    pub fn with_policies(config: RunConfig, policies: Policies) -> Self {
        Self {
            config,
            routes: Routes::default(),
            policies: Arc::new(policies),
            scratch: Scratch::default(),
            actions: ActiveActions::default(),
            views: ViewCache::default(),
        }
    }
}
