//! The configs of lane 7-1: `Derived` keeps two stamps, a section is parsed once, and a stamp is
//! never issued twice, so a rollback can't give a stale hit (design-p7.md §2–§4, §8).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use sheltersim_core::config::{Configs, Derived, Fault, FaultClass, Lazy, Section, Selection};
use sheltersim_core::stamp::Stamp;
use sheltersim_core::world::{Place, Slot, World};

#[test]
fn the_configs_of_a_stamp_are_kept_until_two_newer_stamps_come() {
    let mut derived = Derived::default();
    let (first, second, third) = (Stamp::next(), Stamp::next(), Stamp::next());
    let a = derived.configs(first);
    assert!(Arc::ptr_eq(&a, &derived.configs(first)));
    assert_eq!(a.stamp, Some(first));
    let b = derived.configs(second);
    assert!(!Arc::ptr_eq(&a, &b));
    assert_eq!(derived.stamps(), [Some(second), Some(first)]);
    // A rollback brings the older stamp back: it is still held, and becomes the latest.
    assert!(Arc::ptr_eq(&a, &derived.configs(first)));
    assert_eq!(derived.stamps(), [Some(first), Some(second)]);
    // A third stamp drops the one used least recently.
    let c = derived.configs(third);
    assert_eq!(derived.stamps(), [Some(third), Some(first)]);
    assert!(!Arc::ptr_eq(&c, &b));
    assert!(!Arc::ptr_eq(&b, &derived.configs(second)));
}

#[test]
fn a_section_is_parsed_at_its_first_read_and_kept() {
    let configs = Configs::new(None);
    let parses = AtomicUsize::new(0);
    let parse = || {
        parses.fetch_add(1, Ordering::Relaxed);
        Section::Ok(Arc::new(Selection::new(vec![(Arc::from("queue"), Arc::from("strict-fifo-queue-v1"))])))
    };
    assert!(configs.selection.get().is_none());
    for _ in 0..3 {
        let Section::Ok(selection) = configs.selection.get_or_parse(parse) else { panic!("not parsed") };
        assert_eq!(selection.get("queue").map(|model| &**model), Some("strict-fifo-queue-v1"));
        assert_eq!(selection.get("information"), None);
    }
    assert_eq!(parses.load(Ordering::Relaxed), 1);
}

#[test]
fn a_fault_is_kept_with_its_class_and_text() {
    let lazy: Lazy<Selection> = Lazy::default();
    let fault = Fault::new(FaultClass::Value, "behavior temperature must be finite and positive");
    let kept = lazy.get_or_parse(|| Section::Fault(fault.clone()));
    let Section::Fault(kept) = kept else { panic!("not a fault") };
    assert_eq!(kept, &fault);
    assert!(!kept.is_refusal());
    assert!(Fault::refused("not a selection").is_refusal());
    // Read again, the fault is what was kept: no second parse.
    let Section::Fault(again) = lazy.get_or_parse(|| panic!("parsed twice")) else { panic!("not a fault") };
    assert_eq!(again.text.as_ref(), "behavior temperature must be finite and positive");
}

#[test]
fn a_parse_inside_a_parse_keeps_the_section_of_the_first_to_finish() {
    let lazy: Lazy<Selection> = Lazy::default();
    let outer = lazy.get_or_parse(|| {
        let inner = lazy.get_or_parse(|| Section::Fault(Fault::refused("inner")));
        assert!(matches!(inner, Section::Fault(fault) if &*fault.text == "inner"));
        Section::Fault(Fault::refused("outer"))
    });
    assert!(matches!(outer, Section::Fault(fault) if &*fault.text == "inner"));
}

#[test]
fn a_selection_keeps_the_order_of_the_dict() {
    let selection = Selection::new(vec![
        (Arc::from("psychology"), Arc::from("causal-stress-v1")),
        (Arc::from("behavior"), Arc::from("utility-softmax-v1")),
    ]);
    let domains: Vec<&str> = selection.iter().map(|(domain, _)| domain).collect();
    assert_eq!(domains, ["psychology", "behavior"]);
    assert_eq!(selection.len(), 2);
}

#[test]
fn after_a_rollback_and_a_new_write_the_configs_are_parsed_again() {
    // What a snapshot and a rollback of the configs slot will do (lane 6-2): the old stamp comes
    // back, and the next write gets a stamp no state had before.
    let mut slot = Slot::new(Place::Field);
    let mut derived = Derived::default();
    let before = slot.generation;
    let parsed = derived.configs(before);
    slot.touch();
    let written = slot.generation;
    derived.configs(written);
    slot.generation = before;
    assert!(Arc::ptr_eq(&parsed, &derived.configs(slot.generation)));
    slot.touch();
    assert!(slot.generation != before && slot.generation != written);
    assert!(!Arc::ptr_eq(&parsed, &derived.configs(slot.generation)));
}

#[test]
fn a_world_starts_with_nothing_derived() {
    let mut world = World::default();
    assert_eq!(world.derived.get_mut().stamps(), [None, None]);
    assert!(world.derived.try_borrow_mut().is_ok());
}
