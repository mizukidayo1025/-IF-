//! `domain/movement.py`: the glue of the core's kernels (`sheltersim_core::rules::movement`, lane c2
//! of Issue #152). Each entry reads its arguments and what Python holds (the floor plan, the
//! movement section of the configs, whether the world holds the namespaces and `metric_state`, the
//! staff's operations where the row of `staff_operations_v1` does not answer) in the order the
//! kernel read them before, runs the core on the world borrowed whole (`with_world`), and makes the
//! Python values of what it returns (`ZoneRoute`, `Point2D`, the dicts). The core writes the row of
//! `movement_v1` alone, and an attached tree follows it when the world is given back (lane P8-0c).
//!
//! A replanned queue trip's pending event is settled by the core on the world's queue and log (lane
//! c6); the glue reads the rows of the events Python scheduled first, and lets go the objects of
//! the events the core took out. What stays here besides: the readers that make Python documents of
//! the row for the rules not yet in the core (`row_position`, `journey_of`).

use std::sync::Arc;

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::sync::GILOnceCell;
use pyo3::types::{PyDict, PyString, PyTuple, PyType};

use sheltersim_core::config::movement::Movement;
use sheltersim_core::rules::kit::Held;
use sheltersim_core::rules::movement::{self as core, Advanced, InOrder, StaffOp, StaffOps, StaffTravel, Start, Tick, Validated, Where};

pub(crate) use sheltersim_core::config::movement::EXACT_INT;
pub(crate) use sheltersim_core::rules::kit::mobility_text;

use crate::core::configs;
use crate::core::errors::OrPy;
use crate::core::row::{attribute, exact_dict, exact_float, exact_int, exact_list, exact_str, unsupported, IdMap, PyField};
use crate::core::world_core::{field_held, mirror_of, namespace_held, with_world, NamespaceMirror};
use crate::rules::movement_geometry::{usable, Geo, GeoPy, Pt};
use crate::state::events::{write_result, EventWriter};
use crate::state::movement::{Journey, MovementRow, Point};
use crate::state::staff_operations::NAME as STAFF_OPERATIONS;
use crate::Unsupported;

pub(crate) const KEY: &str = "movement_v1";

pub(crate) fn refused(reason: &'static str) -> PyErr {
    Unsupported::new_err(reason)
}

// ---- What the world holds --------------------------------------------------------------------

/// The mirror of a namespace the world really holds: Python stores past the table are not the row's.
pub(crate) fn mirror_held<'py>(world: &Bound<'py, PyAny>) -> PyResult<Bound<'py, NamespaceMirror>> {
    let py = world.py();
    let mirror = mirror_of(world, KEY)?;
    let table = attribute(world, intern!(py, "model_state"))?;
    let held = exact_dict(&table)?.get_item(KEY)?;
    if !held.is_some_and(|state| mirror.borrow().holds_state(&state)) {
        return unsupported("the world holds another movement state");
    }
    Ok(mirror)
}

/// Whether the world holds `movement_v1`, as a kernel of the core takes it.
fn movement_held(world: &Bound<'_, PyAny>) -> PyResult<Held> {
    namespace_held(world, intern!(world.py(), KEY), "the world holds another movement state")
}

fn queues_held(world: &Bound<'_, PyAny>) -> PyResult<Held> {
    crate::state::facility_queues::held(world)
}

/// Whether `model_state` has `movement_v1`.
fn has_movement(world: &Bound<'_, PyAny>) -> PyResult<bool> {
    let table = attribute(world, intern!(world.py(), "model_state"))?;
    exact_dict(&table)?.contains(KEY)
}

/// Whether the world holds the namespace detached, so that a rule reading its tree reads the
/// row instead (`journey_of`, `row_position`).
pub(crate) fn held_detached(world: &Bound<'_, PyAny>) -> PyResult<bool> {
    let table = attribute(world, intern!(world.py(), "model_state"))?;
    match crate::core::tracked::as_readable_dict(&table) {
        Some(table) => Ok(table.get_item(KEY)?.is_some_and(|held| crate::core::tracked::is_detached(&held))),
        None => Ok(false),
    }
}

/// `state["positions"].get(agent_id)` of a detached namespace, as a new document of the row's
/// record (read it, never write).
pub(crate) fn row_position<'py>(world: &Bound<'py, PyAny>, agent_id: &str) -> PyResult<Option<Bound<'py, PyAny>>> {
    let py = world.py();
    NamespaceMirror::with_row::<MovementRow, _>(&mirror_held(world)?, |row| {
        row.positions.get(agent_id).map(|position| position.to_py(py)).transpose()
    })
}

