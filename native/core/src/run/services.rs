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
/// What `assign_pending` reads of `StaffService` (lane b3 reads it in the core: `staff_service`).
pub const STAFF_SPEC: Spec = Spec::of(&[
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

/// What `assign_pending` takes of a `StaffService` as read (`STAFF_SPEC`): `policy_version` an
/// exact str, `task_duration_minutes` and `fairness_after_minutes` ints, and its route resolver and
/// travel scheduler None, the controller's own steps (`Held::Method`), or another callable. A read
/// that raised is its fault (policy E14).
pub fn staff_service(service: &Obj) -> Read<crate::rules::staff::Service> {
    use crate::rules::staff::{Service, Step};
    use crate::value::V;
    use super::held::Held;
    let policy = match service.get("policy_version")? {
        Held::Value(V::Str(text)) => text.clone(),
        _ => return Err(Fault::refused("a value is not a str")),
    };
    let task_minutes = service.int("task_duration_minutes")?;
    let fairness_after = service.int("fairness_after_minutes")?;
    let step = |name: &str, own: &str| -> Read<Step> {
        Ok(match service.get(name)? {
            Held::Value(V::Null) => Step::None,
            Held::Method(step) if &**step == own => Step::Own,
            _ => Step::Other,
        })
    };
    let resolver = step("route_resolver", "_route")?;
    let scheduler = step("travel_scheduler", "_schedule_staff_support_travel")?;
    Ok(Service { policy, task_minutes, fairness_after, resolver, scheduler })
}
