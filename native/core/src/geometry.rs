//! The floor plan as data for the movement rules, and the geometry of `domain/movement.py` and
//! `contracts/extensions.py`. Every function here is the Python one with its order of operations,
//! its tolerances and its float results (docs/rust-port/lanes/m4-movement.md).
//!
//! A `Geo` is built from the parsed configs (`config::geometry`) and the overlay's facility moves
//! and partitions. The PyO3 crate keeps it in the world's `Configs`, by the overlay's stamp
//! (`GeoCache`, Issue #152, lane 7-2).

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::sync::{Arc, Mutex};

use rustc_hash::FxBuildHasher;

use crate::config::geometry::{FacilityPlan, SpatialConstraints};
use crate::error::{Error, Result};
use crate::mathx::{hypot, py_fsum, py_max, py_min, py_round, py_sum};
use crate::row::IdMap;
use crate::stamp::Stamp;
use crate::value::V;

/// Past this a product could overflow where Python's `Point2D` would raise. Nothing real is here.
const COORDINATE_LIMIT: f64 = 1e100;
/// `MAX_REMEMBERED_ROOM_ROUTES`.
const REMEMBERED_ROUTES: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pt {
    pub x: f64,
    pub y: f64,
}

/// `Identifier`: `^[A-Za-z0-9][A-Za-z0-9._:-]{0,63}$`. `$` also takes a final newline, which is no
/// ID a rule writes; it says false here and the Python rule decides.
pub fn is_identifier(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && bytes[0].is_ascii_alphanumeric()
        && bytes[1..].iter().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

/// A coordinate pair a kernel can compute with: what `Point2D` accepts, and not so large as to overflow.
pub fn usable(x: f64, y: f64) -> bool {
    x.abs() <= COORDINATE_LIMIT && y.abs() <= COORDINATE_LIMIT
}

pub fn distance(first: Pt, second: Pt) -> f64 {
    hypot(&[second.x - first.x, second.y - first.y])
}

/// `point_in_polygon`.
pub fn point_in_polygon(point: Pt, polygon: &[Pt]) -> bool {
    let mut inside = false;
    let mut previous = polygon[polygon.len() - 1];
    for &current in polygon {
        let cross = (point.x - previous.x) * (current.y - previous.y) - (point.y - previous.y) * (current.x - previous.x);
        if cross.abs() < 1e-9
            && py_min(previous.x, current.x) - 1e-9 <= point.x
            && point.x <= py_max(previous.x, current.x) + 1e-9
            && py_min(previous.y, current.y) - 1e-9 <= point.y
            && point.y <= py_max(previous.y, current.y) + 1e-9
        {
            return true;
        }
        if (current.y > point.y) != (previous.y > point.y) {
            let crossing = (previous.x - current.x) * (point.y - current.y) / (previous.y - current.y) + current.x;
            if point.x < crossing {
                inside = !inside;
            }
        }
        previous = current;
    }
    inside
}

fn segment_intersection_parameter(start: Pt, end: Pt, first: Pt, second: Pt) -> Option<f64> {
    let (dx, dy) = (end.x - start.x, end.y - start.y);
    let (ex, ey) = (second.x - first.x, second.y - first.y);
    let denominator = dx * ey - dy * ex;
    if denominator.abs() <= 1e-10 {
        return None;
    }
    let (ax, ay) = (first.x - start.x, first.y - start.y);
    let t = (ax * ey - ay * ex) / denominator;
    let u = (ax * dy - ay * dx) / denominator;
    (-1e-9 <= t && t <= 1.0 + 1e-9 && -1e-9 <= u && u <= 1.0 + 1e-9).then_some(t)
}

/// `_segment_intersection`.
pub fn segment_intersection(start: Pt, end: Pt, first: Pt, second: Pt) -> Option<Pt> {
    if let Some(t) = segment_intersection_parameter(start, end, first, second) {
        return Some(Pt { x: start.x + (end.x - start.x) * t, y: start.y + (end.y - start.y) * t });
    }
    let (dx, dy) = (end.x - start.x, end.y - start.y);
    let denominator = dx * dx + dy * dy;
    if denominator <= 1e-18 || (dx * (first.y - start.y) - dy * (first.x - start.x)).abs() > 1e-9 {
        return None;
    }
    let first_t = ((first.x - start.x) * dx + (first.y - start.y) * dy) / denominator;
    let second_t = ((second.x - start.x) * dx + (second.y - start.y) * dy) / denominator;
    let overlap_start = py_max(0.0, py_min(first_t, second_t));
    let overlap_end = py_min(1.0, py_max(first_t, second_t));
    if overlap_start > overlap_end + 1e-9 {
        return None;
    }
    let midpoint = (overlap_start + overlap_end) / 2.0;
    Some(Pt { x: start.x + dx * midpoint, y: start.y + dy * midpoint })
}

/// `zip(polygon, (*polygon[1:], polygon[0]))`.
fn sides(polygon: &[Pt]) -> impl Iterator<Item = (Pt, Pt)> + '_ {
    polygon.iter().copied().zip(polygon.iter().copied().skip(1).chain(polygon.first().copied()))
}

