//! The models of the input contracts (`host::Contract`) and their validators, each as its Python
//! class has it (Issue #346). A validator reads the validated fields, as the Python one reads the
//! model's attributes, and raises its `ValueError` text in the same order.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use indexmap::IndexMap;

use super::pydantic::{After, Field, Fields, FloatS, IntS, Len, Lit, Model, Pattern, StrS, S};
use crate::geometry::{point_in_polygon, Pt};
use crate::mathx::{hypot, py_max, py_min, py_sum};
use crate::value::V;

// ---- Field types -------------------------------------------------------------------------------

/// `Identifier` of contracts/extensions.py.
const IDENT: S = S::Str(StrS { strip: false, min: None, max: None, pattern: Some(Pattern::Identifier) });
/// `StableId` of events/effects.py (`strict=True`, which a `V` cannot tell apart).
const STABLE: S = S::Str(StrS { strip: false, min: Some(1), max: Some(64), pattern: Some(Pattern::Stable) });
/// `_StableId` of domain/environment.py and domain/infrastructure.py.
const STABLE_STRIPPED: S = S::Str(StrS { strip: true, min: Some(1), max: Some(64), pattern: Some(Pattern::Stable) });
/// `Finite` of contracts/extensions.py.
const FINITE: S = S::Float(FloatS { strict: false, finite: true, ge: None, gt: None, le: None });
const POSITIVE: S = S::Float(FloatS { strict: false, finite: true, ge: None, gt: Some(0.0), le: None });
const IDS: S = S::Tuple(&IDENT, Len { min: None, max: None });
const POINT: S = S::Model(&POINT2D);

const fn len(min: Option<usize>, max: Option<usize>) -> Len {
    Len { min, max }
}

const fn strict_int(ge: Option<i64>, gt: Option<i64>, le: Option<i64>) -> S {
    S::Int(IntS { strict: true, ge, gt, le })
}

const fn strict_float(ge: f64, le: f64) -> S {
    S::Float(FloatS { strict: true, finite: true, ge: Some(ge), gt: None, le: Some(le) })
}

const fn required(name: &'static str, s: S) -> Field {
    Field { name, s, default: None }
}

const fn with(name: &'static str, s: S, default: fn() -> V) -> Field {
    Field { name, s, default: Some(default) }
}

fn null() -> V {
    V::Null
}

fn empty() -> V {
    V::List(Vec::new())
}

fn empty_map() -> V {
    V::Map(IndexMap::new())
}

fn version_one() -> V {
    V::Int(1)
}

fn text(value: &str) -> V {
    V::Str(Arc::from(value))
}

const SCHEMA_VERSION: Field = with("schema_version", S::Lit(&[Lit::Int(1)]), version_one);
const SOURCE_IDS: Field = with("source_ids", IDS, empty);

const fn model(name: &'static str, fields: &'static [Field], after: Option<After>) -> Model {
    Model { name, fields, before: None, after, recursive: false }
}

// ---- Reading validated fields ------------------------------------------------------------------

fn field<'a>(fields: &'a Fields, name: &str) -> &'a V {
    fields.get(name).unwrap_or(&V::Null)
}

fn str_of<'a>(fields: &'a Fields, name: &str) -> Option<&'a str> {
    match field(fields, name) {
        V::Str(text) => Some(text),
        _ => None,
    }
}

fn float_of(fields: &Fields, name: &str) -> f64 {
    number(field(fields, name))
}

fn number(value: &V) -> f64 {
    match value {
        V::Float(number) => *number,
        V::Int(number) => *number as f64,
        V::Bool(flag) => f64::from(u8::from(*flag)),
        _ => f64::NAN,
    }
}

fn int_of(fields: &Fields, name: &str) -> i64 {
    match field(fields, name) {
        V::Int(number) => *number,
        _ => 0,
    }
}

fn items<'a>(fields: &'a Fields, name: &str) -> &'a [V] {
    match field(fields, name) {
        V::List(items) => items,
        _ => &[],
    }
}

fn texts<'a>(fields: &'a Fields, name: &str) -> Vec<&'a str> {
    items(fields, name).iter().filter_map(|item| if let V::Str(text) = item { Some(&**text) } else { None }).collect()
}

/// `len(values) != len(set(values))`.
fn repeats<T: std::hash::Hash + Eq>(values: impl IntoIterator<Item = T>) -> bool {
    let mut seen = HashSet::new();
    values.into_iter().any(|value| !seen.insert(value))
}

fn map_of(value: &V) -> Option<&Fields> {
    match value {
        V::Map(map) => Some(map),
        _ => None,
    }
}

fn pt(value: &V) -> Pt {
    let fields = map_of(value);
    let coordinate = |name| fields.map_or(f64::NAN, |fields| float_of(fields, name));
    Pt { x: coordinate("x"), y: coordinate("y") }
}

fn points(fields: &Fields, name: &str) -> Vec<Pt> {
    items(fields, name).iter().map(pt).collect()
}

// ---- Geometry (contracts/extensions.py, contracts/operations.py) -------------------------------

/// `polygon_area`.
fn polygon_area(polygon: &[Pt]) -> f64 {
    let terms: Vec<f64> = (0..polygon.len())
        .map(|index| {
            let (a, b) = (polygon[index], polygon[(index + 1) % polygon.len()]);
            a.x * b.y - b.x * a.y
        })
        .collect();
    py_sum(&terms).abs() / 2.0
}

