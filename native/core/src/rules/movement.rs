//! `domain/movement.py`: the rules of a tick, on the world borrowed whole (lane c2 of Issue #152).
//! The Python rules are gone (`kernel_only`): the kernels say Unsupported for anything they do not
//! decide, and the PyO3 crate raises it as `KernelRefusal` (docs/rust-port/lanes/m4-movement.md).
//!
//! Every kernel plans first: it reads the rows and the plan of the floor and works out each record,
//! with nothing changed, and says Unsupported for anything it does not decide. Then it writes the
//! row of `movement_v1` alone; an attached tree follows the row when the PyO3 crate gives the world
//! back (lane P8-0c). Planning reads only what the tick started with, so one person's result
//! depends on nobody else's, except in two branches of `advance_movement`: a queue trip that must
//! replan, and a person placed in a queue slot. They read the positions the walk has written so far
//! and take event numbers in its order (docs/rust-port/lanes/i-movement.md). The first walk stops
//! at either branch, and a second one reads a copy of the positions it keeps up to date, as the
//! Python rule's walk does.
//!
//! The scene of a walk is the rows of the zones and the edges (`world.zones`, `world.zone_edges`),
//! the floor plan (`Geo`, which the PyO3 crate reads from the configs and the overlay) and the
//! overlay's value (`geometry_overlay_v1`). The staff's routed operations come from the row of
//! `staff_operations_v1`, or from its tree as the PyO3 crate reads it where the row does not answer
//! (`StaffOps`).
//!
//! What stays with the PyO3 crate: the Python values (`ZoneRoute`, `Point2D`, the dicts a kernel
//! returns), the floor plan, the movement section of the configs, and settling the pending event of
//! a queue trip that replans, whose object may be Python's (`EventWriter::settle`).

use std::sync::Arc;

use crate::config::movement::MovementModel;
use crate::config::read::Read;
use crate::entities::{Activity, AgentRow, EdgeRow, Entities};
use crate::error::{Error, Result};
use crate::events::queue::Queue;
use crate::events::writer::WriteResult;
use crate::events::EventStatus;
use crate::geometry::{distance, point_along_polyline, point_in_polygon, point_in_value, polyline_length, usable, Geo, Pt};
use crate::mathx::{py_fsum, py_max, py_min, py_sum};
use crate::row::IdMap;
use crate::rules::facility_queues as fifo;
use crate::rules::kit::movement::{self as walking, approach_point, is_queue_trip, position_point, queue_slot_in, record, Changes, KINDS};
use crate::rules::kit::{add_metric, check_metric_state, held_row, mobility_text, same_position, Events, Held};
use crate::rules::queues::{plan_trip, slot_route, trip_id, write_plan, Fifo, Plan, Route as QueueRoute, BLOCKED_METRIC};
use crate::state::movement::{Journey, MovementRow, Point, Position};
use crate::state::staff_operations::{Phase, StaffOperationsRow, NAME as STAFF_OPERATIONS};
use crate::value::V;
use crate::world::World;

pub use crate::rules::kit::movement::{EXACT_INT, KEY, POLICY_VERSION};

/// The key of the overlay in `model_state`.
pub const OVERLAY_KEY: &str = "geometry_overlay_v1";
/// How near counts as being there, in `agent_is_at_queue_slot` and its siblings, in metres.
const ARRIVED_WITHIN: f64 = 0.05;
/// The detail of the record of a queue trip's event that a replan suppresses.
pub const QUEUE_REPLAN_DETAIL: &str = "In-room queue route changed after a wall, obstacle, or equipment update";

#[track_caller]
fn unsupported<T>(reason: &'static str) -> Result<T> {
    Err(Error::unsupported(reason))
}

/// The physical-or-unavailable kind of a person on a route.
fn on_route_kind(point: &Option<Pt>) -> Arc<str> {
    if point.is_some() {
        KINDS.physical.clone()
    } else {
        KINDS.unavailable.clone()
    }
}

/// What a world without the namespace reads: `world.model_state.get(KEY, {})`.
pub fn no_positions() -> MovementRow {
    MovementRow {
        schema_version: 2,
        policy_version: Arc::from(POLICY_VERSION),
        positions: Default::default(),
        staff_positions: Default::default(),
        journeys: Default::default(),
        edge_usage_minutes: Default::default(),
        last_updated_minute: 0,
    }
}

/// Run f on the row of the world's positions, or on none in a world without the namespace.
fn with_positions<R>(world: &World, held: &Held, read: impl FnOnce(&MovementRow) -> Result<R>) -> Result<R> {
    match held_row::<MovementRow>(world, KEY, held)? {
        Some(row) => read(&row),
        None => read(&no_positions()),
    }
}

/// Whether `Changes::apply` would change the row.
fn changes_row(row: &MovementRow, changes: &Changes) -> bool {
    let differs = |map: &IdMap<Position>, (id, position): &(Arc<str>, Position)| {
        map.get(&**id).is_none_or(|current| !same_position(current, position))
    };
    changes.minute.is_some_and(|minute| minute != row.last_updated_minute)
        || changes.gone_journeys.iter().any(|id| row.journeys.contains_key(&**id))
        || changes.gone_positions.iter().any(|id| row.positions.contains_key(&**id))
        || !changes.new_journeys.is_empty()
        || !changes.usage.is_empty()
        || changes.positions.iter().any(|change| differs(&row.positions, change))
        || changes.staff.iter().any(|change| differs(&row.staff_positions, change))
}

/// Put a plan in the row, unless it changes nothing: then the slot keeps its stamp and its tree.
fn apply(world: &World, changes: Changes) -> Result<()> {
    let changes_it = changes_row(&*world.row::<MovementRow>(KEY)?, &changes);
    if changes_it {
        changes.apply(&mut *world.row_mut::<MovementRow>(KEY)?);
    }
    Ok(())
}

/// A plan of `rules/travel.rs`, put in the row: the slot is marked, and an
/// attached tree follows.
pub fn write_changes(world: &World, changes: Changes) -> Result<()> {
    apply(world, changes)
}

// ---- The scene -------------------------------------------------------------------------------

/// What a tick's kernel reads beside the row of `movement_v1`.
pub struct Scene<'a> {
    world: &'a World,
    pub geo: &'a Geo,
    pub rows: &'a Entities,
}

impl<'a> Scene<'a> {
    pub fn new(world: &'a World, geo: &'a Geo, rows: &'a Entities) -> Self {
        Self { world, geo, rows }
    }

