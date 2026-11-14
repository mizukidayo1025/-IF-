//! The whole world's validation without Python (policy E6 of docs/rust-port/step5-plan.md; Issue
//! #250, #347): `WorldState.validate` of `core/world.py`, `validate_simulator_extensions` of
//! `domain/simulator_state.py` and the candidate's wrapping of `_txn_validate`
//! (`core/state_mirror.py`), each with the class and the text of Python's first error.
//!
//! The entry points are what a host without Python answers its validation hooks with
//! (`plain::PlainHost`): `world` (`world.validate()`, with or without the event history, with an
//! event in flight), `extensions` (`validate_simulator_extensions(world)`, its errors as they are)
//! and `candidate` (`world.validate()` of a transaction's candidate). The native host keeps
//! calling Python.
//!
//! This file holds the order of the rules, which is Python's: each part is a function of its own
//! file, named after what Python calls, and an error passes on as Python's does (wrapped where
//! Python wraps it). Each part reads the world through a `View` (`view.rs`) and gives Python's
//! errors as the core's (`Error::Invariant` is `StateInvariantError`, `Error::Value` a
//! `ValueError`, ...). A part not ported yet passes (each is marked `STUB(lane ...)`); the lanes of
//! #250 fill them in their own files. See /home/water7/i263-notes/i250/framework.md (and the PR of
//! #347) for who owns what.
//!
//! The history memo of a save (`_history_memo`, `core/save_validation.py`), which skips the entries
//! of append-only histories that passed before, is not here: a later Issue may give `View` one.

mod agent_dynamics;
mod body_state;
pub mod catalog;
mod declared;
mod effect_program;
mod entities;
mod events;
mod extension_head;
mod facility_queues;
mod geometry_overlay;
mod information;
mod infrastructure;
mod models;
mod movement;
mod operational_events;
mod population_dynamics;
mod population_lifecycle;
mod psychology;
pub mod py;
mod resource_ledger;
pub mod resource_service;
pub mod selection;
mod shared_operations;
pub mod signatures;
mod social_network;
mod staff_dynamics;
mod staff_operations;
mod temporary_spaces;
pub mod view;
mod zone_environment;

pub use view::{repr, repr_texts, StateRef, View};

use crate::error::{Error, Result};
use crate::events::EventRow;
use crate::host::{ErrorClass, HostFailure};
use crate::text::py_repr;
use crate::world::World;

/// What `WorldState.validate` is asked: its keyword arguments.
#[derive(Clone, Copy, Debug)]
pub struct Options<'a> {
    /// `validate_event_history` (True by default).
    pub history: bool,
    /// `_in_flight_event`.
    pub in_flight: Option<&'a EventRow>,
}

impl Default for Options<'_> {
    fn default() -> Self {
        Self { history: true, in_flight: None }
    }
}

// ---- Python's errors ------------------------------------------------------------------------------

/// Whether Python's `except ValueError` catches the error: a `ValueError` or one of its subclasses
/// (`StateInvariantError`, `ResourceLedgerError`, the outcomes, a refusal), as
/// `HostFailure::class` says.
pub fn is_value_error(error: &Error) -> bool {
    error.class() == ErrorClass::Value
}

/// Whether the error is a `StateInvariantError`.
pub fn is_invariant(error: &Error) -> bool {
    matches!(error, Error::Invariant(_) | Error::Corrupt { .. })
}

/// `str(exc)`: a `KeyError`'s text is the repr of its key.
pub fn py_str(error: &Error) -> String {
    match error {
        Error::Key(key) => py_repr(key),
        other => other.to_string(),
    }
}

/// `except ValueError as exc: raise StateInvariantError(f"{prefix}{exc}")`.
pub fn wrap_value(prefix: &'static str) -> impl FnOnce(Error) -> Error {
    move |error| if is_value_error(&error) { Error::Invariant(format!("{prefix}{}", py_str(&error))) } else { error }
}

// ---- The entry points -----------------------------------------------------------------------------

/// `world.validate(validate_event_history=options.history, _in_flight_event=options.in_flight)`.
pub fn world(world: &World, options: &Options<'_>) -> Result<()> {
    validate(&View::new(world, options.history, options.in_flight))
}