/// `_segments_intersect` of contracts/operations.py.
fn segments_intersect(a: Pt, b: Pt, c: Pt, d: Pt) -> bool {
    let epsilon = 1e-9;
    let cross = |first: Pt, second: Pt, third: Pt| (second.x - first.x) * (third.y - first.y) - (second.y - first.y) * (third.x - first.x);
    let on_segment = |first: Pt, second: Pt, point: Pt| {
        py_min(first.x, second.x) - epsilon <= point.x
            && point.x <= py_max(first.x, second.x) + epsilon
            && py_min(first.y, second.y) - epsilon <= point.y
            && point.y <= py_max(first.y, second.y) + epsilon
    };
    let (ab_c, ab_d) = (cross(a, b, c), cross(a, b, d));
    let (cd_a, cd_b) = (cross(c, d, a), cross(c, d, b));
    if ((ab_c > epsilon && ab_d < -epsilon) || (ab_c < -epsilon && ab_d > epsilon)) && ((cd_a > epsilon && cd_b < -epsilon) || (cd_a < -epsilon && cd_b > epsilon)) {
        return true;
    }
    (ab_c.abs() <= epsilon && on_segment(a, b, c))
        || (ab_d.abs() <= epsilon && on_segment(a, b, d))
        || (cd_a.abs() <= epsilon && on_segment(c, d, a))
        || (cd_b.abs() <= epsilon && on_segment(c, d, b))
}

fn edges(points: &[Pt]) -> Vec<(Pt, Pt)> {
    (0..points.len()).map(|index| (points[index], points[(index + 1) % points.len()])).collect()
}

/// `_is_simple_polygon`.
fn is_simple_polygon(points: &[Pt]) -> bool {
    let edges = edges(points);
    for (index, &(a, b)) in edges.iter().enumerate() {
        if a == b {
            return false;
        }
        for (other_index, &(c, d)) in edges.iter().enumerate().skip(index + 1) {
            let wrap = if index == 0 { Some(edges.len() - 1) } else { None };
            if other_index == index + 1 || Some(other_index) == wrap {
                continue;
            }
            if segments_intersect(a, b, c, d) {
                return false;
            }
        }
    }
    true
}

/// `_strictly_inside`.
fn strictly_inside(point: Pt, polygon: &[Pt]) -> bool {
    point_in_polygon(point, polygon) && !edges(polygon).into_iter().any(|(a, b)| segments_intersect(point, point, a, b))
}

// ---- contracts/extensions.py -------------------------------------------------------------------

pub static POINT2D: Model = model("Point2D", &[required("x", FINITE), required("y", FINITE)], None);

static PHYSICAL_ZONE: Model = model(
    "PhysicalZone",
    &[required("zone_id", IDENT), required("polygon", S::Tuple(&POINT, len(Some(3), Some(128)))), required("usable_area_m2", POSITIVE), SOURCE_IDS],
    None,
);

static PHYSICAL_ROUTE: Model = model(
    "PhysicalRoute",
    &[
        required("edge_id", IDENT),
        required("points", S::Tuple(&POINT, len(Some(2), Some(128)))),
        required("width_m", POSITIVE),
        with("accessible", S::Bool { strict: false }, || V::Bool(true)),
    ],
    None,
);

const ACCESS_IDS: S = S::Tuple(&IDENT, len(None, Some(64)));

static SERVICE_ACCESS: Model = model(
    "ServiceAccessConditions",
    &[
        with("accessibility", S::Lit(&[Lit::Str("accessible"), Lit::Str("restricted"), Lit::Str("unknown")]), || text("unknown")),
        with("required_access_tags", ACCESS_IDS, empty),
        with("required_staff_skills", ACCESS_IDS, empty),
    ],
    None,
);

fn default_access() -> V {
    V::Map(IndexMap::from([(Arc::from("accessibility"), text("unknown")), (Arc::from("required_access_tags"), empty()), (Arc::from("required_staff_skills"), empty())]))
}

static SERVICE_POINT: Model = model(
    "FacilityServicePoint",
    &[
        SCHEMA_VERSION,
        required("id", IDENT),
        required("facility_id", IDENT),
        required("point_m", POINT),
        with("access", S::Model(&SERVICE_ACCESS), default_access),
        required("service_capacity", strict_int(Some(0), None, Some(1_000_000))),
        required("queue_id", IDENT),
    ],
    None,
);

static APPROACH_ROUTE: Model = model(
    "ArrivalApproachRoute",
    &[
        SCHEMA_VERSION,
        required("route_id", IDENT),
        required("destination_zone_id", IDENT),
        required("points_m", S::Tuple(&POINT, len(Some(2), Some(128)))),
        required("walking_speed_m_per_min", S::Float(FloatS { strict: false, finite: true, ge: None, gt: Some(0.0), le: Some(120.0) })),
        required("source_kind", S::Lit(&[Lit::Str("measured"), Lit::Str("operator"), Lit::Str("inferred")])),
        required("source_ids", S::Tuple(&IDENT, len(Some(1), Some(20)))),
    ],
    Some(approach_route),
);