    fn edge(&self, edge_id: &str) -> Result<&'a EdgeRow> {
        self.rows.zone_edges.get(edge_id).ok_or_else(|| Error::unsupported("a journey's edge is unknown"))
    }

    pub fn edge_minutes(&self, edge_id: &str) -> Result<i64> {
        let minutes = self.edge(edge_id)?.travel_minutes;
        if minutes <= 0 || minutes >= EXACT_INT {
            return unsupported("an edge's minutes are not a plain duration");
        }
        Ok(minutes)
    }

    /// `_route_point`.
    pub fn route_point(&self, edge_id: &str, progress: f64, current_zone: &str) -> Result<Option<Pt>> {
        let Some(layout) = &self.geo.layout else {
            return Ok(None);
        };
        let Some(route) = layout.routes.get(edge_id) else {
            return self.geo.center(current_zone);
        };
        let forward = &*self.edge(edge_id)?.from_zone_id == current_zone;
        let at = |index: usize| {
            if forward {
                route.points[index]
            } else {
                route.points[route.points.len() - 1 - index]
            }
        };
        let segments = route.points.len() - 1;
        let lengths: Vec<f64> = (0..segments).map(|index| distance(at(index), at(index + 1))).collect();
        let total = py_sum(&lengths);
        if total <= 0.0 {
            return Ok(Some(at(0)));
        }
        let mut remaining = py_min(1.0, py_max(0.0, progress)) * total;
        for (index, &length) in lengths.iter().enumerate() {
            if remaining <= length || index == segments - 1 {
                let ratio = if length == 0.0 { 0.0 } else { py_min(1.0, py_max(0.0, remaining / length)) };
                let (first, second) = (at(index), at(index + 1));
                let point = Pt { x: first.x + (second.x - first.x) * ratio, y: first.y + (second.y - first.y) * ratio };
                return Ok(Some(point));
            }
            remaining -= length;
        }
        Ok(Some(at(segments)))
    }

    /// `_reserved_bed_point`, of the overlay's value.
    fn reserved_bed_point(&self, agent_id: &str) -> Result<Option<Pt>> {
        let overlay = self.world.slots.try_value(OVERLAY_KEY)?;
        let Some(overlay) = overlay.as_deref() else {
            return Ok(None);
        };
        let V::Map(overlay) = overlay else {
            return unsupported("the overlay is not a plain dict");
        };
        let Some(reservations) = overlay.get("bed_reservations") else {
            return Ok(None);
        };
        let V::Map(reservations) = reservations else {
            return unsupported("a value is not a plain dict");
        };
        let reservation = match reservations.get(agent_id) {
            None | Some(V::Null) => return Ok(None),
            Some(V::Map(reservation)) => reservation,
            Some(_) => return unsupported("a reservation is not a plain dict"),
        };
        let Some(rooms) = overlay.get("temporary_rooms") else {
            return Ok(None);
        };
        let Some(room_id) = reservation.get("room_id") else {
            return Ok(None);
        };
        let (V::Map(rooms), V::Str(room_id)) = (rooms, room_id) else {
            return unsupported("the rooms are not a plain dict, or a room ID is not a str");
        };
        let Some(room) = rooms.get(&**room_id) else {
            return Ok(None);
        };
        let V::Map(room) = room else {
            return unsupported("a value is not a plain dict");
        };
        let Some(beds) = room.get("bed_points_m") else {
            return Ok(None);
        };
        let V::List(beds) = beds else {
            return unsupported("a value is not a plain list");
        };
        // An exact int that fits: a bool, a negative or a big one reads as no bed.
        let Some(V::Int(index)) = reservation.get("bed_index") else {
            return Ok(None);
        };
        match usize::try_from(*index).ok().and_then(|index| beds.get(index)) {
            Some(bed) => Ok(Some(point_in_value(bed)?)),
            None => Ok(None),
        }
    }

    /// `_facility_move_invalidated_journey`, of the overlay's value.
    fn facility_move_invalidates(&self, journey: &Journey) -> Result<bool> {
        let Some(facility_id) = &journey.facility_id else {
            return Ok(false);
        };
        let overlay = self.world.slots.try_value(OVERLAY_KEY)?;
        let Some(overlay) = overlay.as_deref() else {
            return Ok(false);
        };
        let V::Map(overlay) = overlay else {
            return unsupported("a value is not a plain dict");
        };
        let Some(moves) = overlay.get("facility_points") else {
            return Ok(false);
        };
        let V::Map(moves) = moves else {
            return unsupported("a value is not a plain dict");
        };
        let Some(V::Map(moved)) = moves.get(&**facility_id) else {
            return Ok(false);
        };
        match moved.get("minute") {
            Some(V::Int(minute)) => Ok(*minute > journey.started_minute),
            Some(V::Big(_)) => unsupported("a move's minute is too large"),
            _ => Ok(false),
        }
    }

    /// What `stationary` would refuse for a person idle in this zone, for a plan that makes one.
    pub fn check_idle_point(&self, zone_id: &str) -> Result<()> {
        if self.geo.layout.is_some() {
            self.geo.center(zone_id)?;
        } else if let Some(zone) = self.rows.zones.get(zone_id) {
            if let (Some(x), Some(y)) = (zone.display_x, zone.display_y) {
                if !usable(x, y) {
                    return unsupported("a display point is not a usable point");
                }
            }
        }
        Ok(())
    }

    /// Whether `_set_stationary_position` reads the positions of the people ahead for this person.
    fn stands_in_a_slot(&self, agent: &AgentRow) -> bool {
        self.geo.layout.is_some() && agent.activity == Activity::Queued && agent.target_id.as_deref().is_some_and(|target| !target.is_empty())
    }

    /// `_set_stationary_position`'s record. A person in a queue slot reads the people ahead, so
    /// `slots` is the row they are read from, or None where who walked first would matter.
    fn stationary(&self, agent_id: &str, agent: &AgentRow, slots: Option<&MovementRow>) -> Result<Position> {
        let mut point = None;
        let target = agent.target_id.as_deref().filter(|target| !target.is_empty());
        let placed = self.geo.layout.as_ref();
        if let Some(layout) = placed {
            if agent.activity == Activity::Queued && target.is_some() {
                let Some(row) = slots else {
                    return unsupported("a person stands in a queue slot, which depends on the people ahead");
                };
                point = queue_slot_in(self.geo, row, self.rows, agent_id, target.unwrap_or_default(), None)?;
            } else if agent.activity == Activity::InService && target.is_some() {
                point = target.and_then(|target| layout.facility_points.get(target)).copied();
            } else if agent.activity == Activity::Sleeping {
                point = self.reserved_bed_point(agent_id)?;
            }
            if point.is_none() {
                point = self.geo.center(&agent.zone_id)?;
            }
        } else if let Some(zone) = self.rows.zones.get(&agent.zone_id) {
            if let (Some(x), Some(y)) = (zone.display_x, zone.display_y) {
                if !usable(x, y) {
                    return unsupported("a display point is not a usable point");
                }
                point = Some(Pt { x, y });
            }
        }
        let kind = match (placed, &point) {
            (Some(_), Some(_)) => KINDS.physical.clone(),
            (None, Some(_)) => KINDS.display_only.clone(),
            _ => KINDS.unavailable.clone(),
        };
        Ok(record(agent.zone_id.clone(), point, kind, None, 0.0))
    }
}

/// What `stationary` would refuse for a person idle in each of these zones (admission's plan).
pub fn check_idle_points(world: &World, geo: &Geo, zones: &[&str]) -> Result<()> {
    let rows = world.rows()?;
    let scene = Scene::new(world, geo, &rows);
    for zone in zones {
        scene.check_idle_point(zone)?;
    }
    Ok(())
}

fn points_of_journey(journey: &Journey) -> Result<Vec<Pt>> {
    journey
        .route_points
        .iter()
        .map(|point| {
            if usable(point.x, point.y) {
                Ok(Pt { x: point.x, y: point.y })
            } else {
                unsupported("a route point is not a usable point")
            }
        })
        .collect()
}

