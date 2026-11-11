//! The routers of lane 7-4: built from the zone and zone-edge tables with `ZoneGraph`'s checks and
//! words (`domain/zones.py`), instead of from the Python graph a controller's `_refresh_space` made.
//!
//! The expected routes in `data/zone_graphs.json` are what the build of `main` (`70bae46`) gave
//! through the old snapshot path: `ZoneGraph.from_states` of the world's zones and edges with the
//! controller's route capacities, and `ShortestTravelTimePolicy` over `controller.space` and
//! `controller.staff_space` (whose floor plan keeps the edges `route_is_physically_clear` takes for
//! an independent walker). The worlds are the gate's floor plans (`benchmarks/gate_scenarios.py`):
//! the generated Scenario, with an edge closed, with a route of the plan too narrow, with route
//! capacities (and loads that fill one), and the fixtures' plan (all of `fixtures/scenarios` share
//! one). A script in the PR's notes made them.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde_json::Value;
use sheltersim_core::entities::{AnyTable, EdgeRow, Table, ZoneRow};
use sheltersim_core::error::Error;
use sheltersim_core::routes::{is_stable_id, EdgeLoads, Route, Router, Routes, Which};
use sheltersim_core::run::{Capacity, RouteCapacities};

fn id(text: &str) -> Arc<str> {
    Arc::from(text)
}

fn table<R: Clone>(rows: Vec<(&str, R)>) -> Table<R> {
    let mut table = Table::default();
    for (key, row) in rows {
        let slot = table.reserve_slot();
        table.fill_slot(slot, row);
        assert_eq!(table.bind(id(key), slot), None);
    }
    table
}

fn zone(name: &str) -> ZoneRow {
    ZoneRow { id: id(name), name: id(name), capacity: None, display_x: None, display_y: None }
}

fn edge(name: &str, from: &str, to: &str, minutes: i64) -> EdgeRow {
    EdgeRow { id: id(name), from_zone_id: id(from), to_zone_id: id(to), travel_minutes: minutes, closed: false }
}

fn zones(names: &[&str]) -> Table<ZoneRow> {
    table(names.iter().map(|name| (*name, zone(name))).collect())
}

fn edges(rows: Vec<EdgeRow>) -> Table<EdgeRow> {
    let keyed: Vec<(String, EdgeRow)> = rows.into_iter().map(|row| (row.id.to_string(), row)).collect();
    table(keyed.iter().map(|(key, row)| (key.as_str(), row.clone())).collect())
}

fn capacities(pairs: &[(&str, Capacity)]) -> RouteCapacities {
    RouteCapacities { by_edge: pairs.iter().map(|(edge, capacity)| (id(edge), *capacity)).collect(), any: !pairs.is_empty(), unmatched_keys: false }
}

fn none() -> RouteCapacities {
    RouteCapacities::default()
}

// ---- The gate's floor plans ---------------------------------------------------------------------

fn reference() -> Value {
    serde_json::from_str(include_str!("data/zone_graphs.json")).expect("the reference is JSON")
}

fn text(value: &Value) -> Arc<str> {
    id(value.as_str().expect("a str"))
}

/// The tables of a graph of the reference, in the dicts' order or reversed.
fn tables_of(graph: &Value, reversed: bool) -> (Table<ZoneRow>, Table<EdgeRow>) {
    let mut zone_rows: Vec<(String, ZoneRow)> = graph["zones"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            let float = |value: &Value| value.as_f64();
            let zone = ZoneRow { id: text(&row[1]), name: text(&row[2]), capacity: row[3].as_i64(), display_x: float(&row[4]), display_y: float(&row[5]) };
            (row[0].as_str().unwrap().to_owned(), zone)
        })
        .collect();
    let mut edge_rows: Vec<(String, EdgeRow)> = graph["edges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            let edge = EdgeRow {
                id: text(&row[1]),
                from_zone_id: text(&row[2]),
                to_zone_id: text(&row[3]),
                travel_minutes: row[4].as_i64().unwrap(),
                closed: row[5].as_bool().unwrap(),
            };
            (row[0].as_str().unwrap().to_owned(), edge)
        })
        .collect();
    if reversed {
        zone_rows.reverse();
        edge_rows.reverse();
    }
    (
        table(zone_rows.iter().map(|(key, row)| (key.as_str(), row.clone())).collect()),
        table(edge_rows.iter().map(|(key, row)| (key.as_str(), row.clone())).collect()),
    )
}