/// `ArrivalApproachRoute.route_has_distance_and_sources`.
fn approach_route(fields: &mut Fields) -> Result<(), String> {
    if repeats(texts(fields, "source_ids")) {
        return Err("arrival approach route source IDs must be unique".to_owned());
    }
    let points = points(fields, "points_m");
    let lengths: Vec<f64> = points.windows(2).map(|pair| hypot(&[pair[1].x - pair[0].x, pair[1].y - pair[0].y])).collect();
    if py_sum(&lengths) <= 0.0 {
        return Err("arrival approach route must have positive length".to_owned());
    }
    Ok(())
}

pub static FACILITY_GEOMETRY: Model = model(
    "FacilityGeometry",
    &[
        SCHEMA_VERSION,
        with("units", S::Lit(&[Lit::Str("m")]), || text("m")),
        with("origin", S::Lit(&[Lit::Str("top_left")]), || text("top_left")),
        required("width_m", POSITIVE),
        required("height_m", POSITIVE),
        required("source_kind", S::Lit(&[Lit::Str("synthetic"), Lit::Str("measured"), Lit::Str("operator"), Lit::Str("inferred")])),
        SOURCE_IDS,
        required("zones", S::Tuple(&S::Model(&PHYSICAL_ZONE), len(None, None))),
        with("facility_points", S::Dict(&IDENT, &POINT), empty_map),
        with("service_points", S::Tuple(&S::Model(&SERVICE_POINT), len(None, Some(10_000))), empty),
        with("routes", S::Tuple(&S::Model(&PHYSICAL_ROUTE), len(None, None)), empty),
        with("arrival_approach_routes", S::Tuple(&S::Model(&APPROACH_ROUTE), len(None, Some(128))), empty),
    ],
    Some(geometry_is_bounded),
);

/// `FacilityGeometry.geometry_is_bounded`.
fn geometry_is_bounded(fields: &mut Fields) -> Result<(), String> {
    let models = |name: &str| -> Vec<&Fields> { items(fields, name).iter().filter_map(map_of).collect() };
    let (zones, routes, approaches, services) = (models("zones"), models("routes"), models("arrival_approach_routes"), models("service_points"));
    for (group, key) in [(&zones, "zone_id"), (&routes, "edge_id"), (&approaches, "route_id")] {
        if repeats(group.iter().map(|item| str_of(item, key))) {
            return Err("geometry IDs must be unique".to_owned());
        }
    }
    let mut all = Vec::new();
    for zone in &zones {
        all.extend(points(zone, "polygon"));
    }
    for route in &routes {
        all.extend(points(route, "points"));
    }
    for route in &approaches {
        all.extend(points(route, "points_m"));
    }
    if let Some(facility_points) = map_of(field(fields, "facility_points")) {
        all.extend(facility_points.values().map(pt));
    }
    all.extend(services.iter().map(|service| pt(field(service, "point_m"))));
    let (width, height) = (float_of(fields, "width_m"), float_of(fields, "height_m"));
    if all.iter().any(|p| !(0.0 <= p.x && p.x <= width && 0.0 <= p.y && p.y <= height)) {
        return Err("physical geometry is outside the building bounds".to_owned());
    }
    let zone_by_id: HashMap<Option<&str>, &Fields> = zones.iter().map(|zone| (str_of(zone, "zone_id"), *zone)).collect();
    for route in &approaches {
        let Some(zone) = zone_by_id.get(&str_of(route, "destination_zone_id")) else {
            return Err("arrival approach route references an unknown destination zone".to_owned());
        };
        let route_points = points(route, "points_m");
        let (Some(&first), Some(&last)) = (route_points.first(), route_points.last()) else {
            continue;
        };
        if !point_in_polygon(last, &points(zone, "polygon")) {
            return Err("arrival approach route must end inside its destination zone".to_owned());
        }
        if zones.iter().any(|item| point_in_polygon(first, &points(item, "polygon"))) {
            return Err("arrival approach route must start outside modeled zones".to_owned());
        }
    }
    if repeats(services.iter().map(|service| str_of(service, "id"))) {
        return Err("facility service-point IDs must be unique".to_owned());
    }
    if repeats(services.iter().map(|service| str_of(service, "queue_id"))) {
        return Err("each service point must own a unique queue ID".to_owned());
    }
    for zone in &zones {
        let area = polygon_area(&points(zone, "polygon"));
        if area <= 0.0 || float_of(zone, "usable_area_m2") > area + 1e-8 {
            return Err("usable area must not exceed a nondegenerate polygon area".to_owned());
        }
    }
    Ok(())
}

pub static PERSON_TRAITS: Model = model(
    "PersonTraits",
    &[
        required("age_years", S::Int(IntS { strict: false, ge: Some(0), gt: None, le: Some(120) })),
        with("household_id", S::Opt(&IDENT), null),
        with("household_role", S::Lit(&[Lit::Str("adult"), Lit::Str("child"), Lit::Str("dependent"), Lit::Str("alone")]), || text("adult")),
        with("languages", IDS, || V::List(vec![text("ja")])),
        with("accessibility_needs", IDS, empty),
        with("medical_support", S::Bool { strict: false }, || V::Bool(false)),
        SOURCE_IDS,
    ],
    None,
);

pub static POPULATION_SPEC: Model =
    model("PopulationSpec", &[SCHEMA_VERSION, required("people", S::Dict(&IDENT, &S::Model(&PERSON_TRAITS))), SOURCE_IDS], None);

