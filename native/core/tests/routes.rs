//! The route graph of lane 7-3 without Python: `ShortestTravelTimePolicy.route` of
//! `policies/space.py` over a `Router`, the edges a floor plan closes, a Run's routers by epoch and
//! its route capacities.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use sheltersim_core::config::FacilityPlan;
use sheltersim_core::error::Error;
use sheltersim_core::geometry::{Geo, Pt, Route as Passage};
use sheltersim_core::routes::{Edge, EdgeLoads, Route, Router, Routes, Which};
use sheltersim_core::row::IdMap;
use sheltersim_core::run::{Capacity, RouteCapacities, Run, RunConfig};

fn id(text: &str) -> Arc<str> {
    Arc::from(text)
}

fn edge(name: &str, from: &str, to: &str, minutes: i64) -> Edge {
    Edge { id: id(name), from: id(from), to: id(to), minutes, capacity: None }
}

fn router(zones: &[&str], edges: Vec<Edge>) -> Router {
    Router::new(zones.iter().map(|zone| id(zone)), edges)
}

fn ids(route: &Route) -> (Vec<&str>, Vec<&str>, i64) {
    (route.zone_ids.iter().map(|zone| &**zone).collect(), route.edge_ids.iter().map(|edge| &**edge).collect(), route.minutes)
}

fn shortest(graph: &Router, origin: &str, destination: &str) -> Option<Route> {
    graph.shortest(&HashSet::new(), None, &id(origin), &id(destination)).expect("known zones")
}

#[test]
fn routes_take_the_least_minutes() {
    let graph = router(
        &["a", "b", "c", "d"],
        vec![edge("ab", "a", "b", 2), edge("bd", "b", "d", 2), edge("ac", "a", "c", 1), edge("cd", "c", "d", 5), edge("da", "d", "a", 1)],
    );
    assert_eq!(ids(&shortest(&graph, "a", "d").unwrap()), (vec!["a", "b", "d"], vec!["ab", "bd"], 4));
    // Edges are directed.
    assert_eq!(ids(&shortest(&graph, "d", "c").unwrap()), (vec!["d", "a", "c"], vec!["da", "ac"], 2));
    assert_eq!(ids(&shortest(&graph, "b", "c").unwrap()), (vec!["b", "d", "a", "c"], vec!["bd", "da", "ac"], 4));
    // Already there: no edge and no time.
    let within = shortest(&graph, "c", "c").unwrap();
    assert_eq!(within, Route::within(&id("c")));
    assert_eq!(ids(&within), (vec!["c"], vec![], 0));
    // No way out of a zone without edges.
    let lonely = router(&["a", "b", "z"], vec![edge("ab", "a", "b", 1)]);
    assert_eq!(shortest(&lonely, "z", "a"), None);
    assert_eq!(shortest(&lonely, "b", "a"), None);
    assert!(lonely.has_zone("z") && !lonely.has_zone("y"));
    // A zone the graph lacks is refused, as before, before anything else.
    for (origin, destination) in [("y", "a"), ("a", "y"), ("y", "y")] {
        assert_eq!(lonely.shortest(&HashSet::new(), None, &id(origin), &id(destination)), Err(Error::Unsupported("an unknown zone".into())));
    }
    let long = router(&["a", "b", "c"], vec![edge("ab", "a", "b", i64::MAX), edge("bc", "b", "c", 1)]);
    assert_eq!(long.shortest(&HashSet::new(), None, &id("a"), &id("c")), Err(Error::Unsupported("a route's minutes past 64 bits".into())));
}

#[test]
fn closed_and_full_edges_are_left_out() {
    let mut full = edge("ab", "a", "b", 1);
    full.capacity = Some(2);
    let graph = router(&["a", "b", "c"], vec![full, edge("ac", "a", "c", 2), edge("cb", "c", "b", 2)]);
    assert_eq!(ids(&shortest(&graph, "a", "b").unwrap()).1, vec!["ab"]);
    let closed: HashSet<Arc<str>> = [id("ab")].into();
    let route = graph.shortest(&closed, None, &id("a"), &id("b")).unwrap().unwrap();
    assert_eq!(ids(&route), (vec!["a", "c", "b"], vec!["ac", "cb"], 4));
    let all: HashSet<Arc<str>> = [id("ab"), id("cb")].into();
    assert_eq!(graph.shortest(&all, None, &id("a"), &id("b")).unwrap(), None);
    // `loads.get(edge_id, 0) >= capacity`: an edge of capacity 2 with two trips on it is full.
    let mut loads = EdgeLoads::new();
    loads.insert(id("ab"), 1);
    assert_eq!(ids(&graph.shortest(&HashSet::new(), Some(&loads), &id("a"), &id("b")).unwrap().unwrap()).1, vec!["ab"]);
    loads.insert(id("ab"), 2);
    assert_eq!(ids(&graph.shortest(&HashSet::new(), Some(&loads), &id("a"), &id("b")).unwrap().unwrap()).1, vec!["ac", "cb"]);
    // Loads on an edge without a capacity change nothing.
    loads.insert(id("ac"), 99);
    assert_eq!(ids(&graph.shortest(&HashSet::new(), Some(&loads), &id("a"), &id("b")).unwrap().unwrap()).1, vec!["ac", "cb"]);
    // Without loads the capacities count nothing.
    assert_eq!(ids(&graph.shortest(&HashSet::new(), None, &id("a"), &id("b")).unwrap().unwrap()).1, vec!["ab"]);
}

