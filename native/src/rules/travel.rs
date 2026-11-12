//! `_route`, `_schedule_travel`, `_schedule_staff_support_travel` and the walk home: the glue of the
//! core's kernels (`sheltersim_core::rules::travel`, lane c6 of Issue #152). Each entry reads what
//! Python holds (the Run's graph and capacities, whether `model_state` holds `movement_v1` and
//! `staff_operations_v1`, the movement section of the configs, the floor plan), runs the core on
//! the world borrowed whole, and makes the Python values of what it returns (`ZoneRoute`). The core
//! writes the task, the person's row, the event and the row of `movement_v1`; the objects and an
//! attached tree follow them when the world is given back.
//!
//! The routers are built from the world's tables at each `_refresh_space` (`build_routes`); where
//! the tables dropped their rows they are refused, and the controller's `space` and `staff_space`
//! are not read (lane c6). A `SpaceRouter` a Python caller hands `assign_staff_operations` is read
//! as it is (`StaffRouter::Given`): it is that caller's input.
//!
//! A floor plan that `Geo::of` could not read is passed as none, and where the core reads it, it
//! says `PLAN_UNREAD`: the glue raises what `Geo::of` raised instead, as the rule raised it there.
//!
//! The rules that plan a trip and write it later (`_apply_effect`, the arrivals) are the core's
//! (lanes c7 and e1), with the core's writer.

use std::sync::Arc;

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyTuple};
use sheltersim_core::config::movement::Movement;
use sheltersim_core::routes::{Edge, Router, Which};
use sheltersim_core::rules::kit::{Events, Held, Kept};
use sheltersim_core::rules::overlay::PLAN_UNREAD;
use sheltersim_core::rules::travel::{self as core, Graph, Routing, Walking};
use sheltersim_core::world::World;

pub(crate) use sheltersim_core::rules::travel::{PlannedTrip, Route, Trip};

use crate::core::configs;
use crate::core::errors::{self, OrPy};
use crate::core::row::{attribute, exact_int, exact_str, unsupported};
use crate::core::run::{run_of, RunCore};
use crate::core::world_core::{core_of, field_held, namespace_held, with_world};
use crate::rules::movement::{optional_str, route_points_of, str_tuple, KEY as MOVEMENT};
use crate::rules::movement_geometry::{Geo, GeoPy};
use crate::state::events::{write_result, EventRef, EventRow, EventRowPy, EventWriter};
use crate::state::staff_operations::NAME as STAFF_OPERATIONS;
use crate::Unsupported;

/// The people's graph of a controller's Run, and its capacities.
pub(crate) type RunGraph = Graph;

// ---- What Python holds ----------------------------------------------------------------------------

/// The floor plan as `Geo::of` read it.
pub(crate) struct Plan(PyResult<Arc<Geo>>);

impl Plan {
    pub(crate) fn of(world: &Bound<'_, PyAny>) -> Self {
        Plan(Geo::of(world))
    }

    pub(crate) fn get(&self) -> Option<&Arc<Geo>> {
        self.0.as_ref().ok()
    }

    /// The kernel's result, with what `Geo::of` raised where the kernel read the plan.
    pub(crate) fn finish<T>(&self, py: Python<'_>, result: PyResult<T>) -> PyResult<T> {
        match (result, &self.0) {
            (Err(error), Err(read)) if error.is_instance_of::<Unsupported>(py) && error.value(py).to_string() == PLAN_UNREAD => Err(read.clone_ref(py)),
            (result, _) => result,
        }
    }
}

/// Whether the world holds `movement_v1`, as the rules take it.
pub(crate) fn movement_held(world: &Bound<'_, PyAny>) -> PyResult<Held> {
    namespace_held(world, intern!(world.py(), MOVEMENT), "the world holds another movement state")
}

/// Whether the world holds `staff_operations_v1`, as the rules take it.
pub(crate) fn staff_held(world: &Bound<'_, PyAny>) -> PyResult<Held> {
    namespace_held(world, intern!(world.py(), STAFF_OPERATIONS), "the world holds another document than the row's")
}