/// The record of a queue trip's end: `finish_movement`'s branch for it.
fn arrived(current: &Position) -> Option<Position> {
    current.point.map(|point| Position {
        zone_id: current.zone_id.clone(),
        point: Some(point),
        edge_id: None,
        progress: 0.0,
        position_kind: KINDS.physical.clone(),
    })
}

// ---- The staff's routed operations -------------------------------------------------------------

/// A staff member's operation as `staff_position_snapshots` reads it: travelling or not, or the
/// refusal of a record that is not what it reads. A route's IDs that are not strs are None, refused
/// where the walk reaches them.
#[derive(Clone, Debug, PartialEq)]
pub enum StaffOp {
    NotTravelling,
    Travel(StaffTravel),
    Refused(&'static str),
}

#[derive(Clone, Debug, PartialEq)]
pub struct StaffTravel {
    pub travel_minutes: i64,
    pub started_minute: i64,
    pub route_edge_ids: Vec<Option<Arc<str>>>,
    pub route_zone_ids: Vec<Option<Arc<str>>>,
}

/// Where the staff's operations are read: none (no namespace, or not a dict), or the row of
/// `staff_operations_v1`. Every rule writes the namespace as its row (lanes b2 and b3): a tainted
/// one is refused where the row is read.
#[derive(Clone, Debug, PartialEq)]
pub enum StaffOps {
    None,
    Row,
}

/// The operations of the staff, borrowed for a plan.
enum StaffRecords<'a> {
    None,
    Row(crate::cell::ShardRef<'a, StaffOperationsRow>),
}

impl<'a> StaffRecords<'a> {
    fn of(world: &'a World, ops: &'a StaffOps) -> Result<Self> {
        Ok(match ops {
            StaffOps::None => Self::None,
            StaffOps::Row => Self::Row(world.row::<StaffOperationsRow>(STAFF_OPERATIONS)?),
        })
    }

