//! The core `World` of lane 5-1: `Shard`'s borrow rules, `Slots`, and stamps.

use std::collections::HashSet;
use std::sync::Arc;

use sheltersim_core::cell::{Shard, ShardMut, ShardRef, BORROWED};
use sheltersim_core::entities::Entities;
use sheltersim_core::error::Error;
use sheltersim_core::row::ValueRow;
use sheltersim_core::stamp::Stamp;
use sheltersim_core::value::V;
use sheltersim_core::world::{Place, Slot, Slots, World};

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
