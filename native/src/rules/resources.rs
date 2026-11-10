//! The rules of `domain/resources.py` that run in a tick: one inventory operation, the KPI columns,
//! and whether a reservation ID was used. Each reads the event and the row, checks what the Python
//! rule checked, and then writes the row and the tree in place. It calls no Python.
//!
//! What the Python rule turned down with a `ResourceLedgerError` an operation raises with the same
//! text, in the same order of checks, before any change: the ledger's handler and the water and
//! meal service save it inside their own texts. A state the Python rule's check of the ledger turned
//! down (a write past the table, a row that isn't the tree's), and IDs `ScheduledEvent` itself turns
//! down, are Unsupported, which the entry raises as `KernelRefusal`.
//!
//! The Python rule checked the state after an operation too. Here that follows from the checks
//! before it: the new record's IDs are checked below, its sources were checked with the account,
//! each operation keeps `remaining - reserved >= 0` and the reservation total, and the row's minute
//! order is kept by turning down an older minute (docs/rust-port/lanes/m4-resources.md).

use std::sync::Arc;

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyString};

use crate::core::errors::{self, ledger_error, Wide};
use crate::core::row::{attribute, exact_int, exact_str, namespace_is_absent, unsupported, PyField};
use crate::core::tracked::as_readable_dict;
use crate::core::world_core::{raw_dict_get, NamespaceMirror};
use crate::rules::social_network::mapping_get;
use crate::state::entities::raw_setattr;
use crate::state::resource_ledger::{
    held_mirror, is_valid_id, resource_index, world_inputs, Account, AuditRecord, LedgerRow, Reservation,
    AUDIT_SCHEMA_VERSION, RESOURCE_IDS,
};

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

fn added(first: i64, second: i64) -> PyResult<i64> {
    first.checked_add(second).map_or_else(|| unsupported("a quantity past 64 bits"), Ok)
}

fn taken(first: i64, second: i64) -> PyResult<i64> {
    first.checked_sub(second).map_or_else(|| unsupported("a quantity past 64 bits"), Ok)
}

fn valid_id<'a>(value: &'a Bound<'_, PyAny>) -> PyResult<&'a str> {
    let id = exact_str(value)?;
    if is_valid_id(id) {
        Ok(id)
    } else {
        unsupported("not a stable ID")
    }
}

/// `_valid_id(value)`: Some(the ID), None when it isn't a str that is one. A str subclass is refused.
fn stable_id<'a>(value: &'a Bound<'_, PyAny>) -> PyResult<Option<&'a str>> {
    Ok(errors::str_of(value)?.filter(|id| is_valid_id(id)))
}

/// One operation's values, read from its event or given by a rule that hasn't made the event.
pub(crate) struct Operation<'a> {
    pub(crate) operation: &'a str,
    pub(crate) warehouse_id: &'a str,
    pub(crate) resource_id: &'a str,
    pub(crate) index: usize,
    /// A positive quantity. With `beyond`, an int past 64 bits, which only turns the operation down.
    pub(crate) quantity: i64,
    pub(crate) beyond: bool,
    pub(crate) reservation_id: Option<&'a str>,
    pub(crate) agent_id: Option<&'a str>,
    pub(crate) source_id: &'a str,
    pub(crate) event_id: &'a str,
}

