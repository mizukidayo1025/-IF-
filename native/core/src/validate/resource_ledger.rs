//! The resource ledger block of `WorldState.validate` (`core/world.py`): the identity history of
//! `population_dynamics_v1`, then `validate_resource_ledger` (with the history: the whole audit
//! trail replayed) or `validate_resource_ledger_state` (without: the balances and the records of
//! the last tick) of `domain/resources.py`, with Python's errors and texts. `validate::world` calls
//! it only where `resource_ledger_v1` is in `model_state`, and makes a `ValueError` (a
//! `ResourceLedgerError` too) `invalid resource ledger: {exc}`.
//!
//! Each rule reads the namespace as the JSON value Python holds (`View::state`), so a ledger the
//! typed row would refuse gives the same text. The replay is one pass over the audit trail, with
//! sets for the event and reservation IDs, as Python's.
//!
//! Python's ints have any size; a `V::Big` is read as an `i128`, and one past that is `Unsupported`
//! (no ledger holds such a count).

use std::sync::Arc;

use indexmap::IndexMap;
use rustc_hash::{FxHashMap, FxHashSet};

use super::View;
use crate::error::{Error, Result};
use crate::events::is_stable_id;
use crate::rules::dispatch::py_eq;
use crate::value::V;
use crate::world::ResourcesRow;

const LEDGER: &str = "resource_ledger_v1";
const POPULATION: &str = "population_dynamics_v1";
const RESOURCE_IDS: [&str; 2] = ["water_ml", "food_meal_units"];
const LEDGER_FIELDS: [&str; 6] = ["schema_version", "warehouse_id", "zone_id", "accounts", "used_reservation_ids", "audit"];
const ACCOUNT_FIELDS: [&str; 8] = ["initial", "delivered", "served", "losses", "remaining", "reserved", "source_ids", "reservations"];
const BALANCE_FIELDS: [&str; 6] = ["initial", "delivered", "served", "losses", "remaining", "reserved"];
const RESERVATION_FIELDS: [&str; 4] = ["reservation_id", "resource_id", "agent_id", "quantity"];
const AUDIT_FIELDS: [&str; 17] = [
    "schema_version",
    "minute",
    "event_id",
    "operation",
    "resource_id",
    "quantity",
    "reservation_id",
    "agent_id",
    "source_id",
    "source_ids",
    "warehouse_id",
    "initial",
    "delivered",
    "served",
    "losses",
    "remaining",
    "reserved",
];
// The indexes of BALANCE_FIELDS.
const INITIAL: usize = 0;
const DELIVERED: usize = 1;
const SERVED: usize = 2;
const LOSSES: usize = 3;
const REMAINING: usize = 4;
const RESERVED: usize = 5;

type Map = IndexMap<Arc<str>, V>;

/// The check, as `validate` (`validate/mod.rs`) calls it in Python's order.
pub fn validate(view: &View<'_>) -> Result<()> {
    let archived = archived_agent_ids(view)?;
    let ledger = view.state(LEDGER)?;
    let ledger = ledger.as_deref().unwrap_or(&V::Null);
    let resources = view.world.resources()?;
    let rows = view.rows()?;
    let mut agent_ids: FxHashSet<&str> = rows.agents.iter().map(|(id, _)| &**id).collect();
    agent_ids.extend(archived.iter().map(|id| &**id));
    let zone_ids: FxHashSet<&str> = rows.zones.iter().map(|(id, _)| &**id).collect();
    let world = Against { resources: &resources, agent_ids: &agent_ids, zone_ids: &zone_ids };
    if view.history {
        replay(ledger, &world)
    } else {
        state(ledger, &world, view.minute_index()?)
    }
}

/// What the ledger is checked against: `self.resources`, the people (now and archived) and the
/// zones.
struct Against<'a> {
    resources: &'a ResourcesRow,
    agent_ids: &'a FxHashSet<&'a str>,
    zone_ids: &'a FxHashSet<&'a str>,
}

fn ledger_error(text: impl Into<String>) -> Error {
    Error::Ledger(text.into())
}

