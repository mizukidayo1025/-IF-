//! The entities of a world as rows: agents with their needs, facilities, staff, support requests,
//! active tasks, and the zones and zone edges (docs/rust-port/lanes/p6-entity-windows.md).
//!
//! Each row is a struct named like the dataclass of `domain/models.py`, and `write_json` writes
//! the bytes `canonical_json` writes for it in a WorldState document of a version. A `Table` holds
//! the rows of one `IdMap`, keys in the dict's order; `Entities` holds the seven tables of a world.
//! The zones and zone edges have a taint of their own: a zone that does not fit leaves the people's
//! rows alone, and the other way round.
//!
//! The Python objects, their links to the rows and the reads and writes from Python are the PyO3
//! crate's (`native/src/state/entities.rs`, `EntityStore`). The rows are the truth (decision 1 of
//! docs/rust-port/step5-plan.md, PR 6-1): a Python object is a view of its row, which follows the
//! row's writes and the core's inserts and removals when the writing borrow ends (`Table`).

use std::sync::Arc;

use crate::error::Result;
use crate::json::{write_json_string, Encoder};
use crate::row::IdMap;
use crate::str_enum;
use crate::value::V;

// ---- Kinds -----------------------------------------------------------------------------------

/// The table of an object, as the PyO3 crate's `EntityStore` and Python's `IdMap._table` number it.
pub const AGENT: u8 = 0;
/// An agent's needs: its object has its own number, its row is in the agent's.
pub const NEEDS: u8 = 1;
pub const FACILITY: u8 = 2;
pub const STAFF: u8 = 3;
pub const REQUEST: u8 = 4;
pub const TASK: u8 = 5;
/// `world.zones` (Issue #152, PR 5-2).
pub const ZONE: u8 = 6;
/// `world.zone_edges`.
pub const EDGE: u8 = 7;

/// Whether the kind is a zone's or a zone edge's, which have their own taint.
pub const fn is_zone_kind(kind: u8) -> bool {
    kind == ZONE || kind == EDGE
}

// ---- Typed fields ----------------------------------------------------------------------------

/// A field of an entity row. Not the namespaces' `row::Field`: a WorldState document of v5 leaves
/// out some fields of v6, so writing takes the document's version.
pub trait EntityField: Sized {
    /// Whether the two values are the same: the bits of a float.
    fn same(&self, other: &Self) -> bool;
    /// The bytes `canonical_json` writes for this value in a WorldState document of this version.
    fn write_json(&self, encoder: &mut Encoder, version: i64) -> Result<()>;
    /// Sort the keys of every map inside, as a copy through canonical JSON does (`row::Field`).
    fn canonicalize(&mut self) {}
}

/// The fields of a row one by one, as bits in the order of the row's `NAMES`: what a view of the
/// row follows (`Table`).
pub trait RowFields {
    /// A bit per field whose value is not the same as other's (`EntityField::same`: the bits of a
    /// float).
    fn diff(&self, other: &Self) -> u32;
    /// Copy the fields of the mask's bits into other.
    fn copy_fields(&self, mask: u32, other: &mut Self);
}

/// A row of a table: what orders the rows of a document.
pub trait EntityRow: EntityField {
    /// The entity's own ID, by which `WorldState._canonical_data` sorts the rows.
    fn id(&self) -> &str;
}

fn write_strings(encoder: &mut Encoder, items: &[Arc<str>]) {
    encoder.out.push(b'[');
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            encoder.out.push(b',');
        }
        write_json_string(&mut encoder.out, item);
    }
    encoder.out.push(b']');
}

impl EntityField for f64 {
    fn same(&self, other: &Self) -> bool {
        self.to_bits() == other.to_bits()
    }
    fn write_json(&self, encoder: &mut Encoder, _version: i64) -> Result<()> {
        encoder.try_float(*self)
    }
}

