//! `validate_population_dynamics_state(world)` of `domain/population.py`, with Python's errors and
//! texts (lane D of #250, #349). Raw errors.
//!
//! Python's rule is the core's kernel (`kernel_only("population",
//! "validate_population_dynamics_state")`, `rules::population::validate_facts`), so this reads the
//! same facts of the world and gives its refusal as the `KernelRefusal` Python raises.

use std::sync::Arc;

use super::View;
use crate::config::{ApproachRoute, Fault, FaultClass};
use crate::error::{Error, Result};
use crate::mathx::{hypot, py_sum};
use crate::events::EventRow;
use crate::rules::kit::{DYNAMICS, PRESENCE};
use crate::rules::population::{validate_facts, Facts, Routes, RoutesFault, NO_ROW};
use crate::value::V;

const LABEL: &str = "population.validate_population_dynamics_state";

/// Whether an arrival on its approach route is waiting: the rule then reads the approach routes.
fn reads_routes(state: &V) -> bool {
    matches!(state, V::Map(fields) if matches!(fields.get("pending_arrivals"), Some(V::List(pending))
        if pending.iter().any(|item| matches!(item, V::Map(item) if item.contains_key("approach_route_id")))))
}

/// `parsed_facility_geometry(model_configs.get("facility_geometry", {})).arrival_approach_routes`
/// of a config its contract (`FacilityGeometry`) accepts as it is: each route's fields as written,
/// with its `distance_m` and `travel_minutes` properties. A config of another shape is refused
/// here (Python raises the contract's error, read as the plan's refusal).
fn approach_routes(view: &View<'_>) -> Result<Routes> {
    let refused = |reason: &'static str| Ok(Err(RoutesFault { caught: false, refusal: Error::unsupported(reason) }));
    let Some(V::Map(geometry)) = view.config("facility_geometry")? else {
        return refused("the facility geometry is not a plain dict");
    };
    let routes = match geometry.get("arrival_approach_routes") {
        None => return Ok(Ok(Arc::from(Vec::new()))),
        Some(V::List(routes)) => routes,
        Some(_) => return refused("the approach routes are not a list"),
    };
    let text = |value: Option<&V>| match value {
        Some(V::Str(text)) => Some(text.clone()),
        _ => None,
    };
    let number = |value: Option<&V>| match value {
        Some(V::Float(number)) if number.is_finite() => Some(*number),
        Some(V::Int(number)) => Some(*number as f64),
        _ => None,
    };
    let mut out = Vec::with_capacity(routes.len());
    for route in routes {
        let V::Map(route) = route else {
            return refused("an approach route is not a plain dict");
        };
        let (Some(route_id), Some(destination_zone_id), Some(walking_speed)) =
            (text(route.get("route_id")), text(route.get("destination_zone_id")), number(route.get("walking_speed_m_per_min")))
        else {
            return refused("an approach route has another shape");
        };
        let Some(V::List(sources)) = route.get("source_ids") else {
            return refused("an approach route has another shape");
        };
        let source_ids: Option<Vec<Arc<str>>> = sources.iter().map(|source| text(Some(source))).collect();
        let Some(V::List(points)) = route.get("points_m") else {
            return refused("an approach route has another shape");
        };
        let points: Option<Vec<(f64, f64)>> = points
            .iter()
            .map(|point| match point {
                V::Map(point) => Some((number(point.get("x"))?, number(point.get("y"))?)),
                _ => None,
            })
            .collect();
        let (Some(source_ids), Some(points)) = (source_ids, points) else {
            return refused("an approach route has another shape");
        };
        if points.len() < 2 || walking_speed <= 0.0 {
            return refused("an approach route has another shape");
        }
        let legs: Vec<f64> = points.windows(2).map(|pair| hypot(&[pair[1].0 - pair[0].0, pair[1].1 - pair[0].1])).collect();
        let distance = py_sum(&legs);
        let minutes = (distance / walking_speed).ceil();
        let travel_minutes = if minutes.is_finite() && minutes < 9.0e15 {
            Ok((minutes as i64).max(1))
        } else {
            Err(Fault::new(FaultClass::Other("builtins.OverflowError".into()), "cannot convert float infinity to integer"))
        };
        out.push(ApproachRoute { route_id, destination_zone_id, source_ids, walking_speed, distance, travel_minutes });
    }
    Ok(Ok(Arc::from(out)))
}

/// The check, as `validate` (`validate/mod.rs`) calls it in Python's order.
pub fn validate(view: &View<'_>) -> Result<()> {
    let Some(state) = view.state(DYNAMICS)? else {
        return Ok(());
    };
    let presence = view.state(PRESENCE)?;
    let agents: Vec<Arc<str>> = view.rows()?.agents.iter().map(|(id, _)| id.clone()).collect();
    let zone_known = |zone: &str| -> Result<bool> { Ok(view.rows()?.zones.contains_key(zone)) };
    let event = |event_id: &str| -> Result<Option<Arc<EventRow>>> {
        let queue = view.queue()?;
        match queue.serial_of(event_id) {
            None => Ok(None),
            Some(serial) => queue.slot(serial).row.clone().map(Some).ok_or_else(|| Error::unsupported(NO_ROW)),
        }
    };
    let routes = if reads_routes(&state) { Some(approach_routes(view)?) } else { None };
    validate_facts(&Facts {
        state: Some(&state),
        presence: presence.as_deref(),
        agents: &agents,
        zone_known: &zone_known,
        minute_index: view.minute_index(),
        duration: view.duration_minutes(),
        routes: routes.as_ref(),
        event: &event,
    })
    .map_err(|error| Error::refused(LABEL, error))
}