/// `_valid_id`.
fn valid_id(value: &V) -> bool {
    matches!(value, V::Str(text) if is_stable_id(text))
}

/// The value as a Python int (`isinstance(value, int) and not isinstance(value, bool)`), or None.
fn int(value: &V) -> Result<Option<i128>> {
    match value {
        V::Int(number) => Ok(Some(i128::from(*number))),
        V::Big(text) => text.parse::<i128>().map(Some).map_err(|_| Error::unsupported("a ledger int past 128 bits")),
        _ => Ok(None),
    }
}

/// `set(map) == fields`.
fn has_fields(map: &Map, fields: &[&str]) -> bool {
    map.len() == fields.len() && fields.iter().all(|field| map.contains_key(*field))
}

/// The `TypeError` of hashing a list or a dict. Python holds a namespace no typed row takes as its
/// tracked tree (`core/state_mirror.py`), whose classes the text names.
fn unhashable(value: &V) -> Result<()> {
    match value {
        V::List(_) => Err(Error::Type("unhashable type: 'TrackedList'".to_owned())),
        V::Map(_) => Err(Error::Type("unhashable type: 'TrackedDict'".to_owned())),
        _ => Ok(()),
    }
}

/// The str items of a list.
fn texts(items: &[V]) -> Vec<&str> {
    items.iter().filter_map(|item| if let V::Str(text) = item { Some(&**text) } else { None }).collect()
}

fn repeats(items: &[&str]) -> bool {
    items.iter().collect::<FxHashSet<_>>().len() != items.len()
}

// ---- The identity history of population_dynamics_v1 ----------------------------------------------

/// The people the ledger may name who are not in the world now: `initial_agent_ids` and the
/// admitted arrivals of `population_dynamics_v1`, after its identity checks.
fn archived_agent_ids(view: &View<'_>) -> Result<Vec<Arc<str>>> {
    let invalid = || Error::Value("population_dynamics_v1 identity history is invalid".to_owned());
    let Some(state) = view.state(POPULATION)? else {
        return Ok(Vec::new());
    };
    if !state.is_truthy() {
        return Ok(Vec::new());
    }
    let V::Map(population) = &*state else {
        return Err(invalid());
    };
    let list = |name: &str| match population.get(name) {
        Some(V::List(items)) => Some(items),
        _ => None,
    };
    if !population.get("schema_version").is_some_and(|version| py_eq(version, &V::Int(1))) {
        return Err(invalid());
    }
    let (Some(initial), Some(V::Map(traits)), Some(pending), Some(arrivals)) =
        (list("initial_agent_ids"), population.get("profile_traits"), list("pending_arrivals"), list("arrivals"))
    else {
        return Err(invalid());
    };
    // Only the str members can equal a profile's or a person's ID, but every member is hashed, so
    // a list or a dict among them raises.
    for item in initial {
        unhashable(item)?;
    }
    let mut historical: FxHashSet<&str> = texts(initial).into_iter().collect();
    for item in pending {
        if let V::Map(item) = item {
            if let Some(V::Map(profile)) = item.get("profile") {
                let id = profile.get("id").unwrap_or(&V::Null);
                unhashable(id)?;
                if let V::Str(id) = id {
                    historical.insert(id);
                }
            }
        }
    }
    let admitted: Vec<&Arc<str>> = arrivals
        .iter()
        .filter_map(|item| match item {
            V::Map(item) => match item.get("agent_id") {
                Some(V::Str(id)) => Some(id),
                _ => None,
            },
            _ => None,
        })
        .collect();
    historical.extend(admitted.iter().map(|id| &***id));
    if traits.keys().any(|id| !historical.contains(&**id)) {
        return Err(Error::Value("population_dynamics_v1 contains an unknown profile ID".to_owned()));
    }
    let mut archived: Vec<Arc<str>> =
        initial.iter().filter_map(|item| if let V::Str(id) = item { Some(id.clone()) } else { None }).collect();
    archived.extend(admitted.into_iter().cloned());
    Ok(archived)
}

