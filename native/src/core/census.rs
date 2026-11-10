//! Where each `Unsupported` was raised and who caught it, for benchmarks/rule_shadow/refusal_census.py.
//! The counts live in `sheltersim_core::census`; this file adds the Python entries.

use pyo3::prelude::*;
use pyo3::types::PyList;

pub(crate) use sheltersim_core::census::{is_on, moved, raised};

/// A refusal caught in Rust: the driver's exit of a kernel's refusal (`rules/tick.rs`).
#[cold]
#[inline(never)]
pub(crate) fn caught(py: Python<'_>, by: &str, error: &PyErr) {
    if is_on() {
        sheltersim_core::census::caught(by, error.value(py).to_string());
    }
}

/// Turn the census on or off for this process.
#[pyfunction]
pub(crate) fn census_switch(on: bool) {
    sheltersim_core::census::switch(on);
}

/// A refusal that reached a Python entry of a kernel.
#[pyfunction]
pub(crate) fn census_caught(by: &str, reason: String) {
    sheltersim_core::census::caught_at_entry(by, reason);
}

/// Take what was counted: `(raised, caught)`, rows of `(file, line, reason, n)` and
/// `(by, file, line, reason, n)`, and start again. The moved errors wait for `census_take_errors`.
#[pyfunction]
pub(crate) fn census_take<'py>(py: Python<'py>) -> PyResult<(Bound<'py, PyList>, Bound<'py, PyList>)> {
    let (raised, caught) = sheltersim_core::census::take();
    let raised_rows = PyList::empty(py);
    for row in raised {
        raised_rows.append(row)?;
    }
    let caught_rows = PyList::empty(py);
    for row in caught {
        caught_rows.append(row)?;
    }
    Ok((raised_rows, caught_rows))
}

/// Take the moved errors, rows of `(file, line, text, n)`, and start again. The second list, of
/// what a Python rule raised after a refusal, is empty: no Python rule stands behind a kernel since
/// lane L10. It stays for the readers of this pair.
#[pyfunction]
pub(crate) fn census_take_errors<'py>(py: Python<'py>) -> PyResult<(Bound<'py, PyList>, Bound<'py, PyList>)> {
    let moved_rows = PyList::empty(py);
    for row in sheltersim_core::census::take_errors() {
        moved_rows.append(row)?;
    }
    Ok((moved_rows, PyList::empty(py)))
}
