//! Lane b3: the staff's rules (`rules::staff`) on a world built here with no Python: the support
//! requests' assignment, the complaints that come due, the waits the metrics read, the zones'
//! sanitation, the routed tasks and their relief. The PyO3 crate's kernels of the same names are glue
//! over these, so the gate and the offline tests run them on every Run; these say what each reads,
//! decides, refuses and writes.

mod support;

use std::sync::Arc;

use indexmap::IndexMap;
use serde_json::Value;
use sheltersim_core::cell::Shard;
use sheltersim_core::config::movement::Movement;
use sheltersim_core::config::staff::StaffOperations;
use sheltersim_core::entities::{Activity, AnyTable, AgentRow, Entities, EdgeRow, Mobility, NeedRow, ProfileRow, RequestRow, RequestStatus, Skills, StaffRow, Table, TaskRow, ZoneRow};
use sheltersim_core::error::Error;
use sheltersim_core::events::log::EventLog;
use sheltersim_core::events::queue::Queue;
use sheltersim_core::events::writer::WriteError;
use sheltersim_core::routes::Router;
use sheltersim_core::row::{IdMap, ValueRow};
use sheltersim_core::rules::kit::{Held, Kept};
use sheltersim_core::rules::staff::{self, Assigned, Full, Inputs, Operations, Routed, Service, Step, Travel, WaitRoutes, OTHER_RESOLVER, OTHER_SCHEDULER};
use sheltersim_core::rules::staff_operation_events::{self as ops, FORMULA_UNREAD};
use sheltersim_core::rules::travel::{Graph, Routing, Walking};
use sheltersim_core::run::held::{Class, Held as Attr, Obj};
use sheltersim_core::run::services::staff_service;
use sheltersim_core::run::RouteCapacities;
use sheltersim_core::state::metric_state::MetricRow;
use sheltersim_core::state::staff_operations::{OperationRecord, Outcome, Phase, StaffOperationsRow, ZoneRecord};
use sheltersim_core::value::V;
use sheltersim_core::world::{Fit, Place, World, ID_COUNTERS, METRIC_STATE, MODEL_CONFIGS};
use support::{Call, TestHost};

const NAME: &str = "staff_operations_v1";
const NO_QUALIFIED: &str = "support_no_qualified_worker_request_minutes";
const ATTEMPT: &str = "support_assignment_metrics_minute";
const BUSY: &str = "support_wait_staff_busy_request_minutes";
const WAIT_REASON: &str = "no_available_qualified_staff";

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

fn map(entries: Vec<(&str, V)>) -> V {
    V::Map(entries.into_iter().map(|(key, value)| (text(key), value)).collect())
}

fn unsupported(reason: &str) -> Error {
    Error::Unsupported(reason.to_owned().into())
}

fn put<R: Clone>(table: &mut Table<R>, id: &str, row: R) {
    let slot = table.reserve_slot();
    table.fill_slot(slot, row);
    assert_eq!(table.bind(text(id), slot), None);
}

fn needs() -> NeedRow {
    NeedRow { toilet_urgency: 0.0, hunger: 0.0, fatigue: 0.0, hydration_need: 0.0 }
}

fn agent(id: &str, zone: &str, stress: f64) -> AgentRow {
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
        activity: Activity::SeekingHelp,
        target_id: None,
        needs: needs(),
        stress,
        trust: 0.5,
        next_decision_at: 0,
        support_request_id: None,
        support_cooldown_until: 0,
    }
}

fn member(id: &str, zone: &str, skills: &[&str]) -> StaffRow {
    StaffRow { id: text(id), zone_id: text(zone), skills: Skills(skills.iter().map(|skill| text(skill)).collect()), work_queue: Vec::new(), busy_until: 0, active_task_id: None }
}

