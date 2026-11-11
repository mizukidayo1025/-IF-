//! Lane c6: the rules of travel, proximity and replans (`rules::travel`, `rules::proximity`,
//! `rules::replan`) on a world built here with no Python. The PyO3 crate's kernels of the same names
//! are glue over these, so the gate and the offline tests run them on every Run; these say what each
//! reads, decides, refuses and writes.

use std::collections::HashMap;
use std::sync::Arc;

use indexmap::IndexMap;
use serde_json::{json, Value};
use sheltersim_core::config::movement::Movement;
use sheltersim_core::config::Fault;
use sheltersim_core::entities::{Activity, AgentRow, AnyTable, Entities, FacilityRow, Mobility, NeedRow, ProfileRow, Table, TaskRow, EdgeRow, ZoneRow};
use sheltersim_core::error::Error;
use sheltersim_core::events::log::EventLog;
use sheltersim_core::events::queue::Queue;
use sheltersim_core::events::{EventKind, EventRow, EventStatus};
use sheltersim_core::geometry::Geo;
use sheltersim_core::row::{IdMap, ValueRow};
use sheltersim_core::routes::Router;
use sheltersim_core::rules::kit::movement::POLICY_VERSION;
use sheltersim_core::rules::kit::{Events, Held, Kept, POLICY_FAULT};
use sheltersim_core::rules::overlay::{Beds, PLAN_UNREAD};
use sheltersim_core::rules::proximity;
use sheltersim_core::rules::replan::{self, Replanning, Second};
use sheltersim_core::rules::travel::{self, CapacityWait, Graph, Route, Routing, Selected, Trip, Walking};
use sheltersim_core::run::held::{Class, Held as Attr, Obj};
use sheltersim_core::run::proximity::Proximity;
use sheltersim_core::run::services::Manager;
use sheltersim_core::run::{Capacity, RouteCapacities};
use sheltersim_core::state::metric_state::MetricRow;
use sheltersim_core::state::movement::{Journey, MovementRow, Point, Position};
use sheltersim_core::value::V;
use sheltersim_core::world::{Fit, Place, World, ID_COUNTERS, METRIC_STATE, MODEL_CONFIGS};

const MOVEMENT: &str = "movement_v1";

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

fn map(entries: Vec<(&str, V)>) -> V {
    V::Map(entries.into_iter().map(|(key, value)| (text(key), value)).collect())
}

fn unsupported(reason: &str) -> Error {
    Error::Unsupported(reason.to_owned().into())
}

fn needs() -> NeedRow {
    NeedRow { toilet_urgency: 0.0, hunger: 0.0, fatigue: 0.0, hydration_need: 0.0 }
}

fn agent(id: &str, zone: &str, home: &str) -> AgentRow {
    AgentRow {
        profile: ProfileRow {
            id: text(id),
            group_id: None,
            initial_zone_id: text(home),
            mobility: Mobility::Independent,
            support_need: 0.0,
            stress_sensitivity: 1.0,
            rng_key: text(id),
            initial_needs: needs(),
            provenance: V::Map(IndexMap::new()),
        },
        zone_id: text(zone),
        activity: Activity::Idle,
        target_id: None,
        needs: needs(),
        stress: 0.0,
        trust: 0.5,
        next_decision_at: 0,
        support_request_id: None,
        support_cooldown_until: 0,
    }
}

fn put<R: Clone>(table: &mut Table<R>, id: &str, row: R) {
    let slot = table.reserve_slot();
    table.fill_slot(slot, row);
    assert_eq!(table.bind(text(id), slot), None);
}

fn facility(id: &str, kind: &str, zone: &str) -> FacilityRow {
    FacilityRow {
        id: text(id),
        kind: text(kind),
        zone_id: text(zone),
        capacity: 1,
        working_units: 1,
        active_agent_ids: Vec::new(),
        queue_agent_ids: Vec::new(),
        resource_active_agent_ids: Vec::new(),
        resource_queue_agent_ids: Vec::new(),
    }
}

