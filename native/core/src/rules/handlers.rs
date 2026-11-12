//! The rest of `events/handlers.py`, on the rows of a `World` (moved to the core in P8 d2):
//! proximity contacts, the resource ledger's events, facility outages and recoveries, the
//! operational events (deliveries, water, staff), the checks of an EffectProgram's population
//! primitive, and the infrastructure events. The PyO3 crate's file of the same name reads what Python
//! holds (whether `model_state` holds each namespace, the floor plan, the sections of the configs it
//! keeps by stamp, the manager of the resource service), makes the event's row, and calls these.
//!
//! The checks and their texts are the Python rules', in their order. A suppressed or failed event is
//! the rule's `Error::suppressed` or `Error::failed`, with the world as it was: a handler that writes
//! before it may fail runs in the transaction that stands for the Python rule's copy of the world
//! (the core's `txn::rule` since lane P9-1b: the `*_event` entries at the end read their inputs
//! through the host and open it; the population primitives' bodies are still the PyO3 crate's), and
//! its error rolls the transaction back.
//!
//! The end of an operational transaction (`settle`) is the core's: the event leaves the queue, its
//! APPLIED record and the follow-up records go to the log, a continuity receipt goes into
//! `shared_operations_v1`, and the whole world is validated through the host (`Host::validate_world`,
//! or `Host::validate_in_flight` while the event is still pending; policy E6). The records a receipt's
//! rule reads are looked up through the host (`Host::find_record`): the core's EventLog does not keep
//! the bytes of records Python has taken. The infrastructure's spec is the contract's dump
//! (`Host::contract`, policy E15), and the delivery delays inside a supply route's interruption
//! validate the world as a transaction's start does (`Host::validate_candidate`): they run inside the
//! outer rule's transaction.
//!
//! The queue's slots a rule takes out are added to `taken`, as it goes, for the PyO3 crate to let
//! their objects go (`QueueStore::forget`); so is the slot of a pending event a rule replaced, whose
//! object the PyO3 crate makes again from its new row.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use indexmap::IndexMap;

use crate::cell::ShardRef;
use crate::config::environment::Infrastructure;
use crate::config::proximity::Proximity;
use crate::entities::{Activity, RequestStatus, Table};
use crate::error::{Error, Result};
use crate::events::writer::{WriteError, WriteResult};
use crate::events::{EventKind, EventRow, EventStatus};
use crate::host::{Contract, Host, HostFailure, Stage, TxnKind};
use crate::json::python_float_repr;
use crate::mathx::{hypot, py_is_close};
use crate::row::ValueRow;
use crate::rules::care_receipts::{self, Made};
use crate::rules::common_operations::{self, py_eq, Found, Receipt, CONTINUITY_EVENT};
use crate::rules::kit::movement::config_read;
use crate::rules::kit::{self, Counts, Events, Held};
use crate::rules::movement::{self, KEY as MOVEMENT};
use crate::rules::resources::{self, Applied, Fail, LedgerFault, OperationArg, OperationEvent};
use crate::rules::service::{self, Failure, Manager, Pending};
use crate::rules::staff_operation_events::{self as staff_ops, Stopping};
use crate::state::effect_program::{EffectProgramRow, NAME as PROGRAM};
use crate::state::movement::{MovementRow, Position};
use crate::state::resource_service::{ServiceRow, NAME as SERVICE};
use crate::state::staff_operations::{Phase, StaffOperationsRow, NAME as STAFF_OPERATIONS};
use crate::text::py_repr;
use crate::value::V;
use crate::world::{World, WorldCell, MODEL_CONFIGS};

pub const OPERATIONAL: &str = "operational_events_v1";
pub const INFRASTRUCTURE: &str = "infrastructure_v1";
const PROXIMITY_SOURCE: &str = "ASM-PROXIMITY-CONTACT-V1";
const OPERATIONAL_SOURCE: &str = "ASM-H03-OPERATIONAL-EVENTS-V1";
const OPERATIONAL_POLICY: &str = "operational-events-v1";
const INFRASTRUCTURE_POLICY: &str = "infrastructure-events-v1";
const SUPPORT_SOURCE: &str = "ASM-STAFF-SUPPORT-LIFECYCLE-V1";
/// `STAFF_SUPPORT_POLICY_VERSION` of `domain/staff.py`.
pub const STAFF_SUPPORT_POLICY: &str = "staff-support-fifo-aging-v2";
const SHARED_CONFIG: &str = "shared_operations";
/// The text of `held` of `domain/common_operations.py` for a world without the namespace.
const NOT_SELECTED: &str = "shared_operations_v1 must be selected in the immutable Scenario";

/// The refusal of a rule that reads the resource service's manager, which the PyO3 crate could not
/// make (`ResourceServiceManager(world)` raised): the glue raises what that raised, where the rule
/// made it.
pub const SERVICE_UNREAD: &str = "the resource service's manager could not be made";
/// The refusal of a rule that reads `support_cooldown_minutes`, whose read raised: the glue raises
/// what it raised.
pub const COOLDOWN_UNREAD: &str = "the support cooldown could not be read";

// ---- What ran and what was refused ------------------------------------------------------------------

/// What the handlers ran and refused, by label: the PyO3 crate's `stats`, and its `counted` of each
/// entry. The core counts the labels it reaches inside a rule (`interrupt_for_staff`).
pub static COUNTS: Counts = Counts::new();

// ---- Reading values ---------------------------------------------------------------------------------

static NULL: V = V::Null;

/// `map.get(key)` of a str key: None (`V::Null`) for a key it does not have. Unsupported for a value
/// that is not a mapping.
#[track_caller]
fn get<'a>(map: &'a V, key: &str) -> Result<&'a V> {
    kit::get(map, key)
}

/// `map[key]`: `KeyError(key)` for a key it does not have.
#[track_caller]
fn item<'a>(map: &'a V, key: &str) -> Result<&'a V> {
    match map {
        V::Map(entries) => entries.get(key).ok_or_else(|| Error::Key(key.to_owned())),
        _ => Err(Error::unsupported("a value is not a plain dict")),
    }
}

/// `map.get(key)` of any key, as a dict of str keys answers it: None for a key that is not a str.
/// A key Python cannot hash (a list or a dict) raises `TypeError`, which is refused.
#[track_caller]
fn lookup<'a>(map: &'a V, key: &V) -> Result<Option<&'a V>> {
    let V::Map(entries) = map else {
        return Err(Error::unsupported("a value is not a plain dict"));
    };
    match key {
        V::Str(key) => Ok(entries.get(&**key)),
        V::List(_) | V::Map(_) => Err(Error::unsupported("a key is not hashable")),
        _ => Ok(None),
    }
}

/// `key in table`, of a table whose keys are str.
#[track_caller]
fn contains<R>(table: &Table<R>, key: &V) -> Result<bool> {
    match key {
        V::Str(key) => Ok(table.contains_key(key)),
        V::List(_) | V::Map(_) => Err(Error::unsupported("a key is not hashable")),
        _ => Ok(false),
    }
}

/// `set(map) == set(names)`, of distinct names; a value that is not a mapping is not one.
fn fields_are(map: &V, names: &[&str]) -> bool {
    match map {
        V::Map(entries) => entries.len() == names.len() && names.iter().all(|name| entries.contains_key(*name)),
        _ => false,
    }
}

/// `value == text`.
fn eq_text(value: &V, text: &str) -> bool {
    matches!(value, V::Str(found) if &**found == text)
}

/// `value == number`.
fn eq_int(value: &V, number: i64) -> bool {
    py_eq(value, &V::Int(number))
}

fn str_v(text: &str) -> V {
    V::Str(Arc::from(text))
}

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

fn optional(text: &Option<Arc<str>>) -> V {
    text.clone().map_or(V::Null, V::Str)
}

/// An int that is not a bool, of any size: Python compares one past 64 bits with the rule's i64
/// values, and so does this.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wide {
    Int(i64),
    Above,
    Below,
}

fn wide_int(value: &V) -> Option<Wide> {
    match value {
        V::Int(number) => Some(Wide::Int(*number)),
        V::Big(digits) => Some(if digits.starts_with('-') { Wide::Below } else { Wide::Above }),
        _ => None,
    }
}

/// `isinstance(value, str) and value`.
fn nonempty_str(value: &V) -> Option<&Arc<str>> {
    match value {
        V::Str(text) if !text.is_empty() => Some(text),
        _ => None,
    }
}

/// `type(value) is int`: Some(value), None when it isn't.
#[track_caller]
fn exactly_int(value: &V) -> Result<Option<i64>> {
    match value {
        V::Int(number) => Ok(Some(*number)),
        V::Big(_) => Err(Error::unsupported("an int is too large")),
        _ => Ok(None),
    }
}

/// `not isinstance(value, bool) and isinstance(value, (int, float))`, as `float(value)`. An int a
/// float can't hold exactly is refused.
#[track_caller]
fn real_number(value: &V) -> Result<Option<f64>> {
    match value {
        V::Float(number) => Ok(Some(*number)),
        V::Int(number) if (-(1 << 53)..=1 << 53).contains(number) => Ok(Some(*number as f64)),
        V::Int(_) | V::Big(_) => Err(Error::unsupported("a number does not fit a float")),
        _ => Ok(None),
    }
}

/// `repr(value)` of a scalar, for the `{x!r}` of a rule's text.
#[track_caller]
fn repr(value: &V) -> Result<String> {
    Ok(match value {
        V::Null => "None".to_owned(),
        V::Bool(true) => "True".to_owned(),
        V::Bool(false) => "False".to_owned(),
        V::Int(number) => number.to_string(),
        V::Big(digits) => digits.to_string(),
        V::Float(number) => python_float_repr(*number),
        V::Str(text) => py_repr(text),
        V::List(_) | V::Map(_) => return Err(Error::unsupported("a value whose repr is Python's")),
    })
}

/// `model_state.get(name)` as the row of the namespace the world holds: None for an absent key.
#[track_caller]
fn state<'w>(world: &'w World, name: &str, held: &Held) -> Result<Option<ShardRef<'w, V>>> {
    match kit::held_row::<ValueRow>(world, name, held)? {
        None => Ok(None),
        Some(row) => ShardRef::try_map(row, |row| Ok(&row.0)).map(Some),
    }
}

/// `model_state.get(name)` as a value: `V::Null` for an absent key, as `get` answers.
fn state_value(world: &World, name: &str, held: &Held) -> Result<V> {
    Ok(state(world, name, held)?.map_or(V::Null, |value| (*value).clone()))
}

/// The value at this path of the namespace's row, to write: each step a key of a plain dict.
#[track_caller]
fn at_mut<'a>(mut value: &'a mut V, path: &[&str]) -> Result<&'a mut V> {
    for key in path {
        let V::Map(entries) = value else {
            return Err(Error::unsupported("a value is not a plain dict"));
        };
        value = entries.get_mut(*key).ok_or_else(|| Error::Key((*key).to_owned()))?;
    }
    Ok(value)
}

/// `record[key] = value` of a plain dict: in place, or at the end for a new key.
#[track_caller]
fn put(record: &mut V, key: &str, value: V) -> Result<()> {
    let V::Map(entries) = record else {
        return Err(Error::unsupported("a value is not a plain dict"));
    };
    entries.insert(Arc::from(key), value);
    Ok(())
}

/// Write the record at this path of a namespace's row (its tree follows the row, lane P8-0c).
fn write_record(world: &World, name: &str, path: &[&str], f: impl FnOnce(&mut V) -> Result<()>) -> Result<()> {
    let mut row = world.row_mut::<ValueRow>(name)?;
    f(at_mut(&mut row.0, path)?)
}

/// The key of a record a rule found by this value: a str.
#[track_caller]
fn exact_key(value: &V) -> Result<Arc<str>> {
    kit::exact_str(value).cloned()
}

/// The pending events in queue order, as their rows (the PyO3 crate reads the rows of the events
/// Python scheduled first).
fn pending_rows(world: &World) -> Result<Vec<Arc<EventRow>>> {
    let queue = world.queue()?;
    queue
        .ordered()
        .iter()
        .map(|(_, serial)| queue.slot(*serial).row.clone().ok_or_else(|| Error::unsupported("a pending event has no row")))
        .collect()
}