/// `validate_simulator_extensions(world, validate_event_history=history)`: its errors as they are
/// (`ValueError`, `KeyError`, `TypeError`).
pub fn extensions(world: &World, history: bool) -> Result<()> {
    simulator_extensions(&View::new(world, history, None))
}

/// `_txn_validate(world, candidate=True)`: `world.validate()`, an error other than a
/// `StateInvariantError` wrapped as `WorldState.from_dict` wraps it.
pub fn candidate(world: &World) -> Result<()> {
    match self::world(world, &Options::default()) {
        Err(error) if !is_invariant(&error) && (is_value_error(&error) || matches!(error, Error::Key(_) | Error::Type(_))) => {
            Err(Error::Invariant(format!("invalid WorldState document: {}", py_str(&error))))
        }
        other => other,
    }
}

// ---- WorldState.validate --------------------------------------------------------------------------

/// The keys of `model_state` whose presence has `validate` check the extensions.
const EXTENSION_KEYS: [&str; 9] = [
    "movement_v1",
    "agent_dynamics_v1",
    "social_network_v1",
    "staff_dynamics_v1",
    "population_dynamics_v1",
    "body_state_v1",
    "staff_operations_v1",
    "temporary_spaces_v1",
    "shared_operations_v1",
];

const SHARED_OPERATIONS: &str = "shared_operations_v1";
const LEDGER: &str = "resource_ledger_v1";

/// `WorldState.validate`, its parts in Python's order.
fn validate(view: &View<'_>) -> Result<()> {
    let version = view.schema_version()?;
    if version != 5 && version != 6 {
        return Err(Error::Invariant(format!("unsupported world-state schema version: {version}")));
    }
    let selection = selection::selected_models(view)
        .and_then(|selection| declared::validate(view).map(|()| selection))
        .map_err(wrap_value(""))?;
    if has_extensions(view)? {
        simulator_extensions(view).map_err(|error| {
            if is_value_error(&error) || matches!(error, Error::Key(_) | Error::Type(_)) {
                Error::Invariant("invalid simulator extension state".to_owned())
            } else {
                error
            }
        })?;
    }
    let service = resource_service::validate(view)?;
    information::validate(view)?;
    psychology::validate(view, &selection)?;
    operational_events::validate(view).map_err(wrap_value("invalid operational events state: "))?;
    infrastructure::validate(view).map_err(wrap_value("invalid infrastructure state: "))?;
    zone_environment::validate(view).map_err(wrap_value("invalid zone environment state: "))?;
    effect_program::validate(view).map_err(wrap_value("invalid EffectProgram state: "))?;
    entities::validate(view, service.as_ref())?;
    if view.contains(LEDGER)? {
        resource_ledger::validate(view).map_err(wrap_value("invalid resource ledger: "))?;
    }
    events::validate(view)
}

/// `extension_keys.intersection(self.model_state) or simulator_signatures(self.model_configs)`:
/// the signatures' error passes on as it is.
fn has_extensions(view: &View<'_>) -> Result<bool> {
    for key in EXTENSION_KEYS {
        if view.contains(key)? {
            return Ok(true);
        }
    }
    Ok(!signatures::simulator_signatures(view)?.is_empty())
}

// ---- validate_simulator_extensions ----------------------------------------------------------------

/// `validate_simulator_extensions`, its parts in Python's order.
fn simulator_extensions(view: &View<'_>) -> Result<()> {
    extension_head::validate(view)?;
    agent_dynamics::validate(view)?;
    body_state::validate(view)?;
    population_dynamics::validate(view)?;
    population_lifecycle::validate(view)?;
    social_network::validate(view)?;
    staff_dynamics::validate(view)?;
    staff_operations::validate(view)?;
    temporary_spaces::validate(view)?;
    movement::validate(view)?;
    geometry_overlay::validate(view)?;
    facility_queues::validate(view)?;
    if view.contains(SHARED_OPERATIONS)? && !view.has_config("shared_operations")? {
        return Err(Error::Value("shared_operations_v1 requires its signed Scenario contract".to_owned()));
    }
    shared_operations::validate(view)
}
