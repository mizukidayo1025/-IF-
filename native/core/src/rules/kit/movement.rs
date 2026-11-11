//! What the queue rules ask of `domain/movement.py` (lane c1 of Issue #152; design-p8.md, section
//! 3.1: movement and the queues are one strongly connected component). The places of a queue, the
//! walk to a facility's point, the minutes of a walk in a room, and the plan of a journey's start
//! as changes of the row of `movement_v1`. The PyO3 crate's `rules/movement.rs` calls these for
//! its own kernels until its lane moves them, and keeps the tree of an attached namespace in step
//! itself (`write_tree`); a kernel of the core writes the row alone, which its tree then follows
//! (lane P8-0c).
//!
//! The errors are the core's, with the texts the PyO3 crate's helpers had.

use std::sync::{Arc, LazyLock};

use crate::config::movement::MovementModel;
use crate::config::read::Read;
use crate::entities::Entities;
use crate::error::{Error, Result};
use crate::geometry::{distance, point_along_polyline, point_walkable, polyline_length, usable, Geo, Pt};
use crate::mathx::{hypot, py_fsum, py_max, py_min};
use crate::rules::kit::same_position;
use crate::state::movement::{Journey, MovementRow, Point, Position};
use crate::world::MODEL_CONFIGS;

pub use crate::config::movement::{EXACT_INT, MOVEMENT_SOURCE_ID, POLICY_VERSION};

/// The key of `movement_v1` in `model_state`.
pub const KEY: &str = "movement_v1";

// `_queue_slot_position`'s spacing and `facility_approach_route`'s clearance, in metres.
const SPACING_INDEPENDENT: f64 = 0.6;
const SPACING_ASSISTED: f64 = 0.9;
const SPACING_WHEELCHAIR: f64 = 1.0;
const APPROACH_INDEPENDENT: f64 = 0.30;
const APPROACH_ASSISTED: f64 = 0.45;
const APPROACH_WHEELCHAIR: f64 = 0.50;

/// The kinds of a position record, shared.
pub struct Kinds {
    pub physical: Arc<str>,
    pub display_only: Arc<str>,
    pub unavailable: Arc<str>,
}

pub static KINDS: LazyLock<Kinds> = LazyLock::new(|| Kinds {
    physical: Arc::from("physical"),
    display_only: Arc::from("display_only"),
    unavailable: Arc::from("unavailable"),
});

/// A journey to a place of a facility's queue or service.
pub fn is_queue_trip(purpose: &str) -> bool {
    matches!(
        purpose,
        "toilet_queue_position" | "resource_queue_position" | "toilet_queue_service" | "resource_queue_service"
    )
}

/// The point of a position record.
pub fn position_point(position: Option<&Position>) -> Option<Pt> {
    position.and_then(|position| position.point).map(|point| Pt { x: point.x, y: point.y })
}

/// `_position_record`, with its kind.
pub fn record(zone_id: Arc<str>, point: Option<Pt>, position_kind: Arc<str>, edge_id: Option<Arc<str>>, progress: f64) -> Position {
    Position { zone_id, point: point.map(|point| Point { x: point.x, y: point.y }), edge_id, progress, position_kind }
}

/// A refusal of the plan's route search (`Geo::room_route`).
#[track_caller]
pub fn refused(reason: &'static str) -> Error {
    Error::unsupported(reason)
}

/// A section's value as a read site takes it: a refusal is its `Unsupported`, said here; any other
/// fault is a corrupt `model_configs` (`configs::read` of the PyO3 crate).
#[track_caller]
pub fn config_read<T>(value: Read<T>) -> Result<T> {
    match value {
        Ok(value) => Ok(value),
        Err(fault) if fault.is_refusal() => Err(Error::unsupported(fault.text.to_string())),
        Err(_) => Err(Error::corrupt(MODEL_CONFIGS, "a value of a section holds an error")),
    }
}

// ---- Queue slots and the walk to a facility --------------------------------------------------