/// Zones z0, z1 and z2 (z2 holds one person), the edges e1 (z0 to z1, 4 minutes), e2 (back) and e3
/// (z1 to z2, 2 minutes). a1 is in z0 and a2 in z1, whose home is z0; f1 is a toilet in z1.
fn entities() -> Entities {
    let mut entities = Entities::default();
    put(&mut entities.agents, "a1", agent("a1", "z0", "z0"));
    put(&mut entities.agents, "a2", agent("a2", "z1", "z0"));
    for (id, capacity, x) in [("z0", None, 0.0), ("z1", None, 10.0), ("z2", Some(1), 20.0)] {
        put(&mut entities.zones, id, ZoneRow { id: text(id), name: text(id), capacity, display_x: Some(x), display_y: Some(0.0) });
    }
    for (id, from, to, minutes) in [("e1", "z0", "z1", 4), ("e2", "z1", "z0", 4), ("e3", "z1", "z2", 2)] {
        put(&mut entities.zone_edges, id, EdgeRow { id: text(id), from_zone_id: text(from), to_zone_id: text(to), travel_minutes: minutes, closed: false });
    }
    put(&mut entities.facilities, "f1", facility("f1", "Shared Toilet", "z1"));
    entities
}

fn add(world: &mut World, name: &str, place: Place, row: Box<dyn sheltersim_core::row::Row>) {
    let index = world.slots.add(text(name), place).unwrap();
    world.slots.at(index).unwrap().try_borrow_mut().unwrap().row = Some(row);
}

fn position(zone: &str, x: f64) -> Position {
    Position { zone_id: text(zone), point: Some(Point { x, y: 0.0 }), edge_id: None, progress: 1.0, position_kind: text("physical") }
}

/// A world of 100 minutes at minute 5 holding its queue, log, `id_counters` (event 10),
/// `metric_state`, these configs and the row of `movement_v1` (a1 at x 0, a2 at x 10).
fn world_with(configs: V) -> World {
    let mut world = World::default();
    world.scalars.get_mut().duration_minutes = Fit::Ok(100);
    world.scalars.get_mut().minute_index = Fit::Ok(5);
    *world.entities.get_mut() = entities();
    add(&mut world, ID_COUNTERS, Place::Field, Box::new(ValueRow(map(vec![("event", V::Int(10))]))));
    add(&mut world, METRIC_STATE, Place::Field, Box::new(MetricRow::default()));
    add(&mut world, MODEL_CONFIGS, Place::Field, Box::new(ValueRow(configs)));
    let mut positions = IdMap::default();
    positions.insert(text("a1"), position("z0", 0.0));
    positions.insert(text("a2"), position("z1", 10.0));
    add(&mut world, MOVEMENT, Place::Namespace, Box::new(MovementRow {
        schema_version: 2,
        policy_version: text(POLICY_VERSION),
        positions,
        staff_positions: IdMap::default(),
        journeys: IdMap::default(),
        edge_usage_minutes: IdMap::default(),
        last_updated_minute: 5,
    }));
    world.hold_queue(Arc::new(Queue::new()));
    world.hold_log(Arc::new(EventLog::new()));
    world
}

fn world() -> World {
    world_with(V::Map(IndexMap::new()))
}

fn graph(world: &World, capacities: RouteCapacities) -> Graph {
    let rows = world.entities.try_borrow().unwrap();
    let router = Router::of_tables(&rows.zones, &rows.zone_edges, &capacities).unwrap();
    Graph { router: Arc::new(router), capacities: Arc::new(capacities) }
}

fn no_plan() -> Arc<Geo> {
    Arc::new(Geo::new(None, None, None, None).unwrap())
}

fn movement(configs: &V) -> Kept<Movement> {
    Movement::parse(configs).map(Arc::new)
}

fn counter(world: &World) -> V {
    world.row::<ValueRow>(ID_COUNTERS).unwrap().0.clone()
}

fn records(world: &World) -> Vec<Value> {
    let log = world.log().unwrap();
    (0..log.len()).map(|index| serde_json::from_slice(log.written(index).unwrap()).unwrap()).collect()
}

fn metric(world: &World, name: &str) -> Option<f64> {
    world.metric_state().unwrap().get(name).copied()
}

