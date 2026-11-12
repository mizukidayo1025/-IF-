//! `AgentTravelCompletionHandler` of `events/handlers.py`: a walk that ended (lane c7 of Issue
//! #152; the contracts are the handler's docstrings). The rules are functions on the `World`: they
//! read the event's row and the rows, decide, and write the task, the person's row, the bed's place
//! in `movement_v1` and a relocation's completion (`population_lifecycle`). Nothing is drawn here.
//!
//! A walk the rule refuses raises the rule's own `EventApplicationError` text before any change, in
//! the rule's order of checks (`decide`); every check comes before the first write. Where a value
//! is not what a valid state holds, the rule says Unsupported, still before any change.
//!
//! What a walk to a toilet or a resource leads to (`_after_arrival`) is the PyO3 crate's glue over
//! the functions here (`arrival_approach`, `reject_room`, `rejected_request`) and the toilet's and
//! the resource service's own requests, which a manager may override (a test's hook).

use std::sync::Arc;

use crate::config::movement::Movement;
use crate::entities::{Activity, RequestStatus};
use crate::error::{Error, Result};
use crate::events::EventRow;
use crate::geometry::{Geo, Pt};
use crate::row::ValueRow;
use crate::rules::kit::{self, Events, Held, Kept};
use crate::rules::population_lifecycle::{self as lifecycle, KEY as LIFECYCLE};
use crate::rules::travel::{self, PlannedTrip, Walking};
use crate::rules::{facility_queues, movement, service};
use crate::state::movement::MovementRow;
use crate::state::resource_service::RequestRecord;
use crate::text::py_repr;
use crate::value::V;
use crate::config::SectionId;
use crate::host::Host;
use crate::world::{World, WorldCell};

#[track_caller]
fn refuse<T>(reason: &'static str) -> Result<T> {
    Err(Error::unsupported(reason))
}

fn failed<T>(text: impl Into<String>) -> Result<T> {
    Err(Error::failed(text))
}

/// The rule's `origin_zone_id`: the event's when a str, else the first of a list route, else None.
#[derive(Clone, Debug)]
pub enum Origin {
    Zone(Arc<str>),
    /// No route: None.
    Nothing,
    /// A route whose first is not a str, or an empty one: not a str, which may be None.
    NotStr,
}

impl Origin {
    pub fn zone(&self) -> Option<&Arc<str>> {
        match self {
            Origin::Zone(zone) => Some(zone),
            _ => None,
        }
    }
}

/// What the rule reads of a walk's event.
#[derive(Clone, Debug)]
pub struct Walk {
    pub event_id: Arc<str>,
    pub due_minute: i64,
    pub agent_id: Arc<str>,
    pub task_id: Arc<str>,
    pub destination: Arc<str>,
    pub purpose: Arc<str>,
    pub facility_id: Option<Arc<str>>,
    pub origin: Origin,
    route_ends: Option<(Option<Arc<str>>, Option<Arc<str>>)>,
}

/// `payload.get(key)` when a str.
fn payload_text(row: &EventRow, key: &str) -> Option<Arc<str>> {
    match kit::payload(row, key) {
        Some(V::Str(value)) => Some(value.clone()),
        _ => None,
    }
}

impl Walk {
    /// The walk of an event, or the rule's text for a field it lacks.
    pub fn read(row: &EventRow) -> Result<Self> {
        let route_ends = match kit::payload(row, "route_zone_ids") {
            Some(V::List(items)) => {
                let text = |item: Option<&V>| match item {
                    Some(V::Str(value)) => Some(value.clone()),
                    _ => None,
                };
                Some((text(items.first()), text(items.last())))
            }
            _ => None,
        };
        let origin = match payload_text(row, "origin_zone_id") {
            Some(origin) => Origin::Zone(origin),
            None => match &route_ends {
                Some((Some(first), _)) => Origin::Zone(first.clone()),
                Some(_) => Origin::NotStr,
                None => Origin::Nothing,
            },
        };
        let required = |key: &str| -> Result<Arc<str>> {
            match payload_text(row, key) {
                Some(value) if !value.is_empty() => Ok(value),
                _ => failed("travel completion requires Agent, task, destination, and purpose"),
            }
        };
        Ok(Self {
            event_id: row.event_id.clone(),
            due_minute: row.due_minute,
            agent_id: required("agent_id")?,
            task_id: required("task_id")?,
            destination: required("destination_zone_id")?,
            purpose: required("purpose")?,
            facility_id: payload_text(row, "facility_id"),
            origin,
            route_ends,
        })
    }
}

/// The purposes of a walk to a queue's place or service point.
pub fn is_queue_walk(purpose: &str) -> bool {
    matches!(purpose, "toilet_queue_position" | "resource_queue_position" | "toilet_queue_service" | "resource_queue_service")
}

/// `ResourceServiceManager.resource_for_facility(kind) is None`.
fn is_resource_facility(kind: &str) -> bool {
    matches!(kind, "water_station" | "meal_distribution")
}