/// `_queue_slot_position`: the slot first free of the people ahead as they stand now, with a real
/// route from where the person is. Of a facility's own queue, or of an independent one when
/// `independent` gives its people and its signed point if it has one.
pub fn queue_slot_in(
    geo: &Geo,
    row: &MovementRow,
    rows: &Entities,
    agent_id: &str,
    facility_id: &str,
    independent: Option<(&[Arc<str>], Option<Pt>)>,
) -> Result<Option<Pt>> {
    let Some(facility) = rows.facilities.get(facility_id) else {
        return Ok(None);
    };
    let Some(layout) = &geo.layout else {
        return Ok(None);
    };
    let signed = independent.and_then(|(_, point)| point);
    let own = signed.or_else(|| layout.facility_points.get(facility_id).copied());
    let (Some(facility_point), Some(zone)) = (own, layout.zones.get(&*facility.zone_id)) else {
        return Ok(None);
    };
    let holds = |queue: &[Arc<str>]| queue.iter().position(|id| &**id == agent_id);
    let (queue, slot_index): (&[Arc<str>], Option<usize>) = match independent {
        Some((members, _)) => (members, holds(members)),
        None => match holds(&facility.resource_queue_agent_ids) {
            Some(slot) => (&facility.resource_queue_agent_ids, Some(slot)),
            None => (&facility.queue_agent_ids, holds(&facility.queue_agent_ids)),
        },
    };
    let Some(slot_index) = slot_index else {
        return Ok(Some(facility_point));
    };
    let Some(anchor) = geo.center(&facility.zone_id)? else {
        return Ok(None);
    };
    let (dx, dy) = (anchor.x - facility_point.x, anchor.y - facility_point.y);
    let length = hypot(&[dx, dy]);
    if length <= 1e-9 {
        return Ok(None);
    }
    let (ux, uy) = (dx / length, dy / length);
    let Some(agent) = rows.agents.get(agent_id) else {
        return Err(Error::unsupported("no such agent"));
    };
    let mobility = super::mobility_text(agent);
    let spacing = match mobility {
        "independent" => SPACING_INDEPENDENT,
        "assisted" => SPACING_ASSISTED,
        _ => SPACING_WHEELCHAIR,
    };
    let occupied: Vec<Pt> =
        queue[..slot_index].iter().filter_map(|other| position_point(row.positions.get(&**other))).collect();
    if occupied.iter().any(|point| !usable(point.x, point.y)) {
        return Err(Error::unsupported("a position is not a usable point"));
    }
    let start = position_point(row.positions.get(agent_id)).or(occupied.last().copied()).unwrap_or(anchor);
    if !usable(start.x, start.y) {
        return Err(Error::unsupported("a position is not a usable point"));
    }
    let no_blocks = Vec::new();
    let blocked = geo.constraints.as_ref().map_or(&no_blocks, |constraints| &constraints.blocked);
    for ring in slot_index + 1..slot_index + 65 {
        for side in [0.0, 1.0, -1.0, 2.0, -2.0] {
            let along = spacing * ring as f64;
            let across = spacing * side;
            let candidate =
                Pt { x: facility_point.x + ux * along - uy * across, y: facility_point.y + uy * along + ux * across };
            if point_walkable(candidate, &zone.polygon, blocked)
                && occupied.iter().all(|other| distance(candidate, *other) >= spacing)
                && geo.room_route(&facility.zone_id, Some(start), candidate, mobility).map_err(refused)?.is_some()
            {
                return Ok(Some(candidate));
            }
        }
    }
    Ok(None)
}

/// `in_room_travel_minutes` of a path of this length, for a person standing in `agent_zone`.
pub fn room_minutes(model: &Read<MovementModel>, geo: &Geo, rows: &Entities, agent_zone: &str, mobility: &str, length: f64) -> Result<i64> {
    let Some(layout) = &geo.layout else {
        return Err(Error::unsupported("no floor plan"));
    };
    let model = config_read(model.clone())?;
    let speed = model.speed(mobility);
    let loads = rows.agents.iter().filter(|(_, other)| &*other.zone_id == agent_zone).count();
    let density = match layout.zones.get(agent_zone).map(|zone| zone.usable_area).filter(|area| *area > 0.0) {
        Some(area) => loads as f64 / area,
        None => 0.0,
    };
    let crowding = py_min(model.maximum_slowdown, 1.0 + model.density_slowdown * py_max(0.0, density - 0.5));
    let minutes = length / speed * crowding;
    if !minutes.is_finite() || minutes.abs() >= EXACT_INT as f64 {
        return Err(Error::unsupported("the duration is too large"));
    }
    Ok((minutes.ceil() as i64).max(1))
}