/// The staff's operations as `staff_position_snapshots` reads them: the row of
/// `staff_operations_v1` where it answers for the namespace (held, and not tainted), else what the
/// world holds under the key, read as the kernel read the tree: no operations unless it is a dict,
/// its `staff` an exact dict, and each record a dict or None.
fn staff_ops(world: &Bound<'_, PyAny>) -> PyResult<StaffOps> {
    let py = world.py();
    let held = namespace_held(world, intern!(py, STAFF_OPERATIONS), "the world holds another staff state")?;
    match held {
        Held::Absent => return Ok(StaffOps::None),
        Held::Held => {
            let mirror = mirror_of(world, STAFF_OPERATIONS)?;
            let answers = NamespaceMirror::with_row::<crate::state::staff_operations::StaffOperationsRow, _>(&mirror, |_| Ok(())).is_ok();
            if answers {
                return Ok(StaffOps::Row);
            }
        }
        Held::Other(_) => {}
    }
    let table = attribute(world, intern!(py, "model_state"))?;
    let Some(operations) = exact_dict(&table)?.get_item(STAFF_OPERATIONS)? else {
        return Ok(StaffOps::None);
    };
    let Some(operations) = crate::core::tracked::as_readable_dict(&operations) else {
        return Ok(StaffOps::None);
    };
    let Some(records) = operations.get_item(intern!(py, "staff"))? else {
        return Ok(StaffOps::None);
    };
    let records = exact_dict(&records)?;
    let mut ops = IdMap::default();
    for (staff_id, record) in records.iter() {
        let Ok(staff_id) = staff_id.downcast_exact::<PyString>() else {
            continue;
        };
        let op = staff_op(&record)?;
        ops.insert(Arc::from(staff_id.to_str()?), op);
    }
    Ok(StaffOps::Tree(ops))
}

/// One record of a staff operations tree, as the kernel reads it: travelling only with the str
/// `"travel"` as its phase; the fields of a travelling one, or the refusal of the first that is not
/// what the kernel reads.
fn staff_op(record: &Bound<'_, PyAny>) -> PyResult<StaffOp> {
    let py = record.py();
    let Some(operation) = crate::core::tracked::as_readable_dict(record) else {
        return Ok(if record.is_none() { StaffOp::NotTravelling } else { StaffOp::Refused("a staff operation is not a plain dict") });
    };
    let travelling = match operation.get_item(intern!(py, "phase"))? {
        Some(phase) => phase.downcast_exact::<PyString>().is_ok_and(|phase| phase.to_str().is_ok_and(|text| text == "travel")),
        None => false,
    };
    if !travelling {
        return Ok(StaffOp::NotTravelling);
    }
    let read = || -> PyResult<StaffTravel> {
        let required = |name| -> PyResult<Bound<'_, PyAny>> {
            operation.get_item(name)?.ok_or_else(|| Unsupported::new_err("a staff operation lacks a field"))
        };
        let travel_minutes = exact_int(&required(intern!(py, "travel_minutes"))?)?;
        let started_minute = exact_int(&required(intern!(py, "started_minute"))?)?;
        let edges = required(intern!(py, "route_edge_ids"))?;
        let zones = required(intern!(py, "route_zone_ids"))?;
        let ids = |list: &Bound<'_, PyAny>| -> PyResult<Vec<Option<Arc<str>>>> {
            Ok(exact_list(list)?.iter().map(|id| exact_str(&id).ok().map(Arc::from)).collect())
        };
        let route_edge_ids = ids(&edges)?;
        Ok(StaffTravel { travel_minutes, started_minute, route_edge_ids, route_zone_ids: ids(&zones)? })
    };
    match read() {
        Ok(travel) => Ok(StaffOp::Travel(travel)),
        Err(error) if error.is_instance_of::<Unsupported>(py) => {
            let reason = error.value(py).str()?.to_string();
            Ok(StaffOp::Refused(refusal_text(&reason)))
        }
        Err(error) => Err(error),
    }
}

/// The static text of a refusal of the reads in `staff_op`.
fn refusal_text(reason: &str) -> &'static str {
    const TEXTS: [&str; 6] = [
        "a staff operation lacks a field",
        "a value is not an int",
        "an int is too large",
        "a value is not a plain list",
        "a value is not a str",
        "a str is not UTF-8",
    ];
    TEXTS.into_iter().find(|text| *text == reason).unwrap_or("a staff operation is not what the kernel reads")
}

