//! Where a person walks: `_route`, `_schedule_travel`, `_schedule_staff_support_travel`,
//! `_schedule_return_to_initial_zone`, the choice of a facility (`experiments/phases/propose.py`,
//! `policies/space.py`), the loads of the edges, and the routers of a Run (lane c6 of Issue #152).
//!
//! The rules are functions on the `World`. The route graph is the Run's (`Graph`), built from the
//! world's zone and edge tables at each `_refresh_space` (`routers_of`): rows are the truth (lanes
//! 6-1, P8-0c), and tables that a value did not fit (`zones_tainted`) are refused, with no graph of
//! Python to fall back on. Edge capacities count every trip under way (`active_edge_loads`), and a
//! route a full edge turns down records a capacity wait through the world's writer (`kit::Events`).
//! A trip writes its task, the person's activity and target, its completion event and the start of
//! its walk in the row of `movement_v1`.
//!
//! What only Python has comes as an input read by the PyO3 crate before the world is borrowed: the
//! Run's graph and capacities, whether `model_state` holds `movement_v1` and `staff_operations_v1`
//! (`kit::Held`), the movement section (`kit::Kept`), and the floor plan. A plan the PyO3 crate
//! could not read is None here, and a rule that reads it says `PLAN_UNREAD`, for the PyO3 crate to
//! raise what the read raised, at the point the rule read the plan before (as lane c4).
//!
//! What is refused is refused before anything is written: the Python rule's texts for a route from
//! elsewhere and a support trip without its IDs, and Unsupported (a `KernelRefusal` in Python) for an
//! input outside the rule's contract (docs/rust-port/lanes/m4-service.md).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock};

use indexmap::IndexMap;

use crate::cell::ShardRef;
use crate::config::movement::Movement;
use crate::entities::{Activity, Entities, FacilityRow, TaskRow};
use crate::error::{Error, Result};
use crate::events::writer::WriteResult;
use crate::events::{EventKind, EventRow, EventStatus};
use crate::geometry::{Geo, Pt};
use crate::routes::Router;
use crate::row::ValueRow;
use crate::rules::agent_dynamics::knows_facility;
use crate::rules::kit::movement::{approach_point, config_read, Changes, EXACT_INT, KEY as MOVEMENT};
use crate::rules::kit::{self, held_row, is_toilet, mobility_text, new_event, set_agent, Events, Held, Kept, NewEvent};
use crate::rules::movement::{self, no_positions, Start};
use crate::rules::overlay::PLAN_UNREAD;
use crate::run::RouteCapacities;
use crate::state::movement::{Journey, MovementRow, Point};
use crate::state::staff_operations::{Phase, StaffOperationsRow, NAME as STAFF_OPERATIONS};
use crate::value::V;
use crate::world::{World, MODEL_CONFIGS};

pub use crate::routes::{EdgeLoads, Route};

const ROUTING_SOURCE: &str = "ASM-ZONE-ROUTING-V1";
const ROUTING_POLICY: &str = "shortest-travel-time-v1";
const CAPACITY_SOURCE: &str = "ASM-A05-ROUTE-EDIT-V1";
const CAPACITY_DETAIL: &str = "route edge capacity is full; the Agent remains at its origin";
const APPROACH_SOURCE: &str = "ASM-SPATIAL-MOVEMENT-V2";
const APPROACH_POLICY: &str = "spatial-route-and-room-waypoint-v2";
const APPROACH_EVENT_SOURCE: &str = "agent-facility-approach-v2";
/// The refusal of tables a value did not fit: the zones and the edges dropped their rows.
pub const ZONES_TAINTED: &str = "the zones are tainted";

#[track_caller]
fn refuse<T>(reason: &'static str) -> Result<T> {
    Err(Error::unsupported(reason))
}

/// The floor plan, or `PLAN_UNREAD` where the PyO3 crate could not read it.
#[track_caller]
pub(crate) fn plan(geo: Option<&Arc<Geo>>) -> Result<&Arc<Geo>> {
    geo.ok_or_else(|| Error::unsupported(PLAN_UNREAD))
}

/// `exact_dict(value)` of a value of the configs.
#[track_caller]
fn map(value: &V) -> Result<&IndexMap<Arc<str>, V>> {
    match value {
        V::Map(map) => Ok(map),
        _ => refuse("a value is not a plain dict"),
    }
}

/// `world.model_configs`, from its row: Unsupported where it is not a dict.
fn configs_of(world: &World) -> Result<ShardRef<'_, ValueRow>> {
    let configs = world.row::<ValueRow>(MODEL_CONFIGS)?;
    map(&configs.0)?;
    Ok(configs)
}

/// The zones and the zone edges, which a rule reads as `world.zones` and `world.zone_edges`.
fn zone_rows(world: &World) -> Result<ShardRef<'_, Entities>> {
    let rows = world.entities.try_borrow()?;
    if rows.zones_tainted().is_some() {
        return refuse(ZONES_TAINTED);
    }
    Ok(rows)
}

// ---- The routers of a Run -------------------------------------------------------------------------

/// The people's graph of a controller's Run, and the capacities its trips are counted against.
#[derive(Clone)]
pub struct Graph {
    pub router: Arc<Router>,
    /// `controller.route_capacities`, as the controller read them when it started.
    pub capacities: Arc<RouteCapacities>,
}