/// Zones z0 and z1, edge e1 (z0 to z1, 4 minutes) and e2 back; a1 and a2 in z0; s1 (support) in
/// z1, s2 (cleaning) in z0, s3 (sanitation) in z1.
fn entities() -> Entities {
    let mut entities = Entities::default();
    for id in ["z0", "z1"] {
        put(&mut entities.zones, id, ZoneRow { id: text(id), name: text(id), capacity: None, display_x: None, display_y: None });
    }
    for (id, from, to) in [("e1", "z0", "z1"), ("e2", "z1", "z0")] {
        put(&mut entities.zone_edges, id, EdgeRow { id: text(id), from_zone_id: text(from), to_zone_id: text(to), travel_minutes: 4, closed: false });
    }
    put(&mut entities.agents, "a1", agent("a1", "z0", 0.2));
    put(&mut entities.agents, "a2", agent("a2", "z0", 0.3));
    put(&mut entities.staff, "s1", member("s1", "z1", &["support"]));
    put(&mut entities.staff, "s2", member("s2", "z0", &["cleaning"]));
    put(&mut entities.staff, "s3", member("s3", "z1", &["sanitation"]));
    entities
}

fn idle() -> OperationRecord {
    OperationRecord {
        phase: Phase::Idle,
        task_id: None,
        task_kind: None,
        target_id: None,
        target_zone_id: None,
        reason: None,
        started_minute: None,
        travel_due_minute: None,
        arrival_minute: None,
        due_minute: None,
        route_zone_ids: Vec::new(),
        route_edge_ids: Vec::new(),
        travel_minutes: 0,
        causation_event_ids: Vec::new(),
    }
}

fn working(task: &str, kind: &str, zone: &str, due: i64) -> OperationRecord {
    OperationRecord {
        phase: Phase::Working,
        task_id: Some(text(task)),
        task_kind: Some(text(kind)),
        target_zone_id: Some(text(zone)),
        reason: Some(text("a reason")),
        started_minute: Some(0),
        arrival_minute: Some(0),
        due_minute: Some(due),
        ..idle()
    }
}

fn ops_row() -> StaffOperationsRow {
    let durations = map(vec![
        ("cleaning", V::Int(20)),
        ("maintenance", V::Int(30)),
        ("information_outreach", V::Int(15)),
        ("intake", V::Int(10)),
        ("patrol", V::Int(10)),
    ]);
    let config = map(vec![
        ("task_duration_minutes", durations),
        ("sanitation_use_per_person_minute", V::Float(0.01)),
        ("effects_by_zone", map(vec![])),
    ]);
    let mut zones = IdMap::default();
    zones.insert(text("z0"), ZoneRecord { sanitation: 0.9, updated_minute: 0 });
    zones.insert(text("z1"), ZoneRecord { sanitation: 0.95, updated_minute: 0 });
    let mut records = IdMap::default();
    for id in ["s1", "s2", "s3"] {
        records.insert(text(id), idle());
    }
    StaffOperationsRow {
        schema_version: 1,
        policy_version: text("autonomous-routed-staff-v1"),
        source_id: text("ASM-STAFF-OPERATIONS-V1"),
        config,
        zones,
        staff: records,
        history: Vec::new(),
    }
}

fn add(world: &mut World, name: &str, place: Place, row: Box<dyn sheltersim_core::row::Row>) {
    let index = world.slots.add(text(name), place).unwrap();
    world.slots.at(index).unwrap().try_borrow_mut().unwrap().row = Some(row);
}

/// A world of 100 minutes at minute 10 holding its queue, log, `id_counters` (event 40),
/// `metric_state`, no configs and the row of `staff_operations_v1`.
fn world() -> World {
    let mut world = World::default();
    world.scalars.get_mut().duration_minutes = Fit::Ok(100);
    world.scalars.get_mut().minute_index = Fit::Ok(10);
    *world.entities.get_mut() = entities();
    add(&mut world, ID_COUNTERS, Place::Field, Box::new(ValueRow(map(vec![("event", V::Int(40))]))));
    add(&mut world, METRIC_STATE, Place::Field, Box::new(MetricRow::default()));
    add(&mut world, MODEL_CONFIGS, Place::Field, Box::new(ValueRow(V::Map(IndexMap::new()))));
    add(&mut world, NAME, Place::Namespace, Box::new(ops_row()));
    world.hold_queue(Arc::new(Queue::new()));
    world.hold_log(Arc::new(EventLog::new()));
    world
}

