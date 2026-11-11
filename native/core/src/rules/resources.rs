//! The rules of `domain/resources.py` that run in a tick, on the rows of a `World` (moved to the core
//! in P8b-1): one inventory operation, the KPI columns, and whether a reservation ID was used. Each
//! reads the event and the row, checks what the Python rule checked, and then writes the row of
//! `resource_ledger_v1`, whose tree follows the row when the world is given back (lane P8-0c).
//!
//! What the Python rule turned down with a `ResourceLedgerError` an operation turns down with the
//! same text, in the same order of checks, before any change (`Fail::Ledger`): the ledger's handler
//! and the water and meal service save it inside their own texts. A text that shows a value of the
//! payload as Python's `repr` (`LedgerFault::Repr`) is finished by the PyO3 crate, which has
//! Python's `repr`. A state the Python rule's check of the ledger turned down, and IDs
//! `ScheduledEvent` itself turns down, are Unsupported, which the entry raises as `KernelRefusal`.
//!
//! The Python rule checked the state after an operation too. Here that follows from the checks
//! before it: the new record's IDs are checked below, its sources were checked with the account,
//! each operation keeps `remaining - reserved >= 0` and the reservation total, and the row's minute
//! order is kept by turning down an older minute (docs/rust-port/lanes/m4-resources.md).
//!
//! `world.resources` is not written here: the Python rule gave the world a new dict, and whoever
//! holds the old one keeps the old values, which a tree that follows its row in place would not
//! keep. The operation returns the new balance (`Applied`), and the PyO3 crate puts the new dict in.

use std::sync::Arc;

use crate::error::{Error, Result};
use crate::state::resource_ledger::{
    is_valid_id, resource_index, Account, AuditRecord, LedgerRow, LedgerWorld, Reservation, AUDIT_SCHEMA_VERSION, NAME,
};
use crate::value::V;
use crate::world::World;

/// A `ResourceLedgerError` of the operation.
#[derive(Clone, Debug, PartialEq)]
pub enum LedgerFault {
    /// The whole text.
    Text(String),
    /// `f"{prefix} {value!r}"`: the PyO3 crate writes the `repr`.
    Repr(&'static str, V),
}

impl LedgerFault {
    /// The text, with `repr` for Python's `repr` of a value.
    pub fn text(&self, repr: impl FnOnce(&V) -> String) -> String {
        match self {
            LedgerFault::Text(text) => text.clone(),
            LedgerFault::Repr(prefix, value) => format!("{prefix} {}", repr(value)),
        }
    }
}

/// Why an operation did nothing.
#[derive(Clone, Debug, PartialEq)]
pub enum Fail {
    /// `ResourceLedgerError`.
    Ledger(LedgerFault),
    /// Anything else: a refusal, or what reading the world raised.
    Error(Error),
}

impl From<Error> for Fail {
    fn from(error: Error) -> Self {
        Fail::Error(error)
    }
}

fn ledger<T>(text: impl Into<String>) -> std::result::Result<T, Fail> {
    Err(Fail::Ledger(LedgerFault::Text(text.into())))
}

/// `repr` of a stable ID, which needs no escape: `'id'`.
fn quoted(id: &str) -> String {
    format!("'{id}'")
}

/// What an operation did to the world's balance of its resource: the PyO3 crate's new
/// `world.resources` takes `remaining` for `RESOURCE_IDS[index]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Applied {
    pub index: usize,
    pub remaining: i64,
}

/// What an operation does to the reservations, once the balances are known.
enum ReservationChange {
    None,
    Insert(Reservation),
    Remove,
    Reduce(i64),
}

#[derive(Clone, Copy)]
struct Balances {
    delivered: i64,
    served: i64,
    losses: i64,
    remaining: i64,
    reserved: i64,
}

#[track_caller]
fn added(first: i64, second: i64) -> Result<i64> {
    first.checked_add(second).ok_or_else(|| Error::unsupported("a quantity past 64 bits"))
}

#[track_caller]
fn taken(first: i64, second: i64) -> Result<i64> {
    first.checked_sub(second).ok_or_else(|| Error::unsupported("a quantity past 64 bits"))
}

/// The world beside the ledger, as `validate_resource_ledger_state` reads it: the people, the
/// zones and `world.resources`, from their rows.
struct InWorld<'a>(&'a World);

