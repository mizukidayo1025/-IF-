//! `CounterRNG` (`core/rng.py`) lives in `sheltersim_core::rng`; this file gives it to the lanes
//! under its old path and adds the draw log's Python entries.

use pyo3::prelude::*;

pub(crate) use sheltersim_core::rng::*;

/// Keep the draws this thread makes from now on, until `take_draw_log`.
#[pyfunction]
pub(crate) fn start_draw_log() {
    sheltersim_core::rng::start_draw_log();
}

/// The (key as JSON, value) of each draw this thread made since `start_draw_log`, in order.
#[pyfunction]
pub(crate) fn take_draw_log() -> Vec<(String, f64)> {
    sheltersim_core::rng::take_draw_log()
}
