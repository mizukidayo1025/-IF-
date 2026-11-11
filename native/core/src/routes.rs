//! The route graph of a Run without Python: `ShortestTravelTimePolicy.route` of
//! `policies/space.py` over a snapshot of the zone graph (Issue #152, lane 7-3; design-p7.md,
//! principle 4).
//!
//! A `Router` is built from the world's zone and zone-edge tables (`Entities::zones`,
//! `zone_edges`) with the checks and the words of `ZoneGraph` (`domain/zones.py`; lane 7-4), and
//! `Routes` keeps a Run's two routers (people's and staff's) for the epoch they were built in. The
//! epoch changes exactly where `_refresh_space` runs (`experiments/runner.py`): at the
//! controller's start and after a dispatch that applied a geometry or structure event, and the
//! routers are built from the tables right then. It is a snapshot on purpose: an edge closed later
//! in the same dispatch is not seen until the next refresh, as the Python graph never saw it.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use crate::entities::{EdgeRow, Table, ZoneRow};
use crate::error::{Error, Result};
use crate::geometry::Geo;
use crate::run::{Capacity, RouteCapacities};

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

/// `_STABLE_ID` of `domain/zones.py`: `^[A-Za-z0-9][A-Za-z0-9._:-]{0,63}$`, which only ASCII
/// matches, so its length in characters is its length in bytes.
pub fn is_stable_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    (1..=64).contains(&bytes.len())
        && bytes[0].is_ascii_alphanumeric()
        && bytes[1..].iter().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

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

    /// `ZoneGraph.from_states(zones, edges, edge_capacities=capacities)` of the tables, as a router
    /// of its open edges: what the constructor turns down raises its ValueError, checked in its
    /// order (the capacities' keys, then each zone in the dict's order, then each edge by ID).
    pub fn of_tables(zones: &Table<ZoneRow>, edges: &Table<EdgeRow>, capacities: &RouteCapacities) -> Result<Self> {
        // `set(capacities) - set(edges)`: by the dict's keys.
        if capacities.unmatched_keys || capacities.by_edge.keys().any(|key| edges.get(key).is_none()) {
            return Err(Error::value_error("route capacity references an unknown zone edge"));
        }
        if zones.is_empty() {
            return Err(Error::value_error("zone graph must contain at least one zone"));
        }
        for (key, zone) in zones.iter() {
            if **key != *zone.id || !is_stable_id(&zone.id) {
                return Err(Error::value_error("zone graph mapping keys must match stable zone IDs"));
            }
            // A stable ID is plain ASCII, whose `repr` is the ID in single quotes.
            if zone.capacity.is_some_and(|capacity| capacity < 0) {
                return Err(Error::value_error(format!("zone '{}' capacity cannot be negative", zone.id)));
            }
            if [zone.display_x, zone.display_y].into_iter().flatten().any(|coordinate| !coordinate.is_finite()) {
                return Err(Error::value_error(format!("zone '{}' has non-finite display coordinates", zone.id)));
            }
        }
        // `sorted(edges, key=lambda edge: edge.id)`: stable, and a str's order is its UTF-8 bytes'.
        let mut sorted: Vec<&EdgeRow> = edges.iter().map(|(_, edge)| edge).collect();
        sorted.sort_by(|first, second| first.id.as_bytes().cmp(second.id.as_bytes()));
        let mut seen: HashSet<&str> = HashSet::with_capacity(sorted.len());
        let mut open = Vec::with_capacity(sorted.len());
        for edge in sorted {
            if !is_stable_id(&edge.id) || !seen.insert(&edge.id) {
                return Err(Error::value_error("zone edge IDs must be stable and unique"));
            }
            if zones.get(&edge.from_zone_id).is_none() || zones.get(&edge.to_zone_id).is_none() {
                return Err(Error::value_error(format!("zone edge '{}' references an unknown zone", edge.id)));
            }
            if edge.from_zone_id == edge.to_zone_id {
                return Err(Error::value_error(format!("zone edge '{}' must connect different zones", edge.id)));
            }
            if edge.travel_minutes <= 0 {
                return Err(Error::value_error(format!("zone edge '{}' travel time must be a positive integer", edge.id)));
            }
            // `capacities.get(edge.id)`: by the edge's own ID.
            let capacity = match capacities.by_edge.get(&edge.id) {
                None | Some(Capacity::Unlimited) => None,
                Some(Capacity::Max(capacity)) if (1..=100).contains(capacity) => Some(*capacity),
                Some(_) => return Err(Error::value_error(format!("zone edge '{}' transit capacity must be from 1 to 100", edge.id))),
            };
            if !edge.closed {
                open.push(Edge { id: edge.id.clone(), from: edge.from_zone_id.clone(), to: edge.to_zone_id.clone(), minutes: edge.travel_minutes, capacity });
            }
        }
        Ok(Self::new(zones.iter().map(|(key, _)| key.clone()), open))
    }

    /// The graph `world_graph` of `rules/travel.rs` reads: the open edges of the
    /// tables in the dict's order, with these capacities (checked by the caller). An edge
    /// `ZoneGraph` turns down for its zones or its time is refused, in the dict's order.
    pub fn of_world_tables(zones: &Table<ZoneRow>, edges: &Table<EdgeRow>, capacities: &HashMap<Arc<str>, i64>) -> Result<Self> {
        let mut open = Vec::with_capacity(edges.len());
        for (_, edge) in edges.iter() {
            let (from, to) = (&edge.from_zone_id, &edge.to_zone_id);
            if zones.get(from).is_none() || zones.get(to).is_none() || from == to || edge.travel_minutes <= 0 {
                return Err(Error::unsupported("an edge the zone graph turns down"));
            }
            if !edge.closed {
                let capacity = capacities.get(&edge.id).copied();
                open.push(Edge { id: edge.id.clone(), from: from.clone(), to: to.clone(), minutes: edge.travel_minutes, capacity });
            }
        }
        Ok(Self::new(zones.iter().map(|(key, _)| key.clone()), open))
    }

    /// The graph of the same zones with only the edges `keep` takes: the staff's graph of a floor
    /// plan, whose edges are those `route_is_physically_clear` takes for an independent walker.
    pub fn keeping(&self, keep: impl Fn(&Edge) -> bool) -> Self {
        let outgoing = self
            .outgoing
            .iter()
            .map(|(zone, edges)| (zone.clone(), edges.iter().filter(|edge| keep(edge)).cloned().collect()))
            .collect();
        Self { zones: self.zones.clone(), outgoing, closed: Mutex::default() }
    }

    /// Whether the graph has this zone.
    pub fn has_zone(&self, zone: &str) -> bool {
        self.zones.contains(zone)
    }

    /// The open edges, by ID: for tests.
    pub fn edge_ids(&self) -> Vec<Arc<str>> {
        let mut ids: Vec<Arc<str>> = self.outgoing.values().flatten().map(|edge| edge.id.clone()).collect();
        ids.sort();
        ids
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
