//! The core `World` of lanes 5-1 and 5-3: `Shard`'s borrow rules, `Slots`, value slots and stamps;
//! of lane 5-2: the scalars (`Fit`, `Seed`) and the rows of `resources` and `id_counters`; and of
//! lane 5-4: the typed accessors of `World`; of lane P8-0c: the marks of the slots a kernel of the
//! core writes.

use std::collections::HashSet;
use std::sync::Arc;

use sheltersim_core::cell::{Shard, ShardMut, ShardRef, BORROWED};
use sheltersim_core::entities::Entities;
use sheltersim_core::error::Error;
use sheltersim_core::row::ValueRow;
use sheltersim_core::stamp::Stamp;
use sheltersim_core::value::V;
use sheltersim_core::row::IdMap;
use sheltersim_core::world::{
    id_counter, set_id_counter, Fit, Place, ResourcesRow, Scalars, Seed, Slot, Slots, World, ID_COUNTERS, RESOURCES,
    SCALAR_NAMES,
};

fn borrowed() -> Error {
    Error::Runtime(BORROWED.to_owned())
}

#[test]
fn a_part_borrowed_mutably_twice_is_an_error_not_a_panic() {
    let shard = Shard::new(1_i64);
    let first = shard.try_borrow_mut().unwrap();
    assert_eq!(shard.try_borrow_mut().err(), Some(borrowed()));
    assert_eq!(shard.try_borrow().err(), Some(borrowed()));
    assert!(shard.is_borrowed());
    drop(first);
    assert!(!shard.is_borrowed());
    *shard.try_borrow_mut().unwrap() += 1;
    assert_eq!(*shard.try_borrow().unwrap(), 2);
}

#[test]
fn shared_borrows_stack_and_keep_a_writer_out() {
    let shard = Shard::new(String::from("x"));
    let one = shard.try_borrow().unwrap();
    let two = shard.try_borrow().unwrap();
    assert_eq!((one.as_str(), two.as_str()), ("x", "x"));
    assert_eq!(shard.try_borrow_mut().err(), Some(borrowed()));
    drop(one);
    assert_eq!(shard.try_borrow_mut().err(), Some(borrowed()));
    drop(two);
    shard.try_borrow_mut().unwrap().push('y');
    assert_eq!(&*shard.try_borrow().unwrap(), "xy");
}

#[test]
fn a_mapped_borrow_holds_the_part_until_it_ends() {
    let shard = Shard::new((1_i64, 2_i64));
    let mut second = ShardMut::map(shard.try_borrow_mut().unwrap(), |pair| &mut pair.1);
    *second = 5;
    assert!(shard.try_borrow().is_err());
    drop(second);
    let first = ShardRef::map(shard.try_borrow().unwrap(), |pair| &pair.0);
    assert!(shard.try_borrow_mut().is_err());
    assert_eq!(*first, 1);
    drop(first);
    // A failed try_map releases the borrow.
    let failed = ShardMut::try_map(shard.try_borrow_mut().unwrap(), |_| Err::<&mut i64, _>(Error::unsupported("no")));
    assert!(failed.is_err());
    assert_eq!(shard.try_borrow().unwrap().1, 5);
}

#[test]
fn two_parts_borrow_apart() {
    let mut world = World::default();
    let a = world.slots.add(Arc::from("a"), Place::Namespace).unwrap();
    let b = world.slots.add(Arc::from("b"), Place::Field).unwrap();
    let entities = world.entities.try_borrow_mut().unwrap();
    let slot_a = world.slots.at(a).unwrap().try_borrow_mut().unwrap();
    let slot_b = world.slots.at(b).unwrap().try_borrow_mut().unwrap();
    assert_eq!(world.slots.at(a).unwrap().try_borrow_mut().err(), Some(borrowed()));
    assert_eq!(world.entities.try_borrow().err(), Some(borrowed()));
    assert_eq!((slot_a.place, slot_b.place), (Place::Namespace, Place::Field));
    drop((entities, slot_a, slot_b));
    // The whole world reaches every part without a flag.
    assert!(world.entities.get_mut().tainted().is_none());
}