fn passage(width: f64, accessible: bool) -> Passage {
    Passage { points: vec![Pt { x: 0.0, y: 0.0 }, Pt { x: 1.0, y: 0.0 }], width, accessible }
}

#[test]
fn a_floor_plan_closes_edges_by_mobility() {
    let mut routes: IdMap<Passage> = IdMap::default();
    routes.insert(id("wide"), passage(2.0, true));
    routes.insert(id("narrow"), passage(0.6, true));
    routes.insert(id("steps"), passage(2.0, false));
    let plan = FacilityPlan { zones: IdMap::default(), routes, facility_points: IdMap::default(), service_points: Vec::new() };
    let geo = Arc::new(Geo::new(Some(&plan), None, None, None).unwrap());
    let graph = router(
        &["a", "b"],
        vec![edge("wide", "a", "b", 3), edge("narrow", "a", "b", 1), edge("steps", "a", "b", 2), edge("unplanned", "b", "a", 1)],
    );
    let set = |names: &[&str]| names.iter().map(|name| id(name)).collect::<HashSet<Arc<str>>>();
    // `_physically_closed_edges` reads the plan's routes; an edge the plan lacks is not in it.
    assert_eq!(*graph.closed_to(&geo, "independent").unwrap(), set(&[]));
    assert_eq!(*graph.closed_to(&geo, "assisted").unwrap(), set(&["narrow", "steps"]));
    assert_eq!(*graph.closed_to(&geo, "wheelchair").unwrap(), set(&["narrow", "steps"]));
    // `route_is_physically_clear` of each edge of the graph turns the unplanned one down too.
    assert_eq!(graph.edges_closed_by(&geo, "independent"), set(&["unplanned"]));
    assert_eq!(graph.edges_closed_by(&geo, "wheelchair"), set(&["narrow", "steps", "unplanned"]));
    let closed = graph.closed_to(&geo, "wheelchair").unwrap();
    assert_eq!(ids(&graph.shortest(&closed, None, &id("a"), &id("b")).unwrap().unwrap()).1, vec!["wide"]);
    let open = graph.closed_to(&geo, "independent").unwrap();
    assert_eq!(ids(&graph.shortest(&open, None, &id("a"), &id("b")).unwrap().unwrap()).1, vec!["narrow"]);
    // Kept while the plan is the same `Geo`; another plan is read again.
    assert!(Arc::ptr_eq(&graph.closed_to(&geo, "wheelchair").unwrap(), &closed));
    let other = Arc::new(Geo::new(Some(&plan), None, None, None).unwrap());
    let again = graph.closed_to(&other, "wheelchair").unwrap();
    assert!(!Arc::ptr_eq(&again, &closed));
    assert_eq!(*again, *closed);
    // No floor plan closes nothing.
    let none = Arc::new(Geo::new(None, None, None, None).unwrap());
    assert!(graph.closed_to(&none, "wheelchair").unwrap().is_empty());
}

/// A few draws of a fixed sequence (the graphs only need to be the same each run).
struct Draws(u64);

impl Draws {
    fn next(&mut self, below: u64) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) % below
    }
}

/// The least `(minutes, edge IDs, zone IDs)` of every path without a repeated zone, by brute force.
fn least_path(edges: &[Edge], origin: &str, destination: &str) -> Option<(i64, Vec<Arc<str>>, Vec<Arc<str>>)> {
    fn walk(
        edges: &[Edge],
        at: &str,
        destination: &str,
        path: &mut (i64, Vec<Arc<str>>, Vec<Arc<str>>),
        best: &mut Option<(i64, Vec<Arc<str>>, Vec<Arc<str>>)>,
    ) {
        if at == destination {
            if best.as_ref().is_none_or(|known| *path < *known) {
                *best = Some(path.clone());
            }
            return;
        }
        for edge in edges.iter().filter(|edge| &*edge.from == at) {
            if path.2.contains(&edge.to) {
                continue;
            }
            path.0 += edge.minutes;
            path.1.push(edge.id.clone());
            path.2.push(edge.to.clone());
            walk(edges, &edge.to, destination, path, best);
            path.0 -= edge.minutes;
            path.1.pop();
            path.2.pop();
        }
    }
    let mut best = None;
    walk(edges, origin, destination, &mut (0, Vec::new(), vec![id(origin)]), &mut best);
    best
}

