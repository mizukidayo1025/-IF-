//! The entity rows from outside the core: the bytes of each kind of row in a WorldState document
//! of v5 and v6, the order of a table's rows in a document, and the order of its keys as entities
//! come and go.
//!
//! The expected bytes are those `EntityStore.rows_json(version)` of the `main` build (`174e229`)
//! wrote for the same Python objects; `tests/integration/test_capture_rows.py` checks that those
//! are the bytes `canonical_json` writes for `WorldState._canonical_data`. Those of the zones and
//! zone edges (PR 5-2) are `WorldState._canonical_text(zone.to_dict())` of CPython 3.12.

use std::sync::Arc;

use indexmap::IndexMap;
use sheltersim_core::entities::{
    is_zone_kind, Activity, AgentRow, AnyTable, EdgeRow, Entities, EntityField, EntityRow, FacilityRow, Mobility,
    NeedRow, ProfileRow, RequestRow, RequestStatus, Skills, StaffRow, Table, TaskRow, ZoneRow, ACTIVITIES, AGENT,
    EDGE, MOBILITIES, NEEDS, STATUSES, TASK, ZONE,
};
use sheltersim_core::json::Encoder;
use sheltersim_core::row::{IdMap, StrEnum};
use sheltersim_core::value::V;

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

fn ids(values: &[&str]) -> Vec<Arc<str>> {
    values.iter().map(|value| text(value)).collect()
}

fn json<R: EntityField>(row: &R, version: i64) -> String {
    let mut encoder = Encoder { out: Vec::new() };
    row.write_json(&mut encoder, version).expect("no NaN");
    String::from_utf8(encoder.out).expect("UTF-8")
}

fn needs(toilet_urgency: f64, hunger: f64, fatigue: f64, hydration_need: f64) -> NeedRow {
    NeedRow { toilet_urgency, hunger, fatigue, hydration_need }
}

fn map(entries: Vec<(&str, V)>) -> V {
    let mut map = IndexMap::new();
    for (key, value) in entries {
        map.insert(text(key), value);
    }
    V::Map(map)
}

fn plain_agent() -> AgentRow {
    AgentRow {
        profile: ProfileRow {
            id: text("a1"),
            group_id: Some(text("g1")),
            initial_zone_id: text("z0"),
            mobility: Mobility::Independent,
            support_need: 0.0,
            stress_sensitivity: 1.0,
            rng_key: text("key-a1"),
            initial_needs: needs(0.0, 0.0, 0.0, 0.0),
            provenance: map(vec![]),
        },
        zone_id: text("z0"),
        activity: Activity::Idle,
        target_id: None,
        needs: needs(0.0, 0.0, 0.0, 0.0),
        stress: 0.0,
        trust: 0.5,
        next_decision_at: 0,
        support_request_id: None,
        support_cooldown_until: 0,
    }
}

fn busy_agent() -> AgentRow {
    AgentRow {
        profile: ProfileRow {
            id: text("a2"),
            group_id: None,
            initial_zone_id: text("z1"),
            mobility: Mobility::Wheelchair,
            support_need: 0.25,
            stress_sensitivity: 1e-05,
            rng_key: text("key-a2"),
            initial_needs: needs(0.1, 0.2, 0.30000000000000004, 0.5),
            provenance: map(vec![
                ("source", V::Str(text("合成"))),
                ("list", V::List(vec![V::Int(1), V::Float(2.5), V::Null, V::Bool(true)])),
                ("b", map(vec![("z", V::Int(1)), ("a", V::Str(text("x")))])),
            ]),
        },
        zone_id: text("z1"),
        activity: Activity::SeekingHelp,
        target_id: Some(text("おにぎり\"\\\n\t\u{1}")),
        needs: needs(0.0, 5e-324, 1.0, 0.75),
        stress: 0.1 + 0.2,
        trust: 0.5,
        next_decision_at: i64::MAX,
        support_request_id: Some(text("r1")),
        support_cooldown_until: 30,
    }
}