#[test]
fn slots_keep_their_index_and_refuse_a_second_owner() {
    let mut slots = Slots::default();
    assert_eq!(slots.add(Arc::from("metric_state"), Place::Field).unwrap(), 0);
    assert_eq!(slots.add(Arc::from("body_state_v1"), Place::Namespace).unwrap(), 1);
    assert_eq!(
        slots.add(Arc::from("metric_state"), Place::Field).err(),
        Some(Error::Value("metric_state is owned already".to_owned()))
    );
    assert_eq!(slots.len(), 2);
    assert_eq!(slots.index_of("body_state_v1"), Some(1));
    assert_eq!(slots.index_of("other"), None);
    assert!(slots.get("other").is_none());
    let names: Vec<&str> = slots.names().map(|name| &**name).collect();
    assert_eq!(names, ["metric_state", "body_state_v1"]);
    assert!(std::ptr::eq(slots.at(1).unwrap(), slots.get("body_state_v1").unwrap()));
}

#[test]
fn a_row_is_read_as_its_type_or_refused() {
    let mut slots = Slots::default();
    slots.add(Arc::from("population"), Place::Namespace).unwrap();
    assert_eq!(
        slots.try_row_mut::<ValueRow>("population").err(),
        Some(Error::Unsupported("the namespace has no row".into()))
    );
    assert_eq!(
        slots.try_row::<ValueRow>("other").err(),
        Some(Error::Unsupported("the namespace is not owned".into()))
    );
    slots.get("population").unwrap().try_borrow_mut().unwrap().row = Some(Box::new(ValueRow(V::Int(3))));
    {
        let row = slots.try_row::<ValueRow>("population").unwrap();
        assert_eq!(row.0, V::Int(3));
        // The slot stays borrowed through the row.
        assert!(slots.get("population").unwrap().try_borrow_mut().is_err());
    }
    slots.try_row_mut::<ValueRow>("population").unwrap().0 = V::Bool(true);
    assert_eq!(slots.row_mut::<ValueRow>("population").unwrap().0, V::Bool(true));
    // Another type: refused, and the slot is free again.
    assert_eq!(
        slots.try_row_mut::<f64>("population").err(),
        Some(Error::Unsupported("the namespace has another row".into()))
    );
    assert!(!slots.get("population").unwrap().is_borrowed());
}

#[test]
fn a_misfit_drops_the_row_and_a_touch_gives_a_new_stamp() {
    let mut slot = Slot::new(Place::Namespace);
    slot.row = Some(Box::new(ValueRow(V::Null)));
    let before = slot.generation;
    slot.touch();
    assert_ne!(slot.generation, before);
    slot.misfit("not a dict".to_owned());
    assert!(slot.row.is_none());
    assert_eq!(slot.taint.as_deref(), Some("not a dict"));
}

#[test]
fn entities_keep_the_first_taint_and_drop_their_rows() {
    let mut entities = Entities::default();
    entities.taint("first".to_owned());
    entities.taint("second".to_owned());
    assert_eq!(entities.tainted(), Some("first"));
    assert_eq!(entities.agents.len(), 0);
    entities.clear();
    assert_eq!(entities.tainted(), None);
}

#[test]
fn stamps_are_unique_across_slots_and_threads() {
    let mut seen = HashSet::new();
    let slots: Vec<Slot> = (0..100).map(|_| Slot::new(Place::Namespace)).collect();
    for slot in &slots {
        assert!(seen.insert(slot.generation));
    }
    let threads: Vec<_> = (0..4)
        .map(|_| std::thread::spawn(|| (0..1000).map(|_| Stamp::next()).collect::<Vec<_>>()))
        .collect();
    for thread in threads {
        for stamp in thread.join().unwrap() {
            assert!(seen.insert(stamp), "a stamp came twice");
            assert_ne!(stamp.get(), 0);
        }
    }
}