/// The pending event a value names, as `next(e for e in pending if e.event_id == wanted)` finds it,
/// with its slot.
fn pending_named(world: &World, wanted: &V) -> Result<Option<(u64, Arc<EventRow>)>> {
    let V::Str(wanted) = wanted else {
        return Ok(None);
    };
    let queue = world.queue()?;
    match queue.serial_of(wanted) {
        None => Ok(None),
        Some(serial) => {
            let row = queue.slot(serial).row.clone().ok_or_else(|| Error::unsupported("a pending event has no row"))?;
            Ok(Some((serial, row)))
        }
    }
}

/// `_ensure_event_identity_free`: no pending event and no record has this ID or sequence.
fn identity_free(world: &World, event_id: &str, sequence: i64) -> Result<()> {
    let pending = {
        let queue = world.queue()?;
        queue.has_event(event_id) || queue.has_sequence(sequence)
    };
    let realized = {
        let log = world.log()?;
        log.has_event_id(event_id) || log.has_sequence(sequence)
    };
    if pending || realized {
        return Err(Error::failed(format!("generated event identity {} is not unique", py_repr(event_id))));
    }
    Ok(())
}

/// `derived_effect_marker` (events/effects.py): a copy in the same order, phase `derived` and no
/// guard. The guard decides whether the start applies; what the start schedules isn't guarded again
/// (#89).
fn derived_of(marker: &IndexMap<Arc<str>, V>) -> V {
    let mut derived = marker.clone();
    derived.shift_remove("guard");
    derived.insert(Arc::from("phase"), str_v("derived"));
    V::Map(derived)
}

/// `derived_effect_marker(event.to_dict()["provenance"]["effect_program_v1"])` if the event has a
/// marker.
fn derived_marker(event: &EventRow) -> Option<V> {
    match kit::provenance(event, "effect_program_v1") {
        Some(V::Map(marker)) => Some(derived_of(marker)),
        _ => None,
    }
}

/// A record the handler gives the dispatcher: the outcome of an event of its own, which the
/// dispatcher appends unless the log holds it already.
#[derive(Clone, Debug, PartialEq)]
pub struct Followed {
    pub event: EventRow,
    pub minute: i64,
    pub detail: Option<String>,
}

impl From<Made> for Followed {
    fn from(made: Made) -> Self {
        Followed { event: made.event, minute: made.minute, detail: Some(made.detail) }
    }
}

// ---- ProximityContactHandler ------------------------------------------------------------------------

/// `ProximityContactHandler.apply`: it checks and changes nothing. `config` is
/// `canonical_proximity_contact_config(model_configs.get("social_proximity"))`, None for none;
/// `movement` whether the world holds `movement_v1`.
pub fn proximity(world: &World, config: Option<&Proximity>, movement: &Held, event: &EventRow) -> Result<()> {
    let invalid_time = || Err(Error::failed("proximity contact provenance or time is invalid"));
    let Some(config) = config else {
        return invalid_time();
    };
    let provenance = &event.provenance;
    if event.kind != EventKind::Endogenous
        || event.due_minute != world.minute_index()?
        || event.source_id.as_deref() != Some(PROXIMITY_SOURCE)
        || !fields_are(provenance, &["source_type", "source_id", "policy_version", "observed_minute"])
        || !eq_text(get(provenance, "source_type")?, "assumption")
        || !eq_text(get(provenance, "source_id")?, PROXIMITY_SOURCE)
        || !eq_text(get(provenance, "policy_version")?, &config.policy_version)
    {
        return invalid_time();
    }
    let due = event.due_minute;
    let payload = &event.payload;
    if !fields_are(
        payload,
        &["first_agent_id", "second_agent_id", "zone_id", "observed_minute", "contact_minutes", "distance_m", "first_point", "second_point"],
    ) {
        return Err(Error::failed("proximity contact payload fields do not match"));
    }
    let zone_value = item(payload, "zone_id")?;
    // `or` in the Python rule's order: each test is reached only if those before it passed.
    let valid = (|| -> Result<Option<(Arc<str>, Arc<str>, f64)>> {
        let (Some(first_id), Some(second_id)) = (nonempty_str(item(payload, "first_agent_id")?), nonempty_str(item(payload, "second_agent_id")?)) else {
            return Ok(None);
        };
        let rows = world.rows()?;
        if first_id == second_id || !rows.agents.contains_key(first_id) || !rows.agents.contains_key(second_id) {
            return Ok(None);
        }
        if !matches!(zone_value, V::Str(_)) || !contains(&rows.zones, zone_value)? {
            return Ok(None);
        }
        if exactly_int(item(payload, "observed_minute")?)? != Some(due)
            || exactly_int(get(provenance, "observed_minute")?)? != Some(due)
            || exactly_int(item(payload, "contact_minutes")?)? != Some(0)
        {
            return Ok(None);
        }
        let Some(distance) = real_number(item(payload, "distance_m")?)? else {
            return Ok(None);
        };
        if !distance.is_finite() || distance < 0.0 || distance > config_read(config.max_distance_m.clone())? {
            return Ok(None);
        }
        Ok(Some((first_id.clone(), second_id.clone(), distance)))
    })()?;
    let Some((first_id, second_id, distance)) = valid else {
        return Err(Error::failed("proximity contact participants or measurement are invalid"));
    };
    let first_point = item(payload, "first_point")?;
    let second_point = item(payload, "second_point")?;
    let mut coordinates = [0.0_f64; 4];
    for (index, point) in [first_point, second_point].into_iter().enumerate() {
        if !fields_are(point, &["x", "y"]) {
            return Err(Error::failed("proximity contact coordinates are invalid"));
        }
        let (x, y) = (real_number(item(point, "x")?)?, real_number(item(point, "y")?)?);
        let (Some(x), Some(y)) = (x, y) else {
            return Err(Error::failed("proximity contact coordinates are invalid"));
        };
        if !x.is_finite() || !y.is_finite() {
            return Err(Error::failed("proximity contact coordinates are invalid"));
        }
        coordinates[index * 2] = x;
        coordinates[index * 2 + 1] = y;
    }
    let measured = hypot(&[coordinates[2] - coordinates[0], coordinates[3] - coordinates[1]]);
    if !py_is_close(distance, measured) && (measured - distance).abs() > 1e-9 {
        return Err(Error::failed("proximity contact distance does not match coordinates"));
    }
    let row = kit::held_row::<MovementRow>(world, MOVEMENT, movement)?;
    for (id, point) in [(&first_id, first_point), (&second_id, second_point)] {
        let position = row.as_ref().and_then(|row| row.positions.get(&**id));
        let fresh = position.is_some_and(|position| {
            &*position.position_kind == "physical" && eq_text(zone_value, &position.zone_id) && py_eq(&point_value(position), point)
        });
        if !fresh {
            return Err(Error::suppressed("proximity contact sample is stale"));
        }
    }
    Ok(())
}

/// A position's `point` as its record holds it.
fn point_value(position: &Position) -> V {
    match &position.point {
        None => V::Null,
        Some(point) => {
            let mut entries = IndexMap::with_capacity(2);
            entries.insert(text("x"), V::Float(point.x));
            entries.insert(text("y"), V::Float(point.y));
            V::Map(entries)
        }
    }
}

// ---- ResourceLedgerHandler --------------------------------------------------------------------------

/// What the ledger's handler reads of the namespaces.
pub struct LedgerIn<'a> {
    pub operational: &'a Held,
    pub service: &'a Held,
    pub ledger: &'a Held,
}

/// The payload the ledger reads of an event: a shipment's delivery without its `shipment_id`, after
/// its checks against the operational state; and the checks of a reservation the resource service
/// owns.
fn ledger_payload(world: &World, inputs: &LedgerIn<'_>, event: &EventRow, operation: &str) -> Result<V> {
    let payload = &event.payload;
    let V::Map(entries) = payload else {
        return Err(Error::unsupported("a payload cannot be read"));
    };
    let mut ledger_payload = payload.clone();
    if operation == "delivery" && entries.contains_key("shipment_id") {
        let operational = state_value(world, OPERATIONAL, inputs.operational)?;
        let shipment = if operational.is_truthy() {
            match get(&operational, "shipments")? {
                V::Null => None,
                shipments => lookup(shipments, item(payload, "shipment_id")?)?.cloned(),
            }
        } else {
            None
        };
        let matches = fields_are(payload, &["warehouse_id", "resource_id", "quantity", "shipment_id"])
            && match &shipment {
                Some(shipment @ V::Map(_)) => {
                    eq_text(get(shipment, "event_id")?, &event.event_id)
                        && py_eq(get(shipment, "resource_id")?, item(payload, "resource_id")?)
                        && eq_int(get(shipment, "due_minute")?, event.due_minute)
                }
                _ => false,
            };
        if !matches {
            return Err(Error::failed("shipment delivery does not match operational state"));
        }
        // In the order of the payload, as the Python rule's comprehension.
        let mut without = entries.clone();
        without.shift_remove("shipment_id");
        ledger_payload = V::Map(without);
    }
    if world.schema_version()? == 6 && *inputs.service != Held::Absent {
        let reservation = get(&ledger_payload, "reservation_id")?;
        if matches!(operation, "release" | "consume") {
            if let V::Str(reservation) = reservation {
                let owned = match kit::held_row::<ServiceRow>(world, SERVICE, inputs.service)? {
                    Some(row) => row.active_reservation_ids.iter().any(|known| known == reservation),
                    None => false,
                };
                if owned {
                    return Err(Error::failed("service-owned resource reservations must be completed or cancelled by ResourceServiceManager"));
                }
            }
        }
        if operation == "reserve" {
            let rows = world.rows()?;
            for (_, task) in rows.active_tasks.iter() {
                if eq_text(reservation, &task.id) && &*task.kind == "resource_service_queue" {
                    return Err(Error::failed("resource reservation ID conflicts with a queued resource service task"));
                }
            }
        }
    }
    Ok(ledger_payload)
}

/// `ResourceLedgerHandler.apply`, with `apply_resource_operation`'s own kernel: the world's new
/// balance of the resource, which the PyO3 crate puts in a new `resources` dict. The ledger's error
/// is `Fail::Ledger`, which the handler raises as `EventApplicationError(str(exc))`.
pub fn resource_ledger(world: &mut World, inputs: &LedgerIn<'_>, event: &EventRow, operation: &str) -> std::result::Result<Applied, Fail> {
    let payload = ledger_payload(world, inputs, event, operation)?;
    match inputs.ledger {
        Held::Absent => return Err(Fail::Ledger(LedgerFault::Text("resource ledger is not enabled for this scenario".to_owned()))),
        Held::Other(reason) => return Err(Error::unsupported(*reason).into()),
        Held::Held => {}
    }
    let source_id = optional(&event.source_id);
    let event_id = V::Str(event.event_id.clone());
    let operation_event = OperationEvent { payload: &payload, source_id: &source_id, event_id: &event_id };
    resources::apply_operation(world, &operation_event, OperationArg::Text(operation))
}

// ---- Facility outages and recoveries ----------------------------------------------------------------

/// What a facility's outage or recovery reads of the namespaces.
pub struct FacilityIn<'a> {
    /// `effect_program_v1`, for `has_active_facility_modifier`.
    pub program: &'a Held,
    pub infrastructure: &'a Held,
    /// `SHARED_OPERATIONS_KEY not in world.model_state`: no receipt.
    pub shared_absent: bool,
}

fn has_modifier(world: &World, program: &Held, facility_id: &str) -> Result<bool> {
    let row = kit::held_row::<EffectProgramRow>(world, PROGRAM, program)?;
    Ok(kit::has_active_modifier(row.as_deref(), facility_id))
}

fn active_outage(world: &World, held: &Held, facility_id: &str) -> Result<bool> {
    let infrastructure = state(world, INFRASTRUCTURE, held)?;
    kit::active_outage(infrastructure.as_deref(), facility_id)
}

