//! The pydantic contracts of inputs (policy E15) without Python: `validate` answers
//! `Model.model_validate(input).model_dump(mode="json")`, or the `ValidationError` as pydantic
//! 2.11.9 writes it, as the native host's `contract` does (Issue #346, part of #250).
//!
//! The native host gives `model_validate` an event's values as the event holds them, frozen
//! (`_freeze_json`: mapping proxies and tuples), and the other inputs as dicts and lists
//! (`contract_input` of `native/src/host.rs`); the texts show which (`input_value=mappingproxy(...)`),
//! so `validate` gives each contract its input the same way. `pydantic` holds the part of pydantic
//! the models use, `models` the models and their validators. `tests/contracts.rs` compares each
//! answer with what Python gave for the same input (`native/core/tests/data/contracts_*.json`).

mod models;
mod pydantic;

use std::sync::Arc;

use crate::error::{Error, Result};
use crate::host::Contract;
use crate::text::py_repr;
use crate::value::V;

use pydantic::{Ctx, In, Model};

/// `Model.model_validate(input).model_dump(mode="json")` of the contract, or the error Python
/// raises: the `ValidationError` (a ValueError) with pydantic's text, or the `KeyError` of an
/// operator `_PARAMETER_MODELS` has no model for.
pub fn validate(which: &Contract, input: &V) -> Result<V> {
    let model: &'static Model = match which {
        Contract::RouteAccess => &models::ROUTE_ACCESS,
        Contract::EquipmentTransfer => &models::EQUIPMENT_TRANSFER,
        Contract::MoveFacility => &models::MOVE_FACILITY,
        Contract::Partition => &models::PARTITION,
        Contract::TemporaryRoom => &models::TEMPORARY_ROOM,
        Contract::EffectParameters(operator) => models::parameter_model(operator).ok_or_else(|| Error::Key(py_repr(operator)))?,
        Contract::Guard => &models::GUARD,
        Contract::ZoneEnvironment => &models::ZONE_ENVIRONMENT,
        Contract::Infrastructure => &models::INFRASTRUCTURE,
        Contract::PopulationSpec => &models::POPULATION_SPEC,
        Contract::ArrivalContext => &models::ARRIVAL_CONTEXT,
        Contract::PersonTraits => &models::PERSON_TRAITS,
        Contract::PresenceCommand => &models::PRESENCE_COMMAND,
        Contract::FacilityGeometry => &models::FACILITY_GEOMETRY,
        Contract::SpatialConstraints => &models::SPATIAL_CONSTRAINTS,
    };
    let given = match (which, input) {
        // `set_staff_role` builds a dict around the payload's frozen skills (lane P8 d3).
        (Contract::EffectParameters(operator), V::Map(_)) if &**operator == "set_staff_role" => In { v: input, frozen: false, kids: true, tracked: false },
        // A guard, an effect's parameters and a presence command are an event's values.
        (Contract::Guard | Contract::EffectParameters(_) | Contract::PresenceCommand, _) => In::frozen(input),
        _ => In::plain(input),
    };
    pydantic::model(model, given, &mut Ctx::default()).map(V::Map).map_err(|errors| Error::Value(pydantic::text(model.name, &errors)))
}

// ---- The contracts the world's validation reads (#351) --------------------------------------------

/// A model `WorldState.validate` reads a world's values with (`validate::zone_environment`,
/// `validate::effect_program`).
pub enum Checked {
    /// `ScenarioZoneEnvironmentConfig`.
    ZoneEnvironment,
    /// The parameters' model of a zone condition's operator (`_PARAMETER_MODELS`).
    Parameters(Arc<str>),
    /// `GuardExpression`.
    Guard,
    /// `CompilerProvenance`.
    CompilerProvenance,
    /// `PlanAssumption`.
    PlanAssumption,
}

/// How Python holds the value it validates: a namespace's or a config's tracked tree, or an
/// event's frozen values.
#[derive(Clone, Copy)]
pub enum Holder {
    Tracked,
    Frozen,
}

/// `Model.model_validate(value).model_dump(mode="json")` of a world's value, or the
/// `ValidationError` (a ValueError) with pydantic's text.
pub fn checked(which: &Checked, input: &V, holder: Holder) -> Result<V> {
    let model: &'static Model = match which {
        Checked::ZoneEnvironment => &models::ZONE_ENVIRONMENT,
        Checked::Parameters(operator) => models::parameter_model(operator).ok_or_else(|| Error::Key(py_repr(operator)))?,
        Checked::Guard => &models::GUARD,
        Checked::CompilerProvenance => &models::COMPILER_PROVENANCE,
        Checked::PlanAssumption => &models::PLAN_ASSUMPTION,
    };
    let given = match holder {
        Holder::Tracked => In::tracked(input),
        Holder::Frozen => In::frozen(input),
    };
    pydantic::model(model, given, &mut Ctx::default()).map(V::Map).map_err(|errors| Error::Value(pydantic::text(model.name, &errors)))
}