/// What the rule reads beside the world's rows.
pub struct Inputs<'a> {
    pub movement: Held,
    pub queues: Held,
    pub overlay: Held,
    /// The floor plan and the movement section, read for a queue walk alone.
    pub geo: Option<&'a Geo>,
    pub section: Option<&'a Movement>,
    /// Whether the handler has a resource service.
    pub resource_service: bool,
    /// The lifecycle's inputs, read for a relocation alone.
    pub lifecycle: Option<&'a lifecycle::Inputs<'a>>,
}

impl Inputs<'_> {
    fn geo(&self) -> Result<&Geo> {
        self.geo.ok_or_else(|| Error::unsupported("the floor plan was not read"))
    }

    fn lifecycle(&self) -> Result<&lifecycle::Inputs<'_>> {
        self.lifecycle.ok_or_else(|| Error::unsupported("the lifecycle's inputs were not read"))
    }
}

/// The person's journey in `movement_v1`, if the world holds the namespace and the record.
fn journey_of<'w>(world: &'w World, held: &Held, agent_id: &str) -> Result<Option<crate::state::movement::Journey>> {
    Ok(kit::held_row::<MovementRow>(world, movement::KEY, held)?.and_then(|row| row.journeys.get(agent_id).cloned()))
}

/// The walks to a queue's place or service point: whether the walk fits its FIFO reservation.
fn queue_walk_fits(world: &World, inputs: &Inputs<'_>, row: &EventRow, walk: &Walk) -> Result<bool> {
    let (agent_id, task_id, destination, purpose) = (&walk.agent_id, &walk.task_id, &walk.destination, &*walk.purpose);
    let facility_id = walk.facility_id.as_ref();
    let resource = purpose.starts_with("resource_");
    let expected_kind = if resource { "resource_service_queue" } else { "toilet_queue" };
    let position = purpose.ends_with("_position");
    let journey = journey_of(world, &inputs.movement, agent_id)?;
    let independent = match facility_id {
        Some(facility) => facility_queues::queues(world, &inputs.queues)?.is_some_and(|row| facility_queues::has_independent(&row, facility)),
        None => false,
    };
    let queue_id = payload_text(row, "queue_id");
    let at = |queue: Option<&str>| -> Result<movement::Where<'_>> {
        Ok(movement::Where { geo: inputs.geo()?, movement: inputs.movement.clone(), queues: if queue.is_some() { inputs.queues.clone() } else { Held::Absent } })
    };
    let at_service_point = |facility: &str, point: Option<&str>| -> Result<bool> {
        let geo = inputs.geo()?;
        if geo.layout.is_none() {
            // The route of no floor plan: none of its minutes.
            movement::agent_and_facility(world, agent_id, facility)?;
            return Ok(true);
        }
        let section = inputs.section.ok_or_else(|| Error::unsupported("the movement section was not read"))?;
        movement::at_service_point(world, &at(None)?, &section.model, agent_id, facility, point)
    };
    let (contains_request, at_destination) = if independent {
        let facility = facility_id.ok_or_else(|| Error::unsupported("an independent queue is a facility's"))?;
        let request_id = payload_text(row, "request_id");
        let (request, queued) = {
            let state = facility_queues::queues(world, &inputs.queues)?.ok_or_else(|| Error::unsupported("no queue state"))?;
            let request = request_id.as_ref().and_then(|id| state.requests.get(&**id).cloned());
            let queued = queue_id.as_ref().and_then(|id| state.queues.get(&**id)).map(|queue| queue.queued_request_ids.clone());
            (request, queued)
        };
        let contains = match (&request, &queued, &request_id, &queue_id) {
            (Some(request), Some(queued), Some(request_id), Some(queue_id)) => {
                queued.contains(request_id) && request.agent_id == *agent_id && &*request.facility_id == &**facility && request.queue_id == *queue_id
            }
            _ => false,
        };
        let here = if position {
            let Some(queue) = &queue_id else {
                return refuse("a queue walk's queue_id is not a str");
            };
            movement::at_queue_slot(world, &at(Some(queue))?, agent_id, facility, Some(queue))? || movement::at_trip_destination(world, &inputs.movement, agent_id)?
        } else {
            match &request {
                Some(request) => at_service_point(facility, Some(&request.service_point_id))?,
                None => false,
            }
        };
        (Some(contains), here)
    } else {
        let here = match facility_id {
            Some(facility) if position => movement::at_queue_slot(world, &at(None)?, agent_id, facility, None)? || movement::at_trip_destination(world, &inputs.movement, agent_id)?,
            Some(facility) => at_service_point(facility, None)?,
            None => false,
        };
        (None, here)
    };
    let journey_fits = match &journey {
        None => world.minute_index()? == walk.due_minute,
        Some(journey) => {
            journey.event_id == walk.event_id
                && journey.task_id == *task_id
                && &*journey.purpose == purpose
                && journey.due_minute == walk.due_minute
                && (!independent
                    || match &queue_id {
                        Some(queue) => journey.queue_id.as_ref() == Some(queue),
                        None => return refuse("a queue walk's queue_id is not a str"),
                    })
        }
    };
    let rows = world.rows()?;
    let (Some(task), Some(agent)) = (rows.active_tasks.get(task_id), rows.agents.get(agent_id)) else {
        return refuse("the task or the person left before the queue walk's checks");
    };
    let facility = facility_id.and_then(|id| rows.facilities.get(id));
    let contains = match contains_request {
        Some(contains) => contains,
        None => facility.is_some_and(|facility| {
            let queue = if resource { &facility.resource_queue_agent_ids } else { &facility.queue_agent_ids };
            queue.iter().any(|id| id == agent_id)
        }),
    };
    Ok(&*task.kind == expected_kind
        && task.due_minute == task.start_minute
        && agent.activity == Activity::Queued
        && agent.target_id.as_deref() == facility_id.map(|id| &**id)
        && contains
        && facility.is_some_and(|facility| facility.zone_id == *destination)
        && agent.zone_id == *destination
        && at_destination
        && journey_fits)
}

