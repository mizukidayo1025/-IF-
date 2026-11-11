//! Lane c2: the rules of `domain/movement.py` (`rules::movement`) on a world built here with no
//! Python. The PyO3 crate's kernels of the same names are glue over these, so the gate and the
//! offline tests run them on every Run; these say what each reads, decides and writes.

use std::sync::Arc;

use indexmap::IndexMap;
use sheltersim_core::config::geometry::FacilityPlan;
use sheltersim_core::config::movement::MovementModel;
use sheltersim_core::entities::{Activity, AgentRow, AnyTable, EdgeRow, Entities, Mobility, NeedRow, ProfileRow, Skills, StaffRow, Table, TaskRow, ZoneRow};
use sheltersim_core::error::Error;
use sheltersim_core::events::log::EventLog;
use sheltersim_core::events::queue::Queue;
use sheltersim_core::events::{EventKind, EventRow};
use sheltersim_core::geometry::{zone_center, Geo, OverlayKey, Pt, Route, Zone};
use sheltersim_core::row::{IdMap, ValueRow};
use sheltersim_core::rules::kit::movement::POLICY_VERSION;
use sheltersim_core::rules::kit::Held;
use sheltersim_core::rules::movement::{self as movement, Advanced, InOrder, StaffOp, StaffOps, StaffTravel, Start, Tick, Validated, Where};
use sheltersim_core::state::metric_state::MetricRow;
use sheltersim_core::state::movement::{Journey, MovementRow, Point, Position};
use sheltersim_core::state::staff_operations::{OperationRecord, Phase, StaffOperationsRow};
use sheltersim_core::value::V;
use sheltersim_core::world::{Fit, Place, World, ID_COUNTERS, METRIC_STATE};

const MOVEMENT: &str = "movement_v1";
const STAFF_OPERATIONS: &str = "staff_operations_v1";

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

/// A `V` map of these entries, in this order.
fn map(entries: Vec<(&str, V)>) -> V {
    V::Map(entries.into_iter().map(|(key, value)| (text(key), value)).collect())
}

fn xy_value(x: f64, y: f64) -> V {
    map(vec![("x", V::Float(x)), ("y", V::Float(y))])
}

fn unsupported(reason: &str) -> Error {
    Error::Unsupported(reason.to_owned().into())
}

fn needs() -> NeedRow {
    NeedRow { toilet_urgency: 0.0, hunger: 0.0, fatigue: 0.0, hydration_need: 0.0 }
}

