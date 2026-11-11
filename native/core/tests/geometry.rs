//! The geometry of lane 7-2 without Python: the pure functions, the plan of a floor with the
//! overlay's moves and partitions, the routes in a room, and the plans a configs stamp keeps.
//!
//! The expected values in `data/geometry.json` are what the build of `main` (`720be8b`) gave for
//! the same inputs through the Python functions of `domain/movement.py` and
//! `contracts/extensions.py` and the kernels `route_is_physically_clear` and `plan_in_room_route`
//! (a script in the PR's notes made them). Floats are compared exactly.

use std::sync::Arc;

use serde_json::Value;
use sheltersim_core::config::{FacilityPlan, SpatialConstraints};
use sheltersim_core::geometry::{
    distance, point_in_polygon, point_segment_distance, polyline_length, segment_clear_of_polygon, segment_inside_polygon,
    segment_intersection, zone_center, Geo, GeoCache, Opening, OverlayKey, Pt, Route, ServicePoint, Wall, Zone,
};
use sheltersim_core::row::IdMap;
use sheltersim_core::stamp::Stamp;
use sheltersim_core::value::V;

fn reference() -> Value {
    serde_json::from_str(include_str!("data/geometry.json")).expect("the reference is JSON")
}

fn float(value: &Value) -> f64 {
    value.as_f64().expect("a number")
}

fn pt(value: &Value) -> Pt {
    if let Some(pair) = value.as_array() {
        return Pt { x: float(&pair[0]), y: float(&pair[1]) };
    }
    Pt { x: float(&value["x"]), y: float(&value["y"]) }
}

fn points(value: &Value) -> Vec<Pt> {
    value.as_array().expect("a list").iter().map(pt).collect()
}

fn text(value: &Value) -> Arc<str> {
    Arc::from(value.as_str().expect("a str"))
}

fn optional_pt(value: &Value) -> Option<Pt> {
    (!value.is_null()).then(|| pt(value))
}

fn plan(geometry: &Value) -> FacilityPlan {
    let mut zones = IdMap::default();
    for zone in geometry["zones"].as_array().unwrap() {
        let polygon = points(&zone["polygon"]);
        let center = zone_center(&polygon);
        zones.insert(text(&zone["zone_id"]), Zone { polygon, usable_area: float(&zone["usable_area_m2"]), center });
    }
    let mut routes = IdMap::default();
    for route in geometry["routes"].as_array().unwrap() {
        let accessible = route.get("accessible").map_or(true, |flag| flag.as_bool().unwrap());
        routes.insert(text(&route["edge_id"]), Route { points: points(&route["points"]), width: float(&route["width_m"]), accessible });
    }
    let facility_points = geometry["facility_points"].as_object().unwrap().iter().map(|(id, point)| (Arc::from(id.as_str()), pt(point))).collect();
    let service_points = geometry["service_points"]
        .as_array()
        .unwrap()
        .iter()
        .map(|point| ServicePoint {
            id: text(&point["id"]),
            facility_id: text(&point["facility_id"]),
            point: pt(&point["point_m"]),
            service_capacity: point["service_capacity"].as_i64().unwrap(),
            queue_id: text(&point["queue_id"]),
            accessibility: Arc::from("unknown"),
            required_access_tags: Vec::new(),
            required_staff_skills: Vec::new(),
        })
        .collect();
    FacilityPlan { zones, routes, facility_points, service_points }
}

