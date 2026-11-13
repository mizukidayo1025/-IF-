//! The configs of a world as the kernels read them, kept by the stamp of `model_configs`
//! (design-p7.md §2–§4, Issue #152, lane 7-1; policy E7 of docs/rust-port/step5-plan.md).
//!
//! `Configs` is a value derived from the `model_configs` slot: it is good while the slot has the
//! stamp it was made for. Every write to the configs gives the slot a new stamp (`Slot::touch`), so
//! the next read finds no `Configs` of that stamp and parses again. A rollback that brings an old
//! state back brings its old stamp back too, and the second entry of `Derived` then still holds it.
//! A stamp is never issued twice in a process, so a hit is never a later state.
//!
//! The PyO3 crate parses, with the Python contracts, at the first read of each section
//! (`src/core/configs.rs`). Sections join lane by lane: P7-1 has the model selection and the plan
//! of the end-of-tick check, 7-2 the geometry and the plans built from it, 7-5b the staff, the
//! formulas, information, the body, agent dynamics, the zone environment, infrastructure and
//! proximity; 7-5a movement, routing, the queue policy, the overlay's schema, the resource
//! service, the population's needs and the simulator extensions.
//!
//! A section the kernels parse themselves is parsed here from the configs as a `V` (the row of the
//! `model_configs` slot). A section that a Python contract parses keeps what the contract gave, as
//! the types below.

pub mod agent_dynamics;
pub mod body;
pub mod environment;
pub mod geometry;
pub mod information;
pub mod movement;
pub mod overlay;
pub mod population;
pub mod proximity;
pub mod queues;
pub mod read;
pub mod section;
pub mod selection;
pub mod service;
pub mod staff;
pub mod tick_plan;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::error::Result;
use crate::geometry::GeoCache;
use crate::row::ValueRow;
use crate::stamp::Stamp;
use crate::value::V;
use crate::world::{World, MODEL_CONFIGS};

pub use geometry::{ApproachRoute, FacilityGeometry, FacilityPlan, SpatialConstraints};
pub use section::{Fault, FaultClass, Lazy, Opaque, Section};
pub use selection::Selection;
pub use tick_plan::{ResourceNumbers, TickPlan};

use agent_dynamics::Awareness;
use body::{BodyModels, CareOutcome};
use environment::{Infrastructure, ZoneEnvironmentSpec};
use information::InformationSpread;
use movement::{Movement, RouteCapacities};
use overlay::OverlayRooms;
use population::AccessibilityNeeds;
use proximity::Proximity;
use queues::QueueConfig;
use service::ResourceService;
use staff::{StaffFatigue, StaffOperations};

