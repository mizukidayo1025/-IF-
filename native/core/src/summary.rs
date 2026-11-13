//! The summary of a world that the intervention's reading of a text is given (#302, under #292):
//! `scenario_summary_from_world` of `src/sheltersim/llm/natural_language.py`, made from the core's
//! World alone. Its canonical JSON (`V::write_json`: the keys sorted, the floats as Python writes
//! them) is the bytes of `canonical(scenario_summary_from_world(world).model_dump(mode="json"))`,
//! as the server's `intervention-preview` makes it (no default zone and no start date given:
//! `default_zone_id` is the zone of a world with one zone, `start_date` is null).
//!
//! It reads the world and changes nothing, draws nothing and calls no LLM. A value Python would
//! have failed on (a namespace of another shape than its rules write) is `Unsupported`; a summary
//! the contract `ScenarioSummary` refuses (more staff than it lists, an ID that is not a stable
//! ID, ...) is a `Value` error, as Python's `ValidationError` is a `ValueError`. The texts are the
//! core's own, not pydantic's.

use std::collections::BTreeSet;
use std::sync::Arc;

use indexmap::IndexMap;

use crate::entities::Entities;
use crate::error::{Error, Result};
use crate::row::ValueRow;
use crate::state::resource_ledger::{self, LedgerRow};
use crate::value::V;
use crate::world::{Place, World, MODEL_CONFIGS};

const OPERATIONAL: &str = "operational_events_v1";
const LIFECYCLE: &str = "population_lifecycle_v1";
const PRESENCE: &str = "population_presence_v1";
/// The policies the summary names when their namespace is in `model_state` (`zone_environment_v1`
/// is both the namespace and the policy).
const OPT_IN: [&str; 6] = [resource_ledger::NAME, OPERATIONAL, "population_dynamics_v1", "geometry_overlay_v1", "resource_service_v1", "zone_environment_v1"];
/// The structural handlers every world has.
const CORE_POLICIES: [&str; 2] = ["facility_capacity", "staff_operations"];

/// How many people and routes the summary lists at most (`[:10000]`, `[:2000]`).
const PERSON_LIMIT: usize = 10_000;
const ROUTE_LIMIT: usize = 2000;
/// The contract's bounds (`llm/schemas.py`).
const MAX_DURATION: i64 = 60 * 24 * 366;
const MAX_COUNT: i64 = 10_000;
const MAX_UNITS: i64 = 1000;
const MAX_FACILITIES: usize = 200;
const MAX_ZONES: usize = 100;
const MAX_STAFF: usize = 100;
const MAX_RESOURCES: usize = 10;
const MAX_SHIPMENTS: usize = 500;
const MAX_POLICIES: usize = 50;
const MAX_NAME: usize = 120;

/// The fields of `ScenarioZoneEnvironmentConfig` and of its zones' baselines, and which are floats.
const ENVIRONMENT_FIELDS: [(&str, bool); 17] = [
    ("schema_version", false),
    ("policy_version", false),
    ("source_type", false),
    ("source_id", false),
    ("noise_rest_efficiency_loss", true),
    ("min_probability_multiplier", true),
    ("max_probability_multiplier", true),
    ("min_rest_decision_minutes", false),
    ("max_rest_decision_minutes", false),
    ("max_active_modifiers_per_zone", false),
    ("lighting_sleep_efficiency_loss", true),
    ("ventilation_hygiene_growth_per_minute", true),
    ("privacy_discontent_per_minute", true),
    ("temperature_stress_per_degree_minute", true),
    ("comfortable_min_temperature_c", true),
    ("comfortable_max_temperature_c", true),
    ("baseline_by_zone", false),
];
const BASELINE_FIELDS: [&str; 4] = ["ventilation_effectiveness", "lighting_level", "privacy_level", "temperature_c"];

type Map = IndexMap<Arc<str>, V>;

/// The summary's canonical JSON.
pub fn scenario_summary(world: &World) -> Result<Vec<u8>> {
    summary(world)?.to_json()
}