/// `_segment_inside_polygon`.
pub fn segment_inside_polygon(start: Pt, end: Pt, polygon: &[Pt]) -> bool {
    let mut cuts = vec![0.0, 1.0];
    cuts.extend(sides(polygon).filter_map(|(first, second)| segment_intersection_parameter(start, end, first, second)));
    // sorted(set(cuts)): 0.0 is first in the list, so a -0.0 goes as a set would drop it.
    cuts.sort_by(|first, second| first.partial_cmp(second).unwrap_or(std::cmp::Ordering::Equal));
    cuts.dedup_by(|later, kept| *later == *kept);
    cuts.windows(2).all(|pair| {
        let middle = (pair[0] + pair[1]) / 2.0;
        point_in_polygon(Pt { x: start.x + (end.x - start.x) * middle, y: start.y + (end.y - start.y) * middle }, polygon)
    })
}

fn segment_hits_polygon(start: Pt, end: Pt, polygon: &[Pt]) -> bool {
    if point_in_polygon(start, polygon) || point_in_polygon(end, polygon) {
        return true;
    }
    if sides(polygon).any(|(first, second)| segment_intersection(start, end, first, second).is_some()) {
        return true;
    }
    point_in_polygon(Pt { x: (start.x + end.x) / 2.0, y: (start.y + end.y) / 2.0 }, polygon)
}

pub fn point_segment_distance(point: Pt, start: Pt, end: Pt) -> f64 {
    let (dx, dy) = (end.x - start.x, end.y - start.y);
    let denominator = dx * dx + dy * dy;
    if denominator <= 1e-18 {
        return distance(point, start);
    }
    let fraction = py_min(1.0, py_max(0.0, ((point.x - start.x) * dx + (point.y - start.y) * dy) / denominator));
    distance(point, Pt { x: start.x + fraction * dx, y: start.y + fraction * dy })
}

/// `_segment_clear_of_polygon`.
pub fn segment_clear_of_polygon(start: Pt, end: Pt, polygon: &[Pt], clearance: f64) -> bool {
    if segment_hits_polygon(start, end, polygon) {
        return false;
    }
    sides(polygon).all(|(first, second)| {
        let nearest = py_min(
            py_min(
                py_min(point_segment_distance(start, first, second), point_segment_distance(end, first, second)),
                point_segment_distance(first, start, end),
            ),
            point_segment_distance(second, start, end),
        );
        nearest + 1e-9 >= clearance
    })
}

/// The clearance of `route_is_physically_clear` and `_room_segment_clear`, by the mobility's value.
pub fn walking_clearance(mobility: &str) -> Option<f64> {
    match mobility {
        "independent" => Some(0.25),
        "assisted" => Some(0.4),
        "wheelchair" => Some(0.45),
        _ => None,
    }
}

/// `_point_along_polyline`, for points that are not empty. None when `math.fsum` would not be the plain case.
pub fn point_along_polyline(points: &[Pt], progress: f64) -> Option<Pt> {
    let lengths: Vec<f64> = points.windows(2).map(|pair| distance(pair[0], pair[1])).collect();
    let total = py_fsum(&lengths)?;
    let last = points[points.len() - 1];
    if total <= 1e-12 {
        return Some(last);
    }
    let mut remaining = total * py_min(1.0, py_max(0.0, progress));
    for (pair, &length) in points.windows(2).zip(&lengths) {
        if remaining <= length {
            let ratio = if length <= 1e-12 { 0.0 } else { remaining / length };
            let (first, second) = (pair[0], pair[1]);
            return Some(Pt { x: first.x + (second.x - first.x) * ratio, y: first.y + (second.y - first.y) * ratio });
        }
        remaining -= length;
    }
    Some(last)
}

// ---- The plan ----------------------------------------------------------------------------------

#[derive(Clone)]
pub struct Zone {
    pub polygon: Vec<Pt>,
    pub usable_area: f64,
    /// `_zone_center`. Err for a centre `Point2D` would refuse.
    pub center: std::result::Result<Option<Pt>, ()>,
}

