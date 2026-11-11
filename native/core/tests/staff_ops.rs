//! Lane b2: the events of a staff operation and an operator's order (`rules::staff_operation_events`,
//! `rules::staff_order`) on a world built here with no Python. The PyO3 crate's kernels of the same
//! names are glue over these, so the gate and the offline tests run them on every Run; these say
//! what each reads, decides, refuses and writes.

use std::sync::Arc;

use indexmap::IndexMap;
use serde_json::Value;
use sheltersim_core::config::staff::StaffOperations;
use sheltersim_core::entities::{AnyTable, EdgeRow, Entities, Skills, StaffRow, Table, TaskRow, ZoneRow};
use sheltersim_core::error::Error;
use sheltersim_core::events::log::EventLog;
use sheltersim_core::events::queue::Queue;
use sheltersim_core::events::writer::WriteError;
use sheltersim_core::events::{EventKind, EventRow};
use sheltersim_core::geometry::Geo;
use sheltersim_core::row::{IdMap, ValueRow};
use sheltersim_core::rules::kit::Held;
use sheltersim_core::rules::staff_operation_events::{self as ops, Inputs, Planned, Stopping, FORMULA_UNREAD};
use sheltersim_core::rules::staff_order::{self, Order};
use sheltersim_core::state::staff_operations::{OperationRecord, Outcome, Phase, StaffOperationsRow, ZoneRecord};
use sheltersim_core::value::V;
use sheltersim_core::world::{Fit, Place, World, ID_COUNTERS, MODEL_CONFIGS};

const NAME: &str = "staff_operations_v1";

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

fn map(entries: Vec<(&str, V)>) -> V {
    V::Map(entries.into_iter().map(|(key, value)| (text(key), value)).collect())
}

fn put<R: Clone>(table: &mut Table<R>, id: &str, row: R) {
    let slot = table.reserve_slot();
    table.fill_slot(slot, row);
    assert_eq!(table.bind(text(id), slot), None);
}

fn staff(id: &str, zone: &str, skills: &[&str], task: Option<&str>) -> StaffRow {
    StaffRow {
        id: text(id),
        zone_id: text(zone),
        skills: Skills(skills.iter().map(|skill| text(skill)).collect()),
        work_queue: task.into_iter().map(text).collect(),
        busy_until: if task.is_some() { 30 } else { 0 },
        active_task_id: task.map(text),
    }
}

