//! `resource_ledger_v1`: the two accounts, the reservations and the audit trail
//! (`domain/resources.py`). `rules/resources.rs` writes it in place: a Python rule that replaced the
//! whole namespace at each operation would forward all of it (docs/rust-port/lanes/m4-resources.md).
//!
//! The audit trail is the canonical JSON of each record. Rules read only the newest record of each
//! resource, which the row keeps typed. A record joins the row only if it passes every rule of
//! `_validate_audit_record` that reads the record alone; any other record, or a write into a record
//! already there, taints the namespace, and the Python rules and their errors run on the tree.
//!
//! The PyO3 crate's `state/resource_ledger.rs` reads it from Python and holds its validator, which
//! hands the world's resources, people and zones to `passes_rules_in` as a `LedgerWorld`.

use std::sync::Arc;

use rustc_hash::{FxBuildHasher, FxHashSet};

use crate::error::{Misfit, Result};
use crate::json::Encoder;
use crate::row::{Field, IdMap, Items};
use crate::state_row;
use crate::value::V;

pub const NAME: &str = "resource_ledger_v1";
pub const RESOURCE_IDS: [&str; 2] = ["water_ml", "food_meal_units"];
const SCHEMA_VERSION: i64 = 1;
pub const AUDIT_SCHEMA_VERSION: i64 = 1;

state_row! {
    pub struct Reservation {
        reservation_id: Arc<str>,
        resource_id: Arc<str>,
        agent_id: Arc<str>,
        quantity: i64 => set_quantity,
    }
}

state_row! {
    pub struct Account {
        initial: i64,
        delivered: i64 => set_delivered,
        served: i64 => set_served,
        losses: i64 => set_losses,
        remaining: i64 => set_remaining,
        reserved: i64 => set_reserved,
        source_ids: Vec<Arc<str>>,
        reservations: IdMap<Reservation>,
    }
}

state_row! {
    pub struct Accounts {
        water_ml: Account,
        food_meal_units: Account,
    }
}

impl Accounts {
    pub fn of(&self, index: usize) -> &Account {
        if index == 0 {
            &self.water_ml
        } else {
            &self.food_meal_units
        }
    }

    pub fn of_mut(&mut self, index: usize) -> &mut Account {
        if index == 0 {
            &mut self.water_ml
        } else {
            &mut self.food_meal_units
        }
    }
}

state_row! {
    pub struct AuditRecord {
        schema_version: i64,
        minute: i64,
        event_id: Option<Arc<str>>,
        operation: Arc<str>,
        resource_id: Arc<str>,
        quantity: i64,
        reservation_id: Option<Arc<str>>,
        agent_id: Option<Arc<str>>,
        source_id: Option<Arc<str>>,
        source_ids: Vec<Arc<str>>,
        warehouse_id: Arc<str>,
        initial: i64,
        delivered: i64,
        served: i64,
        losses: i64,
        remaining: i64,
        reserved: i64,
    }
}

pub use crate::events::is_stable_id as is_valid_id;

fn no_repeats(ids: &[Arc<str>]) -> bool {
    // A handful of sources: a set would cost more.
    ids.iter().enumerate().all(|(index, id)| !ids[..index].contains(id))
}

pub fn resource_index(id: &str) -> Option<usize> {
    RESOURCE_IDS.iter().position(|known| *known == id)
}

impl AuditRecord {
    /// The rules of `_validate_audit_record` that read the record alone.
    fn is_valid(&self) -> bool {
        let id_is_valid = |id: &Option<Arc<str>>| id.as_ref().is_none_or(|id| is_valid_id(id));
        self.schema_version == AUDIT_SCHEMA_VERSION
            && self.minute >= 0
            && resource_index(&self.resource_id).is_some()
            && matches!(&*self.operation, "initial" | "delivery" | "loss" | "reserve" | "release" | "consume")
            && self.quantity >= 0
            && id_is_valid(&self.event_id)
            && id_is_valid(&self.reservation_id)
            && id_is_valid(&self.agent_id)
            && id_is_valid(&self.source_id)
            && self.source_ids.iter().all(|id| is_valid_id(id))
            && no_repeats(&self.source_ids)
            && self.initial >= 0
            && self.delivered >= 0
            && self.served >= 0
            && self.losses >= 0
            && self.remaining >= 0
            && self.reserved >= 0
    }
}

