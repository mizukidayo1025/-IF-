//! P8b-1: the rules of the receipts, the shared operations, the resource ledger and the staff's
//! shift and fatigue (`rules::care_receipts`, `common_operations`, `resources`, `staff_dynamics`),
//! over worlds built here. The PyO3 crate's entries are glue over these, so the gate and the offline
//! tests run them on every Run; these say what each does with no Python.

use std::sync::Arc;

use indexmap::IndexMap;
use sheltersim_core::config::staff::StaffFatigue;
use sheltersim_core::entities::{Activity, AgentRow, Entities, FacilityRow, Mobility, NeedRow, ProfileRow, Skills, StaffRow};
use sheltersim_core::error::Error;
use sheltersim_core::events::log::EventLog;
use sheltersim_core::events::queue::Queue;
use sheltersim_core::events::{EventKind, EventRow, EventStatus};
use sheltersim_core::row::{IdMap, Maybe, Row, ValueRow};
use sheltersim_core::rules::care_receipts::{self, Transition};
use sheltersim_core::rules::common_operations::{
    self as operations, py_eq, Found, Logged, Receipt, Verdict, NOT_THE_PAYLOAD, NO_EVENT, RECORDED, STATE_INVALID,
};
use sheltersim_core::rules::resources::{self, Applied, Fail, LedgerFault, OperationArg, OperationEvent};
use sheltersim_core::rules::staff_dynamics;
use sheltersim_core::state::resource_ledger::{
    Account, Accounts, Audit, AuditRecord, LedgerRow, UsedIds, AUDIT_SCHEMA_VERSION, NAME as LEDGER,
};
use sheltersim_core::state::shared_operations::{CareStatus, Heads, Receipts, SharedOperationsRow, NAME as SHARED, POLICY};
use sheltersim_core::state::staff_dynamics::{
    ConfigRow, LastActivity, Recovery, Shift, StaffDynamicsRow, StaffRecord, NAME as DYNAMICS, POLICY_V2,
};
use sheltersim_core::state::staff_operations::NAME as OPERATIONS;
use sheltersim_core::value::V;
use sheltersim_core::world::{Fit, Place, ResourcesRow, World, ID_COUNTERS, MODEL_CONFIGS, RESOURCES};

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

fn string(value: &str) -> V {
    V::Str(text(value))
}

fn map(entries: &[(&str, V)]) -> V {
    V::Map(entries.iter().map(|(key, value)| (text(key), value.clone())).collect())
}

fn unsupported(reason: &str) -> Error {
    Error::Unsupported(reason.to_owned().into())
}

fn needs() -> NeedRow {
    NeedRow { toilet_urgency: 0.0, hunger: 0.0, fatigue: 0.0, hydration_need: 0.0 }
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
            initial_needs: needs(),
            provenance: V::Map(IndexMap::new()),
        },
        zone_id: text("z0"),
        activity: Activity::Idle,
        target_id: None,
        needs: needs(),
        stress: 0.25,
        trust: 0.5,
        next_decision_at: 0,
        support_request_id: None,
        support_cooldown_until: 0,
    }
}

fn staff(id: &str, task: Option<&str>) -> StaffRow {
    StaffRow {
        id: text(id),
        zone_id: text("z0"),
        skills: Skills(Vec::new()),
        work_queue: Vec::new(),
        busy_until: 0,
        active_task_id: task.map(text),
    }
}

fn facility(id: &str, capacity: i64, units: i64) -> FacilityRow {
    FacilityRow {
        id: text(id),
        kind: text("water_station"),
        zone_id: text("z0"),
        capacity,
        working_units: units,
        active_agent_ids: Vec::new(),
        queue_agent_ids: Vec::new(),
        resource_active_agent_ids: Vec::new(),
        resource_queue_agent_ids: Vec::new(),
    }
}

/// A world at minute 5 of 100, holding its queue and log, with `id_counters` at `event: 10`, two
/// people, two staff members and a facility.
fn world() -> World {
    let mut world = World::default();
    {
        let scalars = world.scalars.get_mut();
        scalars.duration_minutes = Fit::Ok(100);
        scalars.minute_index = Fit::Ok(5);
        scalars.schema_version = Fit::Ok(6);
    }
    let mut entities = Entities::default();
    entities.agents.insert(text("a1"), agent("a1"));
    entities.agents.insert(text("a2"), agent("a2"));
    entities.staff.insert(text("s1"), staff("s1", Some("t1")));
    entities.staff.insert(text("s2"), staff("s2", None));
    entities.facilities.insert(text("f1"), facility("f1", 4, 3));
    *world.entities.get_mut() = entities;
    slot(&mut world, ID_COUNTERS, Place::Field, Box::new(ValueRow(map(&[("event", V::Int(10))]))));
    world.hold_queue(Arc::new(Queue::new()));
    world.hold_log(Arc::new(EventLog::new()));
    world
}

