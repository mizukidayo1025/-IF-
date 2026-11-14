//! `validate_movement_runtime_state(world)` of `domain/movement.py`, with Python's errors and
//! texts (lane E of #250, #350). Raw errors: `validate_simulator_extensions` passes them on.
//!
//! `py` holds what Python does with a JSON value where the validators of this lane read one.
//!
//! The check `native.movement_positions_valid` stands for where it passes is the Python loop
//! itself here.

pub(super) mod py;

use std::cmp::Ordering;

use self::py::{contains, eq, has_exactly, in_keys, in_texts, is_finite, is_int, is_number, is_text, item, num_cmp, Held};

/// `world.model_state.get(...)`: the tree of the world, its lists `TrackedList`.
const HELD: Held = Held::Tracked;
use super::geometry_overlay::effective_geometry;
use super::View;
use crate::entities::Entities;
use crate::error::{Error, Result};
use crate::geometry::{point_in_polygon, Pt};
use crate::text::py_repr;
use crate::value::V;

const KEY: &str = "movement_v1";
const QUEUES: &str = "facility_queues_v1";
const POLICY: &str = "spatial-route-and-room-waypoint-v2";
const STATE_FIELDS: [&str; 7] = ["schema_version", "policy_version", "positions", "staff_positions", "journeys", "edge_usage_minutes", "last_updated_minute"];
const POSITION_FIELDS: [&str; 5] = ["zone_id", "point", "edge_id", "progress", "position_kind"];
const POSITION_KINDS: [&str; 3] = ["physical", "display_only", "unavailable"];
const QUEUE_TRIP_PURPOSES: [&str; 4] = ["toilet_queue_position", "resource_queue_position", "toilet_queue_service", "resource_queue_service"];
const JOURNEY_FIELDS: [&str; 10] = [
    "event_id",
    "task_id",
    "purpose",
    "facility_id",
    "destination_zone_id",
    "route_zone_ids",
    "route_edge_ids",
    "route_points",
    "started_minute",
    "due_minute",
];
const SUPPORT_FIELDS: [&str; 2] = ["support_request_id", "support_staff_id"];
const STAFF_SUPPORT: &str = "staff_support_travel";

/// The check, as `validate` (`validate/mod.rs`) calls it in Python's order.
pub fn validate(view: &View<'_>) -> Result<()> {
    let Some(state) = view.state(KEY)? else {
        return Ok(());
    };
    let schema = || Error::Value("movement_v1 has an invalid exact schema".to_owned());
    let map = match &*state {
        V::Null => return Ok(()),
        V::Map(map) => map,
        _ => return Err(schema()),
    };
    if !has_exactly(map, &STATE_FIELDS) || !eq(&map["schema_version"], &V::Int(2)) || !is_text(&map["policy_version"], POLICY) {
        return Err(schema());
    }
    let (V::Map(positions), V::Map(staff_positions), V::Map(journeys), V::Map(_)) =
        (&map["positions"], &map["staff_positions"], &map["journeys"], &map["edge_usage_minutes"])
    else {
        return Err(schema());
    };
    let rows = view.rows()?;
    if positions.len() != rows.agents.len() || positions.keys().any(|id| !rows.agents.contains_key(id)) {
        return Err(Error::Value("movement_v1 positions must contain exactly the active Agents".to_owned()));
    }
    if staff_positions.len() != rows.staff.len() || staff_positions.keys().any(|id| !rows.staff.contains_key(id)) {
        return Err(Error::Value("movement_v1 staff positions must contain exactly the active staff".to_owned()));
    }
    for (staff_id, position) in staff_positions {
        if !staff_position_valid(&rows, position)? {
            return Err(Error::Value(format!("movement_v1 staff position for {} is invalid", py_repr(staff_id))));
        }
    }
    for (agent_id, position) in positions {
        agent_position(&rows, agent_id, position)?;
    }
    for (agent_id, journey) in journeys {
        if !journey_valid(view, &rows, agent_id, journey)? {
            return Err(Error::Value(format!("movement_v1 journey for {} is invalid", py_repr(agent_id))));
        }
        let V::Map(journey) = journey else {
            unreachable!("journey_valid passes only a dict");
        };
        in_room_route(view, &rows, agent_id, journey)?;
    }
    Ok(())
}

