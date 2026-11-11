//! Lane c3: the staffed water and meal service (`rules::service`) and the toilet service
//! (`rules::toilet`), on a world built here with no Python. The PyO3 crate's kernels of the same
//! names are glue over these, so the gate and the offline tests run them on every Run; these say
//! what each reads, decides and writes, and what it asks of the host.

mod support;

use std::sync::Arc;

use indexmap::IndexMap;
use sheltersim_core::cell::Shard;
use sheltersim_core::config::population::AccessibilityNeeds;
use sheltersim_core::entities::{Activity, AgentRow, AnyTable, Entities, FacilityRow, Mobility, NeedRow, ProfileRow, Skills, StaffRow, Table};
use sheltersim_core::error::Error;
use sheltersim_core::events::log::EventLog;
use sheltersim_core::events::queue::Queue;
use sheltersim_core::events::writer::WriteError;
use sheltersim_core::events::{EventKind, EventRow};
use sheltersim_core::geometry::Geo;
use sheltersim_core::host::Part;
use sheltersim_core::rng::{label, Key, KeyedDraws};
use sheltersim_core::row::{IdMap, ValueRow};
use sheltersim_core::rules::kit::{Held, Kept};
use sheltersim_core::rules::resources::LedgerFault;
use sheltersim_core::rules::service::{self, Failure, Manager, Minute, Pending, Request, Requested, Serve, Served, Wrap};
use sheltersim_core::rules::toilet::{self, Rng, ScheduleReturn, Start, Started};
use sheltersim_core::state::metric_state::MetricRow;
use sheltersim_core::state::resource_ledger::{Account, Accounts, Audit, AuditRecord, LedgerRow, UsedIds, AUDIT_SCHEMA_VERSION, NAME as LEDGER};
use sheltersim_core::state::resource_service::{ServiceRow, NAME as SERVICE};
use sheltersim_core::value::V;
use sheltersim_core::world::{Fit, Place, ResourcesRow, World, WorldCell, ID_COUNTERS, METRIC_STATE, RESOURCES};
use support::{Call, TestHost};

const WATER: &str = "water-1";
const TOILET: &str = "toilet-1";

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

fn unsupported(reason: &str) -> Error {
    Error::Unsupported(reason.to_owned().into())
}

fn put<R: Clone>(table: &mut Table<R>, id: &str, row: R) {
    let slot = table.reserve_slot();
    table.fill_slot(slot, row);
    assert_eq!(table.bind(text(id), slot), None);
}

fn needs(value: f64) -> NeedRow {
    NeedRow { toilet_urgency: value, hunger: value, fatigue: 0.0, hydration_need: value }
}

fn agent(id: &str) -> AgentRow {
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
        activity: Activity::Idle,
        target_id: None,
        needs: needs(0.7),
        stress: 0.0,
        trust: 0.5,
        next_decision_at: 0,
        support_request_id: None,
        support_cooldown_until: 0,
    }
}

fn facility(id: &str, kind: &str) -> FacilityRow {
    FacilityRow {
        id: text(id),
        kind: text(kind),
        zone_id: text("z0"),
        capacity: 2,
        working_units: 2,
        active_agent_ids: Vec::new(),
        queue_agent_ids: Vec::new(),
        resource_active_agent_ids: Vec::new(),
        resource_queue_agent_ids: Vec::new(),
    }
}

fn staff(id: &str, skills: &[&str]) -> StaffRow {
    StaffRow { id: text(id), zone_id: text("z0"), skills: Skills(skills.iter().map(|skill| text(skill)).collect()), work_queue: Vec::new(), busy_until: 0, active_task_id: None }
}

fn account(remaining: i64) -> Account {
    Account { initial: remaining, delivered: 0, served: 0, losses: 0, remaining, reserved: 0, source_ids: vec![text("SRC-INIT")], reservations: IdMap::default() }
}

fn initial(resource: &str, remaining: i64) -> AuditRecord {
    AuditRecord {
        schema_version: AUDIT_SCHEMA_VERSION,
        minute: 0,
        event_id: None,
        operation: text("initial"),
        resource_id: text(resource),
        quantity: remaining,
        reservation_id: None,
        agent_id: None,
        source_id: None,
        source_ids: vec![text("SRC-INIT")],
        warehouse_id: text("warehouse-main"),
        initial: remaining,
        delivered: 0,
        served: 0,
        losses: 0,
        remaining,
        reserved: 0,
    }
}