    fn op(&self, staff_id: &str) -> StaffOp {
        match self {
            Self::None => StaffOp::NotTravelling,
            Self::Row(row) => match row.staff.get(staff_id) {
                Some(record) if record.phase == Phase::Travel => match record.started_minute {
                    // The tree's None, which `exact_int` turns down.
                    None => StaffOp::Refused("a value is not an int"),
                    Some(started) => StaffOp::Travel(StaffTravel {
                        travel_minutes: record.travel_minutes,
                        started_minute: started,
                        route_edge_ids: record.route_edge_ids.iter().cloned().map(Some).collect(),
                        route_zone_ids: record.route_zone_ids.iter().cloned().map(Some).collect(),
                    }),
                },
                _ => StaffOp::NotTravelling,
            },
        }
    }
}

/// `staff_position_snapshots`' record of one staff member.
fn plan_staff(scene: &Scene<'_>, op: StaffOp, zone_id: &Arc<str>, boundary: i64) -> Result<Position> {
    let travel = match op {
        StaffOp::NotTravelling => {
            let point = scene.geo.center(zone_id)?;
            let kind = on_route_kind(&point);
            return Ok(record(zone_id.clone(), point, kind, None, 0.0));
        }
        StaffOp::Refused(reason) => return unsupported(reason),
        StaffOp::Travel(travel) => travel,
    };
    let text = |id: &Option<Arc<str>>| id.clone().ok_or_else(|| Error::unsupported("a value is not a str"));
    let elapsed = travel.travel_minutes.min(boundary.saturating_sub(travel.started_minute)).max(0);
    let (mut cumulative, mut edge_id, mut progress) = (0_i64, None::<Arc<str>>, 0.0);
    let mut current_zone = zone_id.clone();
    let last = travel.route_edge_ids.len().wrapping_sub(1);
    for (index, route_edge) in travel.route_edge_ids.iter().enumerate() {
        let route_edge = text(route_edge)?;
        let minutes = scene.edge_minutes(&route_edge)?;
        if elapsed < cumulative + minutes {
            progress = (elapsed - cumulative) as f64 / minutes as f64;
            let zone = travel.route_zone_ids.get(index).ok_or_else(|| Error::unsupported("a route has fewer zones"))?;
            current_zone = text(zone)?;
            edge_id = Some(route_edge);
            break;
        }
        cumulative += minutes;
        current_zone = scene.edge(&route_edge)?.to_zone_id.clone();
        if index == last {
            edge_id = Some(route_edge);
            progress = 1.0;
        }
    }
    let point = match &edge_id {
        Some(edge) => scene.route_point(edge, progress, &current_zone)?,
        None => scene.geo.center(&current_zone)?,
    };
    let kind = on_route_kind(&point);
    Ok(record(current_zone, point, kind, edge_id, progress))
}

/// The staff's records at `boundary`, in ID order.
fn plan_all_staff(scene: &Scene<'_>, staff: &StaffRecords<'_>, boundary: i64, changes: &mut Changes) -> Result<()> {
    let mut members: Vec<_> = scene.rows.staff.iter().collect();
    members.sort_unstable_by(|first, second| first.0.cmp(second.0));
    for (staff_id, member) in members {
        changes.staff.push((staff_id.clone(), plan_staff(scene, staff.op(staff_id), &member.zone_id, boundary)?));
    }
    Ok(())
}

/// `staff_position_snapshots`, while the world holds `movement_v1`: the staff positions at
/// `boundary` written, and every record of the row's staff positions in ID order.
pub fn staff_position_snapshots(world: &World, geo: &Geo, staff: &StaffOps, boundary: i64) -> Result<Vec<(Arc<str>, Position)>> {
    let changes = {
        let _row = world.row::<MovementRow>(KEY)?;
        let rows = world.rows()?;
        let scene = Scene::new(world, geo, &rows);
        let records = StaffRecords::of(world, staff)?;
        let mut changes = Changes::default();
        plan_all_staff(&scene, &records, boundary, &mut changes)?;
        changes
    };
    apply(world, changes)?;
    let row = world.row::<MovementRow>(KEY)?;
    let mut records: Vec<(Arc<str>, Position)> = row.staff_positions.iter().map(|(id, position)| (id.clone(), position.clone())).collect();
    records.sort_unstable_by(|first, second| first.0.cmp(&second.0));
    Ok(records)
}

// ---- advance_movement --------------------------------------------------------------------------

/// `_room_path_is_clear`, with its record of the person.
fn room_path_is_clear(geo: &Geo, position: Option<&Position>, zone_id: &str, points: &[Pt], mobility: &str) -> Result<bool> {
    let (Some(constraints), Some(layout)) = (&geo.constraints, &geo.layout) else {
        return Ok(true);
    };
    let Some(zone) = layout.zones.get(zone_id) else {
        return Ok(false);
    };
    let current = match position.and_then(|position| position.point) {
        Some(point) => {
            if !usable(point.x, point.y) {
                return unsupported("a position is not a usable point");
            }
            Pt { x: point.x, y: point.y }
        }
        None => points[0],
    };
    let lengths: Vec<f64> = points.windows(2).map(|pair| distance(pair[0], pair[1])).collect();
    let Some(total) = py_fsum(&lengths) else {
        return unsupported("a route's length is not a plain sum");
    };
    let progress = position.map_or(0.0, |position| position.progress);
    let target_distance = py_min(1.0, py_max(0.0, progress)) * total;
    let mut cumulative = 0.0;
    let mut remaining = vec![current];
    for (index, &length) in lengths.iter().enumerate() {
        if cumulative + length + 1e-9 >= target_distance {
            if length > 1e-12 {
                let fraction = py_min(1.0, py_max(0.0, (target_distance - cumulative) / length));
                if fraction < 1.0 - 1e-9 {
                    remaining.push(points[index + 1]);
                }
            }
            remaining.extend_from_slice(&points[index + 2..]);
            break;
        }
        cumulative += length;
    }
    if remaining.len() == 1 {
        remaining.push(points[points.len() - 1]);
    }
    Ok(remaining
        .windows(2)
        .all(|pair| Geo::room_segment_clear(constraints, pair[0], pair[1], &zone.polygon, mobility)))
}

/// `min(1.0, max(0.0, a / b))` of ints that a float holds exactly.
fn progress_of(part: i64, whole: i64) -> f64 {
    py_min(1.0, py_max(0.0, part as f64 / whole as f64))
}

/// The plan of one journey in `advance_movement`.
enum Step {
    Replan,
    /// A queue trip's replan, which reads the positions of the walk and writes an event.
    QueueReplan,
    Stop,
    Finish,
    Walk { position: Position, edge: Option<Arc<str>>, completed: bool },
}

fn plan_journey(scene: &Scene<'_>, queue: &Queue, row: &MovementRow, agent_id: &str, agent: &AgentRow, journey: &Journey, minute: i64) -> Result<Step> {
    let rows = scene.rows;
    let queue_trip = is_queue_trip(&journey.purpose);
    let task_active = rows.active_tasks.get(&journey.task_id).is_some();
    let queued = agent.activity == Activity::Queued;
    let trip_active = queued && queue_trip && task_active;
    let pending = queue.has_event(&journey.event_id);
    if trip_active && !pending {
        return Ok(Step::Finish);
    }
    if (agent.activity != Activity::Moving && !trip_active) || !pending || !task_active {
        return Ok(Step::Stop);
    }
    // From here the person walks or replans.
    let replan = || if queue_trip { Step::QueueReplan } else { Step::Replan };
    let facility = journey
        .facility_id
        .as_deref()
        .filter(|facility_id| !facility_id.is_empty())
        .and_then(|facility_id| rows.facilities.get(facility_id));
    let destination = facility.map_or(&journey.destination_zone_id, |facility| &facility.zone_id);
    if *destination != journey.destination_zone_id || scene.facility_move_invalidates(journey)? {
        return Ok(replan());
    }
    if journey.due_minute <= journey.started_minute
        || journey.due_minute.abs() >= EXACT_INT
        || journey.started_minute.abs() >= EXACT_INT
    {
        return unsupported("a journey has no plain duration");
    }
    let mobility = mobility_text(agent);
    let boundary = (minute + 1).min(journey.due_minute);
    let completed = queued && queue_trip && boundary >= journey.due_minute;
    let duration = journey.due_minute - journey.started_minute;
    if !journey.route_points.is_empty() {
        let points = points_of_journey(journey)?;
        if !room_path_is_clear(scene.geo, row.positions.get(agent_id), &journey.destination_zone_id, &points, mobility)? {
            return Ok(replan());
        }
        let progress = progress_of(boundary - journey.started_minute, duration);
        let Some(point) = point_along_polyline(&points, progress) else {
            return unsupported("a route's length is not a plain sum");
        };
        return Ok(Step::Walk {
            position: record(journey.destination_zone_id.clone(), Some(point), KINDS.physical.clone(), None, progress),
            edge: None,
            completed,
        });
    }
    let durations = journey.route_edge_ids.iter().map(|edge| scene.edge_minutes(edge)).collect::<Result<Vec<i64>>>()?;
    // A frame at boundary M is the state before tick M, so the sample is the next boundary.
    let elapsed = (boundary - journey.started_minute).max(0);
    let (mut edge_index, mut progress) = (0, 0.0);
    if !durations.is_empty() {
        let total: i64 = durations.iter().sum();
        if total >= EXACT_INT {
            return unsupported("a route's minutes are too large");
        }
        let duration_scale = duration as f64 / total as f64;
        let mut cumulative = 0.0;
        for (index, &raw_duration) in durations.iter().enumerate() {
            let scaled = raw_duration as f64 * duration_scale;
            if (elapsed as f64) < cumulative + scaled || index == durations.len() - 1 {
                edge_index = index;
                progress = py_min(1.0, py_max(0.0, (elapsed as f64 - cumulative) / scaled));
                break;
            }
            cumulative += scaled;
            edge_index = (index + 1).min(durations.len() - 1);
            progress = 0.0;
        }
    }
    let edge_id = journey.route_edge_ids.get(edge_index).filter(|_| !durations.is_empty());
    if let Some(edge_id) = edge_id {
        if scene.edge(edge_id)?.closed || !scene.geo.route_is_clear(edge_id, mobility) {
            // A closure invalidates this completion event, whatever the trip is for.
            return Ok(Step::Replan);
        }
    }
    let current_zone = if durations.is_empty() {
        &journey.destination_zone_id
    } else {
        journey.route_zone_ids.get(edge_index).ok_or_else(|| Error::unsupported("a route has fewer zones"))?
    };
    let point = match edge_id {
        Some(edge_id) => scene.route_point(edge_id, progress, current_zone)?,
        None => scene.geo.center(current_zone)?,
    };
    let kind = on_route_kind(&point);
    Ok(Step::Walk {
        position: record(current_zone.clone(), point, kind, edge_id.cloned(), progress),
        edge: edge_id.cloned(),
        completed,
    })
}

/// `_replan_physical_queue_trip`, planned with the positions the walk has written so far.
enum QueueReplanEnd {
    /// `facility is None`: the trip only ends.
    NoFacility,
    Blocked,
    /// `_schedule_queue_trip` gave up: past the end of the Run. The replan still counts.
    PastTheEnd,
    /// A route that takes no time: the person is placed at its end.
    Placed,
    Trip(Plan),
}

/// A queue trip that replans: its pending event, which the PyO3 crate settles as suppressed
/// (`QUEUE_REPLAN_DETAIL`), and what `write_queue_replan` then writes.
pub struct QueueReplan {
    pub event_id: Arc<str>,
    facility_id: Option<Arc<str>>,
    end: QueueReplanEnd,
}

/// What `advance_movement` found to do: the changes of the row, the people to replan, and the queue
/// trips that replan now, in the walk's order.
pub struct WalkPlan {
    changes: Changes,
    replans: Vec<Arc<str>>,
    queue_replans: Vec<QueueReplan>,
}

impl WalkPlan {
    pub fn queue_replans(&self) -> &[QueueReplan] {
        &self.queue_replans
    }
}

/// What the walk in order takes besides the world: the movement model of the configs, whether the
/// Python world holds `facility_queues_v1`, and whether `world.metric_state` is its slot's tree.
pub struct InOrder<'a> {
    pub model: &'a Read<MovementModel>,
    pub queues: Held,
    pub metric_state_held: bool,
}