/// `0.0 <= value <= 1.0` of a number.
fn in_unit(value: &V) -> bool {
    num_cmp(&V::Float(0.0), value).is_some_and(Ordering::is_le) && num_cmp(value, &V::Float(1.0)).is_some_and(Ordering::is_le)
}

/// `isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value)`.
fn finite_number(value: &V) -> bool {
    is_number(value) && is_finite(value)
}

/// A point of a position or a route: `{"x", "y"}`, each a finite number.
fn point_valid(point: &V) -> bool {
    matches!(point, V::Map(point) if has_exactly(point, &["x", "y"]) && finite_number(&point["x"]) && finite_number(&point["y"]))
}

fn staff_position_valid(rows: &Entities, position: &V) -> Result<bool> {
    let V::Map(position) = position else {
        return Ok(false);
    };
    if !has_exactly(position, &POSITION_FIELDS) || !in_keys(&position["zone_id"], HELD, |id| rows.zones.contains_key(id))? {
        return Ok(false);
    }
    let edge = &position["edge_id"];
    if !matches!(edge, V::Null) && !in_keys(edge, HELD, |id| rows.zone_edges.contains_key(id))? {
        return Ok(false);
    }
    if !in_texts(&position["position_kind"], HELD, &POSITION_KINDS)? {
        return Ok(false);
    }
    let progress = &position["progress"];
    Ok(is_number(progress) && in_unit(progress))
}

/// The checks of one person's position, each with its text.
fn agent_position(rows: &Entities, agent_id: &str, position: &V) -> Result<()> {
    let invalid = |what: &str| Err(Error::Value(format!("movement_v1 {what} for {} is invalid", py_repr(agent_id))));
    let V::Map(position) = position else {
        return invalid("position");
    };
    if !has_exactly(position, &POSITION_FIELDS)
        || !in_keys(&position["zone_id"], HELD, |id| rows.zones.contains_key(id))?
        || !in_texts(&position["position_kind"], HELD, &POSITION_KINDS)?
    {
        return invalid("position");
    }
    let point = &position["point"];
    if !matches!(point, V::Null) && !point_valid(point) {
        return invalid("point");
    }
    let progress = &position["progress"];
    if !finite_number(progress) || !in_unit(progress) {
        return invalid("progress");
    }
    let edge = &position["edge_id"];
    if !matches!(edge, V::Null) && !in_keys(edge, HELD, |id| rows.zone_edges.contains_key(id))? {
        return Err(Error::Value(format!("movement_v1 edge for {} is unknown", py_repr(agent_id))));
    }
    Ok(())
}

/// The fields of a journey are one of its four shapes.
fn journey_shape(journey: &indexmap::IndexMap<std::sync::Arc<str>, V>) -> bool {
    let has_support = SUPPORT_FIELDS.iter().filter(|name| journey.contains_key(**name)).count();
    let has_queue = usize::from(journey.contains_key("queue_id"));
    JOURNEY_FIELDS.iter().all(|name| journey.contains_key(*name))
        && (has_support == 0 || has_support == 2)
        && journey.len() == JOURNEY_FIELDS.len() + has_support + has_queue
}

