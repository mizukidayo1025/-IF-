//! `RunCore`: the core's `Run` (`sheltersim_core::run`) on a controller, as `controller._run_core`
//! (Issue #152, lane 7-3). One per controller, so a Run, its continuation and each Branch keep
//! their own configs and route graphs.
//!
//! The controller makes it in `__init__` from `route_capacities`, before its first
//! `_refresh_space`, and each `_refresh_space` calls `refresh_routes(world)`, which builds both
//! routers from the world's zone and edge tables right then (`rules/travel.rs`, lanes 7-4 and c6).
//! Where the tables cannot be read (rows a value did not fit, or capacities that are no plain dict),
//! and after `refresh_routes()` without a world, the routers are built from the controller's world
//! when a kernel first asks, which refuses tables that dropped their rows (lane c6).
//!
//! The controller gives itself too (lane P8-0b): its `Policies` are read then, by the core's
//! `CONTROLLER` spec (`sheltersim_core::run::policies`), and again at `refresh_policies`. Reading
//! never raises: an attribute whose read raised is kept as a `Fault` (`configs::fault_of`), which
//! the read site raises again by reading again (policy E14). The read runs no rule and changes
//! nothing; a property it meets runs as `getattr` runs it.
//!
//! The view (lane P8 a1) keeps what it reads from one view to the next in the Run (`Run::views`),
//! and the people's IDs as Python strings here, whose hashes Python then computes once.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyString, PyType};

use indexmap::IndexMap;
use rustc_hash::FxHashMap;
use sheltersim_core::routes::{Router, Which};
use sheltersim_core::run::held::{Class, Held, Obj, Spec};
use sheltersim_core::rules::action_lifecycle::ActiveActions;
use sheltersim_core::run::policies::CONTROLLER;
use sheltersim_core::rules::world_view::ViewCache;
use sheltersim_core::run::{Capacity, Policies, RouteCapacities, Run, RunConfig, Scratch};
use sheltersim_core::value::V;

use crate::core::configs::fault_of;
use crate::core::errors::OrPy;
use crate::core::row::attribute;
use crate::core::tracked::{as_readable_dict, as_readable_list};
use crate::Unsupported;

/// What a controller keeps beside its World.
#[pyclass(module = "sheltersim_native", frozen)]
pub struct RunCore {
    run: Mutex<Run>,
    /// The people's IDs as Python strings, for the views' keys.
    keys: Mutex<FxHashMap<Arc<str>, Py<PyString>>>,
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

// ---- The controller's policies (lane P8-0b) ---------------------------------------------------

/// The class of an object by its names, and whether `sys.modules[module]` reached by `qualname`
/// is this class (no module is imported here).
fn class_of(object: &Bound<'_, PyAny>) -> Class {
    let py = object.py();
    let kind = object.get_type();
    let text = |name: &Bound<'_, PyString>| -> Option<Arc<str>> {
        let value = kind.getattr(name).ok()?;
        Some(Arc::from(value.downcast_exact::<PyString>().ok()?.to_str().ok()?))
    };
    let module = text(intern!(py, "__module__"));
    let qualname = text(intern!(py, "__qualname__"));
    let resolves = match (&module, &qualname) {
        (Some(module), Some(qualname)) => resolves(&kind, module, qualname).unwrap_or(false),
        _ => false,
    };
    Class { module, qualname, resolves }
}

fn resolves(kind: &Bound<'_, PyType>, module: &str, qualname: &str) -> PyResult<bool> {
    let py = kind.py();
    let modules = py.import(intern!(py, "sys"))?.getattr(intern!(py, "modules"))?;
    let Some(mut found) = modules.downcast::<PyDict>()?.get_item(module)? else {
        return Ok(false);
    };
    for part in qualname.split('.') {
        match found.getattr(part) {
            Ok(next) => found = next,
            Err(_) => return Ok(false),
        }
    }
    Ok(found.is(kind))
}

/// A value as a `Held`: an object its spec reads, a plain JSON value, a str subclass's text, or an
/// opaque value.
fn held_of(value: &Bound<'_, PyAny>, spec: Option<&'static Spec>) -> Held {
    if value.is_none() {
        return Held::Value(V::Null);
    }
    if let Some(spec) = spec {
        return Held::Object(Arc::new(read_object(value, spec)));
    }
    if let Some(plain) = plain_of(value, 0) {
        return Held::Value(plain);
    }
    if let Ok(text) = value.downcast::<PyString>() {
        if let Ok(text) = text.to_str() {
            return Held::Text(Arc::from(text));
        }
    }
    Held::Opaque { callable: value.is_callable() }
}