// ---- Python values ---------------------------------------------------------------------------

/// `Point2D.model_validate` of a plain record: only the case where it gives the same two floats.
fn plain_point(value: &Bound<'_, PyAny>) -> PyResult<Pt> {
    let py = value.py();
    let record = exact_dict(value)?;
    if record.len() != 2 {
        return unsupported("a point has other fields");
    }
    let field = |name| -> PyResult<f64> {
        match record.get_item(name)? {
            Some(item) => exact_float(&item),
            None => unsupported("a point lacks a field"),
        }
    };
    let (x, y) = (field(intern!(py, "x"))?, field(intern!(py, "y"))?);
    if !usable(x, y) {
        return unsupported("a point is not a usable point");
    }
    Ok(Pt { x, y })
}

/// `Point2D(x=..., y=...)`.
pub(crate) fn point_object<'py>(py: Python<'py>, point: Pt) -> PyResult<Bound<'py, PyAny>> {
    static CLASS: GILOnceCell<Py<PyAny>> = GILOnceCell::new();
    let class = CLASS.get_or_try_init(py, || -> PyResult<Py<PyAny>> {
        Ok(py.import("sheltersim.contracts.extensions")?.getattr("Point2D")?.unbind())
    })?;
    let fields = PyDict::new(py);
    fields.set_item(intern!(py, "x"), point.x)?;
    fields.set_item(intern!(py, "y"), point.y)?;
    class.bind(py).call((), Some(&fields))
}

/// The `Point2D` a kernel was given, or None for None.
fn given_point(value: &Bound<'_, PyAny>) -> PyResult<Option<Pt>> {
    if value.is_none() {
        return Ok(None);
    }
    let py = value.py();
    if value.get_type().name()?.to_str()? != "Point2D" {
        return unsupported("a point is not a Point2D");
    }
    let (x, y) = (exact_float(&attribute(value, intern!(py, "x"))?)?, exact_float(&attribute(value, intern!(py, "y"))?)?);
    if !usable(x, y) {
        return unsupported("a point is not a usable point");
    }
    Ok(Some(Pt { x, y }))
}

/// A mobility as the Python rules pass it, the member or its value.
fn mobility_of(value: &Bound<'_, PyAny>) -> PyResult<&'static str> {
    let text = match value.downcast_exact::<PyString>() {
        Ok(text) => text.to_str().map(str::to_owned),
        Err(_) => Ok(exact_str(&attribute(value, intern!(value.py(), "value"))?)?.to_owned()),
    }
    .map_err(|_: PyErr| Unsupported::new_err("a mobility is not a str"))?;
    match &*text {
        "independent" => Ok("independent"),
        "assisted" => Ok("assisted"),
        "wheelchair" => Ok("wheelchair"),
        _ => unsupported("another mobility"),
    }
}

fn zone_route_class(py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
    static CLASS: GILOnceCell<Py<PyAny>> = GILOnceCell::new();
    let class = CLASS.get_or_try_init(py, || -> PyResult<Py<PyAny>> {
        Ok(py.import("sheltersim.policies.space")?.getattr("ZoneRoute")?.unbind())
    })?;
    Ok(class.bind(py).clone())
}

fn points_tuple<'py>(py: Python<'py>, points: &[Pt]) -> PyResult<Bound<'py, PyTuple>> {
    let points = points.iter().map(|point| point_object(py, *point)).collect::<PyResult<Vec<_>>>()?;
    PyTuple::new(py, points)
}

pub(crate) fn optional_str(value: Option<&Bound<'_, PyAny>>) -> PyResult<Option<Arc<str>>> {
    match value {
        Some(value) if !value.is_none() => Ok(Some(Arc::from(exact_str(value)?))),
        _ => Ok(None),
    }
}

pub(crate) fn str_tuple(value: &Bound<'_, PyAny>) -> PyResult<Vec<Arc<str>>> {
    value
        .downcast_exact::<PyTuple>()
        .map_err(|_| Unsupported::new_err("not a tuple"))?
        .iter()
        .map(|item| Ok(Arc::from(exact_str(&item)?)))
        .collect()
}