/// A pending trip of a person over these edges, due at `due`.
fn trip_event(event_id: &str, sequence: i64, due: i64, agent: &str, edges: &[&str]) -> EventRow {
    let mut payload = IndexMap::new();
    payload.insert(text("agent_id"), V::Str(text(agent)));
    payload.insert(text("route_edge_ids"), V::List(edges.iter().map(|edge| V::Str(text(edge))).collect()));
    EventRow::new(text(event_id), EventKind::Completion, due, 0, sequence, text("agent_travel_complete"), payload, None, None, IndexMap::new()).unwrap()
}

fn schedule(world: &World, row: EventRow) {
    assert_eq!(world.queue_mut().unwrap().schedule_row(row), 0);
}

fn route(origin: &str, destination: &str, zones: &[&str], edges: &[&str], minutes: i64) -> Route {
    Route {
        origin: text(origin),
        destination: text(destination),
        zone_ids: zones.iter().map(|zone| text(zone)).collect(),
        edge_ids: edges.iter().map(|edge| text(edge)).collect(),
        minutes,
    }
}

fn trip<'a>(agent: &str, route: Route, purpose: &'a str) -> Trip<'a> {
    Trip {
        agent_id: text(agent),
        route,
        points: Vec::new(),
        purpose,
        facility_id: None,
        minute: 5,
        causation_id: Some(text("cause")),
        support_request_id: None,
        support_staff_id: None,
    }
}

// ---- The routers ---------------------------------------------------------------------------------

#[test]
fn the_routers_are_the_tables_and_tainted_tables_build_none() {
    let mut world = world();
    let (people, staff) = travel::routers_of(&world, &RouteCapacities::default(), None).unwrap().unwrap();
    // No floor plan: the staff walk the people's graph.
    assert!(Arc::ptr_eq(&people, &staff.unwrap()));
    let found = travel::world_route(&world, &text("z0"), &text("z2")).unwrap().unwrap();
    assert_eq!((found.edge_ids, found.minutes), (vec![text("e1"), text("e3")], 6));
    // A plan the PyO3 crate could not read is said where the staff's graph needs it.
    let planned = world_with(map(vec![("facility_geometry", V::Null)]));
    let (_, staff) = travel::routers_of(&planned, &RouteCapacities::default(), None).unwrap().unwrap();
    assert_eq!(staff.err(), Some(unsupported(PLAN_UNREAD)));
    world.entities.get_mut().taint_zones("a value that does not fit".to_owned());
    assert!(travel::routers_of(&world, &RouteCapacities::default(), None).unwrap().is_none());
    assert_eq!(travel::world_route(&world, &text("z0"), &text("z2")).err(), Some(unsupported(travel::ZONES_TAINTED)));
}

#[test]
fn the_world_graph_takes_the_capacities_zone_graph_takes() {
    let routing = |table: V| map(vec![("layout_routing", map(vec![("max_agents_in_transit_by_edge", table)]))]);
    let world = world_with(routing(map(vec![("e1", V::Int(2))])));
    let graph = travel::world_graph(&world).unwrap();
    assert!(graph.has_zone("z2"));
    for table in [map(vec![("nope", V::Int(1))]), map(vec![("e1", V::Int(0))]), map(vec![("e1", V::Int(101))])] {
        assert_eq!(travel::world_graph(&world_with(routing(table))).err(), Some(unsupported("a capacity the zone graph turns down")));
    }
    assert_eq!(travel::world_graph(&world_with(routing(map(vec![("e1", V::Str(text("2")))])))).err(), Some(unsupported("a value is not an int")));
}

// ---- Who is on an edge ---------------------------------------------------------------------------

#[test]
fn the_loads_count_the_trips_under_way() {
    let world = world();
    schedule(&world, trip_event("ev1", 3, 50, "a2", &["e1", "e3"]));
    // Due now: no longer under way.
    schedule(&world, trip_event("ev2", 4, 5, "a2", &["e1"]));
    let loads = travel::active_edge_loads(&world, &Held::Absent, 5).unwrap();
    assert_eq!(loads, HashMap::from([(text("e1"), 1), (text("e3"), 1)]));
    assert_eq!(travel::active_edge_loads(&world, &Held::Other("another"), 5).err(), Some(unsupported("another")));
    // An event Python scheduled whose row was not read: the PyO3 crate reads them first.
    world.queue_mut().unwrap().add(text("ev3"), (60, 0, 7), None);
    assert_eq!(travel::active_edge_loads(&world, &Held::Absent, 5).err(), Some(unsupported("an event in the queue is not a plain one")));
}