/// `value == number` in Python, of a value of a document.
fn equals_number(value: &V, number: f64) -> bool {
    match value {
        V::Float(found) => *found == number,
        V::Int(found) => (*found as f64) == number && (-(1_i64 << 53)..=1 << 53).contains(found),
        V::Bool(flag) => f64::from(u8::from(*flag)) == number,
        _ => false,
    }
}

/// `sleep_bed`'s reads, then its checks: the bed's point when the walk fits its live reservation,
/// None when the rule raises its text. `rows_fit` is its first two checks, the task's kind and the
/// person's target. Unsupported for a bed whose point the row of `movement_v1` can't hold, or a
/// person without a position there.
fn sleep_bed_fits(world: &World, inputs: &Inputs<'_>, walk: &Walk, rows_fit: bool) -> Result<Option<Pt>> {
    // `overlay.get("bed_reservations", {}).get(agent_id)` and its room, copied.
    let (reservation, room) = {
        let overlay = kit::held_row::<ValueRow>(world, crate::rules::overlay::OVERLAY_KEY, &inputs.overlay)?;
        match overlay.as_deref().map(|row| &row.0) {
            Some(V::Map(overlay)) => {
                let reservation = match overlay.get("bed_reservations") {
                    Some(V::Map(reservations)) => reservations.get(&*walk.agent_id).cloned(),
                    _ => None,
                };
                let room = match (&reservation, overlay.get("temporary_rooms")) {
                    (Some(V::Map(fields)), Some(V::Map(rooms))) => match fields.get("room_id") {
                        Some(V::Str(room_id)) => rooms.get(&**room_id).cloned(),
                        Some(V::List(_) | V::Map(_)) => return refuse("a reservation's room is not hashable"),
                        _ => None,
                    },
                    (Some(V::Map(_)), Some(V::Null) | None) => None,
                    (Some(V::Map(_)), Some(_)) => return refuse("the rooms are not a dict"),
                    _ => None,
                };
                (reservation, room)
            }
            _ => (None, None),
        }
    };
    let journey = journey_of(world, &inputs.movement, &walk.agent_id)?;
    let (Some(V::Map(reservation)), Some(V::Map(room))) = (&reservation, &room) else {
        return Ok(None);
    };
    // `type(bed_index) is int and 0 <= bed_index < len(room.get("bed_points_m", []))`.
    let bed = match (reservation.get("bed_index"), room.get("bed_points_m")) {
        (Some(V::Int(index)), Some(V::List(points))) if *index >= 0 && (*index as usize) < points.len() => Some(points[*index as usize].clone()),
        (Some(V::Int(_)), Some(_)) => return refuse("the bed points are not a list"),
        _ => None,
    };
    let Some(journey) = journey.filter(|_| rows_fit) else {
        return Ok(None);
    };
    let room_fits = matches!(room.get("zone_id"), Some(V::Str(zone)) if *zone == walk.destination);
    let journey_fits = journey.event_id == walk.event_id && journey.task_id == walk.task_id && &*journey.purpose == "sleep_bed";
    let Some(last) = journey.route_points.last() else {
        return Ok(None);
    };
    if !room_fits || !journey_fits {
        return Ok(None);
    }
    // `route_points[-1] != bed`: a dict of the same two numbers.
    let Some(V::Map(bed)) = bed else {
        return Ok(None);
    };
    let same = bed.len() == 2 && bed.get("x").is_some_and(|x| equals_number(x, last.x)) && bed.get("y").is_some_and(|y| equals_number(y, last.y));
    if !same {
        return Ok(None);
    }
    let (Some(V::Float(x)), Some(V::Float(y))) = (bed.get("x"), bed.get("y")) else {
        return refuse("a bed's point is not two floats");
    };
    if !kit::held_row::<MovementRow>(world, movement::KEY, &inputs.movement)?.is_some_and(|row| row.positions.contains_key(&*walk.agent_id)) {
        return refuse("the person has no position to put at the bed");
    }
    Ok(Some(Pt { x: *x, y: *y }))
}