// ---- _validate_ledger_balances -------------------------------------------------------------------

/// An account as `_validate_ledger_balances` passed it.
struct Account<'a> {
    balances: [i128; 6],
    source_ids: Vec<&'a str>,
    /// Each active reservation's agent and quantity.
    reservations: FxHashMap<&'a str, (&'a str, i128)>,
    /// The account as it is, for `validate_resource_ledger_state`'s comparisons.
    map: &'a Map,
}

/// The ledger as `_validate_ledger_balances` passed it.
struct Ledger<'a> {
    map: &'a Map,
    used_ids: Vec<&'a str>,
    accounts: Vec<Account<'a>>,
}

/// `_validate_ledger_balances`: the header, the balances and the active reservations.
fn balances<'a>(ledger: &'a V, world: &Against<'_>, check_every_reservation_id: bool) -> Result<Ledger<'a>> {
    let map = match ledger {
        V::Map(map) if has_fields(map, &LEDGER_FIELDS) => map,
        _ => return Err(ledger_error("resource ledger fields do not match v1")),
    };
    if !matches!(map["schema_version"], V::Int(1)) {
        return Err(ledger_error("unsupported resource ledger schema version"));
    }
    if !valid_id(&map["warehouse_id"]) {
        return Err(ledger_error("resource ledger warehouse_id is invalid"));
    }
    match &map["zone_id"] {
        V::Null => {}
        V::Str(zone) if is_stable_id(zone) && world.zone_ids.contains(&**zone) => {}
        _ => return Err(ledger_error("resource ledger zone_id is invalid or unknown")),
    }
    let accounts = match &map["accounts"] {
        V::Map(accounts) if has_fields(accounts, &RESOURCE_IDS) => accounts,
        _ => return Err(ledger_error("resource ledger must contain water_ml and food_meal_units")),
    };
    let resources = world.resources;
    if resources.len() != RESOURCE_IDS.len() || RESOURCE_IDS.iter().any(|id| !resources.contains_key(*id)) {
        return Err(ledger_error("WorldState resources must contain exactly the ledger resource IDs"));
    }
    let used_ids = match &map["used_reservation_ids"] {
        V::List(items)
            if items.iter().all(|item| if check_every_reservation_id { valid_id(item) } else { matches!(item, V::Str(_)) }) =>
        {
            texts(items)
        }
        _ => return Err(ledger_error("resource ledger reservation IDs are invalid")),
    };
    let used_set: FxHashSet<&str> = used_ids.iter().copied().collect();
    if used_set.len() != used_ids.len() {
        return Err(ledger_error("resource ledger reservation IDs must be unique"));
    }
    let mut parsed = Vec::with_capacity(RESOURCE_IDS.len());
    for resource_id in RESOURCE_IDS {
        let account = match &accounts[resource_id] {
            V::Map(account) if has_fields(account, &ACCOUNT_FIELDS) => account,
            _ => return Err(ledger_error(format!("resource account '{resource_id}' fields do not match v1"))),
        };
        let mut quantities = [0i128; 6];
        for (index, name) in BALANCE_FIELDS.iter().enumerate() {
            match int(&account[*name])? {
                Some(value) if value >= 0 => quantities[index] = value,
                _ => return Err(ledger_error(format!("resource '{resource_id}' {name} must be non-negative"))),
            }
        }
        let expected = quantities[INITIAL] + quantities[DELIVERED] - (quantities[SERVED] + quantities[LOSSES]);
        if expected != quantities[REMAINING] {
            return Err(ledger_error(format!("resource '{resource_id}' accounting does not balance")));
        }
        if resources.get(resource_id).map(|value| i128::from(*value)) != Some(quantities[REMAINING]) {
            return Err(ledger_error(format!("WorldState resource '{resource_id}' does not match ledger remaining")));
        }
        let V::Map(reservations) = &account["reservations"] else {
            return Err(ledger_error(format!("resource '{resource_id}' reservations must be an object")));
        };
        let mut active = FxHashMap::default();
        let mut reservation_total = 0i128;
        for (reservation_id, record) in reservations {
            if !is_stable_id(reservation_id) {
                return Err(ledger_error("resource reservation ID is invalid"));
            }
            if !used_set.contains(&**reservation_id) {
                return Err(ledger_error("active reservation ID is missing from its history"));
            }
            let record = match record {
                V::Map(record) if has_fields(record, &RESERVATION_FIELDS) => record,
                _ => return Err(ledger_error("resource reservation record fields do not match v1")),
            };
            let agent_id = match &record["agent_id"] {
                V::Str(agent) if is_stable_id(agent) && world.agent_ids.contains(&**agent) => &**agent,
                _ => return Err(ledger_error("resource reservation identity is invalid")),
            };
            if !matches!(&record["reservation_id"], V::Str(id) if id == reservation_id)
                || !matches!(&record["resource_id"], V::Str(id) if &**id == resource_id)
            {
                return Err(ledger_error("resource reservation identity is invalid"));
            }
            let quantity = match int(&record["quantity"])? {
                Some(quantity) if quantity > 0 => quantity,
                _ => return Err(ledger_error("active reservation quantity must be a positive integer")),
            };
            reservation_total += quantity;
            active.insert(&**reservation_id, (agent_id, quantity));
        }
        if reservation_total != quantities[RESERVED] {
            return Err(ledger_error(format!("resource '{resource_id}' reserved total does not match")));
        }
        if quantities[RESERVED] > quantities[REMAINING] {
            return Err(ledger_error(format!("resource '{resource_id}' reserves more than remains")));
        }
        let source_ids = match &account["source_ids"] {
            V::List(items) if items.iter().all(valid_id) => texts(items),
            _ => return Err(ledger_error(format!("resource '{resource_id}' source_ids are invalid"))),
        };
        if repeats(&source_ids) {
            return Err(ledger_error(format!("resource '{resource_id}' source_ids must be unique")));
        }
        parsed.push(Account { balances: quantities, source_ids, reservations: active, map: account });
    }
    Ok(Ledger { map, used_ids, accounts: parsed })
}

