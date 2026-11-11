//! Lane 6-2: the order a copy through canonical JSON gives (`canonicalize`), the copy on write of a
//! world's parts (`snapshot`, `restore`), the EventLog's mark and rollback, the queue made again in
//! pending order, and the transaction around them (`txn::Txn`).
//!
//! The order of `WorldState.from_dict(world.to_dict())` is that of `json.dumps(sort_keys=True)`:
//! the keys of every dict sorted by code point (UTF-8 bytes), lists as they are, the entities by
//! their own ID (`_canonical_data`) and the pending events in queue order (`EventQueue([...])`).
//! tests/integration/test_world_txn.py compares a transaction's start with from_dict itself.

use std::sync::Arc;

use indexmap::IndexMap;
use sheltersim_core::cell::{CowShard, Shard};
use sheltersim_core::entities::{
    Activity, AgentRow, AnyTable, EntityField, Entities, Mobility, NeedRow, ProfileRow, Table,
};
use sheltersim_core::error::Error;
use sheltersim_core::events::log::{EventLog, Fields};
use sheltersim_core::events::queue::Queue;
use sheltersim_core::events::{EventKind, EventRow};
use sheltersim_core::json::Encoder;
use sheltersim_core::row::{Field, IdMap, Maybe, Row, ValueRow};
use sheltersim_core::state::body_state::{History, Relationship as BodyRelationship, Relationships as BodyRelationships};
use sheltersim_core::state::effect_program::{EffectProgramRow, FacilityModifiers, Modifier, Program};
use sheltersim_core::state::shared_operations::{CareHead, CareStatus, Heads};
use sheltersim_core::state::social_network::{Relationship, Relationships};
use sheltersim_core::state::staff_dynamics::{ConfigRow, LastActivity, Shift, StaffDynamicsRow, StaffRecord};
use sheltersim_core::txn::{Host, Stage, Txn, TxnEnd, TxnKind, NESTED};
use sheltersim_core::value::V;
use sheltersim_core::world::{Fit, Place, Snapshot, World, WorldCell};

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

/// Keys in an order no sort gives: capitals, non-ASCII and prefixes, shuffled.
const SHUFFLED: [&str; 7] = ["b", "é", "a", "Z", "ab", "_", "aa"];
/// The same keys by their bytes, as `json.dumps(sort_keys=True)` orders them.
const SORTED: [&str; 7] = ["Z", "_", "a", "aa", "ab", "b", "é"];

fn json<T: Field>(value: &T) -> Vec<u8> {
    let mut encoder = Encoder { out: Vec::new() };
    value.write_json(&mut encoder).unwrap();
    encoder.out
}

fn keys<T>(map: &IdMap<T>) -> Vec<&str> {
    map.keys().map(|key| &**key).collect()
}

fn value_keys(value: &V) -> Vec<&str> {
    match value {
        V::Map(map) => map.keys().map(|key| &**key).collect(),
        _ => panic!("not a map"),
    }
}

fn id_map<T>(make: impl Fn(&str) -> T) -> IdMap<T> {
    SHUFFLED.iter().map(|key| (text(key), make(key))).collect()
}

/// A map in the shuffled order, with a list of maps under one key and a nested map under another.
fn shuffled_value(depth: usize) -> V {
    let mut map = IndexMap::new();
    for (index, key) in SHUFFLED.iter().enumerate() {
        let item = match (index, depth) {
            (0, 0) => V::List(vec![shuffled_value(1), V::Int(1), shuffled_value(1)]),
            (2, 0) => shuffled_value(1),
            _ => V::Float(index as f64),
        };
        map.insert(text(key), item);
    }
    V::Map(map)
}

/// Every map of the value has its keys sorted; lists keep their items where they were.
fn assert_sorted_value(value: &V) {
    match value {
        V::Map(map) => {
            let keys: Vec<&str> = map.keys().map(|key| &**key).collect();
            let mut sorted = keys.clone();
            sorted.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
            assert_eq!(keys, sorted);
            map.values().for_each(assert_sorted_value);
        }
        V::List(items) => items.iter().for_each(assert_sorted_value),
        _ => {}
    }
}