/// `FacilityOutageHandler.apply` and `FacilityRecoveryHandler.apply`: `units` of a sign, and the
/// facility's recovery receipt where the world has the shared contract. The checks and their texts
/// are the Python rule's, in its order.
pub fn facility_units(world: &World, inputs: &FacilityIn<'_>, event: &EventRow, restores: bool) -> Result<Option<Made>> {
    let (what, field) = if restores { ("facility recovery", "units_restored") } else { ("facility outage", "units_out") };
    let payload = &event.payload;
    if !fields_are(payload, &["facility_id", field]) {
        return Err(Error::failed(format!("{what} payload fields do not match")));
    }
    let Some(id) = nonempty_str(item(payload, "facility_id")?).cloned() else {
        return Err(Error::failed(format!("{what} requires a facility_id")));
    };
    let Some(units) = wide_int(item(payload, field)?) else {
        return Err(Error::failed(format!("{what} {field} must be an integer")));
    };
    if matches!(units, Wide::Below) || matches!(units, Wide::Int(units) if units < 0) {
        return Err(Error::failed(format!("{what} {field} cannot be negative")));
    }
    let (previous, capacity) = {
        let rows = world.rows()?;
        match rows.facilities.get(&id) {
            None => return Err(Error::failed(format!("facility {} does not exist", py_repr(&id)))),
            Some(facility) => (facility.working_units, facility.capacity),
        }
    };
    if active_outage(world, inputs.infrastructure, &id)? {
        return Err(Error::failed(format!("{what} conflicts with an active infrastructure outage")));
    }
    if has_modifier(world, inputs.program, &id)? {
        return Err(Error::failed(format!("{what} conflicts with an active EffectProgram availability modifier")));
    }
    if units == Wide::Int(0) {
        return Err(Error::suppressed(format!("zero-unit {what} has no effect")));
    }
    // Units past 64 bits, or a sum past them, are past either bound: Python's ints don't wrap.
    let next = match units {
        Wide::Int(units) if restores => previous.checked_add(units),
        Wide::Int(units) => previous.checked_sub(units),
        _ => None,
    };
    let next = if restores {
        match next.filter(|next| *next <= capacity) {
            Some(next) => next,
            None => return Err(Error::failed("facility recovery exceeds installed capacity")),
        }
    } else {
        match next.filter(|next| *next >= 0) {
            Some(next) => next,
            None => return Err(Error::failed("facility outage exceeds currently working capacity")),
        }
    };

    // Write. As with an outage's Python rule, those being served finish.
    if let Some(facility) = world.rows_mut()?.facilities.get_mut(&id) {
        facility.working_units = next;
    }
    let infrastructure = matches!(state(world, INFRASTRUCTURE, inputs.infrastructure)?.as_deref(), Some(V::Map(_)));
    if infrastructure {
        write_record(world, INFRASTRUCTURE, &["base_working_units"], |base| put(base, &id, V::Int(next)))?;
    }
    if inputs.shared_absent {
        return Ok(None);
    }
    let sources: Vec<Arc<str>> = event.source_id.iter().filter(|source| !source.is_empty()).cloned().collect();
    let status = if restores { "restored" } else { "degraded" };
    care_receipts::facility_recovery_record(world, &id, &event.event_id, status, previous, next, &event.event_id, sources).map(Some)
}

// ---- The operational transaction --------------------------------------------------------------------

/// The end of `_apply_operational_transaction`, on the transaction's world. Without records, the
/// world is validated with the event in flight. Otherwise the event leaves the queue, its APPLIED
/// record and the records go to the log, each continuity receipt goes into `shared_operations_v1`
/// where the configs select the shared contract, and the world is validated. `shared` is whether the
/// world holds `shared_operations_v1`.
pub fn settle<C: WorldCell, H: Host>(cell: &C, host: &mut H, shared: &Held, event: &EventRow, records: &[Made], taken: &mut Vec<u64>) -> std::result::Result<(), H::Error>
where
    H::Error: From<WriteError>,
{
    if records.is_empty() {
        return host.validate_in_flight(event);
    }
    let selected = cell.with(|world| -> WriteResult<bool> {
        let world = &*world;
        let events = Events::of(world)?;
        let Some((serial, _)) = world.queue_mut()?.take_slot(&event.event_id) else {
            return Err(Error::Key(event.event_id.to_string()).into());
        };
        taken.push(serial);
        events.record(event, world.minute_index()?, EventStatus::Applied, None)?;
        for record in records {
            events.record(&record.event, record.minute, EventStatus::Applied, Some(&record.detail))?;
        }
        let configs = world.row::<ValueRow>(MODEL_CONFIGS)?;
        match &configs.0 {
            V::Map(configs) => Ok(configs.contains_key(SHARED_CONFIG)),
            _ => Err(Error::unsupported("model_configs is not a plain dict").into()),
        }
    })??;
    if selected {
        for record in records {
            if &*record.event.event_type != CONTINUITY_EVENT {
                continue;
            }
            // A receipt a kernel made: the contract's check is the core's (policy E15).
            let receipt = Receipt::of(&record.event.payload)?;
            match shared {
                Held::Absent => return Err(Error::value_error(NOT_SELECTED).into()),
                Held::Other(reason) => return Err(Error::unsupported(*reason).into()),
                Held::Held => {}
            }
            let ids = cell.with(|world| common_operations::needed_records(world, &receipt))?;
            let mut found = Vec::with_capacity(ids.len());
            for id in ids {
                let logged = host.find_record(&id)?;
                found.push((id, logged));
            }
            let found = Found(found);
            cell.with(|world| common_operations::record_shared_operation(world, &receipt, &found))??;
        }
    }
    host.validate_world()
}

// ---- Deliveries and water ---------------------------------------------------------------------------

/// `model_state.get(OPERATIONAL)`, `V::Null` for none.
fn operational_of(world: &World, held: &Held) -> Result<V> {
    state_value(world, OPERATIONAL, held)
}

/// `state[kind].get(id)` of a truthy state, `V::Null` for none: a record that is not a mapping, or
/// one without a key the rule reads, raises as Python's read does.
fn truthy_record(state: &V, kind: &str, id: &V) -> Result<V> {
    if !state.is_truthy() {
        return Ok(V::Null);
    }
    Ok(lookup(item(state, kind)?, id)?.cloned().unwrap_or(V::Null))
}

/// `DeliveryDelayHandler`'s rule, on the transaction's world: the shipment's delivery moves to
/// `to_minute`, keeping its identity, noting the delay event and taking a derived marker.
pub fn delivery_delay(world: &World, operational: &Held, event: &EventRow, taken: &mut Vec<u64>) -> Result<()> {
    let payload = &event.payload;
    if !fields_are(payload, &["shipment_id", "to_minute"]) || event.source_id.is_none() {
        return Err(Error::failed("delivery delay payload fields or source are invalid"));
    }
    let state = operational_of(world, operational)?;
    if matches!(state, V::Null) {
        return Err(Error::failed("delivery delay requires operational_events_v1 state"));
    }
    let shipment_id = item(payload, "shipment_id")?;
    let record = lookup(item(&state, "shipments")?, shipment_id)?.cloned().unwrap_or(V::Null);
    if matches!(record, V::Null) {
        return Err(Error::failed("delivery delay references an unknown shipment"));
    }
    let Some((serial, target)) = pending_named(world, item(&record, "event_id")?)? else {
        return Err(Error::failed("shipment is no longer pending for delay"));
    };
    if &*target.event_type != "resource_delivery"
        || !py_eq(get(&target.payload, "shipment_id")?, shipment_id)
        || !eq_int(item(&record, "due_minute")?, target.due_minute)
    {
        return Err(Error::failed("shipment is no longer pending for delay"));
    }
    let target_due = target.due_minute;
    let now = world.minute_index()?;
    // An int past 64 bits is at or past the Run's end, or before the delivery: the same text.
    let to_minute = match wide_int(item(payload, "to_minute")?) {
        Some(Wide::Int(to_minute)) if !(target_due <= now || to_minute <= target_due || to_minute <= now || to_minute >= world.duration_minutes()?) => to_minute,
        _ => return Err(Error::failed("shipment delay must move a not-yet-due delivery forward")),
    };
    let V::Map(mut provenance) = target.provenance.clone() else {
        return Err(Error::unsupported("a provenance is not a mapping"));
    };
    provenance.insert(text("last_delay_event_id"), V::Str(event.event_id.clone()));
    if let Some(marker) = derived_marker(event) {
        provenance.insert(text("effect_program_v1"), marker);
    }
    let V::Map(target_payload) = target.payload.clone() else {
        return Err(Error::unsupported("a payload is not a mapping"));
    };
    let replacement = kit::new_event(kit::NewEvent {
        event_id: target.event_id.clone(),
        kind: target.kind,
        due_minute: to_minute,
        priority: target.priority,
        sequence: target.sequence,
        event_type: target.event_type.clone(),
        payload: target_payload,
        source_id: target.source_id.clone(),
        causation_id: Some(event.event_id.clone()),
        provenance,
    })?;
    // `event_queue.replace`: the same identity under a new order, as a new row of the slot.
    {
        let mut queue = world.queue_mut()?;
        queue.rekey(serial, (replacement.due_minute, replacement.priority, replacement.sequence));
        queue.set_row(serial, Arc::new(replacement));
    }
    taken.push(serial);
    let shipment_key = exact_key(shipment_id)?;
    write_record(world, OPERATIONAL, &["shipments", &shipment_key], |record| put(record, "due_minute", V::Int(to_minute)))
}

/// `WaterOutageHandler`'s rule, on the transaction's world: the outage of a named target becomes
/// active.
pub fn water_outage(world: &World, operational: &Held, event: &EventRow) -> Result<()> {
    let payload = &event.payload;
    let is_zone = fields_are(payload, &["outage_id", "zone_id"]);
    if event.source_id.is_none() || (!is_zone && !fields_are(payload, &["outage_id", "facility_id"])) {
        return Err(Error::failed("water outage payload fields or source are invalid"));
    }
    let state = operational_of(world, operational)?;
    let outage_id = item(payload, "outage_id")?;
    let record = truthy_record(&state, "water_outages", outage_id)?;
    if matches!(record, V::Null) || !eq_text(item(&record, "outage_event_id")?, &event.event_id) {
        return Err(Error::failed("water outage does not match operational state"));
    }
    let target_id = item(payload, if is_zone { "zone_id" } else { "facility_id" })?;
    if !eq_text(item(&record, "target_kind")?, if is_zone { "zone" } else { "facility" }) || !py_eq(item(&record, "target_id")?, target_id) {
        return Err(Error::failed("water outage target does not match operational state"));
    }
    {
        let rows = world.rows()?;
        if is_zone {
            let found = rows.facilities.iter().any(|(_, facility)| &*facility.kind == "water_station" && eq_text(target_id, &facility.zone_id));
            if !found {
                return Err(Error::failed("water outage zone has no water_station"));
            }
        } else {
            let facility = match target_id {
                V::Str(id) => rows.facilities.get(id),
                V::List(_) | V::Map(_) => return Err(Error::unsupported("a key is not hashable")),
                _ => None,
            };
            if !facility.is_some_and(|facility| &*facility.kind == "water_station") {
                return Err(Error::failed("water outage must target a water_station"));
            }
        }
    }
    if item(&record, "active")?.is_truthy() {
        return Err(Error::suppressed("water outage is already active"));
    }
    let key = exact_key(outage_id)?;
    write_record(world, OPERATIONAL, &["water_outages", &key], |record| put(record, "active", V::Bool(true)))
}

/// `WaterRestorationHandler`'s rule, on the transaction's world.
pub fn water_restoration(world: &World, operational: &Held, event: &EventRow) -> Result<()> {
    let payload = &event.payload;
    if event.source_id.is_none() || !fields_are(payload, &["outage_id"]) {
        return Err(Error::failed("water restoration payload fields or source are invalid"));
    }
    let state = operational_of(world, operational)?;
    let outage_id = item(payload, "outage_id")?;
    let record = truthy_record(&state, "water_outages", outage_id)?;
    if matches!(record, V::Null) || !eq_text(item(&record, "restore_event_id")?, &event.event_id) {
        return Err(Error::failed("water restoration does not match its outage"));
    }
    if !item(&record, "active")?.is_truthy() {
        return Err(Error::suppressed("water outage is not active"));
    }
    let key = exact_key(outage_id)?;
    write_record(world, OPERATIONAL, &["water_outages", &key], |record| put(record, "active", V::Bool(false)))
}