#[test]
fn the_passage_loads_count_each_walker_on_the_edge_of_its_share_of_the_time() {
    let world = world();
    {
        let mut row = world.row_mut::<MovementRow>(MOVEMENT).unwrap();
        row.journeys.insert(text("a1"), Journey {
            event_id: text("ev1"),
            task_id: text("t1"),
            purpose: text("return"),
            facility_id: None,
            destination_zone_id: text("z2"),
            route_zone_ids: vec![text("z0"), text("z1"), text("z2")],
            route_edge_ids: vec![text("e1"), text("e3")],
            route_points: Vec::new(),
            started_minute: 0,
            due_minute: 6,
            queue_id: None,
            support_request_id: None,
            support_staff_id: None,
        });
    }
    // Minute 3 of 6 over 4 + 2 minutes: e1. Minute 5: 5 / 6 * 6 = 5, past e1's 4: e3.
    assert_eq!(travel::passage_edge_loads(&world, &Held::Held, &Held::Absent, 3).unwrap(), HashMap::from([(text("e1"), 1)]));
    assert_eq!(travel::passage_edge_loads(&world, &Held::Held, &Held::Absent, 5).unwrap(), HashMap::from([(text("e3"), 1)]));
    assert!(travel::passage_edge_loads(&world, &Held::Held, &Held::Absent, 6).unwrap().is_empty());
}

// ---- _route --------------------------------------------------------------------------------------

#[test]
fn a_full_edge_records_a_capacity_wait_through_the_writer() {
    let world = world();
    let mut capacities = RouteCapacities { any: true, ..RouteCapacities::default() };
    capacities.by_edge.insert(text("e1"), Capacity::Max(1));
    let graph = graph(&world, capacities);
    let (geo, section) = (no_plan(), movement(&V::Map(IndexMap::new())));
    let routing = Routing { graph: &graph, geo: Some(&geo), movement: &section, staff: &Held::Absent };
    // Free: the route, and nothing written.
    let found = travel::route(&world, &routing, &text("z0"), &text("z1"), "a1").unwrap().unwrap();
    assert_eq!((found.edge_ids, found.minutes), (vec![text("e1")], 4));
    assert!(records(&world).is_empty());
    schedule(&world, trip_event("ev1", 3, 50, "a2", &["e1"]));
    let mut waits = Vec::new();
    assert!(travel::route_for(&world, &routing, &text("z0"), &text("z1"), "a1", &mut waits).unwrap().is_none());
    let wait = CapacityWait { agent_id: text("a1"), origin: text("z0"), destination: text("z1"), edge_id: text("e1"), capacity: 1, active: 1 };
    assert_eq!(waits, vec![wait]);
    assert!(travel::route(&world, &routing, &text("z0"), &text("z1"), "a1").unwrap().is_none());
    let records = records(&world);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["status"], json!("suppressed"));
    assert_eq!(records[0]["detail"], json!("route edge capacity is full; the Agent remains at its origin"));
    assert_eq!(records[0]["event"]["event_id"], json!("zone-edge-capacity-wait-10"));
    let payload: Vec<&String> = records[0]["event"]["payload"].as_object().unwrap().keys().collect();
    assert_eq!(payload, ["active_travelers", "agent_id", "capacity", "destination_zone_id", "edge_id", "origin_zone_id"]);
    assert_eq!(counter(&world), map(vec![("event", V::Int(11))]));
}

#[test]
fn a_plan_the_pyo3_crate_could_not_read_is_said_where_the_route_reads_it() {
    let world = world_with(map(vec![("facility_geometry", map(vec![]))]));
    let graph = graph(&world, RouteCapacities::default());
    let section = movement(&V::Map(IndexMap::new()));
    let routing = Routing { graph: &graph, geo: None, movement: &section, staff: &Held::Absent };
    assert_eq!(travel::route(&world, &routing, &text("z0"), &text("z1"), "a1").err().map(|error| format!("{error:?}")), Some(format!("{:?}", sheltersim_core::events::writer::WriteError::from(unsupported(PLAN_UNREAD)))));
}

