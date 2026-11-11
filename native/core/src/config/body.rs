//! The body's sections: the operators of `body_state_v1` (`create_affect`,
//! `create_relationship_policy`), the minutes of a social action, and the selected care formula
//! (`create_care_outcome_formula`) (lane 7-5b of Issue #152).

use std::sync::Arc;

use super::read::{int_or, map, number, Read, INT_TOO_LARGE};
use super::section::Fault;
use crate::mathx::{py_max, py_min};
use crate::value::V;

pub const LINEAR_AFFECT: &str = "linear-positive-negative-v1";
pub const TRUST_WEIGHTED_AFFECT: &str = "trust-weighted-positive-negative-v1";
pub const DISTANCE_TRUST: &str = "distance-duration-repeat-trust-v1";
pub const DURATION_TRUST: &str = "duration-contact-trust-v1";
/// `body_dynamics.social_interaction_minutes` where the config has none.
pub const DEFAULT_INTERACTION_MINUTES: i64 = 5;
pub const TYPED_CARE_MODEL: &str = "typed-care-support-response-v1";
pub const PROPORTIONAL_HYGIENE_MODEL: &str = "hygiene-support-proportional-relief-v1";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AffectModel {
    Linear,
    TrustWeighted,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum TrustModel {
    DistanceDurationRepeat { distance_scale_m: f64, repeat_contact_bonus: f64 },
    DurationOnly,
}

/// The operators the body rule creates, for the models this kernel knows.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct BodyModels {
    pub affect: AffectModel,
    pub trust: TrustModel,
}

impl BodyModels {
    /// `affect` is the selected affect model, or the config's `affect_policy` where none is
    /// selected; `relationship` the selected relationship model. An ID this kernel doesn't know is
    /// left to Python's factory.
    pub fn parse(affect: &str, relationship: Option<&str>, configs: &V) -> Read<Self> {
        let affect = match affect {
            LINEAR_AFFECT => AffectModel::Linear,
            TRUST_WEIGHTED_AFFECT => AffectModel::TrustWeighted,
            _ => return Err(Fault::refused("an affect model this kernel doesn't know")),
        };
        let Some(relationship) = relationship else {
            // `create_relationship_policy` without a selection.
            return Ok(Self { affect, trust: TrustModel::DurationOnly });
        };
        let trust = match relationship {
            DURATION_TRUST => TrustModel::DurationOnly,
            DISTANCE_TRUST => {
                let body = map(configs)?.get("body_dynamics");
                let parameter = |name: &str, default: f64| -> Read<f64> {
                    let Some(body) = body else {
                        return Ok(default);
                    };
                    match map(body)?.get(name) {
                        None => Ok(default),
                        Some(V::Float(number)) => Ok(*number),
                        Some(V::Int(number)) => Ok(*number as f64),
                        Some(V::Big(_)) => Err(Fault::refused(INT_TOO_LARGE)),
                        Some(_) => Err(Fault::refused("a relationship parameter is not a number")),
                    }
                };
                let distance_scale_m = parameter("relationship_distance_scale_m", 2.0)?;
                // Zero divides, which raises in Python. Its validation keeps it within 0.1..100.
                if distance_scale_m <= 0.0 {
                    return Err(Fault::refused("the distance scale is not positive"));
                }
                TrustModel::DistanceDurationRepeat {
                    distance_scale_m,
                    repeat_contact_bonus: parameter("relationship_repeat_contact_bonus", 0.05)?,
                }
            }
            _ => return Err(Fault::refused("a relationship model this kernel doesn't know")),
        };
        Ok(Self { affect, trust })
    }
}

/// `body_dynamics.social_interaction_minutes` as an autonomous social action reads it.
pub fn social_interaction_minutes(configs: &V) -> Read<i64> {
    match map(configs)?.get("body_dynamics") {
        Some(config) => int_or(config, "social_interaction_minutes", DEFAULT_INTERACTION_MINUTES),
        None => Ok(DEFAULT_INTERACTION_MINUTES),
    }
}

/// What the support rule reads of a `CareOutcomeFormula`: its model, its source and its config, a
/// copy of `body_dynamics`.
#[derive(Clone, PartialEq, Debug)]
pub struct CareOutcome {
    pub model_id: Arc<str>,
    pub source_id: Arc<str>,
    config: V,
}

impl CareOutcome {
    pub fn new(model_id: Arc<str>, source_id: Arc<str>, config: V) -> Read<Self> {
        map(&config)?;
        Ok(Self { model_id, source_id, config })
    }

    /// `configured_value` of `support_state_deltas`.
    fn configured(&self, name: &str, default: f64, agent_id: &str) -> Read<f64> {
        let config = map(&self.config)?;
        if let Some(V::Map(per_agent)) = config.get(format!("{name}_by_agent").as_str()) {
            if let Some(value) = per_agent.get(agent_id) {
                return number(value);
            }
        }
        match config.get(name) {
            None => Ok(default),
            Some(value) => number(value),
        }
    }

    /// `CareOutcomeFormula.support_state_deltas`: each field with its delta, in the formula's order.
    pub fn deltas(&self, kind: &str, agent_id: &str) -> Read<Vec<(&'static str, f64)>> {
        if &*self.model_id != TYPED_CARE_MODEL {
            return Ok(Vec::new());
        }
        let value = |name: &str, default: f64| self.configured(name, default, agent_id);
        Ok(match kind {
            "caregiver_assistance" => vec![
                ("stress", -value("caregiver_assistance_stress_relief", 0.03)?),
                ("reassurance", value("caregiver_assistance_reassurance_gain", 0.02)?),
            ],
            "dependent_assistance" => vec![
                ("stress", -value("dependent_assistance_stress_relief", 0.04)?),
                ("reassurance", value("dependent_assistance_reassurance_gain", 0.03)?),
            ],
            "privacy_accommodation" => vec![("discontent", -value("privacy_accommodation_discontent_relief", 0.02)?)],
            "mobility_assistance" => vec![("stress", -value("mobility_assistance_stress_relief", 0.03)?)],
            "language_access" => vec![("uncertainty", -value("language_access_uncertainty_relief", 0.05)?)],
            "medical_assessment" => vec![("uncertainty", -value("medical_assessment_uncertainty_relief", 0.02)?)],
            "medication_support" => vec![("uncertainty", -value("medication_support_uncertainty_relief", 0.01)?)],
            _ => Vec::new(),
        })
    }

    /// `CareOutcomeFormula.hygiene_need_after_support`.
    pub fn hygiene_after(&self, need: f64, relief: f64) -> f64 {
        let bounded_need = py_min(1.0, py_max(0.0, need));
        let bounded_relief = py_min(1.0, py_max(0.0, relief));
        if &*self.model_id == PROPORTIONAL_HYGIENE_MODEL {
            bounded_need * (1.0 - bounded_relief)
        } else {
            py_max(0.0, bounded_need - bounded_relief)
        }
    }
}