/// The summary as a value: `ScenarioSummary.model_dump(mode="json")`.
pub fn summary(world: &World) -> Result<V> {
    let rows = world.rows()?;
    if let Some(taint) = rows.tainted().or(rows.zones_tainted()) {
        return Err(Error::unsupported(format!("the world's tables are tainted: {taint}")));
    }
    let operational = namespace(world, OPERATIONAL)?;
    let operational = match &operational {
        Some(V::Map(map)) => Some(map),
        _ => None,
    };

    let staff_status = match operational.and_then(|map| map.get("staff_status")) {
        None => None,
        Some(V::Map(status)) => Some(status),
        Some(_) => return Err(Error::unsupported("the staff status is not a dict")),
    };
    let staff_ids = sorted(rows.staff.iter().map(|(id, _)| id.clone()));
    let mut available = Vec::new();
    for id in &staff_ids {
        let free = match staff_status.and_then(|status| status.get(id)) {
            None => true,
            Some(V::Map(record)) => record.get("available").is_some_and(V::is_truthy),
            Some(_) => return Err(Error::unsupported("a staff member's status is not a dict")),
        };
        if free {
            available.push(id.clone());
        }
    }

    let shipments = pending_shipments(world, operational)?;
    let (resource_ids, warehouse_id) = ledger(world)?;

    let mut policies: BTreeSet<&str> = CORE_POLICIES.into_iter().collect();
    for name in OPT_IN {
        if has(world, name)? {
            policies.insert(name);
        }
    }
    if has(world, LIFECYCLE)? && has(world, PRESENCE)? {
        policies.insert(LIFECYCLE);
        policies.insert(PRESENCE);
    }

    let zones = zones(&rows)?;
    let default_zone_id = if zones.len() == 1 { V::Str(zones[0].0.clone()) } else { V::Null };
    let facilities = facilities(&rows)?;
    if facilities.iter().any(|(zone, _)| !zones.iter().any(|(id, _)| id == zone)) {
        return Err(Error::Value("facility zone_id must reference a listed zone".to_owned()));
    }
    let people = people(world, &rows)?;
    let routes = sorted(rows.zone_edges.iter().map(|(id, _)| id.clone()));
    let configs = world.slots.try_row::<ValueRow>(MODEL_CONFIGS)?;
    let environment = match &configs.0 {
        V::Map(configs) => match configs.get("zone_environment") {
            Some(config) => environment(config)?,
            None => V::Null,
        },
        _ => return Err(Error::unsupported("the model configs are not a dict")),
    };

    let duration = world.duration_minutes()?;
    let mut summary = Map::new();
    let mut put = |name: &str, value: V| {
        summary.insert(Arc::from(name), value);
    };
    put("duration_minutes", V::Int(duration));
    put("start_date", V::Null);
    put("default_zone_id", default_zone_id);
    put("agent_count", int(rows.agents.len()));
    put("facilities", V::List(facilities.into_iter().map(|(_, facility)| facility).collect()));
    put("zones", V::List(zones.iter().map(|(_, zone)| zone.clone()).collect()));
    put("staff_ids", strings(&staff_ids));
    put("available_staff_ids", strings(&available));
    put("resource_ids", strings(&resource_ids));
    put("warehouse_id", warehouse_id.clone().map_or(V::Null, V::Str));
    put("pending_shipments", V::List(shipments.iter().map(|(_, shipment)| shipment.clone()).collect()));
    put("available_policies", V::List(policies.iter().map(|name| V::Str(Arc::from(*name))).collect()));
    put("person_ids", strings(&people[..people.len().min(PERSON_LIMIT)]));
    put("route_ids", strings(&routes[..routes.len().min(ROUTE_LIMIT)]));
    put("person_targets_truncated", V::Bool(people.len() > PERSON_LIMIT));
    put("zone_environment_settings", environment);

    check(&Checked {
        duration,
        agents: rows.agents.len(),
        zones: &zones,
        facilities: rows.facilities.len(),
        staff: &staff_ids,
        resources: &resource_ids,
        warehouse: warehouse_id.as_deref(),
        shipments: &shipments,
        policies: policies.len(),
        people: &people[..people.len().min(PERSON_LIMIT)],
        routes: &routes[..routes.len().min(ROUTE_LIMIT)],
    })?;
    Ok(V::Map(summary))
}

// ---- The parts ------------------------------------------------------------------------------------

/// Whether `model_state` has the key.
fn has(world: &World, name: &str) -> Result<bool> {
    Ok(match world.slots.get(name) {
        Some(slot) => {
            let slot = slot.try_borrow()?;
            slot.place != Place::Field && (slot.row.is_some() || slot.taint.is_some())
        }
        None => false,
    })
}

/// `model_state.get(name)` of a namespace held as a value; None without the key. A tainted one, or
/// one held as a typed row, is refused: the summary reads none such.
fn namespace(world: &World, name: &str) -> Result<Option<V>> {
    if !has(world, name)? {
        return Ok(None);
    }
    let slot = world.slots.get(name).ok_or_else(|| Error::Runtime("a slot left the world".to_owned()))?.try_borrow()?;
    if let Some(taint) = slot.taint.as_deref().filter(|_| slot.row.is_none()) {
        return Err(Error::unsupported(format!("{name} is tainted: {taint}")));
    }
    Ok(Some(slot.row::<ValueRow>()?.0.clone()))
}