// ---- contracts/population.py, domain/population_lifecycle.py -----------------------------------

/// `SupportKind` of contracts/operations.py.
const SUPPORT_KIND: S = S::Lit(&[
    Lit::Str("medical_assessment"),
    Lit::Str("medication_support"),
    Lit::Str("caregiver_assistance"),
    Lit::Str("privacy_accommodation"),
    Lit::Str("mobility_assistance"),
    Lit::Str("hygiene_support"),
    Lit::Str("emotional_support"),
    Lit::Str("language_access"),
    Lit::Str("dependent_assistance"),
]);

pub static ARRIVAL_CONTEXT: Model = model(
    "DynamicArrivalPersonContext",
    &[required("member_index", strict_int(Some(0), None, Some(999))), with("care_support_needs", S::Tuple(&SUPPORT_KIND, len(None, Some(8))), empty)],
    Some(unique_nonempty_support_needs),
);

/// `DynamicArrivalPersonContext.unique_nonempty_support_needs`.
fn unique_nonempty_support_needs(fields: &mut Fields) -> Result<(), String> {
    let needs = texts(fields, "care_support_needs");
    if repeats(needs.iter()) {
        return Err("dynamic arrival care support kinds cannot contain duplicates".to_owned());
    }
    if needs.is_empty() {
        return Err("dynamic arrival context override requires a care support kind".to_owned());
    }
    Ok(())
}

const PRESENCE_ACTIONS: S = S::Lit(&[Lit::Str("temporary_exit"), Lit::Str("return"), Lit::Str("transfer"), Lit::Str("relocate"), Lit::Str("reunite")]);

pub static PRESENCE_COMMAND: Model = model(
    "PopulationPresenceCommand",
    &[
        required("action", PRESENCE_ACTIONS),
        with("agent_id", S::Opt(&IDENT), null),
        with("agent_ids", S::Tuple(&IDENT, len(None, Some(1000))), empty),
        with("zone_id", S::Opt(&IDENT), null),
        with("household_id", S::Opt(&IDENT), null),
        with("expected_return_minute", S::Opt(&strict_int(Some(0), None, None)), null),
        with("source_ids", S::Tuple(&IDENT, len(None, Some(16))), empty),
    ],
    Some(inputs_match_action),
);

/// `PopulationPresenceCommand.inputs_match_action`.
fn inputs_match_action(fields: &mut Fields) -> Result<(), String> {
    let action = str_of(fields, "action");
    if action == Some("relocate") && field(fields, "zone_id") == &V::Null {
        return Err("relocate requires a destination zone".to_owned());
    }
    if action == Some("reunite") && field(fields, "household_id") == &V::Null {
        return Err("reunite requires a household ID".to_owned());
    }
    if field(fields, "expected_return_minute") != &V::Null && action != Some("temporary_exit") {
        return Err("expected_return_minute is valid only for temporary exits".to_owned());
    }
    Ok(())
}

// ---- domain/geometry_overlay.py, contracts/operations.py ---------------------------------------

pub static ROUTE_ACCESS: Model = model("RouteAccessParameters", &[required("accessible", S::Bool { strict: true })], None);

pub static MOVE_FACILITY: Model = model("MoveFacilityParameters", &[required("point_m", POINT)], None);

pub static PARTITION: Model = model("PartitionParameters", &[required("polygon_m", S::Tuple(&POINT, len(Some(3), Some(64))))], None);

pub static EQUIPMENT_TRANSFER: Model = model(
    "EquipmentTransferParameters",
    &[SCHEMA_VERSION, required("equipment_id", IDENT), required("from_zone_id", IDENT), required("to_zone_id", IDENT), required("point_m", POINT), SOURCE_IDS],
    Some(transfer_changes_zone),
);

/// `EquipmentTransferParameters.transfer_changes_zone`.
fn transfer_changes_zone(fields: &mut Fields) -> Result<(), String> {
    if field(fields, "from_zone_id") == field(fields, "to_zone_id") {
        return Err("equipment transfer must select a different destination zone".to_owned());
    }
    Ok(())
}

pub static TEMPORARY_ROOM: Model = model(
    "TemporaryRoomGeometry",
    &[
        SCHEMA_VERSION,
        required("room_id", IDENT),
        required("zone_id", IDENT),
        required("polygon_m", S::Tuple(&POINT, len(Some(3), Some(64)))),
        required("people_capacity", strict_int(None, Some(0), Some(1_000_000))),
        required("sleeping_capacity", strict_int(Some(0), None, Some(1_000_000))),
        with("bed_points_m", S::Tuple(&POINT, len(None, Some(100_000))), empty),
        required("route_edge_ids", S::Tuple(&IDENT, len(Some(1), Some(10_000)))),
        SOURCE_IDS,
    ],
    Some(capacity_relationship_is_valid),
);

