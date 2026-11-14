//! The resource service block of `WorldState.validate` (`core/world.py`), with Python's errors and
//! texts: the config (`ResourceServiceConfig`, a pydantic model of `experiments/scenario.py`) and
//! WorldState v6, the ledger it needs, and `resource_service_v1`: its state, its request and stall
//! histories, and the stalls at the queues' heads.
//!
//! Lane B of #250 (#347) ports it; until then it passes (`STUB(lane B #347)`) and reads the config
//! as it finds it. It raises the `StateInvariantError`s of `WorldState.validate` itself.

use std::sync::Arc;

use super::View;
use crate::error::Result;
use crate::value::V;

/// `ResourceServiceConfig`: what the entity rules read of it (`resource_config` of
/// `_validate_entity_rules`).
#[derive(Clone, Debug, PartialEq)]
pub struct ServiceConfig {
    pub service_minutes: i64,
    pub water_ml_per_service: i64,
    pub food_meal_units_per_service: i64,
    pub water_staff_skill: Arc<str>,
    pub meal_staff_skill: Arc<str>,
}

impl ServiceConfig {
    /// The config as it is, with the model's defaults where a value is missing or of another type:
    /// the stub's reading, which the port replaces with the model's validation.
    fn as_found(config: &V) -> Self {
        let map = match config {
            V::Map(map) => Some(map),
            _ => None,
        };
        let int = |name: &str| match map.and_then(|map| map.get(name)) {
            Some(V::Int(number)) => *number,
            _ => 0,
        };
        let text = |name: &str, default: &str| match map.and_then(|map| map.get(name)) {
            Some(V::Str(text)) => text.clone(),
            _ => Arc::from(default),
        };
        Self {
            service_minutes: int("service_minutes"),
            water_ml_per_service: int("water_ml_per_service"),
            food_meal_units_per_service: int("food_meal_units_per_service"),
            water_staff_skill: text("water_staff_skill", "water_service"),
            meal_staff_skill: text("meal_staff_skill", "meal_service"),
        }
    }
}

/// The check, as `validate` (`validate/mod.rs`) calls it in Python's order: the config where the
/// configs have one (`resource_config`), or None.
pub fn validate(view: &View<'_>) -> Result<Option<ServiceConfig>> {
    // STUB(lane B #347)
    Ok(view.config("resource_service")?.map(|config| ServiceConfig::as_found(&config)))
}
