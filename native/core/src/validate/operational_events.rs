//! `validate_operational_events_state(world, in_flight_event=..., validate_event_history=...)` of
//! `domain/operational.py`, with Python's errors and texts: the exact shape of
//! `operational_events_v1` (H03), the events that opt into it, and, with the history, each
//! shipment, water outage and staff absence of the state against the pending and realized events.
//!
//! Lane C of #250 (#348). Raw errors: `validate::world` makes a `ValueError`
//! `invalid operational events state: {exc}`; a `TypeError` Python raises on an unhashable value
//! passes as it is.
//!
//! The reads of the event log (`Log`: the records of some types in append order, an event by its
//! ID as `EventsById` finds it, the status of a realized event) serve `infrastructure.rs` too.

use std::sync::Arc;

use indexmap::{IndexMap, IndexSet};
use rustc_hash::FxHashMap;

use super::entities::{get, in_table, index, is_text, payload_hashable, pending_events, same, state_hashable};
use super::extension_head::py::{held_type_name, Held};
use super::View;
use crate::error::{Error, Result};
use crate::events::{EventKind, EventRow};
use crate::rules::dispatch::{py_eq, same_event};
use crate::value::V;

const KEY: &str = "operational_events_v1";
const H03_TYPES: [&str; 5] = ["delivery_delayed", "water_outage", "water_restored", "staff_unavailable", "staff_shift_change"];
/// The event types the full check reads (`_CHECKED_EVENT_TYPES`).
const CHECKED_TYPES: [&str; 13] = [
    "facility_outage",
    "facility_recovery",
    "resource_delivery",
    "resource_loss",
    "resource_reserve",
    "resource_release",
    "resource_consume",
    "staff_restored",
    "delivery_delayed",
    "water_outage",
    "water_restored",
    "staff_unavailable",
    "staff_shift_change",
];
const ABSENCES: [&str; 2] = ["staff_unavailable", "staff_shift_change"];

fn invalid<T>(text: impl Into<String>) -> Result<T> {
    Err(Error::Value(text.into()))
}

/// `_is_stable_id`: `^[A-Za-z0-9][A-Za-z0-9._:-]{0,63}$`.
pub(super) fn is_stable_id(value: Option<&V>) -> bool {
    let Some(V::Str(text)) = value else {
        return false;
    };
    let bytes = text.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && bytes[0].is_ascii_alphanumeric()
        && bytes[1..].iter().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

/// An int and not a bool.
pub(super) fn int_of(value: Option<&V>) -> Option<i64> {
    match value {
        Some(V::Int(number)) => Some(*number),
        _ => None,
    }
}

/// `set(mapping) == set(keys)` of a dict.
pub(super) fn keys_are(map: &IndexMap<Arc<str>, V>, keys: &[&str]) -> bool {
    map.len() == keys.len() && keys.iter().all(|key| map.contains_key(*key))
}

/// `set(payload) == set(keys)` of an event's payload.
pub(super) fn payload_keys_are(event: &EventRow, keys: &[&str]) -> bool {
    matches!(&event.payload, V::Map(map) if keys_are(map, keys))
}

fn has(event: &EventRow, key: &str) -> bool {
    matches!(&event.payload, V::Map(map) if map.contains_key(key))
}

/// `payload.get(first, payload.get(second))`.
fn either<'a>(event: &'a EventRow, first: &str, second: &str) -> Option<&'a V> {
    if has(event, first) {
        get(&event.payload, first)
    } else {
        get(&event.payload, second)
    }
}

/// `uses_operational_events`.
fn uses_operational_events<'a>(events: impl IntoIterator<Item = &'a Arc<EventRow>>) -> bool {
    events
        .into_iter()
        .any(|event| H03_TYPES.contains(&&*event.event_type) || (&*event.event_type == "resource_delivery" && has(event, "shipment_id")))
}

/// `value == None`, a missing value being None.
fn is_none(value: Option<&V>) -> bool {
    matches!(value, None | Some(V::Null))
}

/// `first == second` of two values, a missing one being None.
fn eq(first: Option<&V>, second: Option<&V>) -> bool {
    match (first, second) {
        (None | Some(V::Null), None | Some(V::Null)) => true,
        (Some(first), Some(second)) => py_eq(first, second),
        _ => false,
    }
}

