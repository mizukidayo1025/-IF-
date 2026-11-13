//! The namespaces of `model_state` at minute 0, in the order Python stores them: those of the
//! Scenario's data made whole here (the stock's ledger, the operational events' state), the others
//! from the table's template with each person's, staff member's and zone's record filled in
//! (`ENTITY_MEMBERS` of `world_export.py`).

use std::collections::BTreeSet;
use std::sync::Arc;

use indexmap::IndexMap;
use serde_json::Value;

use super::{bad, text, vmap, vstr, Scenario, TableEntry};
use crate::entities::AgentRow;
use crate::error::{Error, Result};
use crate::geometry::{zone_center, Pt};
use crate::state::agent_dynamics::POLICY_V1;
use crate::value::V;

/// `RESOURCE_IDS`, in their order.
pub(crate) const RESOURCE_IDS: [&str; 2] = ["water_ml", "food_meal_units"];

/// `ORIENTATION_FACILITY_KINDS`.
const ORIENTATION_KINDS: [&str; 6] = ["intake", "meal_distribution", "medical_support", "quiet_space", "toilet", "water_station"];

/// `H03_EVENT_TYPES`.
const H03_EVENT_TYPES: [&str; 6] = ["resource_delivery", "delivery_delayed", "water_outage", "water_restored", "staff_unavailable", "staff_shift_change"];

/// The namespaces Python stores, in its order: `WorldState.from_scenario`'s, then
/// `initialize_simulator_extensions`'s factories, then the declared states. A name in this list
/// that the table has is made from its template; `resource_ledger_v1`, `operational_events_v1`
/// and `facility_queues_v1` follow the Scenario's data.
const ORDER: [&str; 19] = [
    "resource_ledger_v1",
    "resource_service_v1",
    "operational_events_v1",
    "zone_environment_v1",
    "information_spread_v1",
    "psychological_distress_v1",
    "agent_dynamics_v1",
    "population_dynamics_v1",
    "social_network_v1",
    "staff_dynamics_v1",
    "shared_operations_v1",
    "population_lifecycle_v1",
    "temporary_spaces_v1",
    "staff_operations_v1",
    "body_state_v1",
    "movement_v1",
    "facility_queues_v1",
    "geometry_overlay_v1",
    "population_presence_v1",
];

fn sorted_ids<'a>(ids: impl Iterator<Item = &'a Arc<str>>) -> Vec<Arc<str>> {
    let mut ids: Vec<Arc<str>> = ids.cloned().collect();
    ids.sort_by(|first, second| first.as_bytes().cmp(second.as_bytes()));
    ids.dedup();
    ids
}

fn agent_ids(scenario: &Scenario) -> Vec<Arc<str>> {
    sorted_ids(scenario.agents.iter().map(|agent| &agent.profile.id))
}

/// The agents by ID, in ID order (`sorted(world.agents.items())`); the last of a repeated ID, as a
/// dict keeps it.
fn agents_by_id(scenario: &Scenario) -> Vec<(Arc<str>, &AgentRow)> {
    let mut by_id: IndexMap<Arc<str>, &AgentRow> = IndexMap::new();
    for agent in &scenario.agents {
        by_id.insert(agent.profile.id.clone(), agent);
    }
    by_id.sort_by(|first, _, second, _| first.as_bytes().cmp(second.as_bytes()));
    by_id.into_iter().collect()
}

fn staff_ids(scenario: &Scenario) -> Vec<Arc<str>> {
    sorted_ids(scenario.staff.iter().map(|person| &person.id))
}

fn records(ids: &[Arc<str>], record: impl Fn(&Arc<str>) -> Result<V>) -> Result<V> {
    let mut map = IndexMap::with_capacity(ids.len());
    for id in ids {
        map.insert(id.clone(), record(id)?);
    }
    Ok(V::Map(map))
}