#[test]
fn a_value_slot_reads_as_its_value_absent_or_refused() {
    let mut slots = Slots::default();
    slots.add(Arc::from("operational_events_v1"), Place::Value).unwrap();
    slots.add(Arc::from("metric_state"), Place::Field).unwrap();
    // A name with no slot and a slot with no row are both absent.
    assert_eq!(slots.value("other").unwrap(), None);
    assert_eq!(slots.value("operational_events_v1").unwrap(), None);
    assert!(slots.try_value("operational_events_v1").unwrap().is_none());
    let mut map = indexmap::IndexMap::new();
    map.insert(Arc::from("b"), V::Int(1));
    map.insert(Arc::from("a"), V::Float(1.0));
    let slot = slots.get("operational_events_v1").unwrap();
    slot.try_borrow_mut().unwrap().row = Some(Box::new(ValueRow(V::Map(map.clone()))));
    {
        let value = slots.try_value("operational_events_v1").unwrap().unwrap();
        assert_eq!(*value, V::Map(map.clone()));
        let V::Map(read) = &*value else { unreachable!() };
        assert_eq!(read.keys().map(|key| &**key).collect::<Vec<_>>(), ["b", "a"]);
        assert!(slots.get("operational_events_v1").unwrap().try_borrow_mut().is_err());
    }
    assert_eq!(slots.value("operational_events_v1").unwrap(), Some(&V::Map(map)));
    // A tainted key is refused, and so is a slot whose row is not a value.
    slots.get("operational_events_v1").unwrap().try_borrow_mut().unwrap().misfit("value is not a plain JSON value".to_owned());
    assert_eq!(slots.value("operational_events_v1").err(), Some(Error::Unsupported("the namespace has no row".into())));
    assert!(slots.try_value("operational_events_v1").is_err());
    slots.get("metric_state").unwrap().try_borrow_mut().unwrap().row = Some(Box::new(0.5_f64));
    assert_eq!(slots.value("metric_state").err(), Some(Error::Unsupported("the namespace has another row".into())));
    assert!(!slots.get("metric_state").unwrap().is_borrowed());
}

#[test]
fn heap_bytes_count_what_a_value_allocates() {
    assert_eq!(V::Int(3).heap_bytes(), 0);
    assert_eq!(V::Str(Arc::from("abc")).heap_bytes(), 3 + 2 * std::mem::size_of::<usize>());
    let list = V::List(Vec::with_capacity(4));
    assert_eq!(list.heap_bytes(), 4 * std::mem::size_of::<V>());
    let mut map = indexmap::IndexMap::with_capacity(1);
    map.insert(Arc::from("k"), V::Str(Arc::from("vv")));
    let value = V::Map(map);
    let V::Map(inner) = &value else { unreachable!() };
    assert!(inner.capacity() >= 1);
    let words = 2 * std::mem::size_of::<usize>();
    let entry = std::mem::size_of::<(u64, Arc<str>, V)>() + std::mem::size_of::<usize>();
    assert_eq!(value.heap_bytes(), inner.capacity() * entry + (1 + words) + (2 + words));
}

// ---- Lane 5-2 ------------------------------------------------------------------------------

fn unsupported(reason: &str) -> Error {
    Error::Unsupported(reason.to_owned().into())
}

#[test]
fn a_seed_is_decimal_text_of_any_size() {
    let small = Seed::from_text("2026").unwrap();
    assert_eq!((&*small.text, small.small), ("2026", Some(2026)));
    let negative = Seed::from_text("-7").unwrap();
    assert_eq!(negative.small, Some(-7));
    // Past i64: the text alone, which `KeyedDraws` takes.
    let big = Seed::from_text("123456789012345678901234567890").unwrap();
    assert_eq!((&*big.text, big.small), ("123456789012345678901234567890", None));
    assert_eq!(Seed::from_text("9223372036854775807").unwrap().small, Some(i64::MAX));
    assert_eq!(Seed::from_text("9223372036854775808").unwrap().small, None);
    assert_eq!(Seed::from_text("0").unwrap().small, Some(0));
    for text in ["", "-", "-0", "007", "1.5", "+1", " 1", "1e3", "True", "١٢"] {
        assert_eq!(Seed::from_text(text), None, "{text:?}");
    }
}