#[derive(Clone)]
pub struct Route {
    pub points: Vec<Pt>,
    pub width: f64,
    pub accessible: bool,
}

/// A `FacilityServicePoint`, as a rule reads it.
#[derive(Clone)]
pub struct ServicePoint {
    pub id: Arc<str>,
    pub facility_id: Arc<str>,
    pub point: Pt,
    pub service_capacity: i64,
    pub queue_id: Arc<str>,
    pub accessibility: Arc<str>,
    pub required_access_tags: Vec<Arc<str>>,
    pub required_staff_skills: Vec<Arc<str>>,
}

/// `effective_geometry(world)`: the Scenario's plan with the overlay's moved facilities.
pub struct Layout {
    pub zones: IdMap<Zone>,
    pub routes: IdMap<Route>,
    /// The Scenario's points, then the overlay's moves in the overlay's order: a moved facility
    /// keeps its place, and one the Scenario has no point for comes after, as `{**points,
    /// **moves}` orders them. The only order here that follows the overlay; no rule reads it
    /// (they look a point up, or ask whether any point is in a polygon).
    pub facility_points: IdMap<Pt>,
    /// With the overlay's moves. `signed_service_points` are the Scenario's own.
    pub service_points: Vec<ServicePoint>,
    pub signed_service_points: Vec<ServicePoint>,
}

#[derive(Clone)]
pub struct Wall {
    pub id: Arc<str>,
    pub start: Pt,
    pub end: Pt,
}

#[derive(Clone)]
pub struct Opening {
    pub wall_id: Arc<str>,
    pub center: Pt,
    pub width: f64,
    pub accessible: bool,
    pub edge_ids: Vec<Arc<str>>,
}

/// `effective_spatial_constraints(world)`.
pub struct Constraints {
    pub walls: Vec<Wall>,
    pub openings: Vec<Opening>,
    /// Obstacles, then the overlay's partitions in ID order, then equipment footprints.
    pub blocked: Vec<Vec<Pt>>,
    /// The equipment footprints, by facility.
    pub footprints: Vec<(Arc<str>, Vec<Pt>)>,
    /// How many of `blocked` are obstacles, the rest being the footprints.
    pub obstacle_count: usize,
}

/// What a route found in a room depends on, beside the plan it is kept in.
#[derive(Eq, Hash, PartialEq)]
struct RouteKey {
    zone_id: Arc<str>,
    mobility: &'static str,
    start: Option<(u64, u64)>,
    destination: (u64, u64),
}

/// What `_geometry(world)` and `_spatial_constraints(world)` return.
#[derive(Default)]
pub struct Geo {
    pub layout: Option<Layout>,
    pub constraints: Option<Constraints>,
    /// `plan_in_room_route`'s memo. A route is a pure function of this plan and its key.
    routes: Mutex<HashMap<RouteKey, Option<Arc<[Pt]>>, FxBuildHasher>>,
}

/// `_zone_center` of one polygon.
pub fn zone_center(polygon: &[Pt]) -> std::result::Result<Option<Pt>, ()> {
    let crosses: Vec<f64> = sides(polygon).map(|(a, b)| a.x * b.y - b.x * a.y).collect();
    let twice_area = py_sum(&crosses);
    let count = polygon.len() as f64;
    let candidate = if twice_area.abs() < 1e-12 {
        let xs: Vec<f64> = polygon.iter().map(|point| point.x).collect();
        let ys: Vec<f64> = polygon.iter().map(|point| point.y).collect();
        Pt { x: py_sum(&xs) / count, y: py_sum(&ys) / count }
    } else {
        let xs: Vec<f64> = sides(polygon).map(|(a, b)| (a.x + b.x) * (a.x * b.y - b.x * a.y)).collect();
        let ys: Vec<f64> = sides(polygon).map(|(a, b)| (a.y + b.y) * (a.x * b.y - b.x * a.y)).collect();
        Pt { x: py_sum(&xs) / (3.0 * twice_area), y: py_sum(&ys) / (3.0 * twice_area) }
    };
    if !candidate.x.is_finite() || !candidate.y.is_finite() {
        return Err(());
    }
    if point_in_polygon(candidate, polygon) {
        return Ok(Some(candidate));
    }
    // The area centroid of a concave room may lie outside its walls.
    let orientation = if twice_area > 0.0 { 1.0 } else { -1.0 };
    let extent = |value: fn(&Pt) -> f64| {
        let values = polygon.iter().map(value);
        let highest = values.clone().fold(f64::NEG_INFINITY, py_max);
        highest - values.fold(f64::INFINITY, py_min)
    };
    let scale = py_max(py_max(extent(|point| point.x), extent(|point| point.y)), 1.0);
    for (first, second) in sides(polygon) {
        let (dx, dy) = (second.x - first.x, second.y - first.y);
        let length = hypot(&[dx, dy]);
        if length <= 1e-12 {
            continue;
        }
        for fraction in [1e-7, 1e-5, 1e-3, 1e-2] {
            let offset = scale * fraction;
            let candidate = Pt {
                x: (first.x + second.x) / 2.0 - orientation * dy / length * offset,
                y: (first.y + second.y) / 2.0 + orientation * dx / length * offset,
            };
            if !candidate.x.is_finite() || !candidate.y.is_finite() {
                return Err(());
            }
            if point_in_polygon(candidate, polygon) {
                return Ok(Some(candidate));
            }
        }
    }
    Ok(None)
}