// ---- _schedule_travel ----------------------------------------------------------------------------

#[test]
fn a_trip_is_planned_then_written() {
    let world = world_with(map(vec![("layout_routing", map(vec![("source_id", V::Str(text("SRC")))]))]));
    let geo = no_plan();
    let walking = Walking { movement: &Held::Held, geo: Some(&geo) };
    let events = Events::of(&world).unwrap();
    let planned = travel::plan_travel(&world, &events, walking, trip("a1", route("z0", "z1", &["z0", "z1"], &["e1"], 4), "return")).unwrap().unwrap();
    let event = planned.event().clone();
    assert_eq!((&*event.event_id, event.due_minute, event.sequence), ("agent-travel-complete-10", 9, 10));
    let V::Map(payload) = &event.payload else { panic!("a payload") };
    let keys: Vec<&str> = payload.keys().map(|key| &**key).collect();
    assert_eq!(keys, ["agent_id", "task_id", "purpose", "destination_zone_id", "facility_id", "route_zone_ids", "route_edge_ids", "travel_minutes"]);
    let V::Map(provenance) = &event.provenance else { panic!("a provenance") };
    assert_eq!(provenance.get("source_id"), Some(&V::Str(text("SRC"))));
    // Nothing is written while planned.
    assert_eq!(counter(&world), map(vec![("event", V::Int(10))]));
    travel::write_travel(&world, &events, planned).unwrap();
    assert_eq!(counter(&world), map(vec![("event", V::Int(11))]));
    let rows = world.rows().unwrap();
    let task = rows.active_tasks.get("agent-travel-10").unwrap();
    assert_eq!((&*task.kind, task.start_minute, task.due_minute), ("travel_return", 5, 9));
    let a1 = rows.agents.get("a1").unwrap();
    assert_eq!((a1.activity, a1.target_id.as_deref()), (Activity::Moving, Some("z1")));
    drop(rows);
    assert!(world.queue().unwrap().has_event("agent-travel-complete-10"));
    let journey = world.row::<MovementRow>(MOVEMENT).unwrap().journeys.get("a1").cloned().unwrap();
    assert_eq!((&*journey.task_id, journey.started_minute, journey.due_minute), ("agent-travel-10", 5, 9));
}

#[test]
fn a_trip_turned_down_writes_nothing() {
    let world = world();
    let walking = Walking { movement: &Held::Absent, geo: None };
    let events = Events::of(&world).unwrap();
    let plan = |trip: Trip<'_>| travel::plan_travel(&world, &events, walking, trip);
    // From elsewhere: the Python rule's ValueError.
    assert_eq!(plan(trip("a1", route("z1", "z2", &["z1", "z2"], &["e3"], 2), "return")).err(), Some(Error::Value("travel route origin does not match Agent zone".to_owned())));
    // No time, past the end, a full zone (z2 holds one person, and a2 walks there).
    assert!(plan(trip("a1", route("z0", "z1", &["z0", "z1"], &["e1"], 0), "return")).unwrap().is_none());
    assert!(plan(trip("a1", route("z0", "z1", &["z0", "z1"], &["e1"], 95), "return")).unwrap().is_none());
    world.rows_mut().unwrap().agents.get_mut("a2").unwrap().activity = Activity::Moving;
    world.rows_mut().unwrap().agents.get_mut("a2").unwrap().target_id = Some(text("z2"));
    assert!(plan(trip("a2", route("z1", "z2", &["z1", "z2"], &["e3"], 2), "return")).unwrap().is_none());
    // A support trip without its IDs.
    let mut support = trip("a1", route("z0", "z1", &["z0", "z1"], &["e1"], 4), "staff_support_travel");
    support.support_request_id = Some(text(""));
    support.support_staff_id = Some(text("s1"));
    assert_eq!(plan(support).err(), Some(Error::Value("staff support travel requires request and staff IDs".to_owned())));
    assert_eq!(counter(&world), map(vec![("event", V::Int(10))]));
    assert!(world.rows().unwrap().active_tasks.is_empty());
}

