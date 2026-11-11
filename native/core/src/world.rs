//! The state of one world, without Python (design-p5-p6.md §2–§4, Issue #152, lane 5-1).
//!
//! `World` is held by the PyO3 crate's `WorldCore` (`world.__dict__["_core"]`). The Python objects
//! (the tracked trees of the namespaces, the entity objects and their `IdMap`s) are views the
//! PyO3 crate keeps next to it, in `NamespaceMirror` and `EntityStore`. Each part is a `Shard`, so a
//! shim borrows the one part it needs (rule R3 of the design), and a kernel that holds the whole
//! world reaches every part with `get_mut`.
//!
//! Every str key of `model_state` has a slot: a namespace a lane owns has its typed row, and any
//! other key a `ValueRow` slot (`Place::Value`, lane 5-3), made when the key is first stored.
//!
//! Parts that join later, each in its own lane: the scalars, zones and zone edges, `resources` and
//! `id_counters` (5-2); the event queue and the log (6-5). The field `derived` (configs and caches
//! keyed by stamps, design-p7.md §2, lane 7-1) is not state: a snapshot, a fork or a hash never
//! reads it.

use std::sync::Arc;

use indexmap::IndexMap;
use rustc_hash::FxBuildHasher;

use crate::cell::{Shard, ShardMut, ShardRef};
use crate::config::Derived;
use crate::entities::Entities;
use crate::error::{Error, Result};
use crate::row::{Row, ValueRow};
use crate::stamp::Stamp;
use crate::value::V;

/// The state of one world.
#[derive(Default)]
pub struct World {
    /// The rows of the entities (`world.agents` and the others).
    pub entities: Shard<Entities>,
    /// The owned world fields (`metric_state`) and the keys of `model_state`.
    pub slots: Slots,
    /// Not state: what is derived from it, by stamp (`config::Derived`).
    pub derived: Shard<Derived>,
}

/// Where a slot's state is in the Python world.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Place {
    /// A field of the world, `world.<name>`, such as `metric_state`.
    Field,
    /// A key of `world.model_state` whose row a lane wrote.
    Namespace,
    /// Any other key of `world.model_state`, stored as a `ValueRow`. Its slot is made when the key
    /// is first stored and stays: while the key is absent it has no row.
    Value,
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

    /// The value of a key held as a `ValueRow`, for a kernel that holds the whole world: None for a
    /// key the world does not have. Unsupported for a tainted key (its value does not fit `V`) and
    /// for a slot with another row.
    #[track_caller]
    pub fn value(&mut self, name: &str) -> Result<Option<&V>> {
        match self.by_name.get_mut(name) {
            None => Ok(None),
            Some(slot) => slot_value(slot.get_mut()).map(|value| value.map(|row| &row.0)),
        }
    }

    /// `value`, with the slot borrowed on its own, for a shim that holds the world shared.
    #[track_caller]
    pub fn try_value(&self, name: &str) -> Result<Option<ShardRef<'_, V>>> {
        let Some(slot) = self.by_name.get(name) else {
            return Ok(None);
        };
        let slot = slot.try_borrow()?;
        if slot_value(&slot)?.is_none() {
            return Ok(None);
        }
        ShardRef::try_map(slot, |slot| slot.row::<ValueRow>().map(|row| &row.0)).map(Some)
    }
}

#[track_caller]
fn slot_value(slot: &Slot) -> Result<Option<&ValueRow>> {
    if slot.row.is_none() && slot.taint.is_none() {
        return Ok(None);
    }
    slot.row::<ValueRow>().map(Some)
}

#[allow(dead_code)]
fn assert_world_is_shareable() {
    fn check<T: Send + Sync>() {}
    check::<World>();
}