#[test]
fn a_misfit_scalar_is_unsupported_for_a_kernel() {
    let scalars = Scalars::default();
    assert_eq!(scalars.minute_index().err(), Some(unsupported("the minute is not an int")));
    assert_eq!(scalars.duration_minutes().err(), Some(unsupported("the duration is not an int")));
    assert_eq!(scalars.schema_version().err(), Some(unsupported("the schema version is not an int")));
    assert_eq!(scalars.seed().err(), Some(unsupported("the seed is not an int")));
    assert_eq!(scalars.scenario_id().err(), Some(unsupported("the scenario ID is not a str")));
    let fit: Fit<i64> = Fit::Ok(3);
    assert_eq!((fit.ok(), fit.get("x")), (Some(&3), Ok(&3)));
    assert_eq!(Fit::<i64>::default(), Fit::Misfit);
    assert_eq!(SCALAR_NAMES, ["scenario_id", "seed", "duration_minutes", "minute_index", "schema_version"]);
}

#[test]
fn a_world_reads_its_scalars_through_their_part() {
    let world = World::default();
    {
        let mut scalars = world.scalars.try_borrow_mut().unwrap();
        scalars.minute_index = Fit::Ok(42);
        scalars.duration_minutes = Fit::Ok(10_080);
        scalars.schema_version = Fit::Ok(6);
        scalars.seed = Fit::Ok(Seed::from_text("99999999999999999999").unwrap());
        scalars.scenario_id = Fit::Ok(Arc::from("baseline"));
        // A kernel can't read them while a writer holds them.
        assert_eq!(world.minute_index().err(), Some(borrowed()));
    }
    assert_eq!(world.minute_index(), Ok(42));
    assert_eq!(world.duration_minutes(), Ok(10_080));
    assert_eq!(world.schema_version(), Ok(6));
    assert_eq!(&*world.seed_text().unwrap(), "99999999999999999999");
    assert_eq!(&**world.scalars().unwrap().scenario_id().unwrap(), "baseline");
    // Another part stays free.
    let _entities = world.entities.try_borrow_mut().unwrap();
    assert_eq!(world.minute_index(), Ok(42));
}

fn counters(entries: &[(&str, V)]) -> ValueRow {
    let mut map = indexmap::IndexMap::new();
    for (key, value) in entries {
        map.insert(Arc::from(*key), value.clone());
    }
    ValueRow(V::Map(map))
}

#[test]
fn an_id_counter_is_an_int_of_a_map_and_a_new_one_goes_to_the_end() {
    let mut row = counters(&[("agent", V::Int(100)), ("event", V::Int(7))]);
    assert_eq!(id_counter(&row, "event"), Ok(Some(7)));
    assert_eq!(id_counter(&row, "resource_task"), Ok(None));
    set_id_counter(&mut row, "event", 8).unwrap();
    set_id_counter(&mut row, "resource_task", 1).unwrap();
    let V::Map(map) = &row.0 else { unreachable!() };
    let keys: Vec<&str> = map.keys().map(|key| &**key).collect();
    assert_eq!(keys, ["agent", "event", "resource_task"]);
    assert_eq!(id_counter(&row, "event"), Ok(Some(8)));
    let text = counters(&[("event", V::Str(Arc::from("text")))]);
    assert_eq!(id_counter(&text, "event"), Err(unsupported("an ID counter is not an int")));
    let list = ValueRow(V::List(vec![]));
    assert_eq!(id_counter(&list, "event"), Err(unsupported("id_counters is not a map")));
}

