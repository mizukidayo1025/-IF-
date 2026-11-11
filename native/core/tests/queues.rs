//! Lane c1: the queues of a facility (`rules::queues`) and the independent FIFO of each service
//! point (`rules::facility_queues`), on a world built here with no Python. The PyO3 crate's
//! kernels of the same names are glue over these, so the gate and the offline tests run them on
//! every Run; these say what each reads, decides and writes.

use std::sync::Arc;

use indexmap::IndexMap;
use sheltersim_core::config::geometry::FacilityPlan;
use sheltersim_core::config::movement::MovementModel;
use sheltersim_core::config::queues::QueuePolicy;
use sheltersim_core::entities::{Activity, AgentRow, AnyTable, Entities, FacilityRow, Mobility, NeedRow, ProfileRow, Table, TaskRow};
use sheltersim_core::error::Error;
use sheltersim_core::events::log::EventLog;
use sheltersim_core::events::queue::Queue;
use sheltersim_core::geometry::{zone_center, Geo, Pt, ServicePoint, Zone};
use sheltersim_core::row::{IdMap, ValueRow};
use sheltersim_core::rules::facility_queues as fifo;
use sheltersim_core::rules::kit::movement::{self as walking, Changes};
use sheltersim_core::rules::kit::Held;
use sheltersim_core::rules::queues::{self, LeaveInputs, Prepare, Prepared, Walk};
use sheltersim_core::state::facility_queues::{QueueRequest, QueuesRow};
use sheltersim_core::state::metric_state::MetricRow;
use sheltersim_core::state::movement::{MovementRow, Point, Position};
use sheltersim_core::value::V;
use sheltersim_core::world::{Fit, Place, World, ID_COUNTERS, METRIC_STATE};

const TOILET: &str = "toilet_01";
const MOVEMENT: &str = "movement_v1";

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

fn unsupported(reason: &str) -> Error {
    Error::Unsupported(reason.to_owned().into())
}

fn needs(toilet: f64) -> NeedRow {
    NeedRow { toilet_urgency: toilet, hunger: 0.0, fatigue: 0.0, hydration_need: 0.0 }
}

fn agent(id: &str, toilet: f64, activity: Activity) -> AgentRow {
    AgentRow {
        profile: ProfileRow {
            id: text(id),
            group_id: None,
            initial_zone_id: text("z0"),
            mobility: Mobility::Independent,
            support_need: 0.0,
            stress_sensitivity: 1.0,
            rng_key: text(id),
            initial_needs: needs(0.0),
            provenance: V::Map(IndexMap::new()),
        },
        zone_id: text("z0"),
        activity,
        target_id: (activity == Activity::Queued).then(|| text(TOILET)),
        needs: needs(toilet),
        stress: 0.0,
        trust: 0.5,
        next_decision_at: 0,
        support_request_id: None,
        support_cooldown_until: 0,
    }
}

fn task(id: &str, agent_id: &str, start: i64) -> TaskRow {
    TaskRow {
        id: text(id),
        kind: text("toilet_queue"),
        agent_id: Some(text(agent_id)),
        resource_id: Some(text(TOILET)),
        start_minute: start,
        due_minute: start,
        staff_id: None,
    }
}

fn put<R: Clone>(table: &mut Table<R>, id: &str, row: R) {
    let slot = table.reserve_slot();
    table.fill_slot(slot, row);
    assert_eq!(table.bind(text(id), slot), None);
}

/// Three people queued at a toilet, in this order, with the needs given.
fn entities(urgencies: [f64; 3]) -> Entities {
    let mut entities = Entities::default();
    for (index, urgency) in urgencies.into_iter().enumerate() {
        let id = format!("a{}", index + 1);
        put(&mut entities.agents, &id, agent(&id, urgency, Activity::Queued));
        put(&mut entities.active_tasks, &format!("t{}", index + 1), task(&format!("t{}", index + 1), &id, index as i64));
    }
    put(&mut entities.facilities, TOILET, FacilityRow {
        id: text(TOILET),
        kind: text("toilet"),
        zone_id: text("z0"),
        capacity: 1,
        working_units: 1,
        active_agent_ids: Vec::new(),
        queue_agent_ids: vec![text("a1"), text("a2"), text("a3")],
        resource_active_agent_ids: Vec::new(),
        resource_queue_agent_ids: Vec::new(),
    });
    entities
}