/// `_refresh_space` for the kernels: the people's and the staff's routers, from the world's tables
/// now. What `ZoneGraph.from_states(world.zones, world.zone_edges, edge_capacities=capacities)`
/// turns down is its ValueError. With a floor plan (`"facility_geometry" in model_configs` and an
/// edge), the staff's graph keeps the edges the plan finds clear for an independent walker. The
/// staff's router is a result of its own, so that the people's is kept first, as before. None
/// where the tables dropped their rows.
#[allow(clippy::type_complexity)]
pub fn routers_of(world: &World, capacities: &RouteCapacities, geo: Option<&Arc<Geo>>) -> Result<Option<(Arc<Router>, Result<Arc<Router>>)>> {
    let (people, edges) = {
        let rows = world.entities.try_borrow()?;
        if rows.zones_tainted().is_some() {
            return Ok(None);
        }
        (Arc::new(Router::of_tables(&rows.zones, &rows.zone_edges, capacities)?), rows.zone_edges.len())
    };
    let staff = (|| {
        if edges > 0 && map(&configs_of(world)?.0)?.contains_key("facility_geometry") {
            let geo = plan(geo)?;
            Ok(Arc::new(people.keeping(|edge| geo.route_is_clear(&edge.id, "independent"))))
        } else {
            Ok(people.clone())
        }
    })();
    Ok(Some((people, staff)))
}

/// `SpaceRouter(ZoneGraph.from_states(zones, edges)).route(...)` with nothing closed, for a rule
/// that has no controller: what `ZoneGraph` turns down raises its ValueError. `edge_capacities` are
/// left out, which read every trip under way.
pub fn world_route(world: &World, origin: &Arc<str>, destination: &Arc<str>) -> Result<Option<Route>> {
    let graph = {
        let rows = zone_rows(world)?;
        Router::of_tables(&rows.zones, &rows.zone_edges, &RouteCapacities::default())?
    };
    graph.shortest(&HashSet::new(), None, origin, destination)
}

/// `layout_routing.max_agents_in_transit_by_edge` of `model_configs`, each checked as `ZoneGraph`
/// checks it: an int from 1 to 100 of an edge of the world.
fn world_capacities(world: &World, has_edge: impl Fn(&str) -> bool) -> Result<HashMap<Arc<str>, i64>> {
    let configs = configs_of(world)?;
    let mut capacities = HashMap::new();
    if let Some(routing) = map(&configs.0)?.get("layout_routing") {
        if let Some(table) = map(routing)?.get("max_agents_in_transit_by_edge") {
            for (edge, capacity) in map(table)? {
                let capacity = kit::exact_int(capacity)?;
                if !has_edge(edge) || !(1..=100).contains(&capacity) {
                    return refuse("a capacity the zone graph turns down");
                }
                capacities.insert(edge.clone(), capacity);
            }
        }
    }
    Ok(capacities)
}

/// `ZoneGraph.from_states(world.zones, world.zone_edges, edge_capacities=capacities)` as a router,
/// for the routes `_support_route_wait_reasons` reads, from the world's tables. What the constructor
/// would turn down for a capacity or an edge is refused.
pub fn world_graph(world: &World) -> Result<Router> {
    let keys: HashSet<Arc<str>> = zone_rows(world)?.zone_edges.iter().map(|(key, _)| key.clone()).collect();
    let capacities = world_capacities(world, |edge| keys.contains(edge))?;
    let rows = zone_rows(world)?;
    Router::of_world_tables(&rows.zones, &rows.zone_edges, &capacities)
}

// ---- Who is on an edge --------------------------------------------------------------------------

/// `event.payload.get("route_edge_ids", ())`: the IDs that are str.
fn trip_edges(row: &EventRow) -> Result<Vec<Arc<str>>> {
    let V::Map(payload) = &row.payload else {
        return refuse("a payload is not a map");
    };
    match payload.get("route_edge_ids") {
        None => Ok(Vec::new()),
        Some(V::List(items)) => Ok(items.iter().filter_map(|item| if let V::Str(id) = item { Some(id.clone()) } else { None }).collect()),
        Some(_) => refuse("a trip's edges are not a list"),
    }
}

/// `event.payload.get(name)` when it is a str.
fn payload_text(row: &EventRow, name: &str) -> Option<Arc<str>> {
    match kit::payload(row, name) {
        Some(V::Str(text)) => Some(text.clone()),
        _ => None,
    }
}

/// The pending events in queue order, each with its due minute. Unsupported where one has no row:
/// the PyO3 crate reads the rows of the events Python scheduled before it calls.
fn pending_rows(world: &World) -> Result<Vec<(i64, Arc<EventRow>)>> {
    let queue = world.queue()?;
    let mut rows = Vec::with_capacity(queue.ordered().len());
    for (key, serial) in queue.ordered() {
        let Some(row) = &queue.slot(*serial).row else {
            return refuse("an event in the queue is not a plain one");
        };
        rows.push((key.0, row.clone()));
    }
    Ok(rows)
}

/// The row of `staff_operations_v1` where the world holds it.
fn staff_operations<'w>(world: &'w World, staff: &Held) -> Result<Option<ShardRef<'w, StaffOperationsRow>>> {
    held_row::<StaffOperationsRow>(world, STAFF_OPERATIONS, staff)
}

/// `_active_route_edge_loads` and the loads of `_support_route_wait_reasons`: every edge of each
/// trip that ends after this minute, and of each staff member on the way. `staff` says whether the
/// world holds `staff_operations_v1`.
pub fn active_edge_loads(world: &World, staff: &Held, minute: i64) -> Result<EdgeLoads> {
    let mut loads = EdgeLoads::new();
    for (due, row) in pending_rows(world)? {
        if due <= minute || &*row.event_type != "agent_travel_complete" {
            continue;
        }
        for edge in trip_edges(&row)? {
            *loads.entry(edge).or_insert(0) += 1;
        }
    }
    if let Some(row) = staff_operations(world, staff)? {
        for record in row.staff.values() {
            if record.phase == Phase::Travel {
                for edge in &record.route_edge_ids {
                    *loads.entry(edge.clone()).or_insert(0) += 1;
                }
            }
        }
    }
    Ok(loads)
}

