//! The extension namespaces lane D of #250 ports (#349): agent_dynamics_v1, body_state_v1 (with its
//! history), staff_dynamics_v1, staff_operations_v1, social_network_v1, population_dynamics_v1 and
//! population_lifecycle_v1. Each broken world of `validate_namespaces.json` and
//! `validate_staff_tasks.json` (`tests/support/core_validate_namespaces.py`) gives Python's class
//! and text at each entry point: raw from `validate_simulator_extensions`, wrapped as "invalid
//! simulator extension state" by `WorldState.validate`.

mod support;

use support::validate;

#[test]
fn a_broken_namespace_gives_python_s_first_error() {
    assert!(validate::check_file("validate_namespaces") >= 180);
}

#[test]
fn an_ordered_staff_task_gives_python_s_first_error() {
    assert!(validate::check_file("validate_staff_tasks") >= 15);
}