// ---- Staff unavailable and restored -----------------------------------------------------------------

/// Why the PyO3 crate could not make the resource service's manager: its own refusal, said again
/// where the rule makes it, or another error, which the glue raises in place of `SERVICE_UNREAD`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unread {
    Refused(Cow<'static, str>),
    Raised,
}

/// What `StaffUnavailabilityHandler` reads of the Python world.
pub struct StaffIn<'a> {
    pub operational: &'a Held,
    /// What the staff operations read (`staff_operations_v1`, `movement_v1`, the floor plan).
    pub ops: &'a staff_ops::Inputs<'a>,
    /// `ResourceServiceManager(world)`, as the PyO3 crate made it.
    pub service: std::result::Result<&'a Manager<'a>, Unread>,
    /// `model_configs.get("staff", {}).get("support_cooldown_minutes", 30)`, None where its read
    /// raised.
    pub cooldown: Option<i64>,
    /// Whether `world.metric_state` is its slot's tree.
    pub metric_state_held: bool,
}

/// What a staff member's unavailability did besides its writes: the receipt of a support task it
/// interrupted, and the new balance of a resource whose service it stopped.
#[derive(Default)]
pub struct Unavailable {
    pub records: Vec<Made>,
    pub applied: Option<Applied>,
}

/// A new event of an operational rule, with the checks of its identity and of the counter.
fn schedule_new(events: &Events<'_>, world: &World, row: EventRow, sequence: i64, preflight: &str) -> WriteResult<()> {
    identity_free(world, &row.event_id, sequence)?;
    if events.allocate_sequence()? != sequence {
        return Err(Error::failed(format!("event sequence changed during {preflight} preflight")).into());
    }
    events.schedule(row)
}

/// The `reservation_id` of each pending `resource_reserve` event, in the queue's order.
fn pending_reserves(world: &World) -> Result<Vec<Pending>> {
    let mut found = Vec::new();
    for row in pending_rows(world)? {
        if &*row.event_type != "resource_reserve" {
            continue;
        }
        found.push(match kit::mapping_get(&row.payload, "reservation_id")? {
            Some(V::Str(id)) => Pending::Id(id.clone()),
            Some(_) => Pending::Other,
            None => Pending::Missing,
        });
    }
    Ok(found)
}

/// `ResourceServiceManager(world).interrupt_for_staff(staff_id, causation_event=event)`, counted as
/// the PyO3 crate's entry counts it.
fn interrupt_service(world: &mut World, inputs: &StaffIn<'_>, staff_id: &str, event: &EventRow) -> service::Outcome<Option<Applied>> {
    const LABEL: &str = "interrupt_for_staff";
    let result = match &inputs.service {
        Ok(manager) => pending_reserves(world).map_err(Failure::from).and_then(|pending| service::interrupt(world, manager, &pending, staff_id, Some(&event.event_id))),
        Err(Unread::Refused(reason)) => Err(Error::unsupported(reason.clone()).into()),
        Err(Unread::Raised) => return Err(Error::unsupported(SERVICE_UNREAD).into()),
    };
    match &result {
        Ok(_) => COUNTS.ran(LABEL),
        Err(Failure::Write(WriteError::Error(Error::Unsupported(reason)))) => COUNTS.refused(LABEL, reason),
        Err(_) => {}
    }
    result
}

/// `StaffUnavailabilityHandler`'s rule, on the transaction's world: the restore event, the stop of
/// whatever the member was doing (a resource service, a support task, an autonomous operation), and
/// the member's status.
pub fn staff_unavailable(world: &mut World, inputs: &StaffIn<'_>, event: &EventRow, taken: &mut Vec<u64>) -> service::Outcome<Unavailable> {
    let mut done = Unavailable::default();
    let payload = &event.payload;
    if event.source_id.is_none() || !fields_are(payload, &["staff_id", "until_minute"]) {
        return Err(Error::failed("staff unavailability payload fields or source are invalid").into());
    }
    let operational = operational_of(world, inputs.operational)?;
    if matches!(operational, V::Null) {
        return Err(Error::failed("staff unavailability requires operational event state").into());
    }
    let staff_id = kit::exact_str(item(payload, "staff_id")?)?.clone();
    if !world.rows()?.staff.contains_key(&staff_id) {
        return Err(Error::failed("staff unavailability references unknown staff").into());
    }
    let duration = world.duration_minutes()?;
    let until = match wide_int(item(payload, "until_minute")?) {
        Some(Wide::Int(until)) if until > world.minute_index()? && until <= duration => until,
        _ => return Err(Error::failed("staff unavailable until_minute is invalid").into()),
    };
    let status = item(item(&operational, "staff_status")?, &staff_id)?;
    if eq_text(item(status, "unavailability_event_id")?, &event.event_id) {
        return Err(Error::suppressed("staff unavailability was already applied").into());
    }
    let mut restore_event_id: Option<String> = None;
    if until < duration {
        let sequence = world.id_counter("event")?.unwrap_or(0);
        let id = format!("staff-restored-{sequence}");
        let mut restore_payload = IndexMap::with_capacity(2);
        restore_payload.insert(text("staff_id"), V::Str(staff_id.clone()));
        restore_payload.insert(text("unavailability_event_id"), V::Str(event.event_id.clone()));
        let mut provenance = kit::assumption(str_v(OPERATIONAL_SOURCE), OPERATIONAL_POLICY);
        if let Some(marker) = derived_marker(event) {
            provenance.insert(text("effect_program_v1"), marker);
        }
        let restore = kit::new_event(kit::NewEvent {
            event_id: text(&id),
            kind: EventKind::Completion,
            due_minute: until,
            priority: 10,
            sequence,
            event_type: text("staff_restored"),
            payload: restore_payload,
            source_id: event.source_id.clone(),
            causation_id: Some(event.event_id.clone()),
            provenance,
        })?;
        let events = Events::of(world)?;
        schedule_new(&events, world, restore, sequence, "staff")?;
        restore_event_id = Some(id);
    }
    let (support_task, support_count, resource_tasks) = {
        let rows = world.rows()?;
        let mut support = None;
        let (mut support_count, mut resource_tasks) = (0_usize, 0_usize);
        for (_, task) in rows.active_tasks.iter() {
            if task.staff_id.as_deref() != Some(&*staff_id) {
                continue;
            }
            match &*task.kind {
                "staff_support" => {
                    support_count += 1;
                    if support.is_none() {
                        support = Some(task.id.clone());
                    }
                }
                "resource_service_water" | "resource_service_food" => resource_tasks += 1,
                _ => {}
            }
        }
        (support, support_count, resource_tasks)
    };
    let autonomous = match kit::held_row::<StaffOperationsRow>(world, STAFF_OPERATIONS, inputs.ops.staff)? {
        Some(row) => row.staff.get(&*staff_id).is_some_and(|record| record.phase != Phase::Idle),
        None => false,
    };
    if support_count + resource_tasks + usize::from(autonomous) > 1 {
        return Err(Error::failed("staff owns multiple active service tasks").into());
    }
    if resource_tasks > 0 {
        done.applied = interrupt_service(world, inputs, &staff_id, event)?;
    } else if let Some(task_id) = support_task {
        done.records.push(interrupt_support_task(world, inputs, &task_id, &staff_id, event, taken)?);
    } else if autonomous {
        let stopping = Stopping { event_id: &event.event_id, due_minute: event.due_minute };
        staff_ops::interrupt(world, inputs.ops, &staff_id, &stopping, None, taken)?;
    }
    write_record(world, OPERATIONAL, &["staff_status", &staff_id], |status| {
        put(status, "available", V::Bool(false))?;
        put(status, "unavailability_event_id", V::Str(event.event_id.clone()))?;
        put(status, "until_minute", V::Int(until))?;
        put(status, "restore_event_id", restore_event_id.map_or(V::Null, |id| V::Str(text(&id))))
    })?;
    Ok(done)
}

