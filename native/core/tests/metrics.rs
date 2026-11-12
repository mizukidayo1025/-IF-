//! The pilot kernel of lane 5-4: the KPI columns of a minute (`rules::metrics::sample`) over a
//! `World` built here, with no Python; and the `WorldCell` of a world Rust holds alone. Since lane
//! P8 b4 the open requests' waits are the staff rules' over the rows, and the metrics phase gives
//! the sample to the host (`metrics::capture`).
//!
//! The Python test `tests/integration/test_entity_rules_kernel.py` checks the same function, through
//! the PyO3 glue, against the digest of the columns the Python rule gave on the worlds of a Run.

mod support;

use std::sync::Arc;

use indexmap::IndexMap;
use sheltersim_core::cell::{Shard, BORROWED};
use sheltersim_core::config::movement::Movement;
use sheltersim_core::entities::{
    Activity, AgentRow, AnyTable, FacilityRow, Mobility, NeedRow, ProfileRow, RequestRow, RequestStatus, Skills, StaffRow, Table, TaskRow, ZoneRow,
};
use sheltersim_core::error::Error;
use sheltersim_core::events::log::EventLog;
use sheltersim_core::events::queue::Queue;
use sheltersim_core::events::{EventKind, EventRow, EventStatus};
use sheltersim_core::row::{IdMap, ValueRow};
use sheltersim_core::rules::after_tick::Memo;
use sheltersim_core::rules::kit::{is_toilet, Held, Kept};
use sheltersim_core::rules::metrics::{self, Captured, Inputs, MetricSample, Requests, EXPOSURE_SEMANTICS};
use sheltersim_core::rules::staff::{self, WaitRoutes};
use sheltersim_core::state::metric_state::MetricRow;
use sheltersim_core::value::V;
use sheltersim_core::world::{Fit, Place, World, WorldCell, ID_COUNTERS, METRIC_STATE, MODEL_CONFIGS};
use support::{Call, TestHost};

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

fn ids(values: &[&str]) -> Vec<Arc<str>> {
    values.iter().map(|value| text(value)).collect()
}

fn needs() -> NeedRow {
    NeedRow { toilet_urgency: 0.0, hunger: 0.0, fatigue: 0.0, hydration_need: 0.0 }
}

fn agent(id: &str, stress: f64) -> AgentRow {
    AgentRow {
        profile: ProfileRow {
            id: text(id),
            group_id: None,
            initial_zone_id: text("z0"),
            mobility: Mobility::Independent,
            support_need: 0.0,
            stress_sensitivity: 1.0,
            rng_key: text(id),
            initial_needs: needs(),
            provenance: V::Map(IndexMap::new()),
        },
        zone_id: text("z0"),
        activity: Activity::Idle,
        target_id: None,
        needs: needs(),
        stress,
        trust: 0.5,
        next_decision_at: 0,
        support_request_id: None,
        support_cooldown_until: 0,
    }
}

fn facility(id: &str, kind: &str, working_units: i64, queue: &[&str], active: &[&str]) -> FacilityRow {
    FacilityRow {
        id: text(id),
        kind: text(kind),
        zone_id: text("z0"),
        capacity: 2,
        working_units,
        active_agent_ids: ids(active),
        queue_agent_ids: ids(queue),
        resource_active_agent_ids: Vec::new(),
        resource_queue_agent_ids: Vec::new(),
    }
}

fn staff(id: &str, queue: &[&str]) -> StaffRow {
    StaffRow {
        id: text(id),
        zone_id: text("z0"),
        // No one has the skill emotional support needs (`support`).
        skills: Skills(ids(&["triage"])),
        work_queue: ids(queue),
        busy_until: 0,
        active_task_id: None,
    }
}

fn request(id: &str, agent: &str, requested: i64, status: RequestStatus, task: Option<&str>) -> RequestRow {
    RequestRow {
        id: text(id),
        agent_id: text(agent),
        requested_minute: requested,
        timeout_minute: requested + 90,
        status,
        assigned_staff_id: task.map(|_| text("s2")),
        task_id: task.map(text),
        resolved_minute: None,
        cooldown_until: 0,
        last_assignment_attempt_minute: None,
        last_assignment_wait_reason_code: None,
    }
}

fn task(id: &str, start_minute: i64, due_minute: i64) -> TaskRow {
    TaskRow {
        id: text(id),
        kind: text("support"),
        agent_id: None,
        resource_id: None,
        start_minute,
        due_minute,
        staff_id: None,
    }
}