/// `apply_resource_operation`. Its checks are the Python rule's, in its order, with its texts.
#[pyfunction]
pub(crate) fn apply_operation<'py>(
    world: &Bound<'py, PyAny>,
    event: &Bound<'py, PyAny>,
    operation: &Bound<'py, PyAny>,
) -> PyResult<()> {
    let py = world.py();
    let table = attribute(world, intern!(py, "model_state"))?;
    let Some(table) = as_readable_dict(&table) else {
        return unsupported("model_state is another kind of mapping");
    };
    if table.get_item(intern!(py, "resource_ledger_v1"))?.is_none() {
        return Err(ledger_error("resource ledger is not enabled for this scenario"));
    }
    // `validate_resource_ledger_state` of the ledger as it is: what it turns down is Unsupported.
    let mirror = held_mirror(world)?;
    let (resources, agents, zones) = world_inputs(world)?;
    let ledger_warehouse = NamespaceMirror::with_row::<LedgerRow, _>(&mirror, |row| {
        if !row.passes_rules(&resources, &agents, &zones)? {
            return unsupported("the state is one the Python rule's check turned down");
        }
        Ok(row.warehouse_id.clone())
    })?;

    let known = errors::str_of(operation)?.filter(|text| matches!(*text, "delivery" | "loss" | "reserve" | "release" | "consume"));
    let Some(operation_text) = known else {
        return Err(ledger_error(format!("unknown resource operation {}", errors::repr(operation)?)));
    };
    let (has_reservation, has_quantity) = match operation_text {
        "delivery" | "loss" => (false, true),
        "reserve" | "consume" => (true, true),
        _ => (true, false),
    };

    // set(payload) is exactly the required fields.
    let payload = attribute(event, intern!(py, "payload"))?;
    let field_count = 2 + usize::from(has_quantity) + if has_reservation { 2 } else { 0 };
    let mut names = vec![intern!(py, "warehouse_id"), intern!(py, "resource_id")];
    if has_quantity {
        names.push(intern!(py, "quantity"));
    }
    if has_reservation {
        names.push(intern!(py, "reservation_id"));
        names.push(intern!(py, "agent_id"));
    }
    let mut fields = Vec::with_capacity(names.len());
    if payload.len()? == field_count {
        for name in &names {
            match mapping_get(&payload, name)? {
                Some(value) => fields.push(value),
                None => break,
            }
        }
    }
    if fields.len() != field_count {
        return Err(ledger_error(format!("resource {operation_text} payload fields do not match")));
    }
    let (warehouse, resource) = (&fields[0], &fields[1]);
    let warehouse_id = match stable_id(warehouse)? {
        Some(id) if *id == *ledger_warehouse => id,
        _ => return Err(ledger_error(format!("unknown resource warehouse ID {}", errors::repr(warehouse)?))),
    };
    let found = errors::str_of(resource)?.and_then(|id| resource_index(id).map(|index| (id, index)));
    let Some((resource_id, index)) = found else {
        return Err(ledger_error(format!("unknown resource ID {}", errors::repr(resource)?)));
    };
    let source_object = attribute(event, intern!(py, "source_id"))?;
    if source_object.is_none() {
        return Err(ledger_error("resource events require a source_id"));
    }
    let (quantity, beyond) = if has_quantity {
        match errors::wide_int_not_bool(&fields[2])? {
            Some(Wide::Int(quantity)) if quantity > 0 => (quantity, false),
            // Past 64 bits: a delivery would leave the row's ints; anything else is turned down below.
            Some(Wide::Above) if operation_text == "delivery" => return unsupported("a quantity past 64 bits"),
            Some(Wide::Above) => (i64::MAX, true),
            _ => return Err(ledger_error("resource quantity must be a positive integer")),
        }
    } else {
        (0, false)
    };
    let (reservation_id, agent_id) = if has_reservation {
        let (reservation, agent) = (&fields[field_count - 2], &fields[field_count - 1]);
        let known = match stable_id(agent)? {
            Some(id) => raw_dict_get(&agents, agent)?.is_some().then_some(id),
            None => None,
        };
        let Some(agent_id) = known else {
            return Err(ledger_error(format!("unknown resource Agent ID {}", errors::repr(agent)?)));
        };
        let Some(reservation_id) = stable_id(reservation)? else {
            return Err(ledger_error("resource reservation_id must be a stable ID"));
        };
        (Some(reservation_id), Some(agent_id))
    } else {
        (None, None)
    };

    // `ScheduledEvent` takes only stable IDs for these.
    let source_id = valid_id(&source_object)?;
    let event_object = attribute(event, intern!(py, "event_id"))?;
    let event_id = valid_id(&event_object)?;
    apply_planned(
        world,
        &mirror,
        &Operation {
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
        },
    )
}