fn text_value(text: &Option<Arc<str>>) -> V {
    text.as_ref().map_or(V::Null, |text| V::Str(text.clone()))
}

// ---- The event log as the validators read it -------------------------------------------------------

/// The pending events and the log, as `EventsById` and `event_status` read them.
pub(super) struct Log<'v, 'w> {
    view: &'v View<'w>,
    pub pending: Vec<Arc<EventRow>>,
    by_id: FxHashMap<Arc<str>, usize>,
}

impl<'v, 'w> Log<'v, 'w> {
    pub fn of(view: &'v View<'w>) -> Result<Self> {
        let pending = pending_events(view)?;
        let by_id = pending.iter().enumerate().map(|(index, event)| (event.event_id.clone(), index)).collect();
        Ok(Self { view, pending, by_id })
    }

    /// `event_queue.pending()` holds an event equal to this one (`in_flight_event in pending`).
    pub fn holds(&self, event: &EventRow) -> bool {
        self.by_id.get(&event.event_id).is_some_and(|index| same_event(&self.pending[*index], event))
    }

    /// The events of the records of these types, in append order (`event_log.of_types`).
    pub fn of_types(&self, types: &[&str]) -> Result<Vec<Arc<EventRow>>> {
        let log = self.view.log()?;
        let ids: Vec<u32> = types.iter().filter_map(|name| log.type_id(name)).collect();
        log.of_type_ids_from(&ids, 0).into_iter().map(|index| record_event(&log, index).map(|(event, _)| event)).collect()
    }

    /// The records of these types, with whether each was applied.
    pub fn records_of_types(&self, types: &[&str]) -> Result<Vec<(Arc<EventRow>, bool)>> {
        let log = self.view.log()?;
        let ids: Vec<u32> = types.iter().filter_map(|name| log.type_id(name)).collect();
        log.of_type_ids_from(&ids, 0).into_iter().map(|index| record_event(&log, index)).collect()
    }

    /// `EventsById(event_log, pending).get(key)`: a realized event wins. A key that is not a str
    /// finds none, but a key Python cannot hash raises (`hashable`, by where the key comes from).
    pub fn get(&self, key: Option<&V>, hashable: fn(&V) -> Result<()>) -> Result<Option<Arc<EventRow>>> {
        match key {
            Some(V::Str(id)) => self.find(id),
            Some(other) => hashable(other).map(|()| None),
            None => Ok(None),
        }
    }

    pub fn find(&self, id: &str) -> Result<Option<Arc<EventRow>>> {
        let log = self.view.log()?;
        if let Some(index) = log.index_of(id) {
            return record_event(&log, index).map(|(event, _)| Some(event));
        }
        Ok(self.by_id.get(id).map(|index| self.pending[*index].clone()))
    }

    /// `event_status(event_id) is EventStatus.APPLIED`: the event in flight counts as applied.
    pub fn applied(&self, id: Option<&str>) -> Result<bool> {
        let Some(id) = id else {
            return Ok(false);
        };
        if self.view.in_flight.is_some_and(|event| &*event.event_id == id) {
            return Ok(true);
        }
        let log = self.view.log()?;
        Ok(log.index_of(id).is_some_and(|index| log.is_applied(index)))
    }
}

fn record_event(log: &crate::events::log::EventLog, index: usize) -> Result<(Arc<EventRow>, bool)> {
    let record = log.record(index)?.ok_or_else(|| Error::unsupported("a record of the log is only Python's"))?;
    Ok((Arc::new(record.event), log.is_applied(index)))
}

// ---- The check --------------------------------------------------------------------------------------

