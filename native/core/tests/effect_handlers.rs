//! Three handlers of an EffectProgram's events in the core, on the world of the core's tick test
//! (Issue #152, lane P10-1b; P9-1b moved their transactions into the core and left them without core
//! tests): the geometry change (`effects::geometry_event`), the staff task order
//! (`effects::task_order_event`) and the staff handover (`effects::handoff_event`).
//!
//! The World is `tests/data/tick_core.json` at minute 0 (`support/load.rs`), with the states these
//! events need written into its slots here: the approved program of the three effects, an empty
//! geometry overlay, idle staff operations and the shared operations; A001 waits at the information
//! desk. The host is the `TestHost`:
//! contracts take their parameters as given (decision (c)), the whole world's validation passes
//! (policy E6), and the floor plan is a plan of the fixture's four zones and six routes.

mod support;

use std::sync::Arc;

use indexmap::IndexMap;
use sheltersim_core::config::geometry::FacilityPlan;
use sheltersim_core::config::SpatialConstraints;
use sheltersim_core::error::Error;
use sheltersim_core::events::{EventKind, EventRow};
use sheltersim_core::geometry::{zone_center, Geo, Pt, Route, Zone};
use sheltersim_core::host::{Contract, TxnKind};
use sheltersim_core::row::{IdMap, Row, ValueRow};
use sheltersim_core::rules::effects;
use sheltersim_core::state::effect_program::{EffectProgramRow, Program, NAME as PROGRAM};
use sheltersim_core::state::shared_operations::{Heads, Receipts, SharedOperationsRow, NAME as SHARED, POLICY as SHARED_POLICY};
use sheltersim_core::state::staff_operations::{OperationRecord, Phase, StaffOperationsRow, NAME as STAFF_OPERATIONS};
use sheltersim_core::value::V;
use sheltersim_core::world::{Fit, World, WorldCell};

use support::load::{self, Loaded};
use support::{Call, TestHost};

const OVERLAY: &str = "geometry_overlay_v1";
const PROGRAM_ID: &str = "p1";
/// `"modifier-" + sha256(f"{program_id}:{effect_id}")[:32]`, as the program's scheduler writes it.
const ROUTE_MODIFIER: &str = "modifier-62ee1868ced5ead06966d06e64007cd0";
const ORDER_MODIFIER: &str = "modifier-42742f550cec8f92ad74486de15164a1";
const HANDOFF_MODIFIER: &str = "modifier-ceda9a7cae3f4468755459940d80fa16";

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

fn s(value: &str) -> V {
    V::Str(text(value))
}

fn map(entries: Vec<(&str, V)>) -> V {
    V::Map(entries.into_iter().map(|(key, value)| (text(key), value)).collect())
}

fn entries(entries: Vec<(&str, V)>) -> IndexMap<Arc<str>, V> {
    let V::Map(map) = map(entries) else { unreachable!() };
    map
}

fn empty() -> V {
    V::Map(IndexMap::new())
}

/// The approved program `p1`: `e1` closes a route, `e2` orders a staff task, `e3` hands a task over;
/// each scheduled as the event of the same number.
fn program() -> EffectProgramRow {
    let mut operators: IdMap<Arc<str>> = IdMap::default();
    for (effect, op) in [("e1", "set_route_access"), ("e2", "staff_task_order"), ("e3", "staff_handoff")] {
        operators.insert(text(effect), text(op));
    }
    let mut programs: IdMap<Program> = IdMap::default();
    programs.insert(
        text(PROGRAM_ID),
        Program {
            program_hash: text(&"a".repeat(64)),
            source_text_hash: text(&"b".repeat(64)),
            narrative: text("close a corridor, send a worker to the desk, hand the work over"),
            compiler_provenance: map(vec![("compiler_kind", s("offline_rule")), ("compiler_version", s("v1"))]),
            assumptions: V::List(Vec::new()),
            support_mode: text("native"),
            effect_ids: vec![text("e1"), text("e2"), text("e3")],
            operators,
            scheduled_event_ids: vec![text("ev-e1"), text("ev-e2"), text("ev-e3")],
        },
    );
    EffectProgramRow { schema_version: 1, programs, active_facility_modifiers: IdMap::default() }
}