fn point(id: &str, queue_id: &str, x: f64) -> ServicePoint {
    ServicePoint {
        id: text(id),
        facility_id: text(TOILET),
        point: Pt { x, y: 2.0 },
        service_capacity: 1,
        queue_id: text(queue_id),
        accessibility: text("unknown"),
        required_access_tags: Vec::new(),
        required_staff_skills: Vec::new(),
    }
}

/// A room of 20 m by 20 m, its centre at (10, 10), the toilet at (10, 2) with two signed points.
fn geo() -> Geo {
    let polygon = vec![Pt { x: 0.0, y: 0.0 }, Pt { x: 20.0, y: 0.0 }, Pt { x: 20.0, y: 20.0 }, Pt { x: 0.0, y: 20.0 }];
    let center = zone_center(&polygon);
    let mut zones = IdMap::default();
    zones.insert(text("z0"), Zone { polygon, usable_area: 400.0, center });
    let mut facility_points = IdMap::default();
    facility_points.insert(text(TOILET), Pt { x: 10.0, y: 2.0 });
    let service_points = vec![point("sp-right", "q-right", 12.0), point("sp-left", "q-left", 8.0)];
    let plan = FacilityPlan { zones, routes: IdMap::default(), facility_points, service_points };
    Geo::new(Some(&plan), None, None, None).unwrap()
}

fn position(x: f64, y: f64) -> Position {
    Position { zone_id: text("z0"), point: Some(Point { x, y }), edge_id: None, progress: 0.0, position_kind: text("physical") }
}

/// A world of 100 minutes at minute `minute`, holding its queue, log, `id_counters` (the event
/// counter at 10), `metric_state` and the rows of `movement_v1` (each person standing in the
/// middle of the room) and `facility_queues_v1` (empty).
fn world(urgencies: [f64; 3], minute: i64) -> World {
    let mut world = World::default();
    world.scalars.get_mut().duration_minutes = Fit::Ok(100);
    world.scalars.get_mut().minute_index = Fit::Ok(minute);
    *world.entities.get_mut() = entities(urgencies);
    let mut fill = |name: &str, place, row: Box<dyn sheltersim_core::row::Row>| {
        let index = world.slots.add(text(name), place).unwrap();
        world.slots.at(index).unwrap().try_borrow_mut().unwrap().row = Some(row);
    };
    let mut counters = IndexMap::new();
    counters.insert(text("event"), V::Int(10));
    fill(ID_COUNTERS, Place::Field, Box::new(ValueRow(V::Map(counters))));
    fill(METRIC_STATE, Place::Field, Box::new(MetricRow::default()));
    let mut positions = IdMap::default();
    for id in ["a1", "a2", "a3"] {
        positions.insert(text(id), position(10.0, 10.0));
    }
    fill(MOVEMENT, Place::Namespace, Box::new(MovementRow {
        schema_version: 2,
        policy_version: text(walking::POLICY_VERSION),
        positions,
        staff_positions: IdMap::default(),
        journeys: IdMap::default(),
        edge_usage_minutes: IdMap::default(),
        last_updated_minute: minute,
    }));
    fill(fifo::STATE_KEY, Place::Namespace, Box::new(fifo::create_state(&geo()).unwrap()));
    world.hold_queue(Arc::new(Queue::new()));
    world.hold_log(Arc::new(EventLog::new()));
    world
}

fn xy(points: &[Point]) -> Vec<(f64, f64)> {
    points.iter().map(|point| (point.x, point.y)).collect()
}

