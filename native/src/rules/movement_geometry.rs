//! The floor plan of a world for the movement rules: the core's geometry
//! (`sheltersim_core::geometry`), and what reads it from Python (Issue #152, lane 7-2).
//!
//! `Geo::of` gives what `_geometry` and `_spatial_constraints` give a Python rule. The configs'
//! section `geometry` (`config::geometry`) holds `facility_geometry` and `spatial_constraints`
//! parsed by their Python contracts and read into the core's values, once for a stamp of
//! `model_configs`. The plans built from them with the overlay's facility moves and partitions are
//! kept in those configs, by the overlay's stamp (`GeoCache`); where the overlay has no stamp or a
//! new one, by the canonical bytes of the two parts a plan reads, as before #152. A bad config or
//! overlay is Unsupported, and the Python rule then raises its error.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::sync::GILOnceCell;
use pyo3::types::{PyBool, PyString, PyTuple};
pub(crate) use sheltersim_core::config::ApproachRoute;
use sheltersim_core::config::{Configs, Fault, FacilityGeometry, FacilityPlan, Section, SpatialConstraints};
use sheltersim_core::geometry::{OverlayKey, Opening, Route, Wall, Zone};

pub(crate) use sheltersim_core::geometry::*;

use crate::core::configs::{self, fault_of};
use crate::core::errors::OrPy;
use crate::core::row::{attribute, exact_dict, exact_float, exact_int, exact_str, unsupported, IdMap};
use crate::core::tracked::as_readable_dict;
use crate::core::value::V;
use crate::core::world_core::{field_stamp, with_state_value};
use crate::Unsupported;

const OVERLAY_KEY: &str = "geometry_overlay_v1";

/// `point_in_value` of the core, for a rule.
pub(crate) fn point_in_value(value: &V) -> PyResult<Pt> {
    sheltersim_core::geometry::point_in_value(value).or_py()
}

/// `points_in_value` of the core, for a rule.
pub(crate) fn points_in_value(value: &V) -> PyResult<Vec<Pt>> {
    sheltersim_core::geometry::points_in_value(value).or_py()
}

// ---- The section `geometry`: the parsed models, read ---------------------------------------------

fn point_of(model: &Bound<'_, PyAny>) -> PyResult<Pt> {
    let py = model.py();
    let x = exact_float(&attribute(model, intern!(py, "x"))?)?;
    let y = exact_float(&attribute(model, intern!(py, "y"))?)?;
    if !usable(x, y) {
        return unsupported("a coordinate is too large");
    }
    Ok(Pt { x, y })
}

fn tuple_of<'py>(model: &Bound<'py, PyAny>, name: &Bound<'py, PyString>) -> PyResult<Bound<'py, PyTuple>> {
    attribute(model, name)?.downcast_into_exact::<PyTuple>().map_err(|_| Unsupported::new_err("not a tuple"))
}

fn points_of(model: &Bound<'_, PyAny>, name: &Bound<'_, PyString>) -> PyResult<Vec<Pt>> {
    tuple_of(model, name)?.iter().map(|point| point_of(&point)).collect()
}

fn id_of(model: &Bound<'_, PyAny>, name: &Bound<'_, PyString>) -> PyResult<Arc<str>> {
    Ok(Arc::from(exact_str(&attribute(model, name)?)?))
}

fn flag_of(model: &Bound<'_, PyAny>, name: &Bound<'_, PyString>) -> PyResult<bool> {
    attribute(model, name)?
        .downcast_exact::<PyBool>()
        .map(|flag| flag.is_true())
        .map_err(|_| Unsupported::new_err("not a bool"))
}

fn id_list(model: &Bound<'_, PyAny>, name: &Bound<'_, PyString>) -> PyResult<Vec<Arc<str>>> {
    tuple_of(model, name)?.iter().map(|id| Ok(Arc::from(exact_str(&id)?))).collect()
}

fn service_point_of(model: &Bound<'_, PyAny>) -> PyResult<ServicePoint> {
    let py = model.py();
    let access = attribute(model, intern!(py, "access"))?;
    Ok(ServicePoint {
        id: id_of(model, intern!(py, "id"))?,
        facility_id: id_of(model, intern!(py, "facility_id"))?,
        point: point_of(&attribute(model, intern!(py, "point_m"))?)?,
        service_capacity: exact_int(&attribute(model, intern!(py, "service_capacity"))?)?,
        queue_id: id_of(model, intern!(py, "queue_id"))?,
        accessibility: id_of(&access, intern!(py, "accessibility"))?,
        required_access_tags: id_list(&access, intern!(py, "required_access_tags"))?,
        required_staff_skills: id_list(&access, intern!(py, "required_staff_skills"))?,
    })
}

