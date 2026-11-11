//! `controller.decisions`: the behavior runner, its policy and the policy's scorer (lane P8-0b;
//! lane a5 reads them in the core).
//!
//! `due_outcomes` of `behavior.rs` asks for a `BehaviorDecisionRunner` whose policy is a
//! `UtilityBehaviorPolicy`, or a `PriorityRuleBehaviorPolicy` whose scorer is one, each by `is`,
//! and reads the scorer's coefficients.

use std::sync::Arc;

use crate::config::read::Read;
use crate::config::Fault;

use super::held::{Attr, Obj, Spec};

const MODULE: &str = "sheltersim.policies.behavior";

/// What the behavior rules read of the runner.
pub const ATTR: Attr = Attr::object("decisions", &RUNNER);
const RUNNER: Spec = Spec::of(&[Attr::object("policy", &POLICY)]);
/// The utility policy is its own scorer: its attributes are the scorer's.
const POLICY: Spec = Spec::with_fields(&[Attr::value("policy_version"), Attr::object("scorer", &SCORER)]);
const SCORER: Spec = Spec::with_fields(&[Attr::value("policy_version")]);

pub const OTHER_RUNNER: &str = "the decisions are another class";
pub const OTHER_POLICY: &str = "the policy is another class";
pub const OTHER_SCORER: &str = "the policy's scorer is another class";

/// The policy's rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    /// `UtilityBehaviorPolicy`, its own scorer.
    Utility,
    /// `PriorityRuleBehaviorPolicy` over a `UtilityBehaviorPolicy` scorer.
    Priority,
}

/// What the Run read of `controller.decisions`.
#[derive(Clone, Debug, PartialEq)]
pub struct Behavior {
    pub rule: Read<Rule>,
    /// `decisions.policy`.
    pub policy: Read<Arc<Obj>>,
    /// The scorer: the policy itself, or the priority rule's `scorer`.
    pub scorer: Read<Arc<Obj>>,
}

impl Behavior {
    pub fn of(controller: &Obj) -> Self {
        let runner = controller.some_object("decisions");
        let policy = runner.and_then(|runner| {
            if !runner.class.is(MODULE, "BehaviorDecisionRunner") {
                return Err(Fault::refused(OTHER_RUNNER));
            }
            runner.some_object("policy").cloned()
        });
        let (rule, scorer) = match &policy {
            Err(fault) => (Err(fault.clone()), Err(fault.clone())),
            Ok(policy) if policy.class.is(MODULE, "UtilityBehaviorPolicy") => (Ok(Rule::Utility), Ok(policy.clone())),
            Ok(policy) if policy.class.is(MODULE, "PriorityRuleBehaviorPolicy") => match policy.some_object("scorer") {
                Ok(scorer) if scorer.class.is(MODULE, "UtilityBehaviorPolicy") => (Ok(Rule::Priority), Ok(scorer.clone())),
                Ok(_) => (Err(Fault::refused(OTHER_SCORER)), Err(Fault::refused(OTHER_SCORER))),
                Err(fault) => (Err(fault.clone()), Err(fault)),
            },
            Ok(_) => (Err(Fault::refused(OTHER_POLICY)), Err(Fault::refused(OTHER_POLICY))),
        };
        Self { rule, policy, scorer }
    }
}
