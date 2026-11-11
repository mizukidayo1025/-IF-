//! `controller.psychology`: the psychology model and its `PsychologyParameters` (lane P8-0b; lane
//! a3 reads them in the core).
//!
//! `psychology.rs` of the PyO3 crate knows the model by the names of its class (`model_of`) and
//! reads the rates of `parameters`, of `base` for the life-state models, and the body config and
//! formula.

use crate::config::read::Read;
use crate::config::Fault;

use super::held::{Attr, Obj, Spec};
use std::sync::Arc;

/// What the psychology rules read of the policy.
pub const ATTR: Attr = Attr::object("psychology", &POLICY);
const POLICY: Spec = Spec::of(&[
    Attr::value("policy_version"),
    Attr::value("source_id"),
    Attr::object("parameters", &PARAMETERS),
    Attr::value("include_direct_rest_stress_recovery"),
    Attr::value("use_tick_start_stress_for_feedback"),
    Attr::object("base", &BASE),
    Attr::value("body_config"),
    Attr::object("body_formula", &FORMULA),
    Attr::value("update_long_term_state"),
]);
/// Every field of the dataclass.
const PARAMETERS: Spec = Spec::with_fields(&[]);
const BASE: Spec = Spec::of(&[Attr::value("include_direct_rest_stress_recovery")]);
const FORMULA: Spec = Spec::of(&[Attr::value("model_id")]);

/// The refusal of a class `model_of` does not know.
pub const UNKNOWN: &str = "the policy is not one this kernel knows";
/// The refusal of a class whose names are not str.
pub const NO_PLAIN_NAME: &str = "the policy has no plain class name";

/// The psychology model, by its class's names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Model {
    /// `SimplePsychologyPolicy`
    Simple,
    /// `BoundedStressPolicy`
    Bounded,
    /// `MeanRevertingDistressPolicy`
    MeanReverting,
    /// `LifeStatePsychologyPolicy`
    LifeState,
    /// `IntegratedDistressLifeStatePolicy`
    Integrated,
}

/// What the Run read of `controller.psychology`.
#[derive(Clone, Debug, PartialEq)]
pub struct Psychology {
    pub model: Read<Model>,
    /// The policy as read: `parameters` holds every field of `PsychologyParameters`.
    pub policy: Read<Arc<Obj>>,
}

impl Psychology {
    pub fn of(controller: &Obj) -> Self {
        let policy = controller.some_object("psychology").cloned();
        let model = policy.as_ref().map_err(Fault::clone).and_then(|policy| model_of(policy));
        Self { model, policy }
    }

    /// `policy.parameters`.
    pub fn parameters(&self) -> Read<&Arc<Obj>> {
        self.policy.as_ref().map_err(Fault::clone)?.some_object("parameters")
    }
}

/// `model_of` of `psychology.rs`: by `__module__` and `__qualname__`, so a subclass is left to Python.
pub fn model_of(policy: &Obj) -> Read<Model> {
    let class = &policy.class;
    if !class.has_plain_names() {
        return Err(Fault::refused(NO_PLAIN_NAME));
    }
    const KNOWN: [(&str, &str, Model); 5] = [
        ("sheltersim.policies.psychology", "SimplePsychologyPolicy", Model::Simple),
        ("sheltersim.policies.alternatives", "BoundedStressPolicy", Model::Bounded),
        ("sheltersim.policies.psychology", "MeanRevertingDistressPolicy", Model::MeanReverting),
        ("sheltersim.policies.psychology", "LifeStatePsychologyPolicy", Model::LifeState),
        ("sheltersim.policies.psychology", "IntegratedDistressLifeStatePolicy", Model::Integrated),
    ];
    KNOWN
        .iter()
        .find(|(module, name, _)| class.has_name(module, name))
        .map(|(_, _, model)| *model)
        .ok_or_else(|| Fault::refused(UNKNOWN))
}