fn slot(world: &mut World, name: &str, place: Place, row: Box<dyn Row>) -> usize {
    let index = world.slots.add(text(name), place).unwrap();
    world.slots.at(index).unwrap().try_borrow_mut().unwrap().row = Some(row);
    index
}

fn counter(world: &World) -> Option<i64> {
    world.id_counter("event").unwrap()
}

// ---- staff_dynamics ------------------------------------------------------------------------------

fn dynamics(recovery: f64) -> StaffDynamicsRow {
    let mut windows = IdMap::default();
    // s2 is on shift from minute 0 to 60 of each week.
    windows.insert(text("s2"), vec![Shift { start_minute: 0, end_minute: 60 }]);
    let mut members = IdMap::default();
    members.insert(text("s1"), StaffRecord::new());
    members.insert(text("s2"), StaffRecord::new());
    StaffDynamicsRow {
        schema_version: 1,
        policy_version: text(POLICY_V2),
        source_id: text("ASM-STAFF-DYNAMICS-V2"),
        config: ConfigRow {
            schema_version: 1,
            policy_version: text(POLICY_V2),
            source_id: text("ASM-STAFF-DYNAMICS-V2"),
            max_continuous_work_minutes: 4,
            max_continuous_work_minutes_by_staff: Maybe(None),
            rest_minutes: 30,
            shift_windows_by_staff: Maybe(Some(windows)),
            off_shift_fatigue_recovery_per_minute: Maybe(Some(Recovery::float(recovery))),
        },
        staff: members,
    }
}

fn record(world: &World, id: &str) -> StaffRecord {
    *world.row::<StaffDynamicsRow>(DYNAMICS).unwrap().staff.get(id).unwrap()
}

#[test]
fn fatigue_is_the_formula_s() {
    assert_eq!(staff_dynamics::fatigue(false, 120, 240), Some(0.5));
    assert_eq!(staff_dynamics::fatigue(false, 300, 240), Some(1.0));
    // 1 - exp(-0.5), with the pinned libm.
    assert_eq!(staff_dynamics::fatigue(true, 120, 240), Some(0.3934693402873666));
    assert_eq!(staff_dynamics::fatigue(false, 1, 0), None);
    assert_eq!(staff_dynamics::fatigue(false, 1 << 54, 240), None);
}

#[test]
fn a_tick_works_rests_and_recovers_on_the_row_alone() {
    let mut world = world();
    let index = slot(&mut world, DYNAMICS, Place::Namespace, Box::new(dynamics(0.25)));
    world.slots.take_written();
    let formula = StaffFatigue::parse(&map(&[])).unwrap();
    // s1 works; s2 is on shift and idle.
    for minute in 0..4 {
        staff_dynamics::update(&mut world, minute, &formula).unwrap();
    }
    let worked = record(&world, "s1");
    assert_eq!((worked.consecutive_work_minutes, worked.total_work_minutes, worked.fatigue), (4, 4, 1.0));
    assert_eq!(worked.last_activity, LastActivity::Working);
    let idle = record(&world, "s2");
    assert_eq!((idle.consecutive_work_minutes, idle.total_rest_minutes), (0, 0));
    assert_eq!(idle.last_activity, LastActivity::OffDuty);
    // Only the row was written, and its slot is marked for the tree to follow (lane P8-0c).
    assert_eq!(world.slots.take_written(), [index]);

    // s1 has finished its task: at its limit, it rests until minute + 30.
    world.entities.get_mut().staff.get_mut("s1").unwrap().active_task_id = None;
    world.row_mut::<StaffDynamicsRow>(DYNAMICS).unwrap().staff.get_mut("s1").unwrap().fatigue = 0.5;
    staff_dynamics::update(&mut world, 10, &formula).unwrap();
    let resting = record(&world, "s1");
    assert_eq!((resting.rest_until_minute, resting.last_activity), (40, LastActivity::Resting));
    // Off shift (s2 at minute 70): no work counted, fatigue recovers by the config's rate.
    world.row_mut::<StaffDynamicsRow>(DYNAMICS).unwrap().staff.get_mut("s2").unwrap().fatigue = 0.5;
    staff_dynamics::update(&mut world, 70, &formula).unwrap();
    let off = record(&world, "s2");
    assert_eq!((off.fatigue, off.total_rest_minutes, off.last_activity), (0.25, 1, LastActivity::OffDuty));
}