/// The audit trail: the bytes of each record, and what the checks of a tick read.
#[derive(Clone)]
pub struct Audit {
    items: Vec<Box<[u8]>>,
    newest: [Option<AuditRecord>; 2],
    pub last_minute: i64,
    /// Every minute is at least the one before. The Python check of a tick reads only the newest
    /// records, so a false here says nothing, and the Python rule then decides.
    ordered: bool,
    /// The warehouse of every record, or None without records. A second warehouse is a misfit.
    warehouse: Option<Arc<str>>,
}

impl Audit {
    /// An empty trail, with room for this many records.
    pub fn with_capacity(capacity: usize) -> Self {
        Self { items: Vec::with_capacity(capacity), newest: [None, None], last_minute: 0, ordered: true, warehouse: None }
    }

    /// The canonical JSON of each record.
    pub fn item_bytes(&self) -> &[Box<[u8]>] {
        &self.items
    }

    /// Add a record a rule built, or one read from the tree.
    pub fn push(&mut self, record: AuditRecord) -> Result<()> {
        let mut encoder = Encoder { out: Vec::with_capacity(384) };
        Field::write_json(&record, &mut encoder)?;
        self.items.push(encoder.out.into_boxed_slice());
        if record.minute < self.last_minute {
            self.ordered = false;
        }
        self.last_minute = record.minute;
        if self.warehouse.is_none() {
            self.warehouse = Some(record.warehouse_id.clone());
        }
        if let Some(index) = resource_index(&record.resource_id) {
            self.newest[index] = Some(record);
        }
        Ok(())
    }

    /// Add a record read from the tree, if the row can hold it.
    pub fn take(&mut self, record: AuditRecord) -> std::result::Result<(), Misfit> {
        if !record.is_valid() {
            return Err(Misfit::new("an audit record the rules turn down"));
        }
        if self.warehouse.as_ref().is_some_and(|warehouse| *warehouse != record.warehouse_id) {
            return Err(Misfit::new("audit records of two warehouses"));
        }
        // A record has no float, so writing it never fails.
        self.push(record).map_err(|error| Misfit::new(error.to_string()))
    }
}

impl Items for Audit {
    fn count(&self) -> usize {
        self.items.len()
    }

    fn item_json<'a>(&'a self, index: usize, _scratch: &'a mut Encoder) -> Result<&'a [u8]> {
        Ok(&self.items[index])
    }
}

impl Field for Audit {
    fn items(&self) -> Option<&dyn Items> {
        Some(self)
    }

    /// Each record is its canonical JSON already, and has no map inside.
    fn canonicalize(&mut self) {}

    /// Each record as `take` takes it.
    fn from_value(value: &V) -> std::result::Result<Self, Misfit> {
        let V::List(items) = value else {
            return Err(Misfit::new("not a list"));
        };
        let mut audit = Self::with_capacity(items.len());
        for item in items {
            audit.take(AuditRecord::from_value(item)?)?;
        }
        Ok(audit)
    }

    fn write_json(&self, encoder: &mut Encoder) -> Result<()> {
        encoder.out.push(b'[');
        for (index, item) in self.items.iter().enumerate() {
            if index > 0 {
                encoder.out.push(b',');
            }
            encoder.out.extend_from_slice(item);
        }
        encoder.out.push(b']');
        Ok(())
    }
}

/// `used_reservation_ids`, with a set: a rule asks whether an ID was used, four times per service.
#[derive(Clone)]
pub struct UsedIds {
    ids: Vec<Arc<str>>,
    set: FxHashSet<Arc<str>>,
}

impl UsedIds {
    pub fn new(ids: Vec<Arc<str>>) -> Self {
        let mut set = FxHashSet::with_capacity_and_hasher(ids.len(), FxBuildHasher);
        set.extend(ids.iter().cloned());
        Self { ids, set }
    }

    /// The IDs in their order.
    pub fn ids(&self) -> &Vec<Arc<str>> {
        &self.ids
    }

    /// Change the list with f; if it succeeds, the set follows.
    pub fn edit<T, E>(
        &mut self,
        f: impl FnOnce(&mut Vec<Arc<str>>) -> std::result::Result<T, E>,
    ) -> std::result::Result<T, E> {
        let result = f(&mut self.ids)?;
        self.set.clear();
        self.set.extend(self.ids.iter().cloned());
        Ok(result)
    }