/// `TemporaryRoomGeometry.capacity_relationship_is_valid`.
fn capacity_relationship_is_valid(fields: &mut Fields) -> Result<(), String> {
    let polygon = points(fields, "polygon_m");
    if !is_simple_polygon(&polygon) || polygon_area(&polygon) <= 0.0 {
        return Err("temporary room polygon must be simple and have nonzero area".to_owned());
    }
    let (people, sleeping) = (int_of(fields, "people_capacity"), int_of(fields, "sleeping_capacity"));
    if sleeping > people {
        return Err("temporary room sleeping capacity cannot exceed people capacity".to_owned());
    }
    let beds = points(fields, "bed_points_m");
    if sleeping != 0 && beds.len() as i64 != sleeping {
        return Err("a physical sleeping room requires one explicit bed point per sleeping place".to_owned());
    }
    if sleeping == 0 && !beds.is_empty() {
        return Err("bed points require a positive sleeping capacity".to_owned());
    }
    if beds.iter().any(|&point| !strictly_inside(point, &polygon)) {
        return Err("temporary room bed points must lie strictly inside its polygon".to_owned());
    }
    if repeats(texts(fields, "route_edge_ids")) {
        return Err("temporary room route edge IDs must be unique".to_owned());
    }
    if repeats(texts(fields, "source_ids")) {
        return Err("temporary room source IDs must be unique".to_owned());
    }
    Ok(())
}

// ---- contracts/spatial.py ----------------------------------------------------------------------

static WALL: Model = model("Wall", &[required("id", IDENT), required("start", POINT), required("end", POINT), SOURCE_IDS], None);

static OPENING: Model = model(
    "Opening",
    &[
        required("id", IDENT),
        required("wall_id", IDENT),
        required("center", POINT),
        required("width_m", POSITIVE),
        required("edge_ids", IDS),
        with("accessible", S::Bool { strict: false }, || V::Bool(true)),
        SOURCE_IDS,
    ],
    None,
);

static OBSTACLE: Model = model("SpatialObstacle", &[required("id", IDENT), required("polygon", S::Tuple(&POINT, len(Some(3), Some(128)))), SOURCE_IDS], None);

static FOOTPRINT: Model = model(
    "EquipmentFootprint",
    &[required("facility_id", IDENT), required("polygon", S::Tuple(&POINT, len(Some(4), Some(4)))), required("area_m2", POSITIVE), SOURCE_IDS],
    None,
);

pub static SPATIAL_CONSTRAINTS: Model = model(
    "SpatialConstraints",
    &[
        SCHEMA_VERSION,
        with("walls", S::Tuple(&S::Model(&WALL), len(None, None)), empty),
        with("openings", S::Tuple(&S::Model(&OPENING), len(None, None)), empty),
        with("obstacles", S::Tuple(&S::Model(&OBSTACLE), len(None, None)), empty),
        with("equipment_footprints", S::Tuple(&S::Model(&FOOTPRINT), len(None, None)), empty),
        with("planning_capacity_by_zone", S::Dict(&IDENT, &S::Int(IntS { strict: false, ge: None, gt: None, le: None })), empty_map),
        with("accessible_facility_ids", IDS, empty),
    ],
    None,
);

// ---- events/effects.py: GuardExpression and _PARAMETER_MODELS ----------------------------------

const GUARD_OPERATORS: S = S::Lit(&[
    Lit::Str("eq"),
    Lit::Str("ne"),
    Lit::Str("gt"),
    Lit::Str("gte"),
    Lit::Str("lt"),
    Lit::Str("lte"),
    Lit::Str("and"),
    Lit::Str("or"),
]);

const GUARD_FIELDS: S = S::Lit(&[
    Lit::Str("simulation.minute"),
    Lit::Str("target.capacity"),
    Lit::Str("target.working_units"),
    Lit::Str("target.queue_length"),
    Lit::Str("zone.occupancy"),
    Lit::Str("resource.available_quantity"),
    Lit::Str("staff.available"),
]);

pub static GUARD: Model = Model {
    name: "GuardExpression",
    fields: &[
        required("operator", GUARD_OPERATORS),
        with("field", S::Opt(&GUARD_FIELDS), null),
        with("value", S::Opt(&S::GuardValue), null),
        with("operands", S::List(&S::Model(&GUARD), len(None, Some(4))), empty),
    ],
    before: None,
    after: Some(operator_shape_and_complexity_are_valid),
    recursive: true,
};

/// `GuardExpression.operator_shape_and_complexity_are_valid`.
fn operator_shape_and_complexity_are_valid(fields: &mut Fields) -> Result<(), String> {
    let operator = str_of(fields, "operator").unwrap_or_default();
    let (no_field, value, operands) = (field(fields, "field") == &V::Null, field(fields, "value"), items(fields, "operands"));
    if matches!(operator, "eq" | "ne" | "gt" | "gte" | "lt" | "lte") {
        if no_field || value == &V::Null || !operands.is_empty() {
            return Err("comparison guards require one allowed field and scalar value".to_owned());
        }
        if matches!(operator, "gt" | "gte" | "lt" | "lte") && !matches!(value, V::Int(_) | V::Big(_) | V::Float(_)) {
            return Err("ordered comparisons require a numeric value".to_owned());
        }
    } else if !no_field || value != &V::Null || operands.len() < 2 {
        return Err("and/or guards require two or more operands and no leaf fields".to_owned());
    }
    /// `measure`: the nodes, or the depth past four.
    fn measure(node: &Fields, depth: usize) -> Result<usize, String> {
        if depth > 4 {
            return Err("guard nesting depth cannot exceed four".to_owned());
        }
        let mut count = 1;
        for child in items(node, "operands").iter().filter_map(map_of) {
            count += measure(child, depth + 1)?;
        }
        Ok(count)
    }
    if measure(fields, 1)? > 20 {
        return Err("guard AST cannot exceed 20 nodes".to_owned());
    }
    Ok(())
}