/// A world of 600 minutes at minute 5: two people (`a1`, `a2`), a water station and a toilet, a
/// water server `s1` and a member `s0` without the skill, the queue and log, `id_counters` (the
/// event counter at 10), `metric_state`, an empty `resource_service_v1` and a ledger of 1000 ml.
fn world(water: i64) -> World {
    let mut world = World::default();
    world.scalars.get_mut().duration_minutes = Fit::Ok(600);
    world.scalars.get_mut().minute_index = Fit::Ok(5);
    let mut entities = Entities::default();
    put(&mut entities.agents, "a1", agent("a1"));
    put(&mut entities.agents, "a2", agent("a2"));
    put(&mut entities.facilities, TOILET, facility(TOILET, "Toilet"));
    put(&mut entities.facilities, WATER, facility(WATER, "water_station"));
    put(&mut entities.staff, "s0", staff("s0", &["meal"]));
    put(&mut entities.staff, "s1", staff("s1", &["water"]));
    *world.entities.get_mut() = entities;
    let mut fill = |name: &str, place, row: Box<dyn sheltersim_core::row::Row>| {
        let index = world.slots.add(text(name), place).unwrap();
        world.slots.at(index).unwrap().try_borrow_mut().unwrap().row = Some(row);
    };
    let mut counters = IndexMap::new();
    counters.insert(text("event"), V::Int(10));
    fill(ID_COUNTERS, Place::Field, Box::new(ValueRow(V::Map(counters))));
    fill(METRIC_STATE, Place::Field, Box::new(MetricRow::default()));
    fill(
        SERVICE,
        Place::Namespace,
        Box::new(ServiceRow {
            schema_version: 1,
            cancelled_task_ids: Vec::new(),
            active_reservation_ids: Vec::new(),
            request_history: Vec::new(),
            queue_stall_history: Vec::new(),
            stall_state: IdMap::default(),
        }),
    );
    let mut audit = Audit::with_capacity(4);
    audit.push(initial("water_ml", water)).unwrap();
    audit.push(initial("food_meal_units", 50)).unwrap();
    fill(
        LEDGER,
        Place::Namespace,
        Box::new(LedgerRow {
            schema_version: 1,
            warehouse_id: text("warehouse-main"),
            zone_id: None,
            accounts: Accounts { water_ml: account(water), food_meal_units: account(50) },
            used_reservation_ids: UsedIds::new(Vec::new()),
            audit,
        }),
    );
    let mut resources: ResourcesRow = IdMap::default();
    resources.insert(text("water_ml"), water);
    resources.insert(text("food_meal_units"), 50);
    fill(RESOURCES, Place::Field, Box::new(resources));
    world.hold_queue(Arc::new(Queue::new()));
    world.hold_log(Arc::new(EventLog::new()));
    world
}

/// The manager's config of `domain/resource_service.py`'s defaults.
fn config() -> IndexMap<Arc<str>, V> {
    let mut config = IndexMap::new();
    for (key, value) in [
        ("source_type", V::Str(text("assumption"))),
        ("source_id", V::Str(text("ASM-RESOURCE-SERVICE-V1"))),
        ("policy_version", V::Str(text("resource-service-v1"))),
        ("water_staff_skill", V::Str(text("water"))),
        ("meal_staff_skill", V::Str(text("meal"))),
        ("water_ml_per_service", V::Int(500)),
        ("food_meal_units_per_service", V::Int(1)),
        ("service_minutes", V::Int(3)),
        ("hydration_relief", V::Float(0.5)),
        ("hunger_relief", V::Float(0.5)),
    ] {
        config.insert(text(key), value);
    }
    config
}

fn manager<'a>(config: &'a IndexMap<Arc<str>, V>, geo: &'a Geo) -> Manager<'a> {
    Manager { config, service: Held::Held, ledger: Held::Held, queues: Held::Absent, geo, metric_state_held: true }
}

fn no_geo() -> Geo {
    Geo::new(None, None, None, None).unwrap()
}

fn no_needs() -> Kept<AccessibilityNeeds> {
    Ok(Arc::new(AccessibilityNeeds::parse(&V::Map(IndexMap::new())).unwrap()))
}