impl LedgerWorld for InWorld<'_> {
    type Error = Error;

    fn has_zone(&self, id: &str) -> Result<bool> {
        let rows = self.0.entities.try_borrow()?;
        if rows.zones_tainted().is_some() {
            return Err(Error::unsupported("the zone rows are tainted"));
        }
        Ok(rows.zones.contains_key(id))
    }

    fn has_agent(&self, id: &str) -> Result<bool> {
        Ok(self.0.rows()?.agents.contains_key(id))
    }

    fn resource_count(&self) -> Option<usize> {
        self.0.resources().ok().map(|resources| resources.len())
    }

    fn resource_is(&self, id: &str, expected: i64) -> Result<bool> {
        let Ok(resources) = self.0.resources() else {
            return Ok(false);
        };
        Ok(resources.get(id) == Some(&expected))
    }
}

/// Whether `validate_resource_ledger_state` passes the ledger as the world holds it.
pub fn ledger_passes(world: &World, row: &LedgerRow) -> Result<bool> {
    row.passes_rules_in(&InWorld(world))
}

/// `_valid_id(value)`: Some(the ID), None when it isn't a str that is one.
fn stable_id(value: &V) -> Option<&Arc<str>> {
    match value {
        V::Str(id) if is_valid_id(id) => Some(id),
        _ => None,
    }
}

/// An ID `ScheduledEvent` takes: a stable one.
#[track_caller]
fn valid_id(id: &V) -> Result<&Arc<str>> {
    match id {
        V::Str(id) if is_valid_id(id) => Ok(id),
        V::Str(_) => Err(Error::unsupported("not a stable ID")),
        _ => Err(Error::unsupported("a value is not a str")),
    }
}

/// What `apply_resource_operation` reads of its event: the payload, `source_id` (None as
/// `V::Null`) and `event_id`, as the event holds them.
pub struct OperationEvent<'a> {
    pub payload: &'a V,
    pub source_id: &'a V,
    pub event_id: &'a V,
}


/// One operation's values, read from its event or given by a rule that hasn't made the event.
pub struct Operation<'a> {
    pub operation: &'a str,
    pub warehouse_id: &'a str,
    pub resource_id: &'a str,
    pub index: usize,
    /// A positive quantity. With `beyond`, an int past 64 bits, which only turns the operation down.
    pub quantity: i64,
    pub beyond: bool,
    pub reservation_id: Option<&'a str>,
    pub agent_id: Option<&'a str>,
    pub source_id: &'a str,
    pub event_id: &'a str,
}

/// The `operation` argument of `apply_resource_operation`: an exact str, or the `repr` of anything
/// else, which is an unknown operation.
pub enum OperationArg<'a> {
    Text(&'a str),
    Other(String),
}

/// `apply_resource_operation` of a world whose `model_state` holds the ledger's tree (the PyO3 crate
/// asks that first, with the Python rule's first error). Its checks are the Python rule's, in its
/// order, with its texts.
pub fn apply_operation(world: &mut World, event: &OperationEvent<'_>, operation: OperationArg<'_>) -> std::result::Result<Applied, Fail> {
    // `validate_resource_ledger_state` of the ledger as it is: what it turns down is Unsupported.
    let ledger_warehouse = {
        let row = world.row::<LedgerRow>(NAME)?;
        if !ledger_passes(world, &row)? {
            return Err(Error::unsupported("the state is one the Python rule's check turned down").into());
        }
        row.warehouse_id.clone()
    };
    let operation_text = match operation {
        OperationArg::Text(text @ ("delivery" | "loss" | "reserve" | "release" | "consume")) => text,
        OperationArg::Text(text) => return ledger(format!("unknown resource operation {}", text_repr(text)?)),
        OperationArg::Other(repr) => return ledger(format!("unknown resource operation {repr}")),
    };
    let (has_reservation, has_quantity) = match operation_text {
        "delivery" | "loss" => (false, true),
        "reserve" | "consume" => (true, true),
        _ => (true, false),
    };

    // set(payload) is exactly the required fields.
    let field_count = 2 + usize::from(has_quantity) + if has_reservation { 2 } else { 0 };
    let mut names = vec!["warehouse_id", "resource_id"];
    if has_quantity {
        names.push("quantity");
    }
    if has_reservation {
        names.push("reservation_id");
        names.push("agent_id");
    }
    let V::Map(payload) = event.payload else {
        return Err(Error::unsupported("a payload cannot be read").into());
    };
    let mut fields: Vec<&V> = Vec::with_capacity(names.len());
    if payload.len() == field_count {
        for name in &names {
            match payload.get(*name) {
                Some(value) => fields.push(value),
                None => break,
            }
        }
    }
    if fields.len() != field_count {
        return ledger(format!("resource {operation_text} payload fields do not match"));
    }
    let (warehouse, resource) = (fields[0], fields[1]);
    let warehouse_id = match stable_id(warehouse) {
        Some(id) if *id == ledger_warehouse => id,
        _ => return Err(Fail::Ledger(LedgerFault::Repr("unknown resource warehouse ID", warehouse.clone()))),
    };
    let found = match resource {
        V::Str(id) => resource_index(id).map(|index| (id, index)),
        _ => None,
    };
    let Some((resource_id, index)) = found else {
        return Err(Fail::Ledger(LedgerFault::Repr("unknown resource ID", resource.clone())));
    };
    if *event.source_id == V::Null {
        return ledger("resource events require a source_id");
    }
    let (quantity, beyond) = if has_quantity {
        match fields[2] {
            V::Int(quantity) if *quantity > 0 => (*quantity, false),
            // Past 64 bits: a delivery would leave the row's ints; anything else is turned down below.
            V::Big(text) if !text.starts_with('-') && operation_text == "delivery" => {
                return Err(Error::unsupported("a quantity past 64 bits").into());
            }
            V::Big(text) if !text.starts_with('-') => (i64::MAX, true),
            _ => return ledger("resource quantity must be a positive integer"),
        }
    } else {
        (0, false)
    };
    let (reservation_id, agent_id) = if has_reservation {
        let (reservation, agent) = (fields[field_count - 2], fields[field_count - 1]);
        let known = match stable_id(agent) {
            Some(id) => world.rows()?.agents.contains_key(id).then_some(id),
            None => None,
        };
        let Some(agent_id) = known else {
            return Err(Fail::Ledger(LedgerFault::Repr("unknown resource Agent ID", agent.clone())));
        };
        let Some(reservation_id) = stable_id(reservation) else {
            return ledger("resource reservation_id must be a stable ID");
        };
        (Some(reservation_id.clone()), Some(agent_id.clone()))
    } else {
        (None, None)
    };

    // `ScheduledEvent` takes only stable IDs for these.
    let source_id = valid_id(event.source_id)?.clone();
    let event_id = valid_id(event.event_id)?.clone();
    let (warehouse_id, resource_id) = (warehouse_id.clone(), resource_id.clone());
    apply_planned(
        world,
        &Operation {
            operation: operation_text,
            warehouse_id: &warehouse_id,
            resource_id: &resource_id,
            index,
            quantity,
            beyond,
            reservation_id: reservation_id.as_deref(),
            agent_id: agent_id.as_deref(),
            source_id: &source_id,
            event_id: &event_id,
        },
    )
}

