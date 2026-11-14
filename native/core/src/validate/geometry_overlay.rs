//! `validate_geometry_overlay_state(world)` of `domain/geometry_overlay.py`, with Python's errors
//! and texts (lane E of #250, #350). Raw errors: `validate_simulator_extensions` passes them on.
//!
//! Also the floor plans the validators of this lane read: `parsed_facility_geometry` (the
//! Scenario's `FacilityGeometry`, `contracts::validate`) and `effective_geometry` (with the
//! overlay's moved facilities), with the errors Python meets making them, and
//! `effective_spatial_constraints`' equipment footprints.
//!
//! `_GeometryOverlayState` is a pydantic model; only whether it validates matters (Python wraps
//! its error), so its models here are the core's pydantic (`contracts::pydantic`).

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use indexmap::IndexMap;

use super::movement::py::{cmp_i64, int_cmp, item};
use super::View;
use crate::contracts::models::POINT2D;
use crate::contracts::pydantic::{self as pd, Ctx, Field, Fields, In, IntS, Len, Lit, Model, Pattern, StrS, S};
use crate::error::{Error, Result};
use crate::geometry::{point_in_polygon, segment_inside_polygon, Pt};
use crate::mathx::{py_max, py_min};
use crate::host::Contract;
use crate::rules::overlay::bed_target_id;
use crate::value::V;

const KEY: &str = "geometry_overlay_v1";
const POLICY: &str = "bounded-runtime-geometry-v1";

// ---- The floor plan -------------------------------------------------------------------------------

/// What the validators read of a `FacilityGeometry` (its dump), with the overlay's moves where
/// `effective_geometry` made it.
pub struct Plan {
    /// `geometry.zones`, in order.
    pub zones: Vec<(Arc<str>, Vec<Pt>)>,
    /// `geometry.routes`: each route's edge ID and whether it is accessible.
    pub routes: Vec<(Arc<str>, bool)>,
    pub facility_points: IndexMap<Arc<str>, Pt>,
    /// The service points: each one's dump, and its point.
    pub service_points: Vec<(V, Pt)>,
}

impl Plan {
    /// `next((item for item in geometry.zones if item.zone_id == zone_id), None)`'s polygon.
    pub fn zone(&self, zone_id: &str) -> Option<&[Pt]> {
        self.zones.iter().find(|(id, _)| &**id == zone_id).map(|(_, polygon)| polygon.as_slice())
    }
}

fn number(value: &V) -> f64 {
    match value {
        V::Float(number) => *number,
        V::Int(number) => *number as f64,
        _ => f64::NAN,
    }
}

/// A dumped `Point2D`.
fn pt(value: &V) -> Pt {
    match value {
        V::Map(map) => Pt { x: map.get("x").map_or(f64::NAN, number), y: map.get("y").map_or(f64::NAN, number) },
        _ => Pt { x: f64::NAN, y: f64::NAN },
    }
}

fn points(value: Option<&V>) -> Vec<Pt> {
    match value {
        Some(V::List(items)) => items.iter().map(pt).collect(),
        _ => Vec::new(),
    }
}

fn list<'a>(map: &'a IndexMap<Arc<str>, V>, name: &str) -> &'a [V] {
    match map.get(name) {
        Some(V::List(items)) => items,
        _ => &[],
    }
}

fn text_of(map: &IndexMap<Arc<str>, V>, name: &str) -> Arc<str> {
    match map.get(name) {
        Some(V::Str(text)) => text.clone(),
        _ => Arc::from(""),
    }
}

impl Plan {
    fn of_dump(dump: &V) -> Plan {
        let empty = IndexMap::new();
        let map = match dump {
            V::Map(map) => map,
            _ => &empty,
        };
        let zones = list(map, "zones")
            .iter()
            .filter_map(|zone| match zone {
                V::Map(zone) => Some((text_of(zone, "zone_id"), points(zone.get("polygon")))),
                _ => None,
            })
            .collect();
        let routes = list(map, "routes")
            .iter()
            .filter_map(|route| match route {
                V::Map(route) => Some((text_of(route, "edge_id"), matches!(route.get("accessible"), Some(V::Bool(true))))),
                _ => None,
            })
            .collect();
        let facility_points = match map.get("facility_points") {
            Some(V::Map(points)) => points.iter().map(|(id, point)| (id.clone(), pt(point))).collect(),
            _ => IndexMap::new(),
        };
        let service_points = list(map, "service_points")
            .iter()
            .map(|service| {
                let point = match service {
                    V::Map(service) => service.get("point_m").map_or(Pt { x: f64::NAN, y: f64::NAN }, pt),
                    _ => Pt { x: f64::NAN, y: f64::NAN },
                };
                (service.clone(), point)
            })
            .collect();
        Plan { zones, routes, facility_points, service_points }
    }
}