/// Zones z0 and z1, edge e1 (z0 to z1, 4 minutes) and e2 back; s1 (cleaning) and s2 (sanitation)
/// in z0, s3 (patrol) in z1.
fn entities() -> Entities {
    let mut entities = Entities::default();
    for id in ["z0", "z1"] {
        put(&mut entities.zones, id, ZoneRow { id: text(id), name: text(id), capacity: None, display_x: None, display_y: None });
    }
    for (id, from, to) in [("e1", "z0", "z1"), ("e2", "z1", "z0")] {
        put(&mut entities.zone_edges, id, EdgeRow { id: text(id), from_zone_id: text(from), to_zone_id: text(to), travel_minutes: 4, closed: false });
    }
    put(&mut entities.staff, "s1", staff("s1", "z0", &["cleaning"], None));
    put(&mut entities.staff, "s2", staff("s2", "z0", &["sanitation"], None));
    put(&mut entities.staff, "s3", staff("s3", "z1", &["patrol"], None));
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

fn config() -> V {
    let durations = map(vec![
        ("cleaning", V::Int(20)),
        ("maintenance", V::Int(30)),
        ("information_outreach", V::Int(15)),
        ("intake", V::Int(10)),
        ("patrol", V::Int(10)),
    ]);
    map(vec![
        ("task_duration_minutes", durations),
        ("cleaning_sanitation_recovery", V::Float(0.25)),
        ("effects_by_zone", map(vec![("z1", map(vec![("cleaning_sanitation_recovery", V::Float(0.5))]))])),
    ])
}

fn ops_row() -> StaffOperationsRow {
    let mut zones = IdMap::default();
    zones.insert(text("z0"), ZoneRecord { sanitation: 0.5, updated_minute: 0 });
    zones.insert(text("z1"), ZoneRecord { sanitation: 0.4, updated_minute: 0 });
    let mut records = IdMap::default();
    for id in ["s1", "s2", "s3"] {
        records.insert(text(id), idle());
    }
    StaffOperationsRow {
        schema_version: 1,
        policy_version: text("autonomous-routed-staff-v1"),
        source_id: text("ASM-STAFF-OPERATIONS-V1"),
        config: config(),
        zones,
        staff: records,
        history: Vec::new(),
    }
}

fn add(world: &mut World, name: &str, place: Place, row: Box<dyn sheltersim_core::row::Row>) {
    let index = world.slots.add(text(name), place).unwrap();
    world.slots.at(index).unwrap().try_borrow_mut().unwrap().row = Some(row);
}

/// A world of 100 minutes at minute 10 holding its queue, log, `id_counters` (event 40), no configs
/// and the row of `staff_operations_v1`.
fn world() -> World {
    let mut world = World::default();
    world.scalars.get_mut().duration_minutes = Fit::Ok(100);
    world.scalars.get_mut().minute_index = Fit::Ok(10);
    *world.entities.get_mut() = entities();
    add(&mut world, ID_COUNTERS, Place::Field, Box::new(ValueRow(map(vec![("event", V::Int(40))]))));
    add(&mut world, MODEL_CONFIGS, Place::Field, Box::new(ValueRow(V::Map(IndexMap::new()))));
    add(&mut world, NAME, Place::Namespace, Box::new(ops_row()));
    world.hold_queue(Arc::new(Queue::new()));
    world.hold_log(Arc::new(EventLog::new()));
    world
}

struct Given {
    staff: Held,
    absent: Held,
    geo: Arc<Geo>,
    formula: StaffOperations,
}

impl Given {
    fn new() -> Self {
        Self { staff: Held::Held, absent: Held::Absent, geo: Arc::new(Geo::new(None, None, None, None).unwrap()), formula: StaffOperations { model_id: text("urgency-first"), selected_source: None } }
    }

    fn inputs(&self) -> Inputs<'_> {
        Inputs {
            staff: &self.staff,
            movement: &self.absent,
            body: &self.absent,
            program: &self.absent,
            shared_absent: true,
            geo: Some(&self.geo),
            formula: Some(&self.formula),
        }
    }
}

fn record(world: &World, id: &str) -> OperationRecord {
    world.row::<StaffOperationsRow>(NAME).unwrap().staff.get(id).unwrap().clone()
}

fn records(world: &World) -> Vec<Value> {
    let log = world.log().unwrap();
    (0..log.len()).map(|index| serde_json::from_slice(log.written(index).unwrap()).unwrap()).collect()
}

fn event(event_id: &str, sequence: i64, due: i64, event_type: &str, payload: Vec<(&str, V)>) -> EventRow {
    let V::Map(payload) = map(payload) else { unreachable!() };
    EventRow::new(text(event_id), EventKind::Completion, due, 5, sequence, text(event_type), payload, Some(text("autonomous-routed-staff-v1")), None, IndexMap::new()).unwrap()
}

fn s(value: &str) -> V {
    V::Str(text(value))
}