// ---- The records of a program in effect_program_v1 (#351) --------------------------------------

/// `CompilerProvenance` of events/effects.py.
pub static COMPILER_PROVENANCE: Model = model(
    "CompilerProvenance",
    &[
        required("compiler_kind", S::Lit(&[Lit::Str("offline_rule"), Lit::Str("llm")])),
        required("compiler_version", STABLE),
        with("model_id", S::Opt(&STABLE), null),
        with("prompt_hash", S::Opt(&S::Str(StrS { strip: false, min: None, max: Some(128), pattern: None })), null),
    ],
    None,
);

/// `PlanAssumption` of events/effects.py.
pub static PLAN_ASSUMPTION: Model = model(
    "PlanAssumption",
    &[
        required("assumption_id", STABLE),
        required("description", S::Str(StrS { strip: false, min: Some(1), max: Some(500), pattern: None })),
        required("source_id", STABLE),
        with("units", S::Opt(&STABLE), null),
        with("value", S::Opt(&S::AssumptionValue), null),
    ],
    None,
);

static TEMPORARY_SPACE: Model = model(
    "_TemporarySpaceParameters",
    &[required("people_capacity", strict_int(None, Some(0), Some(1000))), with("sleeping_capacity", strict_int(Some(0), None, Some(1000)), || V::Int(0))],
    Some(sleeping_capacity_fits),
);

/// `_TemporarySpaceParameters.sleeping_capacity_fits`.
fn sleeping_capacity_fits(fields: &mut Fields) -> Result<(), String> {
    if int_of(fields, "sleeping_capacity") > int_of(fields, "people_capacity") {
        return Err("sleeping_capacity cannot exceed people_capacity".to_owned());
    }
    Ok(())
}

static FACILITY_REPAIR: Model = model(
    "FacilityRepairParameters",
    &[
        SCHEMA_VERSION,
        required("facility_id", IDENT),
        required("component_id", IDENT),
        required("expected_working_units", strict_int(Some(0), None, None)),
        required("restored_working_units", strict_int(Some(0), None, None)),
        SOURCE_IDS,
    ],
    None,
);

fn one_float() -> V {
    V::Float(1.0)
}

static DEMAND_RATE: Model = model(
    "DemandRateParameters",
    &[
        required(
            "action",
            S::Lit(&[Lit::Str("request_toilet"), Lit::Str("rest"), Lit::Str("sleep"), Lit::Str("seek_help"), Lit::Str("fetch_water"), Lit::Str("collect_meal")]),
        ),
        with("probability_multiplier", strict_float(0.5, 2.0), one_float),
        with("duration_multiplier", strict_float(0.5, 2.0), one_float),
    ],
    Some(duration_only_applies_to_rest),
);

/// `DemandRateParameters.duration_only_applies_to_rest`.
fn duration_only_applies_to_rest(fields: &mut Fields) -> Result<(), String> {
    let (probability, duration) = (float_of(fields, "probability_multiplier"), float_of(fields, "duration_multiplier"));
    if duration != 1.0 && !matches!(str_of(fields, "action"), Some("rest" | "sleep")) {
        return Err("duration_multiplier is supported only for rest and sleep actions".to_owned());
    }
    if probability == duration && duration == 1.0 {
        return Err("a demand modifier must change probability or duration".to_owned());
    }
    Ok(())
}

static AMBIENT_NOISE: Model = model("AmbientNoiseParameters", &[required("level", strict_float(0.0, 1.0))], None);
static LIGHTING: Model = model("LightingParameters", &[required("level", strict_float(0.0, 1.0))], None);
static PRIVACY: Model = model("PrivacyParameters", &[required("level", strict_float(0.0, 1.0))], None);
static TEMPERATURE: Model = model("TemperatureParameters", &[required("temperature_c", strict_float(-40.0, 60.0))], None);
static VENTILATION: Model = model("VentilationParameters", &[required("effectiveness", strict_float(0.0, 1.0))], None);

pub static STAFF_ROLE: Model = model("_StaffRoleParameters", &[required("skills", S::List(&STABLE, len(Some(1), Some(20))))], Some(unique_skills));

/// `_StaffRoleParameters.unique_skills`.
fn unique_skills(fields: &mut Fields) -> Result<(), String> {
    if repeats(texts(fields, "skills")) {
        return Err("staff role skills must be unique".to_owned());
    }
    Ok(())
}

static STAFF_HANDOFF: Model = model(
    "StaffHandoffParameters",
    &[SCHEMA_VERSION, required("staff_id", IDENT), with("replacement_staff_id", S::Opt(&IDENT), null), required("task_id", IDENT), required("reason_code", IDENT)],
    Some(handoff_changes_staff),
);

/// `StaffHandoffParameters.handoff_changes_staff`.
fn handoff_changes_staff(fields: &mut Fields) -> Result<(), String> {
    if field(fields, "replacement_staff_id") == field(fields, "staff_id") {
        return Err("staff handoff replacement must be a different person".to_owned());
    }
    Ok(())
}

