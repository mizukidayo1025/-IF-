//! `controller.social_policy`: the LLM's social policy, None without an LLM, and its
//! `SocialRuntimeLimits` (lane P8-0b; lanes b3, e1 and P9-3 read them in the core).
//!
//! `consider_due_complaints` of `staff.rs` reads `limits.complaint_wait_minutes` (`exact_int`);
//! the dispatcher's gate around `consider_cooperation` reads `limits.stress_threshold`.

use std::sync::Arc;

use crate::config::read::Read;
use crate::config::Fault;

use super::held::{Attr, Obj, Spec};

/// What the rules read of the policy.
pub const ATTR: Attr = Attr::object("social_policy", &POLICY);
const POLICY: Spec = Spec::of(&[Attr::value("policy_version"), Attr::object("limits", &LIMITS)]);
const LIMITS: Spec = Spec::with_fields(&[]);

/// What the Run read of `controller.social_policy`.
#[derive(Clone, Debug, PartialEq)]
pub struct Social {
    /// None without an LLM.
    pub policy: Read<Option<Arc<Obj>>>,
}

impl Social {
    pub fn of(controller: &Obj) -> Self {
        Self { policy: controller.object("social_policy").map(|policy| policy.cloned()) }
    }

    /// `policy.limits`, None without a policy.
    pub fn limits(&self) -> Read<Option<&Arc<Obj>>> {
        match self.policy.as_ref().map_err(Fault::clone)? {
            None => Ok(None),
            Some(policy) => policy.some_object("limits").map(Some),
        }
    }

    /// `exact_int(policy.limits.complaint_wait_minutes)`, None without a policy.
    pub fn complaint_wait_minutes(&self) -> Read<Option<i64>> {
        match self.limits()? {
            None => Ok(None),
            Some(limits) => limits.int("complaint_wait_minutes").map(Some),
        }
    }
}