/// `_current_journey_edge`, of a journey with these edges and times.
fn current_edge(rows: &Entities, ids: &[Arc<str>], start: i64, due: i64, minute: i64) -> Result<Option<Arc<str>>> {
    if ids.is_empty() {
        return Ok(None);
    }
    let duration = due - start;
    if duration <= 0 {
        return Ok(None);
    }
    if rows.zones_tainted().is_some() {
        return refuse(ZONES_TAINTED);
    }
    let elapsed = minute - start;
    let mut weights = Vec::with_capacity(ids.len());
    for id in ids {
        let Some(edge) = rows.zone_edges.get(id) else {
            return refuse("a trip's edge is not in the world");
        };
        weights.push(edge.travel_minutes);
    }
    let mut total = 0_i64;
    for weight in &weights {
        total = total.checked_add(*weight).ok_or_else(|| Error::unsupported("a trip's minutes past 64 bits"))?;
    }
    if total <= 0 {
        return Ok(Some(ids[0].clone()));
    }
    if [elapsed.abs(), duration, total].iter().any(|value| *value >= EXACT_INT) {
        return refuse("a trip's minutes do not fit a float");
    }
    // `elapsed / duration * total`, from the left: an int's true division is the floats' here.
    let scaled = elapsed as f64 / duration as f64 * total as f64;
    let mut cumulative = 0_i64;
    for (id, weight) in ids.iter().zip(&weights) {
        if scaled < (cumulative + weight) as f64 {
            return Ok(Some(id.clone()));
        }
        cumulative += weight;
    }
    Ok(ids.last().cloned())
}

/// `shared_passage_edge_loads`: who is on which edge at this minute. `movement` and `staff` say
/// whether the world holds `movement_v1` and `staff_operations_v1`.
pub fn passage_edge_loads(world: &World, movement: &Held, staff: &Held, minute: i64) -> Result<EdgeLoads> {
    let rows = world.entities.try_borrow()?;
    let mut loads = EdgeLoads::new();
    let mut represented: HashSet<Arc<str>> = HashSet::new();
    if let Some(row) = held_row::<MovementRow>(world, MOVEMENT, movement)? {
        for (agent, journey) in row.journeys.iter() {
            if journey.started_minute <= minute && minute < journey.due_minute {
                if let Some(edge) = current_edge(&rows, &journey.route_edge_ids, journey.started_minute, journey.due_minute, minute)? {
                    *loads.entry(edge).or_insert(0) += 1;
                    represented.insert(agent.clone());
                }
            }
        }
    }
    for (_, event) in pending_rows(world)? {
        if &*event.event_type != "agent_travel_complete" {
            continue;
        }
        if payload_text(&event, "agent_id").is_some_and(|agent| represented.contains(&agent)) {
            continue;
        }
        if payload_text(&event, "origin_zone_id").is_none_or(|zone| zone.is_empty()) {
            continue;
        }
        let ids = trip_edges(&event)?;
        if ids.is_empty() {
            continue;
        }
        let Some(task_id) = payload_text(&event, "task_id") else {
            continue;
        };
        if rows.tainted().is_some() {
            return refuse("the entity store is tainted");
        }
        let Some((start, due)) = rows.active_tasks.get(&task_id).map(|task| (task.start_minute, task.due_minute)) else {
            continue;
        };
        if start <= minute && minute < due {
            if let Some(edge) = current_edge(&rows, &ids, start, due, minute)? {
                *loads.entry(edge).or_insert(0) += 1;
            }
        }
    }
    if let Some(row) = staff_operations(world, staff)? {
        for record in row.staff.values() {
            if record.phase == Phase::Travel {
                let (Some(start), Some(due)) = (record.started_minute, record.travel_due_minute) else {
                    return refuse("a trip of the staff has no times");
                };
                if start <= minute && minute < due {
                    if let Some(edge) = current_edge(&rows, &record.route_edge_ids, start, due, minute)? {
                        *loads.entry(edge).or_insert(0) += 1;
                    }
                }
            }
        }
    }
    Ok(loads)
}

// ---- _route ---------------------------------------------------------------------------------------

/// What a route reads beside the world.
#[derive(Clone, Copy)]
pub struct Routing<'a> {
    pub graph: &'a Graph,
    /// The floor plan; None where the PyO3 crate could not read it.
    pub geo: Option<&'a Arc<Geo>>,
    /// The movement section of the configs (`config::movement`).
    pub movement: &'a Kept<Movement>,
    /// Whether the world holds `staff_operations_v1`, whose trips load the edges.
    pub staff: &'a Held,
}

/// `_record_route_capacity_wait`, not done yet: a rule plans its route before it writes anything.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapacityWait {
    pub agent_id: Arc<str>,
    pub origin: Arc<str>,
    pub destination: Arc<str>,
    pub edge_id: Arc<str>,
    pub capacity: i64,
    pub active: i64,
}