/// The check, as `validate` (`validate/mod.rs`) calls it in Python's order.
pub fn validate(view: &View<'_>) -> Result<()> {
    let state = view.state(KEY)?;
    // `model_state.get(KEY)`: a key that holds None is no state.
    let state = state.as_deref().filter(|state| !matches!(state, V::Null));
    let fields = ["schema_version", "shipments", "water_outages", "staff_status", "cancelled_staff_task_ids"];
    let state = match state {
        None => None,
        Some(V::Map(state))
            if keys_are(state, &fields)
                && state.get("schema_version").is_some_and(|version| py_eq(version, &V::Int(1)))
                && matches!(state.get("shipments"), Some(V::Map(_)))
                && matches!(state.get("water_outages"), Some(V::Map(_)))
                && matches!(state.get("staff_status"), Some(V::Map(_)))
                && matches!(state.get("cancelled_staff_task_ids"), Some(V::List(_))) =>
        {
            Some(state)
        }
        Some(_) => return invalid("operational_events_v1 has an invalid exact schema"),
    };
    let log = Log::of(view)?;
    let minute = view.minute_index()?;
    if let Some(event) = view.in_flight {
        if event.due_minute > minute || !log.holds(event) {
            return invalid("in-flight operational event must be pending and due");
        }
    }
    if state.is_none() && config_enabled(view)? {
        return invalid("model_configs.operational_events requires operational_events_v1 state");
    }
    if !view.history {
        // Plain ledger events such as resource_loss run without operational state.
        if state.is_none() && uses_operational_events(&log.pending) {
            return invalid("operational events require operational_events_v1 state");
        }
        return Ok(());
    }
    let mut all_events = log.pending.clone();
    all_events.extend(log.of_types(&CHECKED_TYPES)?);
    let found = Found::of(view, &log, &all_events)?;
    let Some(state) = state else {
        if uses_operational_events(&all_events) {
            return invalid("H03 events require operational_events_v1 state");
        }
        return Ok(());
    };
    shipments(state, &found)?;
    water_outages(state, &found, &log)?;
    staff_status(view, state, &found, &log)?;
    let Some(V::List(cancelled)) = state.get("cancelled_staff_task_ids") else {
        return Ok(());
    };
    let mut seen: IndexSet<&str> = IndexSet::new();
    let texts = cancelled.iter().all(|value| matches!(value, V::Str(text) if !text.is_empty()));
    if !texts || !cancelled.iter().all(|value| matches!(value, V::Str(text) if seen.insert(text))) {
        return invalid("cancelled staff task IDs must be unique non-empty strings");
    }
    Ok(())
}

/// `operational_events_enabled(world.model_configs, ())`: the config's own check, and whether it
/// opts in.
fn config_enabled(view: &View<'_>) -> Result<bool> {
    match view.config("operational_events")? {
        None | Some(V::Null) => Ok(false),
        Some(V::Map(config)) if config.len() == 1 => match config.get("enabled") {
            Some(V::Bool(enabled)) => Ok(*enabled),
            _ => invalid("operational_events config must be {'enabled': bool}"),
        },
        Some(_) => invalid("operational_events config must be {'enabled': bool}"),
    }
}