/// The template's members with these values; each member must be in the template, empty.
fn filled(name: &str, template: &V, members: Vec<(&str, V)>) -> Result<V> {
    let V::Map(map) = template else {
        return Err(Error::Value(format!("the table's {name} is not a dict")));
    };
    let mut map = map.clone();
    for (member, value) in members {
        let empty = match map.get(member) {
            Some(V::Map(items)) => items.is_empty(),
            Some(V::List(items)) => items.is_empty(),
            _ => false,
        };
        if !empty {
            return Err(Error::Value(format!("the table's {name} has no empty {member}")));
        }
        map.insert(text(member), value);
    }
    Ok(V::Map(map))
}

fn config<'a>(scenario: &'a Scenario, key: &str) -> Option<&'a V> {
    scenario.configs.get(key)
}

fn member<'a>(value: &'a V, key: &str) -> Option<&'a V> {
    match value {
        V::Map(map) => map.get(key),
        _ => None,
    }
}

// ---- Made from the Scenario's data -----------------------------------------------------------------

/// `create_resource_ledger(initial_stock)`.
fn resource_ledger(stock: &Value) -> Result<V> {
    let warehouse = vstr(super::str_of(&stock["warehouse_id"], "warehouse_id")?);
    let zone = match &stock["zone_id"] {
        Value::Null => V::Null,
        found => vstr(super::str_of(found, "a stock's zone_id")?),
    };
    let mut sources: Vec<Arc<str>> =
        super::array(&stock["source_ids"], "source_ids")?.iter().map(|id| super::str_of(id, "a source ID").map(text)).collect::<Result<_>>()?;
    sources.sort_by(|first, second| first.as_bytes().cmp(second.as_bytes()));
    let sources = V::List(sources.into_iter().map(V::Str).collect());
    let mut accounts = IndexMap::new();
    let mut audit = Vec::new();
    for resource in RESOURCE_IDS {
        let initial = V::Int(super::int_of(&stock[resource], resource)?);
        accounts.insert(
            text(resource),
            vmap([
                ("initial", initial.clone()),
                ("delivered", V::Int(0)),
                ("served", V::Int(0)),
                ("losses", V::Int(0)),
                ("remaining", initial.clone()),
                ("reserved", V::Int(0)),
                ("source_ids", sources.clone()),
                ("reservations", V::Map(IndexMap::new())),
            ]),
        );
        audit.push(vmap([
            ("schema_version", V::Int(1)),
            ("minute", V::Int(0)),
            ("event_id", V::Null),
            ("operation", vstr("initial")),
            ("resource_id", vstr(resource)),
            ("quantity", initial.clone()),
            ("reservation_id", V::Null),
            ("agent_id", V::Null),
            ("source_id", V::Null),
            ("source_ids", sources.clone()),
            ("warehouse_id", warehouse.clone()),
            ("initial", initial.clone()),
            ("delivered", V::Int(0)),
            ("served", V::Int(0)),
            ("losses", V::Int(0)),
            ("remaining", initial),
            ("reserved", V::Int(0)),
        ]));
    }
    Ok(vmap([
        ("schema_version", V::Int(1)),
        ("warehouse_id", warehouse),
        ("zone_id", zone),
        ("accounts", V::Map(accounts)),
        ("used_reservation_ids", V::List(Vec::new())),
        ("audit", V::List(audit)),
    ]))
}

/// `operational_events_enabled(configs, events)`.
fn operational_enabled(scenario: &Scenario) -> Result<bool> {
    let configured = match config(scenario, "operational_events") {
        None => false,
        Some(V::Map(map)) if map.len() == 1 => match map.get("enabled") {
            Some(V::Bool(enabled)) => *enabled,
            _ => return Err(bad("operational_events config must be {'enabled': bool}")),
        },
        Some(_) => return Err(bad("operational_events config must be {'enabled': bool}")),
    };
    Ok(configured
        || scenario.events.iter().any(|event| {
            H03_EVENT_TYPES.contains(&&*event.event_type)
                || (&*event.event_type == "resource_delivery" && member(&event.payload, "shipment_id").is_some())
        }))
}

