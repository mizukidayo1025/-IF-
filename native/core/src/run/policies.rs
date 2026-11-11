//! `Policies`: what a Run's rules read of its controller's policies and managers, read once per
//! controller (lane P8-0b; policies E7 and E14 of docs/rust-port/step5-plan.md).
//!
//! The PyO3 crate reads `CONTROLLER` from the controller when it makes the Run's `RunCore`, and
//! again at `RunCore.refresh_policies` (a test or tool that puts another policy on a controller
//! calls it, as `refresh_routes` for another space). Each part is made here from what was read, in
//! its own file: a lane that moves a kernel reads its part here instead of the controller, and adds
//! the attributes it reads to its part's `Spec`. A new part is one line in `CONTROLLER` and one field
//! of `Policies`.
//!
//! Not state: nothing saves, hashes or compares it.

use std::sync::Arc;

use crate::config::read::Read;
use crate::config::Fault;

use super::behavior::{self, Behavior};
use super::held::{Obj, Spec, NO_CONTROLLER};
use super::information::{self, Information};
use super::proximity::{self, Proximity};
use super::psychology::{self, Psychology};
use super::services::{self, Services};
use super::sinks::{self, Sinks};
use super::social::{self, Social};
use super::stress::{self, Stress};

/// What the Run reads of the controller.
pub const CONTROLLER: Spec = Spec::of(&[
    psychology::ATTR,
    behavior::ATTR,
    stress::ATTR,
    information::ATTR,
    proximity::CONFIG,
    proximity::POLICY,
    social::ATTR,
    services::TOILET,
    services::RESOURCE,
    services::STAFF,
    sinks::ARTIFACTS,
    sinks::LLM_ROUTER,
]);

/// What a Run's rules read of its controller.
#[derive(Clone, Debug, PartialEq)]
pub struct Policies {
    /// The controller as read, or the refusal of a Run without one.
    pub controller: Read<Arc<Obj>>,
    pub psychology: Psychology,
    pub behavior: Behavior,
    pub stress: Stress,
    pub information: Information,
    pub proximity: Proximity,
    pub social: Social,
    pub services: Services,
    pub sinks: Sinks,
}

impl Policies {
    /// The parts of what was read of a controller. Never fails: each part keeps its faults.
    pub fn of(controller: Arc<Obj>) -> Self {
        Self {
            psychology: Psychology::of(&controller),
            behavior: Behavior::of(&controller),
            stress: Stress::of(&controller),
            information: Information::of(&controller),
            proximity: Proximity::of(&controller),
            social: Social::of(&controller),
            services: Services::of(&controller),
            sinks: Sinks::of(&controller),
            controller: Ok(controller),
        }
    }

    /// The policies of a Run without a controller (`RunCore(route_capacities)` of a test): every
    /// read refuses.
    pub fn absent() -> Self {
        let mut empty = Obj::new(super::held::Class { module: None, qualname: None, resolves: false });
        for attr in CONTROLLER.attrs {
            empty.attrs.insert(Arc::from(attr.name), Err(Fault::refused(NO_CONTROLLER)));
        }
        let mut policies = Self::of(Arc::new(empty));
        policies.controller = Err(Fault::refused(NO_CONTROLLER));
        policies
    }

    /// Whether the Run read a controller.
    pub fn has_controller(&self) -> bool {
        self.controller.is_ok()
    }

    /// The flags of the LLM, or the first fault.
    pub fn llm(&self) -> Read<sinks::Llm> {
        self.sinks.llm()
    }
}

impl Default for Policies {
    fn default() -> Self {
        Self::absent()
    }
}