/// `min(options, key=(distance, points))`: the tuple order on finite floats.
fn is_shorter(first: (f64, &[Pt]), second: (f64, &[Pt])) -> bool {
    if first.0 != second.0 {
        return first.0 < second.0;
    }
    for (a, b) in first.1.iter().zip(second.1) {
        if a.x != b.x {
            return a.x < b.x;
        }
        if a.y != b.y {
            return a.y < b.y;
        }
    }
    first.1.len() < second.1.len()
}

/// `facility_approach_route` in a placed world, to a facility's own point, or to a surveyed service
/// point when one is given: then no point beside a footprint. The route's minutes and its path, or
/// None for no route. Says Unsupported without a floor plan.
pub fn approach_point(
    geo: &Geo,
    row: &MovementRow,
    rows: &Entities,
    model: &Read<MovementModel>,
    agent_id: &str,
    facility_id: &str,
    service_point: Option<&str>,
) -> Result<Option<(i64, Arc<[Pt]>)>> {
    let (Some(agent), Some(facility)) = (rows.agents.get(agent_id), rows.facilities.get(facility_id)) else {
        return Err(Error::unsupported("no such agent or facility"));
    };
    let Some(layout) = &geo.layout else {
        return Err(Error::unsupported("no floor plan"));
    };
    let mobility = super::mobility_text(agent);
    if agent.zone_id != facility.zone_id {
        return Ok(None);
    }
    let start = position_point(row.positions.get(agent_id));
    if start.is_some_and(|start| !usable(start.x, start.y)) {
        return Err(Error::unsupported("a position is not a usable point"));
    }
    let facility_point = match service_point {
        Some(id) => match layout.service_points.iter().find(|point| &*point.id == id) {
            Some(point) if &*point.facility_id == facility_id => point.point,
            _ => return Ok(None),
        },
        None => match layout.facility_points.get(facility_id) {
            Some(point) => *point,
            None => return Ok(None),
        },
    };
    let mut candidates = vec![facility_point];
    let footprint = geo.constraints.as_ref().filter(|_| service_point.is_none()).and_then(|constraints| {
        constraints.footprints.iter().find(|(id, _)| &**id == facility_id).map(|(_, polygon)| polygon)
    });
    if let Some(footprint) = footprint {
        let count = footprint.len() as f64;
        let not_plain = || Error::unsupported("a footprint's centre is not a plain sum");
        let center = Pt {
            x: py_fsum(&footprint.iter().map(|point| point.x).collect::<Vec<_>>()).ok_or_else(not_plain)? / count,
            y: py_fsum(&footprint.iter().map(|point| point.y).collect::<Vec<_>>()).ok_or_else(not_plain)? / count,
        };
        let clearance = match mobility {
            "independent" => APPROACH_INDEPENDENT,
            "assisted" => APPROACH_ASSISTED,
            _ => APPROACH_WHEELCHAIR,
        };
        let outward = |candidates: &mut Vec<Pt>, point: Pt| {
            let (dx, dy) = (point.x - center.x, point.y - center.y);
            let length = hypot(&[dx, dy]);
            if length > 1e-9 {
                candidates.push(Pt { x: point.x + dx / length * clearance, y: point.y + dy / length * clearance });
            }
        };
        for (index, &first) in footprint.iter().enumerate() {
            let second = footprint[(index + 1) % footprint.len()];
            outward(&mut candidates, Pt { x: (first.x + second.x) / 2.0, y: (first.y + second.y) / 2.0 });
        }
        for &corner in footprint {
            outward(&mut candidates, corner);
        }
    }
    let mut best: Option<(f64, Arc<[Pt]>)> = None;
    for destination in candidates {
        let Some(path) = geo.room_route(&facility.zone_id, start, destination, mobility).map_err(refused)? else {
            continue;
        };
        let length = polyline_length(&path).ok_or_else(|| Error::unsupported("not a plain sum"))?;
        if best.as_ref().is_none_or(|(known, known_path)| is_shorter((length, &path), (*known, known_path))) {
            best = Some((length, path));
        }
    }
    let Some((length, path)) = best else {
        return Ok(None);
    };
    // in_room_travel_minutes, then what facility_approach_route makes of it.
    let mut duration = if path.len() < 2 { 0 } else { room_minutes(model, geo, rows, &agent.zone_id, mobility, length)? };
    if path.len() == 1 || path.windows(2).all(|pair| distance(pair[0], pair[1]) <= 1e-8) {
        duration = 0;
    }
    Ok(Some((duration, path)))
}