/// What the events say the state must hold, each event checked on the way.
struct Found {
    /// shipment ID -> (event ID, resource ID, due minute).
    shipments: IndexMap<Arc<str>, (Arc<str>, Option<V>, i64)>,
    /// outage ID -> (target kind, target ID, event ID).
    outages: IndexMap<Arc<str>, (&'static str, Option<V>, Arc<str>)>,
    /// outage ID -> its restorations.
    restorations: IndexMap<Arc<str>, Vec<Arc<EventRow>>>,
    /// unavailability event ID -> its restorations.
    staff_restorations: FxHashMap<Arc<str>, Vec<Arc<EventRow>>>,
}

impl Found {
    fn of(view: &View<'_>, log: &Log<'_, '_>, all_events: &[Arc<EventRow>]) -> Result<Self> {
        let rows = view.rows()?;
        let duration = view.duration_minutes()?;
        let infrastructure_config = view.config("infrastructure")?;
        let mut found = Found { shipments: IndexMap::new(), outages: IndexMap::new(), restorations: IndexMap::new(), staff_restorations: FxHashMap::default() };
        for event in all_events {
            let kind = &*event.event_type;
            let payload = &event.payload;
            match kind {
                "facility_outage" | "facility_recovery" => {
                    let amount_field = if kind == "facility_outage" { "units_out" } else { "units_restored" };
                    if !payload_keys_are(event, &["facility_id", amount_field]) {
                        return invalid(format!("{kind} payload fields do not match"));
                    }
                    if !in_table(get(payload, "facility_id"), |id| rows.facilities.contains_key(id))? {
                        return invalid(format!("{kind} references an unknown facility"));
                    }
                    if !int_of(get(payload, amount_field)).is_some_and(|amount| amount >= 0) {
                        return invalid(format!("{kind} amount is invalid"));
                    }
                }
                "resource_delivery" | "resource_loss" | "resource_reserve" | "resource_release" | "resource_consume" => {
                    let required: &[&str] = match kind {
                        "resource_delivery" | "resource_loss" => &["warehouse_id", "resource_id", "quantity"],
                        "resource_release" => &["warehouse_id", "resource_id", "reservation_id", "agent_id"],
                        _ => &["warehouse_id", "resource_id", "reservation_id", "agent_id", "quantity"],
                    };
                    let with_shipment = kind == "resource_delivery" && payload_keys_are(event, &["warehouse_id", "resource_id", "quantity", "shipment_id"]);
                    if !payload_keys_are(event, required) && !with_shipment {
                        return invalid(format!("{kind} payload fields do not match"));
                    }
                }
                _ if H03_TYPES.contains(&kind) => h03_event(event, &rows, duration, infrastructure_config.as_ref())?,
                "staff_restored" => {
                    let shape = payload_keys_are(event, &["staff_id", "unavailability_event_id"])
                        && event.kind == EventKind::Completion
                        && event.source_id.is_some()
                        && is_stable_id(get(payload, "staff_id"))
                        && in_table(get(payload, "staff_id"), |id| rows.staff.contains_key(id))?
                        && is_stable_id(get(payload, "unavailability_event_id"));
                    if !shape {
                        return invalid("staff restoration event shape is invalid");
                    }
                    let Some(V::Str(unavailability_id)) = get(payload, "unavailability_event_id") else {
                        return invalid("staff restoration event shape is invalid");
                    };
                    let unavailable = log.find(unavailability_id)?;
                    let matches = unavailable.as_ref().is_some_and(|unavailable| {
                        ABSENCES.contains(&&*unavailable.event_type)
                            && eq(get(&unavailable.payload, "staff_id"), get(payload, "staff_id"))
                            && eq(get(&unavailable.payload, "until_minute"), Some(&V::Int(event.due_minute)))
                            && event.causation_id.as_deref() == Some(&*unavailable.event_id)
                            && event.source_id == unavailable.source_id
                    });
                    if !matches {
                        return invalid("staff restoration event does not match its cause");
                    }
                    found.staff_restorations.entry(unavailability_id.clone()).or_default().push(event.clone());
                }
                _ => {}
            }
            if kind == "resource_delivery" && has(event, "shipment_id") {
                let shipment_id = get(payload, "shipment_id");
                if !is_stable_id(shipment_id) || event.source_id.is_none() || !matches!(event.kind, EventKind::Exogenous | EventKind::Intervention) {
                    return invalid("shipment ID is invalid");
                }
                let Some(V::Str(shipment_id)) = shipment_id else {
                    return invalid("shipment ID is invalid");
                };
                if found.shipments.contains_key(shipment_id) {
                    return invalid("shipment event IDs must be unique");
                }
                found.shipments.insert(shipment_id.clone(), (event.event_id.clone(), get(payload, "resource_id").cloned(), event.due_minute));
            } else if kind == "water_outage" {
                // The H03 check above found a stable ID.
                let Some(V::Str(outage_id)) = get(payload, "outage_id") else {
                    return invalid("water outage ID is invalid");
                };
                if found.outages.contains_key(outage_id) {
                    return invalid("water outage IDs must be unique");
                }
                let zone = has(event, "zone_id");
                let target = either(event, "zone_id", "facility_id");
                if zone {
                    let has_station = match target {
                        Some(V::Str(zone_id)) => {
                            rows.zones.contains_key(zone_id) && rows.facilities.iter().any(|(_, facility)| &*facility.kind == "water_station" && facility.zone_id == *zone_id)
                        }
                        other => in_table(other, |_| false)?,
                    };
                    if !has_station {
                        return invalid("water outage zone must contain a water_station");
                    }
                } else {
                    let station = match target {
                        Some(V::Str(id)) => rows.facilities.get(id).is_some_and(|facility| &*facility.kind == "water_station"),
                        other => in_table(other, |_| false)?,
                    };
                    if !station {
                        return invalid("water outage target must be a water_station");
                    }
                }
                found.outages.insert(outage_id.clone(), (if zone { "zone" } else { "facility" }, target.cloned(), event.event_id.clone()));
            } else if kind == "water_restored" {
                let Some(V::Str(outage_id)) = get(payload, "outage_id") else {
                    return invalid("water restoration outage_id is invalid");
                };
                found.restorations.entry(outage_id.clone()).or_default().push(event.clone());
            }
        }
        Ok(found)
    }
}

/// The checks of an H03 event (`delivery_delayed`, `water_outage`, `water_restored`,
/// `staff_unavailable`, `staff_shift_change`).
fn h03_event(event: &EventRow, rows: &crate::entities::Entities, duration: i64, infrastructure: Option<&V>) -> Result<()> {
    let kind = &*event.event_type;
    let payload = &event.payload;
    let valid_fields = match kind {
        "water_outage" => payload_keys_are(event, &["outage_id", "zone_id"]) || payload_keys_are(event, &["outage_id", "facility_id"]),
        "delivery_delayed" => payload_keys_are(event, &["shipment_id", "to_minute"]),
        "water_restored" => payload_keys_are(event, &["outage_id"]),
        _ => payload_keys_are(event, &["staff_id", "until_minute"]),
    };
    let ordinary = matches!(event.kind, EventKind::Exogenous | EventKind::Intervention);
    if !valid_fields || event.source_id.is_none() || (!ordinary && !infrastructure_delay(event, infrastructure)?) {
        return invalid(format!("{kind} event shape or provenance is invalid"));
    }
    match kind {
        "delivery_delayed" => {
            if !is_stable_id(get(payload, "shipment_id")) {
                return invalid("delivery delay shipment_id is invalid");
            }
            if int_of(get(payload, "to_minute")).is_none() {
                return invalid("delivery delay to_minute is invalid");
            }
        }
        "water_outage" => {
            let target = either(event, "zone_id", "facility_id");
            if !is_stable_id(get(payload, "outage_id")) {
                return invalid("water outage ID is invalid");
            }
            if has(event, "zone_id") && !in_table(target, |id| rows.zones.contains_key(id))? {
                return invalid("water outage references an unknown zone");
            }
            if has(event, "facility_id") {
                let station = match target {
                    Some(V::Str(id)) => rows.facilities.get(id).is_some_and(|facility| &*facility.kind == "water_station"),
                    other => in_table(other, |_| false)?,
                };
                if !station {
                    return invalid("water outage must target a water station");
                }
            }
        }
        "water_restored" => {
            if !is_stable_id(get(payload, "outage_id")) {
                return invalid("water restoration outage_id is invalid");
            }
        }
        _ => {
            let staff_id = get(payload, "staff_id");
            let until = int_of(get(payload, "until_minute"));
            let known = matches!(staff_id, Some(V::Str(id)) if rows.staff.contains_key(id));
            if !is_stable_id(staff_id) || !known || !until.is_some_and(|until| until > event.due_minute && until <= duration) {
                return invalid("staff unavailability target or time is invalid");
            }
        }
    }
    Ok(())
}

/// `infrastructure_delay`: a delay the infrastructure model scheduled as a completion. Its source
/// is read from `model_configs.get("infrastructure", {})`, which Python reads with `.get`: a config
/// that is no dict raises an `AttributeError`.
fn infrastructure_delay(event: &EventRow, infrastructure: Option<&V>) -> Result<bool> {
    if &*event.event_type != "delivery_delayed"
        || event.kind != EventKind::Completion
        || event.causation_id.is_none()
        || !is_text(get(&event.provenance, "policy_version"), "infrastructure-events-v1")
    {
        return Ok(false);
    }
    let source = match infrastructure {
        None => None,
        Some(V::Map(config)) => config.get("source_id"),
        // `selected_models` refuses such a config first, so a world never gets here with one.
        Some(other) => return Err(Error::Attribute(format!("'{}' object has no attribute 'get'", held_type_name(other, Held::Tracked)))),
    };
    Ok(eq(get(&event.provenance, "source_id"), source))
}

/// The state's shipments against the shipment events.
fn shipments(state: &IndexMap<Arc<str>, V>, found: &Found) -> Result<()> {
    let Some(V::Map(records)) = state.get("shipments") else {
        return Ok(());
    };
    if !same_keys(records, found.shipments.keys()) {
        return invalid("operational shipment state does not match shipment events");
    }
    for (shipment_id, (event_id, resource_id, due_minute)) in &found.shipments {
        let fine = match records.get(shipment_id) {
            Some(V::Map(record)) => {
                keys_are(record, &["event_id", "resource_id", "due_minute"])
                    && is_text(record.get("event_id"), event_id)
                    && eq(record.get("resource_id"), resource_id.as_ref())
                    && eq(record.get("due_minute"), Some(&V::Int(*due_minute)))
            }
            _ => false,
        };
        if !fine {
            return invalid("operational shipment record does not match its event");
        }
    }
    Ok(())
}

fn same_keys<'a>(map: &IndexMap<Arc<str>, V>, keys: impl ExactSizeIterator<Item = &'a Arc<str>>) -> bool {
    map.len() == keys.len() && {
        let mut keys = keys;
        keys.all(|key| map.contains_key(key))
    }
}