impl EntityField for i64 {
    fn same(&self, other: &Self) -> bool {
        self == other
    }
    fn write_json(&self, encoder: &mut Encoder, _version: i64) -> Result<()> {
        encoder.int(*self);
        Ok(())
    }
}

impl EntityField for bool {
    fn same(&self, other: &Self) -> bool {
        self == other
    }
    fn write_json(&self, encoder: &mut Encoder, _version: i64) -> Result<()> {
        encoder.bool(*self);
        Ok(())
    }
}

impl EntityField for Arc<str> {
    fn same(&self, other: &Self) -> bool {
        self == other
    }
    fn write_json(&self, encoder: &mut Encoder, _version: i64) -> Result<()> {
        write_json_string(&mut encoder.out, self);
        Ok(())
    }
}

impl<T: EntityField> EntityField for Option<T> {
    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Some(mine), Some(theirs)) => mine.same(theirs),
            (None, None) => true,
            _ => false,
        }
    }
    fn write_json(&self, encoder: &mut Encoder, version: i64) -> Result<()> {
        match self {
            Some(value) => value.write_json(encoder, version),
            None => {
                encoder.null();
                Ok(())
            }
        }
    }
}

/// An ID list: a list of a facility or a staff member.
impl EntityField for Vec<Arc<str>> {
    fn same(&self, other: &Self) -> bool {
        self == other
    }
    fn write_json(&self, encoder: &mut Encoder, _version: i64) -> Result<()> {
        write_strings(encoder, self);
        Ok(())
    }
}

/// `StaffState.skills`: a tuple of str, which a document writes as a list.
#[derive(Clone, PartialEq)]
pub struct Skills(pub Vec<Arc<str>>);

impl EntityField for Skills {
    fn same(&self, other: &Self) -> bool {
        self == other
    }
    fn write_json(&self, encoder: &mut Encoder, _version: i64) -> Result<()> {
        write_strings(encoder, &self.0);
        Ok(())
    }
}

impl EntityField for V {
    fn same(&self, other: &Self) -> bool {
        self == other
    }
    fn write_json(&self, encoder: &mut Encoder, _version: i64) -> Result<()> {
        V::write_json(self, encoder)
    }
    fn canonicalize(&mut self) {
        V::canonicalize(self);
    }
}

/// An enum field: a member of the Python enum, as its number in the order the enum lists them.
/// The PyO3 crate checks at `install` that each member's `.value` is the text here.
macro_rules! member_field {
    ($name:ident, $all:ident, [$($member:ident = $text:literal),+ $(,)?]) => {
        str_enum! { pub enum $name { $($member = $text),+ } }

        /// The members in the order of the Python enum.
        pub const $all: &[$name] = &[$($name::$member),+];

        impl $name {
            /// The member's place in the Python enum.
            pub const fn index(self) -> usize {
                self as usize
            }
        }

        impl EntityField for $name {
            fn same(&self, other: &Self) -> bool {
                self == other
            }
            fn write_json(&self, encoder: &mut Encoder, _version: i64) -> Result<()> {
                write_json_string(&mut encoder.out, self.text());
                Ok(())
            }
        }
    };
}

member_field!(Activity, ACTIVITIES, [
    Idle = "idle",
    Moving = "moving",
    Queued = "queued",
    InService = "in_service",
    SeekingHelp = "seeking_help",
    Sleeping = "sleeping",
    Resting = "resting",
]);
member_field!(Mobility, MOBILITIES, [Independent = "independent", Assisted = "assisted", Wheelchair = "wheelchair"]);
member_field!(RequestStatus, STATUSES, [
    Pending = "pending",
    Assigned = "assigned",
    Resolved = "resolved",
    Unresolved = "unresolved",
]);

// ---- Rows ------------------------------------------------------------------------------------

/// A document holds this field only in WorldState v6.
fn v6<T>(_value: &T, version: i64) -> bool {
    version == 6
}

/// A document leaves this field out while it is None.
fn set<T>(value: &Option<T>, _version: i64) -> bool {
    value.is_some()
}