/// s1 on its way to clean z1 (task t1), due there at 14 and done at 34; the task and its two events.
fn travelling(world: &World) -> (EventRow, EventRow) {
    let mut row = world.row_mut::<StaffOperationsRow>(NAME).unwrap();
    row.staff.insert(
        text("s1"),
        OperationRecord {
            phase: Phase::Travel,
            task_id: Some(text("t1")),
            task_kind: Some(text("cleaning")),
            target_id: None,
            target_zone_id: Some(text("z1")),
            reason: Some(text("sanitation_index_below_0.6")),
            started_minute: Some(10),
            travel_due_minute: Some(14),
            arrival_minute: None,
            due_minute: Some(34),
            route_zone_ids: vec![text("z0"), text("z1")],
            route_edge_ids: vec![text("e1")],
            travel_minutes: 4,
            causation_event_ids: vec![text("cause")],
        },
    );
    drop(row);
    let mut rows = world.rows_mut().unwrap();
    rows.active_tasks.insert(text("t1"), TaskRow { id: text("t1"), kind: text("staff_operation"), agent_id: None, resource_id: None, start_minute: 10, due_minute: 34, staff_id: Some(text("s1")) });
    *rows.staff.get_mut("s1").unwrap() = staff("s1", "z0", &["cleaning"], Some("t1"));
    drop(rows);
    let common = |kind: &str| vec![("staff_id", s("s1")), ("task_id", s("t1")), ("task_kind", s(kind)), ("target_id", V::Null), ("target_zone_id", s("z1"))];
    let arrive = event("staff-operation-arrive-30", 30, 14, "staff_operation_arrive", vec![("staff_id", s("s1")), ("task_id", s("t1")), ("target_zone_id", s("z1"))]);
    let complete = event("staff-operation-complete-31", 31, 34, "staff_operation_complete", common("cleaning"));
    for row in [&arrive, &complete] {
        assert_eq!(world.queue_mut().unwrap().schedule_row(row.clone()), 0);
    }
    (arrive, complete)
}

// ---- Arrival and completion -----------------------------------------------------------------------

#[test]
fn an_arrival_starts_the_work_and_a_completion_cleans_the_zone() {
    let world = world();
    let held = Given::new();
    let (arrive, complete) = travelling(&world);
    let Planned::Go(plan) = ops::plan_arrive(&world, &held.inputs(), &arrive).unwrap() else { panic!("unavailable") };
    ops::write_arrive(&world, plan).unwrap();
    let working = record(&world, "s1");
    assert_eq!((working.phase, working.arrival_minute, working.travel_due_minute), (Phase::Working, Some(14), None));
    assert!(working.route_edge_ids.is_empty() && working.route_zone_ids.is_empty());
    assert_eq!(working.causation_event_ids, vec![text("cause"), text("staff-operation-arrive-30")]);
    assert_eq!(&*world.rows().unwrap().staff.get("s1").unwrap().zone_id, "z1");
    // The same arrival again no longer matches the travelling operation.
    let again = ops::plan_arrive(&world, &held.inputs(), &arrive);
    assert!(matches!(again, Err(Error::Outcome(_, ref text)) if text == "staff travel arrival does not match an active operation"));

    let Planned::Go(plan) = ops::plan_complete(&world, &held.inputs(), &complete).unwrap() else { panic!("unavailable") };
    // Planning changes nothing.
    assert_eq!(world.row::<StaffOperationsRow>(NAME).unwrap().zones.get("z1").unwrap().sanitation, 0.4);
    let receipts = ops::write_complete(&world, &held.inputs(), plan).unwrap();
    assert!(receipts.is_empty(), "no receipts without shared_operations_v1");
    let row = world.row::<StaffOperationsRow>(NAME).unwrap();
    // The zone's own recovery (0.5), not the global one.
    assert_eq!(row.zones.get("z1").unwrap().sanitation, 0.9);
    assert_eq!(row.staff.get("s1").unwrap().phase, Phase::Idle);
    let done = &row.history[0];
    assert_eq!((done.status, done.start_minute, done.arrival_minute, done.complete_minute), (Outcome::Completed, 10, Some(14), 34));
    assert_eq!(done.causation_event_ids, vec![text("cause"), text("staff-operation-arrive-30"), text("staff-operation-complete-31")]);
    drop(row);
    let rows = world.rows().unwrap();
    assert!(rows.active_tasks.get("t1").is_none());
    let s1 = rows.staff.get("s1").unwrap();
    assert_eq!((s1.active_task_id.clone(), s1.busy_until, s1.work_queue.len()), (None, 0, 0));
}