fn put<R: Clone>(table: &mut Table<R>, id: &str, row: R) {
    let slot = table.reserve_slot();
    table.fill_slot(slot, row);
    assert_eq!(table.bind(text(id), slot), None);
}

/// A world at minute 30 of 100 that holds its queue, log and event counter: agents in this dict
/// order (their stresses sum to 1.7 only with Python's compensated `sum`), two toilets of three
/// facilities, two staff members, one of them busy, and four support requests: one assigned, two
/// pending of a kind no one is qualified for, one settled.
fn world() -> World {
    let mut world = bare();
    let index = world.slots.add(text(METRIC_STATE), Place::Field).unwrap();
    let mut counters: MetricRow = IdMap::default();
    counters.insert(text("toilet_wait_count"), 3.0);
    counters.insert(text("toilet_wait_total_minutes"), 7.0);
    counters.insert(text("toilet_wait_max_minutes"), 4.0);
    counters.insert(text("support_wait_staff_busy_request_minutes"), 2.5);
    world.slots.at(index).unwrap().try_borrow_mut().unwrap().row = Some(Box::new(counters));
    world
}

/// `world` without its `metric_state`.
fn bare() -> World {
    let mut world = World::default();
    world.scalars.get_mut().duration_minutes = Fit::Ok(100);
    world.scalars.get_mut().minute_index = Fit::Ok(30);
    {
        let rows = world.entities.get_mut();
        for (id, stress) in [("a3", 1e16), ("a1", 1.0), ("a4", -1e16), ("a2", 0.7)] {
            put(&mut rows.agents, id, agent(id, stress));
        }
        put(&mut rows.facilities, "f2", facility("f2", "Toilet-B", 2, &["a4", "a2"], &["a3"]));
        put(&mut rows.facilities, "f1", facility("f1", "storage", 5, &["a1"], &[]));
        put(&mut rows.facilities, "f3", facility("f3", "toilet", 1, &["a1"], &["a2"]));
        put(&mut rows.staff, "s2", staff("s2", &["t2", "t1"]));
        put(&mut rows.staff, "s1", staff("s1", &["t3"]));
        put(&mut rows.active_tasks, "t1", task("t1", 10, 40));
        put(&mut rows.active_tasks, "t2", task("t2", 40, 50));
        put(&mut rows.active_tasks, "t3", task("t3", 0, 30));
        put(&mut rows.zones, "z0", ZoneRow { id: text("z0"), name: text("hall"), capacity: None, display_x: None, display_y: None });
        put(&mut rows.support_requests, "r1", request("r1", "a2", 5, RequestStatus::Assigned, Some("t1")));
        put(&mut rows.support_requests, "r2", request("r2", "a1", 30, RequestStatus::Pending, None));
        put(&mut rows.support_requests, "r0", request("r0", "a3", 1, RequestStatus::Resolved, None));
        put(&mut rows.support_requests, "r3", request("r3", "a2", 28, RequestStatus::Pending, None));
    }
    let mut counter = IndexMap::new();
    counter.insert(text("event"), V::Int(40));
    add(&mut world, ID_COUNTERS, ValueRow(V::Map(counter)));
    add(&mut world, MODEL_CONFIGS, ValueRow(V::Map(IndexMap::new())));
    world.hold_queue(Arc::new(Queue::new()));
    world.hold_log(Arc::new(log()));
    world
}

fn add(world: &mut World, name: &str, row: ValueRow) {
    let index = world.slots.add(text(name), Place::Field).unwrap();
    world.slots.at(index).unwrap().try_borrow_mut().unwrap().row = Some(Box::new(row));
}

fn event(event_id: &str, sequence: i64) -> EventRow {
    EventRow::new(
        text(event_id),
        EventKind::Endogenous,
        30,
        0,
        sequence,
        text("test_event"),
        IndexMap::new(),
        None,
        None,
        IndexMap::new(),
    )
    .expect("a valid row")
}

fn log() -> EventLog {
    let mut log = EventLog::new();
    for (id, sequence) in [("e-b", 1), ("e-a", 2)] {
        assert_eq!(log.append_row(&event(id, sequence), 30, EventStatus::Applied, None), Ok(0));
    }
    log
}

const ABSENT: Held = Held::Absent;

/// What the staff rules read of the Python world: no namespace held.
fn staff_inputs(held: bool) -> staff::Inputs<'static> {
    staff::Inputs { operations: &ABSENT, dynamics: &ABSENT, body: &ABSENT, shared: &ABSENT, metric_state_held: held, tasks: None }
}