/// `create_operational_events_state(events, staff_ids)`.
fn operational_events(scenario: &Scenario) -> Result<V> {
    let payload = |event: &crate::events::EventRow, key: &str| member(&event.payload, key).cloned().ok_or_else(|| bad(format_args!("a payload has no {key}")));
    let key = |value: V| match value {
        V::Str(found) => Ok(found),
        _ => Err(Error::unsupported("a payload's ID is not a str")),
    };
    let mut shipments = IndexMap::new();
    let mut restores = IndexMap::new();
    for event in &scenario.events {
        if &*event.event_type == "resource_delivery" && member(&event.payload, "shipment_id").is_some() {
            shipments.insert(
                key(payload(event, "shipment_id")?)?,
                vmap([
                    ("event_id", V::Str(event.event_id.clone())),
                    ("resource_id", payload(event, "resource_id")?),
                    ("due_minute", V::Int(event.due_minute)),
                ]),
            );
        }
        if &*event.event_type == "water_restored" {
            restores.insert(key(payload(event, "outage_id")?)?, event.event_id.clone());
        }
    }
    let mut outages = IndexMap::new();
    for event in scenario.events.iter().filter(|event| &*event.event_type == "water_outage") {
        let outage = key(payload(event, "outage_id")?)?;
        let zone = member(&event.payload, "zone_id");
        let target = match zone {
            Some(found) => found.clone(),
            None => member(&event.payload, "facility_id").cloned().unwrap_or(V::Null),
        };
        let restore = restores.get(&outage).cloned().map(V::Str).unwrap_or(V::Null);
        outages.insert(
            outage,
            vmap([
                ("target_kind", vstr(if zone.is_some() { "zone" } else { "facility" })),
                ("target_id", target),
                ("outage_event_id", V::Str(event.event_id.clone())),
                ("restore_event_id", restore),
                ("active", V::Bool(false)),
            ]),
        );
    }
    let status = records(&staff_ids(scenario), |_| {
        Ok(vmap([
            ("available", V::Bool(true)),
            ("unavailability_event_id", V::Null),
            ("until_minute", V::Null),
            ("restore_event_id", V::Null),
        ]))
    })?;
    Ok(vmap([
        ("schema_version", V::Int(1)),
        ("shipments", V::Map(shipments)),
        ("water_outages", V::Map(outages)),
        ("staff_status", status),
        ("cancelled_staff_task_ids", V::List(Vec::new())),
    ]))
}

/// `independent_queues_required` of the floor plan: a service point whose queue is not its
/// facility's ID, or a facility with two points.
pub(crate) fn independent_queues(geometry: &IndexMap<Arc<str>, V>) -> Result<bool> {
    let Some(V::List(points)) = geometry.get("service_points") else {
        return Err(bad("facility_geometry has no service_points"));
    };
    let mut facilities = BTreeSet::new();
    for point in points {
        let (Some(V::Str(facility)), Some(V::Str(queue))) = (member(point, "facility_id"), member(point, "queue_id")) else {
            return Err(bad("a service point has no facility or queue ID"));
        };
        if queue != facility || !facilities.insert(facility.clone()) {
            return Ok(true);
        }
    }
    Ok(false)
}

// ---- Each person's, staff member's and zone's records ----------------------------------------------