fn request(world: &World, id: &str, agent: &str, requested: i64) {
    let row = RequestRow {
        id: text(id),
        agent_id: text(agent),
        requested_minute: requested,
        timeout_minute: requested + 90,
        status: RequestStatus::Pending,
        assigned_staff_id: None,
        task_id: None,
        resolved_minute: None,
        cooldown_until: 0,
        last_assignment_attempt_minute: None,
        last_assignment_wait_reason_code: None,
    };
    put(&mut world.rows_mut().unwrap().support_requests, id, row);
}

fn busy(world: &World, staff_id: &str, task: &str) {
    world.rows_mut().unwrap().staff.get_mut(staff_id).unwrap().active_task_id = Some(text(task));
}

fn counter(world: &World) -> V {
    world.row::<ValueRow>(ID_COUNTERS).unwrap().0.clone()
}

fn metrics(world: &World) -> Vec<(String, f64)> {
    world.metric_state().unwrap().iter().map(|(key, value)| (key.to_string(), *value)).collect()
}

fn records(world: &World) -> Vec<Value> {
    let log = world.log().unwrap();
    (0..log.len()).map(|index| serde_json::from_slice(log.written(index).unwrap()).unwrap()).collect()
}

fn pending(world: &World) -> Vec<(String, i64, i64)> {
    let queue = world.queue().unwrap();
    let mut events: Vec<(String, i64, i64)> = queue
        .ordered()
        .iter()
        .map(|(_, serial)| {
            let row = queue.slot(*serial).row.clone().unwrap();
            (row.event_id.to_string(), row.due_minute, row.sequence)
        })
        .collect();
    events.sort_by_key(|event| event.2);
    events
}

fn absent() -> Held {
    Held::Absent
}

/// What the tasks' rules read: `staff_operations_v1` held, nothing else, no plan.
fn tasks<'a>(staff: &'a Held, none: &'a Held, formula: Option<&'a StaffOperations>) -> ops::Inputs<'a> {
    ops::Inputs { staff, movement: none, body: none, program: none, shared_absent: true, geo: None, formula }
}

fn inputs<'a>(operations: &'a Held, none: &'a Held, tasks: Option<&'a ops::Inputs<'a>>) -> Inputs<'a> {
    Inputs { operations, dynamics: none, body: none, shared: none, metric_state_held: true, tasks }
}

fn service(resolver: Step, scheduler: Step) -> Service {
    Service { policy: text("staff-support-fifo-aging-v2"), task_minutes: 10, fairness_after: 3, resolver, scheduler }
}

fn graph(world: &World) -> Graph {
    let capacities = RouteCapacities::default();
    let rows = world.entities.try_borrow().unwrap();
    let router = Router::of_tables(&rows.zones, &rows.zone_edges, &capacities).unwrap();
    Graph { router: Arc::new(router), capacities: Arc::new(capacities) }
}

fn movement() -> Kept<Movement> {
    Movement::parse(&V::Map(IndexMap::new())).map(Arc::new)
}

// ---- StaffService.assign_pending -----------------------------------------------------------------

