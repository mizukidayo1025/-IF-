//! `psychological_distress_v1`: the row is the core's (`sheltersim_core::state::psychological_distress`);
//! here are its Python side and its validator (docs/rust-port/lanes/p5-template-namespace.md).

use pyo3::prelude::*;

pub(crate) use sheltersim_core::state::psychological_distress::*;

use crate::core::row::{load_row, py_row};
use crate::core::world_core::{mirror_of, NamespaceMirror, RowType};

py_row! { AgentRecord { baseline_distress, long_term_distress, daily_acute_sum, daily_high_distress_minutes, daily_observed_minutes, completed_days } }
py_row! { DistressRow { schema_version, last_updated_minute, agents } }

/// The namespace's validator: accepts, or leaves it to the Python rules and their error text.
#[pyfunction]
fn validate(world: &Bound<'_, PyAny>) -> PyResult<bool> {
    let mirror = mirror_of(world, NAME)?;
    NamespaceMirror::with_row::<DistressRow, _>(&mirror, |row| Ok(row.is_valid()))
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    m.add("ROW", Py::new(py, RowType::new("psychological_distress", load_row::<DistressRow>))?)?;
    m.add_function(wrap_pyfunction!(validate, m)?)?;
    Ok(())
}