macro_rules! keeps_field {
    ($value:expr, $version:expr) => {
        true
    };
    ($value:expr, $version:expr, $keep:ident) => {
        $keep($value, $version)
    };
}

/// A struct of fields, each named as the attribute of the Python object. `name[rule]` says when a
/// document holds the field (`v6`, `set`). `NAMES` lists the fields in their order, for the PyO3
/// crate's `py_entity_row!`, which must list the same.
macro_rules! entity_row {
    ($name:ident { $($field:ident $([$keep:ident])?: $kind:ty),* $(,)? }) => {
        #[derive(Clone)]
        pub struct $name {
            $(pub $field: $kind,)*
        }

        impl $name {
            /// The fields in the order they are listed.
            pub const NAMES: &'static [&'static str] = &[$(stringify!($field)),*];
        }

        impl RowFields for $name {
            fn diff(&self, other: &Self) -> u32 {
                let mut mask = 0;
                let mut bit = 1;
                $(
                    if !self.$field.same(&other.$field) {
                        mask |= bit;
                    }
                    bit <<= 1;
                )*
                let _ = bit;
                mask
            }

            fn copy_fields(&self, mask: u32, other: &mut Self) {
                let mut bit = 1;
                $(
                    if mask & bit != 0 {
                        other.$field = self.$field.clone();
                    }
                    bit <<= 1;
                )*
                let _ = bit;
            }
        }

        impl EntityField for $name {
            fn same(&self, other: &Self) -> bool {
                true $(&& self.$field.same(&other.$field))*
            }
            fn canonicalize(&mut self) {
                $(EntityField::canonicalize(&mut self.$field);)*
            }
            fn write_json(&self, encoder: &mut Encoder, version: i64) -> Result<()> {
                const ORDER: [usize; $name::NAMES.len()] = crate::row::sorted_order($name::NAMES);
                type Writer = fn(&$name, &mut Encoder, i64, &mut bool) -> Result<()>;
                const WRITERS: &[Writer] = &[$(|row, encoder, version, first| {
                    if !keeps_field!(&row.$field, version $(, $keep)?) {
                        return Ok(());
                    }
                    if !*first {
                        encoder.out.push(b',');
                    }
                    *first = false;
                    write_json_string(&mut encoder.out, stringify!($field));
                    encoder.out.push(b':');
                    EntityField::write_json(&row.$field, encoder, version)
                }),*];
                let mut first = true;
                encoder.out.push(b'{');
                for index in ORDER {
                    WRITERS[index](self, encoder, version, &mut first)?;
                }
                encoder.out.push(b'}');
                Ok(())
            }
        }
    };
}

entity_row!(NeedRow { toilet_urgency: f64, hunger: f64, fatigue: f64, hydration_need[v6]: f64 });

entity_row!(ProfileRow {
    id: Arc<str>,
    group_id: Option<Arc<str>>,
    initial_zone_id: Arc<str>,
    mobility: Mobility,
    support_need: f64,
    stress_sensitivity: f64,
    rng_key: Arc<str>,
    initial_needs: NeedRow,
    provenance: V,
});

entity_row!(AgentRow {
    profile: ProfileRow,
    zone_id: Arc<str>,
    activity: Activity,
    target_id: Option<Arc<str>>,
    needs: NeedRow,
    stress: f64,
    trust: f64,
    next_decision_at: i64,
    support_request_id: Option<Arc<str>>,
    support_cooldown_until: i64,
});

entity_row!(FacilityRow {
    id: Arc<str>,
    kind: Arc<str>,
    zone_id: Arc<str>,
    capacity: i64,
    working_units: i64,
    active_agent_ids: Vec<Arc<str>>,
    queue_agent_ids: Vec<Arc<str>>,
    resource_active_agent_ids[v6]: Vec<Arc<str>>,
    resource_queue_agent_ids[v6]: Vec<Arc<str>>,
});