/// `presence_relocation`'s reads and checks. `rows_fit` is its first two checks.
fn relocation_fits(world: &World, inputs: &Inputs<'_>, walk: &Walk, rows_fit: bool) -> Result<bool> {
    let lifecycle = inputs.lifecycle()?;
    let relocation = {
        let state = kit::held_row::<ValueRow>(world, LIFECYCLE, &lifecycle.lifecycle)?;
        match state.as_deref().map(|row| &row.0) {
            None => None,
            Some(V::Map(state)) => match state.get("pending_relocations") {
                None => None,
                Some(V::Map(relocations)) => relocations.get(&*walk.agent_id).cloned(),
                Some(_) => return refuse("the pending relocations are not a dict"),
            },
            Some(_) => return refuse("the lifecycle state is not a dict"),
        }
    };
    let Some(V::Map(relocation)) = relocation.filter(|_| rows_fit) else {
        return Ok(false);
    };
    let is = |key: &str, wanted: &str| matches!(relocation.get(key), Some(V::Str(found)) if &**found == wanted);
    let origin_fits = match &walk.origin {
        Origin::Zone(origin) => is("from_zone_id", origin),
        Origin::Nothing => matches!(relocation.get("from_zone_id"), None | Some(V::Null)),
        Origin::NotStr => return refuse("a relocation's origin is not a str"),
    };
    Ok(is("task_id", &walk.task_id) && is("event_id", &walk.event_id) && origin_fits && is("to_zone_id", &walk.destination))
}

/// What a walk that fits writes, decided before any change.
pub struct Decision {
    pub walk: Walk,
    activity: Activity,
    bed: Option<Pt>,
}

/// The rule's checks, in its order, before any change: None for a queue walk that fits (it writes
/// nothing), what to write for another walk that fits, or the rule's text.
pub fn decide(world: &World, inputs: &Inputs<'_>, row: &EventRow) -> Result<Option<Decision>> {
    let walk = Walk::read(row)?;
    let (agent_id, task_id, destination, purpose) = (&walk.agent_id, &walk.task_id, &walk.destination, &*walk.purpose);
    let facility_id = walk.facility_id.as_deref();
    // The checks every walk passes before its purpose: 1, the task's; 2, the person's.
    let checked = {
        let rows = world.rows()?;
        match rows.active_tasks.get(task_id) {
            Some(task) if matches!(&*task.kind, "travel_to_toilet" | "travel_to_resource" | "travel_return" | "staff_support_travel" | "toilet_queue" | "resource_service_queue") => {
                match rows.agents.get(agent_id) {
                    Some(agent) if task.agent_id.as_ref() == Some(agent_id) && agent.profile.id == *agent_id => 0,
                    _ => 2,
                }
            }
            _ => 1,
        }
    };
    match checked {
        1 => return failed("travel completion does not match an active task"),
        2 => return failed("travel completion Agent does not match its active task"),
        _ => {}
    }
    if is_queue_walk(purpose) {
        if !queue_walk_fits(world, inputs, row, &walk)? {
            return failed("physical queue travel does not match its FIFO reservation");
        }
        return Ok(None);
    }
    let (task_kind, target) = {
        let rows = world.rows()?;
        let (Some(task), Some(agent)) = (rows.active_tasks.get(task_id), rows.agents.get(agent_id)) else {
            return refuse("the task or the person left");
        };
        if task.due_minute != walk.due_minute {
            return failed("travel completion does not match an active due task");
        }
        if agent.activity != Activity::Moving {
            return failed("travel completion Agent does not match its active task");
        }
        (task.kind.clone(), agent.target_id.clone())
    };
    let zone_known = |zone: &str| -> Result<bool> {
        let rows = world.entities.try_borrow()?;
        if rows.zones_tainted().is_some() {
            return refuse("the zones are tainted");
        }
        Ok(rows.zones.contains_key(zone))
    };
    if !zone_known(destination)? {
        return failed(format!("destination zone {} does not exist", py_repr(destination)));
    }
    let at_target = |wanted: &str| target.as_deref() == Some(wanted);
    let returning = &*task_kind == "travel_return" && at_target(destination);
    let facility_of = |id: &str| -> Result<Option<(Arc<str>, Arc<str>)>> { Ok(world.rows()?.facilities.get(id).map(|found| (found.zone_id.clone(), found.kind.clone()))) };
    let mut bed = None;
    match purpose {
        "toilet_request" => {
            let Some(facility) = facility_id.filter(|id| !id.is_empty()) else {
                return failed("toilet travel completion requires a facility_id");
            };
            if !facility_of(facility)?.is_some_and(|(zone, _)| zone == *destination) {
                return failed("toilet travel destination does not match its facility");
            }
            if &*task_kind != "travel_to_toilet" || !at_target(facility) {
                return failed("toilet travel completion does not match its target");
            }
        }
        "resource_request" => {
            if !inputs.resource_service {
                return failed("resource travel is not enabled for this scenario");
            }
            let Some(facility) = facility_id.filter(|id| !id.is_empty()) else {
                return failed("resource travel completion requires a facility_id");
            };
            // The origin a str and a zone, and the route a list from it to the destination.
            let route_fits = match &walk.origin {
                Origin::Zone(origin) => zone_known(origin)? && matches!(&walk.route_ends, Some((Some(first), Some(last))) if first == origin && last == destination),
                Origin::Nothing | Origin::NotStr => false,
            };
            let fits = facility_of(facility)?.is_some_and(|(zone, kind)| zone == *destination && route_fits && is_resource_facility(&kind)) && &*task_kind == "travel_to_resource" && at_target(facility);
            if !fits {
                return failed("resource travel completion does not match its target facility");
            }
        }
        "return" | "queue_abandon" => {
            if !returning {
                return failed("return travel completion does not match its target");
            }
        }
        "staff_support_travel" => {
            let (request_id, staff_id) = (payload_text(row, "request_id"), payload_text(row, "staff_id"));
            let fits = {
                let rows = world.rows()?;
                let agent = rows.agents.get(agent_id).ok_or_else(|| Error::unsupported("the person left"))?;
                let request = request_id.as_ref().and_then(|id| rows.support_requests.get(id));
                let staff = staff_id.as_ref().and_then(|id| rows.staff.get(id));
                match (request, staff) {
                    (Some(request), Some(staff)) => {
                        &*task_kind == "staff_support_travel"
                            && request.agent_id == *agent_id
                            && request.status == RequestStatus::Assigned
                            && request.assigned_staff_id == staff_id
                            && request.task_id.is_some()
                            && agent.support_request_id.as_ref() == Some(&request.id)
                            && staff.zone_id == *destination
                            && at_target(destination)
                    }
                    _ => false,
                }
            };
            if !fits {
                return failed("staff support travel does not match its assigned request and staff member");
            }
        }
        "sleep_bed" => match sleep_bed_fits(world, inputs, &walk, returning)? {
            Some(point) => bed = Some(point),
            None => return failed("sleep-bed travel does not match its live reservation"),
        },
        "presence_relocation" => {
            if !relocation_fits(world, inputs, &walk, returning)? {
                return failed("presence relocation completion does not match its route");
            }
        }
        _ => return failed(format!("unsupported travel purpose {}", py_repr(purpose))),
    }
    let activity = match purpose {
        "staff_support_travel" => Activity::SeekingHelp,
        "sleep_bed" => Activity::Sleeping,
        _ => Activity::Idle,
    };
    Ok(Some(Decision { walk, activity, bed }))
}