#[test]
fn a_minute_where_nobody_can_be_assigned_records_the_attempt_and_the_exposure_once() {
    let world = world();
    let (held, none) = (Held::Held, absent());
    request(&world, "r1", "a1", 8);
    busy(&world, "s1", "t9");
    let inputs = inputs(&held, &none, None);
    let mut taken = Vec::new();
    let assigned = staff::assign_pending(&world, &inputs, &service(Step::Own, Step::Own), 10, None, &mut taken).unwrap();
    assert_eq!(assigned, Assigned::Done(Vec::new()));
    let row = world.rows().unwrap().support_requests.get("r1").cloned().unwrap();
    assert_eq!((row.last_assignment_attempt_minute, row.last_assignment_wait_reason_code.as_deref()), (Some(10), Some(WAIT_REASON)));
    // The qualified member is busy; nobody is off shift, so that count is not written.
    assert_eq!(metrics(&world), [(NO_QUALIFIED.to_owned(), 0.0), (BUSY.to_owned(), 1.0), (ATTEMPT.to_owned(), 10.0)]);
    assert_eq!(counter(&world), map(vec![("event", V::Int(40))]));
    // Once per minute.
    staff::assign_pending(&world, &inputs, &service(Step::Own, Step::Own), 10, None, &mut taken).unwrap();
    assert_eq!(metrics(&world)[1], (BUSY.to_owned(), 1.0));
    assert!(staff::assign_plain(&world, &inputs, 10).unwrap());
    assert!(!staff::assign_plain(&world, &inputs, 11).unwrap());
    // Another minute than the world's is the rule's marked ValueError.
    let wrong = staff::assign_pending(&world, &inputs, &service(Step::None, Step::None), 11, None, &mut taken);
    assert!(matches!(wrong, Err(WriteError::Error(Error::MarkedValue(text))) if text == "staff assignment minute must match WorldState"));
}

#[test]
fn a_free_worker_takes_an_aged_request_where_it_stands() {
    let world = world();
    let (held, none) = (Held::Held, absent());
    request(&world, "r1", "a1", 5);
    let inputs = inputs(&held, &none, None);
    let mut taken = Vec::new();
    // A request may be assigned: nothing is written until the rest is read.
    assert_eq!(staff::assign_pending(&world, &inputs, &service(Step::None, Step::None), 10, None, &mut taken).unwrap(), Assigned::NeedsAll);
    assert_eq!(world.rows().unwrap().support_requests.get("r1").unwrap().last_assignment_attempt_minute, None);
    assert!(metrics(&world).is_empty());
    let full = Full { travel: None };
    let assigned = staff::assign_pending(&world, &inputs, &service(Step::None, Step::None), 10, Some(&full), &mut taken).unwrap();
    assert_eq!(assigned, Assigned::Done(vec![text("r1")]));
    let rows = world.rows().unwrap();
    let task: &TaskRow = rows.active_tasks.get("staff-support-40").unwrap();
    assert_eq!((task.start_minute, task.due_minute, task.staff_id.as_deref(), task.agent_id.as_deref()), (10, 20, Some("s1"), Some("a1")));
    let member = rows.staff.get("s1").unwrap();
    assert_eq!((member.active_task_id.as_deref(), member.work_queue.len(), member.busy_until), (Some("staff-support-40"), 1, 20));
    let request = rows.support_requests.get("r1").unwrap();
    assert_eq!((request.status, request.assigned_staff_id.as_deref(), request.last_assignment_wait_reason_code.as_deref()), (RequestStatus::Assigned, Some("s1"), None));
    drop(rows);
    assert_eq!(pending(&world), [("staff-support-complete-40".to_owned(), 20, 40)]);
    // Waited 5 minutes past fairness 3: promoted, and counted.
    let names: Vec<String> = metrics(&world).into_iter().map(|(name, _)| name).collect();
    assert_eq!(names, [NO_QUALIFIED, "support_fairness_promoted_assignment_count", ATTEMPT]);
    let queue = world.queue().unwrap();
    let row = queue.ordered().iter().map(|(_, serial)| queue.slot(*serial).row.clone().unwrap()).next().unwrap();
    let V::Map(payload) = &row.payload else { panic!("a payload") };
    assert_eq!(payload.keys().map(|key| &**key).collect::<Vec<_>>(), ["request_id", "agent_id", "staff_id", "task_id", "support_duration_minutes"]);
    let V::Map(provenance) = &row.provenance else { panic!("a provenance") };
    assert_eq!((provenance.get("assignment_wait_minutes"), provenance.get("fairness_promoted")), (Some(&V::Int(5)), Some(&V::Bool(true))));
}