/// The sections parsed from one state of `model_configs`.
#[derive(Default)]
pub struct Configs {
    /// The stamp of the `model_configs` slot these were parsed from. None: no slot follows the
    /// configs (a world without a core, or a field assigned whole), so these serve one read.
    pub stamp: Option<Stamp>,
    /// `selected_models(model_configs)`.
    pub selection: Lazy<Selection>,
    /// The plan of the end-of-tick check (`core/tick_validation.py`'s `tick_plan`), as the PyO3
    /// crate read it from the Python contracts (lane P8 d4).
    pub tick_plan: Lazy<TickPlan>,
    /// `facility_geometry`, parsed. Absent where the key is missing or None.
    pub facility_geometry: Lazy<FacilityGeometry>,
    /// `spatial_constraints`, or `{}` where the key is missing, parsed.
    pub spatial_constraints: Lazy<SpatialConstraints>,
    /// The plans (`Geo`) built from these configs, by the overlay (`geometry_overlay_v1`).
    pub geo: Mutex<GeoCache>,
    // ---- Lane 7-5b ----
    /// `create_staff_fatigue_formula`, for `update_staff_dynamics`.
    pub staff_fatigue: Lazy<StaffFatigue>,
    /// `create_staff_operations_formula` and whether `policy_registry` selects it.
    pub staff_operations: Lazy<StaffOperations>,
    /// `assign_staff_operations`' check of the selected task formula.
    pub staff_operations_known: Lazy<()>,
    /// `model_configs["staff"]["support_task_minutes"]` as the route waits read it.
    pub support_task_minutes: Lazy<i64>,
    /// `model_configs.get("staff", {}).get("support_cooldown_minutes", 30)`.
    pub support_cooldown_minutes: Lazy<i64>,
    /// `InformationSpreadConfig.from_mapping(model_configs.get("information_spread"))`.
    pub information_spread: Lazy<InformationSpread>,
    /// The operators of `body_state_v1`.
    pub body_models: Lazy<BodyModels>,
    /// `body_dynamics.social_interaction_minutes`.
    pub social_interaction_minutes: Lazy<i64>,
    /// `create_care_outcome_formula`, read where a care formula is selected.
    pub care_outcome: Lazy<CareOutcome>,
    /// The policy of `agent_dynamics_v1`.
    pub agent_dynamics_policy: Lazy<&'static str>,
    /// `facility_awareness`.
    pub facility_awareness: Lazy<Option<Awareness>>,
    /// Whether the configs limit what an agent knows of the facilities.
    pub knowledge_is_limited: Lazy<bool>,
    /// `ScenarioZoneEnvironmentConfig.model_validate(model_configs["zone_environment"])`.
    pub zone_environment: Lazy<ZoneEnvironmentSpec>,
    /// `ScenarioInfrastructureSpec.model_validate(model_configs["infrastructure"])`.
    pub infrastructure: Lazy<Infrastructure>,
    /// `canonical_proximity_contact_config(model_configs.get("social_proximity"))`.
    pub proximity: Lazy<Option<Proximity>>,
    // ---- Lane 7-5a ----
    /// `movement_model` and whether the configs time routes.
    pub movement: Lazy<Movement>,
    /// `layout_routing` as the end-of-tick check reads it.
    pub route_capacities: Lazy<Option<RouteCapacities>>,
    /// `model_configs.get("layout_routing", {}).get("max_agents_in_transit_by_edge", {})`, truthy.
    pub transit_capacities: Lazy<bool>,
    /// `create_queue_policy` of the two bundled policies.
    pub facility_queues: Lazy<QueueConfig>,
    /// The schema of `geometry_overlay`'s config, for the beds and intake.
    pub overlay_schema: Lazy<Option<i64>>,
    /// `geometry_overlay` as the arrivals' alternate room admission reads it.
    pub overlay_rooms: Lazy<OverlayRooms>,
    /// `resource_service`.
    pub resource_service: Lazy<ResourceService>,
    /// The population's accessibility needs, for the queue choice.
    pub accessibility_needs: Lazy<AccessibilityNeeds>,
    /// `simulator_extensions.get("enabled")`, truthy, for the world view.
    pub extensions_enabled: Lazy<bool>,
}

impl Configs {
    pub fn new(stamp: Option<Stamp>) -> Self {
        Self { stamp, ..Self::default() }
    }
}

// ---- Each section by name, for a host that fills it (lane P9-0, H2 of design-p9.md) ------------------