/// The state's water outages against the outage and restoration events.
fn water_outages(state: &IndexMap<Arc<str>, V>, found: &Found, log: &Log<'_, '_>) -> Result<()> {
    let Some(V::Map(records)) = state.get("water_outages") else {
        return Ok(());
    };
    if !same_keys(records, found.outages.keys()) {
        return invalid("operational water outage state does not match outage events");
    }
    if found.restorations.keys().any(|id| !found.outages.contains_key(id)) || found.restorations.values().any(|events| events.len() > 1) {
        return invalid("water restoration events must uniquely reference known outages");
    }
    let keys = ["target_kind", "target_id", "outage_event_id", "restore_event_id", "active"];
    for (outage_id, (target_kind, target_id, event_id)) in &found.outages {
        let record = match records.get(outage_id) {
            Some(V::Map(record))
                if keys_are(record, &keys)
                    && is_text(record.get("target_kind"), target_kind)
                    && eq(record.get("target_id"), target_id.as_ref())
                    && is_text(record.get("outage_event_id"), event_id)
                    && matches!(record.get("active"), Some(V::Bool(_))) =>
            {
                record
            }
            _ => return invalid("operational water outage record is invalid"),
        };
        let expected_restore = found.restorations.get(outage_id).and_then(|events| events.first()).map(|event| event.event_id.clone());
        let restore_id = record.get("restore_event_id");
        if !same(restore_id, expected_restore.as_deref()) {
            return invalid("operational water restoration state does not match its event");
        }
        if let Some(restore_id) = expected_restore.as_deref() {
            let restore = log.find(restore_id)?;
            let outage = log.find(event_id)?;
            let fine = match (restore, outage) {
                (Some(restore), Some(outage)) => {
                    &*restore.event_type == "water_restored"
                        && is_text(get(&restore.payload, "outage_id"), outage_id)
                        && restore.causation_id.as_deref() == Some(&**event_id)
                        && restore.due_minute > outage.due_minute
                }
                _ => false,
            };
            if !fine {
                return invalid("operational water restoration reference is invalid");
            }
        }
        let expected_active = log.applied(Some(event_id))? && !log.applied(expected_restore.as_deref())?;
        if !matches!(record.get("active"), Some(V::Bool(active)) if *active == expected_active) {
            return invalid("operational water outage activity does not match applied event history");
        }
    }
    Ok(())
}