#[test]
fn the_controller_s_route_and_trip_come_in_the_rule_s_order() {
    let world = world();
    let (held, none) = (Held::Held, absent());
    request(&world, "r1", "a1", 10);
    let inputs = inputs(&held, &none, None);
    let graph = graph(&world);
    let section = movement();
    let travel = Travel { routing: Routing { graph: &graph, geo: None, movement: &section, staff: &held }, walking: Walking { movement: &none, geo: None } };
    let full = Full { travel: Some(travel) };
    let mut taken = Vec::new();
    let assigned = staff::assign_pending(&world, &inputs, &service(Step::Own, Step::Own), 10, Some(&full), &mut taken).unwrap();
    assert_eq!(assigned, Assigned::Done(vec![text("r1")]));
    // The task's sequence first, then the trip's: the person walks 4 minutes to s1's zone.
    assert_eq!(pending(&world), [("staff-support-complete-40".to_owned(), 24, 40), ("agent-travel-complete-41".to_owned(), 14, 41)]);
    let rows = world.rows().unwrap();
    assert_eq!(rows.active_tasks.get("staff-support-40").unwrap().start_minute, 14);
    let person = rows.agents.get("a1").unwrap();
    assert_eq!((person.activity, person.target_id.as_deref()), (Activity::Moving, Some("z1")));
}

#[test]
fn a_task_past_the_run_s_end_and_another_resolver_are_not_started() {
    let world = world();
    let (held, none) = (Held::Held, absent());
    request(&world, "r1", "a1", 10);
    let inputs = inputs(&held, &none, None);
    let graph = graph(&world);
    let section = movement();
    let travel = Travel { routing: Routing { graph: &graph, geo: None, movement: &section, staff: &held }, walking: Walking { movement: &none, geo: None } };
    let full = Full { travel: Some(travel) };
    let mut taken = Vec::new();
    for (resolver, scheduler, reason) in [(Step::Other, Step::Own, OTHER_RESOLVER), (Step::Own, Step::Other, OTHER_SCHEDULER)] {
        let refused = staff::assign_pending(&world, &inputs, &service(resolver, scheduler), 10, Some(&full), &mut taken);
        assert!(matches!(refused, Err(WriteError::Error(error)) if error == unsupported(reason)));
        assert!(metrics(&world).is_empty());
    }
    let long = Service { task_minutes: 1000, ..service(Step::Own, Step::Own) };
    assert_eq!(staff::assign_pending(&world, &inputs, &long, 10, Some(&full), &mut taken).unwrap(), Assigned::Done(Vec::new()));
    assert_eq!(world.rows().unwrap().support_requests.get("r1").unwrap().last_assignment_wait_reason_code.as_deref(), Some("insufficient_run_time"));
    // No sequence was taken.
    assert_eq!(counter(&world), map(vec![("event", V::Int(40))]));
}

#[test]
fn an_urgent_request_preempts_a_routed_task() {
    let mut world = world();
    let (held, none) = (Held::Held, absent());
    request(&world, "r1", "a1", 10);
    busy(&world, "s1", "staff-operation-30");
    world.row_mut::<StaffOperationsRow>(NAME).unwrap().staff.insert(text("s1"), working("staff-operation-30", "patrol", "z1", 50));
    // `model_state["llm"]`, a key no lane owns: a value row.
    add(&mut world, "llm", Place::Value, Box::new(ValueRow(map(vec![("support_priorities", map(vec![("r1", V::Int(1))]))]))));
    let task_inputs = tasks(&held, &none, None);
    let inputs = inputs(&held, &none, Some(&task_inputs));
    let mut taken = Vec::new();
    let assigned = staff::assign_pending(&world, &inputs, &service(Step::None, Step::None), 10, Some(&Full { travel: None }), &mut taken).unwrap();
    assert_eq!(assigned, Assigned::Done(vec![text("r1")]));
    let logged = records(&world);
    assert_eq!(logged.len(), 1);
    assert_eq!(logged[0]["event"]["event_id"], "staff-operation-preempted-40");
    assert_eq!(logged[0]["status"], "applied");
    assert_eq!(logged[0]["detail"], "A priority-1 support request preempted a routed background staff task.");
    let row = world.row::<StaffOperationsRow>(NAME).unwrap();
    assert_eq!((row.staff.get("s1").unwrap().phase, row.history.len(), row.history[0].status), (Phase::Idle, 1, Outcome::Interrupted));
    drop(row);
    assert_eq!(world.rows().unwrap().staff.get("s1").unwrap().active_task_id.as_deref(), Some("staff-support-41"));
}