    pub fn contains(&self, id: &str) -> bool {
        self.set.contains(id)
    }

    pub fn push(&mut self, id: Arc<str>) {
        self.set.insert(id.clone());
        self.ids.push(id);
    }

    fn are_unique(&self) -> bool {
        self.set.len() == self.ids.len()
    }
}

impl Field for UsedIds {
    fn write_json(&self, encoder: &mut Encoder) -> Result<()> {
        Field::write_json(&self.ids, encoder)
    }

    fn from_value(value: &V) -> std::result::Result<Self, Misfit> {
        Ok(Self::new(Field::from_value(value)?))
    }

    /// A list keeps its order.
    fn canonicalize(&mut self) {}
}

state_row! {
    pub struct LedgerRow {
        schema_version: i64,
        warehouse_id: Arc<str>,
        zone_id: Option<Arc<str>>,
        accounts: Accounts,
        used_reservation_ids: UsedIds,
        audit: Audit,
    }
}

fn balances_match(record: &AuditRecord, account: &Account) -> bool {
    record.initial == account.initial
        && record.delivered == account.delivered
        && record.served == account.served
        && record.losses == account.losses
        && record.remaining == account.remaining
        && record.reserved == account.reserved
        && record.source_ids == account.source_ids
}

/// What `passes_rules_in` reads of the world beside the ledger. A reader that cannot answer fails, and
/// `passes_rules_in` fails with its error.
pub trait LedgerWorld {
    type Error;
    fn has_zone(&self, id: &str) -> std::result::Result<bool, Self::Error>;
    fn has_agent(&self, id: &str) -> std::result::Result<bool, Self::Error>;
    /// How many resources the world holds, or None if they are not a plain mapping.
    fn resource_count(&self) -> Option<usize>;
    /// Whether the world holds exactly this int for the resource.
    fn resource_is(&self, id: &str, expected: i64) -> std::result::Result<bool, Self::Error>;
}

impl LedgerRow {
    /// Every rule `validate_resource_ledger_state` has, given a row of the right types, for a world
    /// holding these resources, people and zones. True says the Python rule accepts; false decides
    /// nothing.
    pub fn passes_rules_in<W: LedgerWorld>(&self, world: &W) -> std::result::Result<bool, W::Error> {
        if self.schema_version != SCHEMA_VERSION
            || !is_valid_id(&self.warehouse_id)
            || !self.used_reservation_ids.are_unique()
            || self.audit.items.len() < RESOURCE_IDS.len()
            || !self.audit.ordered
            || self.audit.warehouse.as_ref() != Some(&self.warehouse_id)
        {
            return Ok(false);
        }
        if let Some(zone) = &self.zone_id {
            if !is_valid_id(zone) || !world.has_zone(zone)? {
                return Ok(false);
            }
        }
        if world.resource_count() != Some(RESOURCE_IDS.len()) {
            return Ok(false);
        }
        for (index, resource_id) in RESOURCE_IDS.iter().enumerate() {
            let account = self.accounts.of(index);
            if account.initial < 0
                || account.delivered < 0
                || account.served < 0
                || account.losses < 0
                || account.remaining < 0
                || account.reserved < 0
            {
                return Ok(false);
            }
            let expected = i128::from(account.initial) + i128::from(account.delivered)
                - i128::from(account.served)
                - i128::from(account.losses);
            if expected != i128::from(account.remaining)
                || account.reserved > account.remaining
                || !world.resource_is(resource_id, account.remaining)?
            {
                return Ok(false);
            }
            let mut total = 0_i128;
            for (id, record) in &account.reservations {
                if !is_valid_id(id)
                    || !self.used_reservation_ids.contains(id)
                    || record.reservation_id != *id
                    || &*record.resource_id != *resource_id
                    || !is_valid_id(&record.agent_id)
                    || !world.has_agent(&record.agent_id)?
                    || record.quantity <= 0
                {
                    return Ok(false);
                }
                total += i128::from(record.quantity);
            }
            if total != i128::from(account.reserved)
                || !account.source_ids.iter().all(|id| is_valid_id(id))
                || !no_repeats(&account.source_ids)
                || !self.audit.newest[index].as_ref().is_some_and(|newest| balances_match(newest, account))
            {
                return Ok(false);
            }
        }
        Ok(true)
    }
}