#[test]
fn a_completion_of_a_worker_who_became_unavailable_stops_the_task() {
    let mut world = world();
    let status = map(vec![("staff_status", map(vec![("s1", map(vec![("available", V::Bool(false))]))]))]);
    add(&mut world, "operational_events_v1", Place::Namespace, Box::new(ValueRow(status)));
    let held = Given::new();
    let (arrive, _) = travelling(&world);
    let Planned::Unavailable(id) = ops::plan_arrive(&world, &held.inputs(), &arrive).unwrap() else { panic!("available") };
    assert_eq!(&*id, "s1");
    let mut taken = Vec::new();
    let stopped = ops::stop_and_suppress(&world, &held.inputs(), &id, &arrive, "staff became unavailable before reaching the assigned task", &mut taken);
    assert!(matches!(stopped, Err(WriteError::Error(Error::Outcome(_, ref text))) if text == "staff became unavailable before reaching the assigned task"));
    // The arrival itself is the dispatcher's; its completion leaves the queue as suppressed.
    assert_eq!(taken.len(), 1);
    let logged = records(&world);
    assert_eq!(logged.len(), 1);
    assert_eq!(logged[0]["event"]["event_id"], "staff-operation-complete-31");
    assert_eq!(logged[0]["status"], "suppressed");
    assert_eq!(logged[0]["detail"], "Staff operation was interrupted by staff unavailability");
    let row = world.row::<StaffOperationsRow>(NAME).unwrap();
    let stopped = &row.history[0];
    // Stopped when its walk was over: it arrived then, and the event and the completion are its causes.
    assert_eq!((stopped.status, stopped.arrival_minute, stopped.complete_minute), (Outcome::Interrupted, Some(14), 14));
    assert_eq!(stopped.causation_event_ids, vec![text("cause"), text("staff-operation-arrive-30"), text("staff-operation-complete-31")]);
    assert_eq!(row.staff.get("s1").unwrap().phase, Phase::Idle);
    drop(row);
    // At the end of its route (elapsed 4 of 4): the nearest zone is z1.
    assert_eq!(&*world.rows().unwrap().staff.get("s1").unwrap().zone_id, "z1");
    assert!(world.rows().unwrap().active_tasks.get("t1").is_none());
}

#[test]
fn a_closed_edge_stops_the_trips_over_it() {
    let world = world();
    let held = Given::new();
    travelling(&world);
    let stopping = Stopping { event_id: "close-e1", due_minute: 12 };
    let mut taken = Vec::new();
    assert!(ops::interrupt_trips(&world, &held.inputs(), &stopping, &mut taken).unwrap().is_empty());
    world.entities.try_borrow_mut().unwrap().zone_edges.get_mut("e1").unwrap().closed = true;
    let stopped = ops::interrupt_trips(&world, &held.inputs(), &stopping, &mut taken).unwrap();
    assert_eq!(stopped, vec![text("s1")]);
    assert_eq!(taken.len(), 2);
    // Halfway (2 of 4 minutes) rounds to the next zone.
    assert_eq!(&*world.rows().unwrap().staff.get("s1").unwrap().zone_id, "z1");
    let row = world.row::<StaffOperationsRow>(NAME).unwrap();
    assert_eq!(row.history[0].causation_event_ids, vec![text("cause"), text("close-e1"), text("staff-operation-arrive-30"), text("staff-operation-complete-31")]);
}

#[test]
fn a_tainted_namespace_is_refused_before_anything_is_written() {
    let world = world();
    let held = Given::new();
    let (arrive, _) = travelling(&world);
    world.slots.get(NAME).unwrap().try_borrow_mut().unwrap().misfit("an int sanitation".to_owned());
    let refused = ops::plan_arrive(&world, &held.inputs(), &arrive);
    assert!(matches!(refused, Err(Error::Unsupported(ref reason)) if reason == "the namespace has no row"));
    assert!(records(&world).is_empty());
}

// ---- Relief -----------------------------------------------------------------------------------------