/// `create_psychological_state` of each person's baseline in `psychology.baseline_distress_by_agent`.
fn psychological(scenario: &Scenario) -> Result<Vec<(&'static str, V)>> {
    let baselines = config(scenario, "psychology").and_then(|psychology| member(psychology, "baseline_distress_by_agent"));
    let agents = records(&agent_ids(scenario), |id| {
        let baseline = match baselines.and_then(|map| member(map, id)) {
            None => 0.0,
            Some(V::Int(value)) => *value as f64,
            Some(V::Float(value)) => *value,
            Some(_) => return Err(Error::unsupported("a baseline distress is not a number")),
        };
        if !(0.0..=1.0).contains(&baseline) {
            return Err(Error::Value(format!("baseline distress for {id} must be within 0..1")));
        }
        Ok(vmap([
            ("baseline_distress", V::Float(baseline)),
            ("long_term_distress", V::Float(baseline)),
            ("daily_acute_sum", V::Float(0.0)),
            ("daily_high_distress_minutes", V::Int(0)),
            ("daily_observed_minutes", V::Int(0)),
            ("completed_days", V::Int(0)),
        ]))
    })?;
    Ok(vec![("agents", agents)])
}

/// `create_information_runtime_state(set(world.agents))`.
fn information(scenario: &Scenario) -> Result<Vec<(&'static str, V)>> {
    let ids = agent_ids(scenario);
    Ok(vec![
        ("beliefs", records(&ids, |_| Ok(V::Map(IndexMap::new())))?),
        ("query_pressure", records(&ids, |_| Ok(V::Float(0.0)))?),
    ])
}

/// `create_agent_dynamics_state`: what each person knows (the facilities of their zone and, past
/// v1, those of `ORIENTATION_FACILITY_KINDS`).
fn agent_dynamics(scenario: &Scenario, template: &V) -> Result<Vec<(&'static str, V)>> {
    let oriented = match member(template, "policy_version") {
        Some(V::Str(policy)) => &**policy != POLICY_V1,
        _ => return Err(Error::Value("the table's agent_dynamics_v1 has no policy_version".into())),
    };
    let mut agents = IndexMap::new();
    for (id, agent) in agents_by_id(scenario) {
        let known: BTreeSet<&Arc<str>> = scenario
            .facilities
            .iter()
            .filter(|facility| facility.zone_id == agent.zone_id || (oriented && ORIENTATION_KINDS.contains(&&*facility.kind)))
            .map(|facility| &facility.id)
            .collect();
        agents.insert(
            id,
            vmap([
                ("known_facility_ids", V::List(known.into_iter().cloned().map(V::Str).collect())),
                ("last_action", V::Null),
                ("last_target_id", V::Null),
                ("last_decision_minute", V::Null),
                ("decision_due_minute", V::Int(agent.next_decision_at)),
                ("decision_reason", V::Null),
            ]),
        );
    }
    Ok(vec![("agents", V::Map(agents))])
}

/// `create_population_dynamics_state`: the people's traits as `PopulationSpec` dumps them (the
/// builder's configs are that dump already).
fn population_dynamics(scenario: &Scenario) -> Result<Vec<(&'static str, V)>> {
    let mut traits = IndexMap::new();
    if let Some(population) = config(scenario, "population") {
        let Some(V::Map(people)) = member(population, "people") else {
            return Err(bad("population has no people"));
        };
        for (id, person) in people {
            traits.insert(id.clone(), person.clone());
        }
        traits.sort_by(|first, _, second, _| first.as_bytes().cmp(second.as_bytes()));
    }
    Ok(vec![
        ("initial_agent_ids", V::List(agent_ids(scenario).into_iter().map(V::Str).collect())),
        ("profile_traits", V::Map(traits)),
    ])
}

/// `create_staff_dynamics_state`'s record of each staff member.
fn staff_dynamics(scenario: &Scenario) -> Result<Vec<(&'static str, V)>> {
    let staff = records(&staff_ids(scenario), |_| {
        Ok(vmap([
            ("consecutive_work_minutes", V::Int(0)),
            ("total_work_minutes", V::Int(0)),
            ("total_rest_minutes", V::Int(0)),
            ("fatigue", V::Float(0.0)),
            ("rest_until_minute", V::Int(0)),
            ("last_activity", vstr("off_duty")),
        ]))
    })?;
    Ok(vec![("staff", staff)])
}