#[test]
fn a_value_sorts_every_map_and_keeps_its_lists_and_bytes() {
    let mut value = shuffled_value(0);
    let before = json(&value);
    value.canonicalize();
    assert_eq!(value_keys(&value), SORTED);
    assert_sorted_value(&value);
    let V::Map(map) = &value else { unreachable!() };
    // The list under "b" keeps its items in place: map, 1, map.
    let V::List(items) = &map["b"] else { unreachable!() };
    assert!(matches!(items.as_slice(), [V::Map(_), V::Int(1), V::Map(_)]));
    assert_eq!(json(&value), before);
    // A value sorted already stays as it is.
    let again = value.clone();
    value.canonicalize();
    assert_eq!(value, again);
    assert_eq!(value_keys(&value), SORTED);
}

#[test]
fn records_and_maps_of_records_sort_their_maps_and_keep_their_fields() {
    let program = |key: &str| Program {
        program_hash: text(key),
        source_text_hash: text("s"),
        narrative: text("n"),
        compiler_provenance: shuffled_value(0),
        assumptions: V::List(vec![shuffled_value(1)]),
        support_mode: text("m"),
        effect_ids: vec![text("e2"), text("e1")],
        operators: id_map(|key| text(key)),
        scheduled_event_ids: vec![text("x"), text("a")],
    };
    let modifiers = |key: &str| FacilityModifiers {
        baseline_working_units: 1,
        modifiers: vec![
            Modifier {
                modifier_id: text(key),
                program_id: text("p"),
                effect_id: text("e"),
                working_units: 1,
                start_minute: 0,
                sequence: 2,
            },
            Modifier {
                modifier_id: text("a"),
                program_id: text("p"),
                effect_id: text("e"),
                working_units: 1,
                start_minute: 0,
                sequence: 1,
            },
        ],
    };
    let mut row = EffectProgramRow { schema_version: 1, programs: id_map(program), active_facility_modifiers: id_map(modifiers) };
    let before = json(&row);
    Field::canonicalize(&mut row);
    assert_eq!(keys(&row.programs), SORTED);
    assert_eq!(keys(&row.active_facility_modifiers), SORTED);
    for program in row.programs.values() {
        assert_eq!(keys(&program.operators), SORTED);
        assert_sorted_value(&program.compiler_provenance);
        assert_sorted_value(&program.assumptions);
        // Lists keep their order.
        assert_eq!(program.effect_ids, [text("e2"), text("e1")]);
        assert_eq!(program.scheduled_event_ids, [text("x"), text("a")]);
    }
    let first = &row.active_facility_modifiers["Z"].modifiers;
    assert_eq!(first.iter().map(|modifier| modifier.sequence).collect::<Vec<_>>(), [2, 1]);
    assert_eq!(json(&row), before);
}

#[test]
fn maybe_and_option_sort_what_they_hold() {
    let record = StaffRecord {
        consecutive_work_minutes: 0,
        total_work_minutes: 0,
        total_rest_minutes: 0,
        fatigue: 0.0,
        rest_until_minute: 0,
        last_activity: LastActivity::Working,
    };
    let mut row = StaffDynamicsRow {
        schema_version: 1,
        policy_version: text("p"),
        source_id: text("s"),
        config: ConfigRow {
            schema_version: 1,
            policy_version: text("p"),
            source_id: text("s"),
            max_continuous_work_minutes: 1,
            max_continuous_work_minutes_by_staff: Maybe(Some(id_map(|_| 3))),
            rest_minutes: 1,
            shift_windows_by_staff: Maybe(Some(id_map(|_| {
                vec![Shift { start_minute: 9, end_minute: 10 }, Shift { start_minute: 1, end_minute: 2 }]
            }))),
            off_shift_fatigue_recovery_per_minute: Maybe(None),
        },
        staff: id_map(|_| record),
    };
    let before = json(&row);
    Field::canonicalize(&mut row);
    assert_eq!(keys(row.config.max_continuous_work_minutes_by_staff.0.as_ref().unwrap()), SORTED);
    let shifts = row.config.shift_windows_by_staff.0.as_ref().unwrap();
    assert_eq!(keys(shifts), SORTED);
    assert_eq!(shifts["a"].iter().map(|shift| shift.start_minute).collect::<Vec<_>>(), [9, 1]);
    assert_eq!(keys(&row.staff), SORTED);
    assert_eq!(json(&row), before);
    let mut absent: Maybe<IdMap<i64>> = Maybe(None);
    Field::canonicalize(&mut absent);
    assert!(absent.0.is_none());
    let mut option: Option<V> = Some(shuffled_value(1));
    Field::canonicalize(&mut option);
    assert_eq!(value_keys(option.as_ref().unwrap()), SORTED);
}

