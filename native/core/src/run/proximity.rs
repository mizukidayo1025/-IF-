//! `controller.proximity_contact_config` and `controller.proximity_policy` (lane P8-0b; lane c6
//! reads them in the core).
//!
//! `sample_contacts` of `proximity.rs` does nothing without a config, reads its interval and
//! version, asks for a `DistanceSampledProximityPolicy` by `is` and reads the policy's `config`.

use std::sync::Arc;

use crate::config::read::Read;
use crate::config::Fault;
use crate::value::V;

use super::held::{Attr, Obj, Spec};

/// The config the controller made (a dict, or None).
pub const CONFIG: Attr = Attr::value("proximity_contact_config");
/// What the sampler reads of the policy.
pub const POLICY: Attr = Attr::object("proximity_policy", &POLICY_SPEC);
const POLICY_SPEC: Spec = Spec::of(&[Attr::object("config", &POLICY_CONFIG)]);
const POLICY_CONFIG: Spec = Spec::with_fields(&[]);

/// What the Run read of the controller's proximity.
#[derive(Clone, Debug, PartialEq)]
pub struct Proximity {
    /// `proximity_contact_config`, `V::Null` for None.
    pub config: Read<V>,
    /// Whether the policy is `DistanceSampledProximityPolicy` (`is_the_policy`).
    pub known: Read<bool>,
    pub policy: Read<Option<Arc<Obj>>>,
}

impl Proximity {
    pub fn of(controller: &Obj) -> Self {
        let policy = controller.object("proximity_policy").map(|policy| policy.cloned());
        let known = policy.as_ref().map_err(Fault::clone).map(|policy| {
            policy.as_ref().is_some_and(|policy| policy.class.is("sheltersim.policies.proximity", "DistanceSampledProximityPolicy"))
        });
        Self { config: controller.value("proximity_contact_config").cloned(), known, policy }
    }
}
