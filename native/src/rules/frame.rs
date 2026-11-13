//! `inspection/frames.py`'s `world_frame`: the frame of a world at its minute, made by the core
//! (`sheltersim_core::frame`, #264) on the world's own core, with the native host for what Python
//! parses (the floor plan's and the spatial constraints' documents, the waits' routes). It gives
//! the frame's canonical JSON as bytes; the Python entry reads them back into a dict.

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use sheltersim_core::frame::{self, Request};

use crate::core::row::attribute;
use crate::core::value::{VPy, V};
use crate::core::world_core::PyWorldCell;
use crate::host::PyHost;

/// The frame's canonical JSON. Without `state_hash`, the frame's is the world's digest
/// (`world.state_digest()`), of the world's rows and its `rng_state`.
#[pyfunction]
#[pyo3(signature = (world, *, run_id, branch_id, computed_until, state_hash=None))]
fn world_frame<'py>(
    world: &Bound<'py, PyAny>,
    run_id: &str,
    branch_id: &str,
    computed_until: i64,
    state_hash: Option<&str>,
) -> PyResult<Bound<'py, PyBytes>> {
    let rng_state = match state_hash {
        Some(_) => V::Null,
        None => V::from_py(&attribute(world, intern!(world.py(), "rng_state"))?)?,
    };
    let request = Request { run_id, branch_id, computed_until, state_hash };
    let raw = frame::frame(&PyWorldCell(world), &mut PyHost::of_entry(world), &request, &rng_state).map_err(|error| error.0)?;
    Ok(PyBytes::new(world.py(), &raw))
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(world_frame, m)?)?;
    Ok(())
}