#[test]
fn an_update_that_can_t_be_planned_changes_nothing() {
    let mut world = world();
    let mut row = dynamics(0.0);
    row.staff.shift_remove("s2");
    slot(&mut world, DYNAMICS, Place::Namespace, Box::new(row));
    world.slots.take_written();
    let formula = StaffFatigue::parse(&map(&[])).unwrap();
    assert_eq!(staff_dynamics::update(&mut world, 0, &formula), Err(unsupported("a member has no record")));
    assert!(world.slots.take_written().is_empty());
    assert_eq!(record(&world, "s1").total_work_minutes, 0);
}

#[test]
fn availability_reads_the_operations_and_the_status() {
    let mut world = world();
    slot(&mut world, DYNAMICS, Place::Namespace, Box::new(dynamics(0.0)));
    // No operations and no status: on shift is available, off shift is not.
    assert_eq!(staff_dynamics::is_available_now(&world, "s2", 10, false), Ok(true));
    assert_eq!(staff_dynamics::is_available_now(&world, "s2", 70, false), Ok(false));
    assert_eq!(staff_dynamics::on_shift_now(&world, "s9", 10), Ok(false));
    // The operational status says who is unavailable.
    let status = map(&[("staff_status", map(&[("s2", map(&[("available", V::Bool(false))]))]))]);
    slot(&mut world, staff_dynamics::OPERATIONAL, Place::Value, Box::new(ValueRow(status)));
    assert_eq!(staff_dynamics::is_available_now(&world, "s2", 10, false), Ok(false));
    // A tainted staff_operations_v1 is refused: every rule writes its row (lanes b2 and b3).
    let index = world.slots.add(text(OPERATIONS), Place::Namespace).unwrap();
    world.slots.at(index).unwrap().try_borrow_mut().unwrap().taint = Some("a misfit".to_owned());
    assert_eq!(staff_dynamics::operation_is_idle(&world, "s2", false), Err(unsupported("the namespace has no row")));
    assert_eq!(staff_dynamics::is_available_now(&world, "s1", 10, false), Err(unsupported("the namespace has no row")));
    // None under the key, which the caller read, is no state.
    assert_eq!(staff_dynamics::is_available_now(&world, "s1", 10, true), Ok(true));
}

#[test]
fn a_config_without_windows_and_a_bad_status_are_refused() {
    let mut row = dynamics(0.0);
    row.config.shift_windows_by_staff = Maybe(None);
    assert_eq!(staff_dynamics::on_shift(&row.config, "s1", 0), Err(unsupported("the config has no shift windows")));
    let status = map(&[("s1", map(&[("available", V::Int(1))]))]);
    assert_eq!(staff_dynamics::is_available(Some(&status), "s1"), Err(unsupported("available is not a bool")));
    assert_eq!(staff_dynamics::is_available(Some(&status), "s2"), Ok(true));
}

#[test]
fn a_new_member_gets_a_new_record() {
    let mut world = world();
    let mut row = dynamics(0.0);
    row.staff.get_mut("s1").unwrap().total_work_minutes = 9;
    slot(&mut world, DYNAMICS, Place::Namespace, Box::new(row));
    staff_dynamics::add_staff(&mut world, "s3").unwrap();
    staff_dynamics::add_staff(&mut world, "s1").unwrap();
    let row = world.row::<StaffDynamicsRow>(DYNAMICS).unwrap();
    assert_eq!(row.staff.keys().map(|key| &**key).collect::<Vec<_>>(), ["s1", "s2", "s3"]);
    assert_eq!(row.staff.get("s1").unwrap().total_work_minutes, 0);
}

// ---- resources -------------------------------------------------------------------------------------