/// `_record_route_capacity_wait`, of a sequence reserved before (`Events::reserve`): the wait is a
/// SUPPRESSED record of its own event.
pub fn write_capacity_wait(world: &World, events: &Events<'_>, wait: CapacityWait) -> WriteResult<()> {
    let minute = world.minute_index()?;
    let sequence = events.allocate_reserved()?;
    let mut payload = IndexMap::with_capacity(6);
    payload.insert(Arc::from("agent_id"), V::Str(wait.agent_id));
    payload.insert(Arc::from("origin_zone_id"), V::Str(wait.origin));
    payload.insert(Arc::from("destination_zone_id"), V::Str(wait.destination));
    payload.insert(Arc::from("edge_id"), V::Str(wait.edge_id));
    payload.insert(Arc::from("capacity"), V::Int(wait.capacity));
    payload.insert(Arc::from("active_travelers"), V::Int(wait.active));
    let mut provenance = IndexMap::with_capacity(2);
    provenance.insert(Arc::from("source_type"), V::Str(Arc::from("assumption")));
    provenance.insert(Arc::from("source_id"), V::Str(Arc::from(CAPACITY_SOURCE)));
    let event = new_event(NewEvent {
        event_id: Arc::from(format!("zone-edge-capacity-wait-{sequence}")),
        kind: EventKind::Endogenous,
        due_minute: minute,
        priority: 0,
        sequence,
        event_type: Arc::from("zone_edge_capacity_wait"),
        payload,
        source_id: Some(Arc::from(CAPACITY_SOURCE)),
        causation_id: None,
        provenance,
    })?;
    events.record(&event, minute, EventStatus::Suppressed, Some(CAPACITY_DETAIL))
}

/// The minutes `route_with_movement_time` gives a route of these zones and edges.
fn timed(world: &World, routing: &Routing<'_>, agent_id: &str, route: &Route) -> Result<i64> {
    let movement = kit::kept(routing.movement)?;
    if !movement.times_routes() {
        return Ok(route.minutes);
    }
    let model = config_read(movement.model.clone())?;
    if route.minutes.abs() >= EXACT_INT {
        return refuse("the route's minutes are too large");
    }
    let geo = plan(routing.geo)?;
    let zones: Vec<&str> = route.zone_ids.iter().map(|id| &**id).collect();
    let edges: Vec<&str> = route.edge_ids.iter().map(|id| &**id).collect();
    movement::movement_minutes(world, geo, &model, agent_id, &zones, &edges, route.minutes)
}

/// `_route`, for a person in the world, changing nothing: the waits it would record go in `waits`.
pub fn route_for(
    world: &World,
    routing: &Routing<'_>,
    origin: &Arc<str>,
    destination: &Arc<str>,
    agent_id: &str,
    waits: &mut Vec<CapacityWait>,
) -> Result<Option<Route>> {
    let mobility = {
        let rows = world.rows()?;
        let Some(agent) = rows.agents.get(agent_id) else {
            return refuse("no such agent");
        };
        mobility_text(agent)
    };
    let has_plan = map(&configs_of(world)?.0)?.get("facility_geometry").is_some_and(|raw| !matches!(raw, V::Null));
    let router = &routing.graph.router;
    let closed = if has_plan { router.closed_to(plan(routing.geo)?, mobility)? } else { Arc::new(HashSet::new()) };
    let capacities = &routing.graph.capacities;
    let found = if !capacities.any {
        router.shortest(&closed, None, origin, destination)?
    } else {
        let minute = world.minute_index()?;
        let loads = active_edge_loads(world, routing.staff, minute)?;
        let found = router.shortest(&closed, Some(&loads), origin, destination)?;
        if found.is_none() {
            if let Some(free) = router.shortest(&closed, None, origin, destination)? {
                for edge in &free.edge_ids {
                    let Some(capacity) = capacities.of(edge)? else {
                        continue;
                    };
                    let active = loads.get(edge).copied().unwrap_or(0);
                    if active >= capacity {
                        waits.push(CapacityWait {
                            agent_id: Arc::from(agent_id),
                            origin: origin.clone(),
                            destination: destination.clone(),
                            edge_id: edge.clone(),
                            capacity,
                            active,
                        });
                        break;
                    }
                }
            }
        }
        found
    };
    let Some(mut route) = found else {
        return Ok(None);
    };
    route.minutes = timed(world, routing, agent_id, &route)?;
    Ok(Some(route))
}

/// `_route`: the route, after the capacity waits it records.
pub fn route(world: &World, routing: &Routing<'_>, origin: &Arc<str>, destination: &Arc<str>, agent_id: &str) -> WriteResult<Option<Route>> {
    let mut waits = Vec::new();
    let found = route_for(world, routing, origin, destination, agent_id, &mut waits)?;
    if !waits.is_empty() {
        let events = Events::of(world)?;
        events.reserve(waits.len());
        for wait in waits {
            write_capacity_wait(world, &events, wait)?;
        }
    }
    Ok(found)
}

// ---- _schedule_travel ---------------------------------------------------------------------------

/// What a trip's start reads beside the world: whether the world holds `movement_v1`, and the floor
/// plan (None where the PyO3 crate could not read it).
#[derive(Clone, Copy)]
pub struct Walking<'a> {
    pub movement: &'a Held,
    pub geo: Option<&'a Arc<Geo>>,
}

/// What a trip needs beside its route.
pub struct Trip<'a> {
    pub agent_id: Arc<str>,
    pub route: Route,
    pub points: Vec<Pt>,
    pub purpose: &'a str,
    pub facility_id: Option<Arc<str>>,
    pub minute: i64,
    pub causation_id: Option<Arc<str>>,
    pub support_request_id: Option<Arc<str>>,
    pub support_staff_id: Option<Arc<str>>,
}