fn counter(world: &World) -> i64 {
    world.id_counter("event").unwrap().unwrap()
}

fn written(world: &mut World) -> Vec<Arc<str>> {
    let mut names = world.slots.written_names();
    names.sort();
    world.slots.take_written();
    names
}

// ---- facility_queues -----------------------------------------------------------------------------

#[test]
fn the_state_has_a_queue_per_signed_point_by_queue_id() {
    let state = fifo::create_state(&geo()).unwrap();
    assert_eq!(state.queues.keys().map(|id| &**id).collect::<Vec<_>>(), ["q-left", "q-right"]);
    assert_eq!(&*state.queues["q-left"].service_point_id, "sp-left");
    assert_eq!(fifo::queue_ids(&geo(), TOILET), ["q-left", "q-right"]);
    assert_eq!(fifo::create_state(&Geo::new(None, None, None, None).unwrap()).err(), Some(unsupported("no floor plan")));
}

#[test]
fn the_fifo_is_written_on_the_row_and_marks_its_slot() {
    let mut world = world([0.5; 3], 4);
    written(&mut world);
    let geo = geo();
    let left = fifo::point_of(&geo, "q-left").unwrap().clone();
    fifo::write_enqueue(&world, &left, "t1", "t1", "a1", "toilet", 1).unwrap();
    fifo::write_enqueue(&world, &left, "t2", "t2", "a2", "toilet", 2).unwrap();
    assert_eq!(written(&mut world), [text(fifo::STATE_KEY)]);
    {
        let row = fifo::queues(&world, &Held::Held).unwrap().unwrap();
        assert_eq!(fifo::members_of(&row, "q-left").unwrap(), [text("a1"), text("a2")]);
        assert_eq!(fifo::independent_heads(&row, TOILET).unwrap(), Some(vec![text("a1")]));
        assert_eq!(fifo::position_in(&row.queues["q-left"], "t2"), Some(2));
        assert!(fifo::request_of(&row, "a2", Some("q-right")).is_none());
        assert_eq!(fifo::request_of(&row, "a2", None).map(|request| &*request.request_id), Some("t2"));
    }
    // The second goes to the head, then the first starts, and its stall is cleared.
    fifo::write_to_head(&world, "q-left", "t2").unwrap();
    fifo::write_stall(&world, "q-left", "t2", "staff").unwrap();
    {
        let row = fifo::queues(&world, &Held::Held).unwrap().unwrap();
        assert_eq!(row.queues["q-left"].queued_request_ids, [text("t2"), text("t1")]);
        let stall = row.queues["q-left"].last_stall.as_ref().unwrap();
        assert_eq!((stall.minute, &*stall.request_id, &*stall.reason), (4, "t2", "staff"));
    }
    fifo::write_start(&world, "t1", "q-left", "service-1").unwrap();
    let request = {
        let row = fifo::queues(&world, &Held::Held).unwrap().unwrap();
        assert_eq!(row.queues["q-left"].active_request_ids, [text("t1")]);
        assert!(row.queues["q-left"].last_stall.is_none());
        assert_eq!(fifo::request_of_task(&row, "service-1").map(|request| &*request.status), Some("active"));
        row.requests["t1"].clone()
    };
    fifo::write_finish(&world, &request).unwrap();
    let row = fifo::queues(&world, &Held::Held).unwrap().unwrap();
    assert!(row.queues["q-left"].active_request_ids.is_empty());
    assert_eq!(row.requests.keys().map(|id| &**id).collect::<Vec<_>>(), ["t2"]);
    drop(row);
    // What the Python lists say of an ID they don't hold.
    assert_eq!(fifo::write_to_head(&world, "q-left", "t9"), Err(Error::Value("list.remove(x): x not in list".into())));
    assert_eq!(fifo::write_to_head(&world, "q-none", "t2"), Err(Error::Key("q-none".into())));
    assert_eq!(fifo::write_stall(&world, "q-none", "t2", "staff"), Err(unsupported("no such queue")));
}