/// `_interrupt_support_task`, on the transaction's world: its continuity receipt.
fn interrupt_support_task(world: &World, inputs: &StaffIn<'_>, task_id: &Arc<str>, staff_id: &Arc<str>, causation: &EventRow, taken: &mut Vec<u64>) -> WriteResult<Made> {
    let inconsistent = || Error::failed("interrupted support task has inconsistent ownership");
    let (request, agent_id, task_due) = {
        let rows = world.rows()?;
        let task = rows.active_tasks.get(task_id).ok_or_else(|| Error::unsupported("the task is gone"))?;
        let request = rows
            .support_requests
            .iter()
            .find(|(_, request)| request.status == RequestStatus::Assigned && request.task_id.as_deref() == Some(&**task_id))
            .map(|(_, request)| request.clone());
        let Some(request) = request else {
            return Err(inconsistent().into());
        };
        let task_agent = task.agent_id.clone().unwrap_or_else(|| text(""));
        let Some(agent) = rows.agents.get(&task_agent) else {
            return Err(inconsistent().into());
        };
        (request, agent.profile.id.clone(), task.due_minute)
    };
    let pending = pending_rows(world)?;
    let task_value = V::Str(task_id.clone());
    let completions: Vec<&Arc<EventRow>> = pending
        .iter()
        .filter(|item| &*item.event_type == "staff_support_complete" && py_eq(kit::payload(item, "task_id").unwrap_or(&NULL), &task_value))
        .collect();
    let owns = {
        let rows = world.rows()?;
        let staff = rows.staff.get(staff_id).ok_or_else(|| Error::unsupported("the staff member is gone"))?;
        staff.active_task_id.as_deref() == Some(&**task_id) && staff.work_queue.len() == 1 && staff.work_queue[0] == *task_id
    };
    if !owns || completions.len() != 1 || completions[0].due_minute != task_due {
        return Err(inconsistent().into());
    }
    let request_id = request.id.clone();
    let request_value = V::Str(request_id.clone());
    let timeout = request.timeout_minute;
    let timeouts: Vec<&Arc<EventRow>> = pending
        .iter()
        .filter(|item| &*item.event_type == "support_request_timeout" && py_eq(kit::payload(item, "request_id").unwrap_or(&NULL), &request_value))
        .collect();
    if timeouts.len() > 1 || timeouts.first().is_some_and(|item| item.due_minute != timeout) {
        return Err(Error::failed("support timeout state is inconsistent").into());
    }
    let now = world.minute_index()?;
    let events = Events::of(world)?;
    if timeout > now && timeouts.is_empty() {
        let sequence = world.id_counter("event")?.unwrap_or(0);
        let id = format!("support-timeout-rescheduled-{sequence}");
        let mut payload = IndexMap::with_capacity(2);
        payload.insert(text("request_id"), request_value.clone());
        payload.insert(text("agent_id"), V::Str(request.agent_id.clone()));
        let mut provenance = kit::assumption(str_v(SUPPORT_SOURCE), STAFF_SUPPORT_POLICY);
        provenance.insert(text("rescheduled_after_staff_unavailable"), V::Bool(true));
        let timeout_event = kit::new_event(kit::NewEvent {
            event_id: text(&id),
            kind: EventKind::Completion,
            due_minute: timeout,
            priority: 20,
            sequence,
            event_type: text("support_request_timeout"),
            payload,
            source_id: Some(request_id.clone()),
            causation_id: Some(causation.event_id.clone()),
            provenance,
        })?;
        schedule_new(&events, world, timeout_event, sequence, "support")?;
    }

    let trips: Vec<(Arc<str>, i64)> = {
        let rows = world.rows()?;
        rows.active_tasks
            .iter()
            .filter(|(_, task)| &*task.kind == "staff_support_travel" && task.agent_id.as_deref() == Some(&*agent_id))
            .map(|(_, task)| (task.id.clone(), task.due_minute))
            .collect()
    };
    if trips.len() > 1 {
        return Err(Error::failed("resident has duplicate trips to assigned support").into());
    }
    if let Some((trip_id, trip_due)) = trips.first() {
        let trip_value = V::Str(trip_id.clone());
        let walks: Vec<Arc<EventRow>> = pending_rows(world)?
            .into_iter()
            .filter(|item| {
                &*item.event_type == "agent_travel_complete"
                    && py_eq(kit::payload(item, "task_id").unwrap_or(&NULL), &trip_value)
                    && eq_text(kit::payload(item, "purpose").unwrap_or(&NULL), "staff_support_travel")
                    && py_eq(kit::payload(item, "request_id").unwrap_or(&NULL), &request_value)
            })
            .collect();
        if walks.len() != 1 || walks[0].due_minute != *trip_due {
            return Err(Error::failed("resident support trip has inconsistent travel ownership").into());
        }
        let settled = events.settle(&walks[0], now, EventStatus::Suppressed, Some("assigned staff became unavailable before resident arrival"))?;
        taken.extend(settled.taken);
        world.rows_mut()?.active_tasks.remove_row(trip_id);
        let zone = match kit::held_row::<MovementRow>(world, MOVEMENT, inputs.ops.movement)? {
            Some(row) => row.positions.get(&*agent_id).map(|position| position.zone_id.clone()),
            None => None,
        };
        if let Some(zone) = zone {
            let mut rows = world.rows_mut()?;
            if rows.zones.contains_key(&zone) {
                if let Some(agent) = rows.agents.get_mut(&agent_id) {
                    agent.zone_id = zone;
                }
            }
        }
        match inputs.ops.movement {
            Held::Absent => {}
            Held::Other(reason) => return Err(Error::unsupported(*reason).into()),
            Held::Held => {
                let geo = crate::rules::travel::plan(inputs.ops.geo)?;
                movement::finish(world, geo, &agent_id)?;
            }
        }
    }

    let previous = {
        let mut rows = world.rows_mut()?;
        rows.active_tasks.remove_row(task_id);
        let staff = rows.staff.get_mut(staff_id).ok_or_else(|| Error::unsupported("the staff member is gone"))?;
        staff.work_queue.clear();
        staff.active_task_id = None;
        staff.busy_until = 0;
        staff.id.clone()
    };
    {
        let mut row = world.row_mut::<ValueRow>(OPERATIONAL)?;
        match at_mut(&mut row.0, &["cancelled_staff_task_ids"])? {
            V::List(ids) => ids.push(V::Str(task_id.clone())),
            _ => return Err(Error::unsupported("the cancelled task IDs are not a list").into()),
        }
    }
    let requeued = timeout > now;
    let cooldown_until = if requeued {
        None
    } else {
        let cooldown = inputs.cooldown.ok_or_else(|| Error::unsupported(COOLDOWN_UNREAD))?;
        kit::check_metric_state(world, inputs.metric_state_held)?;
        Some(now.checked_add(cooldown).ok_or_else(|| Error::unsupported("a minute past 64 bits"))?)
    };
    {
        let mut rows = world.rows_mut()?;
        let request_row = rows.support_requests.get_mut(&request_id).ok_or_else(|| Error::unsupported("the request is gone"))?;
        request_row.assigned_staff_id = None;
        request_row.task_id = None;
        match cooldown_until {
            None => request_row.status = RequestStatus::Pending,
            Some(until) => {
                request_row.status = RequestStatus::Unresolved;
                request_row.resolved_minute = Some(now);
                request_row.cooldown_until = until;
            }
        }
        let agent = rows.agents.get_mut(&agent_id).ok_or_else(|| Error::unsupported("the agent is gone"))?;
        agent.target_id = None;
        match cooldown_until {
            None => {
                agent.activity = Activity::SeekingHelp;
                agent.support_request_id = Some(request_id.clone());
            }
            Some(until) => {
                agent.activity = Activity::Idle;
                agent.support_request_id = None;
                agent.support_cooldown_until = until;
            }
        }
    }
    if !requeued {
        kit::add_metric(world, "support_unresolved_count")?;
    }
    let cause_type = if &*causation.event_type == "staff_shift_change" { "staff_shift_change" } else { "staff_unavailable" };
    Ok(care_receipts::continuity_record(
        world,
        &request_id,
        &request.agent_id,
        task_id,
        &previous,
        if requeued { "requeued" } else { "unresolved" },
        &causation.event_id,
        cause_type,
    )?)
}

/// `StaffRestorationHandler`'s rule, on the transaction's world.
pub fn staff_restoration(world: &World, operational: &Held, event: &EventRow) -> Result<()> {
    let payload = &event.payload;
    if event.source_id.is_none() || !fields_are(payload, &["staff_id", "unavailability_event_id"]) {
        return Err(Error::failed("staff restoration payload fields or source are invalid"));
    }
    let state = operational_of(world, operational)?;
    let staff_id = item(payload, "staff_id")?;
    let record = truthy_record(&state, "staff_status", staff_id)?;
    if matches!(record, V::Null) {
        return Err(Error::failed("staff restoration references unknown staff"));
    }
    if !py_eq(item(&record, "unavailability_event_id")?, item(payload, "unavailability_event_id")?) {
        return Err(Error::suppressed("staff restoration belongs to an older unavailability"));
    }
    if !eq_text(item(&record, "restore_event_id")?, &event.event_id) {
        return Err(Error::failed("staff restoration event ID does not match its token"));
    }
    if item(&record, "available")?.is_truthy() {
        return Err(Error::suppressed("staff is already available"));
    }
    let key = exact_key(staff_id)?;
    write_record(world, OPERATIONAL, &["staff_status", &key], |record| put(record, "available", V::Bool(true)))
}

// ---- EffectProgramPopulationHandler -----------------------------------------------------------------

/// A population primitive's operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PopulationOp {
    Add,
    Depart,
    Presence,
}

const BASE_FIELDS: [&str; 5] = ["program_id", "effect_id", "op", "zone_id", "count"];

fn with_fields(extra: &[&'static str]) -> Vec<&'static str> {
    let mut fields = BASE_FIELDS.to_vec();
    fields.extend_from_slice(extra);
    fields
}

fn population_fields_fit(payload: &V, op: PopulationOp) -> bool {
    let shapes: Vec<Vec<&'static str>> = match op {
        PopulationOp::Add => vec![
            with_fields(&["household_size", "mobility", "language", "admission_zone_ids", "approach_route_id", "person_context_overrides"]),
            with_fields(&["household_size", "mobility", "language", "admission_zone_ids", "approach_route_id"]),
            with_fields(&["household_size", "mobility", "language", "admission_zone_ids"]),
            with_fields(&["household_size", "mobility", "language"]),
        ],
        PopulationOp::Depart => vec![with_fields(&["include_initial"]), with_fields(&[])],
        PopulationOp::Presence => vec![vec!["program_id", "effect_id", "op", "command"]],
    };
    shapes.iter().any(|shape| fields_are(payload, shape))
}

/// `EffectProgramPopulationHandler.apply`'s checks of a primitive against its approved program
/// (`effect_program_v1`, read only): its operation. The rules themselves are the population lanes'.
pub fn population_checks(world: &World, program: &Held, event: &EventRow) -> Result<PopulationOp> {
    let payload = &event.payload;
    let op_value = get(payload, "op")?;
    let op = match op_value {
        V::Str(op) if &**op == "add_evacuees" => PopulationOp::Add,
        V::Str(op) if &**op == "depart_evacuees" => PopulationOp::Depart,
        V::Str(op) if &**op == "population_presence" => PopulationOp::Presence,
        _ => return Err(Error::failed("population primitive operation is invalid")),
    };
    if !population_fields_fit(payload, op) {
        return Err(Error::failed("population primitive payload fields do not match"));
    }
    let not_approved = || Err(Error::failed("population primitive is not in the approved program"));
    let Some(state) = kit::held_row::<EffectProgramRow>(world, PROGRAM, program)? else {
        return not_approved();
    };
    let program = match get(payload, "program_id")? {
        V::Str(id) => state.programs.get(&**id),
        V::List(_) | V::Map(_) => return Err(Error::unsupported("a key is not hashable")),
        _ => None,
    };
    let Some(program) = program else {
        return not_approved();
    };
    let effect_id = get(payload, "effect_id")?;
    let listed = program.effect_ids.iter().any(|id| eq_text(effect_id, id));
    let operator = match effect_id {
        V::Str(id) => program.operators.get(&**id),
        _ => None,
    };
    if !listed
        || !operator.is_some_and(|operator| eq_text(op_value, operator))
        || !program.scheduled_event_ids.iter().any(|id| *id == event.event_id)
        || (op != PopulationOp::Presence && !contains(&world.rows()?.zones, get(payload, "zone_id")?)?)
    {
        return not_approved();
    }
    Ok(op)
}

/// `ResourceServiceCancellationHandler.apply`'s check before the manager's `cancel`.
pub fn service_cancellation(world: &World) -> Result<()> {
    if world.schema_version()? != 6 {
        return Err(Error::failed("resource service cancellation requires WorldState v6"));
    }
    Ok(())
}

// ---- InfrastructureEventHandler ---------------------------------------------------------------------

/// The kind of an infrastructure event, by its type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Infra {
    StartOutage,
    RestoreOutage,
    StartStressor,
    EndStressor,
    InterruptRoute,
    RestoreRoute,
}

impl Infra {
    /// The kind of this event type and its label, or the handler's failure for any other type.
    pub fn of(event_type: &str) -> Result<(Infra, &'static str)> {
        Ok(match event_type {
            "power_outage" | "equipment_failure" => (Infra::StartOutage, "infrastructure.start_outage"),
            "power_restored" | "equipment_repaired" => (Infra::RestoreOutage, "infrastructure.restore_outage"),
            "extreme_heat" | "extreme_cold" => (Infra::StartStressor, "infrastructure.start_stressor"),
            "environmental_stressor_end" => (Infra::EndStressor, "infrastructure.end_stressor"),
            "supply_route_interruption" => (Infra::InterruptRoute, "infrastructure.interrupt_route"),
            "supply_route_restored" => (Infra::RestoreRoute, "infrastructure.restore_route"),
            _ => return Err(Error::failed(format!("unsupported infrastructure event type {}", py_repr(event_type)))),
        })
    }
}

/// What the infrastructure's rules read of the namespaces.
pub struct InfraIn<'a> {
    pub infrastructure: &'a Held,
    pub operational: &'a Held,
    pub program: &'a Held,
}

/// `ScenarioInfrastructureSpec.model_validate(model_configs["infrastructure"])` through the host
/// (policy E15), read once per event, where the rule first reads it.
struct Spec(Option<Arc<Infrastructure>>);

impl Spec {
    fn get<C: WorldCell, H: Host>(&mut self, cell: &C, host: &mut H) -> std::result::Result<Arc<Infrastructure>, H::Error> {
        if let Some(spec) = &self.0 {
            return Ok(spec.clone());
        }
        let raw = cell.with(|world| -> Result<V> {
            let configs = world.row::<ValueRow>(MODEL_CONFIGS)?;
            Ok(item(&configs.0, "infrastructure")?.clone())
        })??;
        let dumped = host.contract(&Contract::Infrastructure, &raw)?;
        let spec = Arc::new(config_read(Infrastructure::from_v(&dumped))?);
        self.0 = Some(spec.clone());
        Ok(spec)
    }
}

/// `_infrastructure_state`: the namespace's value, which must be a dict.
fn infrastructure_state(world: &World, inputs: &InfraIn<'_>, event: &EventRow) -> Result<V> {
    let state = state_value(world, INFRASTRUCTURE, inputs.infrastructure)?;
    if !matches!(state, V::Map(_)) {
        return Err(Error::failed(format!("{} requires the opted-in infrastructure model", event.event_type)));
    }
    Ok(state)
}

fn facility_ids(world: &World) -> Result<BTreeSet<String>> {
    Ok(world.rows()?.facilities.iter().map(|(id, _)| id.to_string()).collect())
}

