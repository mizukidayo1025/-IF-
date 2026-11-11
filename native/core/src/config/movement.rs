//! The sections of movement and routing (lane 7-5a of Issue #152): `movement_model` as the walking
//! times read it (`canonical_movement_config`, for the values whose result is certain), whether the
//! configs time routes at all, and `layout_routing` as the end-of-tick check reads it.
//!
//! The kernels parse both themselves, from the configs as a `V`, with the refusals they said when
//! they read the Python tree, in the same order of checks.

use std::sync::Arc;

use super::read::{float, int, map, text, Read};
use super::section::Fault;
use crate::value::V;

pub const POLICY_VERSION: &str = "spatial-route-and-room-waypoint-v2";
pub const MOVEMENT_SOURCE_ID: &str = "ASM-SPATIAL-MOVEMENT-V2";
/// Ints from here on are not all exact as floats.
pub const EXACT_INT: i64 = 1 << 53;

pub const SPEED_INDEPENDENT: f64 = 72.0;
pub const SPEED_ASSISTED: f64 = 36.0;
pub const SPEED_WHEELCHAIR: f64 = 48.0;
pub const DENSITY_SLOWDOWN: f64 = 0.12;
pub const MAXIMUM_SLOWDOWN: f64 = 2.5;

/// `canonical_movement_config`, for the values whose result is certain: floats, and what it accepts.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct MovementModel {
    pub independent: f64,
    pub assisted: f64,
    pub wheelchair: f64,
    pub density_slowdown: f64,
    pub maximum_slowdown: f64,
}

impl Default for MovementModel {
    fn default() -> Self {
        Self {
            independent: SPEED_INDEPENDENT,
            assisted: SPEED_ASSISTED,
            wheelchair: SPEED_WHEELCHAIR,
            density_slowdown: DENSITY_SLOWDOWN,
            maximum_slowdown: MAXIMUM_SLOWDOWN,
        }
    }
}

impl MovementModel {
    /// The speed of a mobility (`mobility_text`): any other than the first two is a wheelchair's.
    pub fn speed(&self, mobility: &str) -> f64 {
        match mobility {
            "independent" => self.independent,
            "assisted" => self.assisted,
            _ => self.wheelchair,
        }
    }

    /// The model of `model_configs.get("movement_model")`: the defaults for None or no key.
    pub fn parse(raw: Option<&V>) -> Read<Self> {
        let mut model = Self::default();
        let raw = match raw {
            None | Some(V::Null) => return Ok(model),
            Some(raw) => map(raw)?,
        };
        for (name, value) in raw {
            match &**name {
                "schema_version" => {
                    if int(value)? != 1 {
                        return Err(Fault::refused("another movement schema"));
                    }
                }
                "policy_version" => {
                    if &**text(value)? != POLICY_VERSION {
                        return Err(Fault::refused("another movement policy"));
                    }
                }
                "source_id" => {
                    if &**text(value)? != MOVEMENT_SOURCE_ID {
                        return Err(Fault::refused("another movement source"));
                    }
                }
                "speed_m_per_minute_by_mobility" => {
                    let speeds = map(value)?;
                    if speeds.len() != 3 {
                        return Err(Fault::refused("the speed table has other fields"));
                    }
                    let speed = |name: &str| -> Read<f64> {
                        let value = speeds.get(name).ok_or_else(|| Fault::refused("a speed is missing"))?;
                        let speed = coefficient(value)?;
                        if !speed.is_finite() || speed <= 0.0 {
                            return Err(Fault::refused("a speed is not positive"));
                        }
                        Ok(speed)
                    };
                    model.independent = speed("independent")?;
                    model.assisted = speed("assisted")?;
                    model.wheelchair = speed("wheelchair")?;
                }
                "density_slowdown_per_person_m2" => {
                    model.density_slowdown = coefficient(value)?;
                    if !model.density_slowdown.is_finite() || model.density_slowdown < 0.0 {
                        return Err(Fault::refused("a slowdown is not a plain value"));
                    }
                }
                "maximum_density_slowdown" => {
                    model.maximum_slowdown = coefficient(value)?;
                    if !model.maximum_slowdown.is_finite() || model.maximum_slowdown < 1.0 {
                        return Err(Fault::refused("a slowdown is not a plain value"));
                    }
                }
                _ => return Err(Fault::refused("the movement config has other fields")),
            }
        }
        Ok(model)
    }
}

/// A coefficient as `canonical_movement_config` takes it: a float, or an int that is not a bool.
/// The Python rule's arithmetic used such an int as the float of the same value.
fn coefficient(value: &V) -> Read<f64> {
    match value {
        V::Int(_) | V::Big(_) => {
            let number = int(value)?;
            if number.abs() >= EXACT_INT {
                return Err(Fault::refused("a coefficient is too large for a float"));
            }
            Ok(number as f64)
        }
        _ => float(value),
    }
}

/// What the walking times read of the configs.
#[derive(Clone, PartialEq, Debug)]
pub struct Movement {
    /// `"movement_model" in model_configs`.
    pub has_model: bool,
    /// `"facility_geometry" in model_configs`.
    pub has_geometry: bool,
    /// The model, or what reading it refuses: said where a walk is timed.
    pub model: Read<MovementModel>,
}

impl Movement {
    /// Whether a route is timed by the movement model (`route_with_movement_time`).
    pub fn times_routes(&self) -> bool {
        self.has_model || self.has_geometry
    }

    /// `exact_dict(model_configs)` and its two keys; the model is read now and refused later.
    pub fn parse(configs: &V) -> Read<Self> {
        let configs = map(configs)?;
        let raw = configs.get("movement_model");
        Ok(Self { has_model: raw.is_some(), has_geometry: configs.contains_key("facility_geometry"), model: MovementModel::parse(raw) })
    }
}

/// `model_configs["layout_routing"]` as the end-of-tick check reads it: None for no config or None,
/// or the capacities of the edges, each an int or what reading it refuses. The checks of the edges
/// themselves stay at the read site, which knows the world's edges.
#[derive(Clone, PartialEq, Debug)]
pub struct RouteCapacities {
    pub capacities: Vec<(Arc<str>, Read<i64>)>,
}

pub const ROUTE_CONFIG_LACKS_CAPACITIES: &str = "the route config lacks its capacities";
pub const ROUTE_CONFIG_IS_PYTHON: &str = "the route config is the Python rule's";

impl RouteCapacities {
    /// `exact_dict(model_configs).get("layout_routing")` and `route_capacities_pass` up to the edges.
    pub fn parse(configs: &V) -> Read<Option<Self>> {
        let config = match map(configs)?.get("layout_routing") {
            None | Some(V::Null) => return Ok(None),
            Some(config) => map(config)?,
        };
        let text_is = |name: &str, expected: &str| matches!(config.get(name), Some(V::Str(value)) if &**value == expected);
        let version_is_one = matches!(config.get("schema_version"), Some(V::Int(1)));
        let Some(capacities) = config.get("max_agents_in_transit_by_edge") else {
            return Err(Fault::refused(ROUTE_CONFIG_LACKS_CAPACITIES));
        };
        if config.len() != 4 || !version_is_one || !text_is("source_type", "assumption") || !text_is("source_id", "ASM-A05-ROUTE-EDIT-V1") {
            return Err(Fault::refused(ROUTE_CONFIG_IS_PYTHON));
        }
        let capacities = map(capacities)?.iter().map(|(edge, capacity)| (edge.clone(), int(capacity))).collect();
        Ok(Some(Self { capacities }))
    }
}