/// `Point2D.model_validate(value)`, or its `ValidationError`.
fn point2d(value: &V) -> Result<Pt> {
    match pd::model(&POINT2D, In::plain(value), &mut Ctx::default()) {
        Ok(fields) => Ok(pt(&V::Map(fields))),
        Err(errors) => Err(Error::Validation(pd::text(POINT2D.name, &errors))),
    }
}

/// `Model.model_validate(raw).model_dump(mode="json")` of an input contract, or its
/// `ValidationError`.
fn contract(which: &Contract, raw: &V) -> Result<V> {
    crate::contracts::validate(which, raw).map_err(|error| match error {
        Error::Value(text) => Error::Validation(text),
        other => other,
    })
}

/// `parsed_facility_geometry(raw)`: the dump of `FacilityGeometry.model_validate(raw)`, or its
/// error.
pub fn parsed_geometry(raw: &V) -> Result<V> {
    contract(&Contract::FacilityGeometry, raw)
}

/// `read_only_part(world, GEOMETRY_OVERLAY_KEY, part, None)`: the part of the overlay, None where
/// the overlay or the part is missing or the overlay is not a dict.
fn overlay_part(view: &View<'_>, part: &str) -> Result<Option<V>> {
    let Some(state) = view.state(KEY)? else {
        return Ok(None);
    };
    Ok(match &*state {
        V::Map(map) => map.get(part).cloned(),
        _ => None,
    })
}

/// `effective_geometry(world)`: None without `facility_geometry`; else the Scenario's plan with
/// the overlay's moved facilities and their service points, or the error Python meets making it.
pub fn effective_geometry(view: &View<'_>) -> Result<Option<Plan>> {
    let raw = match view.config("facility_geometry")? {
        None | Some(V::Null) => return Ok(None),
        Some(raw) => raw,
    };
    let mut plan = Plan::of_dump(&parsed_geometry(&raw)?);
    let overrides = overlay_part(view, "facility_points")?.unwrap_or(V::Map(IndexMap::new()));
    if !overrides.is_truthy() {
        return Ok(Some(plan));
    }
    let V::Map(overrides) = overrides else {
        return Err(Error::unsupported("AttributeError: the overlay's facility_points is not a dict"));
    };
    let mut moved = Vec::with_capacity(overrides.len());
    for (key, row) in &overrides {
        moved.push((key.clone(), point2d(item(row, "point_m")?)?));
    }
    let original = plan.facility_points.clone();
    for (key, point) in moved {
        plan.facility_points.insert(key, point);
    }
    for (service, point) in &mut plan.service_points {
        let facility_id = match service {
            V::Map(map) => text_of(map, "facility_id"),
            _ => continue,
        };
        let Some(row) = overrides.get(&facility_id) else {
            continue;
        };
        let old = *original.get(&facility_id).ok_or_else(|| Error::Key(facility_id.to_string()))?;
        let new = point2d(item(row, "point_m")?)?;
        let shifted = Pt { x: point.x + new.x - old.x, y: point.y + new.y - old.y };
        *point = shifted;
        if let V::Map(map) = service {
            let mut at = IndexMap::new();
            at.insert(Arc::from("x"), V::Float(shifted.x));
            at.insert(Arc::from("y"), V::Float(shifted.y));
            map.insert(Arc::from("point_m"), V::Map(at));
        }
    }
    Ok(Some(plan))
}