/// The Scenario's plan of a parsed `FacilityGeometry`.
fn plan_of(model: &Bound<'_, PyAny>) -> PyResult<FacilityPlan> {
    let py = model.py();
    let mut plan = FacilityPlan {
        zones: IdMap::default(),
        routes: IdMap::default(),
        facility_points: IdMap::default(),
        service_points: Vec::new(),
    };
    for zone in tuple_of(model, intern!(py, "zones"))?.iter() {
        let polygon = points_of(&zone, intern!(py, "polygon"))?;
        if polygon.is_empty() {
            return unsupported("a zone has no polygon");
        }
        let usable_area = exact_float(&attribute(&zone, intern!(py, "usable_area_m2"))?)?;
        let center = zone_center(&polygon);
        // The first of a repeated ID, as `next(...)` finds; the model refuses a repeat anyway.
        plan.zones.entry(id_of(&zone, intern!(py, "zone_id"))?).or_insert(Zone { polygon, usable_area, center });
    }
    for route in tuple_of(model, intern!(py, "routes"))?.iter() {
        let points = points_of(&route, intern!(py, "points"))?;
        if points.is_empty() {
            return unsupported("a route has no points");
        }
        let width = exact_float(&attribute(&route, intern!(py, "width_m"))?)?;
        let accessible = flag_of(&route, intern!(py, "accessible"))?;
        plan.routes.entry(id_of(&route, intern!(py, "edge_id"))?).or_insert(Route { points, width, accessible });
    }
    let points = attribute(model, intern!(py, "facility_points"))?;
    for (id, point) in exact_dict(&points)?.iter() {
        plan.facility_points.insert(Arc::from(exact_str(&id)?), point_of(&point)?);
    }
    for point in tuple_of(model, intern!(py, "service_points"))?.iter() {
        plan.service_points.push(service_point_of(&point)?);
    }
    Ok(plan)
}

/// An `ArrivalApproachRoute`. `travel_minutes` is a property that may raise; it is kept as read.
fn approach_route_of(route: &Bound<'_, PyAny>) -> PyResult<ApproachRoute> {
    let py = route.py();
    let mut source_ids = Vec::new();
    for source in attribute(route, intern!(py, "source_ids"))?.try_iter()? {
        source_ids.push(Arc::from(exact_str(&source?)?));
    }
    let travel_minutes = attribute(route, intern!(py, "travel_minutes")).and_then(|minutes| exact_int(&minutes));
    Ok(ApproachRoute {
        route_id: id_of(route, intern!(py, "route_id"))?,
        destination_zone_id: id_of(route, intern!(py, "destination_zone_id"))?,
        source_ids,
        walking_speed: exact_float(&attribute(route, intern!(py, "walking_speed_m_per_min"))?)?,
        distance: exact_float(&attribute(route, intern!(py, "distance_m"))?)?,
        travel_minutes: travel_minutes.map_err(|error| fault_of(py, &error)),
    })
}