fn overlay_state() -> V {
    map(vec![
        ("schema_version", V::Int(1)),
        ("policy_version", s("bounded-runtime-geometry-v1")),
        ("facility_points", empty()),
        ("partitions", empty()),
        ("route_modifiers", empty()),
        ("history", V::List(Vec::new())),
        ("temporary_rooms", empty()),
        ("bed_reservations", empty()),
        ("room_admissions", empty()),
    ])
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

/// Every member of the fixture's staff idle.
fn staff_operations(world: &World) -> StaffOperationsRow {
    let mut staff: IdMap<OperationRecord> = IdMap::default();
    for (id, _) in world.rows().unwrap().staff.iter() {
        staff.insert(id.clone(), idle());
    }
    StaffOperationsRow {
        schema_version: 1,
        policy_version: text("autonomous-routed-staff-v1"),
        source_id: text("ASM-STAFF-OPERATIONS-TEST"),
        config: map(vec![(
            "task_duration_minutes",
            map(vec![("cleaning", V::Int(30)), ("maintenance", V::Int(45)), ("information_outreach", V::Int(20)), ("intake", V::Int(15)), ("patrol", V::Int(20))]),
        )]),
        zones: IdMap::default(),
        staff,
        history: Vec::new(),
    }
}

fn put<T: Row>(world: &World, name: &str, row: T) {
    world.slots.get(name).unwrap_or_else(|| panic!("{name} has no slot")).try_borrow_mut().unwrap().row = Some(Box::new(row));
}

/// The fixture's world at minute 0 with the states of the three effects.
fn loaded() -> Loaded {
    let fixture = load::fixture();
    let loaded = load::loaded(&fixture);
    loaded
        .cell
        .with(|world| {
            put(world, PROGRAM, program());
            // A001 waits at the information desk, where the staff are.
            world.rows_mut().unwrap().agents.get_mut("A001").unwrap().zone_id = text("information_desk");
            put(world, OVERLAY, ValueRow(overlay_state()));
            let operations = staff_operations(world);
            put(world, STAFF_OPERATIONS, operations);
            put(
                world,
                SHARED,
                SharedOperationsRow {
                    schema_version: 1,
                    policy_version: text(SHARED_POLICY),
                    support_requests: Heads::default(),
                    history: Receipts::default(),
                    history_truncated: false,
                },
            );
        })
        .unwrap();
    loaded
}

/// A plan of the fixture's zones, 10 m squares in a row, and a route for each of its edges.
fn plan(world: &World) -> Geo {
    let rows = world.rows().unwrap();
    let mut zones = IdMap::default();
    for (index, (id, _)) in rows.zones.iter().enumerate() {
        let x = index as f64 * 10.0;
        let polygon = vec![Pt { x, y: 0.0 }, Pt { x: x + 10.0, y: 0.0 }, Pt { x: x + 10.0, y: 10.0 }, Pt { x, y: 10.0 }];
        let center = zone_center(&polygon);
        zones.insert(id.clone(), Zone { polygon, usable_area: 100.0, center });
    }
    let mut routes = IdMap::default();
    for (id, _) in rows.zone_edges.iter() {
        routes.insert(id.clone(), Route { points: vec![Pt { x: 5.0, y: 5.0 }, Pt { x: 15.0, y: 5.0 }], width: 2.0, accessible: true });
    }
    let plan = FacilityPlan { zones, routes, facility_points: IdMap::default(), service_points: Vec::new() };
    Geo::new(Some(&plan), Some(&SpatialConstraints::default()), None, None).unwrap()
}

fn host(loaded: &Loaded) -> TestHost {
    let mut host = TestHost::default();
    host.plan = Some(Arc::new(loaded.cell.with(|world| plan(world)).unwrap()));
    host
}

/// The marker of the start of an effect of `p1`.
fn marker(effect: &str, op: &str) -> V {
    map(vec![
        ("program_id", s(PROGRAM_ID)),
        ("effect_id", s(effect)),
        ("op", s(op)),
        ("source_text_hash", s(&"b".repeat(64))),
        ("compiler_provenance", map(vec![("compiler_kind", s("offline_rule")), ("compiler_version", s("v1"))])),
        ("assumption_ids", V::List(vec![s("ASM-TEST-B"), s("ASM-TEST-A")])),
        ("policy_dependencies", V::List(Vec::new())),
        ("units", s("count")),
        ("phase", s("start")),
    ])
}

fn event(event_id: &str, due: i64, event_type: &str, payload: Vec<(&str, V)>, marker: V) -> EventRow {
    EventRow::new(
        text(event_id),
        EventKind::Intervention,
        due,
        0,
        100,
        text(event_type),
        entries(payload),
        Some(text("SRC-TEST")),
        None,
        entries(vec![("effect_program_v1", marker)]),
    )
    .unwrap()
}

fn route_event(target: &str, end_minute: Option<i64>, modifier: &str) -> EventRow {
    let payload = vec![
        ("program_id", s(PROGRAM_ID)),
        ("effect_id", s("e1")),
        ("modifier_id", s(modifier)),
        ("target_id", s(target)),
        ("op", s("set_route_access")),
        ("parameters", map(vec![("accessible", V::Bool(false)), ("source_ids", V::List(vec![s("SRC-ROUTE")]))])),
        ("end_minute", end_minute.map_or(V::Null, V::Int)),
    ];
    event("ev-e1", 0, "effect_program_geometry", payload, marker("e1", "set_route_access"))
}

fn order_event(due: i64, staff: &str, task_kind: &str, zone: &str) -> EventRow {
    let parameters = map(vec![("task_kind", s(task_kind)), ("target_zone_id", s(zone)), ("target_id", V::Null), ("reason", s("desk_backlog"))]);
    let payload = vec![
        ("program_id", s(PROGRAM_ID)),
        ("effect_id", s("e2")),
        ("modifier_id", s(ORDER_MODIFIER)),
        ("op", s("staff_task_order")),
        ("staff_id", s(staff)),
        ("parameters", parameters),
    ];
    event("ev-e2", due, "effect_program_structure", payload, marker("e2", "staff_task_order"))
}

fn handoff_event(due: i64, staff: &str, replacement: &str, task: &str) -> EventRow {
    let parameters = map(vec![("staff_id", s(staff)), ("replacement_staff_id", s(replacement)), ("task_id", s(task)), ("reason_code", s("shift_end"))]);
    let payload = vec![
        ("program_id", s(PROGRAM_ID)),
        ("effect_id", s("e3")),
        ("modifier_id", s(HANDOFF_MODIFIER)),
        ("op", s("staff_handoff")),
        ("staff_id", s(staff)),
        ("parameters", parameters),
    ];
    event("ev-e3", due, "effect_program_structure", payload, marker("e3", "staff_handoff"))
}

/// The text of an event's failure.
fn failed<T>(result: Result<T, Error>) -> String {
    match result {
        Err(Error::Outcome(_, text)) => text,
        Err(other) => panic!("not an outcome: {other:?}"),
        Ok(_) => panic!("not an outcome: it passed"),
    }
}

/// The canonical bytes of the world's rows and slots, to see that a refused event changed nothing.
fn bytes(loaded: &Loaded) -> Vec<u8> {
    loaded
        .cell
        .with(|world| {
            let mut encoder = sheltersim_core::json::Encoder::with_capacity(4096);
            let rows = world.rows().unwrap();
            for (_, row) in rows.staff.iter() {
                sheltersim_core::entities::EntityField::write_json(row, &mut encoder, 5).unwrap();
            }
            for (_, row) in rows.active_tasks.iter() {
                sheltersim_core::entities::EntityField::write_json(row, &mut encoder, 5).unwrap();
            }
            for (_, row) in rows.zone_edges.iter() {
                sheltersim_core::entities::EntityField::write_json(row, &mut encoder, 5).unwrap();
            }
            drop(rows);
            for name in world.slots.names() {
                let slot = world.slots.get(name).unwrap().try_borrow().unwrap();
                if let Some(row) = slot.row.as_deref() {
                    encoder.string(name);
                    row.write_json(&mut encoder).unwrap();
                }
            }
            encoder.out.extend_from_slice(format!("{:?}", world.queue().unwrap().pending_ids(None)).as_bytes());
            encoder.out
        })
        .unwrap()
}

fn overlay_of(world: &World) -> V {
    world.slots.try_value(OVERLAY).unwrap().unwrap().clone()
}

fn get<'a>(value: &'a V, path: &[&str]) -> &'a V {
    path.iter().fold(value, |value, key| match value {
        V::Map(map) => map.get(*key).unwrap_or_else(|| panic!("no {key}")),
        other => panic!("not a map at {key}: {other:?}"),
    })
}

