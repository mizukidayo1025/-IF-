//! The rules of `WorldState.validate` that #347 moves into the core (#250): each broken world of
//! `tests/support/core_validate_world.py` gives Python's first error, with its class and text, at
//! each entry point (`sheltersim_core::validate`).

mod support;

use support::validate::check_file;

#[test]
fn the_selected_models_and_the_declared_states_give_python_s_errors() {
    assert!(check_file("validate_selection") > 50);
}

#[test]
fn the_resource_service_gives_python_s_errors() {
    assert!(check_file("validate_service") > 40);
}

#[test]
fn the_information_spread_and_the_distress_give_python_s_errors() {
    assert!(check_file("validate_information") > 50);
}

#[test]
fn the_event_rules_give_python_s_errors() {
    assert!(check_file("validate_events") > 10);
}