fn expected(value: &Value) -> Option<(i64, Vec<Arc<str>>, Vec<Arc<str>>)> {
    if value.is_null() {
        return None;
    }
    let ids = |list: &Value| list.as_array().unwrap().iter().map(text).collect::<Vec<_>>();
    Some((value[0].as_i64().unwrap(), ids(&value[1]), ids(&value[2])))
}

fn found(route: Option<Route>) -> Option<(i64, Vec<Arc<str>>, Vec<Arc<str>>)> {
    route.map(|route| (route.minutes, route.edge_ids, route.zone_ids))
}

/// Every route of the graph against the expected ones; how many pairs had one.
fn compare(name: &str, router: &Router, loads: Option<&EdgeLoads>, routes: &Value) -> usize {
    let mut reached = 0;
    for (pair, wanted) in routes.as_object().unwrap() {
        let (origin, destination) = pair.split_once('>').unwrap();
        let route = router.shortest(&HashSet::new(), loads, &id(origin), &id(destination)).expect("known zones");
        assert_eq!(found(route), expected(wanted), "{name}: {pair}");
        reached += usize::from(!wanted.is_null());
    }
    reached
}

#[test]
fn the_tables_give_the_routes_of_the_python_graph() {
    let reference = reference();
    let graphs = reference["graphs"].as_array().unwrap();
    assert_eq!(graphs.len(), 5);
    let mut staff_differs = false;
    let mut full_differs = false;
    for graph in graphs {
        let name = graph["name"].as_str().unwrap();
        let mut by_edge = HashMap::new();
        for (edge, capacity) in graph["capacities"].as_object().unwrap() {
            by_edge.insert(id(edge), Capacity::Max(capacity.as_i64().unwrap()));
        }
        let capacities = RouteCapacities { any: !by_edge.is_empty(), by_edge, unmatched_keys: false };
        let world_capacities: HashMap<Arc<str>, i64> =
            graph["capacities"].as_object().unwrap().iter().map(|(edge, capacity)| (id(edge), capacity.as_i64().unwrap())).collect();
        let staff_edges: HashSet<Arc<str>> = graph["staff_edges"].as_array().unwrap().iter().map(text).collect();
        for reversed in [false, true] {
            let (zones, edges) = tables_of(graph, reversed);
            let people = Router::of_tables(&zones, &edges, &capacities).expect("the gate's graphs are valid");
            let reached = compare(name, &people, None, &graph["people"]);
            assert!(reached > graph["zones"].as_array().unwrap().len(), "{name}: only {reached} routes");
            // The staff's graph: the edges the floor plan keeps for an independent walker.
            let staff = people.keeping(|edge| staff_edges.contains(&edge.id));
            let staff_routes = if graph["staff"].is_null() { &graph["people"] } else { &graph["staff"] };
            staff_differs |= !graph["staff"].is_null();
            compare(name, &staff, None, staff_routes);
            if let Some(loads) = graph.get("loads") {
                let loads: EdgeLoads = loads.as_object().unwrap().iter().map(|(edge, load)| (id(edge), load.as_i64().unwrap())).collect();
                compare(name, &people, Some(&loads), &graph["people_full"]);
                full_differs |= graph["people_full"] != graph["people"];
            }
            // `world_graph`'s reading of the same tables finds the same routes.
            let world = Router::of_world_tables(&zones, &edges, &world_capacities).expect("the gate's graphs are valid");
            compare(name, &world, None, &graph["people"]);
            assert_eq!(world.edge_ids(), people.edge_ids());
        }
    }
    // The reference has a staff's graph a plan narrows and a capacity that turns a route away.
    assert!(staff_differs && full_differs);
}

// ---- ZoneGraph's checks -------------------------------------------------------------------------

fn refusal(zones: &Table<ZoneRow>, edges: &Table<EdgeRow>, capacities: &RouteCapacities) -> String {
    match Router::of_tables(zones, edges, capacities) {
        Err(Error::Value(text)) => text,
        Err(other) => panic!("not a ValueError: {other:?}"),
        Ok(_) => panic!("accepted"),
    }
}

