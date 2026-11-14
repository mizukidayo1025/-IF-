//! The operational events and the infrastructure of the whole world's validation
//! (`sheltersim_core::validate`, #250, #348): `validate_operational_events_state` of
//! `domain/operational.py` and `validate_infrastructure_runtime_state` of
//! `domain/infrastructure.py`. Each broken world of
//! `validate_operations.json` (`tests/support/core_validate_operations.py`) gives Python's class
//! and text at each entry point, with an event in flight too, and the accepted ones pass.

mod support;

use support::validate;

#[test]
fn each_broken_world_gives_python_s_first_operational_or_infrastructure_error() {
    assert!(validate::check_file("validate_operations") >= 100);
}
