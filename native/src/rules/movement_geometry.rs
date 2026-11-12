//! The floor plan's readers for the tests (`tests/integration/test_geo_cache.py`,
//! `test_tick_views.py`): the plans the configs keep, the plan as plain values, the approach routes
//! and whether the independent queues are required, as the kernels read them. The floor plan's
//! reads and the parse of its sections are `core/geometry.rs` (lane P10-1c).

use pyo3::intern;
use pyo3::prelude::*;

use crate::core::configs;
use crate::core::geometry::{approach_routes, independent_queues, travel_minutes, Geo, GeoPy};
use crate::core::row::attribute;

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