/// `facility_queue_slot_route` or `facility_approach_route`, then what the Python rule makes of it.
#[allow(clippy::too_many_arguments)]
fn plan_queue_replan(
    scene: &Scene<'_>,
    events: &Events<'_>,
    in_order: &InOrder<'_>,
    view: &MovementRow,
    agent_id: &Arc<str>,
    journey: &Journey,
    minute: i64,
    sequence: i64,
    changes: &mut Changes,
) -> Result<QueueReplan> {
    let rows = scene.rows;
    let found = |end| QueueReplan { event_id: journey.event_id.clone(), facility_id: journey.facility_id.clone(), end };
    changes.gone_journeys.push(agent_id.clone());
    let Some(facility_id) = &journey.facility_id else {
        return Ok(found(QueueReplanEnd::NoFacility));
    };
    let Some(facility) = rows.facilities.get(facility_id) else {
        return Ok(found(QueueReplanEnd::NoFacility));
    };
    let resource = journey.purpose.starts_with("resource_");
    let position_trip = journey.purpose.ends_with("_position");
    let fifo = match &journey.queue_id {
        Some(queue_id) => Some(Fifo::read(scene.world, &in_order.queues, scene.geo, queue_id)?),
        None => None,
    };
    let model = in_order.model;
    let route = if position_trip {
        slot_route(scene.geo, view, rows, model, agent_id, facility, facility_id, fifo.as_ref())?
    } else if scene.geo.layout.is_none() {
        Some((0, Arc::from(Vec::new())))
    } else {
        let point = fifo.as_ref().and_then(|fifo| fifo.request_of(agent_id)).map(|request| request.service_point_id.clone());
        approach_point(scene.geo, view, rows, model, agent_id, facility_id, point.as_deref())?
    };
    let Some((minutes, path)) = route.filter(|(_, path)| !path.is_empty()) else {
        return Ok(found(QueueReplanEnd::Blocked));
    };
    if minutes <= 0 || path.len() < 2 {
        let target = path[path.len() - 1];
        changes.positions.push((agent_id.clone(), record(facility.zone_id.clone(), Some(target), KINDS.physical.clone(), None, 0.0)));
        return Ok(found(QueueReplanEnd::Placed));
    }
    // `.index(agent_id)` of the provenance, which `_schedule_queue_trip` builds only if it schedules.
    let index = if fifo.is_some() {
        0
    } else {
        let members = if resource { &facility.resource_queue_agent_ids } else { &facility.queue_agent_ids };
        match members.iter().position(|member| member == agent_id) {
            Some(index) => index,
            None if minute.checked_add(minutes).is_some_and(|due| due >= events.duration()) => 0,
            None => return unsupported("a trip of the queue by someone not in it"),
        }
    };
    let phase = if position_trip { "position" } else { "service" };
    let trip = QueueRoute { agent_id: agent_id.clone(), index, minutes, path };
    let taken = events.is_taken(&trip_id(sequence), Some(sequence))?;
    let mut plan = plan_trip(events.duration(), taken, view, rows, facility_id, resource, trip, minute, phase, fifo.as_ref(), sequence)?;
    changes.absorb(plan.take_changes());
    Ok(found(if matches!(plan, Plan::Blocked) { QueueReplanEnd::PastTheEnd } else { QueueReplanEnd::Trip(plan) }))
}

/// The walk of `advance_movement`, changing nothing. `in_order` is the second walk, with what the
/// branches that read the others need; without it None says that the walk reached one of them.
fn plan_walk(
    scene: &Scene<'_>,
    queue: &Queue,
    row: &MovementRow,
    minute: i64,
    staff: &StaffRecords<'_>,
    in_order: Option<(&InOrder<'_>, &Events<'_>)>,
) -> Result<Option<WalkPlan>> {
    let rows = scene.rows;
    let mut changes = Changes::default();
    walking::missing_positions(row, rows, &mut changes);
    let mut plan = WalkPlan { changes: Changes::default(), replans: Vec::new(), queue_replans: Vec::new() };
    if row.last_updated_minute >= minute {
        plan.changes = changes;
        return Ok(Some(plan));
    }
    changes.minute = Some(minute);
    // The positions as the walk has left them, read by the branches that depend on its order.
    let mut walked = in_order.map(|_| MovementRow {
        schema_version: row.schema_version,
        policy_version: row.policy_version.clone(),
        positions: row.positions.clone(),
        staff_positions: Default::default(),
        journeys: Default::default(),
        edge_usage_minutes: Default::default(),
        last_updated_minute: minute,
    });
    let mut applied = 0;
    let mut next_sequence = match in_order {
        Some((_, events)) => events.next_sequence()?,
        None => 0,
    };
    let mut completed: Vec<&str> = Vec::new();
    let mut seen_events: Vec<&str> = Vec::new();
    let mut journeys: Vec<(&Arc<str>, &Journey)> = row.journeys.iter().collect();
    journeys.sort_unstable_by(|first, second| first.0.cmp(second.0));
    for (agent_id, journey) in journeys {
        if let Some(walked) = &mut walked {
            for (id, position) in &changes.positions[applied..] {
                walked.positions.insert(id.clone(), position.clone());
            }
            applied = changes.positions.len();
        }
        let view: &MovementRow = walked.as_ref().unwrap_or(row);
        let Some(agent) = rows.agents.get(agent_id) else {
            changes.gone_journeys.push(agent_id.clone());
            changes.gone_positions.push(agent_id.clone());
            if let Some(walked) = &mut walked {
                walked.positions.shift_remove(&**agent_id);
            }
            continue;
        };
        match plan_journey(scene, queue, view, agent_id, agent, journey, minute)? {
            Step::Replan => plan.replans.push(agent_id.clone()),
            Step::QueueReplan => {
                let Some((in_order, events)) = in_order else {
                    return Ok(None);
                };
                if seen_events.contains(&&*journey.event_id) {
                    return unsupported("two journeys name one event");
                }
                seen_events.push(&journey.event_id);
                let replan = plan_queue_replan(scene, events, in_order, view, agent_id, journey, minute, next_sequence, &mut changes)?;
                next_sequence += i64::from(matches!(replan.end, QueueReplanEnd::Trip(_)));
                plan.queue_replans.push(replan);
            }
            Step::Stop => {
                if in_order.is_none() && scene.stands_in_a_slot(agent) {
                    return Ok(None);
                }
                changes.gone_journeys.push(agent_id.clone());
                let slots = in_order.map(|_| view);
                changes.positions.push((agent_id.clone(), scene.stationary(agent_id, agent, slots)?));
            }
            Step::Finish => {
                changes.gone_journeys.push(agent_id.clone());
                if let Some(position) = view.positions.get(&**agent_id).and_then(arrived) {
                    changes.positions.push((agent_id.clone(), position));
                }
            }
            Step::Walk { position, edge, completed: done } => {
                changes.positions.push((agent_id.clone(), position));
                changes.usage.extend(edge);
                if done {
                    completed.push(agent_id);
                }
            }
        }
    }
    for (agent_id, agent) in rows.agents.iter() {
        let stays_on_journey = (row.journeys.contains_key(&**agent_id) && !changes.gone_journeys.iter().any(|gone| gone == agent_id))
            || changes.new_journeys.iter().any(|(id, _)| id == agent_id);
        if !stays_on_journey && agent.activity != Activity::Queued && !completed.contains(&&**agent_id) {
            changes.positions.push((agent_id.clone(), scene.stationary(agent_id, agent, None)?));
        }
    }
    plan_all_staff(scene, staff, minute + 1, &mut changes)?;
    plan.changes = changes;
    Ok(Some(plan))
}

