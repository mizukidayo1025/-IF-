//! A World's canonical bytes: what `WorldState.canonical_json()` writes for the same world (Issue
//! #225, lane M1). The crate's `sheltersim_core::canonical` (#244, PR 4-3) writes them; these are its
//! calls for a test, which panics where the crate gives an error.

use serde_json::Value;
use sheltersim_core::world::World;

/// The canonical JSON of each top-level key of the world's document but `rng_state`, in the order
/// of the keys.
pub fn parts(world: &World) -> Vec<(&'static str, Vec<u8>)> {
    sheltersim_core::canonical::parts(world).unwrap()
}

/// `WorldState.canonical_json()` of the world, with its `rng_state` (the document's).
pub fn canonical_json(world: &World, rng_state: &Value) -> Vec<u8> {
    sheltersim_core::canonical::canonical_json(world, &super::load::v(rng_state)).unwrap()
}