#[test]
fn a_world_without_the_namespace_or_holding_another_says_so_where_it_reads_it() {
    let world = world([0.5; 3], 0);
    assert!(fifo::queues(&world, &Held::Absent).unwrap().is_none());
    assert_eq!(fifo::queues(&world, &Held::Other("the world holds another queue state")).err(), Some(unsupported("the world holds another queue state")));
    assert_eq!(fifo::heads_in(&world, &Held::Absent, TOILET), Ok(None));
    // Without independent heads, the first of the shared resource queue (empty here).
    assert_eq!(fifo::head_agents(&world, &Held::Absent, TOILET), Ok(Vec::new()));
}

#[test]
fn the_least_loaded_open_queue_is_chosen_then_the_smaller_id() {
    let world = world([0.5; 3], 0);
    let geo = geo();
    assert_eq!(fifo::choose_queue(&world, &Held::Held, &geo, TOILET, "a1", &[]).unwrap().as_deref(), Some("q-left"));
    let left = fifo::point_of(&geo, "q-left").unwrap().clone();
    fifo::write_enqueue(&world, &left, "t1", "t1", "a1", "toilet", 0).unwrap();
    assert_eq!(fifo::choose_queue(&world, &Held::Held, &geo, TOILET, "a2", &[]).unwrap().as_deref(), Some("q-right"));
    assert_eq!(fifo::choose_queue(&world, &Held::Absent, &geo, TOILET, "a2", &[]), Ok(None));
    // A restricted point needs all its tags: the person's needs and mobility.
    let mut plan_geo = geo;
    if let Some(layout) = &mut plan_geo.layout {
        for point in &mut layout.signed_service_points {
            if &*point.queue_id == "q-right" {
                point.accessibility = text("restricted");
                point.required_access_tags = vec![text("wheelchair_access")];
            }
        }
    }
    assert_eq!(fifo::choose_queue(&world, &Held::Held, &plan_geo, TOILET, "a2", &[]).unwrap().as_deref(), Some("q-left"));
    let tags = [text("wheelchair_access")];
    assert_eq!(fifo::choose_queue(&world, &Held::Held, &plan_geo, TOILET, "a2", &tags).unwrap().as_deref(), Some("q-right"));
}

fn request(transition_wait: i64) -> QueueRequest {
    QueueRequest {
        request_id: text("t1"),
        queue_id: text("q-left"),
        service_point_id: text("sp-left"),
        facility_id: text(TOILET),
        agent_id: text("a1"),
        service_kind: text("toilet"),
        requested_minute: transition_wait,
        status: text("queued"),
        queue_task_id: text("t1"),
        active_task_id: text("t1"),
    }
}