/// The points of a route: `Point2D` itself, so `model_dump` would give the same two floats.
pub(crate) fn route_points_of(value: Option<&Bound<'_, PyAny>>) -> PyResult<Vec<Pt>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let points = value.downcast_exact::<PyTuple>().map_err(|_| Unsupported::new_err("the points are not a tuple"))?;
    let mut found = Vec::with_capacity(points.len());
    for point in points.iter() {
        let py = point.py();
        if point.get_type().name()?.to_str()? != "Point2D" {
            return unsupported("a route point is not a Point2D");
        }
        let (x, y) = (exact_float(&attribute(&point, intern!(py, "x"))?)?, exact_float(&attribute(&point, intern!(py, "y"))?)?);
        if !usable(x, y) {
            return unsupported("a route point is not a usable point");
        }
        found.push(Pt { x, y });
    }
    Ok(found)
}

// ---- The movement section ----------------------------------------------------------------------

/// The movement section of the world's configs (`config::movement`), kept for the stamp of
/// `model_configs`. A configs that is not a dict is refused here, as `exact_dict` refused it; the
/// model's own refusal is said where a walk is timed (`room_minutes`, `movement_minutes`).
#[track_caller]
pub(crate) fn movement_of(world: &Bound<'_, PyAny>) -> PyResult<Arc<Movement>> {
    configs::kept_value(world, |configs| &configs.movement, Movement::parse)
}

// ---- Writes of the kernels not yet in the core (admission) -------------------------------------

/// `state["positions"][agent_id].update(zone_id=..., point=point, edge_id=None, progress=1.0,
/// position_kind="physical")` on the row of a detached namespace, which has no tree to update:
/// None, having written nothing, where the namespace is not held detached; Some(false) where the
/// row has no record of the person, and Some(true) once written.
pub(crate) fn place_at_point(world: &Bound<'_, PyAny>, agent_id: &str, zone_id: &Arc<str>, point: Pt) -> PyResult<Option<bool>> {
    if !held_detached(world)? {
        return Ok(None);
    }
    mirror_held(world)?;
    let placed = with_world(world, |world| core::place_at_point(world, agent_id, zone_id, point))?.or_py()?;
    Ok(Some(placed))
}

/// `place_at_point` of a bed's point as Python holds it: false, having written nothing, unless
/// the namespace is held detached, the row has the record and the point is two usable floats.
/// The caller then writes through the tree as before.
pub(crate) fn place_at_bed(world: &Bound<'_, PyAny>, agent_id: &str, zone_id: &Arc<str>, point: &Bound<'_, PyAny>) -> PyResult<bool> {
    if !held_detached(world)? {
        return Ok(false);
    }
    let Ok(point) = plain_point(point) else {
        return Ok(false);
    };
    Ok(place_at_point(world, agent_id, zone_id, point)? == Some(true))
}

// ---- Where a person is -------------------------------------------------------------------------

/// What the readers of a person's place take: `queues` only where a queue ID is given.
fn where_of<'a>(world: &Bound<'_, PyAny>, geo: &'a Geo, queue_id: Option<&str>) -> PyResult<Where<'a>> {
    let queues = if queue_id.is_some() { queues_held(world)? } else { Held::Absent };
    Ok(Where { geo, movement: movement_held(world)?, queues })
}

/// `agent_is_at_queue_slot`.
pub(crate) fn at_queue_slot(world: &Bound<'_, PyAny>, agent_id: &str, facility_id: &str, queue_id: Option<&str>) -> PyResult<bool> {
    let geo = Geo::of(world)?;
    let at = where_of(world, &geo, queue_id)?;
    with_world(world, |world| core::at_queue_slot(world, &at, agent_id, facility_id, queue_id))?.or_py()
}

/// `agent_is_at_facility_service_point`.
pub(crate) fn at_service_point(world: &Bound<'_, PyAny>, agent_id: &str, facility_id: &str, service_point: Option<&str>) -> PyResult<bool> {
    let geo = Geo::of(world)?;
    if geo.layout.is_none() {
        // The route of no floor plan: none of its minutes.
        return with_world(world, |world| core::agent_and_facility(world, agent_id, facility_id))?.or_py().map(|_| true);
    }
    let movement = movement_of(world)?;
    let at = where_of(world, &geo, None)?;
    with_world(world, |world| core::at_service_point(world, &at, &movement.model, agent_id, facility_id, service_point))?.or_py()
}