#[test]
fn the_walk_home_after_a_service_is_a_return_trip() {
    let world = world();
    let graph = graph(&world, RouteCapacities::default());
    let (geo, section) = (no_plan(), movement(&V::Map(IndexMap::new())));
    let routing = Routing { graph: &graph, geo: Some(&geo), movement: &section, staff: &Held::Absent };
    let walking = Walking { movement: &Held::Absent, geo: Some(&geo) };
    let mut payload = IndexMap::new();
    payload.insert(text("agent_id"), V::Str(text("a2")));
    let service = EventRow::new(text("svc-1"), EventKind::Completion, 5, 0, 2, text("toilet_service_complete"), payload, None, None, IndexMap::new()).unwrap();
    travel::schedule_return(&world, &routing, walking, &service).unwrap();
    let pending = world.queue().unwrap().pending_ids(None);
    assert_eq!(pending, vec![text("agent-travel-complete-10")]);
    let queue = world.queue().unwrap();
    let row = queue.slot(queue.serial_of("agent-travel-complete-10").unwrap()).row.clone().unwrap();
    assert_eq!(row.causation_id.as_deref(), Some("svc-1"));
    assert_eq!(kit_text(&row, "purpose"), Some("return"));
    assert_eq!(kit_text(&row, "destination_zone_id"), Some("z0"));
    drop(queue);
    // At home already: nothing.
    let mut home = IndexMap::new();
    home.insert(text("agent_id"), V::Str(text("a1")));
    let other = EventRow::new(text("svc-2"), EventKind::Completion, 5, 0, 3, text("toilet_service_complete"), home, None, None, IndexMap::new()).unwrap();
    travel::schedule_return(&world, &routing, walking, &other).unwrap();
    assert_eq!(world.queue().unwrap().len(), 1);
}

fn kit_text<'a>(row: &'a EventRow, name: &str) -> Option<&'a str> {
    match sheltersim_core::rules::kit::payload(row, name) {
        Some(V::Str(value)) => Some(value),
        _ => None,
    }
}

// ---- The choice of a facility --------------------------------------------------------------------

#[test]
fn a_toilet_is_chosen_by_its_folded_kind_what_the_person_knows_and_its_queue() {
    let select = |world: &World, limited: bool| {
        let graph = graph(world, RouteCapacities::default());
        let (geo, section) = (no_plan(), movement(&V::Map(IndexMap::new())));
        let routing = Routing { graph: &graph, geo: Some(&geo), movement: &section, staff: &Held::Absent };
        let knows: Kept<bool> = Ok(Arc::new(limited));
        let mut waits = Vec::new();
        travel::select(world, &routing, &knows, "a1", None, &mut waits).unwrap()
    };
    let open = world();
    assert_eq!(select(&open, false), Selected::Found(text("f1"), route("z0", "z1", &["z0", "z1"], &["e1"], 4)));
    // Knowledge limited, and no record of what the person knows: nothing known.
    assert_eq!(select(&open, true), Selected::NoFacility);
    // A queue of no room.
    let full = world_with(map(vec![("facility_queues", map(vec![("schema_version", V::Int(1)), ("queue_capacity_by_facility", map(vec![("f1", V::Int(0))]))]))]));
    assert_eq!(select(&full, false), Selected::NoFacility);
    // Its zone out of reach: the lowest ID of what is known.
    let closed = world();
    closed.entities.try_borrow_mut().unwrap().zone_edges.get_mut("e1").unwrap().closed = true;
    assert_eq!(select(&closed, false), Selected::NoRoute(text("f1")));
}

#[test]
fn without_a_floor_plan_the_approach_takes_no_time() {
    let world = world();
    let geo = no_plan();
    let section = movement(&V::Map(IndexMap::new()));
    let found = travel::approach_route(&world, Walking { movement: &Held::Held, geo: Some(&geo) }, &section, "a1", "f1").unwrap().unwrap();
    assert_eq!(found, (route("z0", "z1", &["z0"], &[], 0), Vec::new()));
    assert_eq!(travel::approach_route(&world, Walking { movement: &Held::Held, geo: None }, &section, "a1", "f1").err(), Some(unsupported(PLAN_UNREAD)));
}