/// What `advance_movement` takes besides the world and the minute: the floor plan and where the
/// staff's operations are read.
pub struct Tick<'a> {
    pub geo: &'a Geo,
    pub staff: &'a StaffOps,
}

/// What `advance` did.
pub enum Advanced {
    /// The walk reached a branch that reads the others: nothing is written, and the call is made
    /// again with `InOrder`.
    NeedsInOrder,
    /// Written: the people whose route to replan.
    Done(Vec<Arc<str>>),
    /// Nothing written yet: queue trips replan, whose pending events the PyO3 crate reads as rows
    /// before `write_walk`, so that nothing is refused after the writes begin.
    Planned(WalkPlan),
}

/// `advance_movement`, of a minute a float holds exactly, while the world holds `movement_v1`.
pub fn advance(world: &World, tick: &Tick<'_>, minute: i64, in_order: Option<&InOrder<'_>>) -> Result<Advanced> {
    let plan = {
        let row = world.row::<MovementRow>(KEY)?;
        let rows = world.rows()?;
        let queue = world.queue()?;
        let scene = Scene::new(world, tick.geo, &rows);
        let staff = StaffRecords::of(world, tick.staff)?;
        match in_order {
            None => match plan_walk(&scene, &queue, &row, minute, &staff, None)? {
                Some(plan) => plan,
                None => return Ok(Advanced::NeedsInOrder),
            },
            Some(in_order) => {
                let events = Events::of(world)?;
                let Some(plan) = plan_walk(&scene, &queue, &row, minute, &staff, Some((in_order, &events)))? else {
                    return unsupported("the walk in order asked for itself");
                };
                check_metric_state(world, in_order.metric_state_held)?;
                plan
            }
        }
    };
    if !plan.queue_replans.is_empty() {
        return Ok(Advanced::Planned(plan));
    }
    apply(world, plan.changes)?;
    Ok(Advanced::Done(plan.replans))
}

/// A walk with queue replans, written: the row's part, then each queue trip in the walk's order,
/// its old event settled as suppressed (`QUEUE_REPLAN_DETAIL`) through the world's writer and its
/// new trip written. The old events must be rows (the PyO3 crate reads the rows of the events
/// Python scheduled first): one that is not refuses before anything is written. Returns the people
/// to replan, and the serials of the queue's slots taken out, whose Python objects the PyO3 crate
/// lets go.
pub fn write_walk(world: &World, plan: WalkPlan, minute: i64) -> WriteResult<(Vec<Arc<str>>, Vec<u64>)> {
    let WalkPlan { changes, replans, queue_replans } = plan;
    let events = Events::of(world)?;
    let mut old = Vec::with_capacity(queue_replans.len());
    {
        let queue = world.queue()?;
        for replan in &queue_replans {
            match queue.serial_of(&replan.event_id).and_then(|serial| queue.slot(serial).row.clone()) {
                Some(row) => old.push(row),
                None => return Err(Error::unsupported("the trip's event has no row").into()),
            }
        }
    }
    apply(world, changes)?;
    let mut taken = Vec::new();
    for (old, replan) in old.into_iter().zip(queue_replans) {
        taken.extend(events.settle(&old, minute, EventStatus::Suppressed, Some(QUEUE_REPLAN_DETAIL))?.taken);
        write_queue_replan(world, replan)?;
    }
    Ok((replans, taken))
}

/// A queue replan's writes, after its old event is settled: the counts, and the new trip.
fn write_queue_replan(world: &World, replan: QueueReplan) -> WriteResult<()> {
    match replan.end {
        QueueReplanEnd::NoFacility => return Ok(()),
        QueueReplanEnd::Blocked => {
            add_metric(world, "physical_queue_replan_blocked_count")?;
            add_metric(world, "movement_replan_blocked_count")?;
            return Ok(());
        }
        QueueReplanEnd::PastTheEnd => add_metric(world, BLOCKED_METRIC)?,
        QueueReplanEnd::Placed => {}
        QueueReplanEnd::Trip(plan) => write_plan(world, replan.facility_id.as_deref().unwrap_or_default(), plan)?,
    }
    add_metric(world, "physical_queue_replan_count")?;
    add_metric(world, "movement_replan_count")?;
    Ok(())
}

// ---- start_movement, finish_movement -----------------------------------------------------------

/// What `start_movement` writes, changing nothing.
#[allow(clippy::too_many_arguments)]
pub fn plan_start(
    scene: &Scene<'_>,
    row: &MovementRow,
    agent_id: &Arc<str>,
    journey: Journey,
    origin: &Arc<str>,
    points: &[Pt],
    edge_ids: &[Arc<str>],
    started: i64,
    duration: i64,
) -> Result<Changes> {
    // The point of a journey without the points of a path, which the scene reads.
    let off_path = if points.is_empty() {
        let (elapsed, initial_progress) = walking::start_progress(row, started, duration);
        match edge_ids.first() {
            Some(edge) => scene.route_point(edge, if elapsed { initial_progress } else { 0.0 }, origin)?,
            None => scene.geo.center(origin)?,
        }
    } else {
        None
    };
    walking::plan_start(row, scene.rows, agent_id, journey, origin, points, edge_ids, started, duration, off_path)
}

/// A journey to start: what `start_movement` and the trips of `travel.rs` give.
pub struct Start<'a> {
    pub agent_id: &'a Arc<str>,
    pub journey: Journey,
    pub origin: &'a Arc<str>,
    pub points: &'a [Pt],
    pub edge_ids: &'a [Arc<str>],
    pub started: i64,
    pub duration: i64,
}

/// `plan_start` on the world's rows, while the world holds `movement_v1`.
pub fn plan_start_in(world: &World, geo: &Geo, start: Start<'_>) -> Result<Changes> {
    let row = world.row::<MovementRow>(KEY)?;
    let rows = world.rows()?;
    let scene = Scene::new(world, geo, &rows);
    let Start { agent_id, journey, origin, points, edge_ids, started, duration } = start;
    plan_start(&scene, &row, agent_id, journey, origin, points, edge_ids, started, duration)
}

/// `start_movement`, while the world holds `movement_v1`.
pub fn start(world: &World, geo: &Geo, start: Start<'_>) -> Result<()> {
    let changes = plan_start_in(world, geo, start)?;
    apply(world, changes)
}

