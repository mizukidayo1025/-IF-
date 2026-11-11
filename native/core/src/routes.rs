//! The route graph of a Run without Python: `ShortestTravelTimePolicy.route` of
//! `policies/space.py` over a snapshot of the zone graph (Issue #152, lane 7-3; design-p7.md,
//! principle 4).
//!
//! A `Router` is built from the `ZoneGraph` a controller's `_refresh_space` made
//! (`experiments/runner.py`), and `Routes` keeps a Run's two routers (people's and staff's) for the
//! epoch it was built in. The epoch changes exactly where `_refresh_space` runs: at the
//! controller's start and after a dispatch that applied a geometry or structure event. It is a
//! snapshot on purpose: an edge closed earlier in the same dispatch is not seen until the refresh,
//! as the Python graph never saw it.
//!
//! Lane 7-4 builds the router from the core's zone and edge tables instead.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use crate::error::{Error, Result};
use crate::geometry::Geo;

/// `edge_loads`: looked up, never walked, so its order decides nothing.
pub type EdgeLoads = HashMap<Arc<str>, i64>;

/// An open edge of the graph: closed ones, and ones whose capacity no trip fits, are left out by
/// whoever reads the graph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edge {
    pub id: Arc<str>,
    pub from: Arc<str>,
    pub to: Arc<str>,
    /// `travel_minutes`.
    pub minutes: i64,
    /// `max_agents_in_transit`.
    pub capacity: Option<i64>,
}

/// A route: `ZoneRoute` of a walk through the graph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Route {
    pub origin: Arc<str>,
    pub destination: Arc<str>,
    pub zone_ids: Vec<Arc<str>>,
    pub edge_ids: Vec<Arc<str>>,
    pub minutes: i64,
}

impl Route {
    /// The route of a person already there.
    pub fn within(zone: &Arc<str>) -> Self {
        Self { origin: zone.clone(), destination: zone.clone(), zone_ids: vec![zone.clone()], edge_ids: Vec::new(), minutes: 0 }
    }
}