/// The movement section of the configs, kept for the stamp of `model_configs`.
pub(crate) fn movement_section(world: &Bound<'_, PyAny>) -> PyResult<Kept<Movement>> {
    configs::kept_section(world, |configs| &configs.movement, Movement::parse)
}

/// The rows of the pending events Python scheduled, read before a kernel reads the whole queue.
pub(crate) fn read_queue_rows(world: &Bound<'_, PyAny>) -> PyResult<()> {
    let py = world.py();
    let Ok(core) = core_of(world) else {
        return Ok(());
    };
    let (queue, _) = core.try_borrow()?.event_stores(py);
    match queue {
        Some(queue) => queue.get().read_rows(py),
        None => Ok(()),
    }
}

/// What the rules of travel read of the Python world, read once per entry.
pub(crate) struct Inputs {
    pub(crate) plan: Plan,
    pub(crate) section: Kept<Movement>,
    pub(crate) movement: Held,
    pub(crate) staff: Held,
}

impl Inputs {
    pub(crate) fn of(world: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self { plan: Plan::of(world), section: movement_section(world)?, movement: movement_held(world)?, staff: staff_held(world)? })
    }

    pub(crate) fn routing<'a>(&'a self, graph: &'a Graph) -> Routing<'a> {
        Routing { graph, geo: self.plan.get(), movement: &self.section, staff: &self.staff }
    }

    pub(crate) fn walking(&self) -> Walking<'_> {
        Walking { movement: &self.movement, geo: self.plan.get() }
    }

    /// The kernel's result, with what `Geo::of` raised where the kernel read the plan.
    pub(crate) fn finish<T>(&self, py: Python<'_>, result: PyResult<T>) -> PyResult<T> {
        self.plan.finish(py, result)
    }
}

// ---- The routers of a Run -------------------------------------------------------------------------

/// The routers of the world's tables now, or None where the world has no core or its tables
/// dropped their rows. The staff's router is the people's or what its plan refused.
#[allow(clippy::type_complexity)]
fn routers(world: &Bound<'_, PyAny>, capacities: &sheltersim_core::run::RouteCapacities, plan: &Plan) -> PyResult<Option<(Arc<Router>, PyResult<Arc<Router>>)>> {
    let py = world.py();
    let built = match with_world(world, |cell| core::routers_of(cell, capacities, plan.get())) {
        Ok(built) => built.or_py()?,
        Err(error) if error.is_instance_of::<Unsupported>(py) => return Ok(None),
        Err(error) => return Err(error),
    };
    Ok(built.map(|(people, staff)| {
        let staff = plan.finish(py, staff.or_py()).map_err(|error| errors::kernel_refusal(py, "movement.route_is_physically_clear", error));
        (people, staff)
    }))
}

/// `_refresh_space` for the kernels: the people's and the staff's routers of a new epoch, from the
/// world's tables now, so an edge closed later is not seen until the next refresh. What
/// `ZoneGraph.from_states(world.zones, world.zone_edges, edge_capacities=controller.route_capacities)`
/// turns down raises its ValueError; the floor plan's refusal is `route_is_physically_clear`'s
/// KernelRefusal. Where the capacities or the tables cannot be read, nothing is built: the kernels
/// ask again when they read the routers (`rows_of`), and refuse then.
pub(crate) fn build_routes(run: &RunCore, epoch: u64, world: &Bound<'_, PyAny>) -> PyResult<()> {
    let Ok(capacities) = run.capacities() else {
        return Ok(());
    };
    let plan = Plan::of(world);
    let Some((people, staff)) = routers(world, &capacities, &plan)? else {
        return Ok(());
    };
    run.keep(Which::People, epoch, people)?;
    run.keep(Which::Staff, epoch, staff?)?;
    Ok(())
}

/// A router of the controller's world, built when the refresh built none.
fn router_of(controller: &Bound<'_, PyAny>, run: &RunCore, which: Which) -> PyResult<Arc<Router>> {
    let world = attribute(controller, intern!(controller.py(), "world"))?;
    let capacities = run.capacities()?;
    let plan = Plan::of(&world);
    let Some((people, staff)) = routers(&world, &capacities, &plan)? else {
        // A world without a core says so; tables that dropped their rows are refused.
        core_of(&world)?;
        return unsupported(core::ZONES_TAINTED);
    };
    match which {
        Which::People => Ok(people),
        Which::Staff => staff,
    }
}