/// `finish_movement`, while the world holds `movement_v1`.
pub fn finish(world: &World, geo: &Geo, agent_id: &str) -> Result<()> {
    let changes = {
        let row = world.row::<MovementRow>(KEY)?;
        let mut changes = Changes::default();
        let journey = row.journeys.get(agent_id);
        let key: Arc<str> = Arc::from(agent_id);
        if journey.is_some() {
            changes.gone_journeys.push(key.clone());
        }
        if journey.is_some_and(|journey| is_queue_trip(&journey.purpose)) {
            // The person is still reserved in the FIFO queue: keep their surveyed arrival.
            if let Some(position) = row.positions.get(agent_id).and_then(arrived) {
                changes.positions.push((key, position));
            }
        } else {
            let rows = world.rows()?;
            if let Some(agent) = rows.agents.get(agent_id) {
                let scene = Scene::new(world, geo, &rows);
                changes.positions.push((key, scene.stationary(agent_id, agent, Some(&row))?));
            }
        }
        changes
    };
    apply(world, changes)
}

/// `state["journeys"].pop(agent_id)`.
pub fn stop_journey(world: &World, agent_id: &Arc<str>) -> Result<()> {
    let mut changes = Changes::default();
    changes.gone_journeys.push(agent_id.clone());
    apply(world, changes)
}

/// `state["positions"][agent_id] = ...` of someone who is at the end of a path in a room.
pub fn place_in_room(world: &World, agent_id: &Arc<str>, zone_id: &Arc<str>, point: Pt) -> Result<()> {
    let mut changes = Changes::default();
    changes.positions.push((agent_id.clone(), record(zone_id.clone(), Some(point), KINDS.physical.clone(), None, 0.0)));
    apply(world, changes)
}

/// `state["positions"][agent_id].update(zone_id=..., point=point, edge_id=None, progress=1.0,
/// position_kind="physical")` (`kit::place_in_bed`): false, having written nothing, where the row
/// has no record of the person; the row is written only where the record changes.
pub fn place_at_point(world: &World, agent_id: &str, zone_id: &Arc<str>, point: Pt) -> Result<bool> {
    {
        let row = world.row::<MovementRow>(KEY)?;
        let Some(current) = row.positions.get(agent_id) else {
            return Ok(false);
        };
        let placed = Position {
            zone_id: zone_id.clone(),
            point: Some(Point { x: point.x, y: point.y }),
            edge_id: None,
            progress: 1.0,
            position_kind: KINDS.physical.clone(),
        };
        if same_position(current, &placed) {
            return Ok(true);
        }
    }
    crate::rules::kit::place_in_bed(&mut *world.row_mut::<MovementRow>(KEY)?, agent_id, zone_id, point);
    Ok(true)
}

// ---- Where a person is, of the queues and the facilities -----------------------------------------

/// The FIFO of a kernel's `queue_id`, or None for none or a world without the namespace.
fn fifo_of(world: &World, queues: &Held, geo: &Geo, queue_id: Option<&str>) -> Result<Option<Fifo>> {
    let Some(queue_id) = queue_id else {
        return Ok(None);
    };
    match fifo::queues(world, queues)? {
        Some(row) => Fifo::of(&row, geo, queue_id).map(Some),
        None => Ok(None),
    }
}

/// What the readers of a person's place take besides the world: the floor plan, and whether the
/// Python world holds `movement_v1` and `facility_queues_v1`.
pub struct Where<'a> {
    pub geo: &'a Geo,
    pub movement: Held,
    pub queues: Held,
}

/// Whether the person's record is this near a point. A record's point is not usable: the rule's.
fn is_near(row: &MovementRow, agent_id: &str, point: Pt) -> Result<bool> {
    let Some(current) = position_point(row.positions.get(agent_id)) else {
        return Ok(false);
    };
    if !usable(current.x, current.y) {
        return unsupported("a position is not a usable point");
    }
    Ok(distance(point, current) <= ARRIVED_WITHIN)
}

/// `_queue_slot_position` of a facility's own queue or of an independent one (`queue_id`).
pub fn queue_slot_position(world: &World, at: &Where<'_>, agent_id: &str, facility_id: &str, queue_id: Option<&str>) -> Result<Option<Pt>> {
    let fifo = fifo_of(world, &at.queues, at.geo, queue_id)?;
    let independent = fifo.as_ref().map(Fifo::slot_view);
    with_positions(world, &at.movement, |row| queue_slot_in(at.geo, row, &*world.rows()?, agent_id, facility_id, independent))
}

/// `agent_is_at_queue_slot`.
pub fn at_queue_slot(world: &World, at: &Where<'_>, agent_id: &str, facility_id: &str, queue_id: Option<&str>) -> Result<bool> {
    if at.geo.layout.is_none() {
        return Ok(false);
    }
    let fifo = fifo_of(world, &at.queues, at.geo, queue_id)?;
    let independent = fifo.as_ref().map(Fifo::slot_view);
    with_positions(world, &at.movement, |row| {
        let rows = world.rows()?;
        if rows.facilities.get(facility_id).is_none() {
            return Ok(false);
        }
        match queue_slot_in(at.geo, row, &rows, agent_id, facility_id, independent)? {
            Some(target) => is_near(row, agent_id, target),
            None => Ok(false),
        }
    })
}

/// The zones of the person and of the facility: what `agent_is_at_facility_service_point` and
/// `facility_approach_route` of a world without a floor plan read.
pub fn agent_and_facility(world: &World, agent_id: &str, facility_id: &str) -> Result<(Arc<str>, Arc<str>)> {
    let rows = world.rows()?;
    let (Some(agent), Some(facility)) = (rows.agents.get(agent_id), rows.facilities.get(facility_id)) else {
        return unsupported("no such agent or facility");
    };
    Ok((agent.zone_id.clone(), facility.zone_id.clone()))
}

/// `facility_approach_route` in a placed world (`kit::movement::approach_point`).
pub fn approach_route(
    world: &World,
    at: &Where<'_>,
    model: &Read<MovementModel>,
    agent_id: &str,
    facility_id: &str,
    service_point: Option<&str>,
) -> Result<Option<(i64, Arc<[Pt]>)>> {
    with_positions(world, &at.movement, |row| approach_point(at.geo, row, &*world.rows()?, model, agent_id, facility_id, service_point))
}

/// `agent_is_at_facility_service_point` in a placed world.
pub fn at_service_point(
    world: &World,
    at: &Where<'_>,
    model: &Read<MovementModel>,
    agent_id: &str,
    facility_id: &str,
    service_point: Option<&str>,
) -> Result<bool> {
    let found = approach_route(world, at, model, agent_id, facility_id, service_point)?;
    Ok(found.is_some_and(|(minutes, path)| minutes <= 0 || path.len() < 2))
}

/// `agent_is_at_queue_trip_destination`, of the person's own journey in the row.
pub fn at_trip_destination(world: &World, movement: &Held, agent_id: &str) -> Result<bool> {
    with_positions(world, movement, |row| {
        let Some(point) = row.journeys.get(agent_id).and_then(|journey| journey.route_points.last()) else {
            return Ok(false);
        };
        if !usable(point.x, point.y) {
            return unsupported("a route point is not a usable point");
        }
        is_near(row, agent_id, Pt { x: point.x, y: point.y })
    })
}

/// `agent_is_at_queue_trip_destination`, of the last point of a journey given.
pub fn near_point(world: &World, movement: &Held, agent_id: &str, point: Pt) -> Result<bool> {
    with_positions(world, movement, |row| is_near(row, agent_id, point))
}