/// The equipment footprints of `effective_spatial_constraints(world)`: each facility and its
/// polygon, a moved facility's moved with it; or the error Python meets making them.
fn effective_footprints(view: &View<'_>) -> Result<Vec<(Arc<str>, Vec<Pt>)>> {
    let raw = view.config("spatial_constraints")?.unwrap_or(V::Map(IndexMap::new()));
    let constraints = contract(&Contract::SpatialConstraints, &raw)?;
    let V::Map(constraints) = &constraints else {
        return Ok(Vec::new());
    };
    let state = view.state(KEY)?;
    let empty = IndexMap::new();
    let moves = match state.as_deref() {
        Some(V::Map(state)) => match state.get("facility_points") {
            Some(V::Map(moves)) => moves,
            Some(_) => return Err(Error::unsupported("AttributeError: the overlay's facility_points is not a dict")),
            None => &empty,
        },
        _ => &empty,
    };
    let mut out = Vec::new();
    for footprint in list(constraints, "equipment_footprints") {
        let V::Map(footprint) = footprint else {
            continue;
        };
        let facility_id = text_of(footprint, "facility_id");
        let polygon = points(footprint.get("polygon"));
        let Some(row) = moves.get(&facility_id) else {
            out.push((facility_id, polygon));
            continue;
        };
        // `world.model_configs.get("facility_geometry", {}).get("facility_points", {})`.
        let original = match view.config("facility_geometry")? {
            Some(V::Map(geometry)) => geometry.get("facility_points").cloned().unwrap_or(V::Map(IndexMap::new())),
            _ => return Err(Error::unsupported("AttributeError: facility_geometry is not a dict")),
        };
        let old = point2d(item(&original, &facility_id)?)?;
        let new = point2d(item(row, "point_m")?)?;
        let polygon = polygon.iter().map(|p| Pt { x: p.x + new.x - old.x, y: p.y + new.y - old.y }).collect();
        out.push((facility_id, polygon));
    }
    Ok(out)
}

// ---- _GeometryOverlayState ------------------------------------------------------------------------

const IDENT: S = S::Str(StrS { strip: false, min: None, max: None, pattern: Some(Pattern::Identifier) });
const IDS: S = S::Tuple(&IDENT, Len { min: None, max: None });
const POINT: S = S::Model(&POINT2D);
const POINTS: S = S::Tuple(&POINT, Len { min: None, max: None });
const MINUTE: S = S::Int(IntS { strict: true, ge: Some(0), gt: None, le: None });
const END_MINUTE: S = S::Int(IntS { strict: true, ge: None, gt: Some(0), le: None });
const STRICT_BOOL: S = S::Bool { strict: true };

const fn required(name: &'static str, s: S) -> Field {
    Field { name, s, default: None }
}

const fn with(name: &'static str, s: S, default: fn() -> V) -> Field {
    Field { name, s, default: Some(default) }
}

const fn model(name: &'static str, fields: &'static [Field], after: Option<pd::After>) -> Model {
    Model { name, fields, before: None, after, recursive: false }
}

fn null() -> V {
    V::Null
}

fn empty_list() -> V {
    V::List(Vec::new())
}

fn empty_map() -> V {
    V::Map(IndexMap::new())
}

fn field<'a>(fields: &'a Fields, name: &str) -> &'a V {
    fields.get(name).unwrap_or(&V::Null)
}

/// `self.end_minute <= self.minute` (or `start_minute`) of a model's after-validator.
fn ends_after(fields: &Fields, start: &str) -> bool {
    int_cmp(field(fields, "end_minute"), field(fields, start)) == Some(Ordering::Greater)
}

fn unique(value: &V) -> bool {
    match value {
        V::List(items) => {
            let mut seen = HashSet::new();
            items.iter().all(|item| match item {
                V::Str(text) => seen.insert(text.clone()),
                _ => true,
            })
        }
        _ => true,
    }
}

static FACILITY_POINT: Model = model(
    "_FacilityPointOverride",
    &[required("point_m", POINT), required("event_id", IDENT), required("source_ids", IDS), required("minute", MINUTE)],
    None,
);

fn interval_is_ordered(fields: &mut Fields) -> std::result::Result<(), String> {
    if ends_after(fields, "minute") {
        Ok(())
    } else {
        Err("end must follow its start".to_owned())
    }
}