/// The people's graph of this controller, built at its last `_refresh_space`.
pub(crate) fn rows_of(controller: &Bound<'_, PyAny>) -> PyResult<RunGraph> {
    let run = run_of(controller)?;
    let run = run.get();
    let capacities = run.capacities()?;
    let router = run.router(Which::People, || router_of(controller, run, Which::People))?;
    Ok(Graph { router, capacities })
}

/// Where `assign_staff_operations` finds its graph.
pub(crate) enum StaffRouter<'a, 'py> {
    /// A `SpaceRouter` a Python caller gave, read at each call.
    Given(&'a Bound<'py, PyAny>),
}

/// The open edges of a `ZoneGraph`, as a router: the graph of a `SpaceRouter` a Python caller gave.
fn read_graph(graph: &Bound<'_, PyAny>) -> PyResult<Router> {
    let py = graph.py();
    let zones_object = attribute(graph, intern!(py, "zones"))?;
    let mut zones = Vec::new();
    let mut open = Vec::new();
    for key in zones_object.try_iter()? {
        zones.push(Arc::from(exact_str(&key?)?));
    }
    let edges = attribute(graph, intern!(py, "edges"))?;
    for edge in edges.downcast_exact::<PyTuple>().map_err(|_| Unsupported::new_err("the edges are not a tuple"))?.iter() {
        let closed = attribute(&edge, intern!(py, "closed"))?;
        if closed.downcast_exact::<PyBool>().map_err(|_| Unsupported::new_err("closed is not a bool"))?.is_true() {
            continue;
        }
        let capacity = attribute(&edge, intern!(py, "max_agents_in_transit"))?;
        let capacity = if capacity.is_none() { None } else { Some(exact_int(&capacity)?) };
        // `loads.get(edge_id, 0) >= capacity`, with no trips counted.
        if capacity.is_some_and(|capacity| capacity <= 0) {
            continue;
        }
        let from: Arc<str> = Arc::from(exact_str(&attribute(&edge, intern!(py, "from_zone_id"))?)?);
        open.push(Edge {
            id: Arc::from(exact_str(&attribute(&edge, intern!(py, "id"))?)?),
            from,
            to: Arc::from(exact_str(&attribute(&edge, intern!(py, "to_zone_id"))?)?),
            minutes: exact_int(&attribute(&edge, intern!(py, "travel_minutes"))?)?,
            capacity,
        });
    }
    Ok(Router::new(zones, open))
}

/// The graph of `assign_staff_operations`' router, whose routes `edge_loads` always reach.
pub(crate) fn staff_router_rows(source: StaffRouter<'_, '_>) -> PyResult<Arc<Router>> {
    match source {
        StaffRouter::Given(router) => {
            let py = router.py();
            let policy = attribute(router, intern!(py, "policy"))?;
            if policy.get_type().name()?.to_str()? != "ShortestTravelTimePolicy" {
                return unsupported("another route policy");
            }
            Ok(Arc::new(read_graph(&attribute(router, intern!(py, "graph"))?)?))
        }
    }
}

// ---- _route ---------------------------------------------------------------------------------------

/// The world's writer, with what the caller's `EventWriter` reserved.
fn events_of<'w>(world: &'w World, reserved: usize) -> sheltersim_core::error::Result<Events<'w>> {
    let events = Events::of(world)?;
    events.reserve(reserved);
    Ok(events)
}

pub(crate) fn route_class(py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
    py.import("sheltersim.policies.space")?.getattr("ZoneRoute")
}