fn request<'a>(agent_id: &'a str, needs: &'a Kept<AccessibilityNeeds>) -> Request<'a> {
    Request { agent_id, facility_id: WATER, minute: Minute::Absent, queue_id: None, needs }
}

fn serve_at(minute: i64) -> Serve<'static> {
    Serve { facility_id: WATER, resource_id: "water_ml", minute, queue_id: None, walk: None, placement: None }
}

fn metric(world: &World, name: &str) -> f64 {
    world.metric_state().unwrap().get(name).copied().unwrap_or(0.0)
}

fn counter(world: &World) -> i64 {
    world.id_counter("event").unwrap().unwrap()
}

fn pending(world: &World) -> Vec<(Arc<str>, i64)> {
    let queue = world.queue().unwrap();
    queue.ordered().iter().map(|(key, serial)| (queue.slot(*serial).event_id.clone(), key.0)).collect()
}

fn logged(world: &World) -> usize {
    world.log().unwrap().len()
}

// ---- request -------------------------------------------------------------------------------------

#[test]
fn a_request_queues_the_person_with_a_receipt_and_its_record() {
    let mut world = world(1000);
    let (config, geo, needs) = (config(), no_geo(), no_needs());
    let requested = service::queue_up(&mut world, &manager(&config, &geo), &request("a1", &needs), &[]).unwrap();
    let Requested::Queued(receipt) = requested else { panic!("refused: {requested:?}") };
    assert_eq!((&*receipt.queue_task_id, receipt.queue_position, receipt.resource_id, receipt.requested_at), ("resource-service-task-0", 1, "water_ml", 5));
    assert_eq!(world.id_counter("resource_task").unwrap(), Some(1));
    let rows = world.rows().unwrap();
    assert_eq!(rows.facilities.get(WATER).unwrap().resource_queue_agent_ids, [text("a1")]);
    assert_eq!((rows.agents.get("a1").unwrap().activity, rows.agents.get("a1").unwrap().target_id.as_deref()), (Activity::Queued, Some(WATER)));
    assert_eq!(&*rows.active_tasks.get("resource-service-task-0").unwrap().kind, "resource_service_queue");
    drop(rows);
    let row = world.row::<ServiceRow>(SERVICE).unwrap();
    let record = &row.request_history[0];
    assert_eq!((&*record.action, &*record.outcome, &*record.reason, record.minute), ("fetch_water", "queued", "arrived_at_facility", 5));
}

#[test]
fn a_refused_request_gives_the_python_rule_s_text_with_python_s_repr() {
    let mut world = world(1000);
    let (config, geo, needs) = (config(), no_geo(), no_needs());
    let manager = manager(&config, &geo);
    let refused = |world: &mut World, request: &Request<'_>| match service::queue_up(world, &manager, request, &[]).unwrap() {
        Requested::Refused(text) => text,
        other => panic!("queued: {other:?}"),
    };
    assert_eq!(refused(&mut world, &request("it's", &needs)), "unknown Agent \"it's\"");
    assert_eq!(refused(&mut world, &Request { facility_id: TOILET, ..request("a1", &needs) }), "'toilet-1' is not a resource service facility");
    assert_eq!(refused(&mut world, &Request { minute: Minute::NotAnInt, ..request("a1", &needs) }), "resource request minute must be an integer");
    assert_eq!(refused(&mut world, &Request { minute: Minute::Int(4), ..request("a1", &needs) }), "resource request minute must match WorldState");
    service::queue_up(&mut world, &manager, &request("a1", &needs), &[]).unwrap();
    assert_eq!(refused(&mut world, &request("a1", &needs)), "Agent 'a1' cannot request resource service while active");
    // A pending reserve event of the next ID makes the counter skip it; one with no ID refuses.
    let pending = [Pending::Id(text("resource-service-task-1")), Pending::Other];
    let Requested::Queued(receipt) = service::queue_up(&mut world, &manager, &request("a2", &needs), &pending).unwrap() else { panic!() };
    assert_eq!(&*receipt.queue_task_id, "resource-service-task-2");
    let mut other = self::world(1000);
    assert_eq!(service::queue_up(&mut other, &manager, &request("a1", &needs), &[Pending::Missing]), Err(unsupported("a pending reservation has no ID")));
    assert!(other.row::<ServiceRow>(SERVICE).unwrap().request_history.is_empty());
}