fn relief(task: &str, previous: &str, replacement: &str) -> EventRow {
    let V::Map(payload) = map(vec![("task_id", s(task)), ("previous_staff_id", s(previous)), ("replacement_staff_id", s(replacement)), ("reason_code", s("shift_end"))]) else { unreachable!() };
    let V::Map(provenance) = map(vec![("source_type", s("assumption")), ("source_id", s("ASM-STAFF-RELIEF-V1")), ("policy_version", s("same-site-staff-relief-v1"))]) else { unreachable!() };
    EventRow::new(text("relief-1"), EventKind::Endogenous, 10, 0, 35, text("staff_operation_relief"), payload, Some(text("same-site-staff-relief-v1")), None, provenance).unwrap()
}

/// s1 cleaning z0 since minute 8 (task t1), due at 28, and its completion; s1 is no longer
/// available, which is what a relief is for.
fn working(world: &mut World) {
    let status = map(vec![("staff_status", map(vec![("s1", map(vec![("available", V::Bool(false))]))]))]);
    add(world, "operational_events_v1", Place::Namespace, Box::new(ValueRow(status)));
    let mut row = world.row_mut::<StaffOperationsRow>(NAME).unwrap();
    let mut record = idle();
    record.phase = Phase::Working;
    record.task_id = Some(text("t1"));
    record.task_kind = Some(text("cleaning"));
    record.target_zone_id = Some(text("z0"));
    record.reason = Some(text("sanitation_index_below_0.6"));
    record.started_minute = Some(8);
    record.arrival_minute = Some(8);
    record.due_minute = Some(28);
    record.causation_event_ids = vec![text("cause")];
    row.staff.insert(text("s1"), record);
    drop(row);
    let mut rows = world.rows_mut().unwrap();
    rows.active_tasks.insert(text("t1"), TaskRow { id: text("t1"), kind: text("staff_operation"), agent_id: None, resource_id: None, start_minute: 8, due_minute: 28, staff_id: Some(text("s1")) });
    *rows.staff.get_mut("s1").unwrap() = staff("s1", "z0", &["cleaning"], Some("t1"));
    drop(rows);
    let complete = event("staff-operation-complete-31", 31, 28, "staff_operation_complete", vec![("staff_id", s("s1")), ("task_id", s("t1"))]);
    assert_eq!(world.queue_mut().unwrap().schedule_row(complete), 0);
}

#[test]
fn a_relief_hands_the_task_to_a_qualified_colleague() {
    let mut world = world();
    let held = Given::new();
    working(&mut world);
    // s3 is in another zone: stale.
    let stale = ops::plan_relief(&world, &held.inputs(), &relief("t1", "s1", "s3"));
    assert!(matches!(stale, Err(Error::Outcome(_, ref text)) if text == "staff relief is stale or its replacement is no longer available"));
    let plan = ops::plan_relief(&world, &held.inputs(), &relief("t1", "s1", "s2")).unwrap();
    let mut taken = Vec::new();
    assert!(ops::write_relief(&world, &held.inputs(), plan, &mut taken).unwrap().is_none());
    assert_eq!(taken.len(), 1);
    let replaced = record(&world, "s2");
    assert_eq!((replaced.phase, replaced.started_minute, replaced.due_minute), (Phase::Working, Some(10), Some(30)));
    assert_eq!(replaced.causation_event_ids, vec![text("cause"), text("relief-1")]);
    assert_eq!(record(&world, "s1").phase, Phase::Idle);
    let task = world.rows().unwrap().active_tasks.get("t1").unwrap().clone();
    assert_eq!((task.staff_id, task.start_minute, task.due_minute), (Some(text("s2")), 10, 30));
    // The old completion is suppressed, and a new one is scheduled under the next sequence.
    assert_eq!(records(&world)[0]["detail"], "staff operation was replaced by a qualified co-located worker");
    let queue = world.queue().unwrap();
    let (_, serial) = queue.ordered()[0];
    let pending = queue.slot(serial).row.clone().unwrap();
    assert_eq!((&*pending.event_id, pending.due_minute), ("staff-operation-complete-40", 30));
    assert_eq!(pending.source_id.as_deref(), Some("autonomous-routed-staff-v1"));
}