// ---- The writer's settle -------------------------------------------------------------------------

#[test]
fn a_settled_event_leaves_the_queue_and_is_recorded_once() {
    let world = world();
    let event = trip_event("ev1", 3, 50, "a1", &["e1"]);
    schedule(&world, event.clone());
    let serial = world.queue().unwrap().serial_of("ev1").unwrap();
    let events = Events::of(&world).unwrap();
    let settled = events.settle(&event, 5, EventStatus::Suppressed, Some("why")).unwrap();
    assert_eq!((settled.taken, settled.recorded), (Some(serial), true));
    assert!(!world.queue().unwrap().has_event("ev1"));
    // Its record is there already: nothing more.
    let again = events.settle(&event, 5, EventStatus::Suppressed, Some("why")).unwrap();
    assert_eq!((again.taken, again.recorded), (None, false));
    assert_eq!(records(&world).len(), 1);
    assert_eq!(records(&world)[0]["detail"], json!("why"));
}

// ---- Proximity -----------------------------------------------------------------------------------

fn proximity(config: V, known: bool, policy_config: V) -> Proximity {
    let mut policy = Obj::new(Class::named("sheltersim.policies.proximity", "DistanceSampledProximityPolicy"));
    policy.attrs.insert(text("config"), Ok(Attr::Value(policy_config)));
    Proximity { config: Ok(config), known: Ok(known), policy: Ok(Some(Arc::new(policy))) }
}

fn contact_config(interval: i64) -> V {
    map(vec![("sample_interval_minutes", V::Int(interval)), ("policy_version", V::Str(text("proximity-v1")))])
}

fn sampler(radius: f64, limit: i64) -> V {
    map(vec![("max_distance_m", V::Float(radius)), ("max_pairs_per_sample", V::Int(limit))])
}

#[test]
fn the_step_samples_only_at_its_interval() {
    let on = proximity(contact_config(15), true, sampler(1.5, 10));
    assert_eq!(proximity::when(&proximity(V::Null, true, V::Null), Some(15)).unwrap(), None);
    assert_eq!(proximity::when(&on, Some(15)).unwrap(), Some(15));
    assert_eq!(proximity::when(&on, Some(16)).unwrap(), None);
    assert_eq!(proximity::when(&on, None).err(), Some(unsupported("the minute is not an int")));
    let no_interval = proximity(map(vec![]), true, V::Null);
    assert_eq!(proximity::when(&no_interval, Some(15)).err(), Some(Error::Key("sample_interval_minutes".to_owned())));
    assert_eq!(proximity::when(&proximity(contact_config(0), true, V::Null), Some(15)).err(), Some(unsupported("the sampling interval is not positive")));
    // A policy whose read raised: the PyO3 crate reads it again.
    let raised = Proximity { config: Err(Fault::new(sheltersim_core::config::FaultClass::Value, "boom")), known: Ok(true), policy: Ok(None) };
    assert_eq!(proximity::when(&raised, Some(15)).err(), Some(unsupported(POLICY_FAULT)));
}

#[test]
fn the_pairs_are_the_closest_first_within_a_zone_and_scheduled() {
    let world = world();
    {
        let mut row = world.row_mut::<MovementRow>(MOVEMENT).unwrap();
        row.positions.insert(text("a2"), position("z0", 1.0));
        // Not a person of the world: not sampled.
        row.positions.insert(text("gone"), position("z0", 0.5));
    }
    let (points, pairs) = proximity::sample(&world, &sampler(1.5, 10), &Held::Held).unwrap();
    assert_eq!(points.iter().map(|point| &*point.id).collect::<Vec<_>>(), ["a1", "a2"]);
    assert_eq!(pairs, vec![(1.0, 0, 1)]);
    assert!(proximity::sample(&world, &sampler(0.5, 10), &Held::Held).unwrap().1.is_empty());
    // A world without the namespace places nobody.
    assert!(proximity::sample(&world, &sampler(1.5, 10), &Held::Absent).unwrap().0.is_empty());
    let on = proximity(contact_config(15), true, sampler(1.5, 10));
    proximity::sample_contacts(&world, &on, 15, &Held::Held).unwrap();
    let queue = world.queue().unwrap();
    let row = queue.slot(queue.serial_of("proximity-contact-10").unwrap()).row.clone().unwrap();
    assert_eq!((row.due_minute, row.priority, row.kind), (16, 40, EventKind::Endogenous));
    assert_eq!(kit_text(&row, "first_agent_id"), Some("a1"));
    drop(queue);
    assert_eq!(counter(&world), map(vec![("event", V::Int(11))]));
    let other = proximity(contact_config(15), false, sampler(1.5, 10));
    assert_eq!(proximity::sample_contacts(&world, &other, 30, &Held::Held).err().map(|error| format!("{error:?}")), Some(format!("{:?}", sheltersim_core::events::writer::WriteError::from(unsupported("another proximity policy")))));
}