/// The long condition of a journey, in Python's order: false where it is invalid, or the error a
/// part of it raises.
fn journey_valid(view: &View<'_>, rows: &Entities, agent_id: &str, journey: &V) -> Result<bool> {
    if !rows.agents.contains_key(agent_id) {
        return Ok(false);
    }
    let V::Map(journey) = journey else {
        return Ok(false);
    };
    if !journey_shape(journey) {
        return Ok(false);
    }
    let has_support = SUPPORT_FIELDS.iter().all(|name| journey.contains_key(*name));
    let purpose = &journey["purpose"];
    if has_support && !is_text(purpose, STAFF_SUPPORT) {
        return Ok(false);
    }
    if is_text(purpose, STAFF_SUPPORT) && (!has_support || !SUPPORT_FIELDS.iter().all(|name| matches!(&journey[*name], V::Str(id) if !id.is_empty()))) {
        return Ok(false);
    }
    if let Some(queue_id) = journey.get("queue_id") {
        if !matches!(queue_id, V::Str(id) if !id.is_empty()) || !in_texts(purpose, HELD, &QUEUE_TRIP_PURPOSES)? {
            return Ok(false);
        }
        if !view.contains(QUEUES)? {
            return Ok(false);
        }
        let queues_state = view.state(QUEUES)?.ok_or_else(|| Error::Key(QUEUES.to_owned()))?;
        let queues = item(&queues_state, "queues")?;
        if !contains(queues, queue_id, HELD)? {
            return Ok(false);
        }
        let V::Str(queue_id) = queue_id else {
            unreachable!("checked above");
        };
        if !eq(item(item(queues, queue_id)?, "facility_id")?, &journey["facility_id"]) {
            return Ok(false);
        }
    }
    if !in_keys(&journey["destination_zone_id"], HELD, |id| rows.zones.contains_key(id))? {
        return Ok(false);
    }
    let (V::List(zone_ids), V::List(edge_ids), V::List(_)) = (&journey["route_zone_ids"], &journey["route_edge_ids"], &journey["route_points"]) else {
        return Ok(false);
    };
    for zone_id in zone_ids {
        if !in_keys(zone_id, HELD, |id| rows.zones.contains_key(id))? {
            return Ok(false);
        }
    }
    for edge_id in edge_ids {
        if !in_keys(edge_id, HELD, |id| rows.zone_edges.contains_key(id))? {
            return Ok(false);
        }
    }
    let (started, due) = (&journey["started_minute"], &journey["due_minute"]);
    Ok(is_int(started) && is_int(due) && num_cmp(due, started) == Some(Ordering::Greater))
}

/// The in-room route of a journey with route points: its shape, then that it stays in the person's
/// room of the floor plan (`effective_geometry`).
fn in_room_route(view: &View<'_>, rows: &Entities, agent_id: &str, journey: &indexmap::IndexMap<std::sync::Arc<str>, V>) -> Result<()> {
    let V::List(points) = &journey["route_points"] else {
        return Ok(());
    };
    if points.is_empty() {
        return Ok(());
    }
    let agent_zone = rows.agents.get(agent_id).map(|agent| agent.zone_id.clone()).ok_or_else(|| Error::Key(agent_id.to_owned()))?;
    let route_zones = V::List(vec![V::Str(agent_zone.clone()), journey["destination_zone_id"].clone()]);
    if points.len() < 2 || journey["route_edge_ids"].is_truthy() || !eq(&journey["route_zone_ids"], &route_zones) || !points.iter().all(point_valid) {
        return Err(Error::Value(format!("movement_v2 in-room route for {} is invalid", py_repr(agent_id))));
    }
    let geometry = effective_geometry(view)?;
    let zone = geometry.as_ref().and_then(|geometry| geometry.zone(&agent_zone));
    let leaves = match zone {
        None => true,
        Some(polygon) => points.iter().any(|point| !point_in_polygon(pt(point), polygon)),
    };
    if leaves {
        return Err(Error::Value(format!("movement_v2 in-room route for {} leaves its room", py_repr(agent_id))));
    }
    Ok(())
}

/// `Point2D.model_validate(point)` of a point `point_valid` passed.
fn pt(point: &V) -> Pt {
    let coordinate = |name: &str| match point {
        V::Map(map) => py::float_of(&map[name]).unwrap_or(f64::NAN),
        _ => f64::NAN,
    };
    Pt { x: coordinate("x"), y: coordinate("y") }
}