/// The state's staff status against the staff and the absence and restoration events.
fn staff_status(view: &View<'_>, state: &IndexMap<Arc<str>, V>, found: &Found, log: &Log<'_, '_>) -> Result<()> {
    let Some(V::Map(records)) = state.get("staff_status") else {
        return Ok(());
    };
    let rows = view.rows()?;
    let duration = view.duration_minutes()?;
    if records.len() != rows.staff.len() || !rows.staff.iter().all(|(id, _)| records.contains_key(id)) {
        return invalid("operational staff status must cover the known staff IDs exactly");
    }
    // The latest applied absence of each member, by the staff ID of its payload.
    let mut latest: FxHashMap<Arc<str>, Arc<EventRow>> = FxHashMap::default();
    let mut absences: Vec<Arc<EventRow>> =
        log.records_of_types(&ABSENCES)?.into_iter().filter(|(_, applied)| *applied).map(|(event, _)| event).collect();
    if let Some(event) = view.in_flight.filter(|event| ABSENCES.contains(&&*event.event_type)) {
        absences.push(Arc::new(event.clone()));
    }
    for event in absences {
        let staff_id = index(&event.payload, "staff_id")?;
        payload_hashable(staff_id)?;
        if let V::Str(staff_id) = staff_id {
            latest.insert(staff_id.clone(), event.clone());
        }
    }
    let record_keys = ["available", "unavailability_event_id", "until_minute", "restore_event_id"];
    for (staff_id, record) in records {
        let record = match record {
            V::Map(record) if keys_are(record, &record_keys) && matches!(record.get("available"), Some(V::Bool(_))) => record,
            _ => return invalid("operational staff status record is invalid"),
        };
        let unavailable_id = record.get("unavailability_event_id");
        let until = record.get("until_minute");
        let restore_id = record.get("restore_event_id");
        if is_none(unavailable_id) {
            if !is_none(until) || !is_none(restore_id) {
                return invalid("available staff cannot retain unavailability event metadata");
            }
        } else {
            let unavailable = log.get(unavailable_id, state_hashable)?;
            let restore = if is_none(restore_id) { None } else { log.get(restore_id, state_hashable)? };
            let until_number = int_of(until);
            let fine = unavailable.as_ref().is_some_and(|unavailable| {
                ABSENCES.contains(&&*unavailable.event_type)
                    && is_text(get(&unavailable.payload, "staff_id"), staff_id)
                    && eq(get(&unavailable.payload, "until_minute"), until)
                    && unavailable.source_id.is_some()
                    && until_number.is_some_and(|until| {
                        !(until < duration && restore.is_none()) && !(until == duration && !is_none(restore_id))
                    })
                    && restore.as_ref().is_none_or(|restore| {
                        &*restore.event_type == "staff_restored"
                            && is_text(get(&restore.payload, "staff_id"), staff_id)
                            && eq(get(&restore.payload, "unavailability_event_id"), unavailable_id)
                            && eq(Some(&text_value(&restore.causation_id)), unavailable_id)
                            && restore.source_id == unavailable.source_id
                    })
            });
            if !fine {
                return invalid("operational staff restoration reference is invalid");
            }
        }
        let latest_absence = latest.get(staff_id);
        let expected_unavailability = latest_absence.map(|event| event.event_id.clone());
        let expected_until = latest_absence.map(|event| index(&event.payload, "until_minute").cloned()).transpose()?;
        let matching: &[Arc<EventRow>] = expected_unavailability.as_ref().and_then(|id| found.staff_restorations.get(id)).map_or(&[], Vec::as_slice);
        if matching.len() > 1 {
            return invalid("staff unavailability cannot have duplicate restorations");
        }
        let expected_restore = matching.first().map(|event| event.event_id.clone());
        let expected_available = match &expected_restore {
            Some(id) => log.applied(Some(id))?,
            None => expected_unavailability.is_none(),
        };
        let available = matches!(record.get("available"), Some(V::Bool(true)));
        if !same(unavailable_id, expected_unavailability.as_deref())
            || !eq(until, expected_until.as_ref())
            || !same(restore_id, expected_restore.as_deref())
            || available != expected_available
        {
            return invalid("operational staff availability does not match applied event history");
        }
        if !available {
            let busy = rows.staff.get(staff_id).is_some_and(|member| member.active_task_id.is_some() || !member.work_queue.is_empty() || member.busy_until != 0);
            if busy {
                return invalid("unavailable staff cannot retain an active task or busy state");
            }
        }
    }
    Ok(())
}
