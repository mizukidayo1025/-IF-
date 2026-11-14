//! `validate_infrastructure_runtime_state(world, in_flight_event=..., validate_event_history=...)`
//! of `domain/infrastructure.py`, with Python's errors and texts: the opt-in config (A03,
//! `ScenarioInfrastructureSpec`, whose `ValidationError` text the core's `contracts` writes), the
//! exact shape of `infrastructure_v1` and its base capacities, and, with the history, each outage,
//! environmental stressor and route interruption against its start and end events, the facilities'
//! capacity under the active outages and the completion events.
//!
//! Lane C of #250 (#348). Raw errors: `validate::world` makes a `ValueError`
//! `invalid infrastructure state: {exc}`; a `KeyError` or `TypeError` passes as it is.

use std::collections::{BTreeSet, VecDeque};
use std::sync::Arc;

use indexmap::{IndexMap, IndexSet};
use rustc_hash::{FxHashMap, FxHashSet};

use super::entities::{get, is_text, state_hashable};
use super::operational_events::{int_of, is_stable_id, keys_are, payload_keys_are, Log};
use super::View;
use crate::contracts;
use crate::error::{Error, Result};
use crate::events::{EventKind, EventRow};
use crate::host::Contract;
use crate::rules::dispatch::py_eq;
use crate::value::V;

const KEY: &str = "infrastructure_v1";
const START_TYPES: [&str; 6] = ["power_outage", "equipment_failure", "extreme_heat", "extreme_cold", "staff_shift_change", "supply_route_interruption"];
const COMPLETION_TYPES: [&str; 4] = ["power_restored", "equipment_repaired", "environmental_stressor_end", "supply_route_restored"];
const ALL_TYPES: [&str; 10] = [
    "power_outage",
    "equipment_failure",
    "extreme_heat",
    "extreme_cold",
    "staff_shift_change",
    "supply_route_interruption",
    "power_restored",
    "equipment_repaired",
    "environmental_stressor_end",
    "supply_route_restored",
];
const MAX_START_EVENTS: usize = 256;
const MAX_ROUTE_SHIPMENTS: usize = 100;
const MAX_ACTIVE_STRESSORS_PER_ZONE: i64 = 16;

fn invalid<T>(text: &str) -> Result<T> {
    Err(Error::Value(text.to_owned()))
}

fn ordinary(event: &EventRow) -> bool {
    matches!(event.kind, EventKind::Exogenous | EventKind::Intervention)
}

/// `first == second` of two values, a missing one being None.
fn eq(first: Option<&V>, second: Option<&V>) -> bool {
    match (first, second) {
        (None | Some(V::Null), None | Some(V::Null)) => true,
        (Some(first), Some(second)) => py_eq(first, second),
        _ => false,
    }
}

/// `event.due_minute < until <= duration` of an int `until`.
fn ends_in_run(event: &EventRow, until: Option<&V>, duration: i64) -> bool {
    int_of(until).is_some_and(|until| event.due_minute < until && until <= duration)
}

/// The canonical config's dependency edges: dependency -> its dependents.
struct Graph(FxHashMap<Arc<str>, BTreeSet<Arc<str>>>);

impl Graph {
    fn of(config: &V) -> Self {
        let mut dependents: FxHashMap<Arc<str>, BTreeSet<Arc<str>>> = FxHashMap::default();
        if let Some(V::List(edges)) = get(config, "dependency_edges") {
            for edge in edges {
                if let (Some(V::Str(dependent)), Some(V::Str(dependency))) = (get(edge, "dependent_id"), get(edge, "dependency_id")) {
                    dependents.entry(dependency.clone()).or_default().insert(dependent.clone());
                }
            }
        }
        Graph(dependents)
    }

