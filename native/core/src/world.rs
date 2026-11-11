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
//! `id_counters` (slots of `Place::Field`) joined in 5-2, the event queue (`Queue`) and the log
//! (`EventLog`) in 6-5. The field `derived` (configs and caches keyed by stamps, design-p7.md §2,
//! lane 7-1) is not state: a snapshot, a fork or a hash never reads it.
//!
//! The queue and the log are the world's while its Python `EventQueue` and `EventLog` are Rust
//! stores attached to its core (`queue_held`, `log_held`): the PyO3 crate's `QueueStore` and
//! `EventStore` are then handles that keep only the Python objects of the events and records, and
//! reach the events and records here. A store that belongs to no world holds its own, and moves it
//! in when it is attached (`hold_queue`, `hold_log`) and out again when it is let go
//! (`release_queue`, `release_log`); nothing is copied either way.
//!
//! A kernel of the core reads the world through the typed accessors of `World` (`rows`, `row`,
//! `metric_state`, `configs`, the scalars), each a shared borrow of its part. Who holds the world
//! hands it over as `&mut World` (`WorldCell::with`, the PyO3 crate's `with_world`), lane 5-4.
//!
//! The rows of the slots are the truth (lane P8-0c, as 6-1 made the entity rows): a kernel of the
//! core writes a row alone (`row_mut`, `Slots::row_mut`, `Slots::try_row_mut`), which marks its
//! slot (`Slot::written`) and gives it a new stamp. When the PyO3 crate's borrow of the world ends,
//! before any Python runs, it takes the marks (`Slots::take_written`) and patches each attached
//! tree from its row; a detached slot has no tree, and its row is all there is.
//!
//! The parts of the state (the scalars, the entities, each slot) are `CowShard`s (lane 6-2): a
//! `Snapshot` keeps their `Arc`s, a write copies a part a snapshot shares, and `restore` gives the
//! snapshot back. A transaction (`txn.rs`) takes one, then puts the world in the order a copy
//! through canonical JSON gives it (`canonicalize`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use indexmap::IndexMap;
use rustc_hash::FxBuildHasher;

use crate::cell::{CowShard, Shard, ShardMut, ShardRef};
use crate::config::{Configs, Derived};
use crate::entities::Entities;
use crate::error::{Error, Result};
use crate::events::log::{EventLog, LogMark};
use crate::events::queue::Queue;
use crate::row::{IdMap, Row, ValueRow};
use crate::stamp::Stamp;
use crate::state::metric_state::MetricRow;
use crate::value::V;

/// The state of one world.
#[derive(Default)]
pub struct World {
    /// `scenario_id`, `seed`, `duration_minutes`, `minute_index` and `schema_version`.
    pub scalars: CowShard<Scalars>,
    /// The rows of the entities (`world.agents` and the others, `world.zones`, `world.zone_edges`).
    pub entities: CowShard<Entities>,
    /// The owned world fields (`metric_state`, `model_configs`, `resources`, `id_counters`) and the
    /// keys of `model_state`.
    pub slots: Slots,
    /// Not state: what is derived from it, by stamp (`config::Derived`).
    pub derived: Shard<Derived>,
    /// The pending events (`EventQueue`), while `queue_held`; empty otherwise.
    pub queue: CowShard<Queue>,
    /// The realized records (`EventLog`), while `log_held`; empty otherwise.
    pub log: CowShard<EventLog>,
    /// Not state: whether `queue` is the world's event queue. False while the world's queue is
    /// held in Python, or by a store attached to another world's core.
    pub queue_held: bool,
    /// Not state: whether `log` is the world's EventLog, as `queue_held` says of the queue. A copy
    /// that holds the EventLog of the world it was made from (`_effect_candidate`) does not hold it.
    pub log_held: bool,
    /// Not state: a transaction is open on the world (`txn::Txn`), which does not nest (rule R8).
    pub(crate) txn_open: bool,
}

/// The state parts of a world at one moment (lane 6-2): the scalars, the entities, every slot and
/// the event queue, each the `Arc` its `CowShard` held, and where the log was (`EventLog::mark`,
/// lane 6-5): a rollback takes the records appended since out, so a snapshot does not share the
/// log, whose first write would copy all of it. Taking it copies nothing; the next write to a part
/// copies that part.
pub struct Snapshot {
    scalars: Arc<Scalars>,
    entities: Arc<Entities>,
    /// By slot index: the slots registered after the snapshot are not here.
    slots: Vec<Arc<Slot>>,
    queue: Arc<Queue>,
    log: LogMark,
}