/// A trip planned, with nothing changed: its writes cannot be refused.
pub struct PlannedTrip {
    agent_id: Arc<str>,
    task_id: Arc<str>,
    task_kind: &'static str,
    target_id: Arc<str>,
    minute: i64,
    due: i64,
    event: EventRow,
    changes: Option<Changes>,
}

impl PlannedTrip {
    /// The completion event the trip schedules.
    pub fn event(&self) -> &EventRow {
        &self.event
    }
}

struct Names {
    agent_id: Arc<str>,
    task_id: Arc<str>,
    purpose: Arc<str>,
    destination_zone_id: Arc<str>,
    facility_id: Arc<str>,
    route_zone_ids: Arc<str>,
    route_edge_ids: Arc<str>,
    travel_minutes: Arc<str>,
    origin_zone_id: Arc<str>,
    request_id: Arc<str>,
    staff_id: Arc<str>,
    source_type: Arc<str>,
    source_id: Arc<str>,
    policy_version: Arc<str>,
    assumption: Arc<str>,
    routing_policy: Arc<str>,
    event_type: Arc<str>,
}

static NAMES: LazyLock<Names> = LazyLock::new(|| Names {
    agent_id: Arc::from("agent_id"),
    task_id: Arc::from("task_id"),
    purpose: Arc::from("purpose"),
    destination_zone_id: Arc::from("destination_zone_id"),
    facility_id: Arc::from("facility_id"),
    route_zone_ids: Arc::from("route_zone_ids"),
    route_edge_ids: Arc::from("route_edge_ids"),
    travel_minutes: Arc::from("travel_minutes"),
    origin_zone_id: Arc::from("origin_zone_id"),
    request_id: Arc::from("request_id"),
    staff_id: Arc::from("staff_id"),
    source_type: Arc::from("source_type"),
    source_id: Arc::from("source_id"),
    policy_version: Arc::from("policy_version"),
    assumption: Arc::from("assumption"),
    routing_policy: Arc::from(ROUTING_POLICY),
    event_type: Arc::from("agent_travel_complete"),
});

fn strings(ids: &[Arc<str>]) -> V {
    V::List(ids.iter().map(|id| V::Str(id.clone())).collect())
}

/// The start of the walk in the row of `movement_v1`, where `model_state` has it.
#[allow(clippy::too_many_arguments)]
fn plan_walk(
    world: &World,
    walking: Walking<'_>,
    agent_id: &Arc<str>,
    journey: Journey,
    origin: &Arc<str>,
    points: &[Pt],
    edge_ids: &[Arc<str>],
    minute: i64,
    minutes: i64,
) -> Result<Option<Changes>> {
    match walking.movement {
        Held::Absent => Ok(None),
        Held::Other(reason) => Err(Error::unsupported(*reason)),
        Held::Held => {
            let geo = plan(walking.geo)?;
            let start = Start { agent_id, journey, origin, points, edge_ids, started: minute, duration: minutes };
            movement::plan_start_in(world, geo, start).map(Some)
        }
    }
}

/// `layout_routing.source_id` of the configs, or the routing's own source.
fn routing_source(world: &World) -> Result<Arc<str>> {
    let configs = configs_of(world)?;
    Ok(match map(&configs.0)?.get("layout_routing") {
        Some(config) => match map(config)?.get("source_id") {
            Some(source) => kit::exact_str(source)?.clone(),
            None => Arc::from(ROUTING_SOURCE),
        },
        None => Arc::from(ROUTING_SOURCE),
    })
}