/// `create_staff_operations_state`: each zone's sanitation and each staff member idle.
fn staff_operations(scenario: &Scenario) -> Result<Vec<(&'static str, V)>> {
    let zones = records(&sorted_ids(scenario.zones.iter().map(|zone| &zone.id)), |_| {
        Ok(vmap([("sanitation", V::Float(1.0)), ("updated_minute", V::Int(0))]))
    })?;
    let staff = records(&staff_ids(scenario), |_| {
        Ok(vmap([
            ("phase", vstr("idle")),
            ("task_id", V::Null),
            ("task_kind", V::Null),
            ("target_id", V::Null),
            ("target_zone_id", V::Null),
            ("route_zone_ids", V::List(Vec::new())),
            ("route_edge_ids", V::List(Vec::new())),
            ("travel_minutes", V::Int(0)),
            ("started_minute", V::Null),
            ("arrival_minute", V::Null),
            ("travel_due_minute", V::Null),
            ("due_minute", V::Null),
            ("reason", V::Null),
            ("causation_event_ids", V::List(Vec::new())),
        ]))
    })?;
    Ok(vec![("staff", staff), ("zones", zones)])
}

/// `create_body_state`'s record of each person, from their fatigue and stress (0.0 at minute 0).
fn body_state(scenario: &Scenario) -> Result<Vec<(&'static str, V)>> {
    let mut agents = IndexMap::new();
    for (id, agent) in agents_by_id(scenario) {
        let sleep_debt = agent.needs.fatigue * 360.0;
        let uncertainty = 0.25 + agent.stress * 0.5;
        agents.insert(
            id,
            vmap([
                ("sleep_debt_minutes", V::Float(if sleep_debt < 1440.0 { sleep_debt } else { 1440.0 })),
                ("uncertainty", V::Float(if uncertainty < 1.0 { uncertainty } else { 1.0 })),
                ("reassurance", V::Float(0.0)),
                ("discontent", V::Float(agent.stress)),
                ("positive_affect", V::Float(0.5)),
                ("negative_affect", V::Float(agent.stress)),
                ("hygiene_need", V::Float(0.0)),
            ]),
        );
    }
    Ok(vec![("agents", V::Map(agents))])
}

fn number(value: Option<&V>) -> Result<f64> {
    match value {
        Some(V::Int(found)) => Ok(*found as f64),
        Some(V::Float(found)) if found.is_finite() => Ok(*found),
        _ => Err(bad("a point's coordinate is not a number")),
    }
}

/// The center of each zone of the floor plan (`_zone_center`), by zone ID; the first of a
/// repeated ID.
fn zone_centers(scenario: &Scenario) -> Result<IndexMap<Arc<str>, Option<Pt>>> {
    let mut centers = IndexMap::new();
    let Some(geometry) = config(scenario, "facility_geometry") else {
        return Ok(centers);
    };
    let Some(V::List(zones)) = member(geometry, "zones") else {
        return Err(bad("facility_geometry has no zones"));
    };
    for zone in zones {
        let Some(V::Str(id)) = member(zone, "zone_id") else {
            return Err(bad("a floor plan's zone has no zone_id"));
        };
        if centers.contains_key(id) {
            continue;
        }
        let Some(V::List(corners)) = member(zone, "polygon") else {
            return Err(bad("a floor plan's zone has no polygon"));
        };
        let polygon = corners
            .iter()
            .map(|corner| Ok(Pt { x: number(member(corner, "x"))?, y: number(member(corner, "y"))? }))
            .collect::<Result<Vec<Pt>>>()?;
        if polygon.is_empty() {
            return Err(bad("a floor plan's zone has no corners"));
        }
        let center = zone_center(&polygon).map_err(|()| Error::unsupported("a zone's center is not finite"))?;
        centers.insert(id.clone(), center);
    }
    Ok(centers)
}

