//! The pilot kernel of lane 5-4: the KPI columns of a minute (`rules::metrics::sample`) over a
//! `World` built here, with no Python; and the `WorldCell` of a world Rust holds alone.
//!
//! The Python test `tests/integration/test_entity_rules_kernel.py` checks the same function, through
//! the PyO3 glue, against the digest of the columns the Python rule gave on the worlds of a Run.

use std::sync::Arc;

use indexmap::IndexMap;
use sheltersim_core::cell::{Shard, BORROWED};
use sheltersim_core::entities::{
    Activity, AgentRow, AnyTable, FacilityRow, Mobility, NeedRow, ProfileRow, Skills, StaffRow, Table, TaskRow,
};
use sheltersim_core::error::Error;
use sheltersim_core::events::log::EventLog;
use sheltersim_core::events::{EventKind, EventRow, EventStatus};
use sheltersim_core::row::IdMap;
use sheltersim_core::rules::metrics::{self, Folds, Inputs, MetricSample, OpenWait, EXPOSURE_SEMANTICS};
use sheltersim_core::state::metric_state::MetricRow;
use sheltersim_core::value::V;
use sheltersim_core::world::{Place, World, WorldCell, METRIC_STATE};

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
        skills: Skills(ids(&["triage"])),
        work_queue: ids(queue),
        busy_until: 0,
        active_task_id: None,
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

fn put<R>(table: &mut Table<R>, id: &str, row: R) {
    let slot = table.reserve_slot();
    table.fill_slot(slot, row);
    assert_eq!(table.bind(text(id), slot), None);
}

/// A world at minute 30: agents in this dict order (their stresses sum to 1.7 only with Python's
/// compensated `sum`), two toilets of three facilities, two staff members, one of them busy.
fn world() -> World {
    let mut world = World::default();
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
    }
    let index = world.slots.add(text(METRIC_STATE), Place::Field).unwrap();
    let mut counters: MetricRow = IdMap::default();
    counters.insert(text("toilet_wait_count"), 3.0);
    counters.insert(text("toilet_wait_total_minutes"), 7.0);
    counters.insert(text("toilet_wait_max_minutes"), 4.0);
    counters.insert(text("support_wait_staff_busy_request_minutes"), 2.5);
    world.slots.at(index).unwrap().try_borrow_mut().unwrap().row = Some(Box::new(counters));
    world
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

fn waits() -> Vec<OpenWait<'static>> {
    vec![
        OpenWait { agent: "a2", request_id: "r1", waited: 5, reasons: vec!["staff_busy"], without_qualified_staff: false },
        OpenWait {
            agent: "a1",
            request_id: "r2",
            waited: 0,
            reasons: vec!["no_qualified_staff"],
            without_qualified_staff: true,
        },
        // A later request of the same person is the one `support_wait_by_agent` keeps.
        OpenWait { agent: "a2", request_id: "r3", waited: 2, reasons: vec!["a", "b"], without_qualified_staff: false },
    ]
}

fn sample_of(world: &World, folds: &Folds, held: bool) -> Result<MetricSample, Error> {
    let log = log();
    let waits = waits();
    metrics::sample(world, &Inputs { sample: 30, waits: &waits, log: &log, folds, metric_state_held: held })
}

fn unsupported(reason: &str) -> Error {
    Error::Unsupported(reason.to_owned().into())
}

#[test]
fn the_columns_are_those_of_the_python_rule() {
    let taken = sample_of(&world(), &Folds::default(), true).unwrap();
    let source_ids = format!(
        concat!(
            r#"{{"busy_staff":["s2"],"high_stress_agents":["a1","a2","a3"],"realized_events":["e-a","e-b"],"#,
            r#""staff_tasks":["t1","t2","t3"],"support_requests_without_qualified_staff":["a1"],"#,
            r#""support_wait_by_agent":{{"a1":{{"reason_codes":["no_qualified_staff"],"request_id":"r2","waited_minutes":0}},"#,
            r#""a2":{{"reason_codes":["a","b"],"request_id":"r3","waited_minutes":2}}}},"#,
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
            support_no_qualified_worker_count: 1,
            support_no_qualified_worker_request_minutes: 0.0,
            support_fairness_promoted_assignment_count: 0.0,
            support_wait_staff_busy_request_minutes: 2.5,
            support_wait_staff_off_shift_request_minutes: 0.0,
        }
    );
    assert_eq!(taken.stress_mean.to_bits(), 0.425_f64.to_bits());
}