/// `_route`.
#[pyfunction]
pub(crate) fn route<'py>(
    controller: &Bound<'py, PyAny>,
    origin_zone_id: &Bound<'py, PyAny>,
    destination_zone_id: &Bound<'py, PyAny>,
    agent_id: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = controller.py();
    let world = attribute(controller, intern!(py, "world"))?;
    let graph = rows_of(controller)?;
    let origin: Arc<str> = Arc::from(exact_str(origin_zone_id)?);
    let destination: Arc<str> = Arc::from(exact_str(destination_zone_id)?);
    let agent = exact_str(agent_id)?;
    if graph.capacities.any {
        read_queue_rows(&world)?;
    }
    let inputs = Inputs::of(&world)?;
    let found = with_world(&world, |cell| core::route(cell, &inputs.routing(&graph), &origin, &destination, agent))?;
    let Some(route) = inputs.finish(py, write_result(py, found))? else {
        return Ok(py.None().into_bound(py));
    };
    let zones = PyTuple::new(py, route.zone_ids.iter().map(|id| &**id))?;
    let edges = PyTuple::new(py, route.edge_ids.iter().map(|id| &**id))?;
    route_class(py)?.call1((origin_zone_id, destination_zone_id, zones, edges, route.minutes))
}

// ---- _schedule_travel ---------------------------------------------------------------------------

/// The writes of `_schedule_travel` and `_schedule_room_approach`.
pub(crate) fn write_travel(world: &Bound<'_, PyAny>, writer: &EventWriter<'_>, trip: PlannedTrip) -> PyResult<()> {
    let reserved = writer.reserved();
    let written = with_world(world, |cell| -> sheltersim_core::events::writer::WriteResult<()> {
        let events = events_of(cell, reserved)?;
        core::write_travel(cell, &events, trip)
    })?;
    write_result(world.py(), written)
}

/// A `ZoneRoute` as a row.
fn route_of_object(route: &Bound<'_, PyAny>) -> PyResult<Route> {
    let py = route.py();
    Ok(Route {
        origin: Arc::from(exact_str(&attribute(route, intern!(py, "origin_zone_id"))?)?),
        destination: Arc::from(exact_str(&attribute(route, intern!(py, "destination_zone_id"))?)?),
        zone_ids: str_tuple(&attribute(route, intern!(py, "zone_ids"))?)?,
        edge_ids: str_tuple(&attribute(route, intern!(py, "edge_ids"))?)?,
        minutes: exact_int(&attribute(route, intern!(py, "travel_minutes"))?)?,
    })
}

/// `_schedule_travel` of one trip, with its writes.
fn schedule(world: &Bound<'_, PyAny>, trip: Trip<'_>) -> PyResult<bool> {
    let inputs = Inputs::of(world)?;
    let scheduled = with_world(world, |cell| core::schedule_travel(cell, inputs.walking(), trip))?;
    inputs.finish(world.py(), write_result(world.py(), scheduled))
}

/// `_schedule_staff_support_travel`.
#[pyfunction]
fn schedule_support_travel<'py>(
    controller: &Bound<'py, PyAny>,
    agent_id: &Bound<'py, PyAny>,
    route: &Bound<'py, PyAny>,
    request_id: &Bound<'py, PyAny>,
    staff_id: &Bound<'py, PyAny>,
    minute: &Bound<'py, PyAny>,
) -> PyResult<bool> {
    let world = attribute(controller, intern!(controller.py(), "world"))?;
    let request: Arc<str> = Arc::from(exact_str(request_id)?);
    let trip = Trip {
        agent_id: Arc::from(exact_str(agent_id)?),
        route: route_of_object(route)?,
        points: Vec::new(),
        purpose: "staff_support_travel",
        facility_id: None,
        minute: exact_int(minute)?,
        causation_id: Some(request.clone()),
        support_request_id: Some(request),
        support_staff_id: Some(Arc::from(exact_str(staff_id)?)),
    };
    schedule(&world, trip)
}