/// The writes of a walk that fits: the bed's place, the person in the destination with no target,
/// the task gone, and a relocation's completion.
pub fn write(world: &World, inputs: &Inputs<'_>, row: &EventRow, decision: Decision) -> Result<()> {
    let Decision { walk, activity, bed } = decision;
    if let Some(point) = bed {
        let mut movement = world.row_mut::<MovementRow>(movement::KEY)?;
        kit::place_in_bed(&mut movement, &walk.agent_id, &walk.destination, point);
    }
    {
        let mut rows = world.rows_mut()?;
        let agent = rows.agents.get_mut(&walk.agent_id).ok_or_else(|| Error::Key(walk.agent_id.to_string()))?;
        agent.zone_id = walk.destination.clone();
        agent.activity = activity;
        agent.target_id = None;
        if !rows.active_tasks.remove_row(&walk.task_id) {
            return Err(Error::Key(walk.task_id.to_string()));
        }
    }
    if &*walk.purpose == "presence_relocation" {
        kit::writing(lifecycle::complete_relocation(world, inputs.lifecycle()?, row))?;
    }
    Ok(())
}

/// A walk's checks and writes (`apply` without `_after_arrival`): the walk written, or None for a
/// queue walk that writes nothing. An error of the rule's checks (`decide`) is `Checks`; what a
/// relocation's completion refuses before the writes, and any error of the writes, is `After`.
pub fn apply(world: &World, inputs: &Inputs<'_>, row: &EventRow) -> std::result::Result<Option<Walk>, Applied> {
    let decision = match decide(world, inputs, row) {
        Ok(Some(decision)) => decision,
        Ok(None) => return Ok(None),
        Err(error) => return Err(Applied::Checks(error)),
    };
    let walk = decision.walk.clone();
    if &*walk.purpose == "presence_relocation" {
        let lifecycle = inputs.lifecycle().map_err(Applied::After)?;
        lifecycle::check_relocation_completes(world, lifecycle, row, &walk.destination).map_err(Applied::After)?;
    }
    write(world, inputs, row, decision).map_err(Applied::After)?;
    Ok(Some(walk))
}

/// Why a walk stopped: in the rule's checks, before any write, or past them.
#[derive(Debug)]
pub enum Applied {
    Checks(Error),
    After(Error),
}

// ---- `_after_arrival` and `_reject_room_approach` ------------------------------------------------

