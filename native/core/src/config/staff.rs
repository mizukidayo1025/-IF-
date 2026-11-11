//! The staff's sections: the fatigue formula, the routed task formula and the minutes of a support
//! task (lane 7-5b of Issue #152). The kernels of `rules/staff*.rs` read them.

use std::sync::Arc;

use indexmap::IndexMap;

use super::read::{int, int_or, map, text, Read};
use super::section::Fault;
use crate::value::V;

pub const LINEAR_FATIGUE: &str = "staff-fatigue-linear-v1";
pub const SATURATING_FATIGUE: &str = "staff-fatigue-saturating-v1";
pub const URGENCY_FIRST: &str = "staff-urgency-first-v1";
pub const TRAVEL_FIRST: &str = "staff-travel-first-v1";
/// The provenance of the routed tasks where the configs select no formula.
pub const OPERATIONS_SOURCE: &str = "ASM-STAFF-OPERATIONS-V1";
pub const OPERATIONS_POLICY: &str = "autonomous-routed-staff-v1";
/// `model_configs["staff"].get("support_task_minutes", 10)`.
pub const DEFAULT_SUPPORT_MINUTES: i64 = 10;

/// What `update_staff_dynamics` takes of `create_staff_fatigue_formula` (`rules/staff_dynamics.rs`).
#[derive(Clone, PartialEq, Debug, Default)]
pub struct StaffFatigue {
    /// `staff-fatigue-saturating-v1`, else the linear formula.
    pub saturating: bool,
    /// `staff_dynamics.max_continuous_work_minutes_by_staff`, when it has an entry: each limit, or
    /// what `exact_int` says of it where a member's limit is read.
    limits: Option<IndexMap<Arc<str>, Read<i64>>>,
}

impl StaffFatigue {
    /// The formulas this kernel knows; a later registered one is the Python rule's.
    pub fn parse(configs: &V) -> Read<Self> {
        let configs = map(configs)?;
        let selected = match configs.get("policy_registry") {
            Some(registry) => map(registry)?.get("staff_fatigue"),
            None => None,
        };
        let saturating = match selected {
            Some(selected) => match &**text(selected)? {
                LINEAR_FATIGUE => false,
                SATURATING_FATIGUE => true,
                _ => return Err(Fault::refused("another fatigue formula")),
            },
            None => false,
        };
        let limits = match configs.get("staff_dynamics") {
            Some(config) => match map(config)?.get("max_continuous_work_minutes_by_staff") {
                Some(limits) => Some(map(limits)?),
                None => None,
            },
            None => None,
        };
        let limits = limits
            .filter(|limits| !limits.is_empty())
            .map(|limits| limits.iter().map(|(id, limit)| (id.clone(), int(limit))).collect());
        Ok(Self { saturating, limits })
    }

    /// A member's own limit, None for one without.
    pub fn limit(&self, id: &str) -> Read<Option<i64>> {
        match self.limits.as_ref().and_then(|limits| limits.get(id)) {
            Some(limit) => limit.clone().map(Some),
            None => Ok(None),
        }
    }
}

/// What the staff rules take of `create_staff_operations_formula(configs)`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct StaffOperations {
    /// The formula's `model_id`.
    pub model_id: Arc<str>,
    /// The formula's `source_id`, where `policy_registry` selects `staff_operations`; None where
    /// the configs select none and the events keep the routed tasks' own provenance.
    pub selected_source: Option<Arc<str>>,
}

/// The source of each built-in formula (`MODEL_DEFINITIONS[model_id]["assumption_source_id"]`).
const URGENCY_FIRST_SOURCE: &str = "ASM-STAFF-URGENCY-FIRST-V1";
const TRAVEL_FIRST_SOURCE: &str = "ASM-STAFF-TRAVEL-FIRST-V1";

impl StaffOperations {
    /// `create_staff_operations_formula(configs)` of the built-in formulas, for a build without
    /// Python (the PyO3 crate fills the section from the Python factory, which may know a formula
    /// registered later): the formula `policy_registry` selects, urgency first by default; its
    /// source where the registry names one. Another formula is refused.
    pub fn parse(configs: &V) -> Read<Self> {
        let selected = match map(configs)?.get("policy_registry") {
            Some(registry) => map(registry)?.get("staff_operations"),
            None => None,
        };
        let model_id = match selected {
            Some(selected) => text(selected)?.clone(),
            None => Arc::from(URGENCY_FIRST),
        };
        let source = match &*model_id {
            URGENCY_FIRST => URGENCY_FIRST_SOURCE,
            TRAVEL_FIRST => TRAVEL_FIRST_SOURCE,
            _ => return Err(Fault::refused("another staff operations formula")),
        };
        Ok(Self { model_id, selected_source: selected.map(|_| Arc::from(source)) })
    }

    /// The travel-first ranking.
    pub fn travel_first(&self) -> bool {
        &*self.model_id == TRAVEL_FIRST
    }

    /// The source and policy of the events' provenance, and their `source_id`.
    pub fn provenance(&self) -> (&str, &str, &str) {
        match &self.selected_source {
            Some(source) => (source, &self.model_id, source),
            None => (OPERATIONS_SOURCE, OPERATIONS_POLICY, OPERATIONS_POLICY),
        }
    }
}

/// `assign_staff_operations`' check of `policy_registry["staff_operations"]`: the two formulas
/// this kernel ranks by.
pub fn known_staff_operations(configs: &V) -> Read<()> {
    if let Some(registry) = map(configs)?.get("policy_registry") {
        if let Some(selected) = map(registry)?.get("staff_operations") {
            if !matches!(&**text(selected)?, URGENCY_FIRST | TRAVEL_FIRST) {
                return Err(Fault::refused("another staff operations formula"));
            }
        }
    }
    Ok(())
}

/// `model_configs["staff"].get("support_task_minutes", 10)` as `_support_route_wait_reasons`
/// reads it.
pub fn support_task_minutes(configs: &V) -> Read<i64> {
    match map(configs)?.get("staff") {
        Some(staff) => int_or(staff, "support_task_minutes", DEFAULT_SUPPORT_MINUTES),
        None => Ok(DEFAULT_SUPPORT_MINUTES),
    }
}
