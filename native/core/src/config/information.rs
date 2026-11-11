//! `InformationSpreadConfig.from_mapping(model_configs.get("information_spread"))`, as the
//! information rules read it (lane 7-5b of Issue #152).
//!
//! The PyO3 crate builds the config with the Python class, which raises its own errors, and hands
//! over the attributes the rules read as a map (`InformationSpread::FIELDS`). A number is checked
//! where the rule reads it, as `number` checked the attribute there before.

use std::sync::Arc;

use indexmap::IndexMap;

use super::read::{int, map, number, Read};
use crate::value::V;

/// What the information rules read of the config.
#[derive(Clone, PartialEq, Debug)]
pub struct InformationSpread {
    /// `"information_spread" in model_configs`; a reader that takes the key with `[...]` refuses
    /// a configs without it.
    pub present: bool,
    pub enabled: bool,
    pub minimum_contact_minutes: Read<i64>,
    pub default_source_reliability: Read<f64>,
    correction_source_reliability_by_id: IndexMap<Arc<str>, Read<f64>>,
    pub rumor_belief_probability: Read<f64>,
    pub rumor_trust_sensitivity: Read<f64>,
    pub rumor_source_sensitivity: Read<f64>,
    pub query_pressure_per_claim: Read<f64>,
    pub query_pressure_relief_per_correction: Read<f64>,
    pub correction_acceptance_probability: Read<f64>,
    pub correction_trust_sensitivity: Read<f64>,
    pub correction_source_sensitivity: Read<f64>,
}

impl InformationSpread {
    /// The attributes read, in this order.
    pub const FIELDS: [&'static str; 13] = [
        "enabled",
        "minimum_contact_minutes",
        "default_source_reliability",
        "correction_source_reliability_by_id",
        "rumor_belief_probability",
        "rumor_trust_sensitivity",
        "rumor_source_sensitivity",
        "query_pressure_per_claim",
        "query_pressure_relief_per_correction",
        "correction_acceptance_probability",
        "correction_trust_sensitivity",
        "correction_source_sensitivity",
        // Not an attribute: whether the configs have the key.
        "present",
    ];

    /// From the map of `FIELDS` to the config's attributes. A missing field reads as None.
    pub fn from_v(config: &V) -> Read<Self> {
        let fields = map(config)?;
        let field = |name: &str| fields.get(name).unwrap_or(&V::Null);
        let by_id = map(field("correction_source_reliability_by_id"))?
            .iter()
            .map(|(source, value)| (source.clone(), number(value)))
            .collect();
        Ok(Self {
            present: field("present").is_truthy(),
            enabled: field("enabled").is_truthy(),
            minimum_contact_minutes: int(field("minimum_contact_minutes")),
            default_source_reliability: number(field("default_source_reliability")),
            correction_source_reliability_by_id: by_id,
            rumor_belief_probability: number(field("rumor_belief_probability")),
            rumor_trust_sensitivity: number(field("rumor_trust_sensitivity")),
            rumor_source_sensitivity: number(field("rumor_source_sensitivity")),
            query_pressure_per_claim: number(field("query_pressure_per_claim")),
            query_pressure_relief_per_correction: number(field("query_pressure_relief_per_correction")),
            correction_acceptance_probability: number(field("correction_acceptance_probability")),
            correction_trust_sensitivity: number(field("correction_trust_sensitivity")),
            correction_source_sensitivity: number(field("correction_source_sensitivity")),
        })
    }

    /// `correction_source_reliability_by_id.get(source_id, default_source_reliability)`.
    pub fn reliability(&self, source_id: &str) -> Read<f64> {
        match self.correction_source_reliability_by_id.get(source_id) {
            Some(reliability) => reliability.clone(),
            None => self.default_source_reliability.clone(),
        }
    }
}