fn sample_with(world: &World, held: bool, requests: Requests<'_>) -> Result<Option<MetricSample>, Error> {
    let staff = staff_inputs(held);
    metrics::sample(world, &Inputs { sample: 30, requests, staff: &staff, routes: None })
}

fn sample_of(world: &World, held: bool) -> Result<MetricSample, Error> {
    sample_with(world, held, Requests::Unsettled(None)).map(|taken| taken.expect("no routes are read"))
}

fn unsupported(reason: &str) -> Error {
    Error::Unsupported(reason.to_owned().into())
}

#[test]
fn the_columns_are_those_of_the_python_rule() {
    let taken = sample_of(&world(), true).unwrap();
    let source_ids = format!(
        concat!(
            r#"{{"busy_staff":["s2"],"high_stress_agents":["a1","a2","a3"],"realized_events":["e-a","e-b"],"#,
            r#""staff_tasks":["t1","t2","t3"],"support_requests_without_qualified_staff":["a1","a2"],"#,
            r#""support_wait_by_agent":{{"a1":{{"reason_codes":["no_qualified_staff"],"request_id":"r2","waited_minutes":0}},"#,
            r#""a2":{{"reason_codes":["no_qualified_staff"],"request_id":"r3","waited_minutes":2}}}},"#,
            r#""support_wait_exposure_semantics":"{}","toilet_active_agents":["a2","a3"],"#,
            r#""toilet_queue_agents":["a1","a2","a4"],"unresolved_support_agents":["a1","a2","a2"]}}"#
        ),
        EXPOSURE_SEMANTICS
    );
    assert_eq!(
        taken,
        MetricSample {
            toilet_queue_length: 3,
            toilet_active_users: 2,
            toilet_working_units: 3,
            toilet_wait_count: 3.0,
            toilet_wait_total_minutes: 7.0,
            toilet_wait_mean_minutes: 7.0 / 3.0,
            toilet_wait_max_minutes: 4.0,
            // `sum([1e16, 1.0, -1e16, 0.7]) / 4` of CPython 3.12; a plain left fold gives 0.175.
            stress_mean: 0.425,
            stress_high_count: 3,
            staff_assigned_task_count: 3,
            staff_busy_count: 1,
            staff_load_ratio: 0.5,
            unresolved_support_request_count: 3,
            source_ids_json: source_ids,
            support_no_qualified_worker_count: 2,
            support_no_qualified_worker_request_minutes: 0.0,
            support_fairness_promoted_assignment_count: 0.0,
            support_wait_staff_busy_request_minutes: 2.5,
            support_wait_staff_off_shift_request_minutes: 0.0,
        }
    );
    assert_eq!(taken.stress_mean.to_bits(), 0.425_f64.to_bits());
    // The memo of an end-of-tick check that knows nothing gives every request, as no memo does.
    let memo = Memo::default();
    assert_eq!(sample_with(&world(), true, Requests::Unsettled(Some(&memo))).unwrap(), Some(taken));
}