#[test]
fn a_transition_payload_is_the_receipt_s_and_what_its_contract_refuses_is_refused() {
    let payload = fifo::transition_payload(false, &request(3), "queued", "t1", None, None, Some(1), Some(1), 12, 5).unwrap();
    let keys: Vec<&str> = payload.keys().map(|key| &**key).collect();
    assert_eq!(
        keys,
        [
            "schema_version", "record_type", "event_type", "event_id", "request_id", "queue_id", "service_point_id", "facility_id",
            "agent_id", "service_kind", "transition", "minute", "requested_minute", "task_id", "queue_position",
            "service_capacity_concurrent", "wait_minutes", "causation_id",
        ]
    );
    assert_eq!(payload["event_id"], V::Str(text("facility-queue-transition-12")));
    let event = fifo::transition_event(payload, Some("cause-1"), 12, 5).unwrap();
    assert_eq!((&*event.event_id, event.sequence, event.due_minute), ("facility-queue-transition-12", 12, 5));
    assert_eq!(event.causation_id.as_deref(), Some("cause-1"));

    let refused = Err(unsupported("the receipt's contract refuses it"));
    // Queued without a place, a request from the future, a wait longer than the time since it,
    // an ID that is not an Identifier, a transition the contract doesn't name.
    assert_eq!(fifo::transition_payload(false, &request(3), "requeued", "t1", None, None, None, None, 12, 5).map(|_| ()), refused);
    assert_eq!(fifo::transition_payload(false, &request(6), "completed", "t1", None, None, None, None, 12, 5).map(|_| ()), refused);
    assert_eq!(fifo::transition_payload(false, &request(3), "completed", "t1", Some(3), None, None, None, 12, 5).map(|_| ()), refused);
    assert_eq!(fifo::transition_payload(false, &request(3), "completed", "t 1", None, None, None, None, 12, 5).map(|_| ()), refused);
    assert_eq!(fifo::transition_payload(false, &request(3), "lost", "t1", None, None, None, None, 12, 5).map(|_| ()), refused);
    assert_eq!(fifo::transition_payload(false, &request(3), "queued", "t1", None, None, Some(0), None, 12, 5).map(|_| ()), refused);
    // A wait of all the time since the request, and a cause, are taken.
    assert!(fifo::transition_payload(false, &request(3), "completed", "t1", Some(2), Some("e-1"), None, Some(0), 12, 5).is_ok());
    assert_eq!(
        fifo::transition_payload(true, &request(3), "completed", "t1", None, None, None, None, 12, 5).map(|_| ()),
        Err(unsupported("the record's identity is taken"))
    );
}

#[test]
fn a_recorded_transition_takes_the_next_sequence() {
    let world = world([0.5; 3], 5);
    let geo = geo();
    let left = fifo::point_of(&geo, "q-left").unwrap().clone();
    fifo::write_enqueue(&world, &left, "t1", "t1", "a1", "toilet", 3).unwrap();
    let request = fifo::queues(&world, &Held::Held).unwrap().unwrap().requests["t1"].clone();
    let recording = fifo::plan_record(&world, &Held::Held, &geo, &request, "queued", "t1", None, None).unwrap();
    assert_eq!((recording.sequence, recording.minute), (10, 5));
    assert_eq!(recording.payload["queue_position"], V::Int(1));
    assert_eq!(recording.payload["service_capacity_concurrent"], V::Int(1));
    fifo::write_record(&world, recording, None).unwrap();
    assert_eq!(counter(&world), 11);
    assert!(world.log().unwrap().has_event_id("facility-queue-transition-10"));
}

#[test]
fn the_validator_passes_a_consistent_fifo_and_turns_down_a_lost_request() {
    let world = world([0.5; 3], 5);
    let geo = geo();
    assert_eq!(fifo::validate(&world, &Held::Held, &geo, &Held::Held), Ok(true));
    let left = fifo::point_of(&geo, "q-left").unwrap().clone();
    fifo::write_enqueue(&world, &left, "t1", "t1", "a1", "toilet", 3).unwrap();
    assert_eq!(fifo::validate(&world, &Held::Held, &geo, &Held::Held), Ok(true));
    // A request of a queued person whose task is not a queue task.
    world.rows_mut().unwrap().active_tasks.get_mut("t1").unwrap().kind = text("toilet_service");
    assert_eq!(fifo::validate(&world, &Held::Held, &geo, &Held::Held), Ok(false));
    world.rows_mut().unwrap().active_tasks.get_mut("t1").unwrap().kind = text("toilet_queue");
    // A request no queue lists.
    world.row_mut::<QueuesRow>(fifo::STATE_KEY).unwrap().queues.get_mut("q-left").unwrap().queued_request_ids.clear();
    assert_eq!(fifo::validate(&world, &Held::Held, &geo, &Held::Held), Ok(false));
}

// ---- prepare_facility_queue ----------------------------------------------------------------------

fn need_weighted() -> QueuePolicy {
    QueuePolicy::NeedWeighted { limits: [30, 60, 60], threshold: 0.3, retry: 15 }
}