/// What a walk to a toilet or a resource does first, with a floor plan and a facility without
/// independent queues: the walk in the room to the facility, or its rejection.
pub enum Approach {
    /// No walk in the room: the request follows.
    Request,
    /// No route, or none that ends within the Run.
    Reject,
    Travel(PlannedTrip),
}

/// The approach of `_after_arrival`. `has_geometry` is `"facility_geometry" in model_configs`;
/// `reserved` the sequences the caller's writer reserved.
#[allow(clippy::too_many_arguments)]
pub fn arrival_approach(
    world: &World,
    walking: Walking<'_>,
    section: &Kept<Movement>,
    queues: &Held,
    has_geometry: bool,
    reserved: usize,
    agent_id: &Arc<str>,
    purpose: &str,
    facility_id: &Arc<str>,
    destination: &Arc<str>,
) -> Result<Approach> {
    let independent = facility_queues::queues(world, queues)?.is_some_and(|row| facility_queues::has_independent(&row, facility_id));
    if !has_geometry || independent {
        return Ok(Approach::Request);
    }
    let Some((route, points)) = travel::approach_route(world, Walking { movement: walking.movement, geo: walking.geo }, section, agent_id, facility_id)? else {
        return Ok(Approach::Reject);
    };
    if route.minutes <= 0 {
        return Ok(Approach::Request);
    }
    let events = Events::of(world)?;
    events.reserve(reserved);
    let minute = world.minute_index()?;
    match travel::plan_room_approach(world, &events, walking, agent_id, facility_id, purpose, route, &points, minute, destination)? {
        Some(trip) => Ok(Approach::Travel(trip)),
        None => Ok(Approach::Reject),
    }
}

/// The action of a resource request at a facility of this kind.
fn action_of(kind: Option<&str>) -> &'static str {
    if kind.and_then(service::resource_of) == Some("water_ml") {
        "fetch_water"
    } else {
        "collect_meal"
    }
}

/// `_reject_room_approach`: for a resource request with a resource service (`resource`), the
/// request's outcome rejected ("room_route_blocked") from the origin zone when a str, else the
/// person's zone, and counted; always counted as `facility_approach_rejected_count`.
pub fn reject_room(world: &World, service: &Held, metric_state_held: bool, resource: bool, agent_id: &str, facility_id: &str, origin: Option<&Arc<str>>) -> Result<()> {
    kit::check_metric_state(world, metric_state_held)?;
    if resource {
        let (kind, zone) = {
            let rows = world.rows()?;
            let Some(agent) = rows.agents.get(agent_id) else {
                return refuse("no such person");
            };
            (rows.facilities.get(facility_id).map(|facility| facility.kind.clone()), agent.zone_id.clone())
        };
        let record = RequestRecord {
            minute: world.minute_index()?,
            agent_id: Arc::from(agent_id),
            action: Arc::from(action_of(kind.as_deref())),
            outcome: Arc::from("rejected"),
            reason: Arc::from("room_route_blocked"),
            facility_id: Some(Arc::from(facility_id)),
            origin_zone_id: origin.cloned().unwrap_or(zone),
            detail: None,
        };
        service::append_outcome(world, service, record)?;
        kit::add_metric(world, "resource_service_request_rejected_count")?;
    }
    kit::add_metric(world, "facility_approach_rejected_count")
}

/// What a resource request turned down records: the arrival stands, and the outcome says why the
/// person is not in the queue (`request_rejected`, the text as its detail), counted.
pub fn rejected_request(world: &World, service: &Held, agent_id: &str, facility_id: &str, origin: &Arc<str>, minute: i64, text: &str) -> Result<()> {
    let kind = world.rows()?.facilities.get(facility_id).map(|facility| facility.kind.clone());
    let Some(kind) = kind else {
        return refuse("no such facility");
    };
    let record = RequestRecord {
        minute,
        agent_id: Arc::from(agent_id),
        action: Arc::from(action_of(Some(&kind))),
        outcome: Arc::from("rejected"),
        reason: Arc::from("request_rejected"),
        facility_id: Some(Arc::from(facility_id)),
        origin_zone_id: origin.clone(),
        detail: Some(Arc::from(text)),
    };
    service::append_outcome(world, service, record)?;
    kit::add_metric(world, "resource_service_request_rejected_count")
}

/// What the resource branch of `_after_arrival` checks before the request, which may write the
/// task counter and then turn the person down: the service's row and the metric state.
pub fn check_resource_request(world: &World, service: &Held, metric_state_held: bool) -> Result<()> {
    if let Held::Other(reason) = service {
        return Err(Error::unsupported(*reason));
    }
    world.row::<crate::state::resource_service::ServiceRow>(crate::state::resource_service::NAME)?;
    kit::check_metric_state(world, metric_state_held)
}

// ---- What a walk reads, and `_after_arrival`, through the host (lane P9-1a) -----------------------

