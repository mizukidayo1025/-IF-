//! The validators of `movement_v1`, `geometry_overlay_v1`, `facility_queues_v1`,
//! `temporary_spaces_v1` and `shared_operations_v1` without Python (`sheltersim_core::validate`,
//! #250, #350): each broken world of `validate_places.json` and `validate_shared_operations.json`
//! (`tests/support/core_validate_places.py`) gives Python's first error, class and text, at each
//! entry point, and each valid one passes.

mod support;

use support::validate;

#[test]
fn movement_overlay_queues_and_spaces_give_python_s_errors() {
    assert!(validate::check_file("validate_places") >= 200);
}

#[test]
fn shared_operations_and_their_history_give_python_s_errors() {
    assert!(validate::check_file("validate_shared_operations") >= 50);
}

// ---- The bounds, on worlds too large for a fixture --------------------------------------------
//
// A state past one of the bounds is too large to record (100,001 queue requests, 10,001 bed
// reservations, ...), so these change a case's loaded world here and expect the rule's text,
// which the cases above record from Python for the same rule.

use std::sync::Arc;

use indexmap::IndexMap;
use sheltersim_core::row::ValueRow;
use sheltersim_core::validate::{self as core_validate, py_str, Options};
use sheltersim_core::value::V;
use sheltersim_core::world::World;

fn change(world: &World, name: &str, edit: impl FnOnce(&mut IndexMap<Arc<str>, V>)) {
    let mut row = world.row_mut::<ValueRow>(name).unwrap_or_else(|error| panic!("{name} is no value: {error}"));
    let V::Map(map) = &mut row.0 else { panic!("{name} is not a dict") };
    edit(map);
}

fn raised(world: &World, history: bool) -> Option<(String, String)> {
    core_validate::extensions(world, history).err().map(|error| (validate::class_of(&error), py_str(&error)))
}

fn value_error(text: &str) -> Option<(String, String)> {
    Some(("ValueError".to_owned(), text.to_owned()))
}

fn copies(count: usize, item: &V) -> V {
    V::Map((0..count).map(|index| (Arc::from(format!("k{index}").as_str()), item.clone())).collect())
}

#[test]
fn temporary_spaces_past_their_bounds() {
    let world = validate::world_of("validate_places", "spaces_schema_version_true");
    change(&world, "temporary_spaces_v1", |state| {
        state.insert(Arc::from("spaces"), copies(257, &V::Int(0)));
    });
    assert_eq!(raised(&world, true), value_error("temporary_spaces_v1 has an invalid exact schema"));
    let world = validate::world_of("validate_places", "spaces_schema_version_true");
    change(&world, "temporary_spaces_v1", |state| {
        state.insert(Arc::from("history"), V::List(vec![V::Int(0); 20_001]));
    });
    assert_eq!(raised(&world, true), value_error("temporary_spaces_v1 has an invalid exact schema"));
}

#[test]
fn facility_queues_past_their_bound() {
    let world = validate::world_of("validate_places", "queues_schema_version_true");
    change(&world, "facility_queues_v1", |state| {
        state.insert(Arc::from("requests"), copies(100_001, &V::Int(0)));
    });
    assert_eq!(raised(&world, true), value_error("facility_queues_v1 has an invalid exact schema"));
}

#[test]
fn the_overlay_past_its_bounds() {
    let reservation = |map: &IndexMap<Arc<str>, V>| map["bed_reservations"].clone();
    let world = validate::world_of("validate_places", "overlay_valid_reservation");
    change(&world, "geometry_overlay_v1", |state| {
        let V::Map(one) = reservation(state) else { panic!() };
        let row = one.values().next().unwrap().clone();
        state.insert(Arc::from("bed_reservations"), copies(10_001, &row));
    });
    assert_eq!(raised(&world, true), value_error("geometry_overlay_v1 exceeds its bounded bed reservation limit"));
    let world = validate::world_of("validate_places", "overlay_valid_admission");
    change(&world, "geometry_overlay_v1", |state| {
        let V::Map(one) = state["room_admissions"].clone() else { panic!() };
        let row = one.values().next().unwrap().clone();
        state.insert(Arc::from("room_admissions"), copies(10_001, &row));
    });
    assert_eq!(raised(&world, true), value_error("geometry_overlay_v1 exceeds its bounded room admission limit"));
    let world = validate::world_of("validate_places", "overlay_valid_route_modifier");
    change(&world, "geometry_overlay_v1", |state| {
        let V::Map(routes) = &mut state[&Arc::from("route_modifiers")] else { panic!() };
        let V::Map(route) = routes.values_mut().next().unwrap() else { panic!() };
        let V::Map(modifiers) = route["modifiers"].clone() else { panic!() };
        let record = modifiers.values().next().unwrap().clone();
        route.insert(Arc::from("modifiers"), copies(257, &record));
    });
    assert_eq!(raised(&world, true), value_error("geometry_overlay_v1 exceeds its bounded override limit"));
}

#[test]
fn shared_operations_past_their_history_bound() {
    // A tick reads the records since the previous tick, which validate; the whole history is past
    // its bound. The whole replay meets the model's own bound first.
    let world = validate::world_of("validate_shared_operations", "shared_truncated_text");
    change(&world, "shared_operations_v1", |state| {
        let V::List(history) = &mut state[&Arc::from("history")] else { panic!() };
        let first = history[0].clone();
        let mut longer = vec![first; 20_000];
        longer.append(history);
        *history = longer;
    });
    assert_eq!(raised(&world, false), value_error("shared_operations_v1 history is over limit or out of time order"));
    assert_eq!(raised(&world, true), value_error("shared_operations_v1 has an invalid exact schema"));
    let tick = core_validate::world(&world, &Options { history: false, in_flight: None });
    assert_eq!(tick.err().map(|error| py_str(&error)).as_deref(), Some("invalid simulator extension state"));
}