/// `plan_in_room_route`.
#[pyfunction]
#[pyo3(signature = (world, *, zone_id, start, destination, mobility=None))]
fn plan_in_room_route<'py>(
    world: &Bound<'py, PyAny>,
    zone_id: &Bound<'py, PyAny>,
    start: &Bound<'py, PyAny>,
    destination: &Bound<'py, PyAny>,
    mobility: Option<&Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = world.py();
    let zone_id = exact_str(zone_id)?;
    let start = given_point(start)?;
    let Some(destination) = given_point(destination)? else {
        return unsupported("no destination");
    };
    let mobility = match mobility {
        Some(mobility) => mobility_of(mobility)?,
        None => "independent",
    };
    let geo = Geo::of(world)?;
    match geo.room_route(zone_id, start, destination, mobility).map_err(refused)? {
        Some(route) => Ok(points_tuple(py, &route)?.into_any()),
        None => Ok(py.None().into_bound(py)),
    }
}

/// `route_is_physically_clear`.
#[pyfunction]
#[pyo3(signature = (world, edge_id, *, mobility=None))]
fn route_is_physically_clear(
    world: &Bound<'_, PyAny>,
    edge_id: &Bound<'_, PyAny>,
    mobility: Option<&Bound<'_, PyAny>>,
) -> PyResult<bool> {
    // The member or its value; the Python rule looks either up in a table of strs.
    let member_value;
    let mobility = match mobility {
        Some(mobility) => match mobility.downcast_exact::<PyString>() {
            Ok(text) => text.to_str().map_err(|_| Unsupported::new_err("a str is not UTF-8"))?,
            Err(_) => {
                member_value = attribute(mobility, intern!(mobility.py(), "value"))?;
                exact_str(&member_value)?
            }
        },
        None => "independent",
    };
    Ok(Geo::of(world)?.route_is_clear(exact_str(edge_id)?, mobility))
}

/// `_zone_center` of `_geometry(world)`, for the tests' comparison with the Python rule.
#[pyfunction]
fn zone_center(world: &Bound<'_, PyAny>, zone_id: &str) -> PyResult<Option<(f64, f64)>> {
    Ok(Geo::of(world)?.zone_center(zone_id)?.map(|point| (point.x, point.y)))
}

/// `_queue_slot_position`.
#[pyfunction]
#[pyo3(signature = (world, geometry, agent_id, facility_id, queue_id=None))]
fn queue_slot_position<'py>(
    world: &Bound<'py, PyAny>,
    geometry: &Bound<'py, PyAny>,
    agent_id: &Bound<'py, PyAny>,
    facility_id: &Bound<'py, PyAny>,
    queue_id: Option<&Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = world.py();
    let _ = geometry; // What _geometry(world) gives, as every caller passes.
    let (agent_id, facility_id) = (exact_str(agent_id)?, exact_str(facility_id)?);
    let geo = Geo::of(world)?;
    let queue = queue_id.filter(|queue| !queue.is_none()).map(exact_str).transpose()?;
    let at = where_of(world, &geo, queue)?;
    match with_world(world, |world| core::queue_slot_position(world, &at, agent_id, facility_id, queue))?.or_py()? {
        Some(point) => point_object(py, point),
        None => Ok(py.None().into_bound(py)),
    }
}

#[pyfunction]
#[pyo3(signature = (world, agent_id, facility_id, queue_id=None))]
fn agent_is_at_queue_slot(world: &Bound<'_, PyAny>, agent_id: &Bound<'_, PyAny>, facility_id: &Bound<'_, PyAny>, queue_id: Option<&Bound<'_, PyAny>>) -> PyResult<bool> {
    let queue = queue_id.filter(|queue| !queue.is_none()).map(exact_str).transpose()?;
    at_queue_slot(world, exact_str(agent_id)?, exact_str(facility_id)?, queue)
}

#[pyfunction]
#[pyo3(signature = (world, agent_id, facility_id, service_point_id=None))]
fn agent_is_at_facility_service_point(
    world: &Bound<'_, PyAny>,
    agent_id: &Bound<'_, PyAny>,
    facility_id: &Bound<'_, PyAny>,
    service_point_id: Option<&Bound<'_, PyAny>>,
) -> PyResult<bool> {
    let point = service_point_id.filter(|point| !point.is_none()).map(exact_str).transpose()?;
    at_service_point(world, exact_str(agent_id)?, exact_str(facility_id)?, point)
}