    /// `dependent_facilities`: the facilities downstream of a component, sorted.
    fn facilities(&self, component: &str, facility: &dyn Fn(&str) -> bool) -> Vec<Arc<str>> {
        let mut reached: BTreeSet<Arc<str>> = BTreeSet::new();
        let start: Arc<str> = Arc::from(component);
        let mut visited: FxHashSet<Arc<str>> = FxHashSet::default();
        visited.insert(start.clone());
        let mut pending = VecDeque::from([start]);
        while let Some(current) = pending.pop_front() {
            if facility(&current) {
                reached.insert(current.clone());
            }
            for dependent in self.0.get(&current).into_iter().flatten() {
                if facility(dependent) {
                    reached.insert(dependent.clone());
                }
                if visited.insert(dependent.clone()) {
                    pending.push_back(dependent.clone());
                }
            }
        }
        reached.into_iter().collect()
    }
}

/// The check, as `validate` (`validate/mod.rs`) calls it in Python's order.
pub fn validate(view: &View<'_>) -> Result<()> {
    let configured = view.config("infrastructure")?.filter(|config| !matches!(config, V::Null));
    let state = view.state(KEY)?;
    let state = state.as_deref().filter(|state| !matches!(state, V::Null));
    let log = Log::of(view)?;
    let minute = view.minute_index()?;
    if let Some(event) = view.in_flight {
        if !log.holds(event) || event.due_minute > minute {
            return invalid("in-flight infrastructure event must be pending and due");
        }
    }
    let Some(configured) = configured else {
        let pending_events = log.pending.iter().any(|event| ALL_TYPES.contains(&&*event.event_type));
        let realized = view.history && {
            let records = view.log()?;
            ALL_TYPES.iter().any(|name| !records.positions_of_type(name).is_empty())
        };
        if state.is_some() || pending_events || realized {
            return invalid("infrastructure events require the infrastructure model");
        }
        return Ok(());
    };
    if !matches!(configured, V::Map(_)) {
        return invalid("infrastructure model config must be an object");
    }
    let canonical = contracts::validate(&Contract::Infrastructure, &configured)?;
    if !py_eq(&configured, &canonical) {
        return invalid("infrastructure model config must use complete canonical v1 values");
    }
    let fields = ["schema_version", "base_working_units", "outages", "environmental_stressors", "route_interruptions"];
    let state = match state {
        Some(V::Map(state))
            if keys_are(state, &fields)
                && state.get("schema_version").is_some_and(|version| py_eq(version, &V::Int(1)))
                && ["base_working_units", "outages", "environmental_stressors", "route_interruptions"].iter().all(|name| matches!(state.get(*name), Some(V::Map(_)))) =>
        {
            state
        }
        _ => return invalid("infrastructure_v1 has an invalid exact schema"),
    };
    let (Some(V::Map(base)), Some(V::Map(outages)), Some(V::Map(stressors)), Some(V::Map(routes))) =
        (state.get("base_working_units"), state.get("outages"), state.get("environmental_stressors"), state.get("route_interruptions"))
    else {
        return invalid("infrastructure_v1 has an invalid exact schema");
    };
    let rows = view.rows()?;
    if base.keys().any(|id| !rows.facilities.contains_key(id)) {
        return invalid("infrastructure base capacity references an unknown facility");
    }
    for (facility_id, units) in base {
        let capacity = rows.facilities.get(facility_id).map_or(0, |facility| facility.capacity);
        if !int_of(Some(units)).is_some_and(|units| (0..=capacity).contains(&units)) {
            return invalid("infrastructure base capacity is invalid");
        }
    }
    if !view.history {
        return Ok(());
    }

    let duration = view.duration_minutes()?;
    let mut all_events = log.pending.clone();
    all_events.extend(log.of_types(&ALL_TYPES)?);
    // `start_events`: by event ID, a later event of an ID taking the place of an earlier one.
    let mut start_events: IndexMap<Arc<str>, Arc<EventRow>> = IndexMap::new();
    for event in &all_events {
        if START_TYPES.contains(&&*event.event_type) && &*event.event_type != "staff_shift_change" {
            start_events.insert(event.event_id.clone(), event.clone());
        }
    }
    let ids_of = |types: &[&str]| -> IndexSet<Arc<str>> {
        start_events.iter().filter(|(_, event)| types.contains(&&*event.event_type)).map(|(id, _)| id.clone()).collect()
    };
    let expected_outages = ids_of(&["power_outage", "equipment_failure"]);
    let expected_stressors = ids_of(&["extreme_heat", "extreme_cold"]);
    let expected_routes = ids_of(&["supply_route_interruption"]);
    if start_events.len() > MAX_START_EVENTS {
        return invalid("infrastructure runtime exceeds the bounded event limit");
    }
    for (records, expected, text) in [
        (outages, &expected_outages, "infrastructure outage records do not match scheduled outage events"),
        (stressors, &expected_stressors, "environmental stressor records do not match scheduled events"),
        (routes, &expected_routes, "route interruption records do not match scheduled events"),
    ] {
        let mut starts: FxHashSet<Arc<str>> = FxHashSet::default();
        for record in records.values() {
            match get(record, "start_event_id") {
                Some(V::Str(id)) if matches!(record, V::Map(_)) => {
                    starts.insert(id.clone());
                }
                _ => return invalid(text),
            }
        }
        if starts.len() != expected.len() || !expected.iter().all(|id| starts.contains(id)) {
            return invalid(text);
        }
    }

    let source_id = get(&canonical, "source_id");
    let graph = Graph::of(&canonical);
    let is_facility = |id: &str| rows.facilities.contains_key(id);
    let mut restorations: IndexSet<Arc<str>> = IndexSet::new();
    // `restore_id in restorations`, then `restorations.add(restore_id)`: a value Python cannot
    // hash raises; one that is no str is in none.
    let mut take = |id: &V, text: &str| -> Result<()> {
        state_hashable(id)?;
        if let V::Str(id) = id {
            if !restorations.insert(id.clone()) {
                return invalid(text);
            }
        }
        Ok(())
    };

    // ---- Outages.
    let outage_keys = ["event_type", "target_id", "start_event_id", "restore_event_id", "affected_facility_ids", "units_out", "until_minute", "active"];
    for (outage_id, record) in outages {
        let record = match record {
            V::Map(record) if is_stable_id(Some(&V::Str(outage_id.clone()))) && keys_are(record, &outage_keys) => record,
            _ => return invalid("infrastructure outage record ID is invalid"),
        };
        let event = log.get(record.get("start_event_id"), state_hashable)?;
        let Some(event) = event.filter(|event| is_text(record.get("event_type"), &event.event_type)) else {
            return invalid("infrastructure outage record references an unknown start event");
        };
        let payload = &event.payload;
        let power = &*event.event_type == "power_outage";
        let expected_payload: &[&str] = if power { &["outage_id", "component_id", "until_minute"] } else { &["outage_id", "facility_id", "units_out", "until_minute"] };
        if !ordinary(&event)
            || event.source_id.is_none()
            || !matches!(&*event.event_type, "power_outage" | "equipment_failure")
            || !payload_keys_are(&event, expected_payload)
            || !ends_in_run(&event, get(payload, "until_minute"), duration)
        {
            return invalid("infrastructure outage start event is invalid");
        }
        let target = if power { get(payload, "component_id") } else { get(payload, "facility_id") };
        let Some(V::Str(target)) = target.filter(|target| is_stable_id(Some(target))) else {
            return invalid("infrastructure outage record does not match its start event");
        };
        if !is_text(record.get("target_id"), target)
            || !eq(record.get("until_minute"), get(payload, "until_minute"))
            || !matches!(record.get("active"), Some(V::Bool(_)))
            || !matches!(record.get("affected_facility_ids"), Some(V::List(_)))
        {
            return invalid("infrastructure outage record does not match its start event");
        }
        let expected: Vec<V> = graph.facilities(target, &is_facility).into_iter().map(V::Str).collect();
        if !eq(record.get("affected_facility_ids"), Some(&V::List(expected))) {
            return invalid("infrastructure outage facility closure is invalid");
        }
        if !eq(record.get("units_out"), get(payload, "units_out")) {
            return invalid("infrastructure outage units do not match the start event");
        }
        let restore_id = record.get("restore_event_id").filter(|id| !matches!(id, V::Null));
        if let Some(restore_id) = restore_id {
            take(restore_id, "infrastructure restoration event IDs must be unique")?;
            let expected_type = if power { "power_restored" } else { "equipment_repaired" };
            let fine = log.get(Some(restore_id), state_hashable)?.is_some_and(|restore| {
                &*restore.event_type == expected_type
                    && restore.kind == EventKind::Completion
                    && payload_keys_are(&restore, &["outage_id"])
                    && Some(restore.due_minute) == int_of(get(payload, "until_minute"))
                    && restore.causation_id.as_deref() == Some(&*event.event_id)
                    && restore.source_id == event.source_id
                    && is_text(get(&restore.payload, "outage_id"), outage_id)
            });
            if !fine {
                return invalid("infrastructure restoration event does not match its outage");
            }
        }
        let restore_text = restore_id.and_then(text_of);
        let started = log.applied(Some(&event.event_id))?;
        if int_of(get(payload, "until_minute")).is_some_and(|until| until < duration) && started && restore_id.is_none() {
            return invalid("applied infrastructure outage is missing its restoration event");
        }
        let expected_active = started && !log.applied(restore_text)?;
        if !matches!(record.get("active"), Some(V::Bool(active)) if *active == expected_active) {
            return invalid("infrastructure outage activity does not match event history");
        }
    }

    // ---- Environmental stressors.
    let stressor_keys = ["event_type", "zone_id", "start_event_id", "end_event_id", "until_minute", "stress_per_minute", "source_id", "active"];
    for (stressor_id, record) in stressors {
        let V::Map(record) = record else {
            return invalid("environmental stressor record does not match its event");
        };
        let event = log.get(record.get("start_event_id"), state_hashable)?;
        let fine = event.as_ref().is_some_and(|event| {
            let until = get(&event.payload, "until_minute");
            is_stable_id(Some(&V::Str(stressor_id.clone())))
                && ordinary(event)
                && event.source_id.is_some()
                && keys_are(record, &stressor_keys)
                && matches!(&*event.event_type, "extreme_heat" | "extreme_cold")
                && payload_keys_are(event, &["stressor_id", "zone_id", "until_minute"])
                && is_text(record.get("event_type"), &event.event_type)
                && eq(record.get("zone_id"), get(&event.payload, "zone_id"))
                && eq(record.get("until_minute"), until)
                && ends_in_run(event, until, duration)
                && eq(record.get("source_id"), source_id)
                && matches!(record.get("active"), Some(V::Bool(_)))
        });
        let Some(event) = event.filter(|_| fine) else {
            return invalid("environmental stressor record does not match its event");
        };
        let rate = if &*event.event_type == "extreme_heat" { "heat_stress_per_minute" } else { "cold_stress_per_minute" };
        if !eq(record.get("stress_per_minute"), get(&canonical, rate)) {
            return invalid("environmental stressor rate does not match its assumption");
        }
        let until = int_of(get(&event.payload, "until_minute")).unwrap_or(duration);
        let end_id = record.get("end_event_id").filter(|id| !matches!(id, V::Null));
        if let Some(end_id) = end_id {
            take(end_id, "environmental end event IDs must be unique")?;
            let fine = log.get(Some(end_id), state_hashable)?.is_some_and(|end| {
                &*end.event_type == "environmental_stressor_end"
                    && end.kind == EventKind::Completion
                    && payload_keys_are(&end, &["stressor_id", "zone_id"])
                    && end.due_minute == until
                    && end.causation_id.as_deref() == Some(&*event.event_id)
                    && end.source_id == event.source_id
                    && is_text(get(&end.payload, "stressor_id"), stressor_id)
            });
            if !fine {
                return invalid("environmental end event does not match its stressor");
            }
        }
        let started = log.applied(Some(&event.event_id))?;
        if until < duration && started && end_id.is_none() {
            return invalid("applied environmental stressor is missing its end event");
        }
        let expected_active = started && !log.applied(end_id.and_then(text_of))?;
        if !matches!(record.get("active"), Some(V::Bool(active)) if *active == expected_active) {
            return invalid("environmental stressor activity does not match event history");
        }
    }
    let mut per_zone: FxHashMap<Arc<str>, i64> = FxHashMap::default();
    for record in stressors.values() {
        if matches!(get(record, "active"), Some(V::Bool(true))) {
            if let Some(V::Str(zone)) = get(record, "zone_id") {
                *per_zone.entry(zone.clone()).or_default() += 1;
            }
        }
    }
    if per_zone.values().any(|count| *count > MAX_ACTIVE_STRESSORS_PER_ZONE) {
        return invalid("infrastructure runtime exceeds the per-zone stressor limit");
    }

    // ---- Route interruptions.
    let shipment_ids: FxHashSet<Arc<str>> = match view.state("operational_events_v1")?.as_deref() {
        Some(V::Map(operational)) => match operational.get("shipments") {
            Some(V::Map(shipments)) => shipments.keys().cloned().collect(),
            _ => FxHashSet::default(),
        },
        _ => FxHashSet::default(),
    };
    let route_keys = ["route_id", "start_event_id", "restore_event_id", "until_minute", "shipment_ids", "active"];
    for (outage_id, record) in routes {
        let event = match record {
            V::Map(record) => log.get(record.get("start_event_id"), state_hashable)?,
            _ => None,
        };
        let V::Map(record) = record else {
            return invalid("route interruption record does not match its start event");
        };
        let fine = match &event {
            Some(event) => {
                let payload = &event.payload;
                let until = get(payload, "until_minute");
                let shipments = get(payload, "shipment_ids");
                let shaped = is_stable_id(Some(&V::Str(outage_id.clone())))
                    && ordinary(event)
                    && event.source_id.is_some()
                    && payload_keys_are(event, &["outage_id", "route_id", "shipment_ids", "until_minute"])
                    && keys_are(record, &route_keys)
                    && &*event.event_type == "supply_route_interruption"
                    && eq(record.get("route_id"), get(payload, "route_id"))
                    && eq(record.get("until_minute"), until)
                    && ends_in_run(event, until, duration)
                    && matches!(shipments, Some(V::List(items)) if items.len() <= MAX_ROUTE_SHIPMENTS)
                    && eq(record.get("shipment_ids"), shipments)
                    && matches!(record.get("active"), Some(V::Bool(_)));
                shaped && known_shipments(record.get("shipment_ids"), &shipment_ids)?
            }
            None => false,
        };
        let Some(event) = event.filter(|_| fine) else {
            return invalid("route interruption record does not match its start event");
        };
        let until = int_of(get(&event.payload, "until_minute")).unwrap_or(duration);
        let restore_id = record.get("restore_event_id").filter(|id| !matches!(id, V::Null));
        if let Some(restore_id) = restore_id {
            take(restore_id, "route restoration event IDs must be unique")?;
            let fine = log.get(Some(restore_id), state_hashable)?.is_some_and(|restore| {
                &*restore.event_type == "supply_route_restored"
                    && restore.kind == EventKind::Completion
                    && payload_keys_are(&restore, &["outage_id", "route_id"])
                    && restore.due_minute == until
                    && restore.causation_id.as_deref() == Some(&*event.event_id)
                    && restore.source_id == event.source_id
                    && is_text(get(&restore.payload, "outage_id"), outage_id)
                    && eq(get(&restore.payload, "route_id"), record.get("route_id"))
            });
            if !fine {
                return invalid("route restoration event does not match its interruption");
            }
        }
        let started = log.applied(Some(&event.event_id))?;
        if until < duration && started && restore_id.is_none() {
            return invalid("applied route interruption is missing its restoration event");
        }
        let expected_active = started && !log.applied(restore_id.and_then(text_of))?;
        if !matches!(record.get("active"), Some(V::Bool(active)) if *active == expected_active) {
            return invalid("route interruption activity does not match event history");
        }
    }

    // ---- The facilities under the active outages.
    let mut affected: IndexSet<Arc<str>> = IndexSet::new();
    for record in outages.values() {
        if matches!(get(record, "active"), Some(V::Bool(true))) {
            if let Some(V::List(ids)) = get(record, "affected_facility_ids") {
                affected.extend(ids.iter().filter_map(|id| text_of(id).map(Arc::from)));
            }
        }
    }
    if !affected.is_empty() {
        let capacities = effective_capacities(base, outages, &graph, &is_facility)?;
        for facility_id in &affected {
            let Some(expected) = capacities.get(facility_id) else {
                return Err(Error::Key(facility_id.to_string()));
            };
            if rows.facilities.get(facility_id).is_some_and(|facility| facility.working_units != *expected) {
                return invalid("active infrastructure outage and facility capacity disagree");
            }
        }
    }

    let completions: FxHashSet<Arc<str>> = all_events.iter().filter(|event| COMPLETION_TYPES.contains(&&*event.event_type)).map(|event| event.event_id.clone()).collect();
    if completions.len() != restorations.len() || !restorations.iter().all(|id| completions.contains(id)) {
        return invalid("infrastructure completion events must match active records exactly");
    }
    Ok(())
}

