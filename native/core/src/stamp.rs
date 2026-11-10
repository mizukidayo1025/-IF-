//! Stamps: numbers that say "this state, as it was then" (design-p7.md §2, Issue #152).
//!
//! A stamp is drawn from one counter of the process and is never issued twice. A slot takes a new
//! stamp at every write (`Slot::touch`), so a cache keyed by a stamp can never mistake a later
//! state for the one it read: not after another write, not in another world, and not after a
//! rollback brings an old state back with its old stamp. A per-slot `+= 1` could give two states
//! the same number.

use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(1);

/// A process-unique number, never zero.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Stamp(NonZeroU64);

impl Stamp {
    /// A stamp no one has had before.
    #[inline]
    pub fn next() -> Stamp {
        // 2^64 draws would take centuries at a draw per nanosecond; the counter never wraps to 0.
        let number = NEXT.fetch_add(1, Ordering::Relaxed);
        Stamp(NonZeroU64::new(number).unwrap_or(NonZeroU64::MAX))
    }

    #[inline]
    pub fn get(self) -> u64 {
        self.0.get()
    }
}
