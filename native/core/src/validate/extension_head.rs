//! The head of `validate_simulator_extensions` (`domain/simulator_state.py`): `temporary_spaces_v1`
//! without its contract, the legacy queue marker, and the required states that are missing, with
//! Python's errors and texts (lane D of #250, #349). Raw errors: `validate::extensions` calls it
//! first.
//!
//! `is_legacy_queue_checkpoint` is also what `validate_facility_queue_state` reads
//! (`facility_queues.rs`), with `signatures::independent_queues_required`.

use std::collections::BTreeSet;

use super::signatures::{independent_queues_required, simulator_signatures, FACILITY_QUEUES, TEMPORARY_SPACES};
use super::{repr_texts, View};
use crate::error::{Error, Result};
use crate::value::V;

/// What the extensions' Python rules do with a value of any type, which every port of lane D reads.
#[path = "extension_py.rs"]
pub(super) mod py;

/// `FACILITY_QUEUES_LEGACY_MODE_KEY`.
pub const LEGACY_MODE: &str = "facility_queues_legacy_mode_v1";

/// `is_legacy_queue_checkpoint(world)`, a kernel of Python's: independent queues are required,
/// `facility_queues_v1` is absent (or None), and the legacy marker is `FACILITY_QUEUES_LEGACY_MODE`.
pub fn is_legacy_queue_checkpoint(view: &View<'_>) -> Result<bool> {
    let required = match &*view.configs()? {
        V::Map(configs) => independent_queues_required(configs)?,
        _ => false,
    };
    if !required {
        return Ok(false);
    }
    if view.state(FACILITY_QUEUES)?.is_some_and(|state| !matches!(*state, V::Null)) {
        return Ok(false);
    }
    let Some(mode) = view.state(LEGACY_MODE)? else {
        return Ok(false);
    };
    let V::Map(mode) = &*mode else {
        return Ok(false);
    };
    Ok(py::keys_are(mode, &["schema_version", "mode", "policy_version"])
        && py::eq_int(mode.get("schema_version"), 1)
        && py::eq_str(mode.get("mode"), "pre_independent_queue_checkpoint")
        && py::eq_str(mode.get("policy_version"), "facility_fifo_legacy-v1"))
}

/// The check, as `validate` (`validate/mod.rs`) calls it in Python's order.
pub fn validate(view: &View<'_>) -> Result<()> {
    let signatures = simulator_signatures(view)?;
    let required: BTreeSet<&str> = signatures.iter().filter_map(|(key, _)| key.strip_prefix("simulator:")).collect();
    if view.contains(TEMPORARY_SPACES)? && !required.contains(TEMPORARY_SPACES) {
        return Err(Error::Value("temporary_spaces_v1 requires an enabled, signed state contract".to_owned()));
    }
    let mut missing = BTreeSet::new();
    for key in &required {
        if !view.contains(key)? {
            missing.insert(*key);
        }
    }
    let legacy = is_legacy_queue_checkpoint(view)?;
    if legacy {
        missing.remove(FACILITY_QUEUES);
    }
    if view.contains(LEGACY_MODE)? && !legacy {
        return Err(Error::Value("legacy facility queue checkpoint marker is invalid or mixed with new state".to_owned()));
    }
    if !missing.is_empty() {
        return Err(Error::Value(format!("required simulator state is missing: {}", repr_texts(missing))));
    }
    Ok(())
}