impl Snapshot {
    /// How many slots the world had.
    pub fn slot_count(&self) -> usize {
        self.slots.len()
    }

    /// A slot as it was.
    pub fn slot(&self, index: usize) -> Option<&Slot> {
        self.slots.get(index).map(|slot| &**slot)
    }

    /// The entities as they were.
    pub fn entities(&self) -> &Entities {
        &self.entities
    }

    pub fn scalars(&self) -> &Scalars {
        &self.scalars
    }

    /// The event queue as it was: its serials, which `World::canonicalize` gave new ones.
    pub fn queue(&self) -> &Queue {
        &self.queue
    }

    /// Where the log was.
    pub fn log(&self) -> &LogMark {
        &self.log
    }
}

// ---- Who holds the world ------------------------------------------------------------------

/// A world that a core function borrows for a while and gives back (policy E4, rules R2 and R6 of
/// design-p5-p6.md). `with` holds the whole world exclusively while `f` runs; a function that calls
/// a Host borrows between its calls, never across one. The PyO3 crate's cell is the world's
/// `WorldCore` (`native/src/core/world_core.rs`, `with_world`).
pub trait WorldCell {
    /// Run `f` on the world. `Error::Runtime(BORROWED)` while any of it is borrowed.
    fn with<R>(&self, f: impl FnOnce(&mut World) -> R) -> Result<R>;
}

/// A world held by Rust alone (tests, and a build without Python).
impl WorldCell for Shard<World> {
    fn with<R>(&self, f: impl FnOnce(&mut World) -> R) -> Result<R> {
        let mut world = self.try_borrow_mut()?;
        Ok(f(&mut world))
    }
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
/// `world.metric_state`: a slot of `Place::Field` whose row is a `MetricRow`.
pub const METRIC_STATE: &str = "metric_state";
/// `world.model_configs`: a slot of `Place::Field`; its stamp keys `Derived`.
pub const MODEL_CONFIGS: &str = "model_configs";

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
    /// The state parts now (`Snapshot`), for the one who holds the whole world.
    pub fn snapshot(&mut self) -> Snapshot {
        Snapshot {
            scalars: self.scalars.share_mut(),
            entities: self.entities.share_mut(),
            slots: self.slots.by_name.values_mut().map(CowShard::share_mut).collect(),
            queue: self.queue.share_mut(),
            log: self.log.share_mut().mark(),
        }
    }

    /// Give a snapshot back: every part as it was then. A slot registered since has no state
    /// again (a key of `model_state` first stored after the snapshot), as a deleted key has. The
    /// rows of the entities are all marked changed, since the document bytes the PyO3 crate keeps
    /// by slot may be of rows written since. The queue comes back with its serials; the log loses
    /// the records appended since (`EventLog::rollback`).
    pub fn restore(&mut self, snapshot: Snapshot) {
        self.queue.put(snapshot.queue);
        self.log.get_mut().rollback(snapshot.log);
        self.scalars.put(snapshot.scalars);
        self.entities.put(snapshot.entities);
        self.entities.get_mut().mark_all_changed();
        let count = snapshot.slots.len();
        for (slot, old) in self.slots.by_name.values_mut().zip(snapshot.slots) {
            slot.put(old);
        }
        for slot in self.slots.by_name.values_mut().skip(count) {
            let place = slot.get_mut().place;
            *slot.get_mut() = Slot::new(place);
        }
    }

    /// Put the world in the order `WorldState.from_dict(world.to_dict())` gives it (design §1.3
    /// F1, lane 6-2): each table of entities by the entity's ID (`Entities::canonicalize`), the
    /// keys of every map of every row sorted (`Row::canonicalize`), the queue scheduled again in
    /// queue order under new serials (`Queue::canonicalize`). Every slot gets a new stamp, as
    /// every namespace of a new world has one; the scalars and the log are as they were. The PyO3
    /// crate makes its Python containers in the same order (the views of the rows), and its queue
    /// store finds the old serials in the snapshot.
    pub fn canonicalize(&mut self) {
        self.entities.get_mut().canonicalize();
        self.queue.get_mut().canonicalize();
        for slot in self.slots.by_name.values_mut() {
            let slot = slot.get_mut();
            if let Some(row) = slot.row.as_deref_mut() {
                row.canonicalize();
            }
            slot.touch();
        }
    }