fn prepare(policy: QueuePolicy, minute: i64, movement: Held) -> Prepare<'static> {
    Prepare {
        resource: false,
        minute,
        policy,
        movement,
        queues: Held::Absent,
        service: Held::Absent,
        tree_stall: None,
        tree_stall_removed: Default::default(),
        kind_repr: None,
    }
}

fn queue_of(world: &World) -> Vec<Arc<str>> {
    world.rows().unwrap().facilities.get(TOILET).unwrap().queue_agent_ids.clone()
}

#[test]
fn the_need_weighted_policy_orders_by_need_then_start_then_place() {
    let mut world = world([0.1, 0.9, 0.5], 10);
    assert_eq!(queues::queued_kind(&world, TOILET, false).unwrap().as_deref(), Some("toilet"));
    assert_eq!(queues::queued_kind(&world, TOILET, true), Ok(None));
    assert_eq!(queues::queued_kind(&world, "none", false), Ok(None));
    written(&mut world);
    let done = queues::prepare(&world, TOILET, &prepare(need_weighted(), 10, Held::Held), None);
    assert_eq!(done, Ok(Prepared::Done));
    assert_eq!(queue_of(&world), [text("a2"), text("a3"), text("a1")]);
    // The facility's row alone was written: no slot.
    assert!(written(&mut world).is_empty());
    // The strict policy keeps the order of the starts.
    let world = self::world([0.1, 0.9, 0.5], 10);
    queues::prepare(&world, TOILET, &prepare(QueuePolicy::Strict, 10, Held::Held), None).unwrap();
    assert_eq!(queue_of(&world), [text("a1"), text("a2"), text("a3")]);
}

#[test]
fn someone_who_walks_to_the_queue_leaves_it_alone_and_a_strange_kind_raises_the_rule_s_error() {
    let world = world([0.1, 0.9, 0.5], 10);
    let mut changes = Changes::default();
    let journey = sheltersim_core::state::movement::Journey {
        event_id: text("e"),
        task_id: text("t1"),
        purpose: text("toilet_queue_position"),
        facility_id: Some(text(TOILET)),
        destination_zone_id: text("z0"),
        route_zone_ids: vec![text("z0"), text("z0")],
        route_edge_ids: Vec::new(),
        route_points: vec![Point { x: 10.0, y: 10.0 }, Point { x: 10.0, y: 3.0 }],
        started_minute: 10,
        due_minute: 11,
        queue_id: None,
        support_request_id: None,
        support_staff_id: None,
    };
    changes.new_journeys.push((text("a1"), journey));
    changes.apply(&mut world.row_mut::<MovementRow>(MOVEMENT).unwrap());
    queues::prepare(&world, TOILET, &prepare(need_weighted(), 10, Held::Held), None).unwrap();
    assert_eq!(queue_of(&world), [text("a1"), text("a2"), text("a3")]);

    let world = self::world([0.1, 0.9, 0.5], 10);
    world.rows_mut().unwrap().facilities.get_mut(TOILET).unwrap().kind = text("shower");
    let mut strange = prepare(need_weighted(), 10, Held::Absent);
    strange.kind_repr = Some("'shower'");
    assert_eq!(queues::prepare(&world, TOILET, &strange, None).map_err(|_| ()), Err(()));
    assert_eq!(queues::service_of("ﬆoilet_main"), queues::service_of("toilet"));
    assert_eq!(queues::service_of("shower"), None);
}

