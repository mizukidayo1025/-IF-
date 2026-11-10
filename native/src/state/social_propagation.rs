//! `social_propagation_v1`: the row is the core's (`sheltersim_core::state::social_propagation`);
//! here is its Python side.

use pyo3::prelude::*;

pub(crate) use sheltersim_core::state::social_propagation::*;

use crate::core::row::{load_row, py_row};
use crate::core::world_core::RowType;

py_row! { ContactRecord { last_seen_minute, consecutive_minutes } }
py_row! { SocialRow { schema_version, contacts, agent_cooldown_until } }

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    m.add("ROW", Py::new(py, RowType::new("social_propagation", load_row::<SocialRow>))?)?;
    Ok(())
}