// ---- Replans -------------------------------------------------------------------------------------

#[test]
fn a_person_whose_route_closed_stops_and_is_blocked_with_no_other_route() {
    let world = world();
    {
        let mut row = world.row_mut::<MovementRow>(MOVEMENT).unwrap();
        row.journeys.insert(text("a1"), Journey {
            event_id: text("ev1"),
            task_id: text("t1"),
            purpose: text("return"),
            facility_id: None,
            destination_zone_id: text("z1"),
            route_zone_ids: vec![text("z0"), text("z1")],
            route_edge_ids: vec![text("e1")],
            route_points: Vec::new(),
            started_minute: 3,
            due_minute: 7,
            queue_id: None,
            support_request_id: None,
            support_staff_id: None,
        });
    }
    put(&mut world.rows_mut().unwrap().active_tasks, "t1", TaskRow {
        id: text("t1"),
        kind: text("travel_return"),
        agent_id: Some(text("a1")),
        resource_id: None,
        start_minute: 3,
        due_minute: 7,
        staff_id: None,
    });
    world.rows_mut().unwrap().agents.get_mut("a1").unwrap().activity = Activity::Moving;
    schedule(&world, trip_event("ev1", 3, 7, "a1", &["e1"]));
    let serial = world.queue().unwrap().serial_of("ev1").unwrap();
    // The router of a Run whose e1 closed.
    world.entities.try_borrow_mut().unwrap().zone_edges.get_mut("e1").unwrap().closed = true;
    let graph = graph(&world, RouteCapacities::default());
    let (geo, section) = (no_plan(), movement(&V::Map(IndexMap::new())));
    let schema: Kept<Option<i64>> = Ok(Arc::new(None));
    let staff = Manager { manager: Ok(None), known: Ok(false) };
    let replanning = Replanning {
        routing: Routing { graph: &graph, geo: Some(&geo), movement: &section, staff: &Held::Absent },
        walking: Walking { movement: &Held::Held, geo: Some(&geo) },
        beds: Beds { overlay: &Held::Absent, movement: &Held::Held, schema: &schema, walking: &section, geo: Some(&geo) },
        movement: &section,
        staff_service: &staff,
    };
    let journey = replan::journey_of(&world, "a1").unwrap().unwrap();
    assert!(replan::journey_of(&world, "a2").unwrap().is_none());
    let taken = replan::stop(&world, &text("a1"), &journey, 5).unwrap();
    assert_eq!(taken, vec![serial]);
    assert!(!world.queue().unwrap().has_event("ev1"));
    assert_eq!(records(&world)[0]["detail"], json!("the route changed before travel completion"));
    {
        let rows = world.rows().unwrap();
        assert!(rows.active_tasks.get("t1").is_none());
        assert_eq!(rows.agents.get("a1").unwrap().activity, Activity::Idle);
    }
    assert!(world.row::<MovementRow>(MOVEMENT).unwrap().journeys.get("a1").is_none());
    let second = replan::second_step(&world, &replanning, &text("a1"), &journey, 5).unwrap();
    assert!(matches!(second, Second::Done(ref taken) if taken.is_empty()));
    assert_eq!((metric(&world, "movement_replan_blocked_count"), metric(&world, "movement_replan_count")), (Some(1.0), Some(1.0)));
}