entity_row!(StaffRow {
    id: Arc<str>,
    zone_id: Arc<str>,
    skills: Skills,
    work_queue: Vec<Arc<str>>,
    busy_until: i64,
    active_task_id: Option<Arc<str>>,
});

entity_row!(RequestRow {
    id: Arc<str>,
    agent_id: Arc<str>,
    requested_minute: i64,
    timeout_minute: i64,
    status: RequestStatus,
    assigned_staff_id: Option<Arc<str>>,
    task_id: Option<Arc<str>>,
    resolved_minute: Option<i64>,
    cooldown_until: i64,
    last_assignment_attempt_minute[set]: Option<i64>,
    last_assignment_wait_reason_code[set]: Option<Arc<str>>,
});

entity_row!(TaskRow {
    id: Arc<str>,
    kind: Arc<str>,
    agent_id: Option<Arc<str>>,
    resource_id: Option<Arc<str>>,
    start_minute: i64,
    due_minute: i64,
    staff_id: Option<Arc<str>>,
});

// `ZoneState`. A WorldState document writes `zone.to_dict()`, the same in v5 and v6.
entity_row!(ZoneRow {
    id: Arc<str>,
    name: Arc<str>,
    capacity: Option<i64>,
    display_x: Option<f64>,
    display_y: Option<f64>,
});

// `ZoneEdgeState`.
entity_row!(EdgeRow {
    id: Arc<str>,
    from_zone_id: Arc<str>,
    to_zone_id: Arc<str>,
    travel_minutes: i64,
    closed: bool,
});

impl EntityRow for AgentRow {
    fn id(&self) -> &str {
        &self.profile.id
    }
}

macro_rules! own_id {
    ($($name:ident),+) => {
        $(impl EntityRow for $name {
            fn id(&self) -> &str {
                &self.id
            }
        })+
    };
}

own_id!(FacilityRow, StaffRow, RequestRow, TaskRow, ZoneRow, EdgeRow);

// ---- Tables ----------------------------------------------------------------------------------

/// The rows of one `IdMap`: a stable slot per row, and the keys in the dict's order. `changed`
/// says which slots may differ from the document bytes the PyO3 crate keeps for them: anything
/// that can reach a row for writing sets it, so a clear flag means the row is the one written.
///
/// The rows are the truth (Issue #152, PR 6-1). A row may have a view outside the core, the Python
/// object of the PyO3 crate, and `views` holds what that view holds now. A write through
/// `slot_mut` or `get_mut` marks the slot touched; `insert` and `remove_row` go into the journal.
/// The view's owner takes both when the writing borrow ends (`take_touched`, `take_journal`), and
/// writes each field whose row and view differ (`diff`) into the view: the dirty mask of a slot is
/// computed then, so no write can forget its bit.
#[derive(Clone)]
pub struct Table<R> {
    rows: Vec<Option<R>>,
    free: Vec<u32>,
    ids: IdMap<u32>,
    changed: Vec<bool>,
    /// What the view of each slot holds, for the slots whose row has one.
    views: Vec<Option<R>>,
    /// The slots written since `take_touched`, each once, and their flags.
    touched: Vec<u32>,
    is_touched: Vec<bool>,
    /// The keys put in or taken out by the core since `take_journal`, in order.
    journal: Vec<Change<R>>,
}

/// A key the core put into a table or took out of it, for the view's owner to follow.
#[derive(Clone)]
pub enum Change<R> {
    /// A row put under the key, in the slot. Its view is to be made from the row, and goes to the
    /// key's place: the end of the order for a new key, its old place for a key replaced.
    Inserted { key: Arc<str>, slot: u32 },
    /// The row of the key left the slot. Its view, if it has one, gets the row's last values and
    /// leaves; the key leaves the map too, unless `replaced` (an insert under the same key follows).
    Removed { key: Arc<str>, slot: u32, row: R, view: Option<R>, replaced: bool },
}