/// `repr` of a str of printable ASCII without a quote or a backslash, which Python writes in single
/// quotes as it is; any other is Unsupported. The PyO3 crate hands an unknown operation in as
/// `OperationArg::Other` with Python's `repr`, so this answers only a caller without Python.
fn text_repr(text: &str) -> std::result::Result<String, Fail> {
    if text.bytes().all(|byte| (0x20..0x7f).contains(&byte) && byte != b'\\' && byte != b'\'') {
        Ok(quoted(text))
    } else {
        Err(Error::unsupported("an operation whose repr is Python's").into())
    }
}

/// The operation on the row. What the Python rule turned down after its checks of the payload is
/// its `ResourceLedgerError`, and a state its check of the ledger turned down is Unsupported, both
/// with nothing changed. Every ID is checked by the caller to be a stable ID.
pub fn apply_planned(world: &mut World, planned: &Operation<'_>) -> std::result::Result<Applied, Fail> {
    let &Operation {
        operation: operation_text,
        warehouse_id,
        resource_id,
        index,
        quantity,
        beyond,
        reservation_id,
        agent_id,
        source_id,
        event_id,
    } = planned;
    if let Some(agent) = agent_id {
        if !world.rows()?.agents.contains_key(agent) {
            return ledger(format!("unknown resource Agent ID {}", quoted(agent)));
        }
    }
    let minute = world.minute_index()?;
    if minute < 0 {
        return Err(Error::unsupported("a minute before the Run").into());
    }

    // Plan: nothing changes until the record is known.
    let (next, change, record_quantity, source_is_new) = {
        let row = world.row::<LedgerRow>(NAME)?;
        if minute < row.audit.last_minute || !ledger_passes(world, &row)? {
            return Err(Error::unsupported("the state is one the Python rule's check turned down").into());
        }
        if &*row.warehouse_id != warehouse_id {
            return ledger(format!("unknown resource warehouse ID {}", quoted(warehouse_id)));
        }
        let account = row.accounts.of(index);
        let mut next = Balances {
            delivered: account.delivered,
            served: account.served,
            losses: account.losses,
            remaining: account.remaining,
            reserved: account.reserved,
        };
        let mut change = ReservationChange::None;
        let mut record_quantity = quantity;
        match operation_text {
            "delivery" => {
                next.delivered = added(next.delivered, quantity)?;
                next.remaining = added(next.remaining, quantity)?;
            }
            "loss" => {
                if beyond || quantity > taken(next.remaining, next.reserved)? {
                    return ledger("resource loss exceeds unreserved remaining inventory");
                }
                next.losses = added(next.losses, quantity)?;
                next.remaining = taken(next.remaining, quantity)?;
            }
            "reserve" => {
                let id = reservation_id.unwrap_or_default();
                if row.used_reservation_ids.contains(id) {
                    return ledger(format!("resource reservation ID {} was already used", quoted(id)));
                }
                if beyond || quantity > taken(next.remaining, next.reserved)? {
                    return ledger("resource reservation exceeds available inventory");
                }
                next.reserved = added(next.reserved, quantity)?;
                change = ReservationChange::Insert(Reservation {
                    reservation_id: Arc::from(id),
                    resource_id: Arc::from(resource_id),
                    agent_id: Arc::from(agent_id.unwrap_or_default()),
                    quantity,
                });
            }
            _ => {
                let id = reservation_id.unwrap_or_default();
                let Some(found) = account.reservations.get(id) else {
                    return ledger(format!("unknown resource reservation {}", quoted(id)));
                };
                if Some(&*found.agent_id) != agent_id {
                    return ledger("resource reservation belongs to a different Agent");
                }
                if operation_text == "release" {
                    record_quantity = found.quantity;
                    next.reserved = taken(next.reserved, found.quantity)?;
                    change = ReservationChange::Remove;
                } else {
                    if beyond || quantity > found.quantity {
                        return ledger("resource consume exceeds the reservation");
                    }
                    next.served = added(next.served, quantity)?;
                    next.remaining = taken(next.remaining, quantity)?;
                    next.reserved = taken(next.reserved, quantity)?;
                    change = if quantity == found.quantity {
                        ReservationChange::Remove
                    } else {
                        ReservationChange::Reduce(found.quantity - quantity)
                    };
                }
            }
        }
        let source_is_new = !account.source_ids.iter().any(|id| &**id == source_id);
        (next, change, record_quantity, source_is_new)
    };

    // Write the row. This cannot be turned down.
    let row = world.slots.row_mut::<LedgerRow>(NAME)?;
    let shared_source: Arc<str> = Arc::from(source_id);
    let account: &mut Account = row.accounts.of_mut(index);
    account.delivered = next.delivered;
    account.served = next.served;
    account.losses = next.losses;
    account.remaining = next.remaining;
    account.reserved = next.reserved;
    if source_is_new {
        account.source_ids.push(shared_source.clone());
    }
    match change {
        ReservationChange::Insert(new) => {
            account.reservations.insert(new.reservation_id.clone(), new);
        }
        ReservationChange::Remove => {
            account.reservations.shift_remove(reservation_id.unwrap_or_default());
        }
        ReservationChange::Reduce(left) => {
            if let Some(found) = account.reservations.get_mut(reservation_id.unwrap_or_default()) {
                found.quantity = left;
            }
        }
        ReservationChange::None => {}
    }
    let record = AuditRecord {
        schema_version: AUDIT_SCHEMA_VERSION,
        minute,
        event_id: Some(Arc::from(event_id)),
        operation: Arc::from(operation_text),
        resource_id: Arc::from(resource_id),
        quantity: record_quantity,
        reservation_id: reservation_id.map(Arc::from),
        agent_id: agent_id.map(Arc::from),
        source_id: Some(shared_source),
        source_ids: account.source_ids.clone(),
        warehouse_id: row.warehouse_id.clone(),
        initial: account.initial,
        delivered: account.delivered,
        served: account.served,
        losses: account.losses,
        remaining: account.remaining,
        reserved: account.reserved,
    };
    let remaining = record.remaining;
    if operation_text == "reserve" {
        row.used_reservation_ids.push(Arc::from(reservation_id.unwrap_or_default()));
    }
    row.audit.push(record)?;
    Ok(Applied { index, remaining })
}

/// The KPI columns of `resource_metric_values`, in their order: initial, delivered, served, losses,
/// remaining and reserved, of water and then of meals.
pub fn metric_values(world: &World) -> Result<[[i64; 6]; 2]> {
    let row = world.row::<LedgerRow>(NAME)?;
    if !ledger_passes(world, &row)? {
        return Err(Error::unsupported("the state is for the Python rule to judge"));
    }
    let of = |account: &Account| {
        [account.initial, account.delivered, account.served, account.losses, account.remaining, account.reserved]
    };
    Ok([of(row.accounts.of(0)), of(row.accounts.of(1))])
}

/// `reservation_id in ledger["used_reservation_ids"]`, without reading the list.
pub fn reservation_id_was_used(world: &World, reservation_id: &str) -> Result<bool> {
    Ok(world.row::<LedgerRow>(NAME)?.used_reservation_ids.contains(reservation_id))
}