fn facility() -> FacilityRow {
    FacilityRow {
        id: text("f1"),
        kind: text("toilet"),
        zone_id: text("z0"),
        capacity: 2,
        working_units: 1,
        active_agent_ids: ids(&["a1"]),
        queue_agent_ids: ids(&["a2", "a1"]),
        resource_active_agent_ids: ids(&["a2"]),
        resource_queue_agent_ids: Vec::new(),
    }
}

fn staff() -> StaffRow {
    StaffRow {
        id: text("s1"),
        zone_id: text("z0"),
        skills: Skills(ids(&["triage", "cleaning"])),
        work_queue: ids(&["t1"]),
        busy_until: 90,
        active_task_id: Some(text("t1")),
    }
}

fn open_request() -> RequestRow {
    RequestRow {
        id: text("r0"),
        agent_id: text("a1"),
        requested_minute: 0,
        timeout_minute: 60,
        status: RequestStatus::Pending,
        assigned_staff_id: None,
        task_id: None,
        resolved_minute: None,
        cooldown_until: 0,
        last_assignment_attempt_minute: None,
        last_assignment_wait_reason_code: None,
    }
}

fn assigned_request() -> RequestRow {
    RequestRow {
        id: text("r1"),
        agent_id: text("a2"),
        requested_minute: 10,
        timeout_minute: 70,
        status: RequestStatus::Assigned,
        assigned_staff_id: Some(text("s1")),
        task_id: Some(text("t1")),
        resolved_minute: None,
        cooldown_until: 0,
        last_assignment_attempt_minute: Some(12),
        last_assignment_wait_reason_code: Some(text("no_available_qualified_staff")),
    }
}

fn task() -> TaskRow {
    TaskRow {
        id: text("t1"),
        kind: text("support"),
        agent_id: Some(text("a2")),
        resource_id: None,
        start_minute: 12,
        due_minute: 90,
        staff_id: Some(text("s1")),
    }
}

#[test]
fn agents_write_the_bytes_of_each_version() {
    assert_eq!(
        json(&plain_agent(), 5),
        r#"{"activity":"idle","needs":{"fatigue":0.0,"hunger":0.0,"toilet_urgency":0.0},"next_decision_at":0,"profile":{"group_id":"g1","id":"a1","initial_needs":{"fatigue":0.0,"hunger":0.0,"toilet_urgency":0.0},"initial_zone_id":"z0","mobility":"independent","provenance":{},"rng_key":"key-a1","stress_sensitivity":1.0,"support_need":0.0},"stress":0.0,"support_cooldown_until":0,"support_request_id":null,"target_id":null,"trust":0.5,"zone_id":"z0"}"#
    );
    assert_eq!(
        json(&plain_agent(), 6),
        r#"{"activity":"idle","needs":{"fatigue":0.0,"hunger":0.0,"hydration_need":0.0,"toilet_urgency":0.0},"next_decision_at":0,"profile":{"group_id":"g1","id":"a1","initial_needs":{"fatigue":0.0,"hunger":0.0,"hydration_need":0.0,"toilet_urgency":0.0},"initial_zone_id":"z0","mobility":"independent","provenance":{},"rng_key":"key-a1","stress_sensitivity":1.0,"support_need":0.0},"stress":0.0,"support_cooldown_until":0,"support_request_id":null,"target_id":null,"trust":0.5,"zone_id":"z0"}"#
    );
    assert_eq!(
        json(&busy_agent(), 5),
        r#"{"activity":"seeking_help","needs":{"fatigue":1.0,"hunger":5e-324,"toilet_urgency":0.0},"next_decision_at":9223372036854775807,"profile":{"group_id":null,"id":"a2","initial_needs":{"fatigue":0.30000000000000004,"hunger":0.2,"toilet_urgency":0.1},"initial_zone_id":"z1","mobility":"wheelchair","provenance":{"b":{"a":"x","z":1},"list":[1,2.5,null,true],"source":"合成"},"rng_key":"key-a2","stress_sensitivity":1e-05,"support_need":0.25},"stress":0.30000000000000004,"support_cooldown_until":30,"support_request_id":"r1","target_id":"おにぎり\"\\\n\t\u0001","trust":0.5,"zone_id":"z1"}"#
    );
    assert_eq!(
        json(&busy_agent(), 6),
        r#"{"activity":"seeking_help","needs":{"fatigue":1.0,"hunger":5e-324,"hydration_need":0.75,"toilet_urgency":0.0},"next_decision_at":9223372036854775807,"profile":{"group_id":null,"id":"a2","initial_needs":{"fatigue":0.30000000000000004,"hunger":0.2,"hydration_need":0.5,"toilet_urgency":0.1},"initial_zone_id":"z1","mobility":"wheelchair","provenance":{"b":{"a":"x","z":1},"list":[1,2.5,null,true],"source":"合成"},"rng_key":"key-a2","stress_sensitivity":1e-05,"support_need":0.25},"stress":0.30000000000000004,"support_cooldown_until":30,"support_request_id":"r1","target_id":"おにぎり\"\\\n\t\u0001","trust":0.5,"zone_id":"z1"}"#
    );
}

