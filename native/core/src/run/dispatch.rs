//! The dispatcher's handlers: `controller.event_dispatcher.handlers`, read once per controller (lane
//! P9-0, H3 of design-p9.md).
//!
//! The dispatch loop runs an event of a handler it knows in Rust, with the values it reads of that
//! handler (its cooldown, whether it expires a modifier, the operation of a ledger event, its
//! managers), and gives any other handler to the host (`Host::apply_foreign`). Whether a handler is
//! known is a matter of identity: its class is the one `events/handlers.py` installed, and that
//! class's `apply` is the one installed with it (a test's patch is not). The PyO3 crate reads that
//! here once, at the controller's start and at `RunCore.refresh_policies` (or `refresh_dispatch`,
//! which the controller calls once its dispatcher is made); the tick checks the identity of the
//! dispatcher and its handlers before it uses this (design-p9.md §4, P9-1c).
//!
//! Not state: nothing saves, hashes or compares it.

use std::sync::Arc;

use indexmap::IndexMap;

use crate::config::read::{self, Read};
use crate::config::Fault;
use crate::value::V;

use super::held::{Attr, Held, Obj, Spec};

macro_rules! kinds {
    ($($kind:ident => $module:expr, $qualname:literal, $spec:expr;)*) => {
        /// A handler class the core runs, by its class in `events/handlers.py` or `events/effects.py`.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum HandlerKind {
            $($kind,)*
        }

        impl HandlerKind {
            /// Every kind, in the order `events/handlers.py` installs them.
            pub const ALL: &'static [HandlerKind] = &[$(HandlerKind::$kind),*];

            /// The module and the name of the class.
            pub fn class(self) -> (&'static str, &'static str) {
                match self {
                    $(HandlerKind::$kind => ($module, $qualname),)*
                }
            }

            /// What the Run reads of a handler of this kind.
            pub fn spec(self) -> &'static Spec {
                match self {
                    $(HandlerKind::$kind => $spec,)*
                }
            }
        }
    };
}

const HANDLERS: &str = "sheltersim.events.handlers";
const EFFECTS: &str = "sheltersim.events.effects";

const NOTHING: Spec = Spec::of(&[]);
const EXPIRES: Spec = Spec::of(&[Attr::value("expire")]);
const COOLDOWN: Spec = Spec::of(&[Attr::value("cooldown_minutes")]);
const SUPPORT: Spec = Spec::of(&[Attr::value("cooldown_minutes"), Attr::value("require_colocated")]);
const LEDGER: Spec = Spec::of(&[Attr::value("operation")]);
const TOILET: Spec = Spec::of(&[Attr::value("after_complete")]);
/// A manager a handler holds: whether a test set its own `request` on it.
pub const MANAGER: Spec = Spec { attrs: &[], fields: false, class_own: &[], own: &["request"] };
const TRAVEL: Spec = Spec::of(&[
    Attr::object("toilet_service", &MANAGER),
    Attr::object("resource_service", &MANAGER),
    Attr::value("validate_request_state"),
]);
const SERVICE: Spec = Spec::of(&[Attr::object("service", &MANAGER)]);

kinds! {
    SocialInteraction => HANDLERS, "SocialInteractionHandler", &NOTHING;
    AgentTravelCompletion => HANDLERS, "AgentTravelCompletionHandler", &TRAVEL;
    ToiletServiceCompletion => HANDLERS, "ToiletServiceCompletionHandler", &TOILET;
    ResourceServiceCompletion => HANDLERS, "ResourceServiceCompletionHandler", &SERVICE;
    SupportRequestTimeout => HANDLERS, "SupportRequestTimeoutHandler", &COOLDOWN;
    StaffSupportCompletion => HANDLERS, "StaffSupportCompletionHandler", &SUPPORT;
    ProximityContact => HANDLERS, "ProximityContactHandler", &NOTHING;
    InformationRumor => HANDLERS, "InformationRumorHandler", &NOTHING;
    InformationCorrection => HANDLERS, "InformationCorrectionHandler", &NOTHING;
    InformationContact => HANDLERS, "InformationContactHandler", &NOTHING;
    ResourceLedger => HANDLERS, "ResourceLedgerHandler", &LEDGER;
    FacilityOutage => HANDLERS, "FacilityOutageHandler", &NOTHING;
    FacilityRecovery => HANDLERS, "FacilityRecoveryHandler", &NOTHING;
    InfrastructureEvent => HANDLERS, "InfrastructureEventHandler", &NOTHING;
    ResourceServiceCancellation => HANDLERS, "ResourceServiceCancellationHandler", &SERVICE;
    EffectProgramPopulation => HANDLERS, "EffectProgramPopulationHandler", &NOTHING;
    DeliveryDelay => HANDLERS, "DeliveryDelayHandler", &NOTHING;
    WaterOutage => HANDLERS, "WaterOutageHandler", &NOTHING;
    WaterRestoration => HANDLERS, "WaterRestorationHandler", &NOTHING;
    StaffUnavailability => HANDLERS, "StaffUnavailabilityHandler", &NOTHING;
    StaffRestoration => HANDLERS, "StaffRestorationHandler", &NOTHING;
    StaffOperationArrival => HANDLERS, "StaffOperationArrivalHandler", &NOTHING;
    StaffOperationRelief => HANDLERS, "StaffOperationReliefHandler", &NOTHING;
    StaffOperationCompletion => HANDLERS, "StaffOperationCompletionHandler", &NOTHING;
    EffectProgramFacility => EFFECTS, "EffectProgramFacilityHandler", &EXPIRES;
    EffectProgramEnvironment => EFFECTS, "EffectProgramEnvironmentHandler", &EXPIRES;
    EffectProgramTemporarySpace => EFFECTS, "EffectProgramTemporarySpaceHandler", &EXPIRES;
    EffectProgramGeometry => EFFECTS, "EffectProgramGeometryHandler", &EXPIRES;
    EffectProgramStructure => EFFECTS, "EffectProgramStructureHandler", &NOTHING;
}

