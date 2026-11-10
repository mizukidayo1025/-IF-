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
//! The scalars (`Scalars`), the zones and zone edges (tables of `Entities`), `resources` and
//! `id_counters` (slots of `Place::Field`) joined in 5-2. Parts that join later, each in its own
//! lane: the event queue and the log (6-5). The field `derived` (configs and caches keyed by
//! stamps, design-p7.md §2, lane 7-1) is not state: a snapshot, a fork or a hash never reads it.

use std::sync::Arc;

use indexmap::IndexMap;
use rustc_hash::FxBuildHasher;

use crate::cell::{Shard, ShardMut, ShardRef};
use crate::config::Derived;
use crate::entities::Entities;
use crate::error::{Error, Result};
use crate::row::{IdMap, Row, ValueRow};
use crate::stamp::Stamp;
use crate::value::V;

/// The state of one world.
#[derive(Default)]
pub struct World {
    /// `scenario_id`, `seed`, `duration_minutes`, `minute_index` and `schema_version`.
    pub scalars: Shard<Scalars>,
    /// The rows of the entities (`world.agents` and the others, `world.zones`, `world.zone_edges`).
    pub entities: Shard<Entities>,
    /// The owned world fields (`metric_state`, `model_configs`, `resources`, `id_counters`) and the
    /// keys of `model_state`.
    pub slots: Slots,
    /// Not state: what is derived from it, by stamp (`config::Derived`).
    pub derived: Shard<Derived>,
}

// ---- Scalars ---------------------------------------------------------------------------------

/// A value of the world as Rust holds it, or `Misfit` for a Python value of another type. A kernel
/// that reads a misfit says Unsupported, as reading the attribute with `exact_int` says today.
#[derive(Clone, Debug, PartialEq, Default)]
pub enum Fit<T> {
    Ok(T),
    #[default]
    Misfit,
}

impl<T> Fit<T> {
    /// The value, or None for a misfit.
    pub fn ok(&self) -> Option<&T> {
        match self {
            Fit::Ok(value) => Some(value),
            Fit::Misfit => None,
        }
    }

    /// The value, or Unsupported with this reason.
    #[track_caller]
    pub fn get(&self, reason: &'static str) -> Result<&T> {
        self.ok().ok_or_else(|| Error::unsupported(reason))
    }
}

/// `world.seed`: an int of any size from Python. The text is `str(seed)`, which `KeyedDraws` keys
/// its draws with; `small` is the value when it fits an i64.
#[derive(Clone, Debug, PartialEq)]
pub struct Seed {
    pub text: Arc<str>,
    pub small: Option<i64>,
}

impl Seed {
    /// The seed of this decimal text (`str(int)`: digits, a `-` before them for a negative one),
    /// or None for any other text.
    pub fn from_text(text: &str) -> Option<Seed> {
        let digits = text.strip_prefix('-').unwrap_or(text);
        let decimal = !digits.is_empty()
            && digits.bytes().all(|byte| byte.is_ascii_digit())
            && (digits == "0" || !digits.starts_with('0'))
            && text != "-0";
        decimal.then(|| Seed { text: Arc::from(text), small: text.parse().ok() })
    }
}

/// The scalars of a world. The PyO3 crate writes each from `WorldState.__setattr__` and when it
/// starts the core; a value written past `__setattr__` (`world.__dict__[name] = ...`) is not seen.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scalars {
    pub scenario_id: Fit<Arc<str>>,
    pub seed: Fit<Seed>,
    pub duration_minutes: Fit<i64>,
    pub minute_index: Fit<i64>,
    pub schema_version: Fit<i64>,
}

/// The names of the scalars, as attributes of `WorldState`.
pub const SCALAR_NAMES: [&str; 5] = ["scenario_id", "seed", "duration_minutes", "minute_index", "schema_version"];

impl Scalars {
    #[track_caller]
    pub fn scenario_id(&self) -> Result<&Arc<str>> {
        self.scenario_id.get("the scenario ID is not a str")
    }

    #[track_caller]
    pub fn seed(&self) -> Result<&Seed> {
        self.seed.get("the seed is not an int")
    }

    #[track_caller]
    pub fn duration_minutes(&self) -> Result<i64> {
        self.duration_minutes.get("the duration is not an int").copied()
    }

    #[track_caller]
    pub fn minute_index(&self) -> Result<i64> {
        self.minute_index.get("the minute is not an int").copied()
    }

    #[track_caller]
    pub fn schema_version(&self) -> Result<i64> {
        self.schema_version.get("the schema version is not an int").copied()
    }
}

// ---- Owned world fields --------------------------------------------------------------------

/// `world.resources`: stock by resource ID, a slot of `Place::Field`.
pub const RESOURCES: &str = "resources";
/// `world.id_counters`: a `ValueRow` slot of `Place::Field`, so a counter past i64 still fits.
pub const ID_COUNTERS: &str = "id_counters";

/// The row of `world.resources`.
pub type ResourcesRow = IdMap<i64>;

/// An ID counter of a `ValueRow` of `id_counters`: None without the key, Unsupported for a value
/// that is not an int of an i64, or for a row that is not a map.
#[track_caller]
pub fn id_counter(counters: &ValueRow, name: &str) -> Result<Option<i64>> {
    let V::Map(map) = &counters.0 else {
        return Err(Error::unsupported("id_counters is not a map"));
    };
    match map.get(name) {
        None => Ok(None),
        Some(V::Int(number)) => Ok(Some(*number)),
        Some(_) => Err(Error::unsupported("an ID counter is not an int")),
    }
}

/// Set an ID counter of a `ValueRow` of `id_counters`: a new key goes to the end, as in a dict.
pub fn set_id_counter(counters: &mut ValueRow, name: &str, value: i64) -> Result<()> {
    let V::Map(map) = &mut counters.0 else {
        return Err(Error::unsupported("id_counters is not a map"));
    };
    match map.get_mut(name) {
        Some(slot) => *slot = V::Int(value),
        None => {
            map.insert(Arc::from(name), V::Int(value));
        }
    }
    Ok(())
}

impl World {
    /// The scalars, borrowed shared: for a shim that holds the world shared.
    pub fn scalars(&self) -> Result<ShardRef<'_, Scalars>> {
        self.scalars.try_borrow()
    }

    #[track_caller]
    pub fn minute_index(&self) -> Result<i64> {
        self.scalars.try_borrow()?.minute_index()
    }

    #[track_caller]
    pub fn duration_minutes(&self) -> Result<i64> {
        self.scalars.try_borrow()?.duration_minutes()
    }

    #[track_caller]
    pub fn schema_version(&self) -> Result<i64> {
        self.scalars.try_borrow()?.schema_version()
    }

    /// The seed's text, which `KeyedDraws` takes.
    #[track_caller]
    pub fn seed_text(&self) -> Result<Arc<str>> {
        Ok(self.scalars.try_borrow()?.seed()?.text.clone())
    }

    /// `world.resources` as its row, borrowed on its own; Unsupported while the field holds no row.
    #[track_caller]
    pub fn resources(&self) -> Result<ShardRef<'_, ResourcesRow>> {
        self.slots.try_row::<ResourcesRow>(RESOURCES)
    }

    /// `world.id_counters.get(name)` from its row.
    #[track_caller]
    pub fn id_counter(&self, name: &str) -> Result<Option<i64>> {
        id_counter(&*self.slots.try_row::<ValueRow>(ID_COUNTERS)?, name)
    }
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