/// The resources the ledger accounts for, sorted, and its warehouse: `read_only_part` of
/// `accounts` (a dict's keys, else none) and of `warehouse_id`.
fn ledger(world: &World) -> Result<(Vec<Arc<str>>, Option<Arc<str>>)> {
    if !has(world, resource_ledger::NAME)? {
        return Ok((Vec::new(), None));
    }
    let slot = world.slots.get(resource_ledger::NAME).ok_or_else(|| Error::Runtime("a slot left the world".to_owned()))?.try_borrow()?;
    if let Ok(row) = slot.row::<LedgerRow>() {
        // The typed row's accounts are these two, by their names.
        return Ok((sorted(resource_ledger::RESOURCE_IDS.iter().map(|id| Arc::from(*id))), Some(row.warehouse_id.clone())));
    }
    drop(slot);
    let Some(V::Map(ledger)) = namespace(world, resource_ledger::NAME)? else {
        return Ok((Vec::new(), None));
    };
    let resources = match ledger.get("accounts") {
        Some(V::Map(accounts)) => sorted(accounts.keys().cloned()),
        _ => Vec::new(),
    };
    let warehouse = match ledger.get("warehouse_id") {
        None | Some(V::Null) => None,
        Some(V::Str(id)) => Some(id.clone()),
        Some(_) => return Err(Error::Value("scenario summary: the warehouse_id is not a string".to_owned())),
    };
    Ok((resources, warehouse))
}

/// The shipments whose delivery is still pending, by their IDs: `(id, ShipmentTarget)`.
fn pending_shipments(world: &World, operational: Option<&Map>) -> Result<Vec<(Arc<str>, V)>> {
    let shipments = match operational.and_then(|map| map.get("shipments")) {
        None => return Ok(Vec::new()),
        Some(V::Map(shipments)) => shipments,
        Some(_) => return Err(Error::unsupported("the shipments are not a dict")),
    };
    let queue = world.queue()?;
    let mut found = Vec::new();
    let mut ids: Vec<&Arc<str>> = shipments.keys().collect();
    ids.sort_by(|first, second| first.as_bytes().cmp(second.as_bytes()));
    for id in ids {
        let V::Map(record) = &shipments[id] else { continue };
        let event_id = match record.get("event_id") {
            Some(V::Str(event_id)) => event_id,
            Some(V::List(_) | V::Map(_)) => return Err(Error::unsupported("a shipment's event_id is not hashable")),
            _ => continue,
        };
        let Some(serial) = queue.serial_of(event_id) else { continue };
        let event = queue.slot(serial).row.clone().ok_or_else(|| Error::unsupported("a pending event the core did not read"))?;
        if &*event.event_type != "resource_delivery" {
            continue;
        }
        let resource = match record.get("resource_id") {
            Some(resource) => Some(resource),
            None => match &event.payload {
                V::Map(payload) => payload.get("resource_id"),
                _ => return Err(Error::unsupported("an event's payload is not a dict")),
            },
        };
        let Some(V::Str(resource)) = resource else { continue };
        if !stable_id(resource) {
            return Err(Error::Value(format!("scenario summary: the shipment {id} names a resource that is not a stable ID")));
        }
        let mut shipment = Map::new();
        shipment.insert(Arc::from("id"), V::Str(id.clone()));
        shipment.insert(Arc::from("resource_id"), V::Str(resource.clone()));
        shipment.insert(Arc::from("due_minute"), V::Int(event.due_minute));
        found.push((id.clone(), V::Map(shipment)));
    }
    Ok(found)
}

/// The zones by ID: `(id, ZoneTarget)`.
fn zones(rows: &Entities) -> Result<Vec<(Arc<str>, V)>> {
    let mut ids: Vec<&Arc<str>> = rows.zones.iter().map(|(id, _)| id).collect();
    ids.sort_by(|first, second| first.as_bytes().cmp(second.as_bytes()));
    let mut found = Vec::with_capacity(ids.len());
    for id in ids {
        let zone = rows.zones.get(id).ok_or_else(|| Error::Runtime("a zone left the table".to_owned()))?;
        let water = rows.facilities.iter().any(|(_, facility)| &*facility.kind == "water_station" && facility.zone_id == zone.id);
        let staff = rows.staff.iter().filter(|(_, member)| member.zone_id == zone.id).count();
        let evacuees = rows.agents.iter().filter(|(_, agent)| agent.zone_id == zone.id).count();
        let chars = zone.name.chars().count();
        if chars == 0 || chars > MAX_NAME {
            return Err(Error::Value(format!("scenario summary: the name of zone {id} is not 1 to {MAX_NAME} characters")));
        }
        if staff as i64 > MAX_COUNT || evacuees as i64 > MAX_COUNT {
            return Err(Error::Value(format!("scenario summary: zone {id} holds more than {MAX_COUNT} people")));
        }
        let mut target = Map::new();
        target.insert(Arc::from("id"), V::Str(zone.id.clone()));
        target.insert(Arc::from("name"), V::Str(zone.name.clone()));
        target.insert(Arc::from("has_water_station"), V::Bool(water));
        target.insert(Arc::from("staff_count"), int(staff));
        target.insert(Arc::from("evacuee_count"), int(evacuees));
        found.push((zone.id.clone(), V::Map(target)));
    }
    Ok(found)
}