#[test]
fn facilities_write_their_resource_lists_only_in_v6() {
    assert_eq!(
        json(&facility(), 5),
        r#"{"active_agent_ids":["a1"],"capacity":2,"id":"f1","kind":"toilet","queue_agent_ids":["a2","a1"],"working_units":1,"zone_id":"z0"}"#
    );
    assert_eq!(
        json(&facility(), 6),
        r#"{"active_agent_ids":["a1"],"capacity":2,"id":"f1","kind":"toilet","queue_agent_ids":["a2","a1"],"resource_active_agent_ids":["a2"],"resource_queue_agent_ids":[],"working_units":1,"zone_id":"z0"}"#
    );
}

#[test]
fn staff_requests_and_tasks_write_the_same_bytes_in_both_versions() {
    let staff_bytes = r#"{"active_task_id":"t1","busy_until":90,"id":"s1","skills":["triage","cleaning"],"work_queue":["t1"],"zone_id":"z0"}"#;
    let open = r#"{"agent_id":"a1","assigned_staff_id":null,"cooldown_until":0,"id":"r0","requested_minute":0,"resolved_minute":null,"status":"pending","task_id":null,"timeout_minute":60}"#;
    // The two last fields are left out while they are None.
    let assigned = r#"{"agent_id":"a2","assigned_staff_id":"s1","cooldown_until":0,"id":"r1","last_assignment_attempt_minute":12,"last_assignment_wait_reason_code":"no_available_qualified_staff","requested_minute":10,"resolved_minute":null,"status":"assigned","task_id":"t1","timeout_minute":70}"#;
    let task_bytes = r#"{"agent_id":"a2","due_minute":90,"id":"t1","kind":"support","resource_id":null,"staff_id":"s1","start_minute":12}"#;
    for version in [5, 6] {
        assert_eq!(json(&staff(), version), staff_bytes);
        assert_eq!(json(&open_request(), version), open);
        assert_eq!(json(&assigned_request(), version), assigned);
        assert_eq!(json(&task(), version), task_bytes);
    }
}

#[test]
fn a_nan_is_refused_with_the_text_of_canonical_json() {
    let mut agent = plain_agent();
    agent.stress = f64::NAN;
    let mut encoder = Encoder { out: Vec::new() };
    let error = agent.write_json(&mut encoder, 6).expect_err("NaN");
    let mut expected = Encoder { out: Vec::new() };
    assert_eq!(error.to_string(), expected.try_float(f64::NAN).expect_err("NaN").to_string());
}

#[test]
fn enum_members_keep_the_order_and_values_of_the_python_enums() {
    assert_eq!(
        ACTIVITIES.iter().map(|member| member.as_text()).collect::<Vec<_>>(),
        ["idle", "moving", "queued", "in_service", "seeking_help", "sleeping", "resting"]
    );
    assert_eq!(MOBILITIES.iter().map(|member| member.as_text()).collect::<Vec<_>>(), ["independent", "assisted", "wheelchair"]);
    assert_eq!(
        STATUSES.iter().map(|member| member.as_text()).collect::<Vec<_>>(),
        ["pending", "assigned", "resolved", "unresolved"]
    );
    for (index, member) in ACTIVITIES.iter().enumerate() {
        assert_eq!(member.index(), index);
    }
    assert_eq!(<Activity as StrEnum>::MISFIT, "not a Activity");
}