// ---- _validate_audit_record ----------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Operation {
    Initial,
    Delivery,
    Loss,
    Reserve,
    Release,
    Consume,
}

/// An audit record as `_validate_audit_record` passed it.
struct Record<'a> {
    minute: i128,
    event_id: Option<&'a str>,
    operation: Operation,
    resource: usize,
    quantity: i128,
    reservation_id: Option<&'a str>,
    agent_id: Option<&'a str>,
    source_id: Option<&'a str>,
    source_ids: Vec<&'a str>,
    balances: [i128; 6],
}

/// `_validate_audit_record(record, ledger)`.
fn audit_record<'a>(record: &'a V, warehouse: &V) -> Result<Record<'a>> {
    let record = match record {
        V::Map(record) if has_fields(record, &AUDIT_FIELDS) => record,
        _ => return Err(ledger_error("resource ledger audit record fields do not match v1")),
    };
    if !matches!(record["schema_version"], V::Int(1)) {
        return Err(ledger_error("unsupported resource audit schema version"));
    }
    let invalid = || ledger_error("resource ledger audit values are invalid");
    let minute = match int(&record["minute"])? {
        Some(minute) if minute >= 0 => minute,
        _ => return Err(invalid()),
    };
    let resource = match &record["resource_id"] {
        V::Str(id) => RESOURCE_IDS.iter().position(|known| **known == **id).ok_or_else(invalid)?,
        _ => return Err(invalid()),
    };
    // `operation not in {...}` hashes it.
    unhashable(&record["operation"])?;
    let operation = match &record["operation"] {
        V::Str(text) => match &**text {
            "initial" => Operation::Initial,
            "delivery" => Operation::Delivery,
            "loss" => Operation::Loss,
            "reserve" => Operation::Reserve,
            "release" => Operation::Release,
            "consume" => Operation::Consume,
            _ => return Err(invalid()),
        },
        _ => return Err(invalid()),
    };
    let quantity = match int(&record["quantity"])? {
        Some(quantity) if quantity >= 0 => quantity,
        _ => return Err(invalid()),
    };
    let mut ids = [None; 4];
    for (index, name) in ["event_id", "reservation_id", "agent_id", "source_id"].into_iter().enumerate() {
        match &record[name] {
            V::Null => {}
            V::Str(id) if is_stable_id(id) => ids[index] = Some(&**id),
            _ => return Err(ledger_error("resource ledger audit IDs are invalid")),
        }
    }
    let source_ids = match &record["source_ids"] {
        V::List(items) if items.iter().all(valid_id) => texts(items),
        _ => return Err(ledger_error("resource ledger audit source_ids are invalid")),
    };
    if repeats(&source_ids) {
        return Err(ledger_error("resource ledger audit source_ids must be unique"));
    }
    if !py_eq(&record["warehouse_id"], warehouse) {
        return Err(ledger_error("resource audit warehouse_id does not match its ledger"));
    }
    let mut balances = [0i128; 6];
    for (index, name) in BALANCE_FIELDS.iter().enumerate() {
        match int(&record[*name])? {
            Some(value) if value >= 0 => balances[index] = value,
            _ => return Err(ledger_error(format!("resource audit {name} must be non-negative"))),
        }
    }
    let [event_id, reservation_id, agent_id, source_id] = ids;
    Ok(Record { minute, event_id, operation, resource, quantity, reservation_id, agent_id, source_id, source_ids, balances })
}