/// The refusals of the namespaces the world holds other values of.
pub const OTHER_MOVEMENT: &str = "the world holds another movement state";
pub const OTHER_QUEUES: &str = "the world holds another queue state";
pub const OTHER_OVERLAY: &str = "the world holds another overlay state";
pub const OTHER_SERVICE: &str = "the world holds another service state";
/// The refusal of a resource walk whose origin is not a str.
pub const NO_ORIGIN: &str = "a resource walk's origin is not a str";

/// What a walk reads of the world's outside, read through the host (lane P9-1a): whether
/// `model_state` holds the namespaces (H1), and for a queue walk the floor plan
/// (`Host::floor_plan`) and, with a layout, the movement section (H2). The lifecycle's inputs of a
/// relocation are the caller's.
pub struct Gathered {
    pub movement: Held,
    pub queues: Held,
    pub overlay: Held,
    pub geo: Option<Arc<Geo>>,
    pub section: Option<Arc<Movement>>,
}

impl Gathered {
    pub fn read<C: WorldCell, H: Host>(cell: &C, host: &mut H, row: &EventRow) -> std::result::Result<Self, H::Error> {
        let purpose = match kit::payload(row, "purpose") {
            Some(V::Str(purpose)) => purpose.clone(),
            _ => Arc::from(""),
        };
        let holding = kit::holding(cell, host)?;
        let (geo, section) = if is_queue_walk(&purpose) {
            let geo = host.floor_plan()?;
            let section = if geo.layout.is_some() {
                // A refusal of the section is said here, as the PyO3 crate's `kept_value` said it.
                let section = kit::kept_section_value(cell, host, &holding, SectionId::Movement, |configs| &configs.movement, Movement::parse)?;
                kit::kept(&section)?;
                section.ok()
            } else {
                None
            };
            (Some(geo), section)
        } else {
            (None, None)
        };
        Ok(Self {
            movement: holding.held(movement::KEY, OTHER_MOVEMENT),
            queues: holding.held(QUEUES, OTHER_QUEUES),
            overlay: holding.held(movement::OVERLAY_KEY, OTHER_OVERLAY),
            geo,
            section,
        })
    }

    /// The rule's inputs, with whether the handler has a resource service and the lifecycle's.
    pub fn inputs<'a>(&'a self, resource_service: bool, lifecycle: Option<&'a lifecycle::Inputs<'a>>) -> Inputs<'a> {
        Inputs {
            movement: self.movement.clone(),
            queues: self.queues.clone(),
            overlay: self.overlay.clone(),
            geo: self.geo.as_deref(),
            section: self.section.as_deref(),
            resource_service,
            lifecycle,
        }
    }
}

const QUEUES: &str = crate::state::facility_queues::NAME;

/// `"facility_geometry" in world.model_configs`, of the slot's row while the world holds the field,
/// else of the configs Python holds, which must be a plain dict (lanes P9-1c, P9-3a).
pub fn has_geometry<C: WorldCell, H: Host>(cell: &C, host: &mut H) -> std::result::Result<bool, H::Error> {
    let holding = kit::holding(cell, host)?;
    kit::read_configs(cell, host, &holding, |configs| configs.contains_key("facility_geometry"))?.ok_or_else(|| Error::unsupported(crate::host::TABLE_NOT_PLAIN).into())
}

/// A walk that reached a toilet or a resource, as `_after_arrival` takes it.
pub struct Arrival<'a> {
    pub agent_id: &'a str,
    pub purpose: &'a str,
    pub facility_id: &'a str,
    pub destination: &'a str,
    /// The event's origin when a str (a resource walk reads it).
    pub origin: Option<&'a str>,
    /// `"facility_geometry" in model_configs`.
    pub has_geometry: bool,
    /// Whether the handler has a resource service (`_reject_room_approach` reads it).
    pub resource_service: bool,
    /// `handler.validate_request_state`, truthy.
    pub validates: bool,
}

/// The requests a walk leads to, which the managers make: the toilet's queue and the resource
/// service's (the PyO3 crate's, which reads each manager's inputs).
pub trait Managers<H: Host> {
    /// Whether the toilet's manager (`toilet`) or the resource service has its own `request` (a
    /// test's), which validates the world itself as it is told.
    fn own(&mut self, toilet: bool) -> std::result::Result<bool, H::Error>;
    /// `toilet_service.request(agent_id, facility_id)`, without the world's validation unless the
    /// manager's own `request` does it.
    fn toilet(&mut self, host: &mut H, agent_id: &str, facility_id: &str) -> std::result::Result<(), H::Error>;
    /// `resource_service.request(agent_id, facility_id)`: None queued, or the text of the `ValueError`
    /// the queue turned the person down with, having written what it writes before that.
    fn resource(&mut self, host: &mut H, agent_id: &str, facility_id: &str) -> std::result::Result<Option<String>, H::Error>;
}

/// A rule's result past the host: a core error or a duplicate event is the write's.
pub type Arrived = crate::events::writer::WriteResult<()>;