macro_rules! sections {
    ($($(#[$doc:meta])* $id:ident => $field:ident, $by:ident;)*) => {
        /// A section of `Configs`: one `Lazy` of it. A rule's reader that finds its section empty
        /// for the configs of the world now asks the host to fill it (`Host::fill_section`) before
        /// it borrows the world, then reads it from the configs again.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum SectionId {
            $($(#[$doc])* $id,)*
        }

        impl SectionId {
            /// Every section, in the order of the fields of `Configs`.
            pub const ALL: &'static [SectionId] = &[$(SectionId::$id),*];

            /// The name of the field of `Configs`.
            pub fn name(self) -> &'static str {
                match self {
                    $(SectionId::$id => stringify!($field),)*
                }
            }

            /// Whether these configs hold the section: parsed, absent or a fault.
            pub fn is_filled(self, configs: &Configs) -> bool {
                match self {
                    $(SectionId::$id => configs.$field.get().is_some(),)*
                }
            }

            /// Who parses it: a Python contract or factory (`Parser::Host`), or the core's own parser
            /// of the configs' `V` (`Parser::Value`), which a host runs only where the world's
            /// `model_configs` has no row.
            pub fn parser(self) -> Parser {
                match self {
                    $(SectionId::$id => Parser::$by,)*
                }
            }
        }
    };
}

/// Who parses a section (`SectionId::parser`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Parser {
    /// A Python contract or factory, which only the host runs.
    Host,
    /// A parser of the core, from the `V` of the `model_configs` slot.
    Value,
}

sections! {
    /// `selected_models(model_configs)`.
    Selection => selection, Host;
    /// The plan of the end-of-tick check.
    TickPlan => tick_plan, Host;
    /// `FacilityGeometry`.
    FacilityGeometry => facility_geometry, Host;
    /// `SpatialConstraints`.
    SpatialConstraints => spatial_constraints, Host;
    StaffFatigue => staff_fatigue, Value;
    /// `create_staff_operations_formula`.
    StaffOperations => staff_operations, Host;
    StaffOperationsKnown => staff_operations_known, Value;
    SupportTaskMinutes => support_task_minutes, Value;
    /// `model_configs.get("staff", {}).get("support_cooldown_minutes", 30)`, read in Python.
    SupportCooldownMinutes => support_cooldown_minutes, Host;
    /// `InformationSpreadConfig.from_mapping`.
    InformationSpread => information_spread, Host;
    /// The body's operators, by the model selection.
    BodyModels => body_models, Host;
    SocialInteractionMinutes => social_interaction_minutes, Value;
    /// `create_care_outcome_formula`.
    CareOutcome => care_outcome, Host;
    AgentDynamicsPolicy => agent_dynamics_policy, Value;
    FacilityAwareness => facility_awareness, Value;
    KnowledgeIsLimited => knowledge_is_limited, Value;
    /// `ScenarioZoneEnvironmentConfig`.
    ZoneEnvironment => zone_environment, Host;
    /// `ScenarioInfrastructureSpec`.
    Infrastructure => infrastructure, Host;
    /// `canonical_proximity_contact_config`.
    Proximity => proximity, Host;
    Movement => movement, Value;
    RouteCapacities => route_capacities, Value;
    /// `layout_routing.max_agents_in_transit_by_edge`, read in Python.
    TransitCapacities => transit_capacities, Host;
    FacilityQueues => facility_queues, Value;
    OverlaySchema => overlay_schema, Value;
    /// `geometry_overlay`, read in Python.
    OverlayRooms => overlay_rooms, Host;
    ResourceService => resource_service, Value;
    AccessibilityNeeds => accessibility_needs, Value;
    /// `simulator_extensions.get("enabled")`, read in Python.
    ExtensionsEnabled => extensions_enabled, Host;
}

/// Where a host filled a section (`Host::fill_section`).
pub enum Filled {
    /// In the configs it was given, which keep it for their stamp.
    Kept,
    /// In these other configs, which serve this read alone: the section is not kept for the stamp
    /// (a test has patched the model selection, whose sections are then parsed at every read).
    Once(Arc<Configs>),
}

static PARSES: AtomicU64 = AtomicU64::new(0);

/// A section is parsed now: at its first read for a stamp, or at every read of configs no slot
/// follows. `config_parses()` of the PyO3 crate reads the count, so a parse the core runs counts as
/// one the PyO3 crate runs.
pub fn count_parse() {
    PARSES.fetch_add(1, Ordering::Relaxed);
}

/// How many sections were parsed in this process.
pub fn parses() -> u64 {
    PARSES.load(Ordering::Relaxed)
}

/// A section the kernels parse themselves, parsed from the configs as the world holds them: the
/// `V` of the `model_configs` slot's row. None where the slot has no such row (no slot, or a value
/// that is not JSON), for the caller to read the tree instead; a missing row makes no refusal.
pub fn parse_configs<T>(world: &World, parse: impl FnOnce(&V) -> read::Read<T>) -> Result<Option<Section<T>>> {
    let Some(slot) = world.slots.get(MODEL_CONFIGS) else {
        return Ok(None);
    };
    let slot = slot.try_borrow()?;
    let row = slot.row.as_deref().and_then(|row| row.as_any().downcast_ref::<ValueRow>());
    Ok(row.map(|row| parse(&row.0).into()))
}

/// What a world keeps that is not its state: values derived from it, keyed by stamps. Nothing
/// canonicalizes, hashes, saves or compares it; dropping it costs a parse and changes no result.
#[derive(Default)]
pub struct Derived {
    /// The configs of the two most recent stamps, the latest first.
    configs: [Option<Arc<Configs>>; 2],
    /// The care records the world's frames read so far (`frame::CareMemo`, #264).
    pub care_history: Option<crate::frame::CareMemo>,
}

impl Derived {
    /// The configs of this stamp: kept, or new with nothing parsed, which drops the older entry.
    pub fn configs(&mut self, stamp: Stamp) -> Arc<Configs> {
        let held = |entry: &Option<Arc<Configs>>| entry.as_ref().is_some_and(|configs| configs.stamp == Some(stamp));
        if !held(&self.configs[0]) {
            if held(&self.configs[1]) {
                self.configs.swap(0, 1);
            } else {
                self.configs[1] = self.configs[0].take();
            }
        }
        self.configs[0].get_or_insert_with(|| Arc::new(Configs::new(Some(stamp)))).clone()
    }

    /// The stamps kept, the latest first (for tests).
    pub fn stamps(&self) -> [Option<Stamp>; 2] {
        [0, 1].map(|index| self.configs[index].as_ref().and_then(|configs| configs.stamp))
    }
}