static PARTITION: Model = model(
    "_PartitionOverride",
    &[
        required("zone_id", IDENT),
        required("polygon_m", S::Tuple(&POINT, Len { min: Some(3), max: Some(64) })),
        required("event_id", IDENT),
        required("source_ids", IDS),
        required("minute", MINUTE),
        required("end_minute", END_MINUTE),
    ],
    Some(interval_is_ordered),
);

static ROUTE_MODIFIER: Model = model(
    "_RouteModifier",
    &[
        required("accessible", STRICT_BOOL),
        required("event_id", IDENT),
        required("source_ids", IDS),
        required("minute", MINUTE),
        required("end_minute", END_MINUTE),
    ],
    Some(interval_is_ordered),
);

static ROUTE_OVERLAY: Model =
    model("_RouteOverlay", &[required("base_closed", STRICT_BOOL), required("modifiers", S::Dict(&IDENT, &S::Model(&ROUTE_MODIFIER)))], None);

fn room_shape_is_consistent(fields: &mut Fields) -> std::result::Result<(), String> {
    let bad = |text: &str| Err(text.to_owned());
    if !ends_after(fields, "start_minute") {
        return bad("temporary room end must follow its start");
    }
    let (sleeping, people) = (field(fields, "sleeping_capacity"), field(fields, "people_capacity"));
    if int_cmp(sleeping, people) == Some(Ordering::Greater) {
        return bad("temporary room sleeping capacity exceeds people capacity");
    }
    let beds = match field(fields, "bed_points_m") {
        V::List(items) => items.len() as i64,
        _ => 0,
    };
    if cmp_i64(sleeping, beds) != Some(Ordering::Equal) {
        return bad("temporary room bed points must match sleeping capacity");
    }
    if matches!(field(fields, "active"), V::Bool(true)) == !matches!(field(fields, "end_event_id"), V::Null) {
        return bad("temporary room active status and end event disagree");
    }
    if !unique(field(fields, "route_edge_ids")) || !unique(field(fields, "source_ids")) {
        return bad("temporary room IDs must be unique");
    }
    Ok(())
}

static TEMPORARY_ROOM: Model = model(
    "_TemporaryRoomOverlay",
    &[
        required("schema_version", S::Lit(&[Lit::Int(1)])),
        required("room_id", IDENT),
        required("zone_id", IDENT),
        required("polygon_m", POINTS),
        required("people_capacity", S::Int(IntS { strict: true, ge: None, gt: Some(0), le: Some(1_000_000) })),
        required("sleeping_capacity", S::Int(IntS { strict: true, ge: Some(0), gt: None, le: Some(1_000_000) })),
        required("bed_points_m", POINTS),
        required("route_edge_ids", IDS),
        required("start_minute", MINUTE),
        required("end_minute", END_MINUTE),
        required("active", STRICT_BOOL),
        required("start_event_id", IDENT),
        with("end_event_id", S::Opt(&IDENT), null),
        required("program_id", IDENT),
        required("effect_id", IDENT),
        required("source_ids", IDS),
    ],
    Some(room_shape_is_consistent),
);

static BED_RESERVATION: Model = model(
    "_TemporaryBedReservation",
    &[
        required("agent_id", IDENT),
        required("room_id", IDENT),
        required("bed_index", MINUTE),
        required("target_id", IDENT),
        required("reserved_minute", MINUTE),
    ],
    None,
);

static ROOM_ADMISSION: Model = model(
    "_TemporaryRoomAdmission",
    &[
        required("agent_id", IDENT),
        required("room_id", IDENT),
        required("admitted_minute", MINUTE),
        required("event_id", IDENT),
        with("source_ids", IDS, empty_list),
    ],
    None,
);

static HISTORY: Model = model(
    "_GeometryHistory",
    &[
        required("transition_id", IDENT),
        required(
            "op",
            S::Lit(&[
                Lit::Str("move_facility"),
                Lit::Str("transfer_equipment"),
                Lit::Str("set_partition"),
                Lit::Str("set_route_access"),
                Lit::Str("temporary_room_geometry"),
                Lit::Str("install_facility"),
            ]),
        ),
        required("target_id", IDENT),
        required("status", S::Lit(&[Lit::Str("applied"), Lit::Str("expired")])),
        required("causation_id", S::Opt(&IDENT)),
        required("event_id", IDENT),
        required("source_ids", IDS),
        required("minute", MINUTE),
    ],
    None,
);