// ---- A plan of the row ---------------------------------------------------------------------------

/// What a kernel found to change in the row of `movement_v1`. Applied in this order, each map in its
/// own order.
#[derive(Default)]
pub struct Changes {
    /// `(record's key, new record)`, dropped when the same bits are there already.
    pub positions: Vec<(Arc<str>, Position)>,
    pub gone_positions: Vec<Arc<str>>,
    pub gone_journeys: Vec<Arc<str>>,
    /// In the order of the walk, which is the order they take in the map.
    pub new_journeys: Vec<(Arc<str>, Journey)>,
    pub staff: Vec<(Arc<str>, Position)>,
    /// The edges one step more, in the order of the walk.
    pub usage: Vec<Arc<str>>,
    pub minute: Option<i64>,
}

impl Changes {
    /// What another plan changes, after what this one does.
    pub fn absorb(&mut self, other: Changes) {
        self.positions.extend(other.positions);
        self.new_journeys.extend(other.new_journeys);
    }

    /// Put the plan in the row alone: a kernel of the core, whose slot's tree follows the row.
    pub fn apply(self, row: &mut MovementRow) {
        if let Some(minute) = self.minute {
            row.last_updated_minute = minute;
        }
        for id in self.gone_journeys {
            row.journeys.shift_remove(&*id);
        }
        for id in self.gone_positions {
            row.positions.shift_remove(&*id);
        }
        for (id, journey) in self.new_journeys {
            row.journeys.insert(id, journey);
        }
        for (map, changes) in [(&mut row.positions, self.positions), (&mut row.staff_positions, self.staff)] {
            for (id, position) in changes {
                if map.get(&*id).is_some_and(|current| same_position(current, &position)) {
                    continue;
                }
                map.insert(id, position);
            }
        }
        for edge in self.usage {
            let count = row.edge_usage_minutes.get(&*edge).copied().unwrap_or(0) + 1;
            row.edge_usage_minutes.insert(edge, count);
        }
    }
}

/// `_state`'s records for the people who have none.
pub fn missing_positions(row: &MovementRow, rows: &Entities, changes: &mut Changes) {
    for (id, agent) in rows.agents.iter() {
        if !row.positions.contains_key(&**id) {
            changes.positions.push((id.clone(), record(agent.zone_id.clone(), None, KINDS.unavailable.clone(), None, 0.0)));
        }
    }
}

/// Whether a journey that starts at `started` has walked a minute already (the row was updated at
/// or after it), and its progress then.
pub fn start_progress(row: &MovementRow, started: i64, duration: i64) -> (bool, f64) {
    let elapsed = i64::from(row.last_updated_minute >= started);
    (elapsed != 0, py_min(1.0, elapsed as f64 / duration as f64))
}

/// `start_movement`'s writes as a plan. `off_path` is the point where the path has no points: on
/// the first edge at the progress `start_progress` gives (0.0 before a minute is walked), or the
/// origin zone's centre without an edge; the caller reads it.
#[allow(clippy::too_many_arguments)]
pub fn plan_start(
    row: &MovementRow,
    rows: &Entities,
    agent_id: &Arc<str>,
    journey: Journey,
    origin: &Arc<str>,
    points: &[Pt],
    edge_ids: &[Arc<str>],
    started: i64,
    duration: i64,
    off_path: Option<Pt>,
) -> Result<Changes> {
    let mut changes = Changes::default();
    missing_positions(row, rows, &mut changes);
    let (elapsed, initial_progress) = start_progress(row, started, duration);
    let point = if !points.is_empty() {
        if elapsed {
            match point_along_polyline(points, initial_progress) {
                Some(point) => Some(point),
                None => return Err(Error::unsupported("a route's length is not a plain sum")),
            }
        } else {
            Some(points[0])
        }
    } else {
        off_path
    };
    let kind = if point.is_some() && (!edge_ids.is_empty() || !points.is_empty()) {
        KINDS.physical.clone()
    } else {
        KINDS.unavailable.clone()
    };
    changes.positions.push((agent_id.clone(), record(origin.clone(), point, kind, edge_ids.first().cloned(), initial_progress)));
    changes.new_journeys.push((agent_id.clone(), journey));
    Ok(changes)
}