// ---- The geometry change -----------------------------------------------------------------------

#[test]
fn a_route_closure_closes_the_edge_and_writes_the_overlay_in_a_kept_transaction() {
    let loaded = loaded();
    let mut host = host(&loaded);
    let row = route_event("corridor_toilet", Some(30), ROUTE_MODIFIER);
    effects::geometry_event(&loaded.cell, &mut host, &row, false).unwrap();
    loaded
        .cell
        .with(|world| {
            assert!(world.rows().unwrap().zone_edges.get("corridor_toilet").unwrap().closed);
            assert!(!world.rows().unwrap().zone_edges.get("toilet_corridor").unwrap().closed);
            let overlay = overlay_of(world);
            let modifier = get(&overlay, &["route_modifiers", "corridor_toilet", "modifiers", ROUTE_MODIFIER]);
            assert_eq!(get(modifier, &["accessible"]), &V::Bool(false));
            assert_eq!(get(modifier, &["end_minute"]), &V::Int(30));
            // The event's source, the parameters' and the marker's assumptions, sorted.
            assert_eq!(get(modifier, &["source_ids"]), &V::List(vec![s("ASM-TEST-A"), s("ASM-TEST-B"), s("SRC-ROUTE"), s("SRC-TEST")]));
        })
        .unwrap();
    // The checks, then the transaction: the floor plan read inside it, the contract, the world's
    // validation, the commit.
    let calls: Vec<&Call> = host.calls.iter().filter(|call| !matches!(call, Call::Holding | Call::FillSection(_))).collect();
    assert!(calls.contains(&&Call::FloorPlan), "{calls:?}");
    assert!(calls.contains(&&Call::Contract(Contract::RouteAccess, get(&row.payload, &["parameters"]).clone())), "{calls:?}");
    assert!(calls.contains(&&Call::ValidateWorld), "{calls:?}");
    assert_eq!(calls.last(), Some(&&Call::Committed(TxnKind::Effect)));
    // Its expiry opens the edge again and pops the modifier.
    effects::geometry_event(&loaded.cell, &mut host, &row, true).unwrap();
    loaded
        .cell
        .with(|world| {
            assert!(!world.rows().unwrap().zone_edges.get("corridor_toilet").unwrap().closed);
            assert_eq!(get(&overlay_of(world), &["route_modifiers"]), &empty());
        })
        .unwrap();
    // A second expiry finds nothing to end.
    assert_eq!(failed(effects::geometry_event(&loaded.cell, &mut host, &row, true)), "geometry modifier was not applied or already expired");
}