fn agent(id: &str, zone: &str, activity: Activity) -> AgentRow {
    AgentRow {
        profile: ProfileRow {
            id: text(id),
            group_id: None,
            initial_zone_id: text(zone),
            mobility: Mobility::Independent,
            support_need: 0.0,
            stress_sensitivity: 1.0,
            rng_key: text(id),
            initial_needs: needs(),
            provenance: V::Map(IndexMap::new()),
        },
        zone_id: text(zone),
        activity,
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

fn square(x: f64) -> Vec<Pt> {
    vec![Pt { x, y: 0.0 }, Pt { x: x + 10.0, y: 0.0 }, Pt { x: x + 10.0, y: 10.0 }, Pt { x, y: 10.0 }]
}

/// Two rooms of 10 m by 10 m side by side, z0 and z1, their centres (5, 5) and (15, 5), and the
/// edge e1 between them drawn from centre to centre. `area` is z0's usable area.
fn geo_with(area: f64) -> Geo {
    let mut zones = IdMap::default();
    for (id, x, usable_area) in [("z0", 0.0, area), ("z1", 10.0, 100.0)] {
        let polygon = square(x);
        let center = zone_center(&polygon);
        zones.insert(text(id), Zone { polygon, usable_area, center });
    }
    let mut routes = IdMap::default();
    routes.insert(text("e1"), Route { points: vec![Pt { x: 5.0, y: 5.0 }, Pt { x: 15.0, y: 5.0 }], width: 2.0, accessible: true });
    let plan = FacilityPlan { zones, routes, facility_points: IdMap::default(), service_points: Vec::new() };
    Geo::new(Some(&plan), None, None, None).unwrap()
}

fn geo() -> Geo {
    geo_with(100.0)
}

fn no_plan() -> Geo {
    Geo::new(None, None, None, None).unwrap()
}

/// a1 walks from z0 to z1 over e1 (event ev1, task t1, minutes 0 to 4); a2 idles in z1; the staff
/// member s1 stands in z1. The zones have display points (1, 2) and (3, 4).
fn entities() -> Entities {
    let mut entities = Entities::default();
    put(&mut entities.agents, "a1", agent("a1", "z0", Activity::Moving));
    put(&mut entities.agents, "a2", agent("a2", "z1", Activity::Idle));
    put(&mut entities.active_tasks, "t1", TaskRow {
        id: text("t1"),
        kind: text("travel"),
        agent_id: Some(text("a1")),
        resource_id: None,
        start_minute: 0,
        due_minute: 4,
        staff_id: None,
    });
    put(&mut entities.staff, "s1", StaffRow {
        id: text("s1"),
        zone_id: text("z1"),
        skills: Skills(Vec::new()),
        work_queue: Vec::new(),
        busy_until: 0,
        active_task_id: None,
    });
    for (id, x, y) in [("z0", 1.0, 2.0), ("z1", 3.0, 4.0)] {
        put(&mut entities.zones, id, ZoneRow { id: text(id), name: text(id), capacity: None, display_x: Some(x), display_y: Some(y) });
    }
    put(&mut entities.zone_edges, "e1", EdgeRow { id: text("e1"), from_zone_id: text("z0"), to_zone_id: text("z1"), travel_minutes: 4, closed: false });
    entities
}

fn position(zone: &str, x: f64, y: f64) -> Position {
    Position { zone_id: text(zone), point: Some(Point { x, y }), edge_id: None, progress: 0.0, position_kind: text("physical") }
}

fn journey(purpose: &str) -> Journey {
    Journey {
        event_id: text("ev1"),
        task_id: text("t1"),
        purpose: text(purpose),
        facility_id: None,
        destination_zone_id: text("z1"),
        route_zone_ids: vec![text("z0"), text("z1")],
        route_edge_ids: vec![text("e1")],
        route_points: Vec::new(),
        started_minute: 0,
        due_minute: 4,
        queue_id: None,
        support_request_id: None,
        support_staff_id: None,
    }
}

fn event(event_id: &str, sequence: i64) -> EventRow {
    EventRow::new(text(event_id), EventKind::Completion, 4, 0, sequence, text("agent_travel_complete"), IndexMap::new(), None, None, IndexMap::new())
        .unwrap()
}

/// A world of 100 minutes at minute 1 holding its queue (ev1 pending), log, `id_counters`,
/// `metric_state` and the row of `movement_v1`, last updated at minute 0: a1 on its journey at
/// z0's centre, a2 at z1's.
fn world() -> World {
    let mut world = World::default();
    world.scalars.get_mut().duration_minutes = Fit::Ok(100);
    world.scalars.get_mut().minute_index = Fit::Ok(1);
    *world.entities.get_mut() = entities();
    let mut counters = IndexMap::new();
    counters.insert(text("event"), V::Int(10));
    add(&mut world, ID_COUNTERS, Place::Field, Box::new(ValueRow(V::Map(counters))));
    add(&mut world, METRIC_STATE, Place::Field, Box::new(MetricRow::default()));
    let mut positions = IdMap::default();
    positions.insert(text("a1"), position("z0", 5.0, 5.0));
    positions.insert(text("a2"), position("z1", 15.0, 5.0));
    let mut journeys = IdMap::default();
    journeys.insert(text("a1"), journey("travel"));
    add(&mut world, MOVEMENT, Place::Namespace, Box::new(MovementRow {
        schema_version: 2,
        policy_version: text(POLICY_VERSION),
        positions,
        staff_positions: IdMap::default(),
        journeys,
        edge_usage_minutes: IdMap::default(),
        last_updated_minute: 0,
    }));
    let mut queue = Queue::new();
    assert_eq!(queue.schedule_row(event("ev1", 3)), 0);
    world.hold_queue(Arc::new(queue));
    world.hold_log(Arc::new(EventLog::new()));
    world
}

fn add(world: &mut World, name: &str, place: Place, row: Box<dyn sheltersim_core::row::Row>) {
    let index = world.slots.add(text(name), place).unwrap();
    world.slots.at(index).unwrap().try_borrow_mut().unwrap().row = Some(row);
}

fn row(world: &World) -> MovementRow {
    world.row::<MovementRow>(MOVEMENT).unwrap().clone()
}

fn written(world: &mut World) -> Vec<Arc<str>> {
    let mut names = world.slots.written_names();
    names.sort();
    world.slots.take_written();
    names
}

fn xy(position: &Position) -> Option<(f64, f64)> {
    position.point.map(|point| (point.x, point.y))
}

fn advance(world: &World, geo: &Geo, staff: &StaffOps, minute: i64) -> Result<Advanced, Error> {
    movement::advance(world, &Tick { geo, staff }, minute, None)
}

fn done(advanced: Advanced) -> Vec<Arc<str>> {
    match advanced {
        Advanced::Done(replans) => replans,
        Advanced::NeedsInOrder => panic!("the walk asked for its order"),
        Advanced::Planned(_) => panic!("the walk replanned a queue trip"),
    }
}

// ---- advance_movement ----------------------------------------------------------------------------

#[test]
fn a_journey_walks_its_edge_to_the_next_boundary() {
    let mut world = world();
    let geo = geo();
    assert!(done(advance(&world, &geo, &StaffOps::None, 1).unwrap()).is_empty());
    let row = row(&world);
    assert_eq!(row.last_updated_minute, 1);
    // Boundary 2 of the 4 minutes: half the edge, from z0's centre towards z1's.
    let walking = &row.positions["a1"];
    assert_eq!((xy(walking), walking.edge_id.as_deref(), walking.progress), (Some((10.0, 5.0)), Some("e1"), 0.5));
    assert_eq!((&*walking.zone_id, &*walking.position_kind), ("z0", "physical"));
    assert_eq!(row.edge_usage_minutes.iter().map(|(id, count)| (&**id, *count)).collect::<Vec<_>>(), [("e1", 1)]);
    // Without a journey a2 stands at the centre of z1, as does the staff member.
    assert_eq!(xy(&row.positions["a2"]), Some((15.0, 5.0)));
    assert_eq!(row.staff_positions.keys().map(|id| &**id).collect::<Vec<_>>(), ["s1"]);
    assert_eq!(xy(&row.staff_positions["s1"]), Some((15.0, 5.0)));
    assert_eq!(written(&mut world), [text(MOVEMENT)]);
    // The same minute again changes nothing, and the slot keeps its stamp.
    let stamp = world.stamp_of(MOVEMENT).unwrap();
    assert!(done(advance(&world, &geo, &StaffOps::None, 1).unwrap()).is_empty());
    assert_eq!(world.stamp_of(MOVEMENT).unwrap(), stamp);
    assert!(written(&mut world).is_empty());
}

#[test]
fn without_a_floor_plan_people_stand_at_the_display_points() {
    let world = world();
    assert!(done(advance(&world, &no_plan(), &StaffOps::None, 1).unwrap()).is_empty());
    let row = row(&world);
    let walking = &row.positions["a1"];
    assert_eq!((xy(walking), walking.edge_id.as_deref(), &*walking.position_kind), (None, Some("e1"), "unavailable"));
    let idle = &row.positions["a2"];
    assert_eq!((xy(idle), &*idle.position_kind), (Some((3.0, 4.0)), "display_only"));
    assert_eq!((xy(&row.staff_positions["s1"]), &*row.staff_positions["s1"].position_kind), (None, "unavailable"));
}

#[test]
fn a_closed_edge_returns_the_person_for_a_replan() {
    let world = world();
    world.rows_mut().unwrap().zone_edges.get_mut("e1").unwrap().closed = true;
    assert_eq!(done(advance(&world, &geo(), &StaffOps::None, 1).unwrap()), [text("a1")]);
    let row = row(&world);
    assert_eq!(xy(&row.positions["a1"]), Some((5.0, 5.0)));
    assert!(row.journeys.contains_key("a1"));
    assert!(row.edge_usage_minutes.is_empty());
}

#[test]
fn a_journey_whose_event_is_gone_ends_and_its_person_stands_still() {
    let mut world = world();
    world.hold_queue(Arc::new(Queue::new()));
    assert!(done(advance(&world, &geo(), &StaffOps::None, 1).unwrap()).is_empty());
    let row = row(&world);
    assert!(row.journeys.is_empty());
    assert_eq!(xy(&row.positions["a1"]), Some((5.0, 5.0)));
}

#[test]
fn a_journey_of_someone_who_left_goes_with_their_position() {
    let world = world();
    world.rows_mut().unwrap().agents.remove("a1");
    assert!(done(advance(&world, &geo(), &StaffOps::None, 1).unwrap()).is_empty());
    let row = row(&world);
    assert!(row.journeys.is_empty());
    assert_eq!(row.positions.keys().map(|id| &**id).collect::<Vec<_>>(), ["a2"]);
}

#[test]
fn an_edge_the_world_does_not_know_is_refused_with_nothing_written() {
    let mut world = world();
    world.rows_mut().unwrap().zone_edges.remove("e1");
    assert_eq!(advance(&world, &geo(), &StaffOps::None, 1).err(), Some(unsupported("a journey's edge is unknown")));
    assert_eq!(row(&world).last_updated_minute, 0);
    assert!(written(&mut world).is_empty());
}

#[test]
fn a_person_in_a_queue_slot_asks_for_the_walk_in_order() {
    let mut world = world();
    world.hold_queue(Arc::new(Queue::new()));
    {
        let mut rows = world.rows_mut().unwrap();
        let agent = rows.agents.get_mut("a1").unwrap();
        agent.activity = Activity::Queued;
        agent.target_id = Some(text("toilet_01"));
    }
    let geo = geo();
    assert!(matches!(advance(&world, &geo, &StaffOps::None, 1).unwrap(), Advanced::NeedsInOrder));
    assert!(written(&mut world).is_empty());
    let model = Ok(MovementModel::default());
    let in_order = InOrder { model: &model, queues: Held::Absent, metric_state_held: true };
    let advanced = movement::advance(&world, &Tick { geo: &geo, staff: &StaffOps::None }, 1, Some(&in_order)).unwrap();
    assert!(done(advanced).is_empty());
    // No such facility: the slot is the zone's centre, and the journey ends.
    let row = row(&world);
    assert!(row.journeys.is_empty());
    assert_eq!(xy(&row.positions["a1"]), Some((5.0, 5.0)));
    // `world.metric_state` assigned whole is refused, before anything is written.
    let mut world = self::world();
    world.hold_queue(Arc::new(Queue::new()));
    world.rows_mut().unwrap().agents.get_mut("a1").unwrap().activity = Activity::Queued;
    world.rows_mut().unwrap().agents.get_mut("a1").unwrap().target_id = Some(text("toilet_01"));
    let in_order = InOrder { model: &model, queues: Held::Absent, metric_state_held: false };
    let refused = movement::advance(&world, &Tick { geo: &geo, staff: &StaffOps::None }, 1, Some(&in_order));
    assert_eq!(refused.err(), Some(unsupported("the field holds another dict")));
    assert!(written(&mut world).is_empty());
}

#[test]
fn a_queue_trip_whose_event_was_applied_keeps_its_point_off_the_edge() {
    let mut world = world();
    world.hold_queue(Arc::new(Queue::new()));
    {
        let mut rows = world.rows_mut().unwrap();
        rows.agents.get_mut("a1").unwrap().activity = Activity::Queued;
        let mut row = world.row_mut::<MovementRow>(MOVEMENT).unwrap();
        row.journeys.get_mut("a1").unwrap().purpose = text("toilet_queue_position");
        let walking = row.positions.get_mut("a1").unwrap();
        walking.edge_id = Some(text("e1"));
        walking.progress = 0.75;
    }
    assert!(done(advance(&world, &geo(), &StaffOps::None, 1).unwrap()).is_empty());
    let row = row(&world);
    assert!(row.journeys.is_empty());
    let finished = &row.positions["a1"];
    assert_eq!((xy(finished), finished.edge_id.as_deref(), finished.progress), (Some((5.0, 5.0)), None, 0.0));
}

// ---- staff_position_snapshots -------------------------------------------------------------------

fn staff_world(started: Option<i64>) -> World {
    let mut world = world();
    world.rows_mut().unwrap().staff.get_mut("s1").unwrap().zone_id = text("z0");
    let mut staff = IdMap::default();
    staff.insert(text("s1"), OperationRecord {
        phase: Phase::Travel,
        task_id: Some(text("cleaning-1")),
        task_kind: Some(text("cleaning")),
        target_id: None,
        target_zone_id: Some(text("z1")),
        reason: Some(text("sanitation")),
        started_minute: started,
        travel_due_minute: Some(4),
        arrival_minute: None,
        due_minute: None,
        route_zone_ids: vec![text("z0")],
        route_edge_ids: vec![text("e1")],
        travel_minutes: 4,
        causation_event_ids: Vec::new(),
    });
    add(&mut world, STAFF_OPERATIONS, Place::Namespace, Box::new(StaffOperationsRow {
        schema_version: 1,
        policy_version: text("autonomous-routed-staff-v1"),
        source_id: text("ASM-STAFF-OPERATIONS-V1"),
        config: V::Map(IndexMap::new()),
        zones: IdMap::default(),
        staff,
        history: Vec::new(),
    }));
    world
}

#[test]
fn a_travelling_staff_member_is_on_their_route() {
    let mut world = staff_world(Some(0));
    let records = movement::staff_position_snapshots(&world, &geo(), &StaffOps::Row, 2).unwrap();
    assert_eq!(records.len(), 1);
    let (id, record) = &records[0];
    assert_eq!((&**id, xy(record), record.edge_id.as_deref(), record.progress, &*record.zone_id), ("s1", Some((10.0, 5.0)), Some("e1"), 0.5, "z0"));
    assert_eq!(xy(&row(&world).staff_positions["s1"]), Some((10.0, 5.0)));
    assert_eq!(written(&mut world), [text(MOVEMENT)]);
    // Past the travel minutes: progress 1.0 on the last edge, in the zone it ends in. The edge is
    // read from that zone, so backwards, as the Python rule reads it: its point is the far end.
    let records = movement::staff_position_snapshots(&world, &geo(), &StaffOps::Row, 9).unwrap();
    assert_eq!((xy(&records[0].1), records[0].1.progress, &*records[0].1.zone_id), (Some((5.0, 5.0)), 1.0, "z1"));
    assert_eq!(written(&mut world), [text(MOVEMENT)]);
    // The same records once more: the slot is not written.
    movement::staff_position_snapshots(&world, &geo(), &StaffOps::Row, 9).unwrap();
    assert!(written(&mut world).is_empty());
}

#[test]
fn a_travel_record_the_kernel_does_not_read_is_refused() {
    let world = staff_world(None);
    assert_eq!(movement::staff_position_snapshots(&world, &geo(), &StaffOps::Row, 2).err(), Some(unsupported("a value is not an int")));
    // The tree's records, where the row does not answer: an edge ID that is not a str is refused
    // only where the walk reaches it.
    let travel = |edges: Vec<Option<Arc<str>>>| {
        let mut records = IdMap::default();
        records.insert(text("s1"), StaffOp::Travel(StaffTravel { travel_minutes: 4, started_minute: 0, route_edge_ids: edges, route_zone_ids: vec![Some(text("z0"))] }));
        StaffOps::Tree(records)
    };
    let world = staff_world(Some(0));
    let found = movement::staff_position_snapshots(&world, &geo(), &travel(vec![Some(text("e1")), None]), 2).unwrap();
    assert_eq!(xy(&found[0].1), Some((10.0, 5.0)));
    let refused = movement::staff_position_snapshots(&world, &geo(), &travel(vec![None]), 2);
    assert_eq!(refused.err(), Some(unsupported("a value is not a str")));
    let mut records = IdMap::default();
    records.insert(text("s1"), StaffOp::Refused("a staff operation is not a plain dict"));
    let refused = movement::staff_position_snapshots(&world, &geo(), &StaffOps::Tree(records), 2);
    assert_eq!(refused.err(), Some(unsupported("a staff operation is not a plain dict")));
}

// ---- start_movement, finish_movement and the places --------------------------------------------

#[test]
fn a_journey_starts_at_its_origin_or_one_minute_along_it() {
    let mut world = world();
    world.row_mut::<MovementRow>(MOVEMENT).unwrap().journeys.clear();
    let geo = geo();
    let start = |world: &World, started| {
        let (agent, origin) = (text("a1"), text("z0"));
        let edges = [text("e1")];
        let journey = Journey { started_minute: started, due_minute: started + 4, ..journey("travel") };
        let start = Start { agent_id: &agent, journey, origin: &origin, points: &[], edge_ids: &edges, started, duration: 4 };
        movement::start(world, &geo, start).unwrap();
        row(world).positions["a1"].clone()
    };
    // Movement was last advanced at minute 0, so a journey starting at 0 is a minute along.
    let along = start(&world, 0);
    assert_eq!((xy(&along), along.progress, along.edge_id.as_deref()), (Some((7.5, 5.0)), 0.25, Some("e1")));
    let at_start = start(&world, 1);
    assert_eq!((xy(&at_start), at_start.progress), (Some((5.0, 5.0)), 0.0));
    assert!(row(&world).journeys.contains_key("a1"));
    assert_eq!(written(&mut world), [text(MOVEMENT)]);
}

#[test]
fn finishing_a_queue_trip_keeps_the_arrival_and_any_other_journey_stands_still() {
    let world = world();
    world.row_mut::<MovementRow>(MOVEMENT).unwrap().journeys.get_mut("a1").unwrap().purpose = text("toilet_queue_service");
    world.row_mut::<MovementRow>(MOVEMENT).unwrap().positions.get_mut("a1").unwrap().progress = 0.5;
    movement::finish(&world, &geo(), "a1").unwrap();
    let finished = row(&world).positions["a1"].clone();
    assert_eq!((xy(&finished), finished.progress), (Some((5.0, 5.0)), 0.0));
    assert!(row(&world).journeys.is_empty());
    // Another trip: the person stands at the centre of the zone they are in now.
    let world = self::world();
    world.rows_mut().unwrap().agents.get_mut("a1").unwrap().zone_id = text("z1");
    movement::finish(&world, &geo(), "a1").unwrap();
    assert_eq!((&*row(&world).positions["a1"].zone_id, xy(&row(&world).positions["a1"])), ("z1", Some((15.0, 5.0))));
}

#[test]
fn a_bed_place_is_written_only_where_it_changes_the_record() {
    let mut world = world();
    let zone = text("z1");
    assert!(!movement::place_at_point(&world, "nobody", &zone, Pt { x: 12.0, y: 3.0 }).unwrap());
    assert!(written(&mut world).is_empty());
    assert!(movement::place_at_point(&world, "a2", &zone, Pt { x: 12.0, y: 3.0 }).unwrap());
    let placed = row(&world).positions["a2"].clone();
    assert_eq!((xy(&placed), placed.progress, &*placed.position_kind), (Some((12.0, 3.0)), 1.0, "physical"));
    assert_eq!(row(&world).positions.keys().map(|id| &**id).collect::<Vec<_>>(), ["a1", "a2"]);
    assert_eq!(written(&mut world), [text(MOVEMENT)]);
    assert!(movement::place_at_point(&world, "a2", &zone, Pt { x: 12.0, y: 3.0 }).unwrap());
    assert!(written(&mut world).is_empty());
}

#[test]
fn a_sleeper_stands_at_the_reserved_bed_of_the_overlay() {
    let mut world = world();
    world.rows_mut().unwrap().agents.get_mut("a2").unwrap().activity = Activity::Sleeping;
    let overlay = map(vec![
        ("bed_reservations", map(vec![("a2", map(vec![("room_id", V::Str(text("r1"))), ("bed_index", V::Int(1))]))])),
        ("temporary_rooms", map(vec![("r1", map(vec![("bed_points_m", V::List(vec![xy_value(11.0, 1.0), xy_value(12.0, 2.0)]))]))])),
    ]);
    add(&mut world, movement::OVERLAY_KEY, Place::Namespace, Box::new(ValueRow(overlay)));
    assert!(done(advance(&world, &geo(), &StaffOps::None, 1).unwrap()).is_empty());
    assert_eq!(xy(&row(&world).positions["a2"]), Some((12.0, 2.0)));
}

// ---- Where a person is ---------------------------------------------------------------------------

#[test]
fn a_person_near_their_trips_end_is_there() {
    let world = world();
    {
        let mut row = world.row_mut::<MovementRow>(MOVEMENT).unwrap();
        row.journeys.get_mut("a1").unwrap().route_points = vec![Point { x: 1.0, y: 1.0 }, Point { x: 5.03, y: 5.0 }];
    }
    assert!(movement::at_trip_destination(&world, &Held::Held, "a1").unwrap());
    assert!(!movement::near_point(&world, &Held::Held, "a1", Pt { x: 5.1, y: 5.0 }).unwrap());
    // Without the namespace nobody has a place yet.
    assert!(!movement::at_trip_destination(&world, &Held::Absent, "a1").unwrap());
    assert_eq!(
        movement::at_trip_destination(&world, &Held::Other("the world holds another movement state"), "a1").err(),
        Some(unsupported("the world holds another movement state"))
    );
    let geo = geo();
    let at = Where { geo: &geo, movement: Held::Held, queues: Held::Absent };
    assert_eq!(movement::agent_and_facility(&world, "a1", "f1").err(), Some(unsupported("no such agent or facility")));
    assert!(!movement::at_queue_slot(&world, &at, "a1", "f1", None).unwrap());
    assert_eq!(movement::queue_slot_position(&world, &at, "a1", "f1", None).unwrap(), None);
}

// ---- route_with_movement_time --------------------------------------------------------------------

#[test]
fn a_routes_minutes_are_slowed_by_the_most_crowded_zone() {
    let world = world();
    world.rows_mut().unwrap().agents.get_mut("a2").unwrap().zone_id = text("z0");
    let model = MovementModel::default();
    // Two people on 2 square metres: 1.0 a square metre, so 1 + 0.12 * 0.5 slower.
    let geo = geo_with(2.0);
    assert_eq!(movement::movement_minutes(&world, &geo, &model, "a1", &["z0", "z1"], &["e1"], 2).unwrap(), 3);
    // 10 m at 72 m a minute is under the declared minute, which the route keeps.
    assert_eq!(movement::movement_minutes(&world, &geo_with(100.0), &model, "a1", &["z0", "z1"], &["e1"], 1).unwrap(), 1);
    // No edges: the declared minutes.
    assert_eq!(movement::movement_minutes(&world, &geo, &model, "a1", &["z0"], &[], 7).unwrap(), 7);
    assert_eq!(movement::movement_minutes(&world, &geo, &model, "nobody", &["z0"], &["e1"], 2).err(), Some(unsupported("no such agent")));
}

// ---- validate ------------------------------------------------------------------------------------

#[test]
fn the_row_is_checked_against_the_rows_of_the_world() {
    let world = world();
    // a1's position for every agent, s1's for the staff.
    let mut staff = IdMap::default();
    staff.insert(text("s1"), position("z1", 15.0, 5.0));
    world.row_mut::<MovementRow>(MOVEMENT).unwrap().staff_positions = staff;
    assert_eq!(movement::validate(&world, &Held::Absent, None).unwrap(), Validated::Is(true));
    world.row_mut::<MovementRow>(MOVEMENT).unwrap().positions.get_mut("a2").unwrap().zone_id = text("nowhere");
    assert_eq!(movement::validate(&world, &Held::Absent, None).unwrap(), Validated::Is(false));
    world.row_mut::<MovementRow>(MOVEMENT).unwrap().positions.get_mut("a2").unwrap().zone_id = text("z1");
    // A journey in a room needs the floor plan, asked for before any check.
    world.row_mut::<MovementRow>(MOVEMENT).unwrap().journeys.get_mut("a1").unwrap().route_points =
        vec![Point { x: 1.0, y: 1.0 }, Point { x: 2.0, y: 2.0 }];
    assert_eq!(movement::validate(&world, &Held::Absent, None).unwrap(), Validated::NeedsGeo);
    // Its points must be in the person's room, and its edges none.
    assert_eq!(movement::validate(&world, &Held::Absent, Some(&geo())).unwrap(), Validated::Is(false));
    world.row_mut::<MovementRow>(MOVEMENT).unwrap().journeys.get_mut("a1").unwrap().route_edge_ids.clear();
    assert_eq!(movement::validate(&world, &Held::Absent, Some(&geo())).unwrap(), Validated::Is(true));
    // A queue ID without the queues.
    world.row_mut::<MovementRow>(MOVEMENT).unwrap().journeys.get_mut("a1").unwrap().queue_id = Some(text("q1"));
    world.row_mut::<MovementRow>(MOVEMENT).unwrap().journeys.get_mut("a1").unwrap().purpose = text("toilet_queue_position");
    assert_eq!(movement::validate(&world, &Held::Absent, Some(&geo())).unwrap(), Validated::Is(false));
}

// ---- The overlay's key ---------------------------------------------------------------------------

#[test]
fn the_overlays_key_encodes_only_the_parts_with_entries() {
    let overlay = map(vec![
        ("facility_points", map(Vec::new())),
        ("partitions", map(vec![("p1", map(vec![("minute", V::Int(3))]))])),
        ("bed_reservations", map(Vec::new())),
    ]);
    let key = OverlayKey::of(Some(&overlay), None).unwrap();
    assert!(key.present && key.moves.is_empty());
    assert_eq!(key.partitions, br#"{"p1":{"minute":3}}"#);
    let (moves, partitions) = key.parts(Some(&overlay));
    assert_eq!((moves, partitions.map(|part| part.to_json().unwrap())), (None, Some(key.partitions.clone())));
    let absent = OverlayKey::of(None, None).unwrap();
    assert!(!absent.present && absent.moves.is_empty() && absent.partitions.is_empty());
    assert_eq!(OverlayKey::of(Some(&V::Int(1)), None).err(), Some(unsupported("a value is not a plain dict")));
}