/// The operation on the row and the tree. What the Python rule turned down after its checks of the
/// payload is its `ResourceLedgerError`, and a state its check of the ledger turned down is
/// Unsupported, both with nothing changed. Every ID is checked by the caller to be a stable ID.
pub(crate) fn apply_planned<'py>(
    world: &Bound<'py, PyAny>,
    mirror: &Bound<'py, NamespaceMirror>,
    planned: &Operation<'_>,
) -> PyResult<()> {
    let py = world.py();
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
    let (resources, agents, zones) = world_inputs(world)?;
    if let Some(agent) = agent_id {
        if raw_dict_get(&agents, PyString::new(py, agent).as_any())?.is_none() {
            return Err(ledger_error(format!("unknown resource Agent ID {}", errors::repr_str(py, agent)?)));
        }
    }
    let minute = exact_int(&attribute(world, intern!(py, "minute_index"))?)?;
    if minute < 0 {
        return unsupported("a minute before the Run");
    }

    let (record, change) = NamespaceMirror::with_row_tree::<LedgerRow, _>(mirror, |row, tree| {
        // Plan: nothing changes until the record is known.
        if minute < row.audit.last_minute || !row.passes_rules(&resources, &agents, &zones)? {
            return unsupported("the state is one the Python rule's check turned down");
        }
        if &*row.warehouse_id != warehouse_id {
            return Err(ledger_error(format!("unknown resource warehouse ID {}", errors::repr_str(py, warehouse_id)?)));
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
                    return Err(ledger_error("resource loss exceeds unreserved remaining inventory"));
                }
                next.losses = added(next.losses, quantity)?;
                next.remaining = taken(next.remaining, quantity)?;
            }
            "reserve" => {
                let id = reservation_id.unwrap_or_default();
                if row.used_reservation_ids.contains(id) {
                    return Err(ledger_error(format!("resource reservation ID {} was already used", errors::repr_str(py, id)?)));
                }
                if beyond || quantity > taken(next.remaining, next.reserved)? {
                    return Err(ledger_error("resource reservation exceeds available inventory"));
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
                    return Err(ledger_error(format!("unknown resource reservation {}", errors::repr_str(py, id)?)));
                };
                if Some(&*found.agent_id) != agent_id {
                    return Err(ledger_error("resource reservation belongs to a different Agent"));
                }
                if operation_text == "release" {
                    record_quantity = found.quantity;
                    next.reserved = taken(next.reserved, found.quantity)?;
                    change = ReservationChange::Remove;
                } else {
                    if beyond || quantity > found.quantity {
                        return Err(ledger_error("resource consume exceeds the reservation"));
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

        // Write the row and the tree. These cannot be turned down.
        let account_tree = tree.child(intern!(py, "accounts"))?.child_str(RESOURCE_IDS[index])?;
        let shared_source: Arc<str> = Arc::from(source_id);
        let reservations_tree = account_tree.child(intern!(py, "reservations"))?;
        let account: &mut Account = row.accounts.of_mut(index);
        account.set_delivered(&account_tree, next.delivered)?;
        account.set_served(&account_tree, next.served)?;
        account.set_losses(&account_tree, next.losses)?;
        account.set_remaining(&account_tree, next.remaining)?;
        account.set_reserved(&account_tree, next.reserved)?;
        if source_is_new {
            account_tree.child(intern!(py, "source_ids"))?.list_append(&shared_source)?;
            account.source_ids.push(shared_source.clone());
        }
        match &change {
            ReservationChange::Insert(new) => {
                account.reservations.insert(new.reservation_id.clone(), new.clone());
            }
            ReservationChange::Remove => {
                account.reservations.shift_remove(reservation_id.unwrap_or_default());
            }
            ReservationChange::Reduce(left) => {
                let id = reservation_id.unwrap_or_default();
                if let Some(found) = account.reservations.get_mut(id) {
                    found.set_quantity(&reservations_tree.child_str(id)?, *left)?;
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
        if operation_text == "reserve" {
            let id: Arc<str> = Arc::from(reservation_id.unwrap_or_default());
            tree.child(intern!(py, "used_reservation_ids"))?.list_append(&id)?;
            row.used_reservation_ids.push(id);
        }
        row.audit.push(record.clone())?;
        Ok((record, change))
    })?;

    // What adds or leaves nodes of the tree runs outside the row's borrow.
    let path = ["accounts", RESOURCE_IDS[index], "reservations"];
    match &change {
        ReservationChange::Insert(new) => {
            NamespaceMirror::write_through(mirror, &path, &new.reservation_id, &PyField::to_py(new, py)?)?;
        }
        ReservationChange::Remove => {
            NamespaceMirror::remove_through(mirror, &path, reservation_id.unwrap_or_default())?;
        }
        ReservationChange::Reduce(_) | ReservationChange::None => {}
    }
    NamespaceMirror::append_through(mirror, &["audit"], &PyField::to_py(&record, py)?)?;

    // A new dict, like the Python rule: whoever holds the old one keeps the old values.
    let next_resources = as_readable_dict(&resources).map_or_else(
        || unsupported("the resources are not a dict"),
        |resources| Ok(resources.copy()?),
    )?;
    next_resources.set_item(RESOURCE_IDS[index], record.remaining)?;
    raw_setattr(world, intern!(py, "resources"), next_resources.as_ptr())
}

const COLUMNS: [[&str; 6]; 2] = [
    [
        "water_ml_initial",
        "water_ml_delivered",
        "water_ml_served",
        "water_ml_losses",
        "water_ml_remaining",
        "water_ml_reserved",
    ],
    [
        "food_meal_units_initial",
        "food_meal_units_delivered",
        "food_meal_units_served",
        "food_meal_units_losses",
        "food_meal_units_remaining",
        "food_meal_units_reserved",
    ],
];

/// `resource_metric_values`.
#[pyfunction]
pub(crate) fn metric_values<'py>(world: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyDict>>> {
    let py = world.py();
    if namespace_is_absent(world, intern!(py, "resource_ledger_v1"))? {
        return Ok(None);
    }
    let mirror = held_mirror(world)?;
    let (resources, agents, zones) = world_inputs(world)?;
    let balances = NamespaceMirror::with_row::<LedgerRow, _>(&mirror, |row| {
        if !row.passes_rules(&resources, &agents, &zones)? {
            return unsupported("the state is for the Python rule to judge");
        }
        let of = |account: &Account| {
            [account.initial, account.delivered, account.served, account.losses, account.remaining, account.reserved]
        };
        Ok([of(row.accounts.of(0)), of(row.accounts.of(1))])
    })?;
    let result = PyDict::new(py);
    for (names, values) in COLUMNS.iter().zip(&balances) {
        for (name, value) in names.iter().zip(values) {
            result.set_item(*name, *value)?;
        }
    }
    Ok(Some(result))
}

/// `reservation_id in ledger["used_reservation_ids"]`, without reading the list. The list holds only
/// str, so anything else was never used.
#[pyfunction]
fn reservation_id_was_used(world: &Bound<'_, PyAny>, reservation_id: &Bound<'_, PyAny>) -> PyResult<bool> {
    let mirror = held_mirror(world)?;
    let Some(id) = errors::str_of(reservation_id)? else {
        return Ok(false);
    };
    NamespaceMirror::with_row::<LedgerRow, _>(&mirror, |row| Ok(row.used_reservation_ids.contains(id)))
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(apply_operation, m)?)?;
    m.add_function(wrap_pyfunction!(metric_values, m)?)?;
    m.add_function(wrap_pyfunction!(reservation_id_was_used, m)?)?;
    Ok(())
}