impl HandlerKind {
    /// The kind of a class by its module and name, None for a class the core does not run.
    pub fn of_class(module: &str, qualname: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|kind| kind.class() == (module, qualname))
    }
}

/// The name of the controller's own step a toilet's completion leads to
/// (`DispatchPhase._schedule_return_to_initial_zone`), as `Held::Method` names it.
pub const RETURN_RULE: &str = "_schedule_return_to_initial_zone";

/// The refusal of a value the run cannot test for truth (an object it did not read).
pub const NOT_TESTABLE: &str = "a value is not one the run can test for truth";
/// The refusal of a value that is not a bool.
pub const NOT_A_BOOL: &str = "a value is not a bool";
/// The refusal of a Run whose dispatch table was not installed (`events/handlers.py`).
pub const NO_TABLE: &str = "the dispatch table is not installed";

/// What `after_complete` of a toilet's completion is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AfterComplete {
    None,
    /// The controller's own step, bound to the controller the Run read.
    Own,
    /// Anything else, which the core does not run.
    Other,
}

/// A handler the core runs.
#[derive(Clone, Debug, PartialEq)]
pub struct Native {
    pub kind: HandlerKind,
    /// What the Run read of the handler, by its kind's `Spec`.
    pub handler: Arc<Obj>,
    /// Whether every manager the handler holds is the controller's own of that service (`is`):
    /// `service` is `controller.resource_service`; `toilet_service` and `resource_service` are the
    /// controller's. True for a handler that holds none.
    pub own_managers: bool,
    /// Whether the class's `_after_arrival` is the one defined with it (only a travel completion
    /// has one; true for the others).
    pub after_arrival_as_defined: bool,
}

/// Truth as Python tests it, of a plain value.
fn truthy(value: &V) -> bool {
    match value {
        V::Null => false,
        V::Bool(flag) => *flag,
        V::Int(number) => *number != 0,
        V::Float(number) => *number != 0.0,
        V::Big(_) => true,
        V::Str(text) => !text.is_empty(),
        V::List(items) => !items.is_empty(),
        V::Map(map) => !map.is_empty(),
    }
}

impl Native {
    /// `exact_int(handler.cooldown_minutes)`.
    pub fn cooldown_minutes(&self) -> Read<i64> {
        self.handler.int("cooldown_minutes")
    }

    /// `bool(handler.require_colocated)`.
    pub fn require_colocated(&self) -> Read<bool> {
        self.truth("require_colocated")
    }

    /// `bool(handler.validate_request_state)`.
    pub fn validate_request_state(&self) -> Read<bool> {
        self.truth("validate_request_state")
    }

    /// `handler.expire`, a bool and nothing else.
    pub fn expire(&self) -> Read<bool> {
        match self.handler.get("expire")? {
            Held::Value(V::Bool(flag)) => Ok(*flag),
            _ => Err(Fault::refused(NOT_A_BOOL)),
        }
    }

    /// `handler.operation`, a str.
    pub fn operation(&self) -> Read<Arc<str>> {
        Ok(read::text(self.handler.value("operation")?)?.clone())
    }

    /// What `handler.after_complete` is.
    pub fn after_complete(&self) -> Read<AfterComplete> {
        Ok(match self.handler.get("after_complete")? {
            Held::Value(V::Null) => AfterComplete::None,
            Held::Method(step) if &**step == RETURN_RULE => AfterComplete::Own,
            _ => AfterComplete::Other,
        })
    }

    /// Whether the manager `name` (`toilet_service`, `resource_service`, `service`) has its own
    /// `request` (a test's): False for None.
    pub fn own_request(&self, name: &str) -> Read<bool> {
        Ok(self.handler.object(name)?.is_some_and(|manager| manager.own.contains_key("request")))
    }

    fn truth(&self, name: &str) -> Read<bool> {
        match self.handler.get(name)? {
            Held::Value(value) => Ok(truthy(value)),
            Held::Text(text) => Ok(!text.is_empty()),
            _ => Err(Fault::refused(NOT_TESTABLE)),
        }
    }
}

/// A registered handler.
#[derive(Clone, Debug, PartialEq)]
pub enum Handler {
    Native(Native),
    /// Not a class the core runs, or one whose `apply` was patched: the host runs it.
    Foreign,
}

/// What the Run read of `controller.event_dispatcher`.
#[derive(Clone, Debug, PartialEq)]
pub struct Dispatch {
    /// Whether the dispatcher is of the class the table was installed with (`EventDispatcher`).
    pub known: Read<bool>,
    /// `handlers`, by event type in the dict's order (keys that are not str are left out: no event
    /// type finds them).
    pub handlers: Read<Arc<IndexMap<Arc<str>, Handler>>>,
}

impl Dispatch {
    /// What a Run without a controller (or before its dispatcher was read) holds: every read
    /// refuses with this fault.
    pub fn faulted(fault: Fault) -> Self {
        Self { known: Err(fault.clone()), handlers: Err(fault) }
    }

    /// The handler of an event type: None for none registered.
    pub fn handler(&self, event_type: &str) -> Read<Option<&Handler>> {
        Ok(self.handlers.as_ref().map_err(Fault::clone)?.get(event_type))
    }
}

impl Default for Dispatch {
    fn default() -> Self {
        Self::faulted(Fault::refused(super::held::NO_CONTROLLER))
    }
}