fn spatial(value: &Value) -> SpatialConstraints {
    let list = |name: &str| value[name].as_array().unwrap().clone();
    SpatialConstraints {
        walls: list("walls").iter().map(|wall| Wall { id: text(&wall["id"]), start: pt(&wall["start"]), end: pt(&wall["end"]) }).collect(),
        openings: list("openings")
            .iter()
            .map(|opening| Opening {
                wall_id: text(&opening["wall_id"]),
                center: pt(&opening["center"]),
                width: float(&opening["width_m"]),
                accessible: opening.get("accessible").map_or(true, |flag| flag.as_bool().unwrap()),
                edge_ids: opening["edge_ids"].as_array().unwrap().iter().map(text).collect(),
            })
            .collect(),
        obstacles: list("obstacles").iter().map(|item| points(&item["polygon"])).collect(),
        footprints: list("equipment_footprints").iter().map(|item| (text(&item["facility_id"]), points(&item["polygon"]))).collect(),
    }
}

/// The JSON of the overlay as the kernels read it (`V`), in its order.
fn v(value: &Value) -> V {
    match value {
        Value::Null => V::Null,
        Value::Bool(flag) => V::Bool(*flag),
        Value::Number(number) if number.is_i64() && !number.to_string().contains('.') => V::Int(number.as_i64().unwrap()),
        Value::Number(number) => V::Float(number.as_f64().unwrap()),
        Value::String(text) => V::Str(Arc::from(text.as_str())),
        Value::Array(items) => V::List(items.iter().map(v).collect()),
        Value::Object(map) => V::Map(map.iter().map(|(key, item)| (Arc::from(key.as_str()), v(item))).collect()),
    }
}

/// The parts of an overlay a plan reads, only when they have entries (`Geo::of`).
fn parts(overlay: &V) -> (Option<V>, Option<V>) {
    let part = |name: &str| match overlay {
        V::Map(map) => map.get(name).filter(|part| matches!(part, V::Map(items) if !items.is_empty())).cloned(),
        _ => None,
    };
    (part("facility_points"), part("partitions"))
}

fn reversed(part: &Option<V>) -> Option<V> {
    part.as_ref().map(|part| match part {
        V::Map(map) => V::Map(map.iter().rev().map(|(key, value)| (key.clone(), value.clone())).collect()),
        other => other.clone(),
    })
}

fn mobility(name: &str) -> &'static str {
    match name {
        "independent" => "independent",
        "assisted" => "assisted",
        "wheelchair" => "wheelchair",
        _ => "other",
    }
}

fn route_list(route: &Option<Arc<[Pt]>>) -> Option<Vec<Pt>> {
    route.as_ref().map(|route| route.to_vec())
}

#[test]
fn the_pure_functions_give_the_python_values() {
    let reference = reference();
    let room = points(&reference["geometry"]["zones"][0]["polygon"]);
    for probe in reference["in_polygon"].as_array().unwrap() {
        let point = Pt { x: float(&probe[0]), y: float(&probe[1]) };
        assert_eq!(point_in_polygon(point, &room), probe[2].as_bool().unwrap(), "{point:?}");
    }
    let obstacle = points(&reference["spatial"]["obstacles"][0]["polygon"]);
    for row in reference["segments"].as_array().unwrap() {
        let (start, end) = (pt(&row["start"]), pt(&row["end"]));
        assert_eq!(segment_inside_polygon(start, end, &room), row["inside"].as_bool().unwrap());
        assert_eq!(segment_clear_of_polygon(start, end, &obstacle, 0.25), row["clear_025"].as_bool().unwrap());
        assert_eq!(segment_clear_of_polygon(start, end, &obstacle, 2.0), row["clear_2"].as_bool().unwrap());
        assert_eq!(point_segment_distance(room[3], start, end).to_bits(), float(&row["distance_to_corner"]).to_bits());
        assert_eq!(segment_intersection(start, end, room[2], room[3]), optional_pt(&row["crossing"]));
    }
    let line = points(&reference["polyline"]["points"]);
    assert_eq!(polyline_length(&line).map(f64::to_bits), Some(float(&reference["polyline"]["length"]).to_bits()));
    assert_eq!(distance(line[0], line[1]), 5.0);
    for (zone, expected) in reference["centers"].as_object().unwrap() {
        let geometry = plan(&reference["geometry"]);
        let polygon = &geometry.zones[zone.as_str()].polygon;
        assert_eq!(zone_center(polygon), Ok(optional_pt(expected)), "{zone}");
    }
}