/// `independent_queues_required` of a parsed `FacilityGeometry`: it stops at the first point that
/// decides, as the rule did.
fn independent_of(model: &Bound<'_, PyAny>) -> PyResult<bool> {
    let py = model.py();
    let points = tuple_of(model, intern!(py, "service_points"))?;
    let mut facilities = std::collections::HashSet::with_capacity(points.len());
    for point in points.iter() {
        let facility = attribute(&point, intern!(py, "facility_id"))?;
        let facility = exact_str(&facility)?;
        let queue = attribute(&point, intern!(py, "queue_id"))?;
        if exact_str(&queue)? != facility || !facilities.insert(facility.to_owned()) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// What reading a part of a parsed model gave: the value, or the fault of what it raised.
fn part<T>(py: Python<'_>, read: PyResult<T>) -> Result<T, Fault> {
    read.map_err(|error| fault_of(py, &error))
}

struct Contracts {
    facility: Py<PyAny>,
    spatial: Py<PyAny>,
}

static CONTRACTS: GILOnceCell<Contracts> = GILOnceCell::new();

fn contracts(py: Python<'_>) -> PyResult<&Contracts> {
    CONTRACTS.get_or_try_init(py, || {
        Ok(Contracts {
            facility: py.import("sheltersim.contracts.extensions")?.getattr("FacilityGeometry")?.unbind(),
            spatial: py.import("sheltersim.contracts.spatial")?.getattr("SpatialConstraints")?.unbind(),
        })
    })
}

/// `model_configs.get(name)`: the configs a world holds are a plain dict, and anything else is
/// read through its `get`.
fn config_entry<'py>(raw_configs: &Bound<'py, PyAny>, name: &Bound<'py, PyString>) -> PyResult<Option<Bound<'py, PyAny>>> {
    match as_readable_dict(raw_configs) {
        Some(dict) => dict.get_item(name),
        None => {
            let value = raw_configs.call_method1(intern!(raw_configs.py(), "get"), (name,))?;
            Ok((!value.is_none()).then_some(value))
        }
    }
}

/// `FacilityGeometry.model_validate(raw)`: the parse every read site of the section relays.
fn parse_facility<'py>(raw: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    contracts(raw.py())?.facility.bind(raw.py()).call_method1(intern!(raw.py(), "model_validate"), (raw,))
}

/// The section `facility_geometry` of these configs, parsed at its first read. Absent where the
/// key is missing or None; a fault for what the parse raised. Reading the parts never raises here.
pub(crate) fn facility_geometry<'c>(configs: &'c Configs, raw_configs: &Bound<'_, PyAny>) -> &'c Section<FacilityGeometry> {
    let py = raw_configs.py();
    configs.facility_geometry.get_or_parse(|| {
        let raw = match config_entry(raw_configs, intern!(py, "facility_geometry")) {
            Ok(Some(raw)) if !raw.is_none() => raw,
            Ok(_) => return Section::Absent,
            Err(error) => return Section::Fault(fault_of(py, &error)),
        };
        match parse_facility(&raw) {
            Ok(model) => Section::Ok(Arc::new(FacilityGeometry {
                plan: part(py, plan_of(&model).map(Arc::new)),
                approach_routes: part(
                    py,
                    tuple_of(&model, intern!(py, "arrival_approach_routes"))
                        .and_then(|routes| routes.iter().map(|route| approach_route_of(&route)).collect::<PyResult<Vec<_>>>())
                        .map(Arc::from),
                ),
                independent_queues: part(py, independent_of(&model)),
            })),
            Err(error) => Section::Fault(fault_of(py, &error)),
        }
    })
}

/// The section `spatial_constraints` of these configs: `model_configs.get("spatial_constraints",
/// {})`, parsed at its first read.
fn spatial_constraints<'c>(configs: &'c Configs, raw_configs: &Bound<'_, PyAny>) -> &'c Section<SpatialConstraints> {
    let py = raw_configs.py();
    configs.spatial_constraints.get_or_parse(|| {
        let read = || -> PyResult<SpatialConstraints> {
            let raw = match config_entry(raw_configs, intern!(py, "spatial_constraints"))? {
                Some(raw) => raw,
                None => pyo3::types::PyDict::new(py).into_any(),
            };
            let model = contracts(py)?.spatial.bind(py).call_method1(intern!(py, "model_validate"), (&raw,))?;
            spatial_of(&model)
        };
        match read() {
            Ok(spatial) => Section::Ok(Arc::new(spatial)),
            Err(error) => Section::Fault(fault_of(py, &error)),
        }
    })
}

fn spatial_of(model: &Bound<'_, PyAny>) -> PyResult<SpatialConstraints> {
    let py = model.py();
    let mut spatial = SpatialConstraints::default();
    for item in tuple_of(model, intern!(py, "obstacles"))?.iter() {
        let polygon = points_of(&item, intern!(py, "polygon"))?;
        if polygon.is_empty() {
            return unsupported("an obstacle has no polygon");
        }
        spatial.obstacles.push(polygon);
    }
    for item in tuple_of(model, intern!(py, "equipment_footprints"))?.iter() {
        let polygon = points_of(&item, intern!(py, "polygon"))?;
        if polygon.is_empty() {
            return unsupported("a footprint has no polygon");
        }
        spatial.footprints.push((id_of(&item, intern!(py, "facility_id"))?, polygon));
    }
    for wall in tuple_of(model, intern!(py, "walls"))?.iter() {
        spatial.walls.push(Wall {
            id: id_of(&wall, intern!(py, "id"))?,
            start: point_of(&attribute(&wall, intern!(py, "start"))?)?,
            end: point_of(&attribute(&wall, intern!(py, "end"))?)?,
        });
    }
    for opening in tuple_of(model, intern!(py, "openings"))?.iter() {
        let edge_ids = tuple_of(&opening, intern!(py, "edge_ids"))?;
        spatial.openings.push(Opening {
            wall_id: id_of(&opening, intern!(py, "wall_id"))?,
            center: point_of(&attribute(&opening, intern!(py, "center"))?)?,
            width: exact_float(&attribute(&opening, intern!(py, "width_m"))?)?,
            accessible: flag_of(&opening, intern!(py, "accessible"))?,
            edge_ids: edge_ids.iter().map(|id| Ok(Arc::from(exact_str(&id)?))).collect::<PyResult<_>>()?,
        });
    }
    Ok(spatial)
}

/// A fault as the plan's reader meets it: Unsupported, with the reader's own reason, or, for what
/// the parse raised, the reason the Python rule raises it itself.
fn refusal(fault: &Fault) -> PyErr {
    if fault.is_refusal() {
        Unsupported::new_err(fault.text.to_string())
    } else {
        Unsupported::new_err("the Python rule's parse raises")
    }
}

// ---- The read sites ------------------------------------------------------------------------------

/// `parsed_facility_geometry(model_configs.get("facility_geometry", {})).arrival_approach_routes`.
/// What the parse raised is raised again by a second parse (E14); a route the reader refused is
/// Unsupported, as reading its attributes was.
pub(crate) fn approach_routes(world: &Bound<'_, PyAny>) -> PyResult<Arc<[ApproachRoute]>> {
    let py = world.py();
    let raw_configs = attribute(world, intern!(py, "model_configs"))?;
    let configs = configs::current_of(world, &raw_configs)?;
    let relay = || {
        configs::reraise(|| {
            let raw = raw_configs.call_method1(intern!(py, "get"), ("facility_geometry", pyo3::types::PyDict::new(py)))?;
            parse_facility(&raw)
        })
    };
    match facility_geometry(&configs, &raw_configs) {
        Section::Ok(geometry) => geometry.approach_routes.clone().map_err(|fault| refusal(&fault)),
        // A missing or None config: the parse of `{}` or None raises.
        Section::Absent | Section::Fault(_) => Err(relay()),
    }
}

/// `travel_minutes` of a route, Unsupported as reading it was where it raised.
pub(crate) fn travel_minutes(route: &ApproachRoute) -> PyResult<i64> {
    route.travel_minutes.clone().map_err(|fault| Unsupported::new_err(fault.text.to_string()))
}

/// `independent_queues_required(world.model_configs)`. False without a config dict; a config the
/// parse turns down raises the parse's own error, as the Python rule did.
pub(crate) fn independent_queues(world: &Bound<'_, PyAny>) -> PyResult<bool> {
    let py = world.py();
    let raw_configs = attribute(world, intern!(py, "model_configs"))?;
    let Some(raw) = exact_dict(&raw_configs)?.get_item(intern!(py, "facility_geometry"))? else {
        return Ok(false);
    };
    if as_readable_dict(&raw).is_none() {
        return Ok(false);
    }
    let configs = configs::current_of(world, &raw_configs)?;
    match facility_geometry(&configs, &raw_configs) {
        Section::Ok(geometry) => geometry.independent_queues.clone().map_err(|fault| refusal(&fault)),
        Section::Fault(_) => Err(crate::core::errors::relayed(py, configs::reraise(|| parse_facility(&raw)))),
        Section::Absent => unsupported("no facility geometry"),
    }
}

/// `independent_queues_required(configs)` of a configs dict no world holds: parsed each time.
pub(crate) fn independent_queues_of_configs(raw_configs: &Bound<'_, PyAny>) -> PyResult<bool> {
    let py = raw_configs.py();
    let Some(raw) = exact_dict(raw_configs)?.get_item(intern!(py, "facility_geometry"))? else {
        return Ok(false);
    };
    if as_readable_dict(&raw).is_none() {
        return Ok(false);
    }
    // `parsed_facility_geometry` shares the parse of a Python config scope.
    let model = py
        .import(intern!(py, "sheltersim.domain.geometry_overlay"))?
        .getattr(intern!(py, "parsed_facility_geometry"))?
        .call1((&raw,))
        .map_err(|error| crate::core::errors::relayed(py, error))?;
    independent_of(&model)
}

// ---- Which plan a world has now ------------------------------------------------------------------

/// Stamp hits whose overlay parts were not what the plan was built from, counted while
/// `configs::watch_config_writes` is on: a write to the overlay that gave it no new stamp.
static STALE_HITS: AtomicU64 = AtomicU64::new(0);

/// The stale plans found since `watch_config_writes(True)` (`STALE_HITS`).
#[pyfunction]
pub(crate) fn stale_geometry_hits() -> u64 {
    STALE_HITS.load(Ordering::Relaxed)
}

pub(crate) fn reset_stale_geometry_hits() {
    STALE_HITS.store(0, Ordering::Relaxed);
}

/// The bytes of a part of the overlay: empty for no entries, so most ticks encode nothing.
fn overlay_bytes(overlay: &indexmap::IndexMap<Arc<str>, V>, name: &str) -> PyResult<Vec<u8>> {
    match overlay.get(name) {
        None => Ok(Vec::new()),
        Some(V::Map(items)) if items.is_empty() => Ok(Vec::new()),
        Some(part @ V::Map(_)) => part.to_json().or_py(),
        Some(_) => unsupported("a value is not a plain dict"),
    }
}

/// The key of the overlay a world holds now, by bytes, with its stamp.
fn overlay_key(world: &Bound<'_, PyAny>, present: bool, stamp: Option<sheltersim_core::stamp::Stamp>) -> PyResult<OverlayKey> {
    if !present {
        return Ok(OverlayKey { present, stamp: None, moves: Vec::new(), partitions: Vec::new() });
    }
    // The overlay is read as a `V`: a detached one from its row.
    let (moves, partitions) = with_state_value(world, OVERLAY_KEY, |overlay| {
        let Some(V::Map(overlay)) = overlay else {
            return unsupported("a value is not a plain dict");
        };
        Ok((overlay_bytes(overlay, "facility_points")?, overlay_bytes(overlay, "partitions")?))
    })?;
    Ok(OverlayKey { present, stamp, moves, partitions })
}

/// Whether `model_state` holds the overlay, and the stamp of its slot where one follows it.
fn overlay_held(world: &Bound<'_, PyAny>) -> PyResult<(bool, Option<sheltersim_core::stamp::Stamp>)> {
    let py = world.py();
    let table = world
        .getattr(intern!(py, "model_state"))
        .map_err(|_| Unsupported::new_err("an input has no such attribute"))?;
    let dict = as_readable_dict(&table).ok_or_else(|| Unsupported::new_err("model_state is not a plain dict"))?;
    let Some(held) = dict.get_item(OVERLAY_KEY)? else {
        return Ok((false, None));
    };
    Ok((true, field_stamp(world, OVERLAY_KEY, &held)?.map(|(_, stamp)| stamp)))
}

/// `Geo::of` and the PyResult face of the core's `Geo`.
pub(crate) trait GeoPy {
    /// The plan of this world now. Unsupported for what the Python functions raise on.
    fn of(world: &Bound<'_, PyAny>) -> PyResult<Arc<Geo>>;
    /// `_zone_center`. Unsupported when Python would raise.
    fn zone_center(&self, zone_id: &str) -> PyResult<Option<Pt>>;
}

impl GeoPy for Geo {
    fn of(world: &Bound<'_, PyAny>) -> PyResult<Arc<Geo>> {
        let py = world.py();
        let raw_configs = attribute(world, intern!(py, "model_configs"))?;
        let configs = configs::current_of(world, &raw_configs)?;
        let (present, stamp) = overlay_held(world)?;
        if let Some(stamp) = stamp {
            let found = configs.geo.lock().ok().and_then(|mut cache| cache.by_stamp(stamp));
            if let Some(geo) = found {
                if configs::watching() {
                    let key = overlay_key(world, present, Some(stamp))?;
                    if !configs.geo.lock().is_ok_and(|cache| cache.holds(&key)) {
                        STALE_HITS.fetch_add(1, Ordering::Relaxed);
                    }
                }
                return Ok(geo);
            }
        }
        let key = overlay_key(world, present, stamp)?;
        if let Some(geo) = configs.geo.lock().ok().and_then(|mut cache| cache.by_bytes(&key)) {
            return Ok(geo);
        }
        let facility = match facility_geometry(&configs, &raw_configs) {
            Section::Absent => None,
            Section::Ok(geometry) => Some(geometry.plan.clone().map_err(|fault| refusal(&fault))?),
            Section::Fault(fault) => return Err(refusal(fault)),
        };
        let has_constraints = config_entry(&raw_configs, intern!(py, "spatial_constraints"))?.is_some() || present;
        let spatial = if has_constraints {
            match spatial_constraints(&configs, &raw_configs) {
                Section::Ok(spatial) => Some(spatial.clone()),
                Section::Absent => None,
                Section::Fault(fault) => return Err(refusal(fault)),
            }
        } else {
            None
        };
        // The overlay's parts are read only when they have entries, as the key's bytes say.
        let (moves, partitions) = with_state_value(world, OVERLAY_KEY, |overlay| {
            let part = |name: &str, bytes: &[u8]| match (overlay, bytes.is_empty()) {
                (Some(V::Map(overlay)), false) => overlay.get(name).cloned(),
                _ => None,
            };
            Ok((part("facility_points", &key.moves), part("partitions", &key.partitions)))
        })?;
        let geo = Arc::new(Geo::new(facility.as_deref(), spatial.as_deref(), moves.as_ref(), partitions.as_ref()).or_py()?);
        if let Ok(mut cache) = configs.geo.lock() {
            cache.insert(key, geo.clone());
        }
        Ok(geo)
    }

    fn zone_center(&self, zone_id: &str) -> PyResult<Option<Pt>> {
        self.center(zone_id).or_py()
    }
}

/// How many plans the configs of a world keep now, for tests (`tests/integration/test_geo_cache.py`).
#[pyfunction]
fn geo_plans_of(world: &Bound<'_, PyAny>) -> PyResult<usize> {
    let raw_configs = attribute(world, intern!(world.py(), "model_configs"))?;
    let configs = configs::current_of(world, &raw_configs)?;
    let plans = configs.geo.lock().map(|cache| cache.len()).unwrap_or(0);
    Ok(plans)
}

/// The plan of a world as plain values, for tests: the zone centres, the facility points in
/// their order, the moved service points, the blocked polygons, and the routes clear to each
/// mobility.
#[pyfunction]
fn geo_summary_of<'py>(world: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = world.py();
    let geo = Geo::of(world)?;
    let summary = pyo3::types::PyDict::new(py);
    if let Some(layout) = &geo.layout {
        let centers = pyo3::types::PyDict::new(py);
        for (id, zone) in &layout.zones {
            centers.set_item(&**id, zone.center.ok().flatten().map(|point| (point.x, point.y)))?;
        }
        summary.set_item("zone_centers", centers)?;
        let points: Vec<(&str, (f64, f64))> = layout.facility_points.iter().map(|(id, point)| (&**id, (point.x, point.y))).collect();
        summary.set_item("facility_points", points)?;
        let services: Vec<(&str, (f64, f64))> =
            layout.service_points.iter().map(|point| (&*point.id, (point.point.x, point.point.y))).collect();
        summary.set_item("service_points", services)?;
        let mut clear = Vec::new();
        for mobility in ["independent", "assisted", "wheelchair"] {
            for id in layout.routes.keys() {
                clear.push((mobility, &**id, geo.route_is_clear(id, mobility)));
            }
        }
        summary.set_item("routes_clear", clear)?;
    }
    if let Some(constraints) = &geo.constraints {
        let blocked: Vec<Vec<(f64, f64)>> =
            constraints.blocked.iter().map(|polygon| polygon.iter().map(|point| (point.x, point.y)).collect()).collect();
        summary.set_item("blocked", blocked)?;
        summary.set_item("obstacle_count", constraints.obstacle_count)?;
    }
    Ok(summary.into_any())
}

/// The IDs and travel minutes of the arrival approach routes the kernels read, for tests.
#[pyfunction]
fn approach_routes_of(world: &Bound<'_, PyAny>) -> PyResult<Vec<(String, i64)>> {
    approach_routes(world)?.iter().map(|route| Ok((route.route_id.to_string(), travel_minutes(route)?))).collect()
}

/// `independent_queues_required(world.model_configs)` as the kernels read it, for tests.
#[pyfunction]
fn independent_queues_of(world: &Bound<'_, PyAny>) -> PyResult<bool> {
    independent_queues(world)
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(geo_plans_of, m)?)?;
    m.add_function(wrap_pyfunction!(approach_routes_of, m)?)?;
    m.add_function(wrap_pyfunction!(independent_queues_of, m)?)?;
    m.add_function(wrap_pyfunction!(geo_summary_of, m)?)?;
    Ok(())
}
