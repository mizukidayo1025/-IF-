//! `controller.stress_policy`: the stress propagation policy and its `SocialPropagationParameters`
//! (lane P8-0b; lane a4 reads them in the core).
//!
//! `update_social_propagation` of `social_propagation.rs` reads `parameters.enabled` first, then
//! knows the policy by `is` (`settings_of`) and reads the parameters.

use std::sync::Arc;

use crate::config::read::Read;
use crate::config::Fault;

use super::held::{Attr, Obj, Spec};

/// What the propagation rules read of the policy.
pub const ATTR: Attr = Attr::object("stress_policy", &POLICY);
const POLICY: Spec = Spec::of(&[Attr::value("policy_version"), Attr::object("parameters", &PARAMETERS)]);
const PARAMETERS: Spec = Spec::with_fields(&[]);

pub const UNKNOWN: &str = "a policy this kernel does not know";

/// The policy, by its class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Model {
    /// `StressPropagationPolicy`
    Sparse,
    /// `ExpressivenessChannelOpennessPolicy`
    Channel,
    /// `DeGrootContactPolicy`
    DeGroot,
}

/// What the Run read of `controller.stress_policy`.
#[derive(Clone, Debug, PartialEq)]
pub struct Stress {
    pub model: Read<Model>,
    pub policy: Read<Arc<Obj>>,
}

impl Stress {
    pub fn of(controller: &Obj) -> Self {
        let policy = controller.some_object("stress_policy").cloned();
        let model = policy.as_ref().map_err(Fault::clone).and_then(|policy| {
            let class = &policy.class;
            if class.is("sheltersim.policies.social", "ExpressivenessChannelOpennessPolicy") {
                Ok(Model::Channel)
            } else if class.is("sheltersim.policies.social", "StressPropagationPolicy") {
                Ok(Model::Sparse)
            } else if class.is("sheltersim.policies.alternatives", "DeGrootContactPolicy") {
                Ok(Model::DeGroot)
            } else {
                Err(Fault::refused(UNKNOWN))
            }
        });
        Self { model, policy }
    }

    /// `policy.parameters`.
    pub fn parameters(&self) -> Read<&Arc<Obj>> {
        self.policy.as_ref().map_err(Fault::clone)?.some_object("parameters")
    }
}