/// A `Point2D` of the overlay, for what `Point2D.model_validate` takes as it is.
pub fn point_in_value(value: &V) -> Result<Pt> {
    let V::Map(fields) = value else {
        return Err(Error::unsupported("a point is not a dict"));
    };
    let (Some(V::Float(x)), Some(V::Float(y)), 2) = (fields.get("x"), fields.get("y"), fields.len()) else {
        return Err(Error::unsupported("a point is not two floats"));
    };
    if !usable(*x, *y) {
        return Err(Error::unsupported("a point is not a usable point"));
    }
    Ok(Pt { x: *x, y: *y })
}

pub fn points_in_value(value: &V) -> Result<Vec<Pt>> {
    let V::List(points) = value else {
        return Err(Error::unsupported("points are not a list"));
    };
    points.iter().map(point_in_value).collect()
}

/// A point the arithmetic of a rule gave: `Point2D` takes what is finite, and `usable` is less.
fn moved(point: Pt, new: Pt, old: Pt) -> Result<Pt> {
    let moved = Pt { x: point.x + new.x - old.x, y: point.y + new.y - old.y };
    if !usable(moved.x, moved.y) {
        return Err(Error::unsupported("a moved point is not a usable point"));
    }
    Ok(moved)
}

impl Layout {
    /// `effective_geometry`: the Scenario's plan with the overlay's moved facilities (`moves`, the
    /// overlay's `facility_points`, when it has entries).
    pub fn of(plan: &FacilityPlan, moves: Option<&V>) -> Result<Layout> {
        let mut layout = Layout {
            zones: plan.zones.clone(),
            routes: plan.routes.clone(),
            facility_points: plan.facility_points.clone(),
            service_points: plan.service_points.clone(),
            signed_service_points: plan.service_points.clone(),
        };
        let Some(moves) = moves else {
            return Ok(layout);
        };
        let V::Map(moves) = moves else {
            return Err(Error::unsupported("the overlay's moves are not a dict"));
        };
        if moves.is_empty() {
            return Ok(layout);
        }
        let mut new_points = IdMap::default();
        for (id, row) in moves {
            let V::Map(row) = row else {
                return Err(Error::unsupported("a move is not a dict"));
            };
            let point = point_in_value(row.get("point_m").unwrap_or(&V::Null))?;
            new_points.insert(id.clone(), point);
        }
        for point in &mut layout.service_points {
            let Some(&new) = new_points.get(&*point.facility_id) else {
                continue;
            };
            let Some(&old) = layout.facility_points.get(&*point.facility_id) else {
                return Err(Error::unsupported("a moved service point has no point of its facility"));
            };
            point.point = moved(point.point, new, old)?;
        }
        for (id, point) in new_points {
            layout.facility_points.insert(id, point);
        }
        Ok(layout)
    }
}