static STAFF_TASK_ORDER: Model = model(
    "StaffTaskOrderParameters",
    &[
        required("task_kind", S::Lit(&[Lit::Str("cleaning"), Lit::Str("maintenance"), Lit::Str("information_outreach"), Lit::Str("intake"), Lit::Str("patrol")])),
        required("target_zone_id", STABLE),
        with("target_id", S::Opt(&STABLE), null),
        required("reason", S::Str(StrS { strip: false, min: Some(1), max: Some(200), pattern: None })),
    ],
    Some(facility_target_matches_task),
);

/// `StaffTaskOrderParameters.facility_target_matches_task`.
fn facility_target_matches_task(fields: &mut Fields) -> Result<(), String> {
    if (str_of(fields, "task_kind") == Some("maintenance")) != (field(fields, "target_id") != &V::Null) {
        return Err("only maintenance orders require a target facility ID".to_owned());
    }
    Ok(())
}

/// `_PARAMETER_MODELS[operator]`.
pub fn parameter_model(operator: &str) -> Option<&'static Model> {
    Some(match operator {
        "add_temporary_space" => &TEMPORARY_SPACE,
        "facility_repair" => &FACILITY_REPAIR,
        "modify_demand_rate" => &DEMAND_RATE,
        "set_ambient_noise" => &AMBIENT_NOISE,
        "set_lighting" => &LIGHTING,
        "set_privacy" => &PRIVACY,
        "set_staff_role" => &STAFF_ROLE,
        "set_temperature" => &TEMPERATURE,
        "set_ventilation" => &VENTILATION,
        "staff_handoff" => &STAFF_HANDOFF,
        "staff_task_order" => &STAFF_TASK_ORDER,
        _ => return None,
    })
}

// ---- domain/environment.py ---------------------------------------------------------------------

static ZONE_BASELINE: Model = model(
    "ZoneEnvironmentBaseline",
    &[
        with("ventilation_effectiveness", strict_float(0.0, 1.0), || V::Float(0.7)),
        with("lighting_level", strict_float(0.0, 1.0), || V::Float(0.5)),
        with("privacy_level", strict_float(0.0, 1.0), || V::Float(0.5)),
        with("temperature_c", strict_float(-40.0, 60.0), || V::Float(20.0)),
    ],
    None,
);

/// The fields of the zone environment's feedback, which a `zone-environment-v1` config holds at 0.
const FEEDBACK_FIELDS: [&str; 4] = [
    "lighting_sleep_efficiency_loss",
    "ventilation_hygiene_growth_per_minute",
    "privacy_discontent_per_minute",
    "temperature_stress_per_degree_minute",
];

pub static ZONE_ENVIRONMENT: Model = Model {
    name: "ScenarioZoneEnvironmentConfig",
    fields: &[
        SCHEMA_VERSION,
        with("policy_version", S::Lit(&[Lit::Str("zone-environment-v1"), Lit::Str("zone-environment-v2")]), || text("zone-environment-v2")),
        with("source_type", S::Lit(&[Lit::Str("assumption")]), || text("assumption")),
        with("source_id", STABLE_STRIPPED, || text("ASM-H19-ZONE-ENVIRONMENT-V2")),
        with("noise_rest_efficiency_loss", strict_float(0.0, 0.9), || V::Float(0.5)),
        with("min_probability_multiplier", strict_float(0.1, 1.0), || V::Float(0.5)),
        with("max_probability_multiplier", strict_float(1.0, 2.0), || V::Float(2.0)),
        with("min_rest_decision_minutes", strict_int(Some(1), None, Some(60)), || V::Int(1)),
        with("max_rest_decision_minutes", strict_int(Some(1), None, Some(240)), || V::Int(60)),
        with("max_active_modifiers_per_zone", strict_int(Some(1), None, Some(16)), || V::Int(16)),
        with("lighting_sleep_efficiency_loss", strict_float(0.0, 0.9), || V::Float(0.2)),
        with("ventilation_hygiene_growth_per_minute", strict_float(0.0, 0.01), || V::Float(0.0001)),
        with("privacy_discontent_per_minute", strict_float(0.0, 0.01), || V::Float(0.0001)),
        with("temperature_stress_per_degree_minute", strict_float(0.0, 0.01), || V::Float(0.0001)),
        with("comfortable_min_temperature_c", strict_float(-10.0, 40.0), || V::Float(18.0)),
        with("comfortable_max_temperature_c", strict_float(-10.0, 50.0), || V::Float(26.0)),
        with("baseline_by_zone", S::Dict(&STABLE_STRIPPED, &S::Model(&ZONE_BASELINE)), empty_map),
    ],
    before: Some(keep_legacy_environment_assumptions_neutral),
    after: Some(multipliers_are_ordered),
    recursive: false,
};

/// `ScenarioZoneEnvironmentConfig.keep_legacy_environment_assumptions_neutral`: a legacy config
/// (`zone-environment-v1`, or no version and none of v2's fields) gets v1's version, source and
/// zero feedback where it has none.
fn keep_legacy_environment_assumptions_neutral(raw: &Fields) -> Option<V> {
    let legacy = match raw.get("policy_version") {
        Some(version) => version == &text("zone-environment-v1"),
        None => !FEEDBACK_FIELDS.iter().chain(["baseline_by_zone"].iter()).any(|name| raw.contains_key(*name)),
    };
    if !legacy {
        return None;
    }
    let mut result = raw.clone();
    result.entry(Arc::from("policy_version")).or_insert_with(|| text("zone-environment-v1"));
    result.entry(Arc::from("source_id")).or_insert_with(|| text("ASM-H19-ZONE-ENVIRONMENT-V1"));
    for name in FEEDBACK_FIELDS {
        result.entry(Arc::from(name)).or_insert(V::Float(0.0));
    }
    result.entry(Arc::from("baseline_by_zone")).or_insert_with(empty_map);
    Some(V::Map(result))
}