#[test]
fn a_geometry_change_that_fails_leaves_the_world_as_it_was() {
    let loaded = loaded();
    let mut host = host(&loaded);
    let before = bytes(&loaded);
    // Not the program's modifier: refused before the transaction.
    assert_eq!(
        failed(effects::geometry_event(&loaded.cell, &mut host, &route_event("corridor_toilet", Some(30), "modifier-other"), false)),
        "geometry effect does not reference its approved program"
    );
    // A route the world does not have, and a closure without an end: the change's own checks, in
    // the transaction, which gives the world back.
    assert_eq!(failed(effects::geometry_event(&loaded.cell, &mut host, &route_event("no_such_edge", Some(30), ROUTE_MODIFIER), false)), "target_missing: route does not exist");
    assert_eq!(
        failed(effects::geometry_event(&loaded.cell, &mut host, &route_event("corridor_toilet", None, ROUTE_MODIFIER), false)),
        "expiry_required: route-access changes require a bounded end"
    );
    assert_eq!(bytes(&loaded), before);
    // Two transactions gave the world back; none kept its writes.
    assert_eq!(host.calls.iter().filter(|call| **call == Call::Restored(TxnKind::Effect)).count(), 2);
    assert!(!host.calls.contains(&Call::Committed(TxnKind::Effect)));
    // Without the overlay the change is not possible.
    loaded.cell.with(|world| world.slots.get(OVERLAY).unwrap().try_borrow_mut().unwrap().row = None).unwrap();
    let failure = failed(effects::geometry_event(&loaded.cell, &mut host, &route_event("corridor_toilet", Some(30), ROUTE_MODIFIER), false));
    assert!(failure.starts_with("geometry_overlay_required"), "{failure}");
}

// ---- The staff task order and the handover ------------------------------------------------------