static STATE: Model = model(
    "_GeometryOverlayState",
    &[
        required("schema_version", S::Int(IntS { strict: true, ge: Some(1), gt: None, le: Some(1) })),
        required("policy_version", S::Lit(&[Lit::Str(POLICY)])),
        required("facility_points", S::Dict(&IDENT, &S::Model(&FACILITY_POINT))),
        required("partitions", S::Dict(&IDENT, &S::Model(&PARTITION))),
        required("route_modifiers", S::Dict(&IDENT, &S::Model(&ROUTE_OVERLAY))),
        required("history", S::Tuple(&S::Model(&HISTORY), Len { min: None, max: Some(20_000) })),
        with("temporary_rooms", S::Dict(&IDENT, &S::Model(&TEMPORARY_ROOM)), empty_map),
        with("bed_reservations", S::Dict(&IDENT, &S::Model(&BED_RESERVATION)), empty_map),
        with("room_admissions", S::Dict(&IDENT, &S::Model(&ROOM_ADMISSION)), empty_map),
    ],
    None,
);

// ---- The check ------------------------------------------------------------------------------------

fn map_of(value: &V) -> &IndexMap<Arc<str>, V> {
    static EMPTY: std::sync::OnceLock<IndexMap<Arc<str>, V>> = std::sync::OnceLock::new();
    match value {
        V::Map(map) => map,
        _ => EMPTY.get_or_init(IndexMap::new),
    }
}

fn str_of<'a>(map: &'a IndexMap<Arc<str>, V>, name: &str) -> &'a str {
    match map.get(name) {
        Some(V::Str(text)) => text,
        _ => "",
    }
}

fn int_field<'a>(map: &'a IndexMap<Arc<str>, V>, name: &str) -> &'a V {
    map.get(name).unwrap_or(&V::Null)
}

fn later_than(value: &V, minute: i64) -> bool {
    cmp_i64(value, minute) == Some(Ordering::Greater)
}

fn invalid<T>(text: &str) -> Result<T> {
    Err(Error::Value(text.to_owned()))
}

fn sides(polygon: &[Pt]) -> impl Iterator<Item = (Pt, Pt)> + '_ {
    polygon.iter().copied().zip(polygon.iter().copied().skip(1).chain(polygon.first().copied()))
}

/// `_point_on_segment` of region/facility.py.
fn point_on_segment(point: Pt, start: Pt, end: Pt) -> bool {
    let cross = (point.x - start.x) * (end.y - start.y) - (point.y - start.y) * (end.x - start.x);
    cross.abs() <= 1e-8
        && py_min(start.x, end.x) - 1e-8 <= point.x
        && point.x <= py_max(start.x, end.x) + 1e-8
        && py_min(start.y, end.y) - 1e-8 <= point.y
        && point.y <= py_max(start.y, end.y) + 1e-8
}

/// Whether `_segment_intersection_points` of region/facility.py finds any.
fn segments_meet(first_start: Pt, first_end: Pt, second_start: Pt, second_end: Pt) -> bool {
    let cross = |ax: f64, ay: f64, bx: f64, by: f64| ax * by - ay * bx;
    let (rx, ry) = (first_end.x - first_start.x, first_end.y - first_start.y);
    let (sx, sy) = (second_end.x - second_start.x, second_end.y - second_start.y);
    let denominator = cross(rx, ry, sx, sy);
    let (qpx, qpy) = (second_start.x - first_start.x, second_start.y - first_start.y);
    if denominator.abs() <= 1e-9 {
        if cross(qpx, qpy, rx, ry).abs() > 1e-9 {
            return false;
        }
        return [first_start, first_end, second_start, second_end]
            .iter()
            .any(|point| point_on_segment(*point, first_start, first_end) && point_on_segment(*point, second_start, second_end));
    }
    let t = cross(qpx, qpy, sx, sy) / denominator;
    let u = cross(qpx, qpy, rx, ry) / denominator;
    (-1e-9..=1.0 + 1e-9).contains(&t) && (-1e-9..=1.0 + 1e-9).contains(&u)
}