/// `ScenarioZoneEnvironmentConfig.multipliers_are_ordered`.
fn multipliers_are_ordered(fields: &mut Fields) -> Result<(), String> {
    let baseline = map_of(field(fields, "baseline_by_zone")).is_some_and(|map| !map.is_empty());
    if str_of(fields, "policy_version") == Some("zone-environment-v1") && (FEEDBACK_FIELDS.iter().any(|name| float_of(fields, name) != 0.0) || baseline) {
        return Err("new environment feedback requires zone-environment-v2".to_owned());
    }
    if float_of(fields, "min_probability_multiplier") > float_of(fields, "max_probability_multiplier") {
        return Err("minimum probability multiplier exceeds its maximum".to_owned());
    }
    if int_of(fields, "min_rest_decision_minutes") > int_of(fields, "max_rest_decision_minutes") {
        return Err("minimum rest decision interval exceeds its maximum".to_owned());
    }
    if float_of(fields, "comfortable_min_temperature_c") >= float_of(fields, "comfortable_max_temperature_c") {
        return Err("comfortable temperature interval must be non-empty".to_owned());
    }
    Ok(())
}

// ---- domain/infrastructure.py ------------------------------------------------------------------

static DEPENDENCY_EDGE: Model =
    model("InfrastructureDependencyEdge", &[required("dependent_id", STABLE_STRIPPED), required("dependency_id", STABLE_STRIPPED)], None);

const STRESS: S = S::Float(FloatS { strict: false, finite: true, ge: Some(0.0), gt: None, le: Some(0.01) });

pub static INFRASTRUCTURE: Model = model(
    "ScenarioInfrastructureSpec",
    &[
        SCHEMA_VERSION,
        with("policy_version", S::Lit(&[Lit::Str("infrastructure-events-v1")]), || text("infrastructure-events-v1")),
        with("source_type", S::Lit(&[Lit::Str("assumption")]), || text("assumption")),
        with("source_id", STABLE_STRIPPED, || text("ASM-A03-INFRASTRUCTURE-V1")),
        with("dependency_edges", S::Tuple(&S::Model(&DEPENDENCY_EDGE), len(None, None)), empty),
        with("heat_stress_per_minute", STRESS, || V::Float(0.001)),
        with("cold_stress_per_minute", STRESS, || V::Float(0.001)),
    ],
    Some(validate_dependency_graph),
);

/// `ScenarioInfrastructureSpec.validate_dependency_graph`: unique edges, no edge to itself, no
/// cycle; then the edges sorted by (dependent, dependency).
fn validate_dependency_graph(fields: &mut Fields) -> Result<(), String> {
    let pair = |edge: &V| -> (Arc<str>, Arc<str>) {
        let fields = map_of(edge);
        let id = |name| match fields.map(|fields| field(fields, name)) {
            Some(V::Str(text)) => text.clone(),
            _ => Arc::from(""),
        };
        (id("dependent_id"), id("dependency_id"))
    };
    let edges: Vec<(Arc<str>, Arc<str>)> = items(fields, "dependency_edges").iter().map(pair).collect();
    let unique: HashSet<&(Arc<str>, Arc<str>)> = edges.iter().collect();
    if unique.len() != edges.len() {
        return Err("infrastructure dependency edges must be unique".to_owned());
    }
    if edges.iter().any(|(dependent, dependency)| dependent == dependency) {
        return Err("infrastructure dependency edges cannot point to themselves".to_owned());
    }
    if has_cycle(&edges) {
        return Err("infrastructure dependency graph must be acyclic".to_owned());
    }
    if let Some(V::List(items)) = fields.get_mut("dependency_edges") {
        items.sort_by_key(|edge| pair(edge));
    }
    Ok(())
}

/// Whether the graph of the edges has a cycle (`visit` of the validator raises for one, whatever
/// order it visits in).
fn has_cycle(edges: &[(Arc<str>, Arc<str>)]) -> bool {
    let mut next: HashMap<&str, Vec<&str>> = HashMap::new();
    for (dependent, dependency) in edges {
        next.entry(dependency).or_default().push(dependent);
    }
    // 1: on the path; 2: done.
    let mut state: HashMap<&str, u8> = HashMap::new();
    let mut nodes: Vec<&str> = edges.iter().flat_map(|(a, b)| [&**a, &**b]).collect();
    nodes.sort_unstable();
    nodes.dedup();
    for start in nodes {
        if state.contains_key(start) {
            continue;
        }
        let mut stack: Vec<(&str, usize)> = vec![(start, 0)];
        state.insert(start, 1);
        while let Some((node, index)) = stack.pop() {
            let children = next.get(node).map_or(&[][..], |children| &children[..]);
            if let Some(&child) = children.get(index) {
                stack.push((node, index + 1));
                match state.get(child) {
                    Some(1) => return true,
                    Some(_) => {}
                    None => {
                        state.insert(child, 1);
                        stack.push((child, 0));
                    }
                }
            } else {
                state.insert(node, 2);
            }
        }
    }
    false
}