impl<R> Default for Table<R> {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            free: Vec::new(),
            ids: IdMap::default(),
            changed: Vec::new(),
            views: Vec::new(),
            touched: Vec::new(),
            is_touched: Vec::new(),
            journal: Vec::new(),
        }
    }
}

/// What a table does without knowing its row type, for the code that picks a table by its kind.
pub trait AnyTable {
    /// A free slot, with no row yet and no key.
    fn reserve_slot(&mut self) -> u32;
    /// The slot loses its row and its view and becomes free. Its key, if it has one, stays: see
    /// `remove`.
    fn release_slot(&mut self, slot: u32);
    /// Key the slot: a new key goes to the end of the order, a key already there keeps its place.
    /// The slot the key had, if any, which stays reserved.
    fn bind(&mut self, key: Arc<str>, slot: u32) -> Option<u32>;
    /// Take the key out of the order (`shift_remove`: the others keep their order) and release its
    /// slot. The slot it had, if any. For a key the view's owner took out itself: no journal.
    fn remove(&mut self, key: &str) -> Option<u32>;
    /// Replace the keys and their order.
    fn set_keys(&mut self, ids: IdMap<u32>);
    /// Put the keys in this order. A key the table doesn't have is skipped, and a key the order
    /// leaves out leaves the table.
    fn reorder_keys(&mut self, order: &[&str]);
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// The keys in their order, for checks.
    fn key_order(&self) -> Vec<Arc<str>>;
    /// Whether anything was touched or journaled since it was last taken.
    fn has_changes(&self) -> bool;
}

impl<R> AnyTable for Table<R> {
    fn reserve_slot(&mut self) -> u32 {
        let slot = self.free.pop().unwrap_or_else(|| {
            self.rows.push(None);
            self.changed.push(true);
            self.views.push(None);
            self.is_touched.push(false);
            u32::try_from(self.rows.len() - 1).expect("under 2**32 entities")
        });
        self.changed[slot as usize] = true;
        slot
    }

    fn release_slot(&mut self, slot: u32) {
        if let Some(row) = self.rows.get_mut(slot as usize) {
            if row.take().is_some() {
                self.changed[slot as usize] = true;
                self.views[slot as usize] = None;
                self.free.push(slot);
            }
        }
    }

    fn bind(&mut self, key: Arc<str>, slot: u32) -> Option<u32> {
        self.ids.insert(key, slot)
    }

    fn remove(&mut self, key: &str) -> Option<u32> {
        // shift_remove: the order of the keys is the order of the dict.
        let slot = self.ids.shift_remove(key);
        if let Some(slot) = slot {
            self.release_slot(slot);
        }
        slot
    }

    fn set_keys(&mut self, ids: IdMap<u32>) {
        self.ids = ids;
    }

    fn reorder_keys(&mut self, order: &[&str]) {
        let mut ids = IdMap::with_capacity_and_hasher(order.len(), Default::default());
        for key in order {
            if let Some((id, slot)) = self.ids.get_key_value(*key) {
                ids.insert(id.clone(), *slot);
            }
        }
        self.ids = ids;
    }

    fn len(&self) -> usize {
        self.ids.len()
    }

    fn key_order(&self) -> Vec<Arc<str>> {
        self.ids.keys().cloned().collect()
    }

    fn has_changes(&self) -> bool {
        !self.touched.is_empty() || !self.journal.is_empty()
    }
}

impl<R> Table<R> {
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// The row in a slot, for writing. The slot is touched: its view follows at the next flush.
    pub fn slot_mut(&mut self, slot: u32) -> Option<&mut R> {
        let row = self.rows.get_mut(slot as usize)?.as_mut()?;
        self.changed[slot as usize] = true;
        if !self.is_touched[slot as usize] {
            self.is_touched[slot as usize] = true;
            self.touched.push(slot);
        }
        Some(row)
    }

    /// The row in a slot.
    pub fn row_at(&self, slot: u32) -> Option<&R> {
        self.rows.get(slot as usize)?.as_ref()
    }

