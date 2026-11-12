//! What the rule transactions did, for the gate (lane 6-3 of Issue #152, design-p5-p6.md §6).
//!
//! A rule that changes the world and keeps the change only if the changed world passes its checks
//! used to copy the whole world through canonical JSON, change the copy, validate it and swap it in.
//! It now runs in a transaction of the world's core: since lane P9-1b, the core's `txn::rule`
//! opens it with the native host (`PyHost`), and a world whose Python views a transaction does not
//! give back is refused there (`Host::txn_check`, `_txn_check` of `core/state_mirror.py`) with
//! nothing changed: the handler's `KernelRefusal` with that reason. The copy it took before is gone
//! (policy E16: rule transactions are core-only).

use pyo3::prelude::*;
use pyo3::types::PyDict;

/// What the rule transactions did in this process: how many ran in place and inside another, how
/// many committed and rolled back (`sheltersim_core::txn::counts`).
#[pyfunction]
fn counts(py: Python<'_>) -> PyResult<Bound<'_, PyDict>> {
    let out = PyDict::new(py);
    for (name, count) in sheltersim_core::txn::counts() {
        out.set_item(name, count)?;
    }
    Ok(out)
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(counts, m)?)?;
    Ok(())
}