impl Constraints {
    /// `effective_spatial_constraints`: the Scenario's with the partitions as obstacles, in ID
    /// order, and the footprints of moved facilities moved with them. `signed` is the Scenario's
    /// facility points, which a moved footprint is moved from.
    pub fn of(
        spatial: &SpatialConstraints,
        partitions: Option<&V>,
        moves: Option<&V>,
        signed: Option<&IdMap<Pt>>,
    ) -> Result<Constraints> {
        let mut blocked = spatial.obstacles.clone();
        if let Some(partitions) = partitions {
            let V::Map(partitions) = partitions else {
                return Err(Error::unsupported("the partitions are not a dict"));
            };
            let mut sorted: Vec<(&Arc<str>, &V)> = partitions.iter().collect();
            sorted.sort_by(|first, second| first.0.as_bytes().cmp(second.0.as_bytes()));
            for (_, row) in sorted {
                let V::Map(row) = row else {
                    return Err(Error::unsupported("a partition is not a dict"));
                };
                // SpatialObstacle(id=key, ...) takes 3 to 128 corners, and its ID and sources the
                // overlay's own validation reads: anything it could refuse goes to the Python rule.
                let polygon = points_in_value(row.get("polygon_m").unwrap_or(&V::Null))?;
                if !(3..=128).contains(&polygon.len()) {
                    return Err(Error::unsupported("a partition has another number of corners"));
                }
                if !matches!(row.get("source_ids"), Some(V::List(sources)) if sources.iter().all(|id| matches!(id, V::Str(id) if is_identifier(id)))) {
                    return Err(Error::unsupported("a partition's source is not an identifier"));
                }
                blocked.push(polygon);
            }
            if partitions.keys().any(|id| !is_identifier(id)) {
                return Err(Error::unsupported("a partition's ID is not an identifier"));
            }
        }
        let obstacle_count = blocked.len();
        let moves = match moves {
            Some(V::Map(moves)) => Some(moves),
            Some(_) => return Err(Error::unsupported("the overlay's moves are not a dict")),
            None => None,
        };
        let mut footprints = Vec::with_capacity(spatial.footprints.len());
        for (id, polygon) in &spatial.footprints {
            let mut polygon = polygon.clone();
            if let Some(V::Map(row)) = moves.and_then(|moves| moves.get(&**id)) {
                let Some(signed) = signed else {
                    return Err(Error::unsupported("a moved footprint without the Scenario's plan"));
                };
                let Some(&old) = signed.get(&**id) else {
                    return Err(Error::unsupported("a moved footprint has no point of its facility"));
                };
                let new = point_in_value(row.get("point_m").unwrap_or(&V::Null))?;
                for point in &mut polygon {
                    *point = moved(*point, new, old)?;
                }
            } else if moves.is_some_and(|moves| moves.contains_key(&**id)) {
                return Err(Error::unsupported("a move is not a dict"));
            }
            blocked.push(polygon.clone());
            footprints.push((id.clone(), polygon));
        }
        Ok(Constraints {
            walls: spatial.walls.clone(),
            openings: spatial.openings.clone(),
            blocked,
            footprints,
            obstacle_count,
        })
    }
}

impl Geo {
    /// The plan of a world: the Scenario's facility geometry (None without one) and spatial
    /// constraints (None where `_spatial_constraints` gives None), with the overlay's facility
    /// moves and partitions where it has entries. The overlay is read by key, and the partitions
    /// in ID order, so only `Layout.facility_points` follows the overlay's order.
    pub fn new(
        facility: Option<&FacilityPlan>,
        spatial: Option<&SpatialConstraints>,
        moves: Option<&V>,
        partitions: Option<&V>,
    ) -> Result<Geo> {
        let layout = match facility {
            Some(plan) => Some(Layout::of(plan, moves)?),
            None => None,
        };
        let constraints = match spatial {
            Some(spatial) => Some(Constraints::of(spatial, partitions, moves, facility.map(|plan| &plan.facility_points))?),
            None => None,
        };
        Ok(Geo { layout, constraints, routes: Mutex::default() })
    }

    /// `_zone_center`. Unsupported when Python would raise.
    pub fn center(&self, zone_id: &str) -> Result<Option<Pt>> {
        let Some(zone) = self.layout.as_ref().and_then(|layout| layout.zones.get(zone_id)) else {
            return Ok(None);
        };
        zone.center.map_err(|()| Error::unsupported("the centre of a zone is not finite"))
    }

    /// `_room_segment_clear`, for a zone polygon.
    pub fn room_segment_clear(constraints: &Constraints, start: Pt, end: Pt, polygon: &[Pt], mobility: &str) -> bool {
        let Some(clearance) = walking_clearance(mobility) else {
            return false;
        };
        if !segment_inside_polygon(start, end, polygon) {
            return false;
        }
        if constraints.blocked.iter().any(|blocked| !segment_clear_of_polygon(start, end, blocked, clearance)) {
            return false;
        }
        constraints.walls.iter().all(|wall| {
            let Some(crossing) = segment_intersection(start, end, wall.start, wall.end) else {
                return true;
            };
            constraints.openings.iter().any(|item| {
                item.wall_id == wall.id
                    && item.width + 1e-9 >= 2.0 * clearance
                    && (item.accessible || mobility == "independent")
                    && distance(crossing, item.center) <= item.width / 2.0 - clearance + 1e-8
            })
        })
    }