/// `create_movement_runtime_state`: each person and staff member at their zone's center.
fn movement(scenario: &Scenario) -> Result<Vec<(&'static str, V)>> {
    let centers = zone_centers(scenario)?;
    let position = |zone: &Arc<str>| {
        let point = centers.get(zone).copied().flatten();
        vmap([
            ("zone_id", V::Str(zone.clone())),
            ("point", point.map_or(V::Null, |point| vmap([("x", V::Float(point.x)), ("y", V::Float(point.y))]))),
            ("edge_id", V::Null),
            ("progress", V::Float(0.0)),
            ("position_kind", vstr(if point.is_some() { "physical" } else { "unavailable" })),
        ])
    };
    let positions = V::Map(agents_by_id(scenario).into_iter().map(|(id, agent)| (id, position(&agent.zone_id))).collect());
    let mut staff: IndexMap<Arc<str>, V> = scenario.staff.iter().map(|person| (person.id.clone(), position(&person.zone_id))).collect();
    staff.sort_by(|first, _, second, _| first.as_bytes().cmp(second.as_bytes()));
    Ok(vec![("positions", positions), ("staff_positions", V::Map(staff))])
}

/// The declared presence state's initializer: each person admitted in their zone.
fn presence(scenario: &Scenario) -> Result<Vec<(&'static str, V)>> {
    let people = agents_by_id(scenario)
        .into_iter()
        .map(|(id, agent)| {
            let record = vmap([
                ("agent_id", V::Str(id.clone())),
                ("household_id", agent.profile.group_id.clone().map_or(V::Null, V::Str)),
                ("status", vstr("admitted")),
                ("since_minute", V::Int(0)),
                ("transition_zone_id", V::Str(agent.zone_id.clone())),
                ("return_zone_id", V::Null),
                ("expected_return_minute", V::Null),
                ("archive_path", V::Null),
                ("source_event_ids", V::List(Vec::new())),
            ]);
            (id, record)
        })
        .collect();
    Ok(vec![("people", V::Map(people))])
}

// ---- The namespaces --------------------------------------------------------------------------------

/// Every namespace of the world at minute 0, in the order Python stores them.
pub(crate) fn namespaces(scenario: &Scenario, table: &TableEntry) -> Result<Vec<(String, V)>> {
    for refused in ["infrastructure", "facility_awareness"] {
        if scenario.configs.contains_key(refused) {
            return Err(Error::unsupported(format!("a Scenario with {refused} is not built in the core")));
        }
    }
    if let Some(unknown) = table.namespaces.keys().find(|name| !ORDER.contains(&&***name)) {
        return Err(Error::Value(format!("the table has a namespace the core does not build: {unknown}")));
    }
    let mut made = Vec::new();
    for name in ORDER {
        let state = match name {
            "resource_ledger_v1" => scenario.initial_stock.as_ref().map(resource_ledger).transpose()?,
            "operational_events_v1" => operational_enabled(scenario)?.then(|| operational_events(scenario)).transpose()?,
            // `independent_queues` refused a floor plan that has them (`super::sections`).
            "facility_queues_v1" => None,
            _ => match table.namespaces.get(name) {
                None => None,
                Some(template) => {
                    let members = match name {
                        "psychological_distress_v1" => psychological(scenario)?,
                        "information_spread_v1" => information(scenario)?,
                        "agent_dynamics_v1" => agent_dynamics(scenario, template)?,
                        "population_dynamics_v1" => population_dynamics(scenario)?,
                        "staff_dynamics_v1" => staff_dynamics(scenario)?,
                        "staff_operations_v1" => staff_operations(scenario)?,
                        "body_state_v1" => body_state(scenario)?,
                        "movement_v1" => movement(scenario)?,
                        "population_presence_v1" => presence(scenario)?,
                        _ => Vec::new(),
                    };
                    Some(filled(name, template, members)?)
                }
            },
        };
        if let Some(state) = state {
            made.push((name.to_owned(), state));
        }
    }
    Ok(made)
}