#[test]
fn the_hand_written_fields_sort_their_maps_and_keep_their_bytes() {
    // social_network_v1.relationships.
    let mut social = Relationships::from_map(id_map(|_| Relationship::empty()));
    let before = json(&social);
    Field::canonicalize(&mut social);
    assert_eq!(keys(&social.map), SORTED);
    assert_eq!(json(&social), before);
    // body_state_v1.relationships: what was checked stays as it was.
    let body_record = BodyRelationship {
        contact_count: 1,
        trust: None,
        contact_minutes: 2,
        last_contact_minute: None,
        last_interaction: None,
        topic_ids: vec![text("t2"), text("t1")],
    };
    let mut body = BodyRelationships::from_map(id_map(|_| body_record.clone()));
    body.written = vec![text("b"), text("a")];
    body.read_all = false;
    let before = json(&body);
    Field::canonicalize(&mut body);
    assert_eq!(keys(&body.map), SORTED);
    assert_eq!(body.map["a"].topic_ids, [text("t2"), text("t1")]);
    assert_eq!(body.written, [text("b"), text("a")]);
    assert!(!body.read_all);
    assert_eq!(json(&body), before);
    // shared_operations_v1.support_requests.
    let head = |key: &str| CareHead {
        request_id: text(key),
        agent_id: text("a1"),
        household_id: None,
        support_kind: text("toilet"),
        status: CareStatus::Requested,
        created_minute: 0,
        updated_minute: 0,
        latest_event_id: text("e"),
    };
    let mut heads = Heads::unchecked(id_map(head));
    let before = json(&heads);
    Field::canonicalize(&mut heads);
    assert_eq!(keys(&heads.map), SORTED);
    assert_eq!(json(&heads), before);
    // A history of canonical bytes stays as it is.
    let mut history = History::default();
    history.items.push_back(Box::from(&b"{\"b\":1}"[..]));
    history.items.push_back(Box::from(&b"{\"a\":2}"[..]));
    let before = json(&history);
    Field::canonicalize(&mut history);
    assert_eq!(json(&history), before);
}

