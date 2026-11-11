//! The per-person compute of a tick on more than one thread lives in `sheltersim_core::parallel`;
//! this file adds its Python entries for the tests.

/// For the tests and the gate: one helper for each `people` people from now on (at least 1), and
/// the number before.
#[pyo3::pyfunction]
pub(crate) fn set_people_per_helper(people: usize) -> usize {
    sheltersim_core::parallel::set_people_per_helper(people)
}

/// For the tests: `(helper threads, helpers for this many people)`.
#[pyo3::pyfunction]
pub(crate) fn parallel_state(people: usize) -> (usize, usize) {
    sheltersim_core::parallel::parallel_state(people)
}