// ---- _consider_due_complaints --------------------------------------------------------------------

#[test]
fn the_due_complaints_go_to_the_host_in_id_order() {
    let world = world();
    request(&world, "r1", "a1", 3);
    request(&world, "r0", "a2", 3);
    request(&world, "r2", "a1", 4);
    request(&world, "r3", "a2", 3);
    world.rows_mut().unwrap().support_requests.get_mut("r3").unwrap().status = RequestStatus::Assigned;
    let cell = Shard::new(world);
    let mut host = TestHost::default();
    staff::consider_due_complaints(&cell, &mut host, Some(7), 10).unwrap();
    assert_eq!(host.calls, [Call::Complaint { agent: text("a2"), request: text("r0") }, Call::Complaint { agent: text("a1"), request: text("r1") }]);
    let mut host = TestHost::default();
    staff::consider_due_complaints(&cell, &mut host, None, 10).unwrap();
    assert!(host.calls.is_empty());
}

// ---- What the metrics read of each open request ----------------------------------------------------

#[test]
fn a_wait_reads_its_routes_only_where_free_staff_could_take_it() {
    let world = world();
    let (held, none) = (Held::Held, absent());
    request(&world, "r1", "a1", 4);
    let inputs = inputs(&held, &none, None);
    let ids = [text("r1")];
    busy(&world, "s1", "t9");
    let waits = staff::open_waits(&world, &inputs, None, &ids, 10, true).unwrap().unwrap();
    assert_eq!((waits.requests.len(), waits.requests[0].waited), (1, 6));
    assert_eq!(waits.reasons_of(&waits.requests[0]), ["staff_busy"]);
    world.rows_mut().unwrap().staff.get_mut("s1").unwrap().active_task_id = None;
    // A free worker: its routes are read, and without them the call says so.
    assert!(staff::open_waits(&world, &inputs, None, &ids, 10, true).unwrap().is_none());
    let section = movement();
    let minutes: Kept<i64> = Ok(Arc::new(10));
    let routes = WaitRoutes { loads_staff: &held, geo: None, movement: &section, support_minutes: &minutes };
    let waits = staff::open_waits(&world, &inputs, Some(&routes), &ids, 10, true).unwrap().unwrap();
    assert_eq!(waits.reasons_of(&waits.requests[0]), ["assignment_not_attempted_at_sampled_minute"]);
    let late: Kept<i64> = Ok(Arc::new(200));
    let routes = WaitRoutes { support_minutes: &late, ..routes };
    let waits = staff::open_waits(&world, &inputs, Some(&routes), &ids, 10, true).unwrap().unwrap();
    assert_eq!(waits.reasons_of(&waits.requests[0]), ["insufficient_run_time"]);
}

// ---- update_staff_operations ---------------------------------------------------------------------

#[test]
fn the_sanitation_falls_with_who_is_in_a_zone_and_an_unchanged_zone_is_not_written() {
    let mut world = world();
    let (held, none) = (Held::Held, absent());
    staff::update_operations(&world, &inputs(&held, &none, None), 10).unwrap();
    let row = world.row::<StaffOperationsRow>(NAME).unwrap().clone();
    let z0 = row.zones.get("z0").unwrap();
    assert_eq!((z0.sanitation.to_bits(), z0.updated_minute), ((0.9 - 2.0 * 0.01 * 10.0_f64).to_bits(), 10));
    assert_eq!(row.zones.get("z1").unwrap().sanitation, 0.95);
    assert_eq!(world.slots.written_names(), [text(NAME)]);
    world.slots.take_written();
    staff::update_operations(&world, &inputs(&held, &none, None), 10).unwrap();
    assert!(world.slots.take_written().is_empty());
    // Without the namespace, nothing.
    staff::update_operations(&world, &inputs(&none, &none, None), 11).unwrap();
    assert!(world.slots.take_written().is_empty());
}