/// `_schedule_travel`: with `validate_state` (true when not given), `world.validate()` after a trip
/// (`Host::validate_world`).
#[pyfunction]
#[pyo3(signature = (
    controller, *, agent_id, route, route_points=None, purpose, facility_id, minute, causation_id=None,
    support_request_id=None, support_staff_id=None, validate_state=None
))]
#[allow(clippy::too_many_arguments)]
fn schedule_travel<'py>(
    controller: &Bound<'py, PyAny>,
    agent_id: &Bound<'py, PyAny>,
    route: &Bound<'py, PyAny>,
    route_points: Option<&Bound<'py, PyAny>>,
    purpose: &Bound<'py, PyAny>,
    facility_id: &Bound<'py, PyAny>,
    minute: &Bound<'py, PyAny>,
    causation_id: Option<&Bound<'py, PyAny>>,
    support_request_id: Option<&Bound<'py, PyAny>>,
    support_staff_id: Option<&Bound<'py, PyAny>>,
    validate_state: Option<&Bound<'py, PyAny>>,
) -> PyResult<bool> {
    use sheltersim_core::host::Host;
    let py = controller.py();
    let validate = match validate_state {
        Some(validate) => validate.is_truthy()?,
        None => true,
    };
    let world = attribute(controller, intern!(py, "world"))?;
    let trip = Trip {
        agent_id: Arc::from(exact_str(agent_id)?),
        route: route_of_object(route)?,
        points: route_points_of(route_points)?,
        purpose: exact_str(purpose)?,
        facility_id: optional_str(Some(facility_id))?,
        minute: exact_int(minute)?,
        causation_id: optional_str(causation_id)?,
        support_request_id: optional_str(support_request_id)?,
        support_staff_id: optional_str(support_staff_id)?,
    };
    if !schedule(&world, trip)? {
        return Ok(false);
    }
    if validate {
        crate::host::PyHost::of_world(&world, pyo3::types::PyDict::new(py)).validate_world().map_err(|error| error.0)?;
    }
    Ok(true)
}

/// What the walk home reads of Python: the Run's graph and the inputs of travel.
pub(crate) struct Home {
    graph: Graph,
    inputs: Inputs,
}

impl Home {
    pub(crate) fn of(controller: &Bound<'_, PyAny>, world: &Bound<'_, PyAny>) -> PyResult<Self> {
        let graph = rows_of(controller)?;
        if graph.capacities.any {
            read_queue_rows(world)?;
        }
        Ok(Self { graph, inputs: Inputs::of(world)? })
    }

    /// `_schedule_return_to_initial_zone` on the world borrowed.
    pub(crate) fn schedule(&self, world: &World, event: &EventRow) -> sheltersim_core::events::writer::WriteResult<()> {
        core::schedule_return(world, &self.inputs.routing(&self.graph), self.inputs.walking(), event)
    }

    /// The result of `schedule`, as Python's.
    pub(crate) fn finish(&self, py: Python<'_>, result: sheltersim_core::events::writer::WriteResult<()>) -> PyResult<()> {
        self.inputs.finish(py, write_result(py, result))
    }
}

/// `_schedule_return_to_initial_zone` of `experiments/phases/dispatch.py`.
pub(crate) fn schedule_return<'py>(controller: &Bound<'py, PyAny>, world: &Bound<'py, PyAny>, event: EventRef<'_, 'py>) -> PyResult<()> {
    let owned;
    let row = match event {
        EventRef::Row(row) => row,
        EventRef::Object(object) => {
            owned = EventRow::from_event(object)?.ok_or_else(|| Unsupported::new_err("the event is not a row"))?;
            &owned
        }
    };
    let home = Home::of(controller, world)?;
    let returned = with_world(world, |cell| home.schedule(cell, row))?;
    home.finish(world.py(), returned)
}

/// `_schedule_return_to_initial_zone`, called from Python.
#[pyfunction]
#[pyo3(name = "schedule_return")]
fn schedule_return_step<'py>(controller: &Bound<'py, PyAny>, world: &Bound<'py, PyAny>, event: &Bound<'py, PyAny>) -> PyResult<()> {
    schedule_return(controller, world, EventRef::Object(event))
}

// ---- The choice of a facility -------------------------------------------------------------------

/// Whether `world.metric_state` is still its slot's tree (`kit::check_metric_state`).
pub(crate) fn metric_state_held(world: &Bound<'_, PyAny>) -> PyResult<bool> {
    field_held(world, intern!(world.py(), "metric_state"))
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(route, m)?)?;
    m.add_function(wrap_pyfunction!(schedule_travel, m)?)?;
    m.add_function(wrap_pyfunction!(schedule_support_travel, m)?)?;
    m.add_function(wrap_pyfunction!(schedule_return_step, m)?)?;
    Ok(())
}