#[test]
fn a_plan_gives_the_python_routes_with_each_overlay() {
    let reference = reference();
    let facility = plan(&reference["geometry"]);
    let constraints = spatial(&reference["spatial"]);
    let cases = reference["route_cases"].as_array().unwrap();
    for (name, expected) in reference["plans"].as_object().unwrap() {
        let overlay = v(&reference["overlays"][name]);
        let (moves, partitions) = parts(&overlay);
        let geo = Geo::new(Some(&facility), Some(&constraints), moves.as_ref(), partitions.as_ref()).expect("a plan");
        for (key, clear) in expected["clear"].as_object().unwrap() {
            let (edge, walker) = key.split_once('/').unwrap();
            assert_eq!(geo.route_is_clear(edge, mobility(walker)), clear.as_bool().unwrap(), "{name} {key}");
        }
        for (case, route) in cases.iter().zip(expected["routes"].as_array().unwrap()) {
            let start = optional_pt(&case[1]);
            let found = geo.room_route(case[0].as_str().unwrap(), start, pt(&case[2]), mobility(case[3].as_str().unwrap()));
            let expected = (!route.is_null()).then(|| points(route));
            assert_eq!(route_list(&found.expect("no refusal")), expected, "{name} {case}");
            // Asked again, the memo gives the same route.
            let again = geo.room_route(case[0].as_str().unwrap(), start, pt(&case[2]), mobility(case[3].as_str().unwrap()));
            assert_eq!(route_list(&again.unwrap()), expected);
        }
    }
}

#[test]
fn a_plan_follows_the_overlay_order_only_in_its_facility_points() {
    let reference = reference();
    let facility = plan(&reference["geometry"]);
    let constraints = spatial(&reference["spatial"]);
    // serde_json sorts the keys of a map, so the overlay's order is made here.
    let overlay = v(&reference["overlays"]["moved"]);
    let (_, partitions) = parts(&overlay);
    let point = |x: f64, y: f64| V::Map([(Arc::from("x"), V::Float(x)), (Arc::from("y"), V::Float(y))].into_iter().collect());
    let row = |x: f64, y: f64| V::Map([(Arc::from("point_m"), point(x, y))].into_iter().collect());
    let moves = Some(V::Map(
        [(Arc::from("water"), row(5.0, 13.0)), (Arc::from("stove"), row(10.0, 3.0)), (Arc::from("oven"), row(11.0, 3.0))]
            .into_iter()
            .collect(),
    ));
    let first = Geo::new(Some(&facility), Some(&constraints), moves.as_ref(), partitions.as_ref()).unwrap();
    let (moves_back, partitions_back) = (reversed(&moves), reversed(&partitions));
    let second = Geo::new(Some(&facility), Some(&constraints), moves_back.as_ref(), partitions_back.as_ref()).unwrap();
    let (a, b) = (first.layout.as_ref().unwrap(), second.layout.as_ref().unwrap());
    // `{**points, **moves}`: the Scenario's points keep their places (here in the reference's
    // sorted order), and new ones follow in the overlay's order. Nothing else follows it.
    let order = |layout: &sheltersim_core::geometry::Layout| layout.facility_points.keys().map(|id| id.to_string()).collect::<Vec<_>>();
    assert_eq!(order(a), ["toilet", "water", "stove", "oven"]);
    assert_eq!(order(b), ["toilet", "water", "oven", "stove"]);
    for (id, point) in &a.facility_points {
        assert_eq!(b.facility_points.get(id), Some(point));
    }
    let service = |layout: &sheltersim_core::geometry::Layout| layout.service_points.iter().map(|point| (point.id.clone(), point.point)).collect::<Vec<_>>();
    assert_eq!(service(a), service(b));
    assert_ne!(service(a), facility.service_points.iter().map(|point| (point.id.clone(), point.point)).collect::<Vec<_>>());
    let (c, d) = (first.constraints.as_ref().unwrap(), second.constraints.as_ref().unwrap());
    // Partitions in ID order, then the footprints, the moved one moved.
    assert_eq!(c.blocked, d.blocked);
    assert_eq!(c.obstacle_count, 3);
    assert_eq!(c.blocked[1][0], Pt { x: 2.0, y: 2.0 });
    assert_eq!(c.blocked[3][0], Pt { x: 4.5, y: 12.5 });
    for case in reference["route_cases"].as_array().unwrap() {
        let ask = |geo: &Geo| {
            geo.room_route(case[0].as_str().unwrap(), optional_pt(&case[1]), pt(&case[2]), mobility(case[3].as_str().unwrap())).map(|route| route_list(&route))
        };
        assert_eq!(ask(&first), ask(&second));
    }
}