/// `effective_facility_capacities`.
fn capacities_of(world: &World, state: &V, config: &Infrastructure) -> Result<BTreeMap<String, i64>> {
    let facilities = facility_ids(world)?;
    let mut forced: BTreeSet<String> = BTreeSet::new();
    let mut lost: BTreeMap<String, i64> = BTreeMap::new();
    let V::Map(outages) = item(state, "outages")? else {
        return Err(Error::unsupported("the outages are not a plain dict"));
    };
    for outage in outages.values() {
        if !item(outage, "active")?.is_truthy() {
            continue;
        }
        if eq_text(item(outage, "event_type")?, "power_outage") {
            let V::List(affected) = item(outage, "affected_facility_ids")? else {
                return Err(Error::unsupported("the affected facilities are not a list"));
            };
            for id in affected {
                forced.insert(kit::exact_str(id)?.to_string());
            }
        } else {
            let target = kit::exact_str(item(outage, "target_id")?)?;
            let units = kit::exact_int(item(outage, "units_out")?)?;
            let total = lost.entry(target.to_string()).or_insert(0);
            *total = total.checked_add(units).ok_or_else(|| Error::unsupported("units past 64 bits"))?;
        }
    }
    let V::Map(bases) = item(state, "base_working_units")? else {
        return Err(Error::unsupported("the base working units are not a plain dict"));
    };
    let mut capacity: Vec<(String, i64)> = Vec::with_capacity(bases.len());
    for (id, base) in bases {
        let base = kit::exact_int(base)?;
        let units = lost.get(&**id).copied().unwrap_or(0);
        if units > base {
            return Err(Error::failed("combined infrastructure failures exceed installed units"));
        }
        capacity.push((id.to_string(), base - units));
    }
    for (id, units) in &capacity {
        if *units == 0 {
            forced.extend(config.dependent_facilities(id, &facilities));
        }
    }
    Ok(capacity
        .into_iter()
        .map(|(id, units)| {
            let value = if forced.contains(&id) { 0 } else { units };
            (id, value)
        })
        .collect())
}

/// `state[kind].get(id)`, `V::Null` for none.
fn record_of(state: &V, kind: &str, id: &V) -> Result<V> {
    Ok(lookup(item(state, kind)?, id)?.cloned().unwrap_or(V::Null))
}

/// `_recalculate_infrastructure_capacity`.
fn recalculate_capacity<C: WorldCell, H: Host>(cell: &C, host: &mut H, spec: &mut Spec, inputs: &InfraIn<'_>, cause: &EventRow) -> std::result::Result<(), H::Error> {
    let ids = cell.with(|world| -> Result<BTreeSet<String>> {
        let state = infrastructure_state(world, inputs, cause)?;
        let changed = record_of(&state, "outages", get(&cause.payload, "outage_id")?)?;
        if matches!(changed, V::Null) {
            return Err(Error::failed("infrastructure capacity update references an unknown outage"));
        }
        let V::List(affected) = item(&changed, "affected_facility_ids")? else {
            return Err(Error::unsupported("the affected facilities are not a list"));
        };
        affected.iter().map(|id| Ok(kit::exact_str(id)?.to_string())).collect()
    })??;
    let config = spec.get(cell, host)?;
    cell.with(|world| -> Result<()> {
        let state = infrastructure_state(world, inputs, cause)?;
        let capacities = capacities_of(world, &state, &config)?;
        let mut rows = world.rows_mut()?;
        for id in ids {
            let Some(units) = capacities.get(&id) else {
                return Err(Error::unsupported("a facility has no base units"));
            };
            let facility = rows.facilities.get_mut(&id).ok_or_else(|| Error::Key(id.clone()))?;
            facility.working_units = *units;
        }
        Ok(())
    })??;
    Ok(())
}

/// `_schedule_infrastructure_completion`: the new event's ID, or None at or past the Run's end.
fn schedule_completion<C: WorldCell, H: Host>(
    cell: &C,
    host: &mut H,
    spec: &mut Spec,
    cause: &EventRow,
    event_type: &str,
    payload: IndexMap<Arc<str>, V>,
    due_minute: i64,
) -> std::result::Result<Option<String>, H::Error>
where
    H::Error: From<WriteError>,
{
    let sequence = cell.with(|world| -> Result<Option<i64>> {
        if due_minute >= world.duration_minutes()? {
            return Ok(None);
        }
        Events::of(world)?.allocate_sequence().map(Some)
    })??;
    let Some(sequence) = sequence else {
        return Ok(None);
    };
    let id = format!("{}-{sequence}", event_type.replace('_', "-"));
    let source = spec.get(cell, host)?.source_id.clone();
    let event = kit::new_event(kit::NewEvent {
        event_id: text(&id),
        kind: EventKind::Completion,
        due_minute,
        priority: 10,
        sequence,
        event_type: text(event_type),
        payload,
        source_id: cause.source_id.clone(),
        causation_id: Some(cause.event_id.clone()),
        provenance: kit::assumption(V::Str(source), INFRASTRUCTURE_POLICY),
    })?;
    cell.with(|world| -> WriteResult<()> {
        let world = &*world;
        identity_free(world, &id, sequence)?;
        Events::of(world)?.schedule(event)
    })??;
    Ok(Some(id))
}

/// The payload's `until_minute` if it is an int (not a bool) after the event's minute and within the
/// Run; None where the Python rule's check says it is invalid.
fn until_of(world: &World, event: &EventRow) -> Result<Option<i64>> {
    // An int past 64 bits is before the event or past the Run.
    let Some(Wide::Int(until)) = wide_int(get(&event.payload, "until_minute")?) else {
        return Ok(None);
    };
    if event.due_minute >= until || until > world.duration_minutes()? {
        return Ok(None);
    }
    Ok(Some(until))
}

/// Write the record of a kind of the infrastructure's state, by the ID the rule found it by.
fn write_infrastructure(world: &World, kind: &str, id: &V, f: impl FnOnce(&mut V) -> Result<()>) -> Result<()> {
    let key = exact_key(id)?;
    write_record(world, INFRASTRUCTURE, &[kind, &key], f)
}

fn id_value(id: Option<String>) -> V {
    id.map_or(V::Null, |id| V::Str(text(&id)))
}

