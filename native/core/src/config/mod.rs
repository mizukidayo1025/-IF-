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
//! proximity; movement, routing and the rest follow in 7-5a.
//!
//! A section the kernels parse themselves is parsed here from the configs as a `V` (the row of the
//! `model_configs` slot). A section that a Python contract parses keeps what the contract gave, as
//! the types below.

pub mod agent_dynamics;
pub mod body;
pub mod environment;
pub mod geometry;
pub mod information;
pub mod proximity;
pub mod read;
pub mod section;
pub mod selection;
pub mod staff;

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

use agent_dynamics::Awareness;
use body::{BodyModels, CareOutcome};
use environment::{Infrastructure, ZoneEnvironmentSpec};
use information::InformationSpread;
use proximity::Proximity;
use staff::{StaffFatigue, StaffOperations};

/// The sections parsed from one state of `model_configs`.
#[derive(Default)]
pub struct Configs {
    /// The stamp of the `model_configs` slot these were parsed from. None: no slot follows the
    /// configs (a world without a core, or a field assigned whole), so these serve one read.
    pub stamp: Option<Stamp>,
    /// `selected_models(model_configs)`.
    pub selection: Lazy<Selection>,
    /// The plan of the end-of-tick check (`core/tick_validation.py`'s `tick_plan`), a Python
    /// object of the PyO3 crate.
    pub tick_plan: Lazy<Opaque>,
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
}

impl Configs {
    pub fn new(stamp: Option<Stamp>) -> Self {
        Self { stamp, ..Self::default() }
    }
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