// ---- route_with_movement_time ------------------------------------------------------------------

/// The minutes `route_with_movement_time` gives a route of these zones and edges, under the
/// movement model, where the configs time routes.
#[allow(clippy::too_many_arguments)]
pub fn movement_minutes(world: &World, geo: &Geo, model: &MovementModel, agent_id: &str, zones: &[&str], edges: &[&str], declared: i64) -> Result<i64> {
    let mut wanted: Vec<(&str, i64)> = zones.iter().map(|zone| (*zone, 0)).collect();
    let speed = {
        let rows = world.rows()?;
        let Some(agent) = rows.agents.get(agent_id) else {
            return unsupported("no such agent");
        };
        for (_, other) in rows.agents.iter() {
            for (zone, count) in wanted.iter_mut() {
                if *zone == &*other.zone_id {
                    *count += 1;
                }
            }
        }
        model.speed(mobility_text(agent))
    };
    let mut lengths = Vec::new();
    if let Some(layout) = &geo.layout {
        for edge in edges {
            if let Some(found) = layout.routes.get(*edge) {
                lengths.push(polyline_length(&found.points).ok_or_else(|| Error::unsupported("not a plain sum"))?);
            }
        }
    }
    let distance_m = py_fsum(&lengths).ok_or_else(|| Error::unsupported("not a plain sum"))?;
    let base = model.independent;
    let physical = if distance_m > 0.0 { distance_m / base } else { declared as f64 };
    let mut density: Option<f64> = None;
    if let Some(layout) = &geo.layout {
        let mut done: Vec<&str> = Vec::with_capacity(wanted.len());
        for &(zone, count) in &wanted {
            if done.contains(&zone) {
                continue;
            }
            done.push(zone);
            if let Some(area) = layout.zones.get(zone).map(|zone| zone.usable_area).filter(|area| *area > 0.0) {
                let value = count as f64 / area;
                density = Some(density.map_or(value, |best| py_max(best, value)));
            }
        }
    }
    let crowding = py_min(model.maximum_slowdown, 1.0 + model.density_slowdown * py_max(0.0, density.unwrap_or(0.0) - 0.5));
    let duration = py_max(declared as f64, physical) * (base / speed) * crowding;
    if edges.is_empty() {
        return Ok(declared);
    }
    if !duration.is_finite() || duration.abs() >= EXACT_INT as f64 {
        return unsupported("the duration is too large");
    }
    Ok((duration.ceil() as i64).max(1))
}

// ---- validate ----------------------------------------------------------------------------------

/// `position_kind` of the two record maps, with the rules both share.
fn position_is_plain(position: &Position, rows: &Entities) -> bool {
    if !rows.zones.contains_key(&position.zone_id)
        || !matches!(&*position.position_kind, "physical" | "display_only" | "unavailable")
        || !(0.0..=1.0).contains(&position.progress)
    {
        return false;
    }
    if position.point.is_some_and(|point| !point.x.is_finite() || !point.y.is_finite()) {
        return false;
    }
    match &position.edge_id {
        Some(edge) => rows.zone_edges.contains_key(edge),
        None => true,
    }
}

/// One journey against what `validate_movement_runtime_state` checks.
fn journey_is_valid(world: &World, queues: &Held, rows: &Entities, agent: &AgentRow, journey: &Journey, geo: Option<&Geo>) -> Result<bool> {
    let support = journey.support_request_id.is_some() && journey.support_staff_id.is_some();
    if journey.support_request_id.is_some() != journey.support_staff_id.is_some() {
        return Ok(false);
    }
    let staff_travel = &*journey.purpose == "staff_support_travel";
    if support != staff_travel {
        return Ok(false);
    }
    if support && journey.support_request_id.iter().chain(&journey.support_staff_id).any(|id| id.is_empty()) {
        return Ok(false);
    }
    if let Some(queue_id) = &journey.queue_id {
        if queue_id.is_empty() || !is_queue_trip(&journey.purpose) {
            return Ok(false);
        }
        // The Python rule reads the queue's facility: anything with a queue ID goes to it.
        let Some(row) = fifo::queues(world, queues)? else {
            return Ok(false);
        };
        match (row.queues.get(queue_id), &journey.facility_id) {
            (Some(queue), Some(id)) if queue.facility_id == *id => {}
            _ => return Ok(false),
        }
    }
    if !rows.zones.contains_key(&journey.destination_zone_id) || journey.due_minute <= journey.started_minute {
        return Ok(false);
    }
    if journey.route_zone_ids.iter().any(|zone| !rows.zones.contains_key(zone)) {
        return Ok(false);
    }
    if journey.route_edge_ids.iter().any(|edge| !rows.zone_edges.contains_key(edge)) {
        return Ok(false);
    }
    if journey.route_points.is_empty() {
        return Ok(true);
    }
    if journey.route_points.len() < 2
        || !journey.route_edge_ids.is_empty()
        || journey.route_zone_ids.len() != 2
        || journey.route_zone_ids[0] != agent.zone_id
        || journey.route_zone_ids[1] != journey.destination_zone_id
    {
        return Ok(false);
    }
    let Ok(points) = points_of_journey(journey) else {
        return Ok(false);
    };
    let Some(zone) = geo.and_then(|geo| geo.layout.as_ref()).and_then(|layout| layout.zones.get(&agent.zone_id)) else {
        return Ok(false);
    };
    Ok(points.iter().all(|point| point_in_polygon(*point, &zone.polygon)))
}

/// What `validate` found.
#[derive(Debug, PartialEq, Eq)]
pub enum Validated {
    /// A journey walks in a room, which needs the floor plan: call again with it.
    NeedsGeo,
    Is(bool),
}

/// `validate_movement_runtime_state` of the row, while the world holds `movement_v1`: true accepts,
/// false leaves it to the Python rule and its error text.
pub fn validate(world: &World, queues: &Held, geo: Option<&Geo>) -> Result<Validated> {
    let row = world.row::<MovementRow>(KEY)?;
    if geo.is_none() && row.journeys.values().any(|journey| !journey.route_points.is_empty()) {
        return Ok(Validated::NeedsGeo);
    }
    if row.schema_version != 2 || &*row.policy_version != POLICY_VERSION {
        return Ok(Validated::Is(false));
    }
    let rows = world.rows()?;
    if row.positions.len() != rows.agents.len() || row.staff_positions.len() != rows.staff.len() {
        return Ok(Validated::Is(false));
    }
    for (id, position) in &row.positions {
        if rows.agents.get(id).is_none() || !position_is_plain(position, &rows) {
            return Ok(Validated::Is(false));
        }
    }
    for (id, position) in &row.staff_positions {
        if rows.staff.get(id).is_none() || !position_is_plain(position, &rows) {
            return Ok(Validated::Is(false));
        }
    }
    for (id, journey) in &row.journeys {
        let Some(agent) = rows.agents.get(id) else {
            return Ok(Validated::Is(false));
        };
        if !journey_is_valid(world, queues, &rows, agent, journey, geo)? {
            return Ok(Validated::Is(false));
        }
    }
    Ok(Validated::Is(true))
}