    /// `route_is_physically_clear`.
    pub fn route_is_clear(&self, edge_id: &str, mobility: &str) -> bool {
        let Some(layout) = &self.layout else {
            return true;
        };
        let route = layout.routes.get(edge_id);
        let clearance = walking_clearance(mobility);
        let (Some(route), Some(clearance)) = (route, clearance) else {
            return false;
        };
        if !route.accessible && (mobility == "wheelchair" || mobility == "assisted") {
            return false;
        }
        if route.width + 1e-9 < 2.0 * clearance {
            return false;
        }
        let Some(constraints) = &self.constraints else {
            return true;
        };
        let openings: Vec<&Opening> =
            constraints.openings.iter().filter(|item| item.edge_ids.iter().any(|id| &**id == edge_id)).collect();
        for pair in route.points.windows(2) {
            let (first, second) = (pair[0], pair[1]);
            if constraints.blocked.iter().any(|polygon| !segment_clear_of_polygon(first, second, polygon, clearance)) {
                return false;
            }
            for wall in &constraints.walls {
                let Some(crossing) = segment_intersection(first, second, wall.start, wall.end) else {
                    continue;
                };
                let allowed = openings.iter().any(|opening| {
                    opening.wall_id == wall.id
                        && (opening.accessible || mobility == "independent")
                        && opening.width + 1e-9 >= 2.0 * clearance
                        && distance(crossing, opening.center) <= opening.width / 2.0 - clearance + 1e-9
                });
                if !allowed {
                    return false;
                }
            }
        }
        true
    }
}

// ---- The plans a world keeps --------------------------------------------------------------------

/// What a plan was built from beside the configs: the overlay's state, by its stamp, and by the
/// canonical bytes of its two parts the plan reads (empty for no entries).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverlayKey {
    /// Whether `model_state` has the overlay at all: spatial constraints are built with it.
    pub present: bool,
    /// The stamp of the overlay's slot, where one follows it.
    pub stamp: Option<Stamp>,
    pub moves: Vec<u8>,
    pub partitions: Vec<u8>,
}

/// The plans of one `Configs` (so of one stamp of `model_configs`), by the overlay they were
/// built with, the latest first. A stamp hit needs no read of the overlay. When the overlay's stamp
/// changed (a reservation, a history row), the bytes of its two parts are compared, as before
/// #152: a plan whose parts are the same serves the new stamp too. The bytes are canonical JSON,
/// keys sorted, so two overlays that differ only in order share a plan (see `Layout`).
#[derive(Default)]
pub struct GeoCache {
    entries: Vec<(OverlayKey, Arc<Geo>)>,
}

/// Plans kept for one configs stamp.
pub const CACHED_PLANS: usize = 8;

impl GeoCache {
    /// The plan built with an overlay of this stamp.
    pub fn by_stamp(&mut self, stamp: Stamp) -> Option<Arc<Geo>> {
        let found = self.entries.iter().position(|(key, _)| key.stamp == Some(stamp))?;
        Some(self.promote(found))
    }

    /// The plan built with these parts of an overlay; it now serves `key.stamp` too.
    pub fn by_bytes(&mut self, key: &OverlayKey) -> Option<Arc<Geo>> {
        let found = self.entries.iter().position(|(kept, _)| {
            kept.present == key.present && kept.moves == key.moves && kept.partitions == key.partitions
        })?;
        if key.stamp.is_some() {
            self.entries[found].0.stamp = key.stamp;
        }
        Some(self.promote(found))
    }

    /// Whether the plan kept for this stamp was built from these parts (a debug check that the
    /// overlay's stamp changes with each write).
    pub fn holds(&self, key: &OverlayKey) -> bool {
        self.entries.iter().any(|(kept, _)| {
            kept.stamp == key.stamp && kept.present == key.present && kept.moves == key.moves && kept.partitions == key.partitions
        })
    }

