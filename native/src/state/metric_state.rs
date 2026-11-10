//! `world.metric_state`: the row is the core's (`sheltersim_core::state::metric_state`), a map of
//! floats, whose Python side is `PyField for IdMap<f64>`.

use pyo3::prelude::*;

pub(crate) use sheltersim_core::state::metric_state::*;

use crate::core::row::load_row;
use crate::core::world_core::RowType;

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("ROW", Py::new(m.py(), RowType::new("metric_state", load_row::<MetricRow>))?)?;
    Ok(())
}