    /// How many slots there are, free or not.
    pub fn slot_count(&self) -> usize {
        self.rows.len()
    }

    /// Whether the row of the slot may differ from the bytes last written of it.
    pub fn changed(&self, slot: u32) -> bool {
        self.changed[slot as usize]
    }

    /// The bytes of the slot's row are written and kept.
    pub fn mark_written(&mut self, slot: u32) {
        self.changed[slot as usize] = false;
    }

    /// The row of a key.
    pub fn get(&self, id: &str) -> Option<&R> {
        self.rows.get(*self.ids.get(id)? as usize)?.as_ref()
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut R> {
        let slot = *self.ids.get(id)?;
        self.slot_mut(slot)
    }

    /// The slot of a key.
    pub fn slot_of(&self, id: &str) -> Option<u32> {
        self.ids.get(id).copied()
    }

    /// Whether the table has the key.
    pub fn contains_key(&self, id: &str) -> bool {
        self.ids.contains_key(id)
    }

    /// The keys and rows in the order of the dict.
    pub fn iter(&self) -> impl Iterator<Item = (&Arc<str>, &R)> {
        self.ids.iter().filter_map(|(id, slot)| Some((id, self.rows[*slot as usize].as_ref()?)))
    }

    /// The keys and slots in the order of the dict.
    pub fn slots(&self) -> impl Iterator<Item = (&Arc<str>, u32)> {
        self.ids.iter().map(|(id, slot)| (id, *slot))
    }

    /// A write that reached the view already (the view's owner wrote it there): the row and the
    /// view, if the slot has one. The slot is not touched.
    pub fn row_and_view_mut(&mut self, slot: u32) -> Option<(&mut R, Option<&mut R>)> {
        let row = self.rows.get_mut(slot as usize)?.as_mut()?;
        self.changed[slot as usize] = true;
        Some((row, self.views[slot as usize].as_mut()))
    }

    /// The row and the view of a slot, if it has both: what a flush compares.
    pub fn row_and_view(&mut self, slot: u32) -> Option<(&R, &mut R)> {
        let row = self.rows.get(slot as usize)?.as_ref()?;
        let view = self.views.get_mut(slot as usize)?.as_mut()?;
        Some((row, view))
    }

    /// The view of a slot, if it has one.
    pub fn view_at(&self, slot: u32) -> Option<&R> {
        self.views.get(slot as usize)?.as_ref()
    }

    /// The slots touched since the last call, in the order they were first touched.
    pub fn take_touched(&mut self) -> Vec<u32> {
        let touched = std::mem::take(&mut self.touched);
        for slot in &touched {
            self.is_touched[*slot as usize] = false;
        }
        touched
    }

    /// The keys put in and taken out since the last call, in order.
    pub fn take_journal(&mut self) -> Vec<Change<R>> {
        std::mem::take(&mut self.journal)
    }

    /// Every row may differ from the bytes last written of it: after a snapshot came back, when
    /// the bytes the view's owner keeps may be of rows written since (`World::restore`).
    pub fn mark_all_changed(&mut self) {
        self.changed.fill(true);
    }
}

impl<R: Clone> Table<R> {
    /// Put the row in a slot `reserve_slot` gave, for a view that holds the same values already
    /// (the view's owner read the row from it).
    pub fn fill_slot(&mut self, slot: u32, row: R) {
        self.views[slot as usize] = Some(row.clone());
        self.rows[slot as usize] = Some(row);
    }

    /// The slot's view holds the row now: the view's owner made it from the row.
    pub fn set_view(&mut self, slot: u32) {
        let view = self.rows[slot as usize].clone();
        self.views[slot as usize] = view;
    }