// ---- assign_staff_operations ---------------------------------------------------------------------

#[test]
fn a_routed_task_starts_where_a_free_worker_has_one_to_take() {
    let world = world();
    let (held, none) = (Held::Held, absent());
    let known: Kept<()> = Ok(Arc::new(()));
    // Every zone clean enough: nothing to take, and no route read.
    assert_eq!(staff::assign_operations(&world, &inputs(&held, &none, None), &known, 10, None).unwrap(), Operations::Done(Vec::new()));
    world.row_mut::<StaffOperationsRow>(NAME).unwrap().zones.get_mut("z1").unwrap().sanitation = 0.5;
    assert_eq!(staff::assign_operations(&world, &inputs(&held, &none, None), &known, 10, None).unwrap(), Operations::NeedsRoutes);
    let graph = graph(&world);
    let routed = Routed { router: &graph.router, movement: &none, loads_staff: &held };
    // The formula the configs could not give is refused where the plan reads it.
    let unread = tasks(&held, &none, None);
    let refused = staff::assign_operations(&world, &inputs(&held, &none, Some(&unread)), &known, 10, Some(&routed));
    assert!(matches!(refused, Err(WriteError::Error(error)) if error == unsupported(FORMULA_UNREAD)));
    assert_eq!(counter(&world), map(vec![("event", V::Int(40))]));
    let formula = StaffOperations::parse(&V::Map(IndexMap::new())).unwrap();
    let task_inputs = tasks(&held, &none, Some(&formula));
    let started = staff::assign_operations(&world, &inputs(&held, &none, Some(&task_inputs)), &known, 10, Some(&routed)).unwrap();
    // The staff go by ID (no history, no fatigue): s2 walks 4 minutes to clean z1, and the task is
    // then reserved against s3, who is in z1.
    assert_eq!(started, Operations::Done(vec![text("staff-operation-40")]));
    let row = world.row::<StaffOperationsRow>(NAME).unwrap().clone();
    let record = row.staff.get("s2").unwrap();
    assert_eq!((record.phase, record.task_kind.as_deref(), record.travel_due_minute, record.due_minute), (Phase::Travel, Some("cleaning"), Some(14), Some(34)));
    assert_eq!((record.reason.as_deref(), &*record.route_edge_ids), (Some("sanitation_index_below_0.85"), &[text("e1")][..]));
    assert_eq!(row.staff.get("s3").unwrap().phase, Phase::Idle);
    assert_eq!(pending(&world), [("staff-operation-arrive-40".to_owned(), 14, 40), ("staff-operation-complete-41".to_owned(), 34, 41)]);
    // A known-formula refusal comes first.
    let other: Kept<()> = Err(sheltersim_core::config::Fault::refused("another staff operations formula"));
    let refused = staff::assign_operations(&world, &inputs(&held, &none, None), &other, 10, None);
    assert!(matches!(refused, Err(WriteError::Error(error)) if error == unsupported("another staff operations formula")));
}

// ---- schedule_staff_operation_relief -------------------------------------------------------------