/// The facilities by ID: `(id, FacilityTarget)`.
fn facilities(rows: &Entities) -> Result<Vec<(Arc<str>, V)>> {
    let mut ids: Vec<&Arc<str>> = rows.facilities.iter().map(|(id, _)| id).collect();
    ids.sort_by(|first, second| first.as_bytes().cmp(second.as_bytes()));
    let mut found = Vec::with_capacity(ids.len());
    for id in ids {
        let facility = rows.facilities.get(id).ok_or_else(|| Error::Runtime("a facility left the table".to_owned()))?;
        let occupancy = (facility.active_agent_ids.len() + facility.resource_active_agent_ids.len()) as i64;
        let (capacity, working) = (facility.capacity, facility.working_units);
        if !(0..=MAX_UNITS).contains(&capacity) || !(0..=MAX_UNITS).contains(&working) || occupancy > MAX_UNITS {
            return Err(Error::Value(format!("scenario summary: facility {id} has more than {MAX_UNITS} units or a negative count")));
        }
        if working > capacity {
            return Err(Error::Value("working_units cannot exceed facility capacity".to_owned()));
        }
        if occupancy > working {
            return Err(Error::Value("active_occupancy cannot exceed working_units".to_owned()));
        }
        if !stable_id(&facility.id) || !stable_id(&facility.kind) || !stable_id(&facility.zone_id) {
            return Err(Error::Value(format!("scenario summary: facility {id} has an ID, kind or zone that is not a stable ID")));
        }
        let mut target = Map::new();
        target.insert(Arc::from("id"), V::Str(facility.id.clone()));
        target.insert(Arc::from("name"), V::Str(facility.id.clone()));
        target.insert(Arc::from("kind"), V::Str(facility.kind.clone()));
        target.insert(Arc::from("zone_id"), V::Str(facility.zone_id.clone()));
        target.insert(Arc::from("capacity"), V::Int(capacity));
        target.insert(Arc::from("working_units"), V::Int(working));
        target.insert(Arc::from("active_occupancy"), V::Int(occupancy));
        found.push((facility.zone_id.clone(), V::Map(target)));
    }
    Ok(found)
}

/// The people the world has and those the population's lifecycle keeps (who left, who is still
/// to come), sorted: `set(world.agents) | set(read_only_part(world, LIFECYCLE, "people", {}))`.
fn people(world: &World, rows: &Entities) -> Result<Vec<Arc<str>>> {
    let mut ids: BTreeSet<Arc<str>> = rows.agents.iter().map(|(id, _)| id.clone()).collect();
    if let Some(V::Map(lifecycle)) = namespace(world, LIFECYCLE)? {
        match lifecycle.get("people") {
            None => {}
            Some(V::Map(people)) => ids.extend(people.keys().cloned()),
            Some(V::List(people)) => {
                for person in people {
                    match person {
                        V::Str(id) => {
                            ids.insert(id.clone());
                        }
                        _ => return Err(Error::unsupported("a person of the lifecycle is not a string")),
                    }
                }
            }
            Some(_) => return Err(Error::unsupported("the lifecycle's people are not a dict")),
        }
    }
    Ok(ids.into_iter().collect())
}