/// The edges closed to one mobility in one floor plan.
type ClosedTo = (&'static str, Arc<Geo>, Arc<HashSet<Arc<str>>>);

/// A zone graph to find routes in.
pub struct Router {
    zones: HashSet<Arc<str>>,
    /// Each zone's open edges, in the order they were given.
    outgoing: HashMap<Arc<str>, Vec<Edge>>,
    /// The edges closed to a mobility in a floor plan, kept while that plan is the same.
    closed: Mutex<Vec<ClosedTo>>,
}

/// `(minutes, edge IDs, zone IDs)`: a path's priority, compared as Python compares the tuples.
type Score = (i64, Vec<Arc<str>>, Vec<Arc<str>>);

impl Router {
    /// A graph of these zones and open edges.
    pub fn new(zones: impl IntoIterator<Item = Arc<str>>, edges: impl IntoIterator<Item = Edge>) -> Self {
        let mut outgoing: HashMap<Arc<str>, Vec<Edge>> = HashMap::new();
        for edge in edges {
            outgoing.entry(edge.from.clone()).or_default().push(edge);
        }
        Self { zones: zones.into_iter().collect(), outgoing, closed: Mutex::default() }
    }

    /// Whether the graph has this zone.
    pub fn has_zone(&self, zone: &str) -> bool {
        self.zones.contains(zone)
    }

    /// `_physically_closed_edges`: the routes of the plan `route_is_physically_clear` turns down for
    /// this mobility, kept while the plan is the same `Geo`.
    pub fn closed_to(&self, geo: &Arc<Geo>, mobility: &'static str) -> Result<Arc<HashSet<Arc<str>>>> {
        let mut known = self.closed.lock().map_err(|_| Error::unsupported("the closed edges are poisoned"))?;
        if let Some((_, _, closed)) = known.iter().find(|(name, plan, _)| *name == mobility && Arc::ptr_eq(plan, geo)) {
            return Ok(closed.clone());
        }
        let Some(layout) = &geo.layout else {
            return Ok(Arc::new(HashSet::new()));
        };
        let closed: HashSet<Arc<str>> = layout.routes.keys().filter(|id| !geo.route_is_clear(id, mobility)).cloned().collect();
        let closed = Arc::new(closed);
        known.retain(|(name, _, _)| *name != mobility);
        known.push((mobility, geo.clone(), closed.clone()));
        Ok(closed)
    }

    /// The edges of this graph `route_is_physically_clear` turns down, for a person of this mobility.
    pub fn edges_closed_by(&self, geo: &Geo, mobility: &str) -> HashSet<Arc<str>> {
        self.outgoing.values().flatten().filter(|edge| !geo.route_is_clear(&edge.id, mobility)).map(|edge| edge.id.clone()).collect()
    }

    /// `ShortestTravelTimePolicy.route`, without these edges and with these trips counted.
    ///
    /// The whole path is part of the priority, so of the routes of equal minutes the one of the
    /// least edge IDs, then zone IDs, wins whatever order the edges were given in: travel times
    /// are positive, so a path's priority only grows along it and the first one taken off the heap
    /// at the destination is the least of all.
    pub fn shortest(&self, closed: &HashSet<Arc<str>>, loads: Option<&EdgeLoads>, origin: &Arc<str>, destination: &Arc<str>) -> Result<Option<Route>> {
        if !self.zones.contains(origin) || !self.zones.contains(destination) {
            return Err(Error::unsupported("an unknown zone"));
        }
        if origin == destination {
            return Ok(Some(Route::within(origin)));
        }
        let start: Score = (0, Vec::new(), vec![origin.clone()]);
        let mut best: HashMap<Arc<str>, Score> = HashMap::new();
        best.insert(origin.clone(), start.clone());
        let mut pending = BinaryHeap::new();
        pending.push(Reverse((start, origin.clone())));
        while let Some(Reverse((score, current))) = pending.pop() {
            if best.get(&current) != Some(&score) {
                continue;
            }
            let (minutes, edge_ids, zone_ids) = score;
            if &current == destination {
                return Ok(Some(Route { origin: origin.clone(), destination: destination.clone(), zone_ids, edge_ids, minutes }));
            }
            let Some(edges) = self.outgoing.get(&current) else {
                continue;
            };
            for edge in edges {
                if closed.contains(&edge.id) {
                    continue;
                }
                if let (Some(capacity), Some(loads)) = (edge.capacity, loads) {
                    if loads.get(&edge.id).copied().unwrap_or(0) >= capacity {
                        continue;
                    }
                }
                let Some(total) = minutes.checked_add(edge.minutes) else {
                    return Err(Error::unsupported("a route's minutes past 64 bits"));
                };
                let mut next_edges = edge_ids.clone();
                next_edges.push(edge.id.clone());
                let mut next_zones = zone_ids.clone();
                next_zones.push(edge.to.clone());
                let candidate: Score = (total, next_edges, next_zones);
                if best.get(&edge.to).is_none_or(|known| candidate < *known) {
                    best.insert(edge.to.clone(), candidate.clone());
                    pending.push(Reverse((candidate, edge.to.clone())));
                }
            }
        }
        Ok(None)
    }
}

/// Which of a Run's routers: `controller.space`'s or `controller.staff_space`'s.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Which {
    /// People's routes (`_route`, `_schedule_travel`, the choice of a facility).
    People,
    /// Staff's routes (`assign_staff_operations`): with a floor plan, only the edges clear for an
    /// independent walker.
    Staff,
}

/// A Run's routers, each built at most once per epoch.
#[derive(Default)]
pub struct Routes {
    epoch: u64,
    people: Option<(u64, Arc<Router>)>,
    staff: Option<(u64, Arc<Router>)>,
}

impl Routes {
    /// How many times the graph was refreshed.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// `_refresh_space`: the routers built so far are of an old graph.
    pub fn refresh(&mut self) {
        self.epoch += 1;
        self.people = None;
        self.staff = None;
    }

    fn slot(&mut self, which: Which) -> &mut Option<(u64, Arc<Router>)> {
        match which {
            Which::People => &mut self.people,
            Which::Staff => &mut self.staff,
        }
    }

    /// The router of this epoch, if one was built.
    pub fn get(&self, which: Which) -> Option<Arc<Router>> {
        let held = match which {
            Which::People => &self.people,
            Which::Staff => &self.staff,
        };
        held.as_ref().filter(|(epoch, _)| *epoch == self.epoch).map(|(_, router)| router.clone())
    }

    /// The epoch the router was built in, if one is held.
    pub fn built(&self, which: Which) -> Option<u64> {
        match which {
            Which::People => &self.people,
            Which::Staff => &self.staff,
        }
        .as_ref()
        .map(|(epoch, _)| *epoch)
    }

    /// Keep the router built in `epoch`; one built before a refresh is dropped.
    pub fn keep(&mut self, which: Which, epoch: u64, router: Arc<Router>) -> Arc<Router> {
        if epoch == self.epoch {
            *self.slot(which) = Some((epoch, router.clone()));
        }
        router
    }
}
