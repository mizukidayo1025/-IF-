//! `validate_population_dynamics_state(world)` of `domain/population.py`, with Python's errors and
//! texts (lane D of #250, #349). Raw errors.
//!
//! Python's rule is the core's kernel (`kernel_only("population",
//! "validate_population_dynamics_state")`, `rules::population::validate_facts`), so this reads the
//! same facts of the world and gives its refusal as the `KernelRefusal` Python raises.

use std::sync::Arc;

use super::View;
use crate::config::{ApproachRoute, Fault, FaultClass};
use crate::contracts;
use crate::error::{Error, Result};
use crate::events::EventRow;
use crate::host::Contract;
use crate::mathx::{hypot, py_sum};
use crate::rules::kit::{DYNAMICS, PRESENCE};
use crate::rules::population::{plan_read_of, validate_facts, Facts, Routes, RoutesFault, NO_ROW};
use crate::value::V;

const LABEL: &str = "population.validate_population_dynamics_state";

/// Whether an arrival on its approach route is waiting: the rule then reads the approach routes.
fn reads_routes(state: &V) -> bool {
    matches!(state, V::Map(fields) if matches!(fields.get("pending_arrivals"), Some(V::List(pending))
        if pending.iter().any(|item| matches!(item, V::Map(item) if item.contains_key("approach_route_id")))))
}

/// `parsed_facility_geometry(model_configs.get("facility_geometry", {})).arrival_approach_routes`:
/// the routes of the config's `FacilityGeometry` (`contracts`), each with its `distance_m` and
/// `travel_minutes` properties, or the contract's error as the plan's refusal.
fn approach_routes(view: &View<'_>) -> Result<Routes> {
    let raw = view.config("facility_geometry")?.unwrap_or_else(|| V::Map(Default::default()));
    let geometry = match contracts::validate(&Contract::FacilityGeometry, &raw) {
        Ok(geometry) => geometry,
        Err(error) => return Ok(Err(RoutesFault { caught: false, refusal: plan_read_of(&error) })),
    };
    let shape = || Error::unsupported("the facility geometry's dump has another shape");
    let routes = match &geometry {
        V::Map(geometry) => match geometry.get("arrival_approach_routes") {
            Some(V::List(routes)) => routes.as_slice(),
            _ => return Err(shape()),
        },
        _ => return Err(shape()),
    };
    let text = |value: Option<&V>| match value {
        Some(V::Str(text)) => Ok(text.clone()),
        _ => Err(shape()),
    };
    let number = |value: Option<&V>| match value {
        Some(V::Float(number)) => Ok(*number),
        _ => Err(shape()),
    };
    let mut out = Vec::with_capacity(routes.len());
    for route in routes {
        let V::Map(route) = route else {
            return Err(shape());
        };
        let (Some(V::List(sources)), Some(V::List(points))) = (route.get("source_ids"), route.get("points_m")) else {
            return Err(shape());
        };
        let source_ids = sources.iter().map(|source| text(Some(source))).collect::<Result<Vec<_>>>()?;
        let points = points
            .iter()
            .map(|point| match point {
                V::Map(point) => Ok((number(point.get("x"))?, number(point.get("y"))?)),
                _ => Err(shape()),
            })
            .collect::<Result<Vec<_>>>()?;
        let walking_speed = number(route.get("walking_speed_m_per_min"))?;
        let legs: Vec<f64> = points.windows(2).map(|pair| hypot(&[pair[1].0 - pair[0].0, pair[1].1 - pair[0].1])).collect();
        let distance = py_sum(&legs);
        // `max(1, math.ceil(distance / speed))`: an infinite quotient is OverflowError.
        let minutes = (distance / walking_speed).ceil();
        let travel_minutes = if minutes.is_finite() && minutes.abs() < 9.0e15 {
            Ok((minutes as i64).max(1))
        } else {
            Err(Fault::new(FaultClass::Other("builtins.OverflowError".into()), "cannot convert float infinity to integer"))
        };
        out.push(ApproachRoute {
            route_id: text(route.get("route_id"))?,
            destination_zone_id: text(route.get("destination_zone_id"))?,
            source_ids,
            walking_speed,
            distance,
            travel_minutes,
        });
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
