//! The state of one world, without Python (design-p5-p6.md §2–§4, Issue #152, lane 5-1).
//!
//! `World` is held by the PyO3 crate's `WorldCore` (`world.__dict__["_core"]`). The Python objects
//! (the tracked trees of the namespaces, the entity objects and their `IdMap`s) are views the
//! PyO3 crate keeps next to it, in `NamespaceMirror` and `EntityStore`. Each part is a `Shard`, so a
//! shim borrows the one part it needs (rule R3 of the design), and a kernel that holds the whole
//! world reaches every part with `get_mut`.
//!
//! Parts that join later, each in its own lane: the scalars, zones and zone edges, `resources` and
//! `id_counters` (5-2); the other namespaces as `ValueRow`s (5-3); the event queue and the log
//! (6-5). A non-state field `derived` (configs and caches keyed by stamps, design-p7.md §2) joins
//! in 7-1; it is not state, so a snapshot, a fork or a hash never reads it.

use std::sync::Arc;

use indexmap::IndexMap;
use rustc_hash::FxBuildHasher;

use crate::cell::{Shard, ShardMut, ShardRef};
use crate::entities::Entities;
use crate::error::{Error, Result};
use crate::row::Row;
use crate::stamp::Stamp;

/// The state of one world.
#[derive(Default)]
pub struct World {
    /// The rows of the entities (`world.agents` and the others).
    pub entities: Shard<Entities>,
    /// The owned world fields (`metric_state`) and the owned namespaces of `model_state`.
    pub slots: Slots,
}

/// Where a slot's state is in the Python world.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Place {
    /// A field of the world, `world.<name>`, such as `metric_state`.
    Field,
    /// A key of `world.model_state`.
    Namespace,
}

/// Counters for the budget of a lane (`NamespaceMirror.stats`).
#[derive(Copy, Clone, Default, PartialEq, Eq, Debug)]
pub struct SlotCounts {
    /// Writes forwarded from the tree to the row.
    pub forwards: u64,
    /// Whole values stored (`replace_root`, `load_detached`).
    pub whole_writes: u64,
    /// Trees built again from the row after a detach.
    pub rebuilds: u64,
}

/// One owned field or namespace: its row, and what the PyO3 crate's mirror says of it.
pub struct Slot {
    pub place: Place,
    /// None: the namespace is absent, or tainted (`taint` says why).
    pub row: Option<Box<dyn Row>>,
    /// Why the row was dropped: a write that does not fit. The Python rules then run on the tree.
    pub taint: Option<String>,
    /// The row alone holds the state; Python builds the tree from it when it reads it.
    pub detached: bool,
    /// Changes with every write to the row or its tree, from Python or from Rust: while it and the
    /// tree stand, the state is what it was (`generation_of`). A process-unique stamp, never `+= 1`.
    pub generation: Stamp,
    pub counts: SlotCounts,
}

impl Slot {
    pub fn new(place: Place) -> Self {
        Self { place, row: None, taint: None, detached: false, generation: Stamp::next(), counts: SlotCounts::default() }
    }

    /// Something was written: a new generation.
    #[inline]
    pub fn touch(&mut self) {
        self.generation = Stamp::next();
    }

    /// Drop the row, for a write that does not fit.
    pub fn misfit(&mut self, why: String) {
        self.row = None;
        self.taint = Some(why);
    }

    /// The row as its type, or Unsupported (the kernels' answer for a tainted or absent namespace).
    #[track_caller]
    pub fn row_mut<T: Row>(&mut self) -> Result<&mut T> {
        let row = self.row.as_deref_mut().ok_or_else(|| Error::unsupported("the namespace has no row"))?;
        row.as_any_mut().downcast_mut::<T>().ok_or_else(|| Error::unsupported("the namespace has another row"))
    }

    #[track_caller]
    pub fn row<T: Row>(&self) -> Result<&T> {
        let row = self.row.as_deref().ok_or_else(|| Error::unsupported("the namespace has no row"))?;
        row.as_any().downcast_ref::<T>().ok_or_else(|| Error::unsupported("the namespace has another row"))
    }
}

/// The slots of a world, by name in the order they were registered. A slot is never removed, so
/// its index stays its own: the PyO3 crate's mirror keeps it, and finds its slot without a lookup.
/// The order of `model_state` is the Python table's, not this.
#[derive(Default)]
pub struct Slots {
    by_name: IndexMap<Arc<str>, Shard<Slot>, FxBuildHasher>,
}

impl Slots {
    /// Register a slot, with no state yet; its index. A name owned already is a `ValueError`.
    pub fn add(&mut self, name: Arc<str>, place: Place) -> Result<usize> {
        if self.by_name.contains_key(&name) {
            return Err(Error::Value(format!("{name} is owned already")));
        }
        let (index, _) = self.by_name.insert_full(name, Shard::new(Slot::new(place)));
        Ok(index)
    }

    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }

    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.by_name.get_index_of(name)
    }

    /// The names in the order they were registered.
    pub fn names(&self) -> impl Iterator<Item = &Arc<str>> {
        self.by_name.keys()
    }

    #[inline]
    pub fn at(&self, index: usize) -> Option<&Shard<Slot>> {
        self.by_name.get_index(index).map(|(_, slot)| slot)
    }

    #[inline]
    pub fn get(&self, name: &str) -> Option<&Shard<Slot>> {
        self.by_name.get(name)
    }

    #[track_caller]
    fn owned(&self, name: &str) -> Result<&Shard<Slot>> {
        self.by_name.get(name).ok_or_else(|| Error::unsupported("the namespace is not owned"))
    }

    /// The row of a slot as its type, for a kernel that holds the whole world.
    #[track_caller]
    pub fn row_mut<T: Row>(&mut self, name: &str) -> Result<&mut T> {
        let slot = self.by_name.get_mut(name).ok_or_else(|| Error::unsupported("the namespace is not owned"))?;
        slot.get_mut().row_mut::<T>()
    }

    /// The row of a slot as its type, borrowed on its own, for a shim that holds the world shared.
    #[track_caller]
    pub fn try_row_mut<T: Row>(&self, name: &str) -> Result<ShardMut<'_, T>> {
        let slot = self.owned(name)?.try_borrow_mut()?;
        ShardMut::try_map(slot, |slot| slot.row_mut::<T>())
    }

    /// `try_row_mut`, shared.
    #[track_caller]
    pub fn try_row<T: Row>(&self, name: &str) -> Result<ShardRef<'_, T>> {
        let slot = self.owned(name)?.try_borrow()?;
        ShardRef::try_map(slot, |slot| slot.row::<T>())
    }
}

#[allow(dead_code)]
fn assert_world_is_shareable() {
    fn check<T: Send + Sync>() {}
    check::<World>();
}
