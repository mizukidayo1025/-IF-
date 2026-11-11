//! `RunCore`: the core's `Run` (`sheltersim_core::run`) on a controller, as `controller._run_core`
//! (Issue #152, lane 7-3). One per controller, so a Run, its continuation and each Branch keep
//! their own configs and route graphs.
//!
//! The controller makes it in `__init__` from `route_capacities`, before its first
//! `_refresh_space`, and each `_refresh_space` calls `refresh_routes`. The routers are built when a
//! kernel first asks after that (`rules/travel.rs`), from the graphs `_refresh_space` made, so what
//! reading them refuses is refused where it was before.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyInt, PyString};

use sheltersim_core::routes::{Router, Which};
use sheltersim_core::run::{Capacity, RouteCapacities, Run, RunConfig};

use crate::core::errors::OrPy;
use crate::core::row::attribute;
use crate::core::tracked::as_readable_dict;
use crate::Unsupported;

/// What a controller keeps beside its World.
#[pyclass(module = "sheltersim_native", frozen)]
pub struct RunCore {
    run: Mutex<Run>,
}

/// `controller.route_capacities` as `route_for` reads it: a refusal is kept with the text the
/// read would raise, and raised where a route is asked.
fn read_capacities(table: &Bound<'_, PyAny>) -> Result<Arc<RouteCapacities>, &'static str> {
    let Some(table) = as_readable_dict(table) else {
        return Err("a value is not a plain dict");
    };
    let mut by_edge = HashMap::new();
    for (key, value) in table.iter() {
        // `table.get(edge_id)` with an edge ID: a key that is no text is never found.
        let Some(key) = key.downcast::<PyString>().ok().and_then(|key| key.to_str().ok().map(Arc::<str>::from)) else {
            continue;
        };
        let capacity = if value.is_none() {
            Capacity::Unlimited
        } else if let Ok(number) = value.downcast_exact::<PyInt>() {
            match number.extract::<i64>() {
                Ok(capacity) => Capacity::Max(capacity),
                Err(_) => Capacity::Refused("an int is too large"),
            }
        } else {
            Capacity::Refused("a value is not an int")
        };
        by_edge.insert(key, capacity);
    }
    Ok(Arc::new(RouteCapacities { by_edge, any: !table.is_empty() }))
}

impl RunCore {
    fn lock(&self) -> PyResult<MutexGuard<'_, Run>> {
        self.run.lock().map_err(|_| Unsupported::new_err("the run is poisoned"))
    }

    /// `controller.route_capacities`, or the refusal reading them gives.
    pub(crate) fn capacities(&self) -> PyResult<Arc<RouteCapacities>> {
        self.lock()?.config.route_capacities().cloned().or_py()
    }

    /// The router of this epoch, built with `build` the first time it is asked. Nothing is held
    /// while `build` reads Python; what it refuses is not kept, so the next ask reads again.
    pub(crate) fn router(&self, which: Which, build: impl FnOnce() -> PyResult<Router>) -> PyResult<Arc<Router>> {
        let epoch = {
            let run = self.lock()?;
            if let Some(router) = run.routes.get(which) {
                return Ok(router);
            }
            run.routes.epoch()
        };
        let router = Arc::new(build()?);
        Ok(self.lock()?.routes.keep(which, epoch, router))
    }
}

#[pymethods]
impl RunCore {
    /// A Run's core, with `route_capacities` (`layout_routing.max_agents_in_transit_by_edge`).
    #[new]
    fn new(route_capacities: &Bound<'_, PyAny>) -> Self {
        let config = RunConfig { route_capacities: read_capacities(route_capacities) };
        Self { run: Mutex::new(Run::new(config)) }
    }

    /// `_refresh_space` made new graphs: the routers are built again when next asked.
    fn refresh_routes(&self) -> PyResult<()> {
        self.lock()?.routes.refresh();
        Ok(())
    }

    /// How many times `refresh_routes` ran.
    #[getter]
    fn epoch(&self) -> PyResult<u64> {
        Ok(self.lock()?.routes.epoch())
    }

    /// The epochs the people's and the staff's routers held were built in, None for one not built:
    /// for tests.
    fn routes_built(&self) -> PyResult<(Option<u64>, Option<u64>)> {
        let run = self.lock()?;
        Ok((run.routes.built(Which::People), run.routes.built(Which::Staff)))
    }
}

/// `controller._run_core`.
pub(crate) fn run_of<'py>(controller: &Bound<'py, PyAny>) -> PyResult<Bound<'py, RunCore>> {
    let core = attribute(controller, intern!(controller.py(), "_run_core"))?;
    core.downcast_into_exact::<RunCore>().map_err(|_| Unsupported::new_err("the controller has no run core"))
}