#[test]
fn stable_ids_are_those_of_zone_graph() {
    for good in ["a", "A9", "room-1", "a.b_c:d-e", &"x".repeat(64)] {
        assert!(is_stable_id(good), "{good}");
    }
    for bad in ["", "-a", ".a", "_a", "a b", "a/b", "é", "a\n", &"x".repeat(65)] {
        assert!(!is_stable_id(bad), "{bad:?}");
    }
}

#[test]
fn the_tables_are_checked_with_zone_graph_s_words_in_its_order() {
    let two = zones(&["a", "b"]);
    let one_edge = edges(vec![edge("ab", "a", "b", 1)]);
    // The capacities' keys come first: by the dict's keys of the edges.
    let unknown = capacities(&[("zz", Capacity::Max(1))]);
    assert_eq!(refusal(&zones(&[]), &one_edge, &unknown), "route capacity references an unknown zone edge");
    let unmatched = RouteCapacities { unmatched_keys: true, ..none() };
    assert_eq!(refusal(&two, &one_edge, &unmatched), "route capacity references an unknown zone edge");
    assert_eq!(refusal(&zones(&[]), &edges(Vec::new()), &none()), "zone graph must contain at least one zone");
    // Each zone, in the dict's order.
    let keyed = table(vec![("a", zone("a")), ("b", zone("c"))]);
    assert_eq!(refusal(&keyed, &edges(Vec::new()), &none()), "zone graph mapping keys must match stable zone IDs");
    assert_eq!(refusal(&zones(&["a", "-b"]), &edges(Vec::new()), &none()), "zone graph mapping keys must match stable zone IDs");
    let negative = table(vec![("a", ZoneRow { capacity: Some(-1), ..zone("a") }), ("b", ZoneRow { display_x: Some(f64::NAN), ..zone("b") })]);
    assert_eq!(refusal(&negative, &edges(Vec::new()), &none()), "zone 'a' capacity cannot be negative");
    let reversed = table(vec![("b", ZoneRow { display_y: Some(f64::INFINITY), ..zone("b") }), ("a", ZoneRow { capacity: Some(-1), ..zone("a") })]);
    assert_eq!(refusal(&reversed, &edges(Vec::new()), &none()), "zone 'b' has non-finite display coordinates");
    let fine = table(vec![("a", ZoneRow { capacity: Some(0), display_x: Some(-1.5), ..zone("a") }), ("b", zone("b"))]);
    assert!(Router::of_tables(&fine, &edges(Vec::new()), &none()).is_ok());
    // Each edge, by ID: the first of them that fails names the error.
    let cases: Vec<(EdgeRow, &str)> = vec![
        (edge("-ab", "a", "b", 1), "zone edge IDs must be stable and unique"),
        (edge("ab", "a", "zz", 1), "zone edge 'ab' references an unknown zone"),
        (edge("ab", "zz", "b", 1), "zone edge 'ab' references an unknown zone"),
        (edge("ab", "a", "a", 1), "zone edge 'ab' must connect different zones"),
        (edge("ab", "a", "b", 0), "zone edge 'ab' travel time must be a positive integer"),
        (edge("ab", "a", "b", -3), "zone edge 'ab' travel time must be a positive integer"),
    ];
    for (row, text) in cases {
        assert_eq!(refusal(&two, &edges(vec![row]), &none()), text);
    }
    let twice = table(vec![("first", edge("ab", "a", "b", 1)), ("second", edge("ab", "b", "a", 1))]);
    assert_eq!(refusal(&two, &twice, &none()), "zone edge IDs must be stable and unique");
    for capacity in [Capacity::Max(0), Capacity::Max(101), Capacity::Max(-1), Capacity::Refused("a value is not an int")] {
        assert_eq!(refusal(&two, &one_edge, &capacities(&[("ab", capacity)])), "zone edge 'ab' transit capacity must be from 1 to 100");
    }
    for capacity in [Capacity::Max(1), Capacity::Max(100), Capacity::Unlimited] {
        assert!(Router::of_tables(&two, &one_edge, &capacities(&[("ab", capacity)])).is_ok());
    }
    // In ID order, not the dict's: "ba" fails before "bb" although the dict has "bb" first.
    let order = edges(vec![edge("bb", "a", "zz", 1), edge("ba", "a", "b", 0)]);
    assert_eq!(refusal(&two, &order, &none()), "zone edge 'ba' travel time must be a positive integer");
    // A capacity is the edge's by its own ID; the dict's key is what the capacities' keys meet.
    let rekeyed = table(vec![("key", edge("ab", "a", "b", 1))]);
    assert_eq!(refusal(&two, &rekeyed, &capacities(&[("ab", Capacity::Max(1))])), "route capacity references an unknown zone edge");
    let router = Router::of_tables(&two, &rekeyed, &capacities(&[("key", Capacity::Max(1))])).unwrap();
    assert_eq!(router.edge_ids(), vec![id("ab")]);
    // Closed edges are checked, and left out of the router.
    let closed = edges(vec![EdgeRow { closed: true, ..edge("ab", "a", "b", 1) }, edge("ba", "b", "a", 2)]);
    let router = Router::of_tables(&two, &closed, &none()).unwrap();
    assert_eq!(router.edge_ids(), vec![id("ba")]);
    assert_eq!(router.shortest(&HashSet::new(), None, &id("a"), &id("b")), Ok(None));
    let bad_closed = edges(vec![EdgeRow { closed: true, ..edge("ab", "a", "b", 0) }]);
    assert_eq!(refusal(&two, &bad_closed, &none()), "zone edge 'ab' travel time must be a positive integer");
}