    /// A new world with the same state, for `deepcopy(world)` and the previews that change a copy
    /// and drop it (lane 6-6, design-p5-p6.md §6 and §7). It shares every part with this world
    /// (the scalars, the entities, each slot, in the same slot order and with the same stamps: a
    /// stamp names one state, and both hold it; the queue and the log, lane 6-5) until either
    /// writes the part, which copies it then (`CowShard`). The order of every table and row is this world's. What is derived
    /// (`derived`) is not state, so the fork starts with none; it has no open transaction.
    /// `Error::Runtime(BORROWED)` while a part is borrowed mutably.
    pub fn fork(&self) -> Result<World> {
        let mut by_name = IndexMap::with_capacity_and_hasher(self.slots.len(), FxBuildHasher);
        for (name, slot) in &self.slots.by_name {
            by_name.insert(name.clone(), CowShard::from_shared(slot.share()?));
        }
        Ok(World {
            scalars: CowShard::from_shared(self.scalars.share()?),
            entities: CowShard::from_shared(self.entities.share()?),
            slots: Slots { by_name, any_written: AtomicBool::new(false) },
            derived: Shard::default(),
            queue: CowShard::from_shared(self.queue.share()?),
            log: CowShard::from_shared(self.log.share()?),
            queue_held: self.queue_held,
            log_held: self.log_held,
            txn_open: false,
        })
    }

    /// Whether a transaction is open on the world (`txn::Txn`).
    pub fn in_transaction(&self) -> bool {
        self.txn_open
    }

    /// The world's event queue, borrowed shared; Unsupported while the world's queue is not held
    /// here (`queue_held`).
    #[track_caller]
    pub fn queue(&self) -> Result<ShardRef<'_, Queue>> {
        if !self.queue_held {
            return Err(Error::unsupported("the event queue is not in Rust"));
        }
        self.queue.try_borrow()
    }

    /// `queue`, borrowed mutably.
    #[track_caller]
    pub fn queue_mut(&self) -> Result<ShardMut<'_, Queue>> {
        if !self.queue_held {
            return Err(Error::unsupported("the event queue is not in Rust"));
        }
        self.queue.try_borrow_mut()
    }

    /// The world's EventLog, borrowed shared; Unsupported while it is not held here (`log_held`).
    #[track_caller]
    pub fn log(&self) -> Result<ShardRef<'_, EventLog>> {
        if !self.log_held {
            return Err(Error::unsupported("the event log is not in Rust"));
        }
        self.log.try_borrow()
    }

    /// `log`, borrowed mutably.
    #[track_caller]
    pub fn log_mut(&self) -> Result<ShardMut<'_, EventLog>> {
        if !self.log_held {
            return Err(Error::unsupported("the event log is not in Rust"));
        }
        self.log.try_borrow_mut()
    }

    /// Hold this queue as the world's (a store attached to the world's core), nothing copied.
    /// Whatever queue was held before comes out.
    pub fn hold_queue(&mut self, queue: Arc<Queue>) -> Option<Arc<Queue>> {
        let before = self.release_queue();
        self.queue.put(queue);
        self.queue_held = true;
        before
    }

    /// Let the world's queue go (its store leaves the core), nothing copied: the queue held, or
    /// None. The world then holds none.
    pub fn release_queue(&mut self) -> Option<Arc<Queue>> {
        let held = std::mem::replace(&mut self.queue_held, false);
        let queue = self.queue.replace(Arc::default());
        held.then_some(queue)
    }

    /// `hold_queue` of the EventLog.
    pub fn hold_log(&mut self, log: Arc<EventLog>) -> Option<Arc<EventLog>> {
        let before = self.release_log();
        self.log.put(log);
        self.log_held = true;
        before
    }

    /// `release_queue` of the EventLog.
    pub fn release_log(&mut self) -> Option<Arc<EventLog>> {
        let held = std::mem::replace(&mut self.log_held, false);
        let log = self.log.replace(Arc::default());
        held.then_some(log)
    }

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

    /// The rows of the entities, borrowed shared. Unsupported while the people's tables are
    /// tainted, as the PyO3 crate's `EntityStore::with_rows` says.
    #[track_caller]
    pub fn rows(&self) -> Result<ShardRef<'_, Entities>> {
        let rows = self.entities.try_borrow()?;
        if rows.tainted().is_some() {
            return Err(Error::unsupported("the entity store is tainted"));
        }
        Ok(rows)
    }

    /// `rows`, borrowed mutably.
    #[track_caller]
    pub fn rows_mut(&self) -> Result<ShardMut<'_, Entities>> {
        let rows = self.entities.try_borrow_mut()?;
        if rows.tainted().is_some() {
            return Err(Error::unsupported("the entity store is tainted"));
        }
        Ok(rows)
    }

    /// The row of an owned field or namespace as its type, borrowed shared: Unsupported when the
    /// name is not owned, has no row (absent or tainted) or has a row of another type.
    #[track_caller]
    pub fn row<T: Row>(&self, name: &str) -> Result<ShardRef<'_, T>> {
        self.slots.try_row::<T>(name)
    }

    /// `row`, borrowed mutably.
    #[track_caller]
    pub fn row_mut<T: Row>(&self, name: &str) -> Result<ShardMut<'_, T>> {
        self.slots.try_row_mut::<T>(name)
    }

    /// `world.metric_state` as its row.
    #[track_caller]
    pub fn metric_state(&self) -> Result<ShardRef<'_, MetricRow>> {
        self.row::<MetricRow>(METRIC_STATE)
    }

    /// The stamp of a slot now: it changes with every write to the slot's row or tree. None when
    /// the name is not owned.
    pub fn stamp_of(&self, name: &str) -> Result<Option<Stamp>> {
        match self.slots.get(name) {
            None => Ok(None),
            Some(slot) => Ok(Some(slot.try_borrow()?.generation)),
        }
    }

    /// The configs parsed from the state of `model_configs` that has this stamp (`Derived`): kept,
    /// or new with nothing parsed yet.
    pub fn configs(&self, stamp: Stamp) -> Result<Arc<Configs>> {
        Ok(self.derived.try_borrow_mut()?.configs(stamp))
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
    /// A kernel of the core wrote the row since the PyO3 crate last took the marks
    /// (`Slots::take_written`): its tree, if it has one, does not follow the row yet. Not state.
    pub written: bool,
    /// The keys of a map row the kernels wrote since the marks were taken, when they wrote only
    /// those (`row_keys_written`): the tree follows those keys alone. None for a write of the whole
    /// row (or no write). Not state.
    pub keys: Option<Vec<Arc<str>>>,
}