#[test]
fn someone_past_their_limit_gives_up_with_their_events_and_counts() {
    let mut world = world([0.1, 0.9, 0.5], 40);
    let call = prepare(need_weighted(), 40, Held::Absent);
    // It asks for the inputs of those who give up, having written nothing.
    assert_eq!(queues::prepare(&world, TOILET, &call, None), Ok(Prepared::NeedsLeaveInputs));
    assert_eq!(queue_of(&world), [text("a1"), text("a2"), text("a3")]);
    written(&mut world);
    let model = Ok(MovementModel::default());
    let leave = LeaveInputs { has_geometry: false, model: &model, geo: None, metric_state_held: true };
    assert_eq!(queues::prepare(&world, TOILET, &call, Some(&leave)), Ok(Prepared::Done));
    assert_eq!(queue_of(&world), [text("a2"), text("a3")]);
    let rows = world.rows().unwrap();
    assert!(rows.active_tasks.get("t1").is_none());
    let a1 = rows.agents.get("a1").unwrap();
    assert_eq!((a1.activity, a1.target_id.clone(), a1.next_decision_at), (Activity::Idle, None, 55));
    drop(rows);
    assert_eq!(counter(&world), 11);
    assert!(world.log().unwrap().has_event_id("facility-queue-abandon-10"));
    assert_eq!(world.metric_state().unwrap().get("facility_queue_abandon_count"), Some(&1.0));
    assert_eq!(written(&mut world), [text(ID_COUNTERS), text(METRIC_STATE)]);
}

#[test]
fn someone_who_gives_up_in_a_placed_world_walks_back_to_the_zone_s_middle() {
    let world = world([0.1, 0.9, 0.5], 40);
    world.row_mut::<MovementRow>(MOVEMENT).unwrap().positions.insert(text("a1"), position(10.0, 3.0));
    let call = prepare(need_weighted(), 40, Held::Held);
    let geo = geo();
    let model = Ok(MovementModel::default());
    let leave = LeaveInputs { has_geometry: true, model: &model, geo: Some(&geo), metric_state_held: true };
    queues::prepare(&world, TOILET, &call, Some(&leave)).unwrap();
    // The abandon takes 10, the walk 11: its task, its event and its journey.
    assert_eq!(counter(&world), 12);
    let rows = world.rows().unwrap();
    assert_eq!(rows.active_tasks.get("queue-abandon-travel-11").map(|task| &*task.kind), Some("travel_return"));
    assert_eq!(rows.agents.get("a1").map(|agent| (agent.activity, agent.target_id.clone())), Some((Activity::Moving, Some(text("z0")))));
    drop(rows);
    assert!(world.queue().unwrap().has_event("queue-abandon-arrival-11"));
    let movement = world.row::<MovementRow>(MOVEMENT).unwrap();
    let journey = &movement.journeys["a1"];
    assert_eq!((&*journey.purpose, journey.due_minute), ("queue_abandon", 41));
    assert_eq!(xy(&journey.route_points), [(10.0, 3.0), (10.0, 10.0)]);
}

// ---- The walks to the queue and to service -------------------------------------------------------

fn walk<'a>(geo: &'a Geo, model: &'a sheltersim_core::config::read::Read<MovementModel>, queues: Held) -> Walk<'a> {
    Walk { geo, model, movement: Held::Held, queues, metric_state_held: true }
}

static MODEL: sheltersim_core::config::read::Read<MovementModel> = Ok(MovementModel {
    independent: 72.0,
    assisted: 36.0,
    wheelchair: 48.0,
    density_slowdown: 0.12,
    maximum_slowdown: 2.5,
});

#[test]
fn the_head_walks_to_the_service_point_with_its_event_and_journey() {
    let mut world = world([0.5; 3], 5);
    written(&mut world);
    let geo = geo();
    assert_eq!(queues::head_to_service(&world, &walk(&geo, &MODEL, Held::Absent), TOILET, false, 5, None), Ok(true));
    // 8 m at 72 m a minute is one minute.
    let queue = world.queue().unwrap();
    assert!(queue.has_event("physical-queue-trip-10"));
    drop(queue);
    assert_eq!(counter(&world), 11);
    let movement = world.row::<MovementRow>(MOVEMENT).unwrap();
    let journey = &movement.journeys["a1"];
    assert_eq!((&*journey.purpose, &*journey.task_id, journey.due_minute), ("toilet_queue_service", "t1", 6));
    assert_eq!(xy(&journey.route_points), [(10.0, 10.0), (10.0, 2.0)]);
    drop(movement);
    assert_eq!(written(&mut world), [text(ID_COUNTERS), text(MOVEMENT)]);
    // While the head walks, the next call writes nothing.
    assert_eq!(queues::head_to_service(&world, &walk(&geo, &MODEL, Held::Absent), TOILET, false, 5, None), Ok(true));
    assert_eq!(counter(&world), 11);
}

