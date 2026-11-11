//! `controller.information_policy`: the information spread model and its config (lane P8-0b; lane
//! a4 reads them in the core).
//!
//! `inputs` of `information.rs` does nothing without a policy, knows the model by `is` and reads the
//! config (`Settings::read`); `proposals.rs` reads the config's `enabled`.

use std::sync::Arc;

use crate::config::read::Read;
use crate::config::Fault;

use super::held::{Attr, Obj, Spec};

/// What the information rules read of the policy.
pub const ATTR: Attr = Attr::object("information_policy", &POLICY);
const POLICY: Spec = Spec::of(&[Attr::value("policy_version"), Attr::object("config", &CONFIG)]);
const CONFIG: Spec = Spec::with_fields(&[]);

pub const UNKNOWN: &str = "a model this kernel does not know";

/// The model, by its class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Model {
    /// `InformationSpreadPolicy`
    Sparse,
    /// `EvidenceAwareInformationPolicy`
    EvidenceAware,
    /// `HazardInformationPolicy`
    Hazard,
}

/// What the Run read of `controller.information_policy`.
#[derive(Clone, Debug, PartialEq)]
pub struct Information {
    /// None where the controller has no policy.
    pub model: Read<Option<Model>>,
    /// None where the controller has no policy.
    pub policy: Read<Option<Arc<Obj>>>,
}

impl Information {
    pub fn of(controller: &Obj) -> Self {
        let policy = controller.object("information_policy").map(|policy| policy.cloned());
        let model = policy.as_ref().map_err(Fault::clone).and_then(|policy| match policy {
            None => Ok(None),
            Some(policy) => {
                let class = &policy.class;
                if class.is("sheltersim.policies.information", "InformationSpreadPolicy") {
                    Ok(Some(Model::Sparse))
                } else if class.is("sheltersim.policies.information", "EvidenceAwareInformationPolicy") {
                    Ok(Some(Model::EvidenceAware))
                } else if class.is("sheltersim.policies.alternatives", "HazardInformationPolicy") {
                    Ok(Some(Model::Hazard))
                } else {
                    Err(Fault::refused(UNKNOWN))
                }
            }
        });
        Self { model, policy }
    }

    /// `policy.config`, None without a policy.
    pub fn config(&self) -> Read<Option<&Arc<Obj>>> {
        match self.policy.as_ref().map_err(Fault::clone)? {
            None => Ok(None),
            Some(policy) => policy.some_object("config").map(Some),
        }
    }
}
