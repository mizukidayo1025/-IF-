//! `controller.decisions`: the behavior runner, its policy and the policy's scorer (lane P8-0b;
//! lane a5 reads them in the core, `rules::behavior`).
//!
//! The propose step asks for a `BehaviorDecisionRunner` whose policy is a `UtilityBehaviorPolicy`,
//! or a `PriorityRuleBehaviorPolicy` whose scorer is one, each by `is`, and reads the scorer's
//! coefficients: every field of its `__dict__`.

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

/// The paths of the reads, from the controller, that the PyO3 crate reads again to raise what a
/// read raised (policy E14).
pub const RUNNER_PATH: &[&str] = &["decisions"];
pub const POLICY_PATH: &[&str] = &["decisions", "policy"];
pub const SCORER_PATH: &[&str] = &["decisions", "policy", "scorer"];

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
    /// The read whose fault `rule` holds, from the controller (empty while `rule` is Ok).
    pub fault_path: &'static [&'static str],
}

/// An attribute that is an object; None is the refusal `none`, as the step found `type(None)` to
/// be no class it knows.
fn some<'a>(object: &'a Obj, name: &str, none: &'static str) -> Read<&'a Arc<Obj>> {
    object.object(name)?.ok_or_else(|| Fault::refused(none))
}

impl Behavior {
    pub fn of(controller: &Obj) -> Self {
        let failed = |fault: Fault, path| Self {
            rule: Err(fault.clone()),
            policy: Err(fault.clone()),
            scorer: Err(fault),
            fault_path: path,
        };
        let runner = match some(controller, "decisions", OTHER_RUNNER) {
            Ok(runner) if runner.class.is(MODULE, "BehaviorDecisionRunner") => runner,
            Ok(_) => return failed(Fault::refused(OTHER_RUNNER), RUNNER_PATH),
            Err(fault) => return failed(fault, RUNNER_PATH),
        };
        let policy = match some(runner, "policy", OTHER_POLICY) {
            Ok(policy) => policy,
            Err(fault) => return failed(fault, POLICY_PATH),
        };
        let none: &'static [&'static str] = &[];
        let (rule, scorer, fault_path) = if policy.class.is(MODULE, "UtilityBehaviorPolicy") {
            (Ok(Rule::Utility), Ok(policy.clone()), none)
        } else if policy.class.is(MODULE, "PriorityRuleBehaviorPolicy") {
            match some(policy, "scorer", OTHER_SCORER) {
                Ok(scorer) if scorer.class.is(MODULE, "UtilityBehaviorPolicy") => (Ok(Rule::Priority), Ok(scorer.clone()), none),
                Ok(_) => (Err(Fault::refused(OTHER_SCORER)), Err(Fault::refused(OTHER_SCORER)), SCORER_PATH),
                Err(fault) => (Err(fault.clone()), Err(fault), SCORER_PATH),
            }
        } else {
            (Err(Fault::refused(OTHER_POLICY)), Err(Fault::refused(OTHER_POLICY)), POLICY_PATH)
        };
        Self { rule, policy: Ok(policy.clone()), scorer, fault_path }
    }
}