/// `_schedule_travel`, changing nothing. None is its False. Its errors, and its refusals, come before
/// anything is written: the Python rule's texts for a route from elsewhere and a support trip
/// without its IDs (the Python rule allocated a sequence before the second).
pub fn plan_travel(world: &World, events: &Events<'_>, walking: Walking<'_>, trip: Trip<'_>) -> Result<Option<PlannedTrip>> {
    let Trip { agent_id, route, points, purpose, facility_id, minute, causation_id, support_request_id, support_staff_id } = trip;
    let is_origin = {
        let rows = world.rows()?;
        let Some(agent) = rows.agents.get(&agent_id) else {
            return refuse("no such agent");
        };
        agent.zone_id == route.origin
    };
    if !is_origin {
        return Err(Error::value_error("travel route origin does not match Agent zone"));
    }
    if route.minutes <= 0 {
        return Ok(None);
    }
    if route.minutes >= EXACT_INT {
        return refuse("a journey has no plain duration");
    }
    let Some(due) = minute.checked_add(route.minutes) else {
        return refuse("a due minute out of range");
    };
    if due >= events.duration() {
        return Ok(None);
    }
    let capacity = {
        let rows = zone_rows(world)?;
        let Some(zone) = rows.zones.get(&route.destination) else {
            return refuse("no such zone");
        };
        zone.capacity
    };
    if let (Some(capacity), true) = (capacity, route.origin != route.destination) {
        let rows = world.rows()?;
        let mut occupancy = 0;
        let mut incoming = 0;
        for (_, other) in rows.agents.iter() {
            occupancy += i64::from(other.zone_id == route.destination);
            if other.activity == Activity::Moving {
                if let Some(target) = &other.target_id {
                    let place = rows.facilities.get(target).map_or(target, |facility| &facility.zone_id);
                    incoming += i64::from(*place == route.destination);
                }
            }
        }
        if occupancy + incoming >= capacity {
            return Ok(None);
        }
    }

    let sequence = events.next_sequence()?;
    let event_id: Arc<str> = Arc::from(format!("agent-travel-complete-{sequence}"));
    let task_id: Arc<str> = Arc::from(format!("agent-travel-{sequence}"));
    {
        let task_is_taken = world.rows()?.active_tasks.get(&task_id).is_some();
        if events.is_taken(&event_id, Some(sequence))? || task_is_taken {
            return refuse("the trip's identity is taken");
        }
    }
    let is_facility_trip = matches!(purpose, "toilet_request" | "resource_request");
    let target_id = if is_facility_trip {
        match &facility_id {
            Some(id) => id.clone(),
            None => return refuse("a facility's trip with no facility"),
        }
    } else {
        route.destination.clone()
    };
    let task_kind = match purpose {
        "toilet_request" => "travel_to_toilet",
        "resource_request" => "travel_to_resource",
        "staff_support_travel" => "staff_support_travel",
        _ => "travel_return",
    };
    let names = &*NAMES;
    let mut payload = IndexMap::with_capacity(11);
    payload.insert(names.agent_id.clone(), V::Str(agent_id.clone()));
    payload.insert(names.task_id.clone(), V::Str(task_id.clone()));
    let purpose_arc: Arc<str> = Arc::from(purpose);
    payload.insert(names.purpose.clone(), V::Str(purpose_arc.clone()));
    payload.insert(names.destination_zone_id.clone(), V::Str(route.destination.clone()));
    payload.insert(names.facility_id.clone(), facility_id.as_ref().map_or(V::Null, |id| V::Str(id.clone())));
    payload.insert(names.route_zone_ids.clone(), strings(&route.zone_ids));
    payload.insert(names.route_edge_ids.clone(), strings(&route.edge_ids));
    payload.insert(names.travel_minutes.clone(), V::Int(route.minutes));
    if purpose == "resource_request" {
        payload.insert(names.origin_zone_id.clone(), V::Str(route.origin.clone()));
    }
    if purpose == "staff_support_travel" {
        let (Some(request), Some(staff)) = (&support_request_id, &support_staff_id) else {
            return Err(Error::value_error("staff support travel requires request and staff IDs"));
        };
        if request.is_empty() || staff.is_empty() {
            return Err(Error::value_error("staff support travel requires request and staff IDs"));
        }
        payload.insert(names.request_id.clone(), V::Str(request.clone()));
        payload.insert(names.staff_id.clone(), V::Str(staff.clone()));
    }
    let source = routing_source(world)?;
    let mut provenance = IndexMap::with_capacity(3);
    provenance.insert(names.source_type.clone(), V::Str(names.assumption.clone()));
    provenance.insert(names.source_id.clone(), V::Str(source));
    provenance.insert(names.policy_version.clone(), V::Str(names.routing_policy.clone()));
    let Some(event) = EventRow::new(
        event_id.clone(),
        EventKind::Completion,
        due,
        0,
        sequence,
        names.event_type.clone(),
        payload,
        Some(task_id.clone()),
        causation_id,
        provenance,
    ) else {
        return refuse("the event's values are not what an event takes");
    };
    let journey = Journey {
        event_id,
        task_id: task_id.clone(),
        purpose: purpose_arc,
        facility_id,
        destination_zone_id: route.destination.clone(),
        route_zone_ids: route.zone_ids,
        route_edge_ids: route.edge_ids.clone(),
        route_points: points.iter().map(|point| Point { x: point.x, y: point.y }).collect(),
        started_minute: minute,
        due_minute: due,
        queue_id: None,
        support_request_id,
        support_staff_id,
    };
    let changes = plan_walk(world, walking, &agent_id, journey, &route.origin, &points, &route.edge_ids, minute, route.minutes)?;
    Ok(Some(PlannedTrip { agent_id, task_id, task_kind, target_id, minute, due, event, changes }))
}

/// `_schedule_room_approach` of `events/handlers.py`, changing nothing. None is its False.
#[allow(clippy::too_many_arguments)]
pub fn plan_room_approach(
    world: &World,
    events: &Events<'_>,
    walking: Walking<'_>,
    agent_id: &Arc<str>,
    facility_id: &Arc<str>,
    purpose: &str,
    route: Route,
    points: &[Pt],
    minute: i64,
    origin_zone_id: &Arc<str>,
) -> Result<Option<PlannedTrip>> {
    if route.minutes >= EXACT_INT {
        return refuse("a journey has no plain duration");
    }
    let Some(due) = minute.checked_add(route.minutes) else {
        return refuse("a due minute out of range");
    };
    if due >= events.duration() {
        return Ok(None);
    }
    let sequence = events.next_sequence()?;
    let event_id: Arc<str> = Arc::from(format!("agent-facility-approach-complete-{sequence}"));
    let task_id: Arc<str> = Arc::from(format!("agent-facility-approach-{sequence}"));
    {
        let task_is_taken = world.rows()?.active_tasks.get(&task_id).is_some();
        if events.is_taken(&event_id, Some(sequence))? || task_is_taken {
            return refuse("the approach's identity is taken");
        }
    }
    let task_kind = if purpose == "toilet_request" { "travel_to_toilet" } else { "travel_to_resource" };
    let names = &*NAMES;
    let purpose_arc: Arc<str> = Arc::from(purpose);
    let mut payload = IndexMap::with_capacity(9);
    payload.insert(names.agent_id.clone(), V::Str(agent_id.clone()));
    payload.insert(names.task_id.clone(), V::Str(task_id.clone()));
    payload.insert(names.purpose.clone(), V::Str(purpose_arc.clone()));
    payload.insert(names.destination_zone_id.clone(), V::Str(route.destination.clone()));
    payload.insert(names.facility_id.clone(), V::Str(facility_id.clone()));
    payload.insert(names.origin_zone_id.clone(), V::Str(origin_zone_id.clone()));
    payload.insert(names.route_zone_ids.clone(), strings(&route.zone_ids));
    payload.insert(names.route_edge_ids.clone(), strings(&route.edge_ids));
    payload.insert(names.travel_minutes.clone(), V::Int(route.minutes));
    let mut provenance = IndexMap::with_capacity(3);
    provenance.insert(names.source_type.clone(), V::Str(names.assumption.clone()));
    provenance.insert(names.source_id.clone(), V::Str(Arc::from(APPROACH_SOURCE)));
    provenance.insert(names.policy_version.clone(), V::Str(Arc::from(APPROACH_POLICY)));
    let Some(event) = EventRow::new(
        event_id.clone(),
        EventKind::Completion,
        due,
        0,
        sequence,
        names.event_type.clone(),
        payload,
        Some(Arc::from(APPROACH_EVENT_SOURCE)),
        None,
        provenance,
    ) else {
        return refuse("the event's values are not what an event takes");
    };
    let journey = Journey {
        event_id,
        task_id: task_id.clone(),
        purpose: purpose_arc,
        facility_id: Some(facility_id.clone()),
        destination_zone_id: route.destination.clone(),
        route_zone_ids: route.zone_ids,
        route_edge_ids: route.edge_ids.clone(),
        route_points: points.iter().map(|point| Point { x: point.x, y: point.y }).collect(),
        started_minute: minute,
        due_minute: due,
        queue_id: None,
        support_request_id: None,
        support_staff_id: None,
    };
    let changes = plan_walk(world, walking, agent_id, journey, &route.origin, points, &route.edge_ids, minute, route.minutes)?;
    Ok(Some(PlannedTrip { agent_id: agent_id.clone(), task_id, task_kind, target_id: facility_id.clone(), minute, due, event, changes }))
}

