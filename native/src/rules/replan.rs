//! `_replan_journeys` of `experiments/phases/update.py`: the glue of the core's step
//! (`sheltersim_core::rules::tick::update::replan_journeys`, lanes c6 and P9-3b of Issue #152). The
//! tick's update phase calls the core's step itself; this entry is the phase path's (policy E10) and
//! the tests'. It reads its arguments, and runs the core with the controller's native host and Run:
//! what the world holds, the configs' sections, the floor plan, the queue's rows and the EventLog's
//! records come through the host, the graph and `staff_service` from the Run. A refusal before the
//! first stop says Unsupported, with nothing changed; past it, a stop is written, and a refusal is a
//! RuntimeError.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use pyo3::intern;
use pyo3::prelude::*;
use sheltersim_core::rules::tick::{update, RunCell, Seed, Tick, ZoneNeighbors};
use sheltersim_core::run::{Run, RunConfig};

use crate::core::row::{attribute, exact_int, exact_str};
use crate::core::run::{policies_of, run_of};
use crate::core::world_core::PyWorldCell;
use crate::host::PyHost;

/// The refusal of a controller without a Run, where the step reads the Run's graph.
const NO_RUN: &str = "the controller has no run core";

/// `_replan_journeys`. `state` is the movement namespace, which the row stands for.
#[pyfunction]
pub(crate) fn replan_journeys<'py>(
    controller: &Bound<'py, PyAny>,
    state: &Bound<'py, PyAny>,
    agent_ids: &Bound<'py, PyAny>,
    minute: &Bound<'py, PyAny>,
) -> PyResult<()> {
    let _ = state;
    let py = controller.py();
    let world = attribute(controller, intern!(py, "world"))?;
    let minute = exact_int(minute)?;
    let ids = agent_ids.try_iter()?.collect::<PyResult<Vec<_>>>()?;
    let ids = ids.iter().map(|id| Ok(Arc::<str>::from(exact_str(id)?))).collect::<PyResult<Vec<Arc<str>>>>()?;
    match run_of(controller) {
        Ok(run) => replan_on(controller, &world, run.get(), minute, &ids),
        // Nothing reads the Run before its graph, which refuses as reading the Run refused.
        Err(_) => replan_on(controller, &world, &RefCell::new(Run::new(RunConfig { route_capacities: Err(NO_RUN) })), minute, &ids),
    }
}

fn replan_on<R: RunCell>(controller: &Bound<'_, PyAny>, world: &Bound<'_, PyAny>, run: &R, minute: i64, ids: &[Arc<str>]) -> PyResult<()> {
    let policies = policies_of(controller)?;
    let mut host = PyHost::of_controller(controller)?;
    let cell = PyWorldCell(world);
    let neighbors = Arc::new(ZoneNeighbors { social: Ok(HashMap::new()), information: Ok(HashMap::new()) });
    let mut tick = Tick { cell: &cell, host: &mut host, run, policies, minute, seed: Seed::Other, neighbors };
    update::replan_journeys(&mut tick, ids).map_err(|error| error.0)
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(replan_journeys, m)?)?;
    Ok(())
}