#[test]
fn a_request_validates_the_world_through_the_host_before_and_after() {
    let cell = Shard::new(world(1000));
    let (config, geo, needs) = (config(), no_geo(), no_needs());
    let mut host = TestHost::default();
    let requested = service::request(&cell, &mut host, &manager(&config, &geo), &request("a1", &needs), &[], true).unwrap();
    assert!(matches!(requested, Requested::Queued(_)));
    assert_eq!(host.calls, [Call::ValidateWorld, Call::ValidateWorld]);
    // A refused request is not validated after; without `validate_state` the host is not asked.
    let mut host = TestHost::default();
    let requested = service::request(&cell, &mut host, &manager(&config, &geo), &request("a1", &needs), &[], true).unwrap();
    assert!(matches!(requested, Requested::Refused(_)));
    assert_eq!(host.calls, [Call::ValidateWorld]);
    let mut host = TestHost::default();
    service::request(&cell, &mut host, &manager(&config, &geo), &request("a2", &needs), &[], false).unwrap();
    assert!(host.calls.is_empty());
}

// ---- serve and complete --------------------------------------------------------------------------

/// The completion event `serve` scheduled, as the dispatcher hands it in.
fn due_event(world: &World, event_id: &str) -> EventRow {
    let queue = world.queue().unwrap();
    let serial = queue.serial_of(event_id).unwrap();
    (*queue.slot(serial).row.clone().unwrap()).clone()
}

#[test]
fn the_head_is_served_by_the_first_free_skilled_member_and_the_completion_consumes() {
    let mut world = world(1000);
    let (config, geo, needs) = (config(), no_geo(), no_needs());
    let manager = manager(&config, &geo);
    service::queue_up(&mut world, &manager, &request("a1", &needs), &[]).unwrap();
    let Served::Started(event_id, applied) = service::serve(&mut world, &manager, &serve_at(5), &[]).unwrap() else { panic!() };
    // The completion takes sequence 10, the reserve's audit 11.
    assert_eq!(&*event_id, "resource-service-complete-10");
    assert_eq!((applied.index, applied.remaining), (0, 1000));
    assert_eq!(counter(&world), 12);
    assert_eq!(pending(&world), [(text("resource-service-complete-10"), 8)]);
    assert_eq!(logged(&world), 1);
    {
        let rows = world.rows().unwrap();
        let task = rows.active_tasks.get("resource-service-task-0").unwrap();
        assert_eq!((&*task.kind, task.staff_id.as_deref(), task.due_minute), ("resource_service_water", Some("s1"), 8));
        let server = rows.staff.get("s1").unwrap();
        assert_eq!((server.work_queue.clone(), server.busy_until), (vec![text("resource-service-task-0")], 8));
        assert_eq!(rows.facilities.get(WATER).unwrap().resource_active_agent_ids, [text("a1")]);
        assert_eq!(rows.agents.get("a1").unwrap().activity, Activity::InService);
    }
    assert_eq!(world.row::<ServiceRow>(SERVICE).unwrap().active_reservation_ids, [text("resource-service-task-0")]);
    assert_eq!(world.row::<LedgerRow>(LEDGER).unwrap().accounts.water_ml.reserved, 500);
    assert_eq!((metric(&world, "water_service_wait_count"), metric(&world, "water_service_wait_total_minutes")), (1.0, 0.0));

    // The completion at minute 8.
    world.scalars.get_mut().minute_index = Fit::Ok(8);
    let event = due_event(&world, &event_id);
    assert_eq!(event.kind, EventKind::Completion);
    let applied = service::complete(&mut world, &manager, &event).unwrap();
    assert_eq!((applied.index, applied.remaining), (0, 500));
    let rows = world.rows().unwrap();
    assert!(rows.active_tasks.get("resource-service-task-0").is_none());
    assert_eq!(rows.agents.get("a1").unwrap().needs.hydration_need, 0.7 - 0.5);
    assert_eq!(rows.agents.get("a1").unwrap().activity, Activity::Idle);
    assert_eq!((rows.staff.get("s1").unwrap().active_task_id.clone(), rows.staff.get("s1").unwrap().busy_until), (None, 0));
    drop(rows);
    assert!(world.row::<ServiceRow>(SERVICE).unwrap().active_reservation_ids.is_empty());
    assert_eq!((metric(&world, "water_service_completed_count"), metric(&world, "resource_service_completed_count")), (1.0, 1.0));
    // A second time: the task is gone.
    assert!(matches!(
        service::complete(&mut world, &manager, &event),
        Err(Failure::Write(WriteError::Error(Error::Outcome(_, text)))) if text == "resource completion does not match an active task"
    ));
}