#[test]
fn a_row_behind_the_trait_sorts_and_copies_itself() {
    let mut row: Box<dyn Row> = Box::new(ValueRow(shuffled_value(0)));
    let copy = row.clone_row();
    row.canonicalize();
    let read = |row: &dyn Row| value_keys(&row.as_any().downcast_ref::<ValueRow>().unwrap().0).join(",");
    assert_eq!(read(&*row), SORTED.join(","));
    // The copy is a value of its own.
    assert_eq!(read(&*copy), SHUFFLED.join(","));
    let mut map: Box<dyn Row> = Box::new(id_map(|_| 1.5_f64));
    map.canonicalize();
    assert_eq!(keys(map.as_any().downcast_ref::<IdMap<f64>>().unwrap()), SORTED);
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
            provenance: shuffled_value(1),
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

fn put<R: Clone>(table: &mut Table<R>, id: &str, row: R) -> u32 {
    let slot = table.reserve_slot();
    table.fill_slot(slot, row);
    assert_eq!(table.bind(text(id), slot), None);
    slot
}

fn row_json(row: &AgentRow) -> Vec<u8> {
    let mut encoder = Encoder { out: Vec::new() };
    row.write_json(&mut encoder, 6).unwrap();
    encoder.out
}

#[test]
fn a_table_orders_its_keys_by_the_entity_id_and_touches_nothing() {
    let mut table: Table<AgentRow> = Table::default();
    let ids = ["a10", "a2", "A1", "a1"];
    let slots: Vec<u32> = ids.iter().map(|id| put(&mut table, id, agent(id))).collect();
    let bytes: Vec<Vec<u8>> = ids.iter().map(|id| row_json(table.get(id).unwrap())).collect();
    table.canonicalize();
    assert_eq!(table.key_order().iter().map(|key| &**key).collect::<Vec<_>>(), ["A1", "a1", "a10", "a2"]);
    // Each key keeps its slot and its row, whose maps are sorted, with the same bytes.
    for ((id, slot), bytes) in ids.iter().zip(&slots).zip(&bytes) {
        assert_eq!(table.slot_of(id), Some(*slot));
        let row = table.get(id).unwrap();
        assert_eq!(value_keys(&row.profile.provenance), SORTED);
        assert_eq!(&row_json(row), bytes);
        // The view keeps what the object holds, which compares equal in any order.
        let view = table.view_at(*slot).unwrap();
        assert_eq!(value_keys(&view.profile.provenance), SHUFFLED);
        assert!(view.profile.provenance.same(&row.profile.provenance));
    }
    assert!(!table.has_changes());
    // The same ID twice (keys that are not their IDs) keeps the dict's order between them.
    let mut doubled: Table<AgentRow> = Table::default();
    put(&mut doubled, "k2", agent("b"));
    put(&mut doubled, "k1", agent("a"));
    put(&mut doubled, "k3", agent("a"));
    doubled.canonicalize();
    assert_eq!(doubled.key_order().iter().map(|key| &**key).collect::<Vec<_>>(), ["k1", "k3", "k2"]);
}

fn event(id: &str, due: i64, sequence: i64) -> EventRow {
    let payload: IndexMap<Arc<str>, V> = SHUFFLED.iter().map(|key| (text(key), V::Int(1))).collect();
    let mut provenance: IndexMap<Arc<str>, V> = IndexMap::new();
    provenance.insert(text("source"), V::Str(text("s")));
    provenance.insert(text("object"), shuffled_value(1));
    EventRow::new(text(id), EventKind::Exogenous, due, 0, sequence, text("delivery"), payload, None, None, provenance).unwrap()
}

#[test]
fn a_queue_is_scheduled_again_in_queue_order_under_new_serials() {
    let mut queue = Queue::new();
    let late = queue.add(text("late"), (30, 0, 5), Some(Arc::new(event("late", 30, 5))));
    let early = queue.add(text("early"), (10, 0, 7), None);
    let tie_second = queue.add(text("tie-b"), (20, 0, 2), Some(Arc::new(event("tie-b", 20, 2))));
    let tie_first = queue.add(text("tie-a"), (20, 0, 1), None);
    let kept = queue.slot(late).row.clone().unwrap();
    let generation = queue.generation();
    let moved = queue.canonicalize();
    let old: Vec<u64> = moved.iter().map(|(old, _)| *old).collect();
    assert_eq!(old, [early, tie_first, tie_second, late]);
    // New serials, past every serial given before, in queue order: insertion is the queue order.
    let new: Vec<u64> = moved.iter().map(|(_, new)| *new).collect();
    assert!(new.iter().all(|serial| *serial > tie_first));
    assert_eq!(queue.insertion(), new.as_slice());
    assert_eq!(queue.ordered().iter().map(|(_, serial)| *serial).collect::<Vec<_>>(), new);
    assert_eq!(queue.pending_ids(None).iter().map(|id| &**id).collect::<Vec<_>>(), ["early", "tie-a", "tie-b", "late"]);
    assert_eq!(queue.serial_of("late"), Some(new[3]));
    assert!(queue.has_sequence(7));
    assert_ne!(queue.generation(), generation);
    assert_eq!(queue.validate(), 0);
    // A row keeps its values, with its payload and provenance sorted; the old one is untouched.
    let row = queue.slot(new[3]).row.clone().unwrap();
    assert_eq!(value_keys(&row.payload), SORTED);
    assert_eq!(value_keys(&row.provenance), ["object", "source"]);
    assert_sorted_value(&row.provenance);
    assert_eq!(value_keys(&kept.payload), SHUFFLED);
    assert!(queue.slot(new[0]).row.is_none());
    let mut a = Encoder { out: Vec::new() };
    let mut b = Encoder { out: Vec::new() };
    row.write_json(&mut a).unwrap();
    kept.write_json(&mut b).unwrap();
    assert_eq!(a.out, b.out);
}

fn fields(id: &str, sequence: i64, event_type: &str, minute: i64) -> Fields {
    Fields {
        event_id: id.to_owned(),
        sequence,
        event_type: event_type.to_owned(),
        minute,
        applied: true,
        ..Fields::default()
    }
}

/// A log of every kind of record, from `start`: care transitions, facility events, lifecycle steps,
/// provenance objects, and types new and old.
fn append_all(log: &mut EventLog, start: i64, minute: i64) {
    let mut sequence = start;
    let mut next = |log: &mut EventLog, mut item: Fields| {
        item.sequence = sequence;
        item.event_id = format!("{}-{sequence}", item.event_id);
        sequence += 1;
        assert_eq!(log.add(item, false, Some(Box::from(&b"{}"[..]))), 0);
    };
    let mut care = fields("care", 0, "care_support_transition", minute);
    care.request_id = Some(format!("r{start}"));
    care.agent_id = Some("a1".to_owned());
    care.support_kind = Some(format!("kind{start}"));
    care.payload_status_completed = true;
    next(log, care.clone());
    care.request_id = Some("r-shared".to_owned());
    next(log, care);
    let mut outage = fields("outage", 0, "facility_outage", minute);
    outage.facility_id = Some("f1".to_owned());
    next(log, outage);
    for transition in ["started", "completed", "continued"] {
        let mut step = fields("life", 0, "agent_action_lifecycle", minute);
        step.agent_id = Some(format!("agent{start}"));
        step.transition = Some(transition.to_owned());
        next(log, step);
    }
    let mut provenance = fields("prov", 0, &format!("type{start}"), minute + 1);
    provenance.provenance_objects = vec!["object".to_owned(), format!("object{start}")];
    next(log, provenance);
    let mut unencoded = fields("plain", 0, "delivery", minute + 1);
    unencoded.applied = false;
    assert_eq!(
        log.add(Fields { sequence: start + 100, event_id: format!("plain-{start}"), ..unencoded }, false, None),
        0
    );
}

#[test]
fn a_log_rolled_back_to_its_mark_is_what_it_was() {
    let mut log = EventLog::new();
    append_all(&mut log, 0, 1);
    assert_eq!(log.audit(Some(5), Some(100)), 0);
    log.scan_type("delivery");
    let before = log.clone();
    let mark = log.mark();
    assert_eq!(mark.len(), before.len());
    append_all(&mut log, 1_000, 3);
    append_all(&mut log, 2_000, 4);
    assert_eq!(log.audit(Some(9), Some(100)), 0);
    log.scan_type("type1000");
    assert_ne!(log, before);
    log.rollback(mark);
    assert_eq!(log, before);
    // The indexes answer as before, and appending again works the same.
    assert!(!log.has_event_id("care-1000"));
    assert!(!log.has_sequence(1_000));
    assert_eq!(log.type_id("type1000"), None);
    assert_eq!(log.completed_kinds("a1"), ["kind0".to_owned()]);
    assert_eq!(log.first_care_request_event_of("r-shared"), Some("care-1"));
    assert_eq!(log.first_care_request_event_of("r1000"), None);
    let mut again = before.clone();
    append_all(&mut log, 1_000, 3);
    append_all(&mut again, 1_000, 3);
    assert_eq!(log, again);
    // A mark with nothing appended since rolls back to the same log.
    let same = log.clone();
    let mark = log.mark();
    log.rollback(mark);
    assert_eq!(log, same);
}

#[test]
fn a_cow_shard_copies_a_shared_value_before_a_write() {
    let mut shard = CowShard::new(vec![1, 2]);
    let kept = shard.share().unwrap();
    shard.try_borrow_mut().unwrap().push(3);
    assert_eq!(*kept, [1, 2]);
    assert_eq!(*shard.try_borrow().unwrap(), [1, 2, 3]);
    assert!(!shard.is_shared());
    // A borrowed part can't be shared, and two borrows are refused as for a Shard.
    {
        let _writing = shard.try_borrow_mut().unwrap();
        assert!(shard.share().is_err());
        assert!(shard.try_borrow().is_err());
    }
    let snapshot = shard.share_mut();
    assert!(shard.is_shared());
    shard.get_mut().clear();
    shard.put(snapshot);
    assert_eq!(*shard.try_borrow().unwrap(), [1, 2, 3]);
}

fn slot_state(world: &mut World) -> Vec<(String, Option<Vec<u8>>, bool, u64)> {
    let names: Vec<Arc<str>> = world.slots.names().cloned().collect();
    names
        .iter()
        .map(|name| {
            let slot = world.slots.get(name).unwrap().try_borrow().unwrap();
            let bytes = slot.row.as_deref().map(|row| {
                let mut encoder = Encoder { out: Vec::new() };
                row.write_json(&mut encoder).unwrap();
                encoder.out
            });
            (name.to_string(), bytes, slot.detached, slot.generation.get())
        })
        .collect()
}

/// A world of two agents, a value slot, a map slot and its scalars, all in a shuffled order.
fn small_world() -> World {
    let mut world = World::default();
    {
        let rows = world.entities.get_mut();
        put(&mut rows.agents, "b", agent("b"));
        put(&mut rows.agents, "a", agent("a"));
    }
    world.scalars.get_mut().minute_index = Fit::Ok(5);
    let value = world.slots.add(text("operational_events_v1"), Place::Value).unwrap();
    let metrics = world.slots.add(text("metric_state"), Place::Field).unwrap();
    world.slots.at(value).unwrap().try_borrow_mut().unwrap().row = Some(Box::new(ValueRow(shuffled_value(0))));
    world.slots.at(metrics).unwrap().try_borrow_mut().unwrap().row = Some(Box::new(id_map(|_| 0.5_f64)));
    world
}

fn agent_order(world: &mut World) -> Vec<String> {
    // Read shared: a read does not copy a part a fork shares.
    world.entities.try_borrow().unwrap().agents.key_order().iter().map(|key| key.to_string()).collect()
}

#[test]
fn a_snapshot_keeps_the_world_and_restore_gives_it_back() {
    let mut world = small_world();
    let slots = slot_state(&mut world);
    let snapshot = world.snapshot();
    world.canonicalize();
    assert_eq!(agent_order(&mut world), ["a", "b"]);
    // Every slot has a new stamp and the same bytes; the snapshot kept the old order.
    for (old, new) in slots.iter().zip(slot_state(&mut world)) {
        assert_eq!(old.1, new.1);
        assert_ne!(old.3, new.3);
    }
    let kept = snapshot.slot(0).unwrap().row.as_deref().unwrap();
    assert_eq!(value_keys(&kept.as_any().downcast_ref::<ValueRow>().unwrap().0), SHUFFLED);
    assert_eq!(snapshot.entities().agents.key_order().iter().map(|key| &**key).collect::<Vec<_>>(), ["b", "a"]);
    // Writes of every part, and a slot registered after the snapshot.
    world.scalars.try_borrow_mut().unwrap().minute_index = Fit::Ok(6);
    world.entities.try_borrow_mut().unwrap().agents.get_mut("a").unwrap().stress = 0.75;
    world.entities.get_mut().agents.insert(text("c"), agent("c"));
    world.slots.row_mut::<IdMap<f64>>("metric_state").unwrap().insert(text("new"), 1.0);
    let added = world.slots.add(text("later"), Place::Value).unwrap();
    world.slots.at(added).unwrap().try_borrow_mut().unwrap().row = Some(Box::new(ValueRow(V::Int(1))));
    assert_eq!(snapshot.scalars().minute_index, Fit::Ok(5));
    assert_eq!(snapshot.entities().agents.get("a").unwrap().stress, 0.25);
    world.restore(snapshot);
    assert_eq!(world.minute_index(), Ok(5));
    assert_eq!(agent_order(&mut world), ["b", "a"]);
    assert_eq!(world.entities.get_mut().agents.get("a").unwrap().stress, 0.25);
    assert!(!world.entities.get_mut().agents.contains_key("c"));
    // The slots are as they were, stamps included; the later slot has no state.
    let restored = slot_state(&mut world);
    assert_eq!(restored[..2], slots[..]);
    assert_eq!(restored[2].1, None);
    // The rows' document bytes are to be written again.
    let rows = world.entities.get_mut();
    assert!(rows.agents.slots().all(|(_, slot)| rows.agents.changed(slot)));
}

#[test]
fn a_fork_shares_every_part_until_either_world_writes_it() {
    let mut world = small_world();
    let slots = slot_state(&mut world);
    let mut fork = world.fork().unwrap();
    // The same state, order and stamps, and nothing copied yet.
    assert_eq!(slot_state(&mut fork), slots);
    assert_eq!(agent_order(&mut fork), ["b", "a"]);
    assert_eq!(fork.minute_index(), Ok(5));
    assert!(!fork.in_transaction());
    assert!(world.scalars.is_shared() && world.entities.is_shared());
    // A write to the fork copies what it writes; the world keeps its own.
    fork.entities.try_borrow_mut().unwrap().agents.get_mut("a").unwrap().stress = 0.75;
    fork.entities.get_mut().agents.insert(text("c"), agent("c"));
    fork.scalars.try_borrow_mut().unwrap().minute_index = Fit::Ok(6);
    fork.slots.row_mut::<IdMap<f64>>("metric_state").unwrap().insert(text("new"), 1.0);
    let added = fork.slots.add(text("later"), Place::Value).unwrap();
    fork.slots.at(added).unwrap().try_borrow_mut().unwrap().row = Some(Box::new(ValueRow(V::Int(1))));
    assert_eq!(world.minute_index(), Ok(5));
    assert_eq!(world.entities.get_mut().agents.get("a").unwrap().stress, 0.25);
    assert_eq!(agent_order(&mut world), ["b", "a"]);
    assert_eq!(slot_state(&mut world), slots);
    assert_eq!(world.slots.len(), 2);
    // And the other way: the world's writes don't reach the fork.
    world.canonicalize();
    world.slots.row_mut::<ValueRow>("operational_events_v1").unwrap().0 = V::Null;
    assert_eq!(agent_order(&mut fork), ["b", "a", "c"]);
    assert_eq!(slot_state(&mut fork)[0], slots[0]);
    // A part borrowed mutably can't be forked.
    let _writing = world.entities.try_borrow_mut().unwrap();
    assert!(world.fork().is_err());
}

/// A host that writes down what it was asked, and fails where it is told to.
#[derive(Default)]
struct Recorder {
    calls: Vec<String>,
    fail: Option<&'static str>,
}

impl Recorder {
    fn call(&mut self, name: String) -> Result<(), Error> {
        let failing = self.fail == Some(name.as_str());
        self.calls.push(name);
        if failing {
            return Err(Error::Value("refused".to_owned()));
        }
        Ok(())
    }
}

impl Host for Recorder {
    type Error = Error;

    fn validate_candidate(&mut self, kind: TxnKind, stage: Stage) -> Result<(), Error> {
        self.call(format!("validate {kind:?} {stage:?}"))
    }

    fn canonicalized(&mut self, _kind: TxnKind) -> Result<(), Error> {
        self.call("canonicalized".to_owned())
    }

    fn restored(&mut self, _kind: TxnKind) -> Result<(), Error> {
        self.call("restored".to_owned())
    }

    fn committed(&mut self, _kind: TxnKind) -> Result<(), Error> {
        self.call("committed".to_owned())
    }
}

fn order_in(cell: &Shard<World>) -> Vec<String> {
    cell.with(agent_order).unwrap()
}

#[test]
fn a_transaction_validates_twice_and_commits_or_rolls_back() {
    let cell = Shard::new(small_world());
    let mut host = Recorder::default();
    let kept = Txn::run(&cell, &mut host, TxnKind::Full, |cell, host| {
        assert_eq!(host.calls, ["validate Full World", "canonicalized", "validate Full Candidate"]);
        assert_eq!(order_in(cell), ["a", "b"]);
        cell.with(|world| world.entities.get_mut().agents.get_mut("a").unwrap().stress = 0.9)?;
        Ok(TxnEnd::Commit(7))
    });
    assert_eq!(kept, Ok(7));
    assert_eq!(host.calls.last().map(String::as_str), Some("committed"));
    assert_eq!(cell.with(|world| world.entities.get_mut().agents.get("a").unwrap().stress), Ok(0.9));
    assert_eq!(cell.with(|world| world.in_transaction()), Ok(false));

    // A rollback, asked for or by an error, gives the world back in its order then.
    let fresh = Shard::new(small_world());
    let before = fresh.with(slot_state).unwrap();
    let mut host = Recorder::default();
    let back = Txn::run(&fresh, &mut host, TxnKind::Effect, |cell, _| {
        cell.with(|world| world.entities.get_mut().agents.remove_row("a"))?;
        Ok(TxnEnd::Rollback("no"))
    });
    assert_eq!(back, Ok("no"));
    assert_eq!(host.calls.last().map(String::as_str), Some("restored"));
    assert_eq!(order_in(&fresh), ["b", "a"]);
    assert_eq!(fresh.with(slot_state).unwrap(), before);
    let failed: Result<(), Error> = Txn::run(&fresh, &mut host, TxnKind::Effect, |cell, _| {
        cell.with(|world| world.scalars.get_mut().minute_index = Fit::Ok(99))?;
        Err(Error::Value("the body failed".to_owned()))
    });
    assert_eq!(failed, Err(Error::Value("the body failed".to_owned())));
    assert_eq!(fresh.with(|world| world.minute_index()).unwrap(), Ok(5));
    assert_eq!(fresh.with(slot_state).unwrap(), before);
}

#[test]
fn a_failed_start_leaves_the_world_and_a_transaction_does_not_nest() {
    let cell = Shard::new(small_world());
    let before = cell.with(slot_state).unwrap();
    // The world's own validation fails: nothing was done.
    let mut host = Recorder { fail: Some("validate Effect World"), ..Recorder::default() };
    assert!(Txn::begin(&cell, &mut host, TxnKind::Effect).is_err());
    assert_eq!(host.calls, ["validate Effect World"]);
    assert_eq!(cell.with(slot_state).unwrap(), before);
    // The candidate fails: the snapshot came back before the error.
    let mut host = Recorder { fail: Some("validate Effect Candidate"), ..Recorder::default() };
    assert!(Txn::begin(&cell, &mut host, TxnKind::Effect).is_err());
    assert_eq!(host.calls.last().map(String::as_str), Some("restored"));
    assert_eq!(order_in(&cell), ["b", "a"]);
    assert_eq!(cell.with(slot_state).unwrap(), before);
    assert_eq!(cell.with(|world| world.in_transaction()), Ok(false));
    // The views fail to follow: the same.
    let mut host = Recorder { fail: Some("canonicalized"), ..Recorder::default() };
    assert!(Txn::begin(&cell, &mut host, TxnKind::Full).is_err());
    assert_eq!(cell.with(slot_state).unwrap(), before);
    // One open transaction at a time.
    let mut host = Recorder::default();
    let open = Txn::begin(&cell, &mut host, TxnKind::Full).ok().unwrap();
    assert_eq!(Txn::begin(&cell, &mut host, TxnKind::Full).err(), Some(Error::Runtime(NESTED.to_owned())));
    let snapshot: &Snapshot = open.snapshot();
    assert_eq!(snapshot.slot_count(), 2);
    open.rollback(&cell, &mut host).unwrap();
    assert_eq!(cell.with(slot_state).unwrap(), before);
}

#[test]
fn entities_copy_and_sort_as_a_whole() {
    let mut rows = Entities::default();
    put(&mut rows.agents, "b", agent("b"));
    put(&mut rows.agents, "a", agent("a"));
    let copy = rows.clone();
    rows.canonicalize();
    assert_eq!(rows.agents.key_order().iter().map(|key| &**key).collect::<Vec<_>>(), ["a", "b"]);
    assert_eq!(copy.agents.key_order().iter().map(|key| &**key).collect::<Vec<_>>(), ["b", "a"]);
    assert!(!rows.has_changes());
}