/// `agent_is_at_queue_trip_destination`, of the journey it is given.
#[pyfunction]
fn agent_is_at_queue_trip_destination(world: &Bound<'_, PyAny>, agent_id: &Bound<'_, PyAny>, journey: &Bound<'_, PyAny>) -> PyResult<bool> {
    let Some(journey) = crate::core::tracked::as_readable_dict(journey) else {
        return if journey.is_instance_of::<PyDict>() { unsupported("another kind of dict") } else { Ok(false) };
    };
    let Some(points) = journey.get_item(intern!(world.py(), "route_points"))? else {
        return Ok(false);
    };
    let points = exact_list(&points)?;
    if points.is_empty() {
        return Ok(false);
    }
    let destination = plain_point(&points.get_item(points.len() - 1)?)?;
    let agent_id = exact_str(agent_id)?;
    let movement = movement_held(world)?;
    with_world(world, |world| core::near_point(world, &movement, agent_id, destination))?.or_py()
}

/// `facility_approach_route`.
#[pyfunction]
#[pyo3(signature = (world, agent_id, facility_id, service_point_id=None))]
fn facility_approach_route<'py>(
    world: &Bound<'py, PyAny>,
    agent_id: &Bound<'py, PyAny>,
    facility_id: &Bound<'py, PyAny>,
    service_point_id: Option<&Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = world.py();
    let service_point = match service_point_id {
        Some(point) if !point.is_none() => Some(exact_str(point)?),
        _ => None,
    };
    let (agent_id, facility_id) = (exact_str(agent_id)?, exact_str(facility_id)?);
    let geo = Geo::of(world)?;
    let movement = movement_of(world)?;
    let (agent_zone, facility_zone) = with_world(world, |world| core::agent_and_facility(world, agent_id, facility_id))?.or_py()?;
    let route_class = zone_route_class(py)?;
    if geo.layout.is_none() {
        let route = route_class.call1((&*agent_zone, &*facility_zone, (&*agent_zone,), PyTuple::empty(py), 0))?;
        return Ok((route, PyTuple::empty(py)).into_pyobject(py)?.into_any());
    }
    let at = where_of(world, &geo, None)?;
    let found = with_world(world, |world| core::approach_route(world, &at, &movement.model, agent_id, facility_id, service_point))?.or_py()?;
    let Some((duration, path)) = found else {
        return Ok(py.None().into_bound(py));
    };
    let route = route_class.call1((&*facility_zone, &*facility_zone, (&*facility_zone, &*facility_zone), PyTuple::empty(py), duration))?;
    Ok((route, points_tuple(py, &path)?).into_pyobject(py)?.into_any())
}

// ---- The kernels of a tick ---------------------------------------------------------------------

/// `staff_position_snapshots`, from outside `advance_movement`.
#[pyfunction]
#[pyo3(signature = (world, *, boundary_minute=None))]
fn staff_position_snapshots<'py>(world: &Bound<'py, PyAny>, boundary_minute: Option<&Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyDict>> {
    let py = world.py();
    let result = PyDict::new(py);
    if !has_movement(world)? {
        return Ok(result);
    }
    let boundary = match boundary_minute.filter(|boundary| !boundary.is_none()) {
        Some(boundary) => exact_int(boundary)?,
        None => exact_int(&attribute(world, intern!(py, "minute_index"))?)?,
    };
    mirror_held(world)?;
    let geo = Geo::of(world)?;
    let staff = staff_ops(world)?;
    let records = with_world(world, |world| core::staff_position_snapshots(world, &geo, &staff, boundary))?.or_py()?;
    for (id, position) in &records {
        result.set_item(&**id, PyField::to_py(position, py)?)?;
    }
    Ok(result)
}

/// `advance_movement`.
#[pyfunction]
pub(crate) fn advance_movement<'py>(world: &Bound<'py, PyAny>, minute: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyTuple>> {
    let py = world.py();
    let minute = exact_int(minute)?;
    if minute.abs() >= EXACT_INT {
        return unsupported("a minute is too large");
    }
    mirror_held(world)?;
    let geo = Geo::of(world)?;
    let staff = staff_ops(world)?;
    let tick = Tick { geo: &geo, staff: &staff };
    let mut advanced = with_world(world, |world| core::advance(world, &tick, minute, None))?.or_py()?;
    if matches!(advanced, Advanced::NeedsInOrder) {
        // A branch reads the walk so far: what it needs, read as the kernel read it then.
        let movement = movement_of(world)?;
        let in_order = InOrder {
            model: &movement.model,
            queues: queues_held(world)?,
            metric_state_held: field_held(world, intern!(py, "metric_state"))?,
        };
        advanced = with_world(world, |world| core::advance(world, &tick, minute, Some(&in_order)))?.or_py()?;
    }
    let replans = match advanced {
        Advanced::Done(replans) => replans,
        Advanced::NeedsInOrder => return unsupported("the walk in order asked for itself"),
        Advanced::Planned(plan) => {
            // The old events are read as rows from the world's queue, and settled there by the core.
            crate::rules::travel::read_queue_rows(world)?;
            let queue = EventWriter::of(world)?.queue().clone();
            let (replans, taken) = write_result(py, with_world(world, |world| core::write_walk(world, plan, minute))?)?;
            queue.get().forget(&taken)?;
            replans
        }
    };
    PyTuple::new(py, replans.iter().map(|id| PyString::new(py, id)))
}