#[test]
fn a_waiting_head_is_recorded_once_per_reason() {
    let mut world = world(100);
    let (config, geo, needs) = (config(), no_geo(), no_needs());
    let manager = manager(&config, &geo);
    service::queue_up(&mut world, &manager, &request("a1", &needs), &[]).unwrap();
    // 100 ml in stock, 500 per service.
    assert_eq!(service::serve(&mut world, &manager, &serve_at(5), &[]).unwrap(), Served::Stopped(None));
    assert_eq!(service::serve(&mut world, &manager, &serve_at(5), &[]).unwrap(), Served::Stopped(None));
    let row = world.row::<ServiceRow>(SERVICE).unwrap();
    let stall = row.stall_state.get(WATER).unwrap();
    assert_eq!((&*stall.agent_id, &*stall.reason), ("a1", "stock"));
    assert_eq!(row.queue_stall_history.len(), 1);
    drop(row);
    assert_eq!(metric(&world, "resource_service_wait_stock_count"), 1.0);
    // Nobody with the skill: a staff wait.
    world.rows_mut().unwrap().staff.get_mut("s1").unwrap().skills = Skills(vec![text("meal")]);
    assert_eq!(service::serve(&mut world, &manager, &serve_at(5), &[]).unwrap(), Served::Stopped(None));
    assert_eq!(&*world.row::<ServiceRow>(SERVICE).unwrap().stall_state.get(WATER).unwrap().reason, "staff");
    // The pass of an independent facility starts from the stall the row holds.
    assert_eq!(service::previous_stall(&world, &Held::Held, WATER).unwrap().map(|stall| stall.reason), Some(text("staff")));
    assert_eq!(service::previous_stall(&world, &Held::Absent, WATER).err(), Some(Error::Key(SERVICE.to_owned())));
}

#[test]
fn a_ledger_refusal_takes_the_sequences_and_its_rule_wraps_the_text() {
    let mut world = world(1000);
    let (config, geo, needs) = (config(), no_geo(), no_needs());
    let manager = manager(&config, &geo);
    service::queue_up(&mut world, &manager, &request("a1", &needs), &[]).unwrap();
    // The task's ID is a used reservation ID: the Python rule's invariant, before anything.
    world.row_mut::<LedgerRow>(LEDGER).unwrap().used_reservation_ids.push(text("resource-service-task-0"));
    assert!(matches!(
        service::serve(&mut world, &manager, &serve_at(5), &[]),
        Err(Failure::Write(WriteError::Error(Error::Invariant(text)))) if text == "resource service task ID 'resource-service-task-0' conflicts with an H01 reservation"
    ));
    assert_eq!(counter(&world), 10);
    assert_eq!(Wrap::Consume.error("no reservation"), Error::failed("resource consume rejected: no reservation"));
    let fault = LedgerFault::Text("x".to_owned());
    assert_eq!(fault.text(|_| String::new()), "x");
}

#[test]
fn a_cancel_of_a_queued_task_lets_the_person_go_and_lists_the_task() {
    let mut world = world(1000);
    let (config, geo, needs) = (config(), no_geo(), no_needs());
    let manager = manager(&config, &geo);
    service::queue_up(&mut world, &manager, &request("a1", &needs), &[]).unwrap();
    let mut payload = IndexMap::new();
    payload.insert(text("task_id"), V::Str(text("resource-service-task-0")));
    payload.insert(text("reason"), V::Str(text("operator_cancelled")));
    let event = EventRow::new(text("cancel-1"), EventKind::Intervention, 5, 0, 3, text("resource_service_cancel"), payload, None, None, IndexMap::new()).unwrap();
    assert_eq!(service::cancel(&mut world, &manager, &event).unwrap(), None);
    let rows = world.rows().unwrap();
    assert!(rows.facilities.get(WATER).unwrap().resource_queue_agent_ids.is_empty());
    assert_eq!(rows.agents.get("a1").unwrap().activity, Activity::Idle);
    drop(rows);
    assert_eq!(world.row::<ServiceRow>(SERVICE).unwrap().cancelled_task_ids, [text("resource-service-task-0")]);
    assert!(matches!(
        service::cancel(&mut world, &manager, &event),
        Err(Failure::Write(WriteError::Error(Error::Outcome(_, text)))) if text == "resource service task 'resource-service-task-0' was already cancelled"
    ));
}