/// The writes of `_schedule_travel` and `_schedule_room_approach`.
pub fn write_travel(world: &World, events: &Events<'_>, trip: PlannedTrip) -> WriteResult<()> {
    let PlannedTrip { agent_id, task_id, task_kind, target_id, minute, due, event, changes } = trip;
    events.allocate_sequence()?;
    {
        let mut rows = world.rows_mut()?;
        rows.active_tasks.insert(
            task_id.clone(),
            TaskRow {
                id: task_id.clone(),
                kind: Arc::from(task_kind),
                agent_id: Some(agent_id.clone()),
                resource_id: None,
                start_minute: minute,
                due_minute: due,
                staff_id: None,
            },
        );
        set_agent(&mut rows, &agent_id, Activity::Moving, Some(&target_id))?;
    }
    events.schedule(event)?;
    if let Some(changes) = changes {
        movement::write_changes(world, changes)?;
    }
    Ok(())
}

/// `_schedule_travel` and its writes: whether a trip was scheduled.
pub fn schedule_travel(world: &World, walking: Walking<'_>, trip: Trip<'_>) -> WriteResult<bool> {
    let events = Events::of(world)?;
    let Some(planned) = plan_travel(world, &events, walking, trip)? else {
        return Ok(false);
    };
    write_travel(world, &events, planned)?;
    Ok(true)
}

/// `_schedule_return_to_initial_zone` of `experiments/phases/dispatch.py`, after a completion event.
pub fn schedule_return(world: &World, routing: &Routing<'_>, walking: Walking<'_>, event: &EventRow) -> WriteResult<()> {
    let Some(agent_id) = payload_text(event, "agent_id") else {
        return Ok(());
    };
    let found = world.rows()?.agents.get(&agent_id).map(|agent| (agent.zone_id.clone(), agent.profile.initial_zone_id.clone()));
    let Some((zone_id, destination)) = found else {
        return Ok(());
    };
    if zone_id == destination {
        return Ok(());
    }
    let events = Events::of(world)?;
    let mut waits = Vec::new();
    let found = route_for(world, routing, &zone_id, &destination, &agent_id, &mut waits)?;
    events.reserve(waits.len());
    for wait in waits {
        write_capacity_wait(world, &events, wait)?;
    }
    let Some(route) = found else {
        return Ok(());
    };
    let minute = world.minute_index()?;
    let trip = Trip {
        agent_id,
        route,
        points: Vec::new(),
        purpose: "return",
        facility_id: None,
        minute,
        causation_id: Some(event.event_id.clone()),
        support_request_id: None,
        support_staff_id: None,
    };
    if let Some(planned) = plan_travel(world, &events, walking, trip)? {
        write_travel(world, &events, planned)?;
    }
    Ok(())
}

// ---- The choice of a facility -------------------------------------------------------------------

/// `_facility_queue_has_capacity`.
fn queue_has_capacity(configs: &V, rows: &Entities, facility_id: &str, facility: &FacilityRow, resource: bool) -> Result<bool> {
    let Some(config) = map(configs)?.get("facility_queues") else {
        return Ok(true);
    };
    let config = map(config)?;
    let Some(schema) = config.get("schema_version") else {
        return refuse("the queue config has no schema");
    };
    if kit::exact_int(schema)? != 1 {
        return refuse("another queue config");
    }
    let Some(capacities) = config.get("queue_capacity_by_facility") else {
        return Ok(true);
    };
    let Some(maximum) = map(capacities)?.get(facility_id) else {
        return Ok(true);
    };
    if matches!(maximum, V::Null) {
        return Ok(true);
    }
    let maximum = kit::exact_int(maximum)?;
    if maximum < 0 {
        return refuse("a negative queue capacity");
    }
    let (queued, active) = if resource {
        (&facility.resource_queue_agent_ids, &facility.resource_active_agent_ids)
    } else {
        (&facility.queue_agent_ids, &facility.active_agent_ids)
    };
    let transit = in_transit(rows, facility_id);
    Ok(queued.len() as i64 + active.len() as i64 + transit < maximum)
}