fn account(remaining: i64) -> Account {
    Account {
        initial: remaining,
        delivered: 0,
        served: 0,
        losses: 0,
        remaining,
        reserved: 0,
        source_ids: vec![text("SRC-INIT")],
        reservations: IdMap::default(),
    }
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

fn ledger_world() -> World {
    let mut world = world();
    let mut audit = Audit::with_capacity(4);
    audit.push(initial("water_ml", 1000)).unwrap();
    audit.push(initial("food_meal_units", 50)).unwrap();
    let ledger = LedgerRow {
        schema_version: 1,
        warehouse_id: text("warehouse-main"),
        zone_id: None,
        accounts: Accounts { water_ml: account(1000), food_meal_units: account(50) },
        used_reservation_ids: UsedIds::new(Vec::new()),
        audit,
    };
    slot(&mut world, LEDGER, Place::Namespace, Box::new(ledger));
    let mut resources: ResourcesRow = IdMap::default();
    resources.insert(text("water_ml"), 1000);
    resources.insert(text("food_meal_units"), 50);
    slot(&mut world, RESOURCES, Place::Field, Box::new(resources));
    world
}

fn apply(world: &mut World, operation: &str, payload: V, event_id: &str) -> Result<Applied, Fail> {
    let (source, event) = (string("SRC-OP"), string(event_id));
    let event = OperationEvent { payload: &payload, source_id: &source, event_id: &event };
    resources::apply_operation(world, &event, OperationArg::Text(operation))
}

/// `apply`, and then `world.resources` takes the new balance, as the PyO3 crate's entry gives it.
fn operate(world: &mut World, operation: &str, payload: V, event_id: &str) -> Result<Applied, Fail> {
    let applied = apply(world, operation, payload, event_id)?;
    let resource = ["water_ml", "food_meal_units"][applied.index];
    world.row_mut::<ResourcesRow>(RESOURCES).unwrap().insert(text(resource), applied.remaining);
    Ok(applied)
}

fn ledger_text(fail: Result<Applied, Fail>) -> String {
    match fail {
        Err(Fail::Ledger(fault)) => fault.text(|value| format!("<{value:?}>")),
        other => panic!("not a ledger error: {other:?}"),
    }
}

fn water(quantity: i64) -> V {
    map(&[("warehouse_id", string("warehouse-main")), ("resource_id", string("water_ml")), ("quantity", V::Int(quantity))])
}

fn reservation(quantity: Option<i64>, id: &str, agent: &str) -> V {
    let mut entries = vec![("warehouse_id", string("warehouse-main")), ("resource_id", string("water_ml"))];
    if let Some(quantity) = quantity {
        entries.push(("quantity", V::Int(quantity)));
    }
    entries.push(("reservation_id", string(id)));
    entries.push(("agent_id", string(agent)));
    map(&entries)
}

#[test]
fn operations_change_the_balances_and_the_audit() {
    let mut world = ledger_world();
    world.slots.take_written();
    assert_eq!(apply(&mut world, "delivery", water(500), "op-1"), Ok(Applied { index: 0, remaining: 1500 }));
    // `world.resources` is the PyO3 crate's to replace: its row is not written here.
    assert_eq!(world.resources().unwrap().get("water_ml"), Some(&1000));
    let written: Vec<_> = world.slots.written_names().iter().map(|name| name.to_string()).collect();
    assert_eq!(written, [LEDGER]);
    world.row_mut::<ResourcesRow>(RESOURCES).unwrap().insert(text("water_ml"), 1500);
    assert_eq!(operate(&mut world, "reserve", reservation(Some(100), "r1", "a1"), "op-2"), Ok(Applied { index: 0, remaining: 1500 }));
    assert_eq!(operate(&mut world, "consume", reservation(Some(60), "r1", "a1"), "op-3"), Ok(Applied { index: 0, remaining: 1440 }));
    assert_eq!(operate(&mut world, "release", reservation(None, "r1", "a1"), "op-4"), Ok(Applied { index: 0, remaining: 1440 }));
    assert_eq!(operate(&mut world, "loss", water(40), "op-5"), Ok(Applied { index: 0, remaining: 1400 }));
    let row = world.row::<LedgerRow>(LEDGER).unwrap();
    let water = row.accounts.of(0);
    assert_eq!((water.delivered, water.served, water.losses, water.remaining, water.reserved), (500, 60, 40, 1400, 0));
    assert_eq!(water.source_ids, [text("SRC-INIT"), text("SRC-OP")]);
    assert!(water.reservations.is_empty());
    assert!(row.used_reservation_ids.contains("r1"));
    // The audit: two initial records and one per operation; the release records what it freed.
    let audit = row.audit.item_bytes();
    assert_eq!(audit.len(), 7);
    let release = std::str::from_utf8(&audit[5]).unwrap();
    assert!(release.contains("\"operation\":\"release\"") && release.contains("\"quantity\":40"), "{release}");
}

#[test]
fn what_the_python_rule_turned_down_is_its_text_with_nothing_changed() {
    let mut world = ledger_world();
    operate(&mut world, "reserve", reservation(Some(100), "r1", "a1"), "op-1").unwrap();
    let before = world.row::<LedgerRow>(LEDGER).unwrap().audit.item_bytes().len();
    assert_eq!(ledger_text(operate(&mut world, "reserve", reservation(Some(1), "r1", "a2"), "op-2")), "resource reservation ID 'r1' was already used");
    assert_eq!(ledger_text(operate(&mut world, "loss", water(901), "op-3")), "resource loss exceeds unreserved remaining inventory");
    assert_eq!(ledger_text(operate(&mut world, "consume", reservation(Some(1), "r1", "a2"), "op-4")), "resource reservation belongs to a different Agent");
    assert_eq!(ledger_text(operate(&mut world, "consume", reservation(Some(101), "r1", "a1"), "op-5")), "resource consume exceeds the reservation");
    assert_eq!(ledger_text(operate(&mut world, "release", reservation(None, "r9", "a1"), "op-6")), "unknown resource reservation 'r9'");
    assert_eq!(ledger_text(operate(&mut world, "delivery", water(0), "op-7")), "resource quantity must be a positive integer");
    assert_eq!(
        ledger_text(operate(&mut world, "delivery", map(&[("warehouse_id", string("warehouse-main"))]), "op-8")),
        "resource delivery payload fields do not match"
    );
    // A payload value is shown as Python's repr, which the PyO3 crate writes.
    let other = map(&[("warehouse_id", string("elsewhere")), ("resource_id", string("water_ml")), ("quantity", V::Int(1))]);
    assert_eq!(
        operate(&mut world, "delivery", other, "op-9"),
        Err(Fail::Ledger(LedgerFault::Repr("unknown resource warehouse ID", string("elsewhere"))))
    );
    assert_eq!(ledger_text(operate(&mut world, "reserve", reservation(Some(1), "r2", "a9"), "op-10")), "unknown resource Agent ID <Str(\"a9\")>");
    assert_eq!(
        resources::apply_operation(
            &mut world,
            &OperationEvent { payload: &water(1), source_id: &string("SRC-OP"), event_id: &string("op-11") },
            OperationArg::Other("'spill'".to_owned())
        ),
        Err(Fail::Ledger(LedgerFault::Text("unknown resource operation 'spill'".to_owned())))
    );
    // IDs `ScheduledEvent` turns down, and a source of None.
    let bad = OperationEvent { payload: &water(1), source_id: &string("not stable"), event_id: &string("op-12") };
    assert_eq!(resources::apply_operation(&mut world, &bad, OperationArg::Text("delivery")), Err(Fail::Error(unsupported("not a stable ID"))));
    let none = OperationEvent { payload: &water(1), source_id: &V::Null, event_id: &string("op-13") };
    assert_eq!(ledger_text(resources::apply_operation(&mut world, &none, OperationArg::Text("delivery"))), "resource events require a source_id");
    assert_eq!(world.row::<LedgerRow>(LEDGER).unwrap().audit.item_bytes().len(), before);
}

#[test]
fn a_ledger_the_check_turns_down_is_refused() {
    let mut world = ledger_world();
    // `world.resources` no longer holds the ledger's balance.
    world.row_mut::<ResourcesRow>(RESOURCES).unwrap().insert(text("water_ml"), 7);
    assert_eq!(
        operate(&mut world, "delivery", water(1), "op-1"),
        Err(Fail::Error(unsupported("the state is one the Python rule's check turned down")))
    );
    assert_eq!(resources::metric_values(&world), Err(unsupported("the state is for the Python rule to judge")));
}

#[test]
fn the_metric_columns_and_the_used_ids() {
    let mut world = ledger_world();
    operate(&mut world, "reserve", reservation(Some(10), "r1", "a1"), "op-1").unwrap();
    assert_eq!(resources::metric_values(&world), Ok([[1000, 0, 0, 0, 1000, 10], [50, 0, 0, 0, 50, 0]]));
    assert_eq!(resources::reservation_id_was_used(&world, "r1"), Ok(true));
    assert_eq!(resources::reservation_id_was_used(&world, "r2"), Ok(false));
}

// ---- The receipts' contracts -----------------------------------------------------------------------

fn recovery_document(event_id: &str, minute: i64, units: i64) -> V {
    map(&[
        ("record_type", string("operations")),
        ("schema_version", V::Int(1)),
        ("event_type", string("facility_recovery_transition")),
        ("event_id", string(event_id)),
        ("facility_id", string("f1")),
        ("component_id", string("pump")),
        ("status", string("restored")),
        ("minute", V::Int(minute)),
        ("previous_working_units", V::Int(0)),
        ("working_units", V::Int(units)),
        ("source_ids", V::List(vec![string("ASM-FACILITY-RECOVERY-PRODUCER-V1")])),
        ("causation_id", string("cause-1")),
    ])
}

fn change(before: V, after: V) -> V {
    map(&[
        ("entity_id", string("a1")),
        ("entity_kind", string("agent")),
        ("field_path", string("stress")),
        ("before", before),
        ("after", after),
        ("unit", string("index")),
    ])
}

#[test]
fn a_receipt_is_what_its_contract_takes_and_dumps() {
    let document = recovery_document("e-1", 5, 3);
    let receipt = Receipt::check(&document).unwrap();
    assert!(receipt.document().same_in_order(&document));
    // Defaults the dump writes.
    let V::Map(mut short) = document.clone() else { unreachable!() };
    short.shift_remove("record_type");
    short.shift_remove("source_ids");
    let dumped = Receipt::check(&V::Map(short)).unwrap().document();
    assert!(dumped.same_in_order(&map(&[
        ("record_type", string("operations")),
        ("schema_version", V::Int(1)),
        ("event_type", string("facility_recovery_transition")),
        ("event_id", string("e-1")),
        ("facility_id", string("f1")),
        ("component_id", string("pump")),
        ("status", string("restored")),
        ("minute", V::Int(5)),
        ("previous_working_units", V::Int(0)),
        ("working_units", V::Int(3)),
        ("source_ids", V::List(Vec::new())),
        ("causation_id", string("cause-1")),
    ])));
    // What the contract turns down, and what only it can judge.
    let V::Map(mut extra) = document.clone() else { unreachable!() };
    extra.insert(text("note"), string("x"));
    assert_eq!(Receipt::check(&V::Map(extra)), Err(Verdict::Invalid));
    assert_eq!(Receipt::check(&recovery_document("e-1", 5, 0)), Err(Verdict::Invalid));
    assert_eq!(Receipt::check(&recovery_document("bad id", 5, 3)), Err(Verdict::Invalid));
    assert_eq!(Receipt::of(&map(&[("event_type", string("other"))])), Err(unsupported("a receipt its contract does not take as it is")));

    // A care receipt: an int becomes its float, a str for a float is the contract's to judge.
    let care = |changes: Vec<V>| {
        map(&[
            ("event_type", string("care_support_transition")),
            ("event_id", string("e-2")),
            ("request_id", string("q-1")),
            ("agent_id", string("a1")),
            ("support_kind", string("emotional_support")),
            ("status", string("completed")),
            ("minute", V::Int(5)),
            ("outcome_observation", string("observed_change")),
            ("state_changes", V::List(changes)),
        ])
    };
    let Receipt::Care(receipt) = Receipt::check(&care(vec![change(V::Int(0), V::Float(0.25))])).unwrap() else { unreachable!() };
    assert_eq!((receipt.state_changes[0].before, receipt.status), (0.0, CareStatus::Completed));
    assert_eq!(Receipt::check(&care(vec![change(string("0"), V::Float(0.25))])), Err(Verdict::Unsure));
    // observed_change with no changed value.
    assert_eq!(Receipt::check(&care(vec![change(V::Float(0.25), V::Float(0.25))])), Err(Verdict::Invalid));
}

#[test]
fn equality_is_python_s() {
    assert!(py_eq(&V::Int(1), &V::Float(1.0)));
    assert!(py_eq(&V::Bool(true), &V::Int(1)));
    assert!(!py_eq(&V::Int(9_007_199_254_740_993), &V::Float(9_007_199_254_740_992.0)));
    assert!(py_eq(&map(&[("a", V::Int(1)), ("b", V::Null)]), &map(&[("b", V::Null), ("a", V::Float(1.0))])));
    assert!(!py_eq(&V::List(vec![V::Int(1)]), &V::List(vec![V::Int(1), V::Int(2)])));
    assert!(!py_eq(&string("1"), &V::Int(1)));
}

// ---- record_shared_operation ------------------------------------------------------------------------

fn shared() -> SharedOperationsRow {
    SharedOperationsRow {
        schema_version: 1,
        policy_version: text(POLICY),
        support_requests: Heads::default(),
        history: Receipts::default(),
        history_truncated: false,
    }
}

fn logged(document: &V, event_id: &str, minute: i64) -> Logged {
    let V::Map(payload) = document.clone() else { unreachable!() };
    let event = EventRow::new(
        text(event_id),
        EventKind::Endogenous,
        minute,
        0,
        1,
        text("facility_recovery_transition"),
        payload,
        Some(text("ASM-FACILITY-RECOVERY-PRODUCER-V1")),
        Some(text("cause-1")),
        IndexMap::new(),
    )
    .unwrap();
    Logged { status: EventStatus::Applied, realized_minute: minute, event }
}

#[test]
fn a_recovery_receipt_is_recorded_once_with_the_rule_s_texts() {
    let mut world = world();
    let index = slot(&mut world, SHARED, Place::Namespace, Box::new(shared()));
    world.slots.take_written();
    let document = recovery_document("e-1", 5, 3);
    let receipt = Receipt::of(&document).unwrap();
    assert_eq!(operations::needed_records(&world, &receipt), [text("e-1")]);
    // No such event.
    let none = Found(vec![(text("e-1"), None)]);
    assert_eq!(operations::record_shared_operation(&mut world, &receipt, &none), Err(Error::Value(NO_EVENT.to_owned())));
    // An event whose payload is another.
    let other = Found(vec![(text("e-1"), Some(logged(&recovery_document("e-1", 5, 2), "e-1", 5)))]);
    assert_eq!(operations::record_shared_operation(&mut world, &receipt, &other), Err(Error::Value(NOT_THE_PAYLOAD.to_owned())));
    assert!(world.slots.take_written().is_empty());

    let found = Found(vec![(text("e-1"), Some(logged(&document, "e-1", 5)))]);
    operations::record_shared_operation(&mut world, &receipt, &found).unwrap();
    assert_eq!(world.slots.take_written(), [index]);
    {
        let row = world.row::<SharedOperationsRow>(SHARED).unwrap();
        let [record] = row.history.items.iter().collect::<Vec<_>>()[..] else { panic!() };
        assert_eq!((record.minute, record.event_id.as_deref(), record.verified, record.reads_staff_history), (Some(5), Some("e-1"), true, false));
        assert_eq!(record.bytes, document.to_json().unwrap().into_boxed_slice());
    }
    assert_eq!(operations::record_shared_operation(&mut world, &receipt, &found), Err(Error::Value(RECORDED.to_owned())));
    // The facility's units are no longer the receipt's.
    world.entities.get_mut().facilities.get_mut("f1").unwrap().working_units = 2;
    let second = recovery_document("e-2", 5, 3);
    let found = Found(vec![(text("e-2"), Some(logged(&second, "e-2", 5)))]);
    assert_eq!(
        operations::record_shared_operation(&mut world, &Receipt::of(&second).unwrap(), &found),
        Err(Error::Value("facility recovery receipt does not match the applied facility state".to_owned()))
    );
}

#[test]
fn a_recent_receipt_the_contract_turns_down_makes_the_state_invalid() {
    let mut world = world();
    let mut row = shared();
    row.history.items.push_back(sheltersim_core::state::shared_operations::Receipt {
        minute: Some(5),
        event_id: Some(text("e-1")),
        verified: false,
        reads_staff_history: false,
        bytes: br#"{"event_type":"facility_recovery_transition","minute":5}"#.to_vec().into_boxed_slice(),
    });
    slot(&mut world, SHARED, Place::Namespace, Box::new(row));
    assert_eq!(operations::state_holds(&world, 5), Err(Error::Value(STATE_INVALID.to_owned())));
}

// ---- Care transitions ----------------------------------------------------------------------------

fn transition(status: &'static str) -> Transition {
    Transition {
        request_id: text("q-1"),
        agent_id: text("a1"),
        household_id: None,
        support_kind: text("emotional_support"),
        status,
        staff_id: None,
        causation_id: None,
        source_ids: vec![text("SRC-B"), text("SRC-A")],
        outcome: None,
    }
}

#[test]
fn a_care_transition_is_its_record_and_its_receipt() {
    let mut world = world();
    slot(&mut world, SHARED, Place::Namespace, Box::new(shared()));
    let planned = care_receipts::plan_transition(&world, transition("requested"), 10).unwrap();
    assert_eq!((&*planned.row().event_id, planned.row().sequence, planned.minute()), ("care-support-transition-10", 10, 5));
    assert_eq!(planned.detail(), "care transition requested recorded after state application");
    care_receipts::write_transition(&mut world, planned).unwrap();
    assert_eq!(counter(&world), Some(11));
    assert!(world.log().unwrap().has_event_id("care-support-transition-10"));
    {
        let row = world.row::<SharedOperationsRow>(SHARED).unwrap();
        let head = row.support_requests.map.get("q-1").unwrap();
        assert_eq!((head.status, head.created_minute, &*head.latest_event_id), (CareStatus::Requested, 5, "care-support-transition-10"));
        let bytes = std::str::from_utf8(&row.history.items[0].bytes).unwrap();
        // The sources sorted, with the producer's.
        assert!(bytes.contains(r#""source_ids":["ASM-CARE-TRANSITION-PRODUCER-V1","SRC-A","SRC-B"]"#), "{bytes}");
    }
    // An order the lifecycle turns down.
    assert_eq!(
        care_receipts::append(&mut world, transition("completed")).map_err(|_| ()),
        Err(())
    );
    care_receipts::append(&mut world, transition("assigned")).unwrap();
    assert_eq!(counter(&world), Some(12));
    // A planned sequence someone took in between.
    let planned = care_receipts::plan_transition(&world, transition("started"), 12).unwrap();
    care_receipts::allocate_sequence(&world).unwrap();
    assert!(care_receipts::write_transition(&mut world, planned).is_err());
}

#[test]
fn the_other_receipts_are_applied_records_of_their_events() {
    let world = world();
    let made = care_receipts::facility_recovery_record(&world, "f1", "pump", "restored", 0, 3, "cause-1", vec![text("SRC-Z")]).unwrap();
    assert_eq!((&*made.event.event_id, made.event.sequence, made.minute), ("facility-recovery-transition-10", 10, 5));
    assert_eq!(made.detail, "facility restored receipt recorded after working-unit update");
    let V::Map(payload) = &made.event.payload else { unreachable!() };
    assert_eq!(payload.get("source_ids"), Some(&V::List(vec![string("ASM-FACILITY-RECOVERY-PRODUCER-V1"), string("SRC-Z")])));
    // The payload is the receipt's dump, in its order.
    assert!(Receipt::of(&made.event.payload).unwrap().document().same_in_order(&made.event.payload));
    assert_eq!(counter(&world), Some(11));
    // What the contract turns down is refused, before a sequence is taken.
    assert_eq!(
        care_receipts::facility_recovery_record(&world, "f1", "pump", "restored", 3, 3, "cause-1", Vec::new()).map(|_| ()),
        Err(unsupported("a receipt its contract turns down"))
    );
    assert_eq!(counter(&world), Some(11));

    let handoff = care_receipts::handoff_record(&world, "t1", "s1", "s2", "shift_end", "cause-2", Vec::new()).unwrap();
    assert_eq!((&*handoff.event.event_id, &*handoff.detail), ("staff-handoff-11", "staff task assignment updated before handoff receipt"));
    let continuity = care_receipts::continuity_record(&world, "q-1", "a1", "t1", "s1", "requeued", "cause-3", "staff_shift_change").unwrap();
    assert_eq!(&*continuity.event.event_id, "staff-support-continuity-12");
    assert_eq!(continuity.detail, "staff support request was requeued after assigned staff became unavailable");
    assert!(Receipt::of(&continuity.event.payload).is_ok());
}

#[test]
fn a_staff_operation_receipt_samples_its_changes() {
    let world = world();
    let outcome = |changes: Vec<V>, measured: bool, status: &str| care_receipts::OperationOutcome {
        task_id: string("t1"),
        staff_id: string("s1"),
        task_kind: string("cleaning"),
        target_id: V::Null,
        target_zone_id: string("z0"),
        reason: string("sanitation is low"),
        status: string(status),
        start_minute: V::Int(1),
        arrival_minute: V::Int(2),
        travel_minutes: V::Int(1),
        changes,
        measured,
        causation: text("cause-1"),
    };
    let made = care_receipts::staff_operation_record(&world, outcome(vec![change(V::Float(0.5), V::Float(0.75)); 300], true, "completed")).unwrap();
    let V::Map(payload) = &made.event.payload else { unreachable!() };
    assert_eq!(payload.get("total_state_change_count"), Some(&V::Int(300)));
    assert_eq!(payload.get("state_changes_truncated"), Some(&V::Bool(true)));
    assert!(matches!(payload.get("state_changes"), Some(V::List(sampled)) if sampled.len() == 256));
    assert_eq!(payload.get("outcome_observation"), Some(&string("observed_change")));
    assert_eq!(made.detail, "staff operation completed outcome recorded after state application");
    // A blocked operation reports no change; one that reports a change is refused.
    let blocked = care_receipts::staff_operation_record(&world, outcome(Vec::new(), true, "blocked")).unwrap();
    assert!(Receipt::of(&blocked.event.payload).is_ok());
    assert_eq!(
        care_receipts::staff_operation_record(&world, outcome(vec![change(V::Float(0.5), V::Float(0.75))], true, "blocked")).map(|_| ()),
        Err(unsupported("a receipt its contract turns down"))
    );
}

#[test]
fn a_person_s_sources_are_read_from_the_context_namespaces() {
    let mut world = world();
    let configs = map(&[("population_context", map(&[("people", map(&[("a2", map(&[("source_ids", V::List(vec![string("SRC-C")]))]))]))]))]);
    slot(&mut world, MODEL_CONFIGS, Place::Field, Box::new(ValueRow(configs)));
    let context = map(&[("people", map(&[("a1", map(&[("source_ids", V::List(vec![string("SRC-B"), V::Int(3), string("SRC-A")]))]))]))]);
    slot(&mut world, care_receipts::POPULATION_CONTEXT, Place::Value, Box::new(ValueRow(context)));
    assert_eq!(
        care_receipts::source_ids_of(&world, "a1").unwrap(),
        [text("ASM-STAFF-SUPPORT-LIFECYCLE-V1"), text("SRC-A"), text("SRC-B")]
    );
    assert_eq!(care_receipts::source_ids_of(&world, "a2").unwrap(), [text("ASM-STAFF-SUPPORT-LIFECYCLE-V1"), text("SRC-C")]);
    assert_eq!(care_receipts::source_ids_of(&world, "a9").unwrap(), [text("ASM-STAFF-SUPPORT-LIFECYCLE-V1")]);
    assert_eq!(care_receipts::person_context(&world, "a9").unwrap(), None);
}