#[test]
fn an_empty_world_has_means_of_zero() {
    let mut world = World::default();
    let index = world.slots.add(text(METRIC_STATE), Place::Field).unwrap();
    world.slots.at(index).unwrap().try_borrow_mut().unwrap().row = Some(Box::new(MetricRow::default()));
    let log = EventLog::new();
    let taken = metrics::sample(
        &world,
        &Inputs { sample: 0, waits: &[], log: &log, folds: &Folds::default(), metric_state_held: true },
    )
    .unwrap();
    assert_eq!((taken.stress_mean, taken.toilet_wait_mean_minutes, taken.staff_load_ratio), (0.0, 0.0, 0.0));
}

#[test]
fn a_kind_that_is_not_ascii_is_folded_by_the_caller() {
    let mut world = world();
    {
        let rows = world.entities.get_mut();
        put(&mut rows.facilities, "f4", facility("f4", "ﬆoilet", 4, &["a3"], &[]));
        put(&mut rows.facilities, "f5", facility("f5", "TOİLET", 8, &["a4"], &[]));
        put(&mut rows.facilities, "f6", facility("f6", "ﬆoilet", 16, &[], &[]));
    }
    let mut folds = Folds::default();
    assert_eq!(metrics::unfolded_kinds(&world, &folds).unwrap(), ids(&["ﬆoilet", "TOİLET"]));
    assert_eq!(sample_of(&world, &folds, true).err(), Some(unsupported("a facility kind was not folded")));
    // `str.casefold` gives "stoilet" and "toi̇let".
    folds.insert(text("ﬆoilet"), true);
    folds.insert(text("TOİLET"), false);
    assert!(metrics::unfolded_kinds(&world, &folds).unwrap().is_empty());
    assert_eq!(sample_of(&world, &folds, true).unwrap().toilet_working_units, 3 + 4 + 16);
    assert!(metrics::is_toilet("A-TOILET-room", &Folds::default()).unwrap());
}

#[test]
fn what_the_kernel_refused_it_still_refuses_with_its_text() {
    let mut world = world();
    world.entities.get_mut().active_tasks = Table::default();
    assert_eq!(sample_of(&world, &Folds::default(), true).err(), Some(unsupported("a staff queue names no task")));

    let mut world = self::world();
    world.entities.get_mut().taint("a value does not fit".to_owned());
    assert_eq!(sample_of(&world, &Folds::default(), true).err(), Some(unsupported("the entity store is tainted")));
    assert!(metrics::unfolded_kinds(&world, &Folds::default()).unwrap().is_empty());

    let mut world = self::world();
    put(&mut world.entities.get_mut().facilities, "f9", facility("f9", "toilet", i64::MAX, &[], &[]));
    assert_eq!(
        sample_of(&world, &Folds::default(), true).err(),
        Some(unsupported("the working units do not fit an i64"))
    );

    let world = self::world();
    assert_eq!(sample_of(&world, &Folds::default(), false).err(), Some(unsupported("the field holds another dict")));
    world.slots.get(METRIC_STATE).unwrap().try_borrow_mut().unwrap().misfit("an int".to_owned());
    assert_eq!(sample_of(&world, &Folds::default(), true).err(), Some(unsupported("the namespace has no row")));

    let mut bare = World::default();
    *bare.entities.get_mut() = std::mem::take(self::world().entities.get_mut());
    assert_eq!(sample_of(&bare, &Folds::default(), false).err(), Some(unsupported("the namespace is not owned")));
}

#[test]
fn a_world_cell_lends_the_whole_world_once_at_a_time() {
    let cell = Shard::new(world());
    let taken = cell.with(|world| sample_of(world, &Folds::default(), true)).unwrap().unwrap();
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