/// `VPy::from_py` without its refusal, which the census would count: None for a value that is not
/// a plain JSON value.
pub(crate) fn plain_of(value: &Bound<'_, PyAny>, depth: usize) -> Option<V> {
    if depth > crate::MAX_DEPTH {
        return None;
    }
    if value.is_none() {
        return Some(V::Null);
    }
    if let Ok(number) = value.downcast_exact::<PyFloat>() {
        return Some(V::Float(number.value()));
    }
    if let Ok(text) = value.downcast_exact::<PyString>() {
        return Some(V::Str(Arc::from(text.to_str().ok()?)));
    }
    if let Ok(flag) = value.downcast_exact::<PyBool>() {
        return Some(V::Bool(flag.is_true()));
    }
    if let Ok(number) = value.downcast_exact::<PyInt>() {
        return Some(match number.extract::<i64>() {
            Ok(small) => V::Int(small),
            Err(_) => V::Big(number.repr().ok()?.to_str().ok()?.into()),
        });
    }
    if let Some(object) = as_readable_dict(value) {
        let mut map = IndexMap::with_capacity(object.len());
        for (key, item) in object.iter() {
            let key = key.downcast_exact::<PyString>().ok()?;
            map.insert(Arc::from(key.to_str().ok()?), plain_of(&item, depth + 1)?);
        }
        return Some(V::Map(map));
    }
    if let Some(array) = as_readable_list(value) {
        let mut items = Vec::with_capacity(array.len());
        for item in array.iter() {
            items.push(plain_of(&item, depth + 1)?);
        }
        return Some(V::List(items));
    }
    None
}

/// What a spec reads of an object; what a read raised is kept as its fault.
fn read_object(object: &Bound<'_, PyAny>, spec: &'static Spec) -> Obj {
    let py = object.py();
    let mut found = Obj::new(class_of(object));
    let mut attrs = IndexMap::with_capacity(spec.attrs.len());
    for attr in spec.attrs {
        let read = object.getattr(attr.name).map(|value| held_of(&value, attr.spec)).map_err(|error| fault_of(py, &error));
        attrs.insert(Arc::from(attr.name), read);
    }
    if spec.fields {
        if let Some(own) = object.getattr(intern!(py, "__dict__")).ok().and_then(|own| own.downcast_into::<PyDict>().ok()) {
            for (key, value) in own.iter() {
                let Some(key) = key.downcast::<PyString>().ok().and_then(|key| key.to_str().ok().map(str::to_owned)) else {
                    continue;
                };
                if !attrs.contains_key(key.as_str()) {
                    attrs.insert(Arc::from(key.as_str()), Ok(held_of(&value, None)));
                }
            }
        }
    }
    found.attrs = attrs;
    if !spec.class_own.is_empty() {
        if let Ok(own) = object.get_type().getattr(intern!(py, "__dict__")) {
            for name in spec.class_own {
                if let Ok(value) = own.get_item(*name) {
                    found.class_own.insert(Arc::from(*name), held_of(&value, None));
                }
            }
        }
    }
    found
}

/// The policies of a controller, read now.
fn read_policies(controller: &Bound<'_, PyAny>) -> Policies {
    Policies::of(Arc::new(read_object(controller, &CONTROLLER)))
}

impl RunCore {
    fn lock(&self) -> PyResult<MutexGuard<'_, Run>> {
        self.run.lock().map_err(|_| Unsupported::new_err("the run is poisoned"))
    }

    /// Run f on the open actions of the Run (lane P8 a2), with the Run locked: f must not reach
    /// this Run again.
    pub(crate) fn with_actions<R>(&self, f: impl FnOnce(&mut ActiveActions) -> R) -> PyResult<R> {
        Ok(f(&mut self.lock()?.actions))
    }

    /// The controller's policies, shared: a host answers from them without the lock.
    pub(crate) fn policies(&self) -> PyResult<Arc<Policies>> {
        Ok(self.lock()?.policies.clone())
    }

    /// Run f on the values of the tick under way (`Run::scratch`, lane P8 a3), with the Run locked:
    /// f must not reach this Run again.
    pub(crate) fn with_scratch<R>(&self, f: impl FnOnce(&mut Scratch) -> R) -> PyResult<R> {
        Ok(f(&mut self.lock()?.scratch))
    }

    /// `controller.route_capacities`, or the refusal reading them gives.
    pub(crate) fn capacities(&self) -> PyResult<Arc<RouteCapacities>> {
        self.lock()?.config.route_capacities().cloned().or_py()
    }

    /// Keep the router built in `epoch`, unless the routes were refreshed since.
    pub(crate) fn keep(&self, which: Which, epoch: u64, router: Arc<Router>) -> PyResult<Arc<Router>> {
        Ok(self.lock()?.routes.keep(which, epoch, router))
    }

    /// What the view keeps, taken out of the Run while a view is built (`put_views` gives it back),
    /// so that nothing is locked while the world is borrowed.
    pub(crate) fn take_views(&self) -> PyResult<ViewCache> {
        Ok(std::mem::take(&mut self.lock()?.views))
    }

    pub(crate) fn put_views(&self, views: ViewCache) -> PyResult<()> {
        self.lock()?.views = views;
        Ok(())
    }

    /// The Python strings of these IDs, made once for the Run.
    pub(crate) fn keys<'a>(&self, py: Python<'_>, ids: impl Iterator<Item = &'a Arc<str>>) -> PyResult<Vec<Py<PyString>>> {
        let mut keys = self.keys.lock().map_err(|_| Unsupported::new_err("the run is poisoned"))?;
        Ok(ids.map(|id| keys.entry(id.clone()).or_insert_with(|| PyString::new(py, id).unbind()).clone_ref(py)).collect())
    }

    /// The router of this epoch, built with `build` the first time it is asked. Nothing is held
    /// while `build` reads Python; what it refuses is not kept, so the next ask reads again.
    pub(crate) fn router(&self, which: Which, build: impl FnOnce() -> PyResult<Arc<Router>>) -> PyResult<Arc<Router>> {
        let epoch = {
            let run = self.lock()?;
            if let Some(router) = run.routes.get(which) {
                return Ok(router);
            }
            run.routes.epoch()
        };
        let router = build()?;
        Ok(self.lock()?.routes.keep(which, epoch, router))
    }
}