#[test]
fn equal_minutes_go_to_the_least_path_whatever_the_edge_order() {
    let zones = ["z0", "z1", "z2", "z3", "z4", "z5"];
    let mut draws = Draws(7);
    let mut ties = 0;
    for _ in 0..60 {
        // Few distinct minutes, so many routes tie; edge IDs drawn so that their order and the zones'
        // order disagree.
        let mut edges = Vec::new();
        for number in 0..(6 + draws.next(9)) {
            let from = zones[draws.next(6) as usize];
            let to = zones[draws.next(6) as usize];
            if from == to {
                continue;
            }
            let name = format!("e{}-{number}", draws.next(4));
            edges.push(edge(&name, from, to, 1 + draws.next(2) as i64));
        }
        let mut orders = vec![edges.clone()];
        let mut reversed = edges.clone();
        reversed.reverse();
        orders.push(reversed);
        for _ in 0..6 {
            let mut shuffled = edges.clone();
            for index in (1..shuffled.len()).rev() {
                shuffled.swap(index, draws.next(index as u64 + 1) as usize);
            }
            orders.push(shuffled);
        }
        let graphs: Vec<Router> = orders.into_iter().map(|order| router(&zones, order)).collect();
        for origin in zones {
            for destination in zones {
                let found: Vec<Option<Route>> = graphs.iter().map(|graph| shortest(graph, origin, destination)).collect();
                assert!(found.windows(2).all(|pair| pair[0] == pair[1]), "{origin} -> {destination}: {found:?}");
                if origin == destination {
                    continue;
                }
                let least = least_path(&edges, origin, destination);
                let route = found[0].as_ref().map(|route| (route.minutes, route.edge_ids.clone(), route.zone_ids.clone()));
                assert_eq!(route, least, "{origin} -> {destination}");
                // Count the pairs where another path of the same minutes lost on its IDs.
                if let Some((minutes, ..)) = &least {
                    let mut others = edges.clone();
                    others.retain(|edge| route.as_ref().is_some_and(|(_, ids, _)| !ids.contains(&edge.id)));
                    if least_path(&others, origin, destination).is_some_and(|other| other.0 == *minutes) {
                        ties += 1;
                    }
                }
            }
        }
    }
    assert!(ties > 50, "only {ties} ties were tried");
}

#[test]
fn a_run_keeps_one_router_of_each_kind_per_epoch() {
    let mut routes = Routes::default();
    assert_eq!(routes.epoch(), 0);
    assert!(routes.get(Which::People).is_none());
    routes.refresh();
    assert_eq!(routes.epoch(), 1);
    let people = Arc::new(router(&["a"], Vec::new()));
    let kept = routes.keep(Which::People, 1, people.clone());
    assert!(Arc::ptr_eq(&kept, &people));
    assert!(Arc::ptr_eq(&routes.get(Which::People).unwrap(), &people));
    assert!(routes.get(Which::Staff).is_none());
    assert_eq!((routes.built(Which::People), routes.built(Which::Staff)), (Some(1), None));
    let staff = Arc::new(router(&["a"], Vec::new()));
    routes.keep(Which::Staff, 1, staff.clone());
    assert!(Arc::ptr_eq(&routes.get(Which::Staff).unwrap(), &staff));
    // A refresh drops both; a router built from the graph before it is not kept.
    routes.refresh();
    assert_eq!(routes.epoch(), 2);
    assert!(routes.get(Which::People).is_none() && routes.get(Which::Staff).is_none());
    let late = routes.keep(Which::People, 1, people.clone());
    assert!(Arc::ptr_eq(&late, &people));
    assert_eq!(routes.built(Which::People), None);
    routes.keep(Which::People, 2, people.clone());
    assert_eq!(routes.built(Which::People), Some(2));
}

#[test]
fn route_capacities_refuse_where_they_are_read() {
    let mut by_edge = HashMap::new();
    by_edge.insert(id("open"), Capacity::Unlimited);
    by_edge.insert(id("two"), Capacity::Max(2));
    by_edge.insert(id("text"), Capacity::Refused("a value is not an int"));
    let table = RouteCapacities { by_edge, any: true, unmatched_keys: false };
    assert_eq!(table.of("open"), Ok(None));
    assert_eq!(table.of("missing"), Ok(None));
    assert_eq!(table.of("two"), Ok(Some(2)));
    assert_eq!(table.of("text"), Err(Error::Unsupported("a value is not an int".into())));
    let run = Run::new(RunConfig { route_capacities: Ok(Arc::new(table.clone())) });
    assert_eq!(**run.config.route_capacities().unwrap(), table);
    assert_eq!(run.routes.epoch(), 0);
    let refused = RunConfig { route_capacities: Err("a value is not a plain dict") };
    assert_eq!(refused.route_capacities(), Err(Error::Unsupported("a value is not a plain dict".into())));
}