#[test]
fn a_task_order_starts_the_work_and_a_handover_gives_it_to_an_idle_colleague() {
    let loaded = loaded();
    let mut host = host(&loaded);
    // S01 and S02 are at the information desk with the support skill: a patrol there, where A001
    // waits, needs no travel, so S01 works at once.
    effects::task_order_event(&loaded.cell, &mut host, &order_event(0, "S01", "patrol", "information_desk")).unwrap();
    let (task_id, due) = loaded
        .cell
        .with(|world| {
            let operations = world.slots.try_row::<StaffOperationsRow>(STAFF_OPERATIONS).unwrap();
            let record = operations.staff.get("S01").unwrap();
            assert_eq!(record.phase, Phase::Working);
            assert_eq!(record.causation_event_ids, vec![text("ev-e2")]);
            let task_id = record.task_id.clone().unwrap();
            let rows = world.rows().unwrap();
            let task = rows.active_tasks.get(&task_id).unwrap();
            assert_eq!((&*task.kind, task.staff_id.as_deref(), task.start_minute), ("staff_operation", Some("S01"), 0));
            assert_eq!(rows.staff.get("S01").unwrap().active_task_id.as_deref(), Some(&*task_id));
            // Its completion is pending for S01.
            let queue = world.queue().unwrap();
            let completions: Vec<Arc<EventRow>> = queue
                .ordered()
                .iter()
                .filter_map(|(_, serial)| queue.slot(*serial).row.clone())
                .filter(|row| &*row.event_type == "staff_operation_complete")
                .collect();
            assert_eq!(completions.len(), 1);
            assert_eq!(get(&completions[0].payload, &["staff_id"]), &s("S01"));
            (task_id, task.due_minute)
        })
        .unwrap();
    assert!(due > 0);
    assert_eq!(host.calls.iter().filter(|call| matches!(call, Call::Committed(TxnKind::Effect))).count(), 1);
    // An order for the busy worker is refused, with nothing changed.
    let before = bytes(&loaded);
    assert_eq!(
        failed(effects::task_order_event(&loaded.cell, &mut host, &order_event(0, "S01", "patrol", "information_desk"))),
        "staff task order requires an idle, available, on-shift worker"
    );
    assert_eq!(bytes(&loaded), before);

    // A minute later S02 takes the work over.
    loaded.cell.with(|world| world.scalars.get_mut().minute_index = Fit::Ok(1)).unwrap();
    let made = effects::handoff_event(&loaded.cell, &mut host, &handoff_event(1, "S01", "S02", &task_id)).unwrap();
    loaded
        .cell
        .with(|world| {
            let rows = world.rows().unwrap();
            assert_eq!(rows.active_tasks.get(&task_id).unwrap().staff_id.as_deref(), Some("S02"));
            let (old, new) = (rows.staff.get("S01").unwrap(), rows.staff.get("S02").unwrap());
            assert_eq!((old.active_task_id.as_deref(), old.busy_until, old.work_queue.len()), (None, 0, 0));
            assert_eq!((new.active_task_id.as_deref(), new.busy_until, new.work_queue.clone()), (Some(&*task_id), due, vec![task_id.clone()]));
            drop(rows);
            let operations = world.slots.try_row::<StaffOperationsRow>(STAFF_OPERATIONS).unwrap();
            assert_eq!(operations.staff.get("S01").unwrap().phase, Phase::Idle);
            let handed = operations.staff.get("S02").unwrap();
            assert_eq!(handed.phase, Phase::Working);
            assert_eq!(handed.reason.as_deref(), Some("shift_end"));
            assert_eq!(handed.causation_event_ids, vec![text("ev-e2"), text("ev-e3")]);
            drop(operations);
            // The pending completion is S02's now, under the same event.
            let queue = world.queue().unwrap();
            let completion = queue
                .ordered()
                .iter()
                .filter_map(|(_, serial)| queue.slot(*serial).row.clone())
                .find(|row| &*row.event_type == "staff_operation_complete")
                .unwrap();
            assert_eq!(get(&completion.payload, &["staff_id"]), &s("S02"));
            assert_eq!(completion.due_minute, due);
        })
        .unwrap();
    // The world has the shared operations: the continuity receipt of the handover.
    assert!(made.is_some());
    // The same handover again: S01 no longer works on the task.
    let before = bytes(&loaded);
    assert_eq!(
        failed(effects::handoff_event(&loaded.cell, &mut host, &handoff_event(1, "S01", "S03", &task_id))),
        "staff handoff requires a qualified idle replacement already at active work"
    );
    assert_eq!(bytes(&loaded), before);
}

#[test]
fn a_task_order_and_a_handover_check_their_program_and_participants() {
    let loaded = loaded();
    let mut host = host(&loaded);
    // Not due now.
    assert_eq!(
        failed(effects::task_order_event(&loaded.cell, &mut host, &order_event(5, "S01", "patrol", "information_desk"))),
        "staff task order does not reference a current approved program"
    );
    // A kind no worker skill is registered for.
    assert_eq!(
        failed(effects::task_order_event(&loaded.cell, &mut host, &order_event(0, "S01", "juggling", "information_desk"))),
        "staff task order uses an unregistered task kind"
    );
    // A handover of a task the world does not have.
    assert_eq!(
        failed(effects::handoff_event(&loaded.cell, &mut host, &handoff_event(0, "S01", "S02", "no-such-task"))),
        "staff handoff participants or task are invalid"
    );
    // Without the shared operations a task order is refused.
    loaded.cell.with(|world| world.slots.get(SHARED).unwrap().try_borrow_mut().unwrap().row = None).unwrap();
    assert_eq!(
        failed(effects::task_order_event(&loaded.cell, &mut host, &order_event(0, "S01", "patrol", "information_desk"))),
        "staff task order requires staff and shared operation state"
    );
}