    /// Put a row under a key: a new key goes to the end of the order, a key already there keeps
    /// its place and loses its row. Its view is made at the next flush (`Change::Inserted`).
    pub fn insert(&mut self, key: Arc<str>, row: R) -> u32 {
        if let Some(old) = self.ids.get(&*key).copied() {
            self.take_out(key.clone(), old, true);
        }
        let slot = self.reserve_slot();
        self.rows[slot as usize] = Some(row);
        self.bind(key.clone(), slot);
        self.journal.push(Change::Inserted { key, slot });
        slot
    }

    /// Take the key and its row out (`shift_remove`: the others keep their order). Its view gets
    /// the row's last values at the next flush and leaves (`Change::Removed`). False if the table
    /// has no such key.
    pub fn remove_row(&mut self, key: &str) -> bool {
        let Some((key, slot)) = self.ids.shift_remove_entry(key) else {
            return false;
        };
        self.take_out(key, slot, false);
        true
    }

    fn take_out(&mut self, key: Arc<str>, slot: u32, replaced: bool) {
        let index = slot as usize;
        if let Some(row) = self.rows[index].take() {
            let view = self.views[index].take();
            self.changed[index] = true;
            self.free.push(slot);
            self.journal.push(Change::Removed { key, slot, row, view, replaced });
        }
    }
}

impl<R: EntityRow> Table<R> {
    /// The slots of the rows in the order `WorldState._canonical_data` gives: by the entity's own
    /// ID, stably from the dict's order.
    pub fn document_slots(&self) -> Vec<u32> {
        let mut order: Vec<(&str, u32)> = self
            .ids
            .values()
            .filter_map(|slot| Some((self.rows[*slot as usize].as_ref()?.id(), *slot)))
            .collect();
        order.sort_by(|first, second| first.0.as_bytes().cmp(second.0.as_bytes()));
        order.into_iter().map(|(_, slot)| slot).collect()
    }

    /// The order `WorldState.from_dict(world.to_dict())` gives the dict (lane 6-2): the keys by
    /// the entity's own ID, stably from the dict's order (`document_slots`), and the keys of the
    /// maps inside each row sorted. The rows stay in their slots and nothing is touched: the views
    /// hold the same values, and a dict compares equal in any order (`EntityField::same`).
    pub fn canonicalize(&mut self) {
        let sorted = self.ids.values().zip(self.ids.values().skip(1)).all(|(first, second)| {
            match (self.rows[*first as usize].as_ref(), self.rows[*second as usize].as_ref()) {
                (Some(first), Some(second)) => first.id().as_bytes() <= second.id().as_bytes(),
                _ => false,
            }
        });
        if !sorted {
            let slots = self.document_slots();
            let mut ids = IdMap::with_capacity_and_hasher(self.ids.len(), Default::default());
            let mut keys: Vec<Option<Arc<str>>> = vec![None; self.rows.len()];
            for (key, slot) in &self.ids {
                keys[*slot as usize] = Some(key.clone());
            }
            for slot in slots {
                if let Some(key) = keys[slot as usize].take() {
                    ids.insert(key, slot);
                }
            }
            // A key without a row (none in a store that follows its objects) keeps its place last.
            for (key, slot) in &self.ids {
                if !ids.contains_key(key) {
                    ids.insert(key.clone(), *slot);
                }
            }
            self.ids = ids;
        }
        for row in self.rows.iter_mut().flatten() {
            row.canonicalize();
        }
    }
}

/// All the rows of a world. `Clone` is for a snapshot's copy on write (lane 6-2).
#[derive(Clone, Default)]
pub struct Entities {
    pub agents: Table<AgentRow>,
    pub facilities: Table<FacilityRow>,
    pub staff: Table<StaffRow>,
    pub support_requests: Table<RequestRow>,
    pub active_tasks: Table<TaskRow>,
    pub zones: Table<ZoneRow>,
    pub zone_edges: Table<EdgeRow>,
    /// Why the rows of the people's five tables were dropped: a value that does not fit. The Python
    /// objects are the only truth then, and kernels say Unsupported until the PyO3 crate's `refit`
    /// reads them again.
    taint: Option<String>,
    /// The same for the zones and the zone edges.
    zone_taint: Option<String>,
}

impl Entities {
    /// Why the rows of the people's tables were dropped, or None.
    pub fn tainted(&self) -> Option<&str> {
        self.taint.as_deref()
    }