/// A copy of the row too (`Row::clone_row`), for a snapshot's copy on write.
impl Clone for Slot {
    fn clone(&self) -> Self {
        Self {
            place: self.place,
            row: self.row.as_ref().map(|row| row.clone_row()),
            taint: self.taint.clone(),
            detached: self.detached,
            generation: self.generation,
            counts: self.counts,
            written: self.written,
            keys: self.keys.clone(),
        }
    }
}

impl Slot {
    pub fn new(place: Place) -> Self {
        Self {
            place,
            row: None,
            taint: None,
            detached: false,
            generation: Stamp::next(),
            counts: SlotCounts::default(),
            written: false,
            keys: None,
        }
    }

    /// The row as its type, to write from a kernel of the core: the slot gets a new stamp and its
    /// mark (`written`), so its tree follows the row when the world is given back. Unsupported as
    /// `row_mut`, with nothing marked.
    #[track_caller]
    pub fn row_written<T: Row>(&mut self) -> Result<&mut T> {
        self.row::<T>()?;
        self.touch();
        self.written = true;
        self.keys = None;
        self.row_mut::<T>()
    }

    /// `row_written` of a map row of which the kernel writes these keys alone (each set to the
    /// row's value, or gone): the tree follows those keys when the world is given back, unless a
    /// write of the whole row comes too.
    #[track_caller]
    pub fn row_keys_written<T: Row>(&mut self, keys: &[&str]) -> Result<&mut T> {
        self.row::<T>()?;
        self.touch();
        if !self.written {
            self.written = true;
            self.keys = Some(Vec::with_capacity(keys.len()));
        }
        if let Some(known) = &mut self.keys {
            known.extend(keys.iter().map(|key| Arc::from(*key)));
        }
        self.row_mut::<T>()
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
    by_name: IndexMap<Arc<str>, CowShard<Slot>, FxBuildHasher>,
    /// Some slot may be marked (`Slot::written`): `take_written` looks at the slots only then. An
    /// atomic, so that kernels writing different slots at once mark them without a shared borrow.
    any_written: AtomicBool,
}

impl Slots {
    /// Register a slot, with no state yet; its index. A name owned already is a `ValueError`.
    pub fn add(&mut self, name: Arc<str>, place: Place) -> Result<usize> {
        if self.by_name.contains_key(&name) {
            return Err(Error::Value(format!("{name} is owned already")));
        }
        let (index, _) = self.by_name.insert_full(name, CowShard::new(Slot::new(place)));
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
    pub fn at(&self, index: usize) -> Option<&CowShard<Slot>> {
        self.by_name.get_index(index).map(|(_, slot)| slot)
    }

    #[inline]
    pub fn get(&self, name: &str) -> Option<&CowShard<Slot>> {
        self.by_name.get(name)
    }

    #[track_caller]
    fn owned(&self, name: &str) -> Result<&CowShard<Slot>> {
        self.by_name.get(name).ok_or_else(|| Error::unsupported("the namespace is not owned"))
    }

    /// The row of a slot as its type, for a kernel that holds the whole world. The slot is marked
    /// written (`Slot::row_written`), so its tree follows the row when the world is given back.
    #[track_caller]
    pub fn row_mut<T: Row>(&mut self, name: &str) -> Result<&mut T> {
        let slot = self.by_name.get_mut(name).ok_or_else(|| Error::unsupported("the namespace is not owned"))?;
        let row = slot.get_mut().row_written::<T>()?;
        *self.any_written.get_mut() = true;
        Ok(row)
    }

    /// The row of a slot as its type, borrowed on its own, for a kernel or a shim that holds the
    /// world shared. The slot is marked written, as by `row_mut`.
    #[track_caller]
    pub fn try_row_mut<T: Row>(&self, name: &str) -> Result<ShardMut<'_, T>> {
        let slot = self.owned(name)?.try_borrow_mut()?;
        let row = ShardMut::try_map(slot, |slot| slot.row_written::<T>())?;
        self.any_written.store(true, Ordering::Relaxed);
        Ok(row)
    }