#[test]
fn same_compares_floats_by_their_bits() {
    let mut first = plain_agent();
    let second = plain_agent();
    assert!(first.same(&second));
    first.needs.hunger = -0.0;
    assert!(!first.same(&second));
    first.needs.hunger = f64::NAN;
    let mut third = plain_agent();
    third.needs.hunger = f64::NAN;
    assert!(first.same(&third));
}

/// A table of the given rows, keyed by their IDs in this order.
fn table_of(rows: Vec<RequestRow>) -> Table<RequestRow> {
    let mut table = Table::default();
    for row in rows {
        let slot = table.reserve_slot();
        let key = text(row.id());
        table.fill_slot(slot, row);
        assert_eq!(table.bind(key, slot), None);
    }
    table
}

fn request(id: &str) -> RequestRow {
    RequestRow { id: text(id), ..open_request() }
}

fn keys<R>(table: &Table<R>) -> Vec<String> {
    table.iter().map(|(id, _)| id.to_string()).collect()
}

#[test]
fn keys_keep_the_dict_order_as_entities_come_and_go() {
    let mut table = table_of(vec![request("r3"), request("r1"), request("r2")]);
    assert_eq!(keys(&table), ["r3", "r1", "r2"]);
    // shift_remove: the others keep their order, and the slot is free again.
    let removed = table.remove("r1").expect("r1 is there");
    assert_eq!(keys(&table), ["r3", "r2"]);
    assert!(table.row_at(removed).is_none());
    assert_eq!(table.remove("r1"), None);
    // A new key goes to the end, in the free slot.
    let slot = table.reserve_slot();
    assert_eq!(slot, removed);
    table.fill_slot(slot, request("r1"));
    table.bind(text("r1"), slot);
    assert_eq!(keys(&table), ["r3", "r2", "r1"]);
    // A key already there keeps its place and takes the new slot.
    let replacement = table.reserve_slot();
    table.fill_slot(replacement, RequestRow { cooldown_until: 9, ..request("r3") });
    let old = table.bind(text("r3"), replacement).expect("r3 had a slot");
    table.release_slot(old);
    assert_eq!(keys(&table), ["r3", "r2", "r1"]);
    assert_eq!(table.get("r3").map(|row| row.cooldown_until), Some(9));
    assert_eq!(table.len(), 3);
    // A document orders the rows by their own IDs, whatever the dict's order.
    let ids: Vec<&str> =
        table.document_slots().into_iter().map(|slot| table.row_at(slot).expect("a row").id()).collect();
    assert_eq!(ids, ["r1", "r2", "r3"]);
}

#[test]
fn keys_can_be_replaced_and_reordered() {
    let mut table = table_of(vec![request("r1"), request("r2"), request("r3")]);
    table.reorder_keys(&["r3", "unknown", "r1"]);
    // r2 is left out of the order: it leaves the keys.
    assert_eq!(keys(&table), ["r3", "r1"]);
    let mut kept = IdMap::<u32>::default();
    kept.insert(text("r1"), 0);
    table.set_keys(kept);
    assert_eq!(keys(&table), ["r1"]);
}

#[test]
fn a_write_marks_the_row_changed_until_its_bytes_are_kept() {
    let mut table = table_of(vec![request("r1")]);
    let slot = table.document_slots()[0];
    assert!(table.changed(slot));
    table.mark_written(slot);
    assert!(!table.changed(slot));
    let _ = table.get("r1");
    assert!(!table.changed(slot));
    table.get_mut("r1").expect("r1").cooldown_until = 5;
    assert!(table.changed(slot));
}

// ---- Zones and zone edges (PR 5-2) ---------------------------------------------------------

