//! `canonical_proximity_contact_config(model_configs.get("social_proximity"))`, as the proximity
//! contact handler reads it (lane 7-5b of Issue #152). The PyO3 crate calls the Python check, which
//! raises its own errors, and hands over the dict it returns.

use std::sync::Arc;

use super::read::{map, number, text, Read};
use crate::value::V;

#[derive(Clone, PartialEq, Debug)]
pub struct Proximity {
    pub policy_version: Arc<str>,
    pub max_distance_m: Read<f64>,
}

impl Proximity {
    /// From the canonical config; None for None (the sampler is off).
    pub fn from_v(config: &V) -> Read<Option<Self>> {
        if matches!(config, V::Null) {
            return Ok(None);
        }
        let config = map(config)?;
        let field = |name: &str| config.get(name).unwrap_or(&V::Null);
        Ok(Some(Self {
            policy_version: text(field("policy_version"))?.clone(),
            max_distance_m: number(field("max_distance_m")),
        }))
    }
}
