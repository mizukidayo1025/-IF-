//! The section of `model_configs["resource_service"]` (lane 7-5a of Issue #152): the copy that
//! `ResourceServiceManager(world)` makes of it, and the staff skills the overlay's placement checks.

use std::sync::Arc;

use indexmap::IndexMap;

use super::read::{map, Read};
use super::section::Fault;
use crate::value::V;

pub const NOT_A_MAPPING: &str = "the service config is another mapping";

/// What the rules read of the resource service's config.
#[derive(Clone, PartialEq, Debug)]
pub struct ResourceService {
    /// `"resource_service" in model_configs`.
    pub present: bool,
    /// `dict(model_configs["resource_service"])` where it is a dict: a copy of its items. None for no
    /// config, or one that is not a dict (the PyO3 crate then copies the Python value as before).
    pub config: Option<Arc<IndexMap<Arc<str>, V>>>,
    /// The skills `_require_service_staff_in_zone` reads, or its refusal of a config that is not a dict.
    pub skills: Read<ServiceSkills>,
}

/// `config.get("water_staff_skill")` and the meal's, where each is a str.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ServiceSkills {
    pub water: Option<Arc<str>>,
    pub meal: Option<Arc<str>>,
}

impl ResourceService {
    /// `exact_dict(model_configs)` and its `resource_service`.
    pub fn parse(configs: &V) -> Read<Self> {
        let raw = map(configs)?.get("resource_service");
        let (config, skills) = match raw {
            None => (None, Ok(ServiceSkills::default())),
            Some(V::Map(config)) => {
                let skill = |name: &str| match config.get(name) {
                    Some(V::Str(skill)) => Some(skill.clone()),
                    _ => None,
                };
                (Some(Arc::new(config.clone())), Ok(ServiceSkills { water: skill("water_staff_skill"), meal: skill("meal_staff_skill") }))
            }
            Some(_) => (None, Err(Fault::refused(NOT_A_MAPPING))),
        };
        Ok(Self { present: raw.is_some(), config, skills })
    }
}
