//! A digest of a test's answers and the world after each (Issue #225, lane M3).
//!
//! A test that records what a rule answered and left (a pytest's `repr` digest, recorded again from
//! the core: decision 1 of #225) adds each call's result and the world after it to `Answers`, in the
//! order of its calls, as the pytests chained theirs. The world is its canonical parts
//! (`canonical::parts`: the canonical JSON of each top-level key of `WorldState.canonical_json()`
//! but `rng_state`), a line each with the key's name.

use sheltersim_core::checkpoint::sha256_hex;
use sheltersim_core::world::World;

/// The world as bytes: each canonical part's name and JSON, a line each.
pub fn world_bytes(world: &World) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 << 16);
    for (name, raw) in super::canonical::parts(world) {
        out.extend_from_slice(name.as_bytes());
        out.push(b' ');
        out.extend_from_slice(&raw);
        out.push(b'\n');
    }
    out
}

/// A test's answers, chained in the order of its calls: what each call returned (as text) and the
/// world after it.
#[derive(Default)]
pub struct Answers {
    raw: Vec<u8>,
}

impl Answers {
    pub fn add(&mut self, result: &str, world: &World) {
        self.raw.extend_from_slice(result.as_bytes());
        self.raw.push(b'\n');
        self.raw.extend_from_slice(&world_bytes(world));
    }

    /// An answer that leaves no world behind (a read).
    pub fn text(&mut self, result: &str) {
        self.raw.extend_from_slice(result.as_bytes());
        self.raw.push(b'\n');
    }

    /// The sha256 of the answers so far.
    pub fn hex(&self) -> String {
        sha256_hex(&self.raw)
    }
}
