//! The resource ledger block of the whole world's validation (`sheltersim_core::validate`, #250,
//! #351): the identity history of `population_dynamics_v1`, `validate_resource_ledger` (the whole
//! audit trail replayed) and `validate_resource_ledger_state` (the tick's check) give Python's class
//! and text for each broken ledger of `validate_resource_ledger.json`
//! (`tests/support/core_validate_resource_ledger.py`), and pass the valid ones.

mod support;

use support::validate;

#[test]
fn a_broken_ledger_gives_python_s_first_error_at_each_entry_point() {
    assert!(validate::check_file("validate_resource_ledger") >= 90);
}