    pub fn insert(&mut self, key: OverlayKey, geo: Arc<Geo>) {
        self.entries.insert(0, (key, geo));
        self.entries.truncate(CACHED_PLANS);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn promote(&mut self, found: usize) -> Arc<Geo> {
        if found != 0 {
            let entry = self.entries.remove(found);
            self.entries.insert(0, entry);
        }
        self.entries[0].1.clone()
    }
}

impl Geo {
    /// `plan_in_room_route`. Err for what the Python function would raise or a kernel doesn't decide.
    pub fn room_route(
        &self,
        zone_id: &str,
        start: Option<Pt>,
        destination: Pt,
        mobility: &'static str,
    ) -> std::result::Result<Option<Arc<[Pt]>>, &'static str> {
        let key = RouteKey {
            zone_id: Arc::from(zone_id),
            mobility,
            start: start.map(|point| (point.x.to_bits(), point.y.to_bits())),
            destination: (destination.x.to_bits(), destination.y.to_bits()),
        };
        if let Ok(routes) = self.routes.lock() {
            if let Some(route) = routes.get(&key) {
                return Ok(route.clone());
            }
        }
        let route = self.find_room_route(zone_id, start, destination, mobility)?;
        let route: Option<Arc<[Pt]>> = route.map(Arc::from);
        if let Ok(mut routes) = self.routes.lock() {
            if routes.len() >= REMEMBERED_ROUTES {
                routes.clear();
            }
            routes.insert(key, route.clone());
        }
        Ok(route)
    }

    /// `_plan_in_room_route`.
    fn find_room_route(
        &self,
        zone_id: &str,
        start: Option<Pt>,
        destination: Pt,
        mobility: &str,
    ) -> std::result::Result<Option<Vec<Pt>>, &'static str> {
        const REFUSED: &str = "the centre of a zone is not finite";
        let Some(zone) = self.layout.as_ref().and_then(|layout| layout.zones.get(zone_id)) else {
            return Ok(None);
        };
        let origin = match start {
            Some(start) => Some(start),
            None => zone.center.map_err(|()| REFUSED)?,
        };
        let Some(constraints) = &self.constraints else {
            return Ok(origin.filter(|origin| segment_inside_polygon(*origin, destination, &zone.polygon)).map(|origin| vec![origin, destination]));
        };
        let Some(clearance) = walking_clearance(mobility) else {
            return Ok(None);
        };
        let Some(origin) = origin else {
            return Ok(None);
        };
        let walkable = |point: Pt| point_walkable(point, &zone.polygon, &constraints.blocked);
        if !walkable(origin) || !walkable(destination) {
            return Ok(None);
        }
        let mut nodes = vec![origin, destination];
        let clearance = clearance + 0.05;
        for polygon in &constraints.blocked {
            let area = py_fsum(&sides(polygon).map(|(a, b)| a.x * b.y - b.x * a.y).collect::<Vec<f64>>())
                .ok_or("an area is not a plain sum")?;
            let orientation = if area > 0.0 { 1.0 } else { -1.0 };
            let count = polygon.len();
            for (index, &corner) in polygon.iter().enumerate() {
                let previous = polygon[(index + count - 1) % count];
                let following = polygon[(index + 1) % count];
                let incoming = (corner.x - previous.x, corner.y - previous.y);
                let outgoing = (following.x - corner.x, following.y - corner.y);
                let turn = incoming.0 * outgoing.1 - incoming.1 * outgoing.0;
                if turn * orientation <= 1e-10 {
                    continue;
                }
                let first_length = hypot(&[incoming.0, incoming.1]);
                let second_length = hypot(&[outgoing.0, outgoing.1]);
                if first_length <= 1e-9 || second_length <= 1e-9 {
                    continue;
                }
                let normal_first = (orientation * incoming.1 / first_length, -orientation * incoming.0 / first_length);
                let normal_second = (orientation * outgoing.1 / second_length, -orientation * outgoing.0 / second_length);
                let miter = (normal_first.0 + normal_second.0, normal_first.1 + normal_second.1);
                let miter_length = hypot(&[miter.0, miter.1]);
                if miter_length <= 1e-9 {
                    continue;
                }
                let miter = (miter.0 / miter_length, miter.1 / miter_length);
                let scale = clearance / (miter.0 * normal_first.0 + miter.1 * normal_first.1);
                if !scale.is_finite() || scale <= 0.0 || scale > 20.0 {
                    continue;
                }
                let candidate = Pt { x: corner.x + miter.0 * scale, y: corner.y + miter.1 * scale };
                if walkable(candidate) {
                    nodes.push(candidate);
                }
            }
        }
        for wall in &constraints.walls {
            for opening in constraints.openings.iter().filter(|item| item.wall_id == wall.id) {
                let (dx, dy) = (wall.end.x - wall.start.x, wall.end.y - wall.start.y);
                let length = hypot(&[dx, dy]);
                if length <= 1e-9 {
                    continue;
                }
                let (nx, ny) = (-dy / length * clearance, dx / length * clearance);
                for side in [-1.0, 1.0] {
                    let candidate = Pt { x: opening.center.x + nx * side, y: opening.center.y + ny * side };
                    if walkable(candidate) {
                        nodes.push(candidate);
                    }
                }
            }
        }
        let key_of = |point: Pt| -> std::result::Result<(f64, f64), &'static str> {
            Ok((py_round(point.x, 8).ok_or("a round is not plain")?, py_round(point.y, 8).ok_or("a round is not plain")?))
        };
        // {key: point} of the Python rule: the later point takes a key's place, and -0.0 is 0.0.
        let mut unique: Vec<((f64, f64), Pt)> = Vec::with_capacity(nodes.len());
        let mut put = |key: (f64, f64), point: Pt| match unique.iter_mut().find(|(known, _)| *known == key) {
            Some(slot) => slot.1 = point,
            None => unique.push((key, point)),
        };
        for &point in &nodes {
            put(key_of(point)?, point);
        }
        let (origin_key, destination_key) = (key_of(origin)?, key_of(destination)?);
        put(destination_key, destination);
        put(origin_key, origin);
        unique.sort_by(|first, second| first.0.partial_cmp(&second.0).unwrap_or(Ordering::Equal));
        let start_index = unique.iter().position(|(key, _)| *key == origin_key).ok_or("no origin")?;
        let end_index = unique.iter().position(|(key, _)| *key == destination_key).ok_or("no destination")?;
        let nodes: Vec<Pt> = unique.iter().map(|(_, point)| *point).collect();
        let mut graph: Vec<Vec<(f64, u32)>> = vec![Vec::new(); nodes.len()];
        for left in 0..nodes.len() {
            for right in left + 1..nodes.len() {
                if Self::room_segment_clear(constraints, nodes[left], nodes[right], &zone.polygon, mobility) {
                    let length = distance(nodes[left], nodes[right]);
                    graph[left].push((length, right as u32));
                    graph[right].push((length, left as u32));
                }
            }
        }
        for adjacent in &mut graph {
            adjacent.sort_by_key(|item| item.1);
        }
        let mut best: Vec<Option<(f64, Vec<u32>)>> = vec![None; nodes.len()];
        best[start_index] = Some((0.0, vec![start_index as u32]));
        let mut pending = BinaryHeap::new();
        pending.push(Walk { distance: 0.0, path: vec![start_index as u32], current: start_index as u32 });
        while let Some(Walk { distance: walked, path, current }) = pending.pop() {
            let at = current as usize;
            if best[at].as_ref().is_none_or(|(known, known_path)| *known != walked || *known_path != path) {
                continue;
            }
            if at == end_index {
                return Ok(Some(path.iter().map(|index| nodes[*index as usize]).collect()));
            }
            for &(weight, adjacent) in &graph[at] {
                let next_distance = walked + weight;
                let mut next_path = path.clone();
                next_path.push(adjacent);
                let better = match &best[adjacent as usize] {
                    None => true,
                    Some((known, known_path)) => {
                        next_distance < *known || (next_distance == *known && next_path < *known_path)
                    }
                };
                if better {
                    best[adjacent as usize] = Some((next_distance, next_path.clone()));
                    pending.push(Walk { distance: next_distance, path: next_path, current: adjacent });
                }
            }
        }
        Ok(None)
    }
}