#[test]
fn the_world_graph_refuses_what_it_refused() {
    let two = zones(&["a", "b"]);
    for row in [edge("ab", "a", "zz", 1), edge("ab", "a", "a", 1), edge("ab", "a", "b", 0), EdgeRow { closed: true, ..edge("ab", "a", "b", 0) }] {
        let refused = Router::of_world_tables(&two, &edges(vec![row]), &HashMap::new()).err();
        assert_eq!(refused, Some(Error::Unsupported("an edge the zone graph turns down".into())));
    }
    // It reads no more than it read: neither the zones' IDs nor the edges'.
    let loose = table(vec![("a", zone("-a")), ("b", zone("b"))]);
    let router = Router::of_world_tables(&loose, &edges(vec![edge("-ab", "a", "b", 1)]), &HashMap::new()).unwrap();
    assert_eq!(router.edge_ids(), vec![id("-ab")]);
    // The capacities go to the edges by their IDs.
    let mut full = HashMap::new();
    full.insert(id("ab"), 1);
    let router = Router::of_world_tables(&two, &edges(vec![edge("ab", "a", "b", 1)]), &full).unwrap();
    let mut loads = EdgeLoads::new();
    loads.insert(id("ab"), 1);
    assert_eq!(router.shortest(&HashSet::new(), Some(&loads), &id("a"), &id("b")), Ok(None));
}

// ---- The epoch's snapshot -----------------------------------------------------------------------

#[test]
fn a_router_is_the_tables_when_the_routes_were_refreshed() {
    let three = zones(&["a", "b", "c"]);
    let mut table_edges = edges(vec![edge("ab", "a", "b", 1), edge("ac", "a", "c", 1), edge("cb", "c", "b", 1)]);
    let mut routes = Routes::default();
    routes.refresh();
    let epoch = routes.epoch();
    let router = Arc::new(Router::of_tables(&three, &table_edges, &none()).unwrap());
    routes.keep(Which::People, epoch, router.clone());
    routes.keep(Which::Staff, epoch, Arc::new(router.keeping(|edge| &*edge.id != "ab")));
    let route = |routes: &Routes, which| found(routes.get(which).unwrap().shortest(&HashSet::new(), None, &id("a"), &id("b")).unwrap()).map(|route| route.1);
    assert_eq!(route(&routes, Which::People), Some(vec![id("ab")]));
    assert_eq!(route(&routes, Which::Staff), Some(vec![id("ac"), id("cb")]));
    // An edge closed in the tables afterwards is not seen until the next refresh builds again.
    table_edges.get_mut("ab").unwrap().closed = true;
    assert_eq!(route(&routes, Which::People), Some(vec![id("ab")]));
    routes.refresh();
    assert!(routes.get(Which::People).is_none() && routes.get(Which::Staff).is_none());
    let epoch = routes.epoch();
    routes.keep(Which::People, epoch, Arc::new(Router::of_tables(&three, &table_edges, &none()).unwrap()));
    assert_eq!(route(&routes, Which::People), Some(vec![id("ac"), id("cb")]));
    // The staff's graph keeps the zones and the order of each zone's edges.
    let kept = router.keeping(|_| true);
    assert_eq!(kept.edge_ids(), router.edge_ids());
    assert!(kept.has_zone("c") && !kept.has_zone("d"));
}