/// `_valid_simple_polygon` of region/facility.py.
fn valid_simple(polygon: &[Pt]) -> bool {
    if polygon.len() < 3 {
        return false;
    }
    let crosses: Vec<f64> = sides(polygon).map(|(a, b)| a.x * b.y - b.x * a.y).collect();
    if crate::mathx::py_sum(&crosses).abs() <= 1e-9 {
        return false;
    }
    let edges: Vec<(Pt, Pt)> = sides(polygon).collect();
    let count = edges.len();
    for (index, &(start, end)) in edges.iter().enumerate() {
        for (other_index, &(other_start, other_end)) in edges.iter().enumerate().skip(index + 1) {
            if other_index == index + 1 || other_index == (index + count - 1) % count {
                continue;
            }
            if segments_meet(start, end, other_start, other_end) {
                return false;
            }
        }
    }
    true
}

/// `_polygons_overlap`.
fn polygons_overlap(left: &[Pt], right: &[Pt]) -> bool {
    if left.iter().any(|point| point_in_polygon(*point, right)) || right.iter().any(|point| point_in_polygon(*point, left)) {
        return true;
    }
    sides(left).any(|(a, b)| sides(right).any(|(c, d)| segments_meet(a, b, c, d)))
}

/// `_validate_room_polygon`.
fn room_polygon(zone: &[Pt], polygon: &[Pt]) -> Result<()> {
    if !valid_simple(polygon) {
        return invalid("area_invalid: temporary room polygon must be simple and nondegenerate");
    }
    if polygon.iter().any(|point| !point_in_polygon(*point, zone)) {
        return invalid("area_invalid: temporary room polygon must lie inside its zone");
    }
    if sides(polygon).any(|(a, b)| !segment_inside_polygon(a, b, zone)) {
        return invalid("area_invalid: temporary room polygon crosses the zone boundary");
    }
    Ok(())
}

/// `_validate_partition_polygon`.
fn partition_polygon(zone: &[Pt], polygon: &[Pt]) -> Result<()> {
    if !valid_simple(polygon) || polygon.iter().any(|point| !point_in_polygon(*point, zone)) {
        return invalid("area_invalid: partition requires a simple positive-area polygon inside its zone");
    }
    if sides(polygon).any(|(a, b)| !segment_inside_polygon(a, b, zone)) {
        return invalid("area_invalid: partition leaves a concave zone");
    }
    Ok(())
}