#[pymethods]
impl RunCore {
    /// A Run's core, with `route_capacities` (`layout_routing.max_agents_in_transit_by_edge`) and
    /// the policies of `controller`, read now; without a controller every read of them refuses.
    #[new]
    #[pyo3(signature = (route_capacities, controller=None))]
    fn new(route_capacities: &Bound<'_, PyAny>, controller: Option<&Bound<'_, PyAny>>) -> Self {
        let config = RunConfig { route_capacities: read_capacities(route_capacities) };
        let policies = controller.map_or_else(Policies::absent, read_policies);
        Self { run: Mutex::new(Run::with_policies(config, policies)), keys: Mutex::new(FxHashMap::default()) }
    }

    /// Read the controller's policies again: after a test or tool puts another policy or manager
    /// on it.
    fn refresh_policies(&self, controller: &Bound<'_, PyAny>) -> PyResult<()> {
        let policies = Arc::new(read_policies(controller));
        self.lock()?.policies = policies;
        Ok(())
    }

    /// What the policies say, by part, as text: for tests. A fault is `"!"` and its text, a
    /// refusal `"refused: "` and its text.
    fn policies_summary<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let policies = self.policies()?;
        let summary = PyDict::new(py);
        fn said<T: std::fmt::Debug>(read: &Result<T, sheltersim_core::config::Fault>) -> String {
            match read {
                Ok(value) => format!("{value:?}"),
                Err(fault) if fault.is_refusal() => format!("refused: {}", fault.text),
                Err(fault) => format!("! {:?}: {}", fault.class, fault.text),
            }
        }
        summary.set_item("controller", policies.has_controller())?;
        summary.set_item("psychology", said(&policies.psychology.model))?;
        summary.set_item("behavior", said(&policies.behavior.rule))?;
        summary.set_item("stress", said(&policies.stress.model))?;
        summary.set_item("information", said(&policies.information.model))?;
        summary.set_item("proximity", said(&policies.proximity.known))?;
        summary.set_item("proximity_config", said(&policies.proximity.config.clone().map(|config| !matches!(config, V::Null))))?;
        summary.set_item("complaint_wait_minutes", said(&policies.social.complaint_wait_minutes()))?;
        summary.set_item("toilet_service", said(&policies.services.toilet.known))?;
        summary.set_item("resource_service", said(&policies.services.resource.known))?;
        summary.set_item("staff_service", said(&policies.services.staff.known))?;
        summary.set_item("decision_sink", said(&policies.sinks.decision_sink))?;
        summary.set_item("trace_level", said(&policies.sinks.trace_level))?;
        summary.set_item("llm", said(&policies.llm()))?;
        Ok(summary)
    }

    /// `_refresh_space`: a new epoch, whose routers are built from the world's zone and edge tables
    /// now, with `ZoneGraph`'s checks and words. Without a world, or where the tables cannot be
    /// read, they are built from the controller's world when next asked, which refuses tables that
    /// dropped their rows (lane c6: the controller's `space` and `staff_space` are not read).
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

    /// How many person contexts and zones' conditions the view keeps: for tests.
    fn views_kept(&self) -> PyResult<(usize, usize)> {
        Ok(self.lock()?.views.kept())
    }

    /// Drop what the view keeps, as a new controller would start: for tests.
    fn forget_views(&self) -> PyResult<()> {
        self.put_views(ViewCache::default())
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

/// The policies of a controller (lane P8 a3): those its Run read, or, for a holder without a Run
/// that read them (a test's namespace, a Run made without its controller), read now.
pub(crate) fn policies_of(controller: &Bound<'_, PyAny>) -> PyResult<Arc<Policies>> {
    if let Ok(run) = run_of(controller) {
        let policies = run.get().policies()?;
        if policies.has_controller() {
            return Ok(policies);
        }
    }
    Ok(Arc::new(read_policies(controller)))
}

/// `controller._run_core`.
pub(crate) fn run_of<'py>(controller: &Bound<'py, PyAny>) -> PyResult<Bound<'py, RunCore>> {
    let core = attribute(controller, intern!(controller.py(), "_run_core"))?;
    core.downcast_into_exact::<RunCore>().map_err(|_| Unsupported::new_err("the controller has no run core"))
}