#[test]
fn a_world_reads_resources_and_counters_from_their_slots() {
    let mut world = World::default();
    assert_eq!(world.resources().err(), Some(unsupported("the namespace is not owned")));
    let resources = world.slots.add(Arc::from(RESOURCES), Place::Field).unwrap();
    let ids = world.slots.add(Arc::from(ID_COUNTERS), Place::Field).unwrap();
    assert_eq!(world.resources().err(), Some(unsupported("the namespace has no row")));
    let mut stock: ResourcesRow = IdMap::default();
    stock.insert(Arc::from("water_ml"), 5_000);
    world.slots.at(resources).unwrap().try_borrow_mut().unwrap().row = Some(Box::new(stock));
    world.slots.at(ids).unwrap().try_borrow_mut().unwrap().row = Some(Box::new(counters(&[("event", V::Int(3))])));
    assert_eq!(world.resources().unwrap()["water_ml"], 5_000);
    assert_eq!(world.id_counter("event"), Ok(Some(3)));
    assert_eq!(world.id_counter("agent"), Ok(None));
}

#[test]
fn the_typed_accessors_borrow_their_part_and_say_what_is_wrong() {
    let mut world = World::default();
    let configs = world.slots.add(Arc::from(sheltersim_core::world::MODEL_CONFIGS), Place::Field).unwrap();
    assert_eq!(world.stamp_of("absent"), Ok(None));
    let stamp = world.stamp_of(sheltersim_core::world::MODEL_CONFIGS).unwrap().unwrap();
    // The configs of a stamp are kept until two newer stamps were asked for.
    let first = world.configs(stamp).unwrap();
    assert!(Arc::ptr_eq(&first, &world.configs(stamp).unwrap()));
    world.slots.at(configs).unwrap().try_borrow_mut().unwrap().touch();
    let next = world.stamp_of(sheltersim_core::world::MODEL_CONFIGS).unwrap().unwrap();
    assert_ne!(next, stamp);
    assert!(!Arc::ptr_eq(&first, &world.configs(next).unwrap()));
    // A slot borrowed elsewhere: its stamp can't be read now.
    let held = world.slots.at(configs).unwrap().try_borrow_mut().unwrap();
    assert_eq!(world.stamp_of(sheltersim_core::world::MODEL_CONFIGS), Err(borrowed()));
    drop(held);

    assert_eq!(world.metric_state().err(), Some(unsupported("the namespace is not owned")));
    let metrics = world.slots.add(Arc::from(sheltersim_core::world::METRIC_STATE), Place::Field).unwrap();
    assert_eq!(world.metric_state().err(), Some(unsupported("the namespace has no row")));
    world.slots.at(metrics).unwrap().try_borrow_mut().unwrap().row = Some(Box::new(ValueRow(V::Null)));
    assert_eq!(world.metric_state().err(), Some(unsupported("the namespace has another row")));
    let mut row: IdMap<f64> = IdMap::default();
    row.insert(Arc::from("toilet_wait_count"), 2.0);
    world.slots.at(metrics).unwrap().try_borrow_mut().unwrap().row = Some(Box::new(row));
    assert_eq!(world.metric_state().unwrap().get("toilet_wait_count"), Some(&2.0));
    assert_eq!(world.row::<ValueRow>("metric_state").err(), Some(unsupported("the namespace has another row")));
    {
        let mut row = world.row_mut::<IdMap<f64>>("metric_state").unwrap();
        row.insert(Arc::from("toilet_wait_max_minutes"), 1.0);
        assert_eq!(world.metric_state().err(), Some(borrowed()));
    }

    assert_eq!(world.rows().unwrap().agents.len(), 0);
    world.entities.get_mut().taint("why".to_owned());
    assert_eq!(world.rows().err(), Some(unsupported("the entity store is tainted")));
    assert_eq!(world.rows_mut().err(), Some(unsupported("the entity store is tainted")));
}

// ---- The marks of written slots (lane P8-0c) ------------------------------------------------------

fn value_world(names: &[&str]) -> World {
    let mut world = World::default();
    for name in names {
        let index = world.slots.add(Arc::from(*name), Place::Value).unwrap();
        world.slots.at(index).unwrap().try_borrow_mut().unwrap().row = Some(Box::new(ValueRow(V::Int(0))));
    }
    world
}

fn marked(world: &World) -> Vec<String> {
    world.slots.written_names().iter().map(|name| name.to_string()).collect()
}