fn start_outage<C: WorldCell, H: Host>(cell: &C, host: &mut H, spec: &mut Spec, inputs: &InfraIn<'_>, event: &EventRow) -> std::result::Result<(), H::Error>
where
    H::Error: From<WriteError>,
{
    let power = &*event.event_type == "power_outage";
    let outage_id = get(&event.payload, "outage_id")?.clone();
    let (until, target) = cell.with(|world| -> Result<(i64, V)> {
        let state = infrastructure_state(world, inputs, event)?;
        let payload = &event.payload;
        let record = record_of(&state, "outages", &outage_id)?;
        if matches!(record, V::Null) || !eq_text(item(&record, "start_event_id")?, &event.event_id) {
            return Err(Error::failed("infrastructure outage does not match scenario state"));
        }
        if !eq_text(item(&record, "event_type")?, &event.event_type) {
            return Err(Error::failed("infrastructure outage event type does not match scenario state"));
        }
        if item(&record, "active")?.is_truthy() || event.source_id.is_none() {
            return Err(Error::failed("infrastructure outage is already active or lacks source"));
        }
        let target = get(payload, if power { "component_id" } else { "facility_id" })?.clone();
        let right_fields = if power {
            fields_are(payload, &["outage_id", "component_id", "until_minute"])
        } else {
            fields_are(payload, &["outage_id", "facility_id", "units_out", "until_minute"])
        };
        let until = if right_fields { until_of(world, event)? } else { None };
        let Some(until) = until else {
            return Err(Error::failed("infrastructure outage payload or duration is invalid"));
        };
        Ok((until, target))
    })??;
    let config = spec.get(cell, host)?;
    let completion = cell.with(|world| -> Result<IndexMap<Arc<str>, V>> {
        let affected = config.dependent_facilities(kit::exact_str(&target)?, &facility_ids(world)?);
        let state = infrastructure_state(world, inputs, event)?;
        let record = record_of(&state, "outages", &outage_id)?;
        let listed = V::List(affected.iter().map(|id| str_v(id)).collect());
        if affected.is_empty() || !py_eq(item(&record, "affected_facility_ids")?, &listed) {
            return Err(Error::failed("infrastructure outage has no valid affected facilities"));
        }
        for id in &affected {
            if has_modifier(world, inputs.program, id)? {
                return Err(Error::failed("infrastructure outage conflicts with an active facility modifier"));
            }
        }
        let units_out = if power { V::Null } else { item(&event.payload, "units_out")?.clone() };
        write_infrastructure(world, "outages", &outage_id, |record| {
            put(record, "active", V::Bool(true))?;
            put(record, "until_minute", V::Int(until))?;
            put(record, "units_out", units_out)
        })?;
        let mut completion = IndexMap::with_capacity(1);
        completion.insert(text("outage_id"), item(&event.payload, "outage_id")?.clone());
        Ok(completion)
    })??;
    let restore = schedule_completion(cell, host, spec, event, if power { "power_restored" } else { "equipment_repaired" }, completion, until)?;
    cell.with(|world| write_infrastructure(world, "outages", &outage_id, |record| put(record, "restore_event_id", id_value(restore))))??;
    recalculate_capacity(cell, host, spec, inputs, event)
}

fn restore_outage<C: WorldCell, H: Host>(cell: &C, host: &mut H, spec: &mut Spec, inputs: &InfraIn<'_>, event: &EventRow) -> std::result::Result<(), H::Error> {
    cell.with(|world| -> Result<()> {
        let state = infrastructure_state(world, inputs, event)?;
        let payload = &event.payload;
        let outage_id = get(payload, "outage_id")?;
        let record = record_of(&state, "outages", outage_id)?;
        let matches = fields_are(payload, &["outage_id"])
            && !matches!(record, V::Null)
            && item(&record, "active")?.is_truthy()
            && eq_text(item(&record, "restore_event_id")?, &event.event_id)
            && py_eq(item(&record, "start_event_id")?, &optional(&event.causation_id))
            && event.source_id.is_some();
        if !matches {
            return Err(Error::failed("infrastructure restoration does not match an active outage"));
        }
        let expected = if eq_text(item(&record, "event_type")?, "power_outage") { "power_restored" } else { "equipment_repaired" };
        if &*event.event_type != expected {
            return Err(Error::failed("infrastructure restoration type does not match its cause"));
        }
        write_infrastructure(world, "outages", outage_id, |record| put(record, "active", V::Bool(false)))
    })??;
    recalculate_capacity(cell, host, spec, inputs, event)
}

fn start_stressor<C: WorldCell, H: Host>(cell: &C, host: &mut H, spec: &mut Spec, inputs: &InfraIn<'_>, event: &EventRow) -> std::result::Result<(), H::Error>
where
    H::Error: From<WriteError>,
{
    let stressor_id = get(&event.payload, "stressor_id")?.clone();
    let (until, completion) = cell.with(|world| -> Result<(i64, IndexMap<Arc<str>, V>)> {
        let state = infrastructure_state(world, inputs, event)?;
        let payload = &event.payload;
        let stressors = item(&state, "environmental_stressors")?;
        let record = lookup(stressors, &stressor_id)?.cloned().unwrap_or(V::Null);
        let valid = fields_are(payload, &["stressor_id", "zone_id", "until_minute"])
            && !matches!(record, V::Null)
            && eq_text(item(&record, "start_event_id")?, &event.event_id)
            && !item(&record, "active")?.is_truthy()
            && contains(&world.rows()?.zones, get(payload, "zone_id")?)?
            && event.source_id.is_some();
        let until = if valid { until_of(world, event)? } else { None };
        let Some(until) = until else {
            return Err(Error::failed("environmental stressor payload or duration is invalid"));
        };
        let zone = item(&record, "zone_id")?.clone();
        let V::Map(all) = stressors else {
            return Err(Error::unsupported("the stressors are not a plain dict"));
        };
        let mut active = 0;
        for other in all.values() {
            if item(other, "active")?.is_truthy() && py_eq(item(other, "zone_id")?, &zone) {
                active += 1;
            }
        }
        if active >= 16 {
            return Err(Error::failed("zone environmental stressor limit was reached"));
        }
        write_infrastructure(world, "environmental_stressors", &stressor_id, |record| put(record, "active", V::Bool(true)))?;
        let mut completion = IndexMap::with_capacity(2);
        completion.insert(text("stressor_id"), stressor_id.clone());
        completion.insert(text("zone_id"), zone);
        Ok((until, completion))
    })??;
    let end = schedule_completion(cell, host, spec, event, "environmental_stressor_end", completion, until)?;
    cell.with(|world| {
        write_infrastructure(world, "environmental_stressors", &stressor_id, |record| {
            put(record, "end_event_id", id_value(end))?;
            put(record, "until_minute", V::Int(until))
        })
    })??;
    Ok(())
}

fn end_stressor(world: &World, inputs: &InfraIn<'_>, event: &EventRow) -> Result<()> {
    let state = infrastructure_state(world, inputs, event)?;
    let payload = &event.payload;
    let stressor_id = get(payload, "stressor_id")?;
    let record = record_of(&state, "environmental_stressors", stressor_id)?;
    let matches = fields_are(payload, &["stressor_id", "zone_id"])
        && !matches!(record, V::Null)
        && item(&record, "active")?.is_truthy()
        && eq_text(item(&record, "end_event_id")?, &event.event_id)
        && py_eq(item(&record, "start_event_id")?, &optional(&event.causation_id))
        && py_eq(item(&record, "zone_id")?, get(payload, "zone_id")?)
        && event.source_id.is_some();
    if !matches {
        return Err(Error::failed("environmental end event does not match an active stressor"));
    }
    write_infrastructure(world, "environmental_stressors", stressor_id, |record| put(record, "active", V::Bool(false)))
}

/// What `_interrupt_route` checked before its first write: the end, the route, and each shipment
/// with its delivery's new minute.
struct RoutePlan {
    until: i64,
    route_id: V,
    delays: Vec<(V, i64)>,
}

fn plan_route(world: &World, inputs: &InfraIn<'_>, event: &EventRow, outage_id: &V) -> Result<RoutePlan> {
    let state = infrastructure_state(world, inputs, event)?;
    let payload = &event.payload;
    let interruptions = item(&state, "route_interruptions")?;
    let record = lookup(interruptions, outage_id)?.cloned().unwrap_or(V::Null);
    let valid = fields_are(payload, &["outage_id", "route_id", "shipment_ids", "until_minute"])
        && !matches!(record, V::Null)
        && eq_text(item(&record, "start_event_id")?, &event.event_id)
        && !item(&record, "active")?.is_truthy();
    let until = if !valid || event.source_id.is_none() { None } else { until_of(world, event)? };
    let Some(until) = until else {
        return Err(Error::failed("supply route interruption payload or duration is invalid"));
    };
    let route_id = item(&record, "route_id")?.clone();
    let V::Map(all) = interruptions else {
        return Err(Error::unsupported("the interruptions are not a plain dict"));
    };
    for other in all.values() {
        if item(other, "active")?.is_truthy() && py_eq(item(other, "route_id")?, &route_id) {
            return Err(Error::failed("supply route already has an active interruption"));
        }
    }
    let operational = operational_of(world, inputs.operational)?;
    if matches!(operational, V::Null) {
        return Err(Error::failed("supply route interruption requires shipment records"));
    }
    let due = event.due_minute;
    let delay = until - due;
    let duration = world.duration_minutes()?;
    let V::List(shipment_ids) = item(&record, "shipment_ids")? else {
        return Err(Error::unsupported("the shipment IDs are not a list"));
    };
    let mut delays = Vec::with_capacity(shipment_ids.len());
    for shipment_id in shipment_ids {
        let cannot = || -> Result<Error> { Ok(Error::failed(format!("shipment {} cannot be safely delayed by this route event", repr(shipment_id)?))) };
        let shipment = lookup(item(&operational, "shipments")?, shipment_id)?.filter(|shipment| !matches!(shipment, V::Null));
        let Some(shipment) = shipment else {
            return Err(cannot()?);
        };
        let Some((_, delivery)) = pending_named(world, item(shipment, "event_id")?)? else {
            return Err(cannot()?);
        };
        let delivery_due = delivery.due_minute;
        if &*delivery.event_type != "resource_delivery" || delivery_due <= due || !eq_int(item(shipment, "due_minute")?, delivery_due) || delivery_due + delay >= duration {
            return Err(cannot()?);
        }
        delays.push((shipment_id.clone(), delivery_due + delay));
    }
    Ok(RoutePlan { until, route_id, delays })
}

/// `_interrupt_route`: the APPLIED records of the deliveries it delays, which the dispatcher appends.
/// Each delay runs inside this rule's transaction, as `DeliveryDelayHandler` did inside it: the world
/// is validated as a transaction's start validates it, the delay's rule writes it, the world is
/// validated with the delay in flight, and the delay leaves the queue.
fn interrupt_route<C: WorldCell, H: Host>(cell: &C, host: &mut H, spec: &mut Spec, inputs: &InfraIn<'_>, event: &EventRow, taken: &mut Vec<u64>) -> std::result::Result<Vec<Followed>, H::Error>
where
    H::Error: From<WriteError>,
{
    let outage_id = get(&event.payload, "outage_id")?.clone();
    let RoutePlan { until, route_id, delays } = cell.with(|world| plan_route(world, inputs, event, &outage_id))??;
    let source = spec.get(cell, host)?.source_id.clone();
    let mut delayed = Vec::with_capacity(delays.len());
    for (shipment_id, new_due) in delays {
        let delay_event = cell.with(|world| -> WriteResult<EventRow> {
            let world = &*world;
            let events = Events::of(world)?;
            let sequence = events.allocate_sequence()?;
            let id = format!("route-delay-{sequence}");
            let mut payload = IndexMap::with_capacity(2);
            payload.insert(text("shipment_id"), shipment_id.clone());
            payload.insert(text("to_minute"), V::Int(new_due));
            let mut provenance = kit::assumption(V::Str(source.clone()), INFRASTRUCTURE_POLICY);
            provenance.insert(text("route_id"), route_id.clone());
            let delay_event = kit::new_event(kit::NewEvent {
                event_id: text(&id),
                kind: EventKind::Completion,
                due_minute: event.due_minute,
                priority: -10,
                sequence,
                event_type: text("delivery_delayed"),
                payload,
                source_id: event.source_id.clone(),
                causation_id: Some(event.event_id.clone()),
                provenance,
            })?;
            identity_free(world, &id, sequence)?;
            events.schedule(delay_event.clone())?;
            Ok(delay_event)
        })??;
        // The delay's transaction inside this one: the checks of its start, its rule, and the
        // validation with the delay in flight.
        host.validate_candidate(TxnKind::Full, Stage::World)?;
        host.validate_candidate(TxnKind::Full, Stage::Candidate)?;
        crate::txn::NESTED_RULES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mark = taken.len();
        cell.with(|world| delivery_delay(world, inputs.operational, &delay_event, taken))??;
        host.slots_changed(&taken[mark..])?;
        host.validate_in_flight(&delay_event)?;
        let minute = cell.with(|world| -> Result<i64> {
            let Some((serial, _)) = world.queue_mut()?.take_slot(&delay_event.event_id) else {
                return Err(Error::Key(delay_event.event_id.to_string()));
            };
            taken.push(serial);
            world.minute_index()
        })??;
        delayed.push(Followed { event: delay_event, minute, detail: None });
    }
    cell.with(|world| {
        write_infrastructure(world, "route_interruptions", &outage_id, |record| {
            put(record, "active", V::Bool(true))?;
            put(record, "until_minute", V::Int(until))
        })
    })??;
    let mut completion = IndexMap::with_capacity(2);
    completion.insert(text("outage_id"), outage_id.clone());
    completion.insert(text("route_id"), route_id);
    let restore = schedule_completion(cell, host, spec, event, "supply_route_restored", completion, until)?;
    cell.with(|world| write_infrastructure(world, "route_interruptions", &outage_id, |record| put(record, "restore_event_id", id_value(restore))))??;
    Ok(delayed)
}

fn restore_route(world: &World, inputs: &InfraIn<'_>, event: &EventRow) -> Result<()> {
    let state = infrastructure_state(world, inputs, event)?;
    let payload = &event.payload;
    let outage_id = get(payload, "outage_id")?;
    let record = record_of(&state, "route_interruptions", outage_id)?;
    let matches = fields_are(payload, &["outage_id", "route_id"])
        && !matches!(record, V::Null)
        && item(&record, "active")?.is_truthy()
        && eq_text(item(&record, "restore_event_id")?, &event.event_id)
        && py_eq(item(&record, "start_event_id")?, &optional(&event.causation_id))
        && py_eq(item(&record, "route_id")?, get(payload, "route_id")?)
        && event.source_id.is_some();
    if !matches {
        return Err(Error::failed("route restoration does not match an active interruption"));
    }
    write_infrastructure(world, "route_interruptions", outage_id, |record| put(record, "active", V::Bool(false)))
}

/// `InfrastructureEventHandler`'s rule of this kind, on the transaction's world: the APPLIED records
/// of the deliveries a route's interruption delayed.
pub fn infrastructure<C: WorldCell, H: Host>(cell: &C, host: &mut H, inputs: &InfraIn<'_>, event: &EventRow, kind: Infra, taken: &mut Vec<u64>) -> std::result::Result<Vec<Followed>, H::Error>
where
    H::Error: From<WriteError>,
{
    let mut spec = Spec(None);
    match kind {
        Infra::StartOutage => start_outage(cell, host, &mut spec, inputs, event)?,
        Infra::RestoreOutage => restore_outage(cell, host, &mut spec, inputs, event)?,
        Infra::StartStressor => start_stressor(cell, host, &mut spec, inputs, event)?,
        Infra::EndStressor => cell.with(|world| end_stressor(world, inputs, event))??,
        Infra::InterruptRoute => return interrupt_route(cell, host, &mut spec, inputs, event, taken),
        Infra::RestoreRoute => cell.with(|world| restore_route(world, inputs, event))??,
    }
    Ok(Vec::new())
}

/// The facilities' working units before an infrastructure event, by ID in the world's order.
pub fn working_units(world: &World) -> Result<Vec<(Arc<str>, i64)>> {
    Ok(world.rows()?.facilities.iter().map(|(id, facility)| (id.clone(), facility.working_units)).collect())
}

/// The first of the payload's IDs that is a non-empty str, or the event's.
fn component_of(event: &EventRow) -> Arc<str> {
    for name in ["component_id", "outage_id", "facility_id"] {
        if let Some(V::Str(id)) = kit::payload(event, name) {
            if !id.is_empty() {
                return id.clone();
            }
        }
    }
    event.event_id.clone()
}

/// The receipts of the facilities whose working units the committed event changed, in ID order,
/// where the world has the shared contract: `previous` is `working_units` before the transaction.
pub fn infrastructure_receipts(world: &World, shared_absent: bool, previous: &[(Arc<str>, i64)], event: &EventRow) -> Result<Vec<Made>> {
    let component = component_of(event);
    let mut sources: Vec<Arc<str>> = Vec::new();
    for value in [optional(&event.source_id), kit::provenance(event, "source_id").cloned().unwrap_or(V::Null)] {
        if let V::Str(source) = value {
            if !source.is_empty() {
                sources.push(source);
            }
        }
    }
    sources.sort_unstable();
    sources.dedup();
    let mut sorted_previous = previous.to_vec();
    sorted_previous.sort_unstable();
    let mut current = working_units(world)?;
    current.sort_unstable_by(|first, second| first.0.cmp(&second.0));
    let mut records = Vec::new();
    for (id, units) in current {
        let Ok(index) = sorted_previous.binary_search_by(|(known, _)| known.cmp(&id)) else {
            continue;
        };
        let was = sorted_previous[index].1;
        if units == was || shared_absent {
            continue;
        }
        let status = if units < was { "degraded" } else { "restored" };
        records.push(care_receipts::facility_recovery_record(world, &id, &component, status, was, units, &event.event_id, sources.clone())?);
    }
    Ok(records)
}

// ---- The operational handlers with their inputs, in a rule transaction (lane P9-1b) --------------
//
// Each reads what the world's outside holds through the host (`Host::holding`, the sections of the
// configs, the floor plan: H1 and H2 of design-p9.md), and runs in the core's rule transaction
// (`txn::rule`, policy E16), whose start makes the host read the holding again: the rule reads the
// world in canonical order, as the copy was. A read the Python rule made and the core could not (the
// support cooldown, the staff operations formula) is refused with its `*_UNREAD` reason where the
// rule reads it, for the PyO3 crate to raise what the read raised. The PyO3 crate reads the rows of
// the pending events Python scheduled before it calls these.

const OPERATIONAL_OTHER: &str = "the world holds another operational state";
/// `SHARED_OPERATIONS_KEY` of `domain/shared_operations.py`.
const SHARED_OPERATIONS: &str = "shared_operations_v1";

/// `_apply_operational_transaction`: the rule (`body`, with the slots it took out) and the core's
/// end of it (`settle`), in a rule transaction that stands for the Python rule's copy through
/// canonical JSON, the EventLog included: the APPLIED record and the follow-ups go to the world's
/// log, and a rollback takes them out again. The host lets go its views of the slots the rule and
/// the end took out (`Host::slots_changed`).
pub fn operational<C: WorldCell, H: Host, T>(
    cell: &C,
    host: &mut H,
    event: &EventRow,
    body: impl FnOnce(&C, &mut H, &mut Vec<u64>) -> std::result::Result<(Vec<Made>, T), H::Error>,
) -> std::result::Result<(Vec<Made>, T), H::Error>
where
    H::Error: From<WriteError>,
{
    crate::txn::rule_kept(cell, host, TxnKind::Full, |cell, host| {
        let mut taken = Vec::new();
        let done = body(cell, host, &mut taken);
        host.slots_changed(&taken)?;
        let (records, value) = done?;
        let shared = kit::holding(cell, host)?.held(SHARED_OPERATIONS, "the world holds another document than the row's");
        let mut taken = Vec::new();
        let settled = settle(cell, host, &shared, event, &records, &mut taken);
        host.slots_changed(&taken)?;
        settled?;
        Ok((records, value))
    })
}

/// An operational event whose rule reads only `operational_events_v1`, in its transaction.
fn simple_operational<C: WorldCell, H: Host>(
    cell: &C,
    host: &mut H,
    event: &EventRow,
    rule: impl FnOnce(&World, &Held, &EventRow, &mut Vec<u64>) -> Result<()>,
) -> std::result::Result<(), H::Error>
where
    H::Error: From<WriteError>,
{
    operational(cell, host, event, |cell, host, taken| {
        let held = kit::holding(cell, host)?.held(OPERATIONAL, OPERATIONAL_OTHER);
        cell.with(|world| rule(world, &held, event, taken))??;
        Ok((Vec::new(), ()))
    })
    .map(drop)
}

/// `DeliveryDelayHandler.apply`.
pub fn delivery_delay_event<C: WorldCell, H: Host>(cell: &C, host: &mut H, event: &EventRow) -> std::result::Result<(), H::Error>
where
    H::Error: From<WriteError>,
{
    simple_operational(cell, host, event, delivery_delay)
}

/// `WaterOutageHandler.apply`.
pub fn water_outage_event<C: WorldCell, H: Host>(cell: &C, host: &mut H, event: &EventRow) -> std::result::Result<(), H::Error>
where
    H::Error: From<WriteError>,
{
    simple_operational(cell, host, event, |world, held, row, _| water_outage(world, held, row))
}

/// `WaterRestorationHandler.apply`.
pub fn water_restoration_event<C: WorldCell, H: Host>(cell: &C, host: &mut H, event: &EventRow) -> std::result::Result<(), H::Error>
where
    H::Error: From<WriteError>,
{
    simple_operational(cell, host, event, |world, held, row, _| water_restoration(world, held, row))
}

/// `StaffRestorationHandler.apply`.
pub fn staff_restoration_event<C: WorldCell, H: Host>(cell: &C, host: &mut H, event: &EventRow) -> std::result::Result<(), H::Error>
where
    H::Error: From<WriteError>,
{
    simple_operational(cell, host, event, |world, held, row, _| staff_restoration(world, held, row))
}

/// The manager of the resource service as `ResourceServiceManager(world)` makes it (the PyO3 crate's
/// `service::Inputs::of_world`), owned.
struct ServiceRead {
    config: Arc<IndexMap<Arc<str>, V>>,
    service: Held,
    ledger: Held,
    queues: Held,
    geo: Arc<crate::geometry::Geo>,
    metric_state_held: bool,
}

impl ServiceRead {
    /// What the manager reads, in its order, or what its first read that failed said.
    fn of<C: WorldCell, H: Host>(cell: &C, host: &mut H, holding: &crate::host::Holding) -> std::result::Result<Self, H::Error> {
        let service = kit::kept_section_value(cell, host, holding, crate::config::SectionId::ResourceService, |configs| &configs.resource_service, crate::config::service::ResourceService::parse)?;
        let service = kit::kept(&service).map(|service| (service.present, service.config.clone()))?;
        if !service.0 {
            return Err(Error::unsupported("no resource service").into());
        }
        if !holding.table {
            return Err(Error::unsupported(crate::host::TABLE_MAPPING).into());
        }
        let schema = cell.with(|world| world.schema_version())??;
        if schema != 6 || !holding.keys.contains_key(SERVICE) || !holding.keys.contains_key("resource_ledger_v1") {
            return Err(Error::unsupported("not an opted-in world").into());
        }
        // A config that is not a dict was copied with `dict(raw)` of its Python value.
        let config = service.1.ok_or_else(|| Error::unsupported("the resource service's config is not a dict"))?;
        Ok(Self {
            config,
            service: holding.held(SERVICE, "the world holds another service state"),
            ledger: holding.held("resource_ledger_v1", "the world holds another ledger"),
            queues: holding.held("facility_queues_v1", "the world holds another queue state"),
            geo: host.floor_plan()?,
            metric_state_held: holding.field(crate::world::METRIC_STATE),
        })
    }

    fn manager(&self) -> Manager<'_> {
        Manager {
            config: &self.config,
            service: self.service.clone(),
            ledger: self.ledger.clone(),
            queues: self.queues.clone(),
            geo: &self.geo,
            metric_state_held: self.metric_state_held,
        }
    }
}

/// A failure of a service's operation as the PyO3 crate raises it (`service::failed`): the ledger's
/// text inside the rule's wrap. A value the text writes with `repr` that the core cannot write is
/// refused.
fn service_failed<E: From<Error> + From<WriteError>>(failure: Failure) -> E {
    match failure {
        Failure::Write(error) => E::from(error),
        Failure::Ledger(fault, wrap) => {
            let mut written = true;
            let text = fault.text(|value| match value {
                V::Str(text) => py_repr(text),
                V::Int(value) => value.to_string(),
                V::Bool(true) => "True".to_owned(),
                V::Bool(false) => "False".to_owned(),
                V::Null => "None".to_owned(),
                _ => {
                    written = false;
                    String::new()
                }
            });
            if !written {
                return E::from(Error::unsupported("a ledger's value the core does not write with repr"));
            }
            E::from(wrap.error(&text))
        }
    }
}

/// `world.resources = {**world.resources, resource: remaining}`, as a write of the field's row
/// (its tree follows when the world is given back). Refused while the field holds another dict.
fn replace_resources(world: &World, holding: &crate::host::Holding, applied: Applied) -> Result<()> {
    if !holding.field(crate::world::RESOURCES) {
        return Err(Error::unsupported("the resources are not the row's"));
    }
    let id = crate::state::resource_ledger::RESOURCE_IDS[applied.index];
    let mut resources = world.row_mut::<crate::world::ResourcesRow>(crate::world::RESOURCES)?;
    match resources.get_mut(id) {
        Some(value) => *value = applied.remaining,
        None => {
            resources.insert(Arc::from(id), applied.remaining);
        }
    }
    Ok(())
}

/// `StaffUnavailabilityHandler.apply`: the records of a support task it interrupted. A manager of
/// the resource service whose read raised is that error where the rule makes the manager; a cooldown
/// whose read raised stays `COOLDOWN_UNREAD`, and a formula `FORMULA_UNREAD`, for the PyO3 crate.
pub fn staff_unavailable_event<C: WorldCell, H: Host>(cell: &C, host: &mut H, event: &EventRow) -> std::result::Result<Vec<Made>, H::Error>
where
    H::Error: From<WriteError>,
{
    operational(cell, host, event, |cell, host, taken| {
        let holding = kit::holding(cell, host)?;
        let operational = holding.held(OPERATIONAL, OPERATIONAL_OTHER);
        let mut gathered = kit::StaffRead::of(cell, host, true, false)?;
        let read = ServiceRead::of(cell, host, &holding);
        let manager = read.as_ref().ok().map(ServiceRead::manager);
        let service = match (&manager, &read) {
            (Some(manager), _) => Ok(manager),
            (None, Err(error)) if error.class() == crate::host::ErrorClass::Refused => Err(Unread::Refused(error.text().into())),
            (None, _) => Err(Unread::Raised),
        };
        let cooldown = kit::kept_section(cell, host, &holding, crate::config::SectionId::SupportCooldownMinutes, |configs| &configs.support_cooldown_minutes)?;
        let ops = gathered.inputs();
        let inputs = StaffIn {
            operational: &operational,
            ops: &ops,
            service,
            cooldown: cooldown.as_ref().ok().map(|minutes| **minutes),
            metric_state_held: holding.field(crate::world::METRIC_STATE),
        };
        let done = match cell.with(|world| staff_unavailable(world, &inputs, event, taken))? {
            Ok(done) => done,
            Err(failure) => {
                let error: H::Error = service_failed(failure);
                drop(manager);
                return match read {
                    Err(read) if error.class() == crate::host::ErrorClass::Refused && error.text() == SERVICE_UNREAD => Err(read),
                    _ => gathered.finish(Err(error)),
                };
            }
        };
        if let Some(applied) = done.applied {
            cell.with(|world| replace_resources(world, &holding, applied))??;
        }
        Ok((done.records, ()))
    })
    .map(|(records, ())| records)
}

/// `InfrastructureEventHandler.apply` up to its records: the facilities' working units before it,
/// and the APPLIED records of the deliveries a route's interruption delayed.
pub fn infrastructure_event<C: WorldCell, H: Host>(
    cell: &C,
    host: &mut H,
    event: &EventRow,
    kind: Infra,
) -> std::result::Result<(Vec<(Arc<str>, i64)>, Vec<Followed>), H::Error>
where
    H::Error: From<WriteError>,
{
    let previous = cell.with(|world| working_units(world))??;
    let (_, followed) = operational(cell, host, event, |cell, host, taken| {
        let holding = kit::holding(cell, host)?;
        let infrastructure_state = holding.held(INFRASTRUCTURE, "the world holds another infrastructure state");
        let operational = holding.held(OPERATIONAL, OPERATIONAL_OTHER);
        let program = holding.held("effect_program_v1", "the world holds another effect_program_v1");
        let inputs = InfraIn { infrastructure: &infrastructure_state, operational: &operational, program: &program };
        Ok((Vec::new(), infrastructure(cell, host, &inputs, event, kind, taken)?))
    })?;
    Ok((previous, followed))
}

/// The receipts of the facilities an infrastructure event changed, once the world has changed.
pub fn infrastructure_after<C: WorldCell, H: Host>(cell: &C, host: &mut H, previous: &[(Arc<str>, i64)], event: &EventRow) -> std::result::Result<Vec<Made>, H::Error> {
    let shared_absent = !kit::in_model_state(&*kit::holding(cell, host)?, SHARED_OPERATIONS)?;
    Ok(cell.with(|world| infrastructure_receipts(world, shared_absent, previous, event))??)
}
