//! The zone environment and EffectProgram parts of the whole world's validation
//! (`sheltersim_core::validate`, #250, #351): `validate_zone_environment_runtime_state` and
//! `validate_effect_program_state`, with and without the history, give Python's class and text for
//! each broken world of `validate_effects.json` (`tests/support/core_validate_effects.py`), the
//! pydantic contracts' texts included, and pass the valid ones.

mod support;

use support::validate;

#[test]
fn a_broken_effect_or_zone_state_gives_python_s_first_error_at_each_entry_point() {
    assert!(validate::check_file("validate_effects") >= 95);
}