    /// `try_row_mut` of a map row of which the kernel writes these keys alone
    /// (`Slot::row_keys_written`).
    #[track_caller]
    pub fn try_row_keys_mut<T: Row>(&self, name: &str, keys: &[&str]) -> Result<ShardMut<'_, T>> {
        let slot = self.owned(name)?.try_borrow_mut()?;
        let row = ShardMut::try_map(slot, |slot| slot.row_keys_written::<T>(keys))?;
        self.any_written.store(true, Ordering::Relaxed);
        Ok(row)
    }

    /// `take_written`, with the keys each slot's tree is to follow (`Slot::keys`): None for the
    /// whole row.
    pub fn take_written_keys(&mut self) -> Vec<(usize, Option<Vec<Arc<str>>>)> {
        let written = self.take_written();
        written
            .into_iter()
            .map(|index| {
                let keys = self.by_name.get_index_mut(index).and_then(|(_, slot)| slot.get_mut().keys.take());
                (index, keys)
            })
            .collect()
    }

    /// The indexes of the slots a kernel of the core wrote since the last call, in index order,
    /// and their marks cleared: the PyO3 crate patches their trees from the rows (lane P8-0c). A
    /// slot no kernel wrote is not copied (`CowShard`) to read its mark.
    pub fn take_written(&mut self) -> Vec<usize> {
        if !std::mem::replace(self.any_written.get_mut(), false) {
            return Vec::new();
        }
        let mut written = Vec::new();
        for (index, slot) in self.by_name.values_mut().enumerate() {
            if slot.try_borrow().is_ok_and(|slot| slot.written) {
                slot.get_mut().written = false;
                written.push(index);
            }
        }
        written
    }

    /// The names of the slots marked written now, the marks kept: for a check that every borrow
    /// was followed by its patch (`--verify-rows`).
    pub fn written_names(&self) -> Vec<Arc<str>> {
        if !self.any_written.load(Ordering::Relaxed) {
            return Vec::new();
        }
        self.by_name
            .iter()
            .filter(|(_, slot)| slot.try_borrow().is_ok_and(|slot| slot.written))
            .map(|(name, _)| name.clone())
            .collect()
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