/// `_point_walkable`.
pub fn point_walkable(point: Pt, zone: &[Pt], blocked: &[Vec<Pt>]) -> bool {
    point_in_polygon(point, zone) && !blocked.iter().any(|polygon| point_in_polygon(point, polygon))
}

/// An entry of the Python rule's heap: the least `(distance, path, current)` comes first.
struct Walk {
    distance: f64,
    path: Vec<u32>,
    current: u32,
}

impl Walk {
    fn order(&self, other: &Self) -> Ordering {
        self.distance
            .partial_cmp(&other.distance)
            .unwrap_or(Ordering::Equal)
            .then_with(|| self.path.cmp(&other.path))
            .then_with(|| self.current.cmp(&other.current))
    }
}

impl PartialEq for Walk {
    fn eq(&self, other: &Self) -> bool {
        self.order(other) == Ordering::Equal
    }
}

impl Eq for Walk {}

impl PartialOrd for Walk {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Walk {
    /// Reversed, since `BinaryHeap` pops the greatest.
    fn cmp(&self, other: &Self) -> Ordering {
        other.order(self)
    }
}

/// `_polyline_length`. None when `math.fsum` is not the plain case.
pub fn polyline_length(points: &[Pt]) -> Option<f64> {
    let lengths: Vec<f64> = points.windows(2).map(|pair| distance(pair[0], pair[1])).collect();
    py_fsum(&lengths)
}
