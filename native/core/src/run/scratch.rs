//! `Scratch`: what one tick's phases hand on to the next ones, kept on the `Run` (lane P8-0b).
//!
//! The update phase's `View` and `Deltas`, the propose phase's proposals and the arbitrate phase's
//! outcomes are values of one tick, not state: nothing saves, hashes or compares them, and the tick
//! clears them when it begins (`begin`). Each lane keeps its own types here by type, so that lanes
//! running at the same time add no field to a shared struct: `put` a value, `get` or `take` it by
//! its type.

use std::any::{Any, TypeId};
use std::collections::HashMap;

/// One tick's values, by type.
#[derive(Default)]
pub struct Scratch {
    /// The minute of the tick the values are of, None before the first tick.
    minute: Option<i64>,
    values: HashMap<TypeId, Box<dyn Any + Send>>,
}

impl std::fmt::Debug for Scratch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scratch").field("minute", &self.minute).field("values", &self.values.len()).finish()
    }
}

impl Scratch {
    /// A tick begins: what the last one kept goes.
    pub fn begin(&mut self, minute: i64) {
        self.values.clear();
        self.minute = Some(minute);
    }

    /// The minute of the tick, None before the first.
    pub fn minute(&self) -> Option<i64> {
        self.minute
    }

    /// Keep a value of the tick, in place of one of its type. The old one, if any.
    pub fn put<T: Any + Send>(&mut self, value: T) -> Option<T> {
        let old = self.values.insert(TypeId::of::<T>(), Box::new(value))?;
        old.downcast::<T>().ok().map(|old| *old)
    }

    pub fn get<T: Any + Send>(&self) -> Option<&T> {
        self.values.get(&TypeId::of::<T>())?.downcast_ref::<T>()
    }

    pub fn get_mut<T: Any + Send>(&mut self) -> Option<&mut T> {
        self.values.get_mut(&TypeId::of::<T>())?.downcast_mut::<T>()
    }

    pub fn take<T: Any + Send>(&mut self) -> Option<T> {
        let value = self.values.remove(&TypeId::of::<T>())?;
        value.downcast::<T>().ok().map(|value| *value)
    }

    /// How many values the tick keeps.
    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}
