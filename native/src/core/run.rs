//! `RunCore`: the core's `Run` (`sheltersim_core::run`) on a controller, as `controller._run_core`
//! (Issue #152, lane 7-3). One per controller, so a Run, its continuation and each Branch keep
//! their own configs and route graphs.
//!
//! The controller makes it in `__init__` from `route_capacities`, before its first
//! `_refresh_space`, and each `_refresh_space` calls `refresh_routes(world)`, which builds both
//! routers from the world's zone and edge tables right then (`rules/travel.rs`, lane 7-4). Where the
//! tables cannot be read (rows that do not follow the world's maps, or capacities that are no plain
//! dict), and after `refresh_routes()` without a world, the routers are built when a kernel first
//! asks, from the graphs `_refresh_space` made, as in lane 7-3.

use std::collections::{HashMap, HashSet};
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
    let mut unmatched_keys = false;
    for (key, value) in table.iter() {
        // `table.get(edge_id)` with an edge ID: a key that is no text is never found.
        let Some(key) = key.downcast::<PyString>().ok().and_then(|key| key.to_str().ok().map(Arc::<str>::from)) else {
            unmatched_keys = true;
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
    Ok(Arc::new(RouteCapacities { by_edge, any: !table.is_empty(), unmatched_keys }))
}

impl RunCore {
    fn lock(&self) -> PyResult<MutexGuard<'_, Run>> {
        self.run.lock().map_err(|_| Unsupported::new_err("the run is poisoned"))
    }

    /// `controller.route_capacities`, or the refusal reading them gives.
    pub(crate) fn capacities(&self) -> PyResult<Arc<RouteCapacities>> {
        self.lock()?.config.route_capacities().cloned().or_py()
    }

    /// Keep the router built in `epoch`, unless the routes were refreshed since.
    pub(crate) fn keep(&self, which: Which, epoch: u64, router: Arc<Router>) -> PyResult<Arc<Router>> {
        Ok(self.lock()?.routes.keep(which, epoch, router))
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

    /// `_refresh_space`: a new epoch, whose routers are built from the world's zone and edge tables
    /// now, with `ZoneGraph`'s checks and words. Without a world, or where the tables cannot be
    /// read, they are built when next asked from the controller's `space` and `staff_space`.
    #[pyo3(signature = (world=None))]
    fn refresh_routes(&self, world: Option<&Bound<'_, PyAny>>) -> PyResult<()> {
        let epoch = {
            let mut run = self.lock()?;
            run.routes.refresh();
            run.routes.epoch()
        };
        match world {
            Some(world) => crate::rules::travel::build_routes(self, epoch, world),
            None => Ok(()),
        }
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

    /// The route of the router this epoch holds (`"people"` or `"staff"`) with nothing closed and
    /// no trips counted, as `(zone IDs, edge IDs, minutes)` or None: for tests. Unsupported when no
    /// router is held.
    fn held_route(&self, which: &str, origin: &str, destination: &str) -> PyResult<Option<(Vec<String>, Vec<String>, i64)>> {
        let which = match which {
            "people" => Which::People,
            "staff" => Which::Staff,
            _ => return Err(Unsupported::new_err("no such router")),
        };
        let Some(router) = self.lock()?.routes.get(which) else {
            return Err(Unsupported::new_err("no router is held"));
        };
        let found = router.shortest(&HashSet::new(), None, &Arc::from(origin), &Arc::from(destination)).or_py()?;
        let texts = |ids: &[Arc<str>]| ids.iter().map(|id| id.to_string()).collect::<Vec<String>>();
        Ok(found.map(|route| (texts(&route.zone_ids), texts(&route.edge_ids), route.minutes)))
    }
}

/// `controller._run_core`.
pub(crate) fn run_of<'py>(controller: &Bound<'py, PyAny>) -> PyResult<Bound<'py, RunCore>> {
    let core = attribute(controller, intern!(controller.py(), "_run_core"))?;
    core.downcast_into_exact::<RunCore>().map_err(|_| Unsupported::new_err("the controller has no run core"))
}
