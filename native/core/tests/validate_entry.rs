//! The whole world's validation without Python (`sheltersim_core::validate`, #250, #347): the valid
//! worlds pass every entry point, and the entry points' own rules (the schema version, the
//! extensions' wrapping, the candidate's wrapping) raise Python's class and text for each broken
//! world of `validate_entry.json` (`tests/support/core_validate.py`).

mod support;

use support::validate;

#[test]
fn the_valid_worlds_pass_every_entry_point() {
    assert_eq!(validate::check_valid_worlds("validate_worlds"), 2);
}

#[test]
fn a_broken_world_gives_python_s_first_error_at_each_entry_point() {
    assert!(validate::check_file("validate_entry") >= 3);
}