#[test]
fn zones_and_edges_write_the_bytes_of_to_dict_in_both_versions() {
    let corridor = ZoneRow {
        id: text("z1"),
        name: text("共用通路"),
        capacity: None,
        display_x: Some(100.0),
        display_y: Some(260.5),
    };
    let sleeping = ZoneRow {
        id: text("sleeping"),
        name: text("Sleeping \"A\""),
        capacity: Some(30),
        display_x: None,
        display_y: Some(1e-05),
    };
    let open = EdgeRow {
        id: text("e1"),
        from_zone_id: text("a"),
        to_zone_id: text("b"),
        travel_minutes: 3,
        closed: false,
    };
    let closed = EdgeRow { id: text("e2"), from_zone_id: text("b"), to_zone_id: text("a"), travel_minutes: 0, closed: true };
    for version in [5, 6] {
        assert_eq!(
            json(&corridor, version),
            r#"{"capacity":null,"display_x":100.0,"display_y":260.5,"id":"z1","name":"共用通路"}"#
        );
        assert_eq!(
            json(&sleeping, version),
            r#"{"capacity":30,"display_x":null,"display_y":1e-05,"id":"sleeping","name":"Sleeping \"A\""}"#
        );
        assert_eq!(
            json(&open, version),
            r#"{"closed":false,"from_zone_id":"a","id":"e1","to_zone_id":"b","travel_minutes":3}"#
        );
        assert_eq!(
            json(&closed, version),
            r#"{"closed":true,"from_zone_id":"b","id":"e2","to_zone_id":"a","travel_minutes":0}"#
        );
    }
    assert_eq!(corridor.id(), "z1");
    assert_eq!(closed.id(), "e2");
    assert_eq!(ZoneRow::NAMES, ["id", "name", "capacity", "display_x", "display_y"]);
    assert_eq!(EdgeRow::NAMES, ["id", "from_zone_id", "to_zone_id", "travel_minutes", "closed"]);
    let mut nan = corridor.clone();
    nan.display_x = Some(f64::NAN);
    let mut encoder = Encoder { out: Vec::new() };
    assert!(nan.write_json(&mut encoder, 6).is_err());
}

fn zone(id: &str) -> ZoneRow {
    ZoneRow { id: text(id), name: text(id), capacity: None, display_x: None, display_y: None }
}

fn with_rows() -> Entities {
    let mut rows = Entities::default();
    for id in ["b", "a"] {
        let slot = rows.zones.reserve_slot();
        rows.zones.fill_slot(slot, zone(id));
        rows.zones.bind(text(id), slot);
    }
    let slot = rows.active_tasks.reserve_slot();
    rows.active_tasks.fill_slot(slot, task());
    rows.active_tasks.bind(text("t1"), slot);
    rows
}

#[test]
fn the_zones_have_kinds_and_tables_of_their_own() {
    assert!(is_zone_kind(ZONE) && is_zone_kind(EDGE));
    assert!(![AGENT, NEEDS, TASK].into_iter().any(is_zone_kind));
    let rows = with_rows();
    assert_eq!(rows.table(ZONE).len(), 2);
    assert_eq!(rows.table(EDGE).len(), 0);
    assert_eq!(rows.table(TASK).len(), 1);
    assert_eq!(keys(&rows.zones), ["b", "a"]);
    assert_eq!(rows.zones.document_slots(), [1, 0]);
}

#[test]
fn a_taint_of_the_zones_leaves_the_people_and_the_other_way_round() {
    let mut rows = with_rows();
    rows.taint_for(ZONE, "a zone does not fit".to_owned());
    assert_eq!(rows.zones_tainted(), Some("a zone does not fit"));
    assert_eq!(rows.tainted_for(EDGE), Some("a zone does not fit"));
    assert_eq!(rows.tainted(), None);
    assert!(rows.zones.is_empty() && rows.zone_edges.is_empty());
    assert_eq!(rows.active_tasks.len(), 1);
    rows.taint_for(EDGE, "later".to_owned());
    assert_eq!(rows.zones_tainted(), Some("a zone does not fit"));

    let mut rows = with_rows();
    rows.taint("a task does not fit".to_owned());
    assert_eq!(rows.tainted_for(TASK), Some("a task does not fit"));
    assert_eq!(rows.zones_tainted(), None);
    assert!(rows.active_tasks.is_empty());
    assert_eq!(keys(&rows.zones), ["b", "a"]);
    rows.clear();
    assert_eq!((rows.tainted(), rows.zones_tainted()), (None, None));
    assert!(rows.zones.is_empty());
}