/// `ScenarioZoneEnvironmentConfig.model_validate(config).model_dump(mode="json")` of a config in
/// the form a Scenario stores it (its own dump: every field, `ScenarioSpec`'s canonical configs).
/// An int where a float is is the float pydantic makes of it. Any other form is refused: the core
/// does not repeat the contract's defaults and checks.
fn environment(config: &V) -> Result<V> {
    let refused = || Error::unsupported("a zone_environment config not in the form a Scenario stores");
    let V::Map(config) = config else { return Err(refused()) };
    if config.len() != ENVIRONMENT_FIELDS.len() {
        return Err(refused());
    }
    let mut dumped = Map::new();
    for (name, is_float) in ENVIRONMENT_FIELDS {
        let value = config.get(name).ok_or_else(refused)?;
        let value = if name == "baseline_by_zone" {
            let V::Map(zones) = value else { return Err(refused()) };
            let mut baselines = Map::new();
            for (zone, baseline) in zones {
                let V::Map(baseline) = baseline else { return Err(refused()) };
                if baseline.len() != BASELINE_FIELDS.len() {
                    return Err(refused());
                }
                let mut fields = Map::new();
                for field in BASELINE_FIELDS {
                    fields.insert(Arc::from(field), float(baseline.get(field).ok_or_else(refused)?).ok_or_else(refused)?);
                }
                baselines.insert(zone.clone(), V::Map(fields));
            }
            V::Map(baselines)
        } else if is_float {
            float(value).ok_or_else(refused)?
        } else {
            value.clone()
        };
        dumped.insert(Arc::from(name), value);
    }
    Ok(V::Map(dumped))
}

fn float(value: &V) -> Option<V> {
    match value {
        V::Float(number) => Some(V::Float(*number)),
        V::Int(number) => Some(V::Float(*number as f64)),
        _ => None,
    }
}

// ---- The contract's checks ----------------------------------------------------------------------

struct Checked<'a> {
    duration: i64,
    agents: usize,
    zones: &'a [(Arc<str>, V)],
    facilities: usize,
    staff: &'a [Arc<str>],
    resources: &'a [Arc<str>],
    warehouse: Option<&'a str>,
    shipments: &'a [(Arc<str>, V)],
    policies: usize,
    people: &'a [Arc<str>],
    routes: &'a [Arc<str>],
}

/// What `ScenarioSummary` checks of the fields the facilities' and zones' checks did not.
fn check(summary: &Checked<'_>) -> Result<()> {
    let refuse = |what: String| Err(Error::Value(format!("scenario summary: {what}")));
    if !(1..=MAX_DURATION).contains(&summary.duration) {
        return refuse(format!("the duration is not 1 to {MAX_DURATION} minutes"));
    }
    if summary.agents as i64 > MAX_COUNT {
        return refuse(format!("more than {MAX_COUNT} people"));
    }
    let lengths = [
        ("facilities", summary.facilities, MAX_FACILITIES),
        ("zones", summary.zones.len(), MAX_ZONES),
        ("staff", summary.staff.len(), MAX_STAFF),
        ("resources", summary.resources.len(), MAX_RESOURCES),
        ("pending shipments", summary.shipments.len(), MAX_SHIPMENTS),
        ("policies", summary.policies, MAX_POLICIES),
    ];
    for (what, count, limit) in lengths {
        if count > limit {
            return refuse(format!("more than {limit} {what}"));
        }
    }
    let ids = summary
        .zones
        .iter()
        .map(|(id, _)| id)
        .chain(summary.staff)
        .chain(summary.resources)
        .chain(summary.shipments.iter().map(|(id, _)| id))
        .chain(summary.people)
        .chain(summary.routes)
        .map(|id| &**id)
        .chain(summary.warehouse);
    for id in ids {
        if !stable_id(id) {
            return refuse(format!("{id:?} is not a stable ID"));
        }
    }
    if !summary.resources.is_empty() && summary.warehouse.is_none() {
        return Err(Error::Value("resource targets require a warehouse_id".to_owned()));
    }
    let due = |shipment: &V| match shipment {
        V::Map(map) => matches!(map.get("due_minute"), Some(V::Int(minute)) if *minute >= summary.duration || *minute < 0),
        _ => false,
    };
    if summary.shipments.iter().any(|(_, shipment)| due(shipment)) {
        return Err(Error::Value("pending shipment minutes must be within the scenario".to_owned()));
    }
    Ok(())
}

/// `StableId`: 1 to 64 characters, a letter or digit of ASCII first, then those or `._:-`.
fn stable_id(text: &str) -> bool {
    let bytes = text.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && bytes[0].is_ascii_alphanumeric()
        && bytes.iter().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

// ---- Values -----------------------------------------------------------------------------------------

fn int(value: usize) -> V {
    V::Int(value as i64)
}

fn strings(ids: &[Arc<str>]) -> V {
    V::List(ids.iter().cloned().map(V::Str).collect())
}

/// The IDs sorted as Python sorts str: by code point, which is the order of their UTF-8 bytes.
fn sorted(ids: impl Iterator<Item = Arc<str>>) -> Vec<Arc<str>> {
    ids.collect::<BTreeSet<_>>().into_iter().collect()
}