/// `start_movement`.
#[pyfunction]
#[pyo3(signature = (
    world, *, agent_id, route, purpose, facility_id, started_minute, due_minute, event_id, task_id,
    route_points=None, queue_id=None, support_request_id=None, support_staff_id=None
))]
#[allow(clippy::too_many_arguments)]
pub(crate) fn start_movement<'py>(
    world: &Bound<'py, PyAny>,
    agent_id: &Bound<'py, PyAny>,
    route: &Bound<'py, PyAny>,
    purpose: &Bound<'py, PyAny>,
    facility_id: &Bound<'py, PyAny>,
    started_minute: &Bound<'py, PyAny>,
    due_minute: &Bound<'py, PyAny>,
    event_id: &Bound<'py, PyAny>,
    task_id: &Bound<'py, PyAny>,
    route_points: Option<&Bound<'py, PyAny>>,
    queue_id: Option<&Bound<'py, PyAny>>,
    support_request_id: Option<&Bound<'py, PyAny>>,
    support_staff_id: Option<&Bound<'py, PyAny>>,
) -> PyResult<()> {
    let py = world.py();
    mirror_held(world)?;
    let started = exact_int(started_minute)?;
    let due = exact_int(due_minute)?;
    let duration = due.checked_sub(started).filter(|duration| *duration > 0 && *duration < EXACT_INT);
    let Some(duration) = duration else {
        return unsupported("a journey has no plain duration");
    };
    let origin: Arc<str> = Arc::from(exact_str(&attribute(route, intern!(py, "origin_zone_id"))?)?);
    let zone_ids = str_tuple(&attribute(route, intern!(py, "zone_ids"))?)?;
    let edge_ids = str_tuple(&attribute(route, intern!(py, "edge_ids"))?)?;
    let points = route_points_of(route_points)?;
    let agent_id: Arc<str> = Arc::from(exact_str(agent_id)?);
    let support = match (optional_str(support_request_id)?, optional_str(support_staff_id)?) {
        (Some(request), Some(staff)) => (Some(request), Some(staff)),
        _ => (None, None),
    };
    let journey = Journey {
        event_id: Arc::from(exact_str(event_id)?),
        task_id: Arc::from(exact_str(task_id)?),
        purpose: Arc::from(exact_str(purpose)?),
        facility_id: optional_str(Some(facility_id))?,
        destination_zone_id: Arc::from(exact_str(&attribute(route, intern!(py, "destination_zone_id"))?)?),
        route_zone_ids: zone_ids,
        route_edge_ids: edge_ids.clone(),
        route_points: points.iter().map(|point| Point { x: point.x, y: point.y }).collect(),
        started_minute: started,
        due_minute: due,
        queue_id: optional_str(queue_id)?,
        support_request_id: support.0,
        support_staff_id: support.1,
    };
    let geo = Geo::of(world)?;
    let start = Start { agent_id: &agent_id, journey, origin: &origin, points: &points, edge_ids: &edge_ids, started, duration };
    with_world(world, |world| core::start(world, &geo, start))?.or_py()
}

/// `finish_movement`.
#[pyfunction]
pub(crate) fn finish_movement<'py>(world: &Bound<'py, PyAny>, agent_id: &Bound<'py, PyAny>) -> PyResult<()> {
    if !has_movement(world)? {
        return Ok(());
    }
    mirror_held(world)?;
    let agent_id = exact_str(agent_id)?;
    let geo = Geo::of(world)?;
    with_world(world, |world| core::finish(world, &geo, agent_id))?.or_py()
}

// ---- route_with_movement_time ------------------------------------------------------------------