#[test]
fn an_interrupted_service_puts_the_person_back_at_the_head_with_a_new_task() {
    let mut world = world(1000);
    let (config, geo, needs) = (config(), no_geo(), no_needs());
    let manager = manager(&config, &geo);
    service::queue_up(&mut world, &manager, &request("a1", &needs), &[]).unwrap();
    service::queue_up(&mut world, &manager, &request("a2", &needs), &[]).unwrap();
    let Served::Started(..) = service::serve(&mut world, &manager, &serve_at(5), &[]).unwrap() else { panic!() };
    let cause = text("staff-absent-1");
    let applied = service::interrupt(&mut world, &manager, &[], "s1", Some(&cause)).unwrap().unwrap();
    assert_eq!((applied.index, applied.remaining), (0, 1000));
    assert_eq!(world.row::<LedgerRow>(LEDGER).unwrap().accounts.water_ml.reserved, 0);
    let rows = world.rows().unwrap();
    assert_eq!(rows.facilities.get(WATER).unwrap().resource_queue_agent_ids, [text("a1"), text("a2")]);
    assert_eq!(&*rows.active_tasks.get("resource-service-task-2").unwrap().kind, "resource_service_queue");
    assert_eq!(rows.agents.get("a1").unwrap().activity, Activity::Queued);
    drop(rows);
    let row = world.row::<ServiceRow>(SERVICE).unwrap();
    assert_eq!(row.cancelled_task_ids, [text("resource-service-task-0")]);
    assert_eq!(row.request_history.last().unwrap().detail.as_deref(), Some("service_requeued_after_staff_unavailable"));
    drop(row);
    // A member who serves nobody: nothing.
    assert_eq!(service::interrupt(&mut world, &manager, &[], "s0", Some(&cause)).unwrap(), None);
}

#[test]
fn a_tainted_service_namespace_is_refused() {
    let mut world = world(1000);
    let (config, geo, needs) = (config(), no_geo(), no_needs());
    let manager = Manager { service: Held::Other("the world holds another service state"), ..manager(&config, &geo) };
    assert_eq!(service::queue_up(&mut world, &manager, &request("a1", &needs), &[]), Err(unsupported("the world holds another service state")));
}

// ---- toilet --------------------------------------------------------------------------------------

fn toilet_request<'a>(agent_id: &'a str, geo: &'a Geo, needs: &'a Kept<AccessibilityNeeds>) -> toilet::Request<'a> {
    toilet::Request { agent_id, facility_id: TOILET, queue_id: None, queues: &Held::Absent, geo, needs }
}

fn start<'a>(rng: &'a Rng, geo: &'a Geo) -> Start<'a> {
    Start {
        facility_id: TOILET,
        minute: 5,
        queue_id: None,
        lowest: 4,
        highest: 8,
        policy: text("toilet-service-uniform-v1"),
        rng,
        queues: &Held::Absent,
        geo,
        walk: None,
        metric_state_held: true,
    }
}

#[test]
fn a_toilet_request_folds_the_kind_and_hashes_the_task_id() {
    let world = world(1000);
    let (geo, needs) = (no_geo(), no_needs());
    let receipt = toilet::queue_up(&world, &toilet_request("a1", &geo, &needs)).unwrap();
    assert_eq!(receipt.queue_task_id, toilet::queue_task_id(TOILET, "a1", 5));
    assert!(receipt.queue_task_id.starts_with("toilet-queue-") && receipt.queue_task_id.len() == 13 + 24);
    assert_eq!(receipt.queue_position, 1);
    assert!(matches!(
        toilet::queue_up(&world, &toilet_request("a1", &geo, &needs)),
        Err(WriteError::Error(Error::Value(text))) if text == "Agent 'a1' cannot request a toilet while active"
    ));
    assert!(matches!(
        toilet::queue_up(&world, &toilet::Request { facility_id: WATER, ..toilet_request("a2", &geo, &needs) }),
        Err(WriteError::Error(Error::Value(text))) if text == "'water-1' is not a toilet Facility"
    ));
}