fn text_of(value: &V) -> Option<&str> {
    match value {
        V::Str(text) => Some(text),
        _ => None,
    }
}

/// `set(record.get("shipment_ids", ())) <= shipment_ids`.
fn known_shipments(ids: Option<&V>, known: &FxHashSet<Arc<str>>) -> Result<bool> {
    let Some(V::List(ids)) = ids else {
        return Ok(ids.is_none());
    };
    let mut all = true;
    for id in ids {
        state_hashable(id)?;
        all &= matches!(id, V::Str(id) if known.contains(id));
    }
    Ok(all)
}

/// `effective_facility_capacities`: the working units each facility of the base keeps under the
/// active outages.
fn effective_capacities(base: &IndexMap<Arc<str>, V>, outages: &IndexMap<Arc<str>, V>, graph: &Graph, is_facility: &dyn Fn(&str) -> bool) -> Result<FxHashMap<Arc<str>, i64>> {
    let mut forced_offline: FxHashSet<Arc<str>> = FxHashSet::default();
    let mut units_lost: FxHashMap<Arc<str>, i64> = FxHashMap::default();
    for outage in outages.values() {
        if !matches!(get(outage, "active"), Some(V::Bool(true))) {
            continue;
        }
        if is_text(get(outage, "event_type"), "power_outage") {
            if let Some(V::List(ids)) = get(outage, "affected_facility_ids") {
                forced_offline.extend(ids.iter().filter_map(|id| text_of(id).map(Arc::from)));
            }
        } else if let (Some(V::Str(target)), Some(units)) = (get(outage, "target_id"), int_of(get(outage, "units_out"))) {
            *units_lost.entry(target.clone()).or_default() += units;
        }
    }
    let mut equipment: IndexMap<Arc<str>, i64> = IndexMap::new();
    for (facility_id, base_units) in base {
        let base_units = int_of(Some(base_units)).unwrap_or(0);
        let units = units_lost.get(facility_id).copied().unwrap_or(0);
        if units > base_units {
            return invalid("combined infrastructure failures exceed installed units");
        }
        equipment.insert(facility_id.clone(), base_units - units);
    }
    for (facility_id, capacity) in &equipment {
        if *capacity == 0 {
            forced_offline.extend(graph.facilities(facility_id, is_facility));
        }
    }
    Ok(equipment.into_iter().map(|(id, capacity)| {
        let capacity = if forced_offline.contains(&id) { 0 } else { capacity };
        (id, capacity)
    }).collect())
}