/// `ledger["audit"]`, a list with the initial records' room.
fn audit_of<'a>(ledger: &Ledger<'a>) -> Result<&'a [V]> {
    let V::List(audit) = &ledger.map["audit"] else {
        return Err(ledger_error("resource ledger audit must be a list"));
    };
    if audit.len() < RESOURCE_IDS.len() {
        return Err(ledger_error("resource ledger audit is missing initial records"));
    }
    Ok(audit)
}

// ---- validate_resource_ledger --------------------------------------------------------------------

/// An account as the audit trail replays it.
struct Replay<'a> {
    balances: [i128; 6],
    source_ids: Vec<&'a str>,
    reservations: FxHashMap<&'a str, (&'a str, i128)>,
}

/// `validate_resource_ledger`: the whole audit trail replayed, in one pass, and compared with the
/// balances.
fn replay(ledger: &V, world: &Against<'_>) -> Result<()> {
    let ledger = balances(ledger, world, true)?;
    let audit = audit_of(&ledger)?;
    let warehouse = &ledger.map["warehouse_id"];
    let mut replays: [Option<Replay<'_>>; 2] = [None, None];
    let mut used_history: Vec<&str> = Vec::new();
    let mut used_history_set: FxHashSet<&str> = FxHashSet::default();
    let mut event_ids: FxHashSet<&str> = FxHashSet::default();
    let mut last_audit_minute = 0i128;
    let known_agent = |agent: &str| is_stable_id(agent) && world.agent_ids.contains(agent);
    for (index, record) in audit.iter().enumerate() {
        let record = audit_record(record, warehouse)?;
        if record.minute < last_audit_minute {
            return Err(ledger_error("resource audit minutes must be non-decreasing"));
        }
        last_audit_minute = record.minute;
        if record.operation == Operation::Initial {
            if index >= RESOURCE_IDS.len() || record.resource != index {
                return Err(ledger_error("initial resource audit records must lead in stable order"));
            }
            if record.event_id.is_some() || record.reservation_id.is_some() || record.agent_id.is_some() || record.source_id.is_some() {
                return Err(ledger_error("initial resource audit cannot reference an event or Agent"));
            }
            if record.minute != 0 {
                return Err(ledger_error("initial resource audit values are invalid"));
            }
            if record.quantity != record.balances[INITIAL] {
                return Err(ledger_error("initial resource audit quantity does not match balance"));
            }
            let mut sorted = record.source_ids.clone();
            sorted.sort_unstable();
            sorted.dedup();
            if record.source_ids != sorted {
                return Err(ledger_error("initial resource audit source_ids must be sorted and unique"));
            }
            let mut balances = [0i128; 6];
            balances[INITIAL] = record.quantity;
            balances[REMAINING] = record.quantity;
            replays[record.resource] =
                Some(Replay { balances, source_ids: record.source_ids.clone(), reservations: FxHashMap::default() });
        } else {
            if index < RESOURCE_IDS.len() {
                return Err(ledger_error("initial resource audit records must lead in stable order"));
            }
            if record.quantity == 0 {
                return Err(ledger_error("resource event audit quantity must be positive"));
            }
            let (Some(event_id), Some(source_id)) = (record.event_id, record.source_id) else {
                return Err(ledger_error("resource event audit requires event_id and source_id"));
            };
            if !event_ids.insert(event_id) {
                return Err(ledger_error("resource audit event IDs must be unique"));
            }
            let has_reservation = matches!(record.operation, Operation::Reserve | Operation::Release | Operation::Consume);
            if has_reservation != record.reservation_id.is_some() || has_reservation != record.agent_id.is_some() {
                return Err(ledger_error("resource audit reservation identity is incomplete"));
            }
            if !record.source_ids.contains(&source_id) {
                return Err(ledger_error("resource audit source is missing from source_ids"));
            }
            let Some(replay) = replays[record.resource].as_mut() else {
                return Err(ledger_error("resource operation precedes its initial audit record"));
            };
            let quantity = record.quantity;
            // A reservation's operation has both IDs (checked above).
            let reservation = record.reservation_id.zip(record.agent_id);
            match (record.operation, reservation) {
                (Operation::Delivery, _) => {
                    replay.balances[DELIVERED] += quantity;
                    replay.balances[REMAINING] += quantity;
                }
                (Operation::Loss, _) => {
                    if quantity > replay.balances[REMAINING] - replay.balances[RESERVED] {
                        return Err(ledger_error("resource audit loss exceeds available inventory"));
                    }
                    replay.balances[LOSSES] += quantity;
                    replay.balances[REMAINING] -= quantity;
                }
                (Operation::Reserve, Some((reservation_id, agent_id))) => {
                    if used_history_set.contains(reservation_id) {
                        return Err(ledger_error("resource audit reservation ID was reused"));
                    }
                    if quantity > replay.balances[REMAINING] - replay.balances[RESERVED] {
                        return Err(ledger_error("resource audit reservation exceeds available stock"));
                    }
                    if !known_agent(agent_id) {
                        return Err(ledger_error("resource audit references an unknown Agent"));
                    }
                    used_history.push(reservation_id);
                    used_history_set.insert(reservation_id);
                    replay.reservations.insert(reservation_id, (agent_id, quantity));
                    replay.balances[RESERVED] += quantity;
                }
                (Operation::Release | Operation::Consume, Some((reservation_id, agent_id))) => {
                    let reserved = match replay.reservations.get(reservation_id) {
                        Some((agent, reserved)) if *agent == agent_id => *reserved,
                        _ => return Err(ledger_error("resource audit reservation history is invalid")),
                    };
                    if !known_agent(agent_id) {
                        return Err(ledger_error("resource audit references an unknown Agent"));
                    }
                    if record.operation == Operation::Release {
                        if quantity != reserved {
                            return Err(ledger_error("resource audit release quantity is invalid"));
                        }
                        replay.balances[RESERVED] -= reserved;
                        replay.reservations.remove(reservation_id);
                    } else {
                        if quantity > reserved {
                            return Err(ledger_error("resource audit consume exceeds its reservation"));
                        }
                        replay.balances[SERVED] += quantity;
                        replay.balances[REMAINING] -= quantity;
                        replay.balances[RESERVED] -= quantity;
                        if quantity == reserved {
                            replay.reservations.remove(reservation_id);
                        } else {
                            replay.reservations.insert(reservation_id, (agent_id, reserved - quantity));
                        }
                    }
                }
                _ => return Err(Error::unsupported("a reservation's operation without its IDs")),
            }
            if !replay.source_ids.contains(&source_id) {
                replay.source_ids.push(source_id);
            }
            if record.source_ids != replay.source_ids {
                return Err(ledger_error("resource audit source_ids do not match replay"));
            }
        }
        // Every record reaching here has its resource's replay: an initial one made it, and any
        // other is refused above without one.
        let replay = replays[record.resource].as_ref().ok_or_else(|| Error::unsupported("a record without its replay"))?;
        if record.balances != replay.balances {
            return Err(ledger_error("resource audit after-state does not match replay"));
        }
    }
    if ledger.used_ids != used_history {
        return Err(ledger_error("resource reservation history does not match audit replay"));
    }
    for (index, resource_id) in RESOURCE_IDS.iter().enumerate() {
        let account = &ledger.accounts[index];
        let Some(replay) = replays[index].as_ref() else {
            return Err(ledger_error(format!("resource '{resource_id}' initial audit is missing")));
        };
        if account.balances != replay.balances {
            return Err(ledger_error(format!("resource '{resource_id}' balance does not match audit")));
        }
        if account.source_ids != replay.source_ids {
            return Err(ledger_error(format!("resource '{resource_id}' source IDs do not match audit")));
        }
        if account.reservations != replay.reservations {
            return Err(ledger_error(format!("resource '{resource_id}' reservations do not match audit")));
        }
    }
    Ok(())
}

// ---- validate_resource_ledger_state --------------------------------------------------------------

/// `records_since_previous_tick(history, minute_index)` of `core/history.py`: the records of the
/// last tick and the one before them.
fn records_since_previous_tick(history: &[V], minute_index: i64) -> &[V] {
    let mut start = history.len();
    while start > 0 {
        let minute = match &history[start - 1] {
            V::Map(item) => item.get("minute"),
            _ => None,
        };
        // `type(minute) is int and minute < minute_index - 1`; a Big is past any minute either way.
        let before = match minute {
            Some(V::Int(minute)) => i128::from(*minute) < i128::from(minute_index) - 1,
            Some(V::Big(text)) => text.starts_with('-'),
            _ => false,
        };
        if before {
            break;
        }
        start -= 1;
    }
    &history[start.saturating_sub(1)..]
}

/// `validate_resource_ledger_state`: the balances, the records of the last tick, and the newest
/// record of each resource against its account.
fn state(ledger: &V, world: &Against<'_>, minute_index: i64) -> Result<()> {
    let ledger = balances(ledger, world, false)?;
    let audit = audit_of(&ledger)?;
    let warehouse = &ledger.map["warehouse_id"];
    let mut previous_minute: Option<i128> = None;
    for record in records_since_previous_tick(audit, minute_index) {
        let record = audit_record(record, warehouse)?;
        if previous_minute.is_some_and(|previous| record.minute < previous) {
            return Err(ledger_error("resource audit minutes must be non-decreasing"));
        }
        previous_minute = Some(record.minute);
    }
    for (index, resource_id) in RESOURCE_IDS.iter().enumerate() {
        let account = ledger.accounts[index].map;
        let newest = audit.iter().rev().find_map(|record| match record {
            V::Map(record) if matches!(record.get("resource_id"), Some(V::Str(id)) if &**id == *resource_id) => Some(record),
            _ => None,
        });
        let Some(newest) = newest else {
            return Err(ledger_error(format!("resource '{resource_id}' initial audit is missing")));
        };
        if BALANCE_FIELDS.iter().any(|name| !py_eq(newest.get(*name).unwrap_or(&V::Null), &account[*name])) {
            return Err(ledger_error(format!("resource '{resource_id}' balance does not match audit")));
        }
        if !py_eq(newest.get("source_ids").unwrap_or(&V::Null), &account["source_ids"]) {
            return Err(ledger_error(format!("resource '{resource_id}' source IDs do not match audit")));
        }
    }
    Ok(())
}