#[test]
fn an_overlay_part_that_is_not_a_dict_is_refused() {
    let reference = reference();
    let facility = plan(&reference["geometry"]);
    let constraints = spatial(&reference["spatial"]);
    let list = V::List(Vec::new());
    assert!(Geo::new(Some(&facility), None, Some(&list), None).is_err());
    assert!(Geo::new(None, Some(&constraints), None, Some(&list)).is_err());
    let bad = V::Map([(Arc::from("p"), V::Map([(Arc::from("polygon_m"), V::List(Vec::new()))].into_iter().collect()))].into_iter().collect());
    assert!(Geo::new(None, Some(&constraints), None, Some(&bad)).is_err());
    let geo = Geo::new(None, None, None, None).unwrap();
    assert!(geo.layout.is_none() && geo.constraints.is_none());
    assert_eq!(geo.center("room"), Ok(None));
    assert!(geo.route_is_clear("e1", "independent"));
}

fn key(stamp: Option<Stamp>, moves: &[u8]) -> OverlayKey {
    OverlayKey { present: true, stamp, moves: moves.to_vec(), partitions: Vec::new() }
}

#[test]
fn the_plans_of_a_configs_stamp_are_found_by_the_overlay_stamp_then_by_bytes() {
    let mut cache = GeoCache::default();
    let (first, second, third) = (Stamp::next(), Stamp::next(), Stamp::next());
    let geo = Arc::new(Geo::default());
    cache.insert(key(Some(first), b"{}"), geo.clone());
    assert!(Arc::ptr_eq(&cache.by_stamp(first).unwrap(), &geo));
    assert!(cache.by_stamp(second).is_none());
    // A new stamp with the same parts: the same plan, which now answers to the new stamp.
    assert!(Arc::ptr_eq(&cache.by_bytes(&key(Some(second), b"{}")).unwrap(), &geo));
    assert!(cache.by_stamp(second).is_some());
    assert!(cache.holds(&key(Some(second), b"{}")));
    assert!(!cache.holds(&key(Some(second), b"{\"a\":1}")));
    // Other parts are another plan; no overlay is another kind of key.
    assert!(cache.by_bytes(&key(Some(third), b"{\"a\":1}")).is_none());
    let absent = OverlayKey { present: false, stamp: None, moves: Vec::new(), partitions: Vec::new() };
    assert!(cache.by_bytes(&absent).is_none());
    cache.insert(absent.clone(), Arc::new(Geo::default()));
    assert!(cache.by_bytes(&absent).is_some());
    // At most CACHED_PLANS, the least recently used dropped.
    for index in 0..sheltersim_core::geometry::CACHED_PLANS {
        cache.insert(key(Some(Stamp::next()), format!("{index}").as_bytes()), Arc::new(Geo::default()));
    }
    assert_eq!(cache.len(), sheltersim_core::geometry::CACHED_PLANS);
    assert!(cache.by_stamp(second).is_none());
}