#[test]
fn a_relief_without_the_formula_says_so_where_it_reads_it() {
    let mut world = world();
    let held = Given::new();
    working(&mut world);
    let inputs = Inputs { formula: None, ..held.inputs() };
    let refused = ops::plan_relief(&world, &inputs, &relief("t1", "s1", "s2"));
    assert!(matches!(refused, Err(Error::Unsupported(ref reason)) if reason == FORMULA_UNREAD));
    // A stale relief never reads it.
    let stale = ops::plan_relief(&world, &inputs, &relief("t1", "s1", "s3"));
    assert!(matches!(stale, Err(Error::Outcome(..))));
}

// ---- An order ---------------------------------------------------------------------------------------

fn order<'a>(staff: &'a str, kind: &'a str, zone: &'a str) -> Order<'a> {
    Order { staff_id: staff, task_kind: kind, target_zone_id: zone, target_id: None, reason: "operator", causation_event_id: "program-1", source_id: None }
}

fn order_failure(world: &World, inputs: &Inputs<'_>, order: &Order<'_>) -> String {
    match staff_order::order_operation(world, inputs, order) {
        Err(WriteError::Error(Error::Outcome(_, text))) => text,
        Err(other) => panic!("{other:?}"),
        Ok(task) => panic!("ordered {task}"),
    }
}

#[test]
fn an_order_writes_the_task_and_its_events() {
    let mut world = world();
    let held = Given::new();
    let mut inputs = held.inputs();
    inputs.shared_absent = false;
    assert_eq!(order_failure(&world, &inputs, &order("s9", "cleaning", "z1")), "staff task order references unknown staff");
    assert_eq!(order_failure(&world, &inputs, &order("s1", "laundry", "z1")), "staff task order uses an unregistered task kind");
    assert_eq!(
        order_failure(&world, &inputs, &order("s1", "patrol", "z1")),
        "staff_lacks_task_skill: s1は巡回の技能（patrol・support）を持ちません。技能を持つ職員（s3）に指示してください。"
    );
    assert_eq!(order_failure(&world, &inputs, &order("s1", "cleaning", "z9")), "staff task order references an unknown target zone");
    let without = Inputs { shared_absent: true, ..held.inputs() };
    assert_eq!(order_failure(&world, &without, &order("s1", "cleaning", "z1")), "staff task order requires staff and shared operation state");
    assert!(records(&world).is_empty());

    let task = staff_order::order_operation(&world, &inputs, &order("s1", "cleaning", "z1")).unwrap();
    assert_eq!(task, "staff-operation-40");
    let ordered = record(&world, "s1");
    assert_eq!((ordered.phase, ordered.travel_due_minute, ordered.due_minute, ordered.travel_minutes), (Phase::Travel, Some(14), Some(34), 4));
    assert_eq!(ordered.route_edge_ids, vec![text("e1")]);
    let s1 = world.rows().unwrap().staff.get("s1").unwrap().clone();
    assert_eq!((s1.active_task_id, s1.work_queue, s1.busy_until), (Some(text("staff-operation-40")), vec![text("staff-operation-40")], 34));
    let queue = world.queue().unwrap();
    let pending: Vec<(Arc<str>, i64)> = queue.ordered().iter().map(|(key, serial)| (queue.slot(*serial).event_id.clone(), key.0)).collect();
    assert_eq!(pending, vec![(text("staff-operation-arrive-40"), 14), (text("staff-operation-complete-41"), 34)]);
    let (_, serial) = queue.ordered()[1];
    let complete = queue.slot(serial).row.clone().unwrap();
    assert_eq!(complete.provenance, map(vec![
        ("source_type", s("assumption")),
        ("source_id", s("ASM-STAFF-OPERATIONS-V1")),
        ("policy_version", s("autonomous-routed-staff-v1")),
        ("reason", s("operator")),
    ]));
    drop(queue);
    world.scalars.get_mut().minute_index = Fit::Ok(90);
    *world.row_mut::<StaffOperationsRow>(NAME).unwrap().staff.get_mut("s2").unwrap() = idle();
    assert_eq!(order_failure(&world, &inputs, &order("s2", "cleaning", "z1")), "staff task order would finish beyond the Run boundary");
}