#[test]
fn a_list_of_requests_is_read_in_its_order() {
    // The assigned request after the later pending one: `support_wait_by_agent` keeps the last.
    let given = ids(&["r3", "r1", "r0"]);
    let taken = sample_with(&world(), true, Requests::Given(&given)).unwrap().unwrap();
    assert!(taken.source_ids_json.contains(r#""a2":{"reason_codes":["support_in_progress"],"request_id":"r1","waited_minutes":5}"#));
    assert_eq!((taken.unresolved_support_request_count, taken.support_no_qualified_worker_count), (2, 1));
    let unknown = ids(&["r9"]);
    assert_eq!(sample_with(&world(), true, Requests::Given(&unknown)).err(), Some(unsupported("a request has no row")));
}

#[test]
fn the_metrics_phase_gives_the_sample_to_the_host_and_reads_routes_only_where_asked() {
    let cell = Shard::new(world());
    let mut host = TestHost::default();
    let staff = staff_inputs(true);
    let inputs = Inputs { sample: 30, requests: Requests::Unsettled(None), staff: &staff, routes: None };
    assert_eq!(metrics::capture(&cell, &mut host, &inputs), Ok(Captured::Taken));
    let expected = cell.with(|world| sample_of(world, true)).unwrap().unwrap();
    assert_eq!(host.calls, [Call::Metric(expected, 30)]);

    // A free worker who could take the pending requests: their routes are read.
    cell.with(|world| world.rows_mut().unwrap().staff.get_mut("s1").unwrap().skills = Skills(ids(&["support"]))).unwrap();
    let mut host = TestHost::default();
    assert_eq!(metrics::capture(&cell, &mut host, &inputs), Ok(Captured::NeedsRoutes));
    assert!(host.calls.is_empty());
    let movement: Kept<Movement> = Movement::parse(&V::Map(IndexMap::new())).map(Arc::new);
    let minutes: Kept<i64> = Ok(Arc::new(10));
    let routes = WaitRoutes { loads_staff: &ABSENT, geo: None, movement: &movement, support_minutes: &minutes };
    let inputs = Inputs { routes: Some(&routes), ..inputs };
    assert_eq!(metrics::capture(&cell, &mut host, &inputs), Ok(Captured::Taken));
    let [Call::Metric(taken, 30)] = host.calls.as_slice() else {
        panic!("one sample: {:?}", host.calls);
    };
    assert_eq!(taken.support_no_qualified_worker_count, 0);
    assert!(taken.source_ids_json.contains(r#""a1":{"reason_codes":["assignment_not_attempted_at_sampled_minute"]"#));
}

#[test]
fn an_empty_world_has_means_of_zero() {
    let mut world = World::default();
    world.scalars.get_mut().duration_minutes = Fit::Ok(10);
    let index = world.slots.add(text(METRIC_STATE), Place::Field).unwrap();
    world.slots.at(index).unwrap().try_borrow_mut().unwrap().row = Some(Box::new(MetricRow::default()));
    add(&mut world, ID_COUNTERS, ValueRow(V::Map(IndexMap::new())));
    world.hold_queue(Arc::new(Queue::new()));
    world.hold_log(Arc::new(EventLog::new()));
    let staff = staff_inputs(true);
    let taken = metrics::sample(&world, &Inputs { sample: 0, requests: Requests::Unsettled(None), staff: &staff, routes: None }).unwrap().unwrap();
    assert_eq!((taken.stress_mean, taken.toilet_wait_mean_minutes, taken.staff_load_ratio), (0.0, 0.0, 0.0));
}

#[test]
fn a_kind_that_is_not_ascii_is_folded_as_python_folds_it() {
    let mut world = world();
    {
        let rows = world.entities.get_mut();
        put(&mut rows.facilities, "f4", facility("f4", "ﬆoilet", 4, &["a3"], &[]));
        put(&mut rows.facilities, "f5", facility("f5", "TOİLET", 8, &["a4"], &[]));
        put(&mut rows.facilities, "f6", facility("f6", "ﬆoilet", 16, &[], &[]));
    }
    // `str.casefold` gives "stoilet" and "toi̇let" (P8-0a: the core's fold, Python's before).
    assert_eq!(sample_of(&world, true).unwrap().toilet_working_units, 3 + 4 + 16);
    assert!(is_toilet("A-TOILET-room"));
    assert!(is_toilet("ﬆoilet"));
    assert!(!is_toilet("TOİLET"));
}

#[test]
fn what_the_kernel_refused_it_still_refuses_with_its_text() {
    let mut world = world();
    world.entities.get_mut().active_tasks = Table::default();
    assert_eq!(sample_of(&world, true).err(), Some(unsupported("a staff queue names no task")));

    let mut world = self::world();
    world.entities.get_mut().taint("a value does not fit".to_owned());
    assert_eq!(sample_of(&world, true).err(), Some(unsupported("the entity store is tainted")));

    let mut world = self::world();
    put(&mut world.entities.get_mut().facilities, "f9", facility("f9", "toilet", i64::MAX, &[], &[]));
    assert_eq!(
        sample_of(&world, true).err(),
        Some(unsupported("the working units do not fit an i64"))
    );

    let world = self::world();
    assert_eq!(sample_of(&world, false).err(), Some(unsupported("the field holds another dict")));
    world.slots.get(METRIC_STATE).unwrap().try_borrow_mut().unwrap().misfit("an int".to_owned());
    assert_eq!(sample_of(&world, true).err(), Some(unsupported("the namespace has no row")));

    assert_eq!(sample_of(&bare(), false).err(), Some(unsupported("the namespace is not owned")));

    // A world whose EventLog or queue is held elsewhere (lane 6-5), or whose event counter a rule
    // cannot add to, as the PyO3 crate's `EventWriter::of` refused it before.
    let mut world = self::world();
    assert!(world.release_log().is_some());
    assert_eq!(sample_of(&world, true).err(), Some(unsupported("the event log is not in Rust")));
    let mut world = self::world();
    assert!(world.release_queue().is_some());
    assert_eq!(sample_of(&world, true).err(), Some(unsupported("the event queue is not in Rust")));
}

#[test]
fn a_world_cell_lends_the_whole_world_once_at_a_time() {
    let cell = Shard::new(world());
    let taken = cell.with(|world| sample_of(world, true)).unwrap().unwrap();
    assert_eq!(taken.staff_busy_count, 1);
    let held = cell.try_borrow().unwrap();
    assert_eq!(cell.with(|_| ()).err(), Some(Error::Runtime(BORROWED.to_owned())));
    drop(held);
    // Inside, a part is borrowed as the accessors borrow it, and given back.
    cell.with(|world| {
        let rows = world.rows().unwrap();
        assert_eq!(world.rows().unwrap().agents.len(), rows.agents.len());
        assert!(world.rows_mut().is_err());
    })
    .unwrap();
}

/// `operational_kpi_values`' reads of the rows (lane P10-1c): the needs' means over the people in
/// their order, the water and meal queues, and each part's own refusal, in the rule's order.
#[test]
fn the_kpi_reads_are_those_of_the_python_rule() {
    let mut world = world();
    {
        let rows = world.entities.get_mut();
        for (index, id) in ["a3", "a1", "a4", "a2"].into_iter().enumerate() {
            let needs = &mut rows.agents.get_mut(id).unwrap().needs;
            needs.toilet_urgency = [1e16, 1.0, -1e16, 0.7][index];
            needs.hunger = 0.25 * index as f64;
            needs.hydration_need = 0.5;
        }
        let mut water = facility("w1", "water_station", 1, &[], &[]);
        water.resource_queue_agent_ids = ids(&["a1", "a2"]);
        water.resource_active_agent_ids = ids(&["a3"]);
        let mut meal = facility("m1", "meal_distribution", 1, &[], &[]);
        meal.resource_queue_agent_ids = ids(&["a4"]);
        put(&mut rows.facilities, "w1", water);
        put(&mut rows.facilities, "m1", meal);
    }
    {
        let slot = world.slots.get(METRIC_STATE).unwrap();
        let mut slot = slot.try_borrow_mut().unwrap();
        let row = slot.row.as_deref_mut().unwrap().as_any_mut().downcast_mut::<MetricRow>().unwrap();
        row.insert(text("water_service_wait_count"), 2.0);
        row.insert(text("water_service_wait_total_minutes"), 9.0);
        row.insert(text("water_service_wait_max_minutes"), 6.0);
        row.insert(text("resource_service_wait_stock_count"), 1.0);
    }
    let read = metrics::kpi_rows(&world, &Held::Absent, true).unwrap();
    // Python's compensated `sum`: 1e16 + 1.0 - 1e16 + 0.7 is 1.7, not 0.7.
    assert_eq!(read.means, Some([1.7 / 4.0, 1.5 / 4.0, 0.5]));
    assert_eq!(read.queues, [2, 1, 1, 0]);
    assert_eq!(read.reserved, Err(unsupported("the world holds another ledger")));
    assert_eq!(read.metrics, Ok(([(2.0, 9.0, 6.0), (0.0, 0.0, 0.0)], 0.0, 1.0)));

    let read = metrics::kpi_rows(&world, &Held::Other("model_state is another kind of mapping"), false).unwrap();
    assert_eq!(read.reserved, Err(unsupported("model_state is another kind of mapping")));
    assert_eq!(read.metrics, Err(unsupported("the field holds another dict")));
    // The world's ledger is not a namespace this world owns.
    assert_eq!(metrics::kpi_rows(&world, &Held::Held, true).unwrap().reserved, Err(unsupported("the namespace is not owned")));

    // No one: the Python rule's mean divides by zero, which the PyO3 crate raises.
    let mut empty = self::world();
    empty.entities.get_mut().agents = Table::default();
    assert_eq!(metrics::kpi_rows(&empty, &Held::Absent, true).unwrap().means, None);

    let mut tainted = self::world();
    tainted.entities.get_mut().taint("a value does not fit".to_owned());
    assert_eq!(metrics::kpi_rows(&tainted, &Held::Absent, true).err(), Some(unsupported("the entity store is tainted")));
}