/// `route_with_movement_time`.
#[pyfunction]
pub(crate) fn route_with_movement_time<'py>(
    world: &Bound<'py, PyAny>,
    agent_id: &Bound<'py, PyAny>,
    route: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = world.py();
    let class: Bound<'py, PyType> = route.get_type();
    let edge_ids = attribute(route, intern!(py, "edge_ids"))?;
    let zone_ids = attribute(route, intern!(py, "zone_ids"))?;
    if !movement_of(world)?.times_routes() {
        return Ok(route.clone());
    }
    if class.name()?.to_str()? != "ZoneRoute" {
        return unsupported("the route is not a ZoneRoute");
    }
    let edges = edge_ids.downcast_exact::<PyTuple>().map_err(|_| Unsupported::new_err("not a tuple"))?;
    let zones = zone_ids.downcast_exact::<PyTuple>().map_err(|_| Unsupported::new_err("not a tuple"))?;
    let declared = exact_int(&attribute(route, intern!(py, "travel_minutes"))?)?;
    let zone_items: Vec<Bound<'py, PyAny>> = zones.iter().collect();
    let zone_texts = zone_items.iter().map(exact_str).collect::<PyResult<Vec<_>>>()?;
    let edge_items: Vec<Bound<'py, PyAny>> = edges.iter().collect();
    let edge_texts = edge_items.iter().map(exact_str).collect::<PyResult<Vec<_>>>()?;
    let minutes = movement_minutes(world, exact_str(agent_id)?, &zone_texts, &edge_texts, declared)?;
    if edges.is_empty() {
        return Ok(route.clone());
    }
    class.call1((
        attribute(route, intern!(py, "origin_zone_id"))?,
        attribute(route, intern!(py, "destination_zone_id"))?,
        zones,
        edges,
        minutes,
    ))
}

/// The minutes `route_with_movement_time` gives a route of these zones and edges.
pub(crate) fn movement_minutes(world: &Bound<'_, PyAny>, agent_id: &str, zones: &[&str], edges: &[&str], declared: i64) -> PyResult<i64> {
    let movement = movement_of(world)?;
    if !movement.times_routes() {
        return Ok(declared);
    }
    let model = configs::read(movement.model.clone())?;
    if declared.abs() >= EXACT_INT {
        return unsupported("the route's minutes are too large");
    }
    let geo = Geo::of(world)?;
    with_world(world, |world| core::movement_minutes(world, &geo, &model, agent_id, zones, edges, declared))?.or_py()
}

// ---- validate ----------------------------------------------------------------------------------

/// `validate_movement_runtime_state`: accepts, or leaves it to the Python rule and its error text.
#[pyfunction]
fn validate(world: &Bound<'_, PyAny>) -> PyResult<bool> {
    let py = world.py();
    let table = attribute(world, intern!(py, "model_state"))?;
    let Some(held) = exact_dict(&table)?.get_item(KEY)? else {
        return Ok(true);
    };
    if held.is_none() {
        return Ok(true);
    }
    mirror_held(world)?;
    let queues = queues_held(world)?;
    let mut found = with_world(world, |world| core::validate(world, &queues, None))?.or_py()?;
    if found == Validated::NeedsGeo {
        // Only journeys in rooms need the floor plan, whose read may run Python.
        let geo = Geo::of(world)?;
        found = with_world(world, |world| core::validate(world, &queues, Some(&geo)))?.or_py()?;
    }
    match found {
        Validated::Is(valid) => Ok(valid),
        Validated::NeedsGeo => unsupported("the floor plan was asked for twice"),
    }
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(advance_movement, m)?)?;
    m.add_function(wrap_pyfunction!(start_movement, m)?)?;
    m.add_function(wrap_pyfunction!(finish_movement, m)?)?;
    m.add_function(wrap_pyfunction!(route_with_movement_time, m)?)?;
    m.add_function(wrap_pyfunction!(plan_in_room_route, m)?)?;
    m.add_function(wrap_pyfunction!(route_is_physically_clear, m)?)?;
    m.add_function(wrap_pyfunction!(zone_center, m)?)?;
    m.add_function(wrap_pyfunction!(queue_slot_position, m)?)?;
    m.add_function(wrap_pyfunction!(facility_approach_route, m)?)?;
    m.add_function(wrap_pyfunction!(staff_position_snapshots, m)?)?;
    m.add_function(wrap_pyfunction!(agent_is_at_queue_slot, m)?)?;
    m.add_function(wrap_pyfunction!(agent_is_at_queue_trip_destination, m)?)?;
    m.add_function(wrap_pyfunction!(agent_is_at_facility_service_point, m)?)?;
    m.add_function(wrap_pyfunction!(validate, m)?)?;
    Ok(())
}