#[test]
fn a_walk_past_the_end_of_the_run_is_counted_as_blocked() {
    let world = world([0.5; 3], 99);
    let geo = geo();
    assert_eq!(queues::head_to_service(&world, &walk(&geo, &MODEL, Held::Absent), TOILET, false, 99, None), Ok(true));
    assert_eq!(world.metric_state().unwrap().get(queues::BLOCKED_METRIC), Some(&1.0));
    assert_eq!(counter(&world), 10);
}

#[test]
fn the_first_not_in_their_slot_walks_there() {
    let world = world([0.5; 3], 5);
    let geo = geo();
    // The head stands at the facility's point, the second in the middle of the room.
    world.row_mut::<MovementRow>(MOVEMENT).unwrap().positions.insert(text("a1"), position(10.0, 2.0));
    assert_eq!(queues::positions(&world, &walk(&geo, &MODEL, Held::Absent), TOILET, false, 5, None), Ok(false));
    let movement = world.row::<MovementRow>(MOVEMENT).unwrap();
    let journey = &movement.journeys["a2"];
    assert_eq!((&*journey.purpose, journey.due_minute), ("toilet_queue_position", 6));
    // The slot behind the head, on the line to the zone's middle.
    assert_eq!(xy(&journey.route_points).last(), Some(&(10.0, 3.2)));
}

#[test]
fn an_independent_queue_walks_its_own_people_to_its_signed_point() {
    let world = world([0.5; 3], 5);
    let geo = geo();
    let left = fifo::point_of(&geo, "q-left").unwrap().clone();
    fifo::write_enqueue(&world, &left, "t2", "t2", "a2", "toilet", 1).unwrap();
    let found = queues::Fifo::read(&world, &Held::Held, &geo, "q-left").unwrap();
    assert_eq!(found.members, [text("a2")]);
    assert_eq!(found.slot_view().1, Some(Pt { x: 8.0, y: 2.0 }));
    assert_eq!(queues::Fifo::read(&world, &Held::Absent, &geo, "q-left").err(), Some(unsupported("a queue ID without the queues")));
    assert_eq!(queues::Fifo::read(&world, &Held::Held, &geo, "q-none").err(), Some(unsupported("no such queue")));
    // The service point is the signed one.
    assert_eq!(queues::head_to_service(&world, &walk(&geo, &MODEL, Held::Held), TOILET, false, 5, Some("q-left")), Ok(true));
    let movement = world.row::<MovementRow>(MOVEMENT).unwrap();
    let journey = &movement.journeys["a2"];
    assert_eq!(journey.queue_id.as_deref(), Some("q-left"));
    assert_eq!(xy(&journey.route_points).last(), Some(&(8.0, 2.0)));
}

#[test]
fn without_a_queue_task_the_rule_raises_its_own_error() {
    let world = world([0.5; 3], 5);
    let geo = geo();
    world.rows_mut().unwrap().active_tasks.remove_row("t1");
    let raised = queues::head_to_service(&world, &walk(&geo, &MODEL, Held::Absent), TOILET, false, 5, None);
    assert_eq!(
        raised.err().map(|error| format!("{error:?}")),
        Some(format!("{:?}", sheltersim_core::events::writer::WriteError::Error(Error::Value("physical queue reservation has no matching active queue task".into()))))
    );
    assert_eq!(counter(&world), 10);
}