fn in_transit(rows: &Entities, facility_id: &str) -> i64 {
    rows.agents.iter().filter(|(_, other)| other.activity == Activity::Moving && other.target_id.as_deref() == Some(facility_id)).count() as i64
}

struct Candidate {
    id: Arc<str>,
    zone: Arc<str>,
    load: i64,
}

/// What `_select_resource_facility` found: `(facility, route, rejection)`.
#[derive(Debug, PartialEq)]
pub enum Selected {
    NoFacility,
    NoRoute(Arc<str>),
    Found(Arc<str>, Route),
}

/// `_select_toilet` (no `resource_kind`) and `_select_resource_facility`, which differ in which
/// facilities they take, changing nothing: the capacity waits the routes would record go in
/// `waits`, whose sequences the caller reserves before the trip it plans. `knows` is the section
/// `knowledge_is_limited` that `agent_knows_facility` reads.
pub fn select(world: &World, routing: &Routing<'_>, knows: &Kept<bool>, agent_id: &str, resource_kind: Option<&str>, waits: &mut Vec<CapacityWait>) -> Result<Selected> {
    let resource = resource_kind.is_some();
    let (agent_zone, listed) = {
        let rows = world.rows()?;
        let Some(agent) = rows.agents.get(agent_id) else {
            return refuse("no such agent");
        };
        let mut listed = Vec::new();
        let mut configs: Option<ShardRef<'_, ValueRow>> = None;
        for (id, facility) in rows.facilities.iter() {
            let takes = match resource_kind {
                Some(kind) => &*facility.kind == kind,
                // `"toilet" in kind.casefold()`.
                None => is_toilet(&facility.kind),
            };
            if !takes {
                continue;
            }
            if configs.is_none() {
                configs = Some(configs_of(world)?);
            }
            let Some(configs) = configs.as_ref() else {
                continue;
            };
            if queue_has_capacity(&configs.0, &rows, id, facility, resource)? {
                let load = if resource {
                    facility.resource_queue_agent_ids.len() + facility.resource_active_agent_ids.len()
                } else {
                    facility.queue_agent_ids.len() + facility.active_agent_ids.len()
                } as i64
                    + in_transit(&rows, id);
                listed.push(Candidate { id: id.clone(), zone: facility.zone_id.clone(), load });
            }
        }
        (agent.zone_id.clone(), listed)
    };
    let mut known = Vec::with_capacity(listed.len());
    for candidate in listed {
        if knows_facility(world, knows, Some(agent_id), Some(&candidate.id))? {
            known.push(candidate);
        }
    }
    if known.is_empty() {
        return Ok(Selected::NoFacility);
    }
    let mut best: Option<(i64, i64, Arc<str>, Route)> = None;
    for candidate in &known {
        let Some(route) = route_for(world, routing, &agent_zone, &candidate.zone, agent_id, waits)? else {
            continue;
        };
        let better = best
            .as_ref()
            .is_none_or(|(load, minutes, id, _)| (candidate.load, route.minutes, candidate.id.as_bytes()) < (*load, *minutes, id.as_bytes()));
        if better {
            best = Some((candidate.load, route.minutes, candidate.id.clone(), route));
        }
    }
    match best {
        Some((.., id, route)) => Ok(Selected::Found(id, route)),
        None => Ok(Selected::NoRoute(
            known.iter().map(|candidate| candidate.id.clone()).min_by(|first, second| first.as_bytes().cmp(second.as_bytes())).unwrap_or_else(|| Arc::from("")),
        )),
    }
}

/// `_facility_approach`: its route and path, or None for no route. Without a floor plan the route
/// takes no time and has no path.
pub fn approach_route(world: &World, walking: Walking<'_>, movement: &Kept<Movement>, agent_id: &str, facility_id: &str) -> Result<Option<(Route, Vec<Pt>)>> {
    let geo = plan(walking.geo)?;
    if geo.layout.is_none() {
        let rows = world.rows()?;
        let (Some(agent), Some(facility)) = (rows.agents.get(agent_id), rows.facilities.get(facility_id)) else {
            return refuse("no such agent or facility");
        };
        let route = Route {
            origin: agent.zone_id.clone(),
            destination: facility.zone_id.clone(),
            zone_ids: vec![agent.zone_id.clone()],
            edge_ids: Vec::new(),
            minutes: 0,
        };
        return Ok(Some((route, Vec::new())));
    }
    let movement = kit::kept(movement)?;
    let read = |row: &MovementRow| -> Result<_> {
        let rows = world.rows()?;
        let Some(facility) = rows.facilities.get(facility_id) else {
            return refuse("no such facility");
        };
        let zone = facility.zone_id.clone();
        Ok(approach_point(geo, row, &rows, &movement.model, agent_id, facility_id, None)?.map(|found| (found, zone)))
    };
    let found = match held_row::<MovementRow>(world, MOVEMENT, walking.movement)? {
        Some(row) => read(&row)?,
        // `world.model_state.get(MOVEMENT_STATE_KEY, {})`: nobody has a position yet.
        None => read(&no_positions())?,
    };
    Ok(found.map(|((minutes, path), zone)| {
        let route = Route { origin: zone.clone(), destination: zone.clone(), zone_ids: vec![zone.clone(), zone], edge_ids: Vec::new(), minutes };
        (route, path.to_vec())
    }))
}