/// `_reject_room_approach`, with whether the world holds the service and `metric_state` read
/// through the host. `resource`: a resource request with a resource service.
pub fn reject_room_through<C: WorldCell, H: Host>(
    cell: &C,
    host: &mut H,
    resource: bool,
    agent_id: &str,
    facility_id: &str,
    origin: Option<&str>,
) -> std::result::Result<(), H::Error> {
    let holding = kit::holding(cell, host)?;
    let held = holding.held(crate::state::resource_service::NAME, OTHER_SERVICE);
    let metrics = holding.field(crate::world::METRIC_STATE);
    let origin: Option<Arc<str>> = origin.map(Arc::from);
    Ok(cell.with(|world| reject_room(world, &held, metrics, resource, agent_id, facility_id, origin.as_ref()))??)
}

/// `_after_arrival` where the managers make their own requests (a manager with its own `request`, a
/// test's, is the host's: `Host::apply_foreign`). With a floor plan and a facility without
/// independent queues the person first walks in the room (`arrival_approach`), or the approach is
/// turned away (`_reject_room_approach`). Then a toilet walk asks the toilet's queue, with the
/// world's validation around it where the handler asks for it, and its `ValueError` is the event's
/// failure with its text; a resource walk asks the resource service's, and its refusal is recorded
/// (`rejected_request`). What the floor plan's read raised is raised where the approach reads it.
pub fn arrive<C: WorldCell, H: Host, M: Managers<H>>(
    cell: &C,
    host: &mut H,
    arrival: &Arrival<'_>,
    managers: &mut M,
) -> std::result::Result<Arrived, H::Error> {
    let is_toilet = arrival.purpose == "toilet_request";
    if !is_toilet && arrival.purpose != "resource_request" {
        return Ok(Ok(()));
    }
    if arrival.has_geometry {
        let holding = kit::holding(cell, host)?;
        let plan = host.floor_plan();
        let section = kit::kept_section_value(cell, host, &holding, SectionId::Movement, |configs| &configs.movement, Movement::parse)?;
        let moving = holding.held(movement::KEY, OTHER_MOVEMENT);
        let queues = holding.held(QUEUES, OTHER_QUEUES);
        let (agent, facility, destination): (Arc<str>, Arc<str>, Arc<str>) = (Arc::from(arrival.agent_id), Arc::from(arrival.facility_id), Arc::from(arrival.destination));
        let walking = Walking { movement: &moving, geo: plan.as_ref().ok() };
        let approach = cell.with(|world| arrival_approach(world, walking, &section, &queues, true, 0, &agent, arrival.purpose, &facility, &destination))?;
        let approach = match (approach, plan) {
            (Err(Error::Unsupported(reason)), Err(read)) if reason == crate::rules::overlay::PLAN_UNREAD => return Err(read),
            (Err(error), _) => return Ok(Err(error.into())),
            (Ok(approach), _) => approach,
        };
        match approach {
            Approach::Request => {}
            Approach::Reject => {
                let resource = arrival.purpose == "resource_request" && arrival.resource_service;
                return reject_room_through(cell, host, resource, arrival.agent_id, arrival.facility_id, arrival.origin).map(Ok);
            }
            Approach::Travel(trip) => {
                return Ok(cell.with(|world| -> Arrived {
                    let events = Events::of(world)?;
                    travel::write_travel(world, &events, trip)
                })?);
            }
        }
    }
    if is_toilet {
        // `except ValueError as exc: raise EventApplicationError(str(exc))`, also for the world's
        // validation around the request.
        let requested = (|| {
            let validates = arrival.validates && !managers.own(true)?;
            if validates {
                host.validate_world()?;
            }
            managers.toilet(host, arrival.agent_id, arrival.facility_id)?;
            if validates {
                host.validate_world()?;
            }
            Ok(())
        })();
        return match requested {
            Ok(()) => Ok(Ok(())),
            Err(error) if crate::host::HostFailure::class(&error) == crate::host::ErrorClass::Value => {
                Err(Error::failed(crate::host::HostFailure::text(&error)).into())
            }
            Err(error) => Err(error),
        };
    }
    // Read before the request, which may write the task counter and then turn the person down.
    let holding = kit::holding(cell, host)?;
    let held = holding.held(crate::state::resource_service::NAME, OTHER_SERVICE);
    let metrics = holding.field(crate::world::METRIC_STATE);
    cell.with(|world| check_resource_request(world, &held, metrics))??;
    let Some(text) = managers.resource(host, arrival.agent_id, arrival.facility_id)? else {
        return Ok(Ok(()));
    };
    // The arrival stands; the record says why the person is not in the queue, with `str(exc)`.
    let Some(origin) = arrival.origin else {
        return Err(Error::unsupported(NO_ORIGIN).into());
    };
    let origin: Arc<str> = Arc::from(origin);
    Ok(cell.with(|world| -> Arrived {
        let minute = world.minute_index()?;
        Ok(rejected_request(world, &held, arrival.agent_id, arrival.facility_id, &origin, minute, &text)?)
    })?)
}
