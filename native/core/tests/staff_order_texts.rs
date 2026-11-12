//! An operator's staff order through its EffectProgram's handler, and a worker who became
//! unavailable mid-task (Issue #225, lane M11; moved from
//! `tests/integration/test_staff_operation_texts.py`).
//!
//! The worlds are the gate's generated Scenario (20 evacuees, 120 minutes) at minute 0, as the
//! pytest file built them (`tests/support/core_fixtures_staff_work.py`, `staff_order_texts.json`).
//! `EffectProgramStructureHandler` has no Python rule left (`kernel_only`): the outcomes are the
//! class and text that rule gave, written down from it in the pytest file. The arrival and the
//! completion of a task whose worker became unavailable stopped the operation and then were
//! suppressed: the world the core leaves is the one Python's `interrupt_staff_operation` left on a
//! copy made before. Each case runs the core's handler (`dispatch::apply_alone`, the kernel the
//! handler's PyO3 entry calls) on a fresh load of its world (`support/cases.rs`).
//!
//! The checks of `order_staff_operation` itself stay in pytest: they compare the kernel with that
//! Python rule, which is still in `src`.

mod support;

use support::cases::check_group;

const FILE: &str = "staff_order_texts";

/// `test_an_order_the_handler_turns_down_is_python_s_text`: the handler's own checks of an order (a
/// modifier, a phase, a staff ID or a due minute that is not the program's) and its guard (false,
/// or a field with no facility target), before the order: the world stays as it was.
#[test]
fn an_order_the_handler_turns_down_is_python_s_text() {
    check_group(FILE, "turned_down", 6);
}

/// `test_an_order_that_went_stale_fails_in_the_handler_with_python_s_text`: an order that would
/// end past the Run when its event comes.
#[test]
fn an_order_that_went_stale_fails_in_the_handler_with_python_s_text() {
    check_group(FILE, "stale", 1);
}

/// `test_an_arrival_of_a_worker_who_became_unavailable_stops_the_task` and
/// `test_a_completion_of_a_worker_who_became_unavailable_stops_the_task`: the operation stops (the
/// worker idle, the task gone, and for the arrival its completion out of the queue), then the event
/// is suppressed with the Python rule's text.
#[test]
fn a_worker_who_became_unavailable_stops_the_task() {
    check_group(FILE, "unavailable", 2);
}