/// The check, as `validate` (`validate/mod.rs`) calls it in Python's order.
pub fn validate(view: &View<'_>) -> Result<()> {
    let Some(state) = view.state(KEY)? else {
        return Ok(());
    };
    if matches!(*state, V::Null) {
        return Ok(());
    }
    let contract = match pd::model(&STATE, In::plain(&state), &mut Ctx::default()) {
        Ok(fields) => fields,
        Err(_) => return invalid("geometry_overlay_v1 has an invalid schema"),
    };
    drop(state);
    let facility_points = map_of(field(&contract, "facility_points"));
    let partitions = map_of(field(&contract, "partitions"));
    let route_modifiers = map_of(field(&contract, "route_modifiers"));
    let rooms = map_of(field(&contract, "temporary_rooms"));
    let reservations = map_of(field(&contract, "bed_reservations"));
    let admissions = map_of(field(&contract, "room_admissions"));
    let history = match field(&contract, "history") {
        V::List(items) => items.as_slice(),
        _ => &[],
    };
    let nested: usize = route_modifiers.values().map(|route| map_of(map_of(route).get("modifiers").unwrap_or(&V::Null)).len()).sum();
    if [facility_points, partitions, route_modifiers, rooms].iter().any(|part| part.len() > 256) || nested > 256 {
        return invalid("geometry_overlay_v1 exceeds its bounded override limit");
    }
    if reservations.len() > 10_000 {
        return invalid("geometry_overlay_v1 exceeds its bounded bed reservation limit");
    }
    if admissions.len() > 10_000 {
        return invalid("geometry_overlay_v1 exceeds its bounded room admission limit");
    }
    let minute = view.minute_index()?;
    let transitions: HashSet<&str> = history.iter().map(|row| str_of(map_of(row), "transition_id")).collect();
    let minutes: Vec<&V> = history.iter().map(|row| int_field(map_of(row), "minute")).collect();
    if transitions.len() != history.len()
        || minutes.iter().any(|at| later_than(at, minute))
        || minutes.windows(2).any(|pair| int_cmp(pair[0], pair[1]) == Some(Ordering::Greater))
    {
        return invalid("geometry_overlay_v1 history identity/order/time mismatch");
    }
    let Some(geometry) = effective_geometry(view)? else {
        return invalid("geometry_overlay requires surveyed geometry");
    };
    let mut zones: HashMap<&str, &[Pt]> = HashMap::new();
    for (id, polygon) in &geometry.zones {
        zones.insert(id, polygon);
    }
    let zone_of = |zone_id: &str| zones.get(zone_id).copied().ok_or_else(|| Error::Key(zone_id.to_owned()));
    let rows = view.rows()?;
    let mut room_polygons: HashMap<&str, Vec<Pt>> = HashMap::new();
    for (room_id, room) in rooms {
        let room = map_of(room);
        let zone_id = str_of(room, "zone_id");
        if str_of(room, "room_id") != &**room_id || !zones.contains_key(zone_id) {
            return invalid("temporary room references an unknown room or zone");
        }
        if later_than(int_field(room, "start_minute"), minute) {
            return invalid("temporary room starts in the future");
        }
        let active = matches!(room.get("active"), Some(V::Bool(true)));
        if active && cmp_i64(int_field(room, "end_minute"), minute) == Some(Ordering::Less) {
            return invalid("temporary room remained active past its expiry");
        }
        let polygon = points(room.get("polygon_m"));
        room_polygon(zone_of(zone_id)?, &polygon)?;
        if points(room.get("bed_points_m")).iter().any(|point| !point_in_polygon(*point, &polygon)) {
            return invalid("temporary room bed point lies outside its polygon");
        }
        let edge_ids: Vec<&str> = list(room, "route_edge_ids").iter().map(|id| if let V::Str(id) = id { &**id } else { "" }).collect();
        if edge_ids.iter().any(|id| !rows.zone_edges.contains_key(id)) {
            return invalid("temporary room references an unknown access route");
        }
        if active {
            let routes: HashMap<&str, bool> = geometry.routes.iter().map(|(id, accessible)| (&**id, *accessible)).collect();
            let open = |id: &str, toward: bool| {
                let edge = rows.zone_edges.get(id).expect("checked above");
                let end = if toward { &edge.to_zone_id } else { &edge.from_zone_id };
                &**end == zone_id && !edge.closed && routes.get(id).copied() == Some(true)
            };
            let incoming = edge_ids.iter().any(|id| open(id, true));
            let outgoing = edge_ids.iter().any(|id| open(id, false));
            if !incoming || !outgoing {
                return invalid("active temporary room lacks a directed ingress or egress route");
            }
        }
        room_polygons.insert(room_id, polygon);
    }
    let active_rooms: Vec<(&str, &IndexMap<Arc<str>, V>)> =
        rooms.iter().map(|(id, room)| (&**id, map_of(room))).filter(|(_, room)| matches!(room.get("active"), Some(V::Bool(true)))).collect();
    for (index, (left_id, left_room)) in active_rooms.iter().enumerate() {
        for (right_id, right_room) in &active_rooms[index + 1..] {
            if str_of(left_room, "zone_id") == str_of(right_room, "zone_id") && polygons_overlap(&room_polygons[left_id], &room_polygons[right_id]) {
                return invalid("active temporary room polygons overlap");
            }
        }
        let polygon = &room_polygons[left_id];
        if geometry.facility_points.values().any(|point| point_in_polygon(*point, polygon)) {
            return invalid("active temporary room covers a facility");
        }
    }
    for (key, row) in facility_points {
        let row = map_of(row);
        let Some(facility) = rows.facilities.get(key) else {
            return invalid("geometry overlay references an unknown facility or invalid record");
        };
        if later_than(int_field(row, "minute"), minute) {
            return invalid("geometry overlay references an unknown facility or invalid record");
        }
        if !point_in_polygon(pt(int_field(row, "point_m")), zone_of(&facility.zone_id)?) {
            return invalid("runtime facility point leaves its original zone");
        }
    }
    for (service, point) in &geometry.service_points {
        let facility = rows.facilities.get(str_of(map_of(service), "facility_id"));
        let inside = match facility {
            None => false,
            Some(facility) => point_in_polygon(*point, zone_of(&facility.zone_id)?),
        };
        if !inside {
            return invalid("runtime service point leaves its facility's physical zone");
        }
    }
    for row in partitions.values() {
        let row = map_of(row);
        let zone_id = str_of(row, "zone_id");
        if !zones.contains_key(zone_id) || later_than(int_field(row, "minute"), minute) {
            return invalid("invalid runtime partition record");
        }
        partition_polygon(zone_of(zone_id)?, &points(row.get("polygon_m")))?;
    }
    for (key, row) in route_modifiers {
        let row = map_of(row);
        let modifiers = map_of(int_field(row, "modifiers"));
        let Some(edge) = rows.zone_edges.get(key) else {
            return invalid("invalid runtime route-access record");
        };
        if modifiers.is_empty() {
            return invalid("invalid runtime route-access record");
        }
        if modifiers.values().any(|record| later_than(int_field(map_of(record), "minute"), minute)) {
            return invalid("invalid runtime route modifier");
        }
        let closed = matches!(row.get("base_closed"), Some(V::Bool(true)))
            || modifiers.values().any(|record| !matches!(map_of(record).get("accessible"), Some(V::Bool(true))));
        if edge.closed != closed {
            return invalid("route geometry overlay and graph closure disagree");
        }
    }
    drop(rows);
    let footprints = effective_footprints(view)?;
    let rows = view.rows()?;
    for (facility_id, polygon) in &footprints {
        let facility = rows.facilities.get(facility_id).ok_or_else(|| Error::Key(facility_id.to_string()))?;
        let zone = zone_of(&facility.zone_id)?;
        if polygon.iter().any(|point| !point_in_polygon(*point, zone)) {
            return invalid("runtime equipment footprint leaves its original zone");
        }
        let overlaps = rooms.values().map(map_of).any(|room| {
            matches!(room.get("active"), Some(V::Bool(true)))
                && str_of(room, "zone_id") == &*facility.zone_id
                && polygons_overlap(&points(room.get("polygon_m")), polygon)
        });
        if overlaps {
            return invalid("active temporary room overlaps an equipment footprint");
        }
    }
    let mut reserved: HashSet<(&str, i64)> = HashSet::new();
    for (agent_id, reservation) in reservations {
        let reservation = map_of(reservation);
        let room_id = str_of(reservation, "room_id");
        let room = rooms.get(room_id).map(map_of);
        let bed_index = int_field(reservation, "bed_index");
        let bad = str_of(reservation, "agent_id") != &**agent_id
            || !rows.agents.contains_key(agent_id)
            || match room {
                None => true,
                Some(room) => int_cmp(bed_index, int_field(room, "sleeping_capacity")) != Some(Ordering::Less),
            };
        // A bed index past the sleeping capacity (at most 1,000,000) has failed above.
        let index = match bed_index {
            V::Int(index) => *index,
            _ => -1,
        };
        if bad
            || str_of(reservation, "target_id") != bed_target_id(room_id, index)
            || later_than(int_field(reservation, "reserved_minute"), minute)
            || reserved.contains(&(room_id, index))
        {
            return invalid("temporary room bed reservation references an invalid or duplicate bed");
        }
        reserved.insert((room_id, index));
    }
    let mut counts: IndexMap<&str, i64> = IndexMap::new();
    for (agent_id, admission) in admissions {
        let admission = map_of(admission);
        let room_id = str_of(admission, "room_id");
        if str_of(admission, "agent_id") != &**agent_id
            || !rows.agents.contains_key(agent_id)
            || !rooms.contains_key(room_id)
            || later_than(int_field(admission, "admitted_minute"), minute)
        {
            return invalid("temporary room admission references an invalid resident or room");
        }
        *counts.entry(room_id).or_insert(0) += 1;
    }
    if counts.iter().any(|(room_id, count)| cmp_i64(int_field(map_of(&rooms[*room_id]), "people_capacity"), *count) == Some(Ordering::Less)) {
        return invalid("temporary room admissions exceed physical room capacity");
    }
    Ok(())
}
