//! The service managers: `controller.toilet_service`, `controller.resource_service` (None without
//! the resource service) and `controller.staff_service` (lane P8-0b; lanes b3, c3 and P9-2 read
//! them in the core).
//!
//! The tick driver arbitrates a manager of the class it was installed with (`is`) in Rust and any
//! other in Python; the service rules read the parameters below. A manager's `world` is the
//! controller's, which the Run does not read.

use std::sync::Arc;

use crate::config::read::Read;
use crate::config::Fault;

use super::held::{Attr, Obj, Spec};

/// `ToiletServiceManager` of `domain/facilities.py`.
pub const TOILET_CLASS: (&str, &str) = ("sheltersim.domain.facilities", "ToiletServiceManager");
/// `ResourceServiceManager` of `domain/resource_service.py`.
pub const RESOURCE_CLASS: (&str, &str) = ("sheltersim.domain.resource_service", "ResourceServiceManager");
/// `StaffService` of `domain/staff.py`.
pub const STAFF_CLASS: (&str, &str) = ("sheltersim.domain.staff", "StaffService");

pub const TOILET: Attr = Attr::object("toilet_service", &TOILET_SPEC);
const TOILET_SPEC: Spec = Spec::of(&[
    Attr::value("policy_version"),
    Attr::value("min_service_minutes"),
    Attr::value("max_service_minutes"),
]);

pub const RESOURCE: Attr = Attr::object("resource_service", &RESOURCE_SPEC);
const RESOURCE_SPEC: Spec = Spec::of(&[Attr::value("policy_version"), Attr::value("config")]);

pub const STAFF: Attr = Attr::object("staff_service", &STAFF_SPEC);
const STAFF_SPEC: Spec = Spec::of(&[
    Attr::value("policy_version"),
    Attr::value("task_duration_minutes"),
    Attr::value("unresolved_after_minutes"),
    Attr::value("cooldown_minutes"),
    Attr::value("fairness_after_minutes"),
    Attr::value("route_resolver"),
    Attr::value("travel_scheduler"),
]);

/// A manager as the Run read it.
#[derive(Clone, Debug, PartialEq)]
pub struct Manager {
    /// None for a manager that is None (only the resource service may be).
    pub manager: Read<Option<Arc<Obj>>>,
    /// Whether the manager is of the class the driver was installed with.
    pub known: Read<bool>,
}

impl Manager {
    fn of(controller: &Obj, name: &str, (module, qualname): (&str, &str)) -> Self {
        let manager = controller.object(name).map(|manager| manager.cloned());
        let known = manager
            .as_ref()
            .map_err(Fault::clone)
            .map(|manager| manager.as_ref().is_some_and(|manager| manager.class.is(module, qualname)));
        Self { manager, known }
    }

    /// The manager, a refusal for None.
    pub fn object(&self) -> Read<&Arc<Obj>> {
        self.manager.as_ref().map_err(Fault::clone)?.as_ref().ok_or_else(|| Fault::refused(super::held::NOT_AN_OBJECT))
    }

    /// `manager is None`.
    pub fn is_none(&self) -> Read<bool> {
        Ok(self.manager.as_ref().map_err(Fault::clone)?.is_none())
    }
}

/// What the Run read of the service managers.
#[derive(Clone, Debug, PartialEq)]
pub struct Services {
    pub toilet: Manager,
    pub resource: Manager,
    pub staff: Manager,
}

impl Services {
    pub fn of(controller: &Obj) -> Self {
        Self {
            toilet: Manager::of(controller, "toilet_service", TOILET_CLASS),
            resource: Manager::of(controller, "resource_service", RESOURCE_CLASS),
            staff: Manager::of(controller, "staff_service", STAFF_CLASS),
        }
    }
}
