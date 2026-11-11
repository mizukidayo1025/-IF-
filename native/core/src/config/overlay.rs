//! The sections of the overlay's config (lane 7-5a of Issue #152): the schema of
//! `model_configs["geometry_overlay"]` as the bed and intake rules read it, and as the arrivals'
//! check of alternate room admission reads it.

use super::read::{int, map, Read};
use super::section::Fault;
use crate::value::V;

/// `config.get("schema_version")` of `exact_dict(model_configs).get("geometry_overlay")`: None for
/// no config, a config that is not a dict, or no schema; else the schema, an int or refused.
pub fn overlay_schema(configs: &V) -> Read<Option<i64>> {
    let Some(V::Map(config)) = map(configs)?.get("geometry_overlay") else {
        return Ok(None);
    };
    match config.get("schema_version") {
        None | Some(V::Null) => Ok(None),
        Some(schema) => int(schema).map(Some),
    }
}

/// `model_configs.get("geometry_overlay", {})` as the arrivals read it: whether it is a dict, and
/// `schema_version in {3, 4}` of it, or what that raised. The PyO3 crate evaluates both in Python.
#[derive(Clone, PartialEq, Debug)]
pub struct OverlayRooms {
    pub is_dict: bool,
    /// Evaluated only for a dict.
    pub room_schema: Result<bool, Fault>,
}