#[test]
fn a_toilet_service_draws_its_length_from_the_counter_rng_and_completes() {
    let cell = Shard::new(world(1000));
    let (geo, needs) = (no_geo(), no_needs());
    cell.with(|world| toilet::queue_up(world, &toilet_request("a1", &geo, &needs)).unwrap()).unwrap();
    let rng = Rng::Seed("42".to_owned());
    let mut host = TestHost::default();
    let Started::Event(event_id) = toilet::start(&cell, &mut host, &start(&rng, &geo)).unwrap() else { panic!() };
    assert!(host.calls.is_empty());
    let mut key = Key::labelled(label::TOILET_SERVICE);
    key.text(TOILET).text("a1").text("minute").int(5).text("toilet-service-uniform-v1").text("duration");
    let draw = key.draw(&KeyedDraws::new("42"));
    let minutes = (4 + (draw * 5.0) as i64).min(8);
    assert_eq!(&*event_id, "toilet-complete-10");
    let event = cell.with(|world| due_event(world, &event_id)).unwrap();
    assert_eq!(event.due_minute, 5 + minutes);
    let V::Map(provenance) = &event.provenance else { panic!() };
    assert_eq!(provenance["rng_draw"], V::Float(draw));
    assert_eq!(provenance["service_minutes"], V::Int(minutes));
    // The completion lets the person go and asks for the walk home.
    let returned = cell
        .with(|world| {
            world.scalars.get_mut().minute_index = Fit::Ok(event.due_minute);
            toilet::complete(world, &event, &Held::Absent, &geo, &Held::Absent, true)
        })
        .unwrap();
    assert_eq!(returned.unwrap(), ScheduleReturn);
    cell.with(|world| {
        let rows = world.rows().unwrap();
        assert!(rows.facilities.get(TOILET).unwrap().active_agent_ids.is_empty());
        assert_eq!((rows.agents.get("a1").unwrap().activity, rows.agents.get("a1").unwrap().needs.toilet_urgency), (Activity::Idle, 0.0));
        drop(rows);
        assert_eq!(metric(world, "toilet_service_completed_count"), 1.0);
        assert_eq!(metric(world, "toilet_wait_count"), 1.0);
    })
    .unwrap();
}

#[test]
fn another_random_source_is_drawn_by_the_host_with_the_world_let_go() {
    let cell = Shard::new(world(1000));
    let (geo, needs) = (no_geo(), no_needs());
    cell.with(|world| toilet::queue_up(world, &toilet_request("a1", &geo, &needs)).unwrap()).unwrap();
    let rng = Rng::Host;
    // A host that cannot draw refuses, with nothing written.
    let mut host = TestHost::default();
    assert_eq!(toilet::start(&cell, &mut host, &start(&rng, &geo)), Err(unsupported("not a CounterRNG")));
    let parts = vec![
        Part::Text(text("toilet-service")),
        Part::Text(text(TOILET)),
        Part::Text(text("a1")),
        Part::Text(text("minute")),
        Part::Int(5),
        Part::Text(text("toilet-service-uniform-v1")),
        Part::Text(text("duration")),
    ];
    assert_eq!(host.calls, [Call::Draw(parts.clone())]);
    assert_eq!(cell.with(|world| counter(world)).unwrap(), 10);
    // A draw outside 0..1 is the rule's ValueError; 0.5 gives 4 + 2 minutes.
    let mut host = TestHost { draws: vec![1.0], ..TestHost::default() };
    assert_eq!(toilet::start(&cell, &mut host, &start(&rng, &geo)), Err(Error::Value("keyed RNG draw must be between zero and one".to_owned())));
    let mut host = TestHost { draws: vec![0.5], ..TestHost::default() };
    let Started::Event(event_id) = toilet::start(&cell, &mut host, &start(&rng, &geo)).unwrap() else { panic!() };
    assert_eq!(cell.with(|world| due_event(world, &event_id).due_minute).unwrap(), 11);
}