#[test]
fn a_write_from_the_core_marks_its_slot_once_with_a_new_stamp() {
    let mut world = value_world(&["a", "b", "c"]);
    assert_eq!(world.slots.take_written(), Vec::<usize>::new());
    let before = world.stamp_of("c").unwrap().unwrap();
    // Reads mark nothing.
    assert_eq!(world.row::<ValueRow>("c").unwrap().0, V::Int(0));
    assert_eq!(world.slots.try_value("b").unwrap().as_deref(), Some(&V::Int(0)));
    assert_eq!(world.slots.value("a"), Ok(Some(&V::Int(0))));
    assert!(marked(&world).is_empty());
    // A write through a shared borrow, twice, and one through the whole world.
    world.row_mut::<ValueRow>("c").unwrap().0 = V::Int(1);
    world.slots.try_row_mut::<ValueRow>("c").unwrap().0 = V::Int(2);
    world.slots.row_mut::<ValueRow>("a").unwrap().0 = V::Int(3);
    assert_ne!(world.stamp_of("c").unwrap().unwrap(), before);
    assert_eq!(marked(&world), ["a", "c"]);
    // In index order, each once; then no more.
    assert_eq!(world.slots.take_written(), [0, 2]);
    assert!(marked(&world).is_empty());
    assert_eq!(world.slots.take_written(), Vec::<usize>::new());
    assert_eq!(world.row::<ValueRow>("c").unwrap().0, V::Int(2));
}

#[test]
fn a_refused_write_marks_nothing() {
    let mut world = value_world(&["a"]);
    let stamp = world.stamp_of("a").unwrap().unwrap();
    assert_eq!(world.row_mut::<IdMap<f64>>("a").err(), Some(unsupported("the namespace has another row")));
    assert_eq!(world.slots.row_mut::<IdMap<f64>>("a").err(), Some(unsupported("the namespace has another row")));
    assert_eq!(world.row_mut::<ValueRow>("absent").err(), Some(unsupported("the namespace is not owned")));
    world.slots.at(0).unwrap().try_borrow_mut().unwrap().misfit("why".to_owned());
    assert_eq!(world.row_mut::<ValueRow>("a").err(), Some(unsupported("the namespace has no row")));
    let held = world.slots.at(0).unwrap().try_borrow().unwrap();
    assert_eq!(world.row_mut::<ValueRow>("a").err(), Some(borrowed()));
    drop(held);
    assert_eq!(world.stamp_of("a").unwrap(), Some(stamp));
    assert_eq!(world.slots.take_written(), Vec::<usize>::new());
}

#[test]
fn canonical_order_restore_and_fork_mark_nothing_and_a_snapshot_keeps_its_slot_unmarked() {
    let mut world = value_world(&["a", "b"]);
    let snapshot = world.snapshot();
    world.row_mut::<ValueRow>("a").unwrap().0 = V::Int(5);
    // The write copied the slot the snapshot shares: the snapshot's is neither changed nor marked.
    assert!(!snapshot.slot(0).unwrap().written);
    assert_eq!(snapshot.slot(0).unwrap().row::<ValueRow>().unwrap().0, V::Int(0));
    assert_eq!(world.slots.take_written(), [0]);
    world.canonicalize();
    assert!(marked(&world).is_empty());
    let fork = world.fork().unwrap();
    assert!(marked(&fork).is_empty());
    world.restore(snapshot);
    assert!(marked(&world).is_empty());
    assert_eq!(world.row::<ValueRow>("a").unwrap().0, V::Int(0));
    assert_eq!(fork.row::<ValueRow>("a").unwrap().0, V::Int(5));
}

#[test]
fn kernels_on_threads_mark_the_slots_they_write() {
    let names: Vec<String> = (0..8).map(|index| format!("s{index}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut world = value_world(&refs);
    std::thread::scope(|scope| {
        for name in names.iter().skip(1).step_by(2) {
            let world = &world;
            scope.spawn(move || world.row_mut::<ValueRow>(name).unwrap().0 = V::Bool(true));
        }
    });
    assert_eq!(world.slots.take_written(), [1, 3, 5, 7]);
}