#[test]
fn an_unavailable_worker_s_task_is_handed_to_one_at_the_task() {
    let mut world = world();
    let (held, none) = (Held::Held, absent());
    busy(&world, "s2", "staff-operation-30");
    put(&mut world.rows_mut().unwrap().active_tasks, "staff-operation-30", TaskRow {
        id: text("staff-operation-30"),
        kind: text("staff_operation"),
        agent_id: None,
        resource_id: None,
        start_minute: 0,
        due_minute: 50,
        staff_id: Some(text("s2")),
    });
    world.row_mut::<StaffOperationsRow>(NAME).unwrap().staff.insert(text("s2"), working("staff-operation-30", "cleaning", "z1", 50));
    let inputs = inputs(&held, &none, None);
    // s2 is available: nothing to hand.
    assert!(staff::relief(&world, &inputs, 10).unwrap().is_empty());
    let status = map(vec![("staff_status", map(vec![("s2", map(vec![("available", V::Bool(false))]))]))]);
    add(&mut world, "operational_events_v1", Place::Value, Box::new(ValueRow(status)));
    assert_eq!(staff::relief(&world, &inputs, 10).unwrap(), [text("staff-operation-relief-40")]);
    assert_eq!(pending(&world), [("staff-operation-relief-40".to_owned(), 11, 40)]);
    // Already handed.
    assert!(staff::relief(&world, &inputs, 10).unwrap().is_empty());
    // The last minute has no boundary after it.
    assert!(staff::relief(&world, &inputs, 99).unwrap().is_empty());
}

// ---- The staff service's parameters and the formula's section ----------------------------------------

#[test]
fn the_service_s_route_is_the_controller_s_own_step_or_refused() {
    let class = Class { module: Some(text("sheltersim.domain.staff")), qualname: Some(text("StaffService")), resolves: true };
    let of = |resolver: Attr, scheduler: Attr| {
        let mut object = Obj::new(class.clone());
        for (name, value) in [
            ("policy_version", Attr::Value(V::Str(text("staff-support-fifo-aging-v2")))),
            ("task_duration_minutes", Attr::Value(V::Int(10))),
            ("unresolved_after_minutes", Attr::Value(V::Int(90))),
            ("cooldown_minutes", Attr::Value(V::Int(30))),
            ("fairness_after_minutes", Attr::Value(V::Int(45))),
            ("route_resolver", resolver),
            ("travel_scheduler", scheduler),
        ] {
            object.attrs.insert(text(name), Ok(value));
        }
        staff_service(&object)
    };
    let own = of(Attr::Method(text("_route")), Attr::Method(text("_schedule_staff_support_travel"))).unwrap();
    assert_eq!((own.resolver, own.scheduler, own.task_minutes, own.fairness_after), (Step::Own, Step::Own, 10, 45));
    let none = of(Attr::Value(V::Null), Attr::Opaque { callable: true }).unwrap();
    assert_eq!((none.resolver, none.scheduler), (Step::None, Step::Other));
    // A method of the controller that is another step is another resolver.
    let other = of(Attr::Method(text("_schedule_staff_support_travel")), Attr::Value(V::Null)).unwrap();
    assert_eq!((other.resolver, other.scheduler), (Step::Other, Step::None));
}

#[test]
fn the_built_in_formulas_are_the_python_factory_s() {
    let none = StaffOperations::parse(&V::Map(IndexMap::new())).unwrap();
    assert_eq!((&*none.model_id, none.selected_source.as_deref(), none.travel_first()), ("staff-urgency-first-v1", None, false));
    assert_eq!(none.provenance(), ("ASM-STAFF-OPERATIONS-V1", "autonomous-routed-staff-v1", "autonomous-routed-staff-v1"));
    let registry = |model: &str| map(vec![("policy_registry", map(vec![("staff_operations", V::Str(text(model)))]))]);
    let travel = StaffOperations::parse(&registry("staff-travel-first-v1")).unwrap();
    assert_eq!(travel.provenance(), ("ASM-STAFF-TRAVEL-FIRST-V1", "staff-travel-first-v1", "ASM-STAFF-TRAVEL-FIRST-V1"));
    assert!(travel.travel_first());
    let urgency = StaffOperations::parse(&registry("staff-urgency-first-v1")).unwrap();
    assert_eq!(urgency.selected_source.as_deref(), Some("ASM-STAFF-URGENCY-FIRST-V1"));
    assert!(StaffOperations::parse(&registry("a-later-formula")).is_err());
}