    /// Why the rows of the zones and zone edges were dropped, or None.
    pub fn zones_tainted(&self) -> Option<&str> {
        self.zone_taint.as_deref()
    }

    /// The taint that covers a kind's table.
    pub fn tainted_for(&self, kind: u8) -> Option<&str> {
        if is_zone_kind(kind) {
            self.zones_tainted()
        } else {
            self.tainted()
        }
    }

    /// Drop every row of the people's tables. The first reason stays the reason. The zones stay.
    pub fn taint(&mut self, why: String) {
        let taint = self.taint.take().unwrap_or(why);
        self.agents = Table::default();
        self.facilities = Table::default();
        self.staff = Table::default();
        self.support_requests = Table::default();
        self.active_tasks = Table::default();
        self.taint = Some(taint);
    }

    /// Drop every row of the zones and zone edges. The first reason stays the reason.
    pub fn taint_zones(&mut self, why: String) {
        let taint = self.zone_taint.take().unwrap_or(why);
        self.zones = Table::default();
        self.zone_edges = Table::default();
        self.zone_taint = Some(taint);
    }

    /// `taint` or `taint_zones`, for the kind's table.
    pub fn taint_for(&mut self, kind: u8, why: String) {
        if is_zone_kind(kind) {
            self.taint_zones(why);
        } else {
            self.taint(why);
        }
    }

    /// Whether any table has writes or keys its views have not followed yet.
    pub fn has_changes(&self) -> bool {
        self.agents.has_changes()
            || self.facilities.has_changes()
            || self.staff.has_changes()
            || self.support_requests.has_changes()
            || self.active_tasks.has_changes()
            || self.zones.has_changes()
            || self.zone_edges.has_changes()
    }

    /// The order a copy through canonical JSON gives (`Table::canonicalize`), for each table that
    /// has rows; a tainted table has none.
    pub fn canonicalize(&mut self) {
        self.agents.canonicalize();
        self.facilities.canonicalize();
        self.staff.canonicalize();
        self.support_requests.canonicalize();
        self.active_tasks.canonicalize();
        self.zones.canonicalize();
        self.zone_edges.canonicalize();
    }

    /// `Table::mark_all_changed` of every table.
    pub fn mark_all_changed(&mut self) {
        self.agents.mark_all_changed();
        self.facilities.mark_all_changed();
        self.staff.mark_all_changed();
        self.support_requests.mark_all_changed();
        self.active_tasks.mark_all_changed();
        self.zones.mark_all_changed();
        self.zone_edges.mark_all_changed();
    }

    /// No rows and no taint, as before the first attach.
    pub fn clear(&mut self) {
        *self = Entities::default();
    }

    /// The table of a kind. `NEEDS` and anything unknown are the active tasks', as they always were
    /// for the PyO3 crate's `match`; it never asks for those.
    pub fn table(&self, kind: u8) -> &dyn AnyTable {
        match kind {
            AGENT => &self.agents,
            FACILITY => &self.facilities,
            STAFF => &self.staff,
            REQUEST => &self.support_requests,
            ZONE => &self.zones,
            EDGE => &self.zone_edges,
            _ => &self.active_tasks,
        }
    }

    pub fn table_mut(&mut self, kind: u8) -> &mut dyn AnyTable {
        match kind {
            AGENT => &mut self.agents,
            FACILITY => &mut self.facilities,
            STAFF => &mut self.staff,
            REQUEST => &mut self.support_requests,
            ZONE => &mut self.zones,
            EDGE => &mut self.zone_edges,
            _ => &mut self.active_tasks,
        }
    }
}

#[allow(dead_code)]
fn assert_rows_are_shareable() {
    fn check<T: Send + Sync>() {}
    check::<Entities>();
}
