//! The world of a Scenario at minute 0, built in the core (Issue #249, stage 6 of #132; for the
//! browser's Scenario screen, #285).
//!
//! `entry` takes a Scenario's canonical JSON (`ScenarioSpec.canonical_json()`) and gives the bytes
//! the Python side writes of the world a Run starts from: `export_world(world, controller)` of the
//! world `WorldState.from_scenario` made and `OfflineSimulationController` set up (as
//! `tests/support/core_fixtures.py`'s `run_world` does), written with sorted keys and no spaces
//! (`scripts/build_browser_sample.py`'s `export.json`). The loader (`crate::load::loaded`) and the
//! browser's `Simulation` take those bytes as they take Python's.
//!
//! The core computes what depends on the Scenario's own data: the rows, the event queue, the
//! stock's ledger, the operational events' state, each person's, staff member's and zone's record
//! in the namespaces (`namespaces`), the floor plan's sections and the zone neighbors. What Python
//! derives from the other configs (the Run's `policies_document`, the sections its contracts
//! parse, the rest of each namespace) it reads from a table Python writes (`table.json`, by
//! `tests/support/core_initial.py`), keyed by a fingerprint of the configs that leaves out
//! `DATA_CONFIGS` but that they are there (`sheltersim.core.world_export.configs_fingerprint`).
//! A Scenario whose fingerprint the table lacks is refused (`Unsupported`), as is one with what the
//! Scenario screen's builder never makes (an infrastructure config, facility awareness, arrival
//! approach routes, independent service-point queues).
//!
//! The Scenario is untrusted input: what is not a Scenario's canonical JSON is an error, never a
//! panic. Like the browser's builder that makes it (#291), this does not run `WorldState.validate`:
//! the Python tests check that the builder's Scenarios give Python's bytes.

mod namespaces;
mod records;

pub use records::{evidence, model_versions};

use std::sync::{Arc, OnceLock};

use indexmap::IndexMap;
use serde_json::Value;

use crate::checkpoint::sha256_hex;
use crate::entities::{Activity, AgentRow, EdgeRow, FacilityRow, Mobility, NeedRow, ProfileRow, Skills, StaffRow, Table, ZoneRow};
use crate::error::{Error, Result};
use crate::events::log::EventLog;
use crate::events::queue::Queue;
use crate::events::{EventKind, EventRow};
use crate::json::Encoder;
use crate::load;
use crate::row::{IdMap, Row, StrEnum, ValueRow};
use crate::value::V;
use crate::world::{self, Fit, Place, World};

/// The configs whose contents are the Scenario's own data (`DATA_CONFIGS` of `world_export.py`).
pub const DATA_CONFIGS: [&str; 4] = ["facility_geometry", "population", "population_context", "spatial_constraints"];

/// The namespaces whose row the core's world holds, then the owned fields (`OWNED_NAMESPACES` and
/// `OWNED_FIELDS` of `core/state_mirror.py`), in the order a Python world registers their slots.
const OWNED_NAMESPACES: [&str; 19] = [
    "psychological_distress_v1",
    "staff_dynamics_v1",
    "information_spread_v1",
    "social_propagation_v1",
    "movement_v1",
    "body_state_v1",
    "social_network_v1",
    "agent_dynamics_v1",
    "staff_operations_v1",
    "shared_operations_v1",
    "resource_ledger_v1",
    "population_presence_v1",
    "population_lifecycle_v1",
    "population_dynamics_v1",
    "temporary_spaces_v1",
    "geometry_overlay_v1",
    "resource_service_v1",
    "effect_program_v1",
    "facility_queues_v1",
];
const OWNED_FIELDS: [&str; 4] = [world::METRIC_STATE, world::MODEL_CONFIGS, world::RESOURCES, world::ID_COUNTERS];

/// The error of input that is not a Scenario's canonical JSON.
fn bad(what: impl std::fmt::Display) -> Error {
    Error::Value(format!("not a Scenario: {what}"))
}

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

fn str_of<'a>(json: &'a Value, what: &str) -> Result<&'a str> {
    json.as_str().ok_or_else(|| bad(format_args!("{what} is not a str")))
}

fn opt_str(json: &Value, what: &str) -> Result<Option<Arc<str>>> {
    match json {
        Value::Null => Ok(None),
        _ => str_of(json, what).map(|found| Some(text(found))),
    }
}

fn int_of(json: &Value, what: &str) -> Result<i64> {
    json.as_i64().ok_or_else(|| bad(format_args!("{what} is not an int")))
}

fn float_of(json: &Value, what: &str) -> Result<f64> {
    json.as_f64().filter(|value| value.is_finite()).ok_or_else(|| bad(format_args!("{what} is not a number")))
}

fn array<'a>(json: &'a Value, what: &str) -> Result<&'a Vec<Value>> {
    json.as_array().ok_or_else(|| bad(format_args!("{what} is not a list")))
}

fn value_of(json: &Value) -> Result<V> {
    V::from_json(json).ok_or_else(|| bad("a number Python does not read"))
}

fn map_of(json: &Value, what: &str) -> Result<IndexMap<Arc<str>, V>> {
    match value_of(json)? {
        V::Map(map) => Ok(map),
        _ => Err(bad(format_args!("{what} is not a dict"))),
    }
}

/// A map with these items, in this order.
pub(crate) fn vmap<const N: usize>(items: [(&str, V); N]) -> V {
    V::Map(items.into_iter().map(|(key, value)| (text(key), value)).collect())
}

pub(crate) fn vstr(value: &str) -> V {
    V::Str(text(value))
}

/// The canonical JSON of a value (`V::write_json`).
fn canonical(value: &V) -> Result<Vec<u8>> {
    let mut encoder = Encoder::with_capacity(1024);
    value.write_json(&mut encoder)?;
    Ok(encoder.out)
}

// ---- The table -----------------------------------------------------------------------------------

/// What Python derives for one fingerprint (`world_export.initial_table_entry`).
pub(crate) struct TableEntry {
    pub policies: V,
    pub route_capacities: V,
    pub sections: IndexMap<Arc<str>, V>,
    pub namespaces: IndexMap<Arc<str>, V>,
    /// `model_signatures(configs)` but the service points' signature.
    pub model_signatures: IndexMap<Arc<str>, V>,
    /// The evidence route's answer, without the Scenario's own parts.
    pub evidence: IndexMap<Arc<str>, V>,
}

/// The table's version that this reader takes.
const TABLE_SCHEMA_VERSION: i64 = 2;

fn read_table() -> std::result::Result<IndexMap<Arc<str>, Arc<TableEntry>>, String> {
    let json: Value = serde_json::from_str(include_str!("table.json")).map_err(|error| error.to_string())?;
    if json["schema_version"].as_i64() != Some(TABLE_SCHEMA_VERSION) {
        return Err("the table has another schema_version".into());
    }
    let entries = json["entries"].as_object().ok_or("the table has no entries")?;
    let Some(V::Map(shared)) = V::from_json(&json["shared_evidence"]) else {
        return Err("the table has no shared evidence".into());
    };
    let mut table = IndexMap::new();
    for (fingerprint, entry) in entries {
        let map = |key: &str| match V::from_json(&entry[key]) {
            Some(V::Map(map)) => Ok(map),
            _ => Err(format!("an entry's {key} is not a dict")),
        };
        let made = TableEntry {
            policies: V::from_json(&entry["policies"]).ok_or("an entry's policies")?,
            route_capacities: V::from_json(&entry["route_capacities"]).ok_or("an entry's route capacities")?,
            sections: map("sections")?,
            namespaces: map("namespaces")?,
            model_signatures: map("model_signatures")?,
            evidence: {
                let mut evidence = map("evidence")?;
                for (key, value) in &shared {
                    if evidence.insert(key.clone(), value.clone()).is_some() {
                        return Err(format!("an entry's evidence has the shared {key}"));
                    }
                }
                evidence
            },
        };
        table.insert(text(fingerprint), Arc::new(made));
    }
    Ok(table)
}

/// The table's entry of these configs, or `Unsupported`.
pub(crate) fn entry_of(configs: &IndexMap<Arc<str>, V>) -> Result<Arc<TableEntry>> {
    let fingerprint = fingerprint(configs)?;
    table()?.get(fingerprint.as_str()).cloned().ok_or_else(|| {
        Error::unsupported(format!("the core's table has no entry for these configs (fingerprint {fingerprint})"))
    })
}

fn table() -> Result<&'static IndexMap<Arc<str>, Arc<TableEntry>>> {
    static TABLE: OnceLock<std::result::Result<IndexMap<Arc<str>, Arc<TableEntry>>, String>> = OnceLock::new();
    TABLE.get_or_init(read_table).as_ref().map_err(|why| Error::Value(format!("the core's table of minute-0 worlds: {why}")))
}

/// `configs_fingerprint`: the sha256 of the canonical JSON of the configs, each of `DATA_CONFIGS`
/// there as `true`.
pub fn fingerprint(configs: &IndexMap<Arc<str>, V>) -> Result<String> {
    let masked: IndexMap<Arc<str>, V> = configs
        .iter()
        .map(|(key, value)| (key.clone(), if DATA_CONFIGS.contains(&&**key) { V::Bool(true) } else { value.clone() }))
        .collect();
    Ok(sha256_hex(&canonical(&V::Map(masked))?))
}

/// Whether the core's table has an entry for a Scenario's `model_configs` (JSON): whether `entry`
/// can build a Scenario with them.
pub fn knows(configs: &Value) -> Result<bool> {
    Ok(table()?.contains_key(fingerprint(&map_of(configs, "model_configs")?)?.as_str()))
}

// ---- The rows --------------------------------------------------------------------------------------

/// The Scenario's parts the world is made of, read.
pub(crate) struct Scenario {
    pub id: Arc<str>,
    pub seed: Value,
    pub duration: i64,
    pub configs: IndexMap<Arc<str>, V>,
    pub initial_stock: Option<Value>,
    pub agents: Vec<AgentRow>,
    pub facilities: Vec<FacilityRow>,
    pub staff: Vec<StaffRow>,
    pub zones: Vec<ZoneRow>,
    pub edges: Vec<EdgeRow>,
    pub events: Vec<EventRow>,
}

fn needs(json: &Value) -> Result<NeedRow> {
    Ok(NeedRow {
        toilet_urgency: float_of(&json["toilet_urgency"], "toilet_urgency")?,
        hunger: float_of(&json["hunger"], "hunger")?,
        fatigue: float_of(&json["fatigue"], "fatigue")?,
        hydration_need: match json.get("hydration_need") {
            Some(found) => float_of(found, "hydration_need")?,
            None => 0.0,
        },
    })
}

/// `AgentState(profile=AgentProfile(...), zone_id=initial_zone_id, needs=NeedState(...))`.
fn agent(json: &Value) -> Result<AgentRow> {
    let initial_needs = needs(&json["initial_needs"])?;
    let zone = text(str_of(&json["initial_zone_id"], "initial_zone_id")?);
    let mobility = Mobility::from_text(str_of(&json["mobility"], "mobility")?).ok_or_else(|| bad("mobility is not a member"))?;
    Ok(AgentRow {
        profile: ProfileRow {
            id: text(str_of(&json["id"], "a profile's id")?),
            group_id: opt_str(&json["group_id"], "group_id")?,
            initial_zone_id: zone.clone(),
            mobility,
            support_need: float_of(&json["support_need"], "support_need")?,
            stress_sensitivity: float_of(&json["stress_sensitivity"], "stress_sensitivity")?,
            rng_key: text(str_of(&json["rng_key"], "rng_key")?),
            initial_needs: initial_needs.clone(),
            provenance: V::Map(map_of(&json["provenance"], "a profile's provenance")?),
        },
        zone_id: zone,
        activity: Activity::Idle,
        target_id: None,
        needs: initial_needs,
        stress: 0.0,
        trust: 0.5,
        next_decision_at: 0,
        support_request_id: None,
        support_cooldown_until: 0,
    })
}

fn facility(json: &Value) -> Result<FacilityRow> {
    let capacity = int_of(&json["capacity"], "capacity")?;
    let working_units = match json.get("working_units") {
        None | Some(Value::Null) => capacity,
        Some(found) => int_of(found, "working_units")?,
    };
    Ok(FacilityRow {
        id: text(str_of(&json["id"], "a facility's id")?),
        kind: text(str_of(&json["kind"], "kind")?),
        zone_id: text(str_of(&json["zone_id"], "a facility's zone_id")?),
        capacity,
        working_units,
        active_agent_ids: Vec::new(),
        queue_agent_ids: Vec::new(),
        resource_active_agent_ids: Vec::new(),
        resource_queue_agent_ids: Vec::new(),
    })
}

fn staff(json: &Value) -> Result<StaffRow> {
    let skills = array(&json["skills"], "skills")?.iter().map(|skill| str_of(skill, "a skill").map(text)).collect::<Result<_>>()?;
    Ok(StaffRow {
        id: text(str_of(&json["id"], "a staff member's id")?),
        zone_id: text(str_of(&json["zone_id"], "a staff member's zone_id")?),
        skills: Skills(skills),
        work_queue: Vec::new(),
        busy_until: 0,
        active_task_id: None,
    })
}

fn zone(json: &Value) -> Result<ZoneRow> {
    let display = |key: &str| match &json[key] {
        Value::Null => Ok(None),
        found => float_of(found, key).map(Some),
    };
    Ok(ZoneRow {
        id: text(str_of(&json["id"], "a zone's id")?),
        name: text(str_of(&json["name"], "a zone's name")?),
        capacity: match &json["capacity"] {
            Value::Null => None,
            found => Some(int_of(found, "a zone's capacity")?),
        },
        display_x: display("display_x")?,
        display_y: display("display_y")?,
    })
}

fn edge(json: &Value) -> Result<EdgeRow> {
    Ok(EdgeRow {
        id: text(str_of(&json["id"], "an edge's id")?),
        from_zone_id: text(str_of(&json["from_zone_id"], "from_zone_id")?),
        to_zone_id: text(str_of(&json["to_zone_id"], "to_zone_id")?),
        travel_minutes: int_of(&json["travel_minutes"], "travel_minutes")?,
        closed: json["closed"].as_bool().ok_or_else(|| bad("closed is not a bool"))?,
    })
}

/// A planned event as `ScheduledEvent`, with its checks.
fn event(json: &Value) -> Result<EventRow> {
    let kind = EventKind::parse(str_of(&json["kind"], "kind")?).ok_or_else(|| bad("kind is not an event kind"))?;
    EventRow::new(
        text(str_of(&json["id"], "an event's id")?),
        kind,
        int_of(&json["due_minute"], "due_minute")?,
        int_of(&json["priority"], "priority")?,
        int_of(&json["sequence"], "sequence")?,
        text(str_of(&json["event_type"], "event_type")?),
        map_of(&json["payload"], "payload")?,
        opt_str(&json["source_id"], "source_id")?,
        opt_str(&json["causation_id"], "causation_id")?,
        map_of(&json["provenance"], "an event's provenance")?,
    )
    .ok_or_else(|| bad("an event ScheduledEvent refuses"))
}

fn rows<R>(json: &Value, what: &str, row: impl Fn(&Value) -> Result<R>) -> Result<Vec<R>> {
    array(json, what)?.iter().map(row).collect()
}

fn read_scenario(raw: &[u8]) -> Result<Scenario> {
    let json: Value = serde_json::from_slice(raw).map_err(|error| bad(error))?;
    if !json.is_object() {
        return Err(bad("the document is not a dict"));
    }
    let seed = json["seed"].clone();
    if world::Seed::from_text(&seed.to_string()).is_none() || !seed.is_number() {
        return Err(bad("seed is not an int"));
    }
    let initial_stock = match &json["initial_stock"] {
        Value::Null => None,
        found if found.is_object() => Some(found.clone()),
        _ => return Err(bad("initial_stock is not a dict")),
    };
    Ok(Scenario {
        id: text(str_of(&json["id"], "id")?),
        seed,
        duration: int_of(&json["duration_minutes"], "duration_minutes")?,
        configs: map_of(&json["model_configs"], "model_configs")?,
        initial_stock,
        agents: rows(&json["profiles"], "profiles", agent)?,
        facilities: rows(&json["facilities"], "facilities", facility)?,
        staff: rows(&json["staff"], "staff", staff)?,
        zones: rows(&json["zones"], "zones", zone)?,
        edges: rows(&json["zone_edges"], "zone_edges", edge)?,
        events: rows(&json["scheduled_events"], "scheduled_events", event)?,
    })
}

// ---- The World -------------------------------------------------------------------------------------

fn table_of<R: Clone>(rows: &[R], id: impl Fn(&R) -> Arc<str>) -> Table<R> {
    let mut table = Table::default();
    for row in rows {
        table.insert(id(row), row.clone());
    }
    table
}

fn put_row<T: Row>(world: &mut World, name: &str, row: T) -> Result<()> {
    let slot = world.slots.get(name).ok_or_else(|| Error::unsupported("a field has no slot"))?;
    slot.try_borrow_mut()?.row = Some(Box::new(row));
    Ok(())
}

/// `WorldState.from_scenario` and the controller's set-up: the World at minute 0.
fn build_world(scenario: &Scenario, table: &TableEntry) -> Result<World> {
    let mut world = World::default();
    let schema_version = if scenario.configs.contains_key("resource_service") { 6 } else { 5 };
    {
        let scalars = world.scalars.get_mut();
        scalars.scenario_id = Fit::Ok(scenario.id.clone());
        scalars.seed = Fit::Ok(world::Seed::from_text(&scenario.seed.to_string()).ok_or_else(|| bad("seed is not an int"))?);
        scalars.duration_minutes = Fit::Ok(scenario.duration);
        scalars.minute_index = Fit::Ok(0);
        scalars.schema_version = Fit::Ok(schema_version);
    }
    {
        let entities = world.entities.get_mut();
        entities.agents = table_of(&scenario.agents, |row| row.profile.id.clone());
        entities.facilities = table_of(&scenario.facilities, |row| row.id.clone());
        entities.staff = table_of(&scenario.staff, |row| row.id.clone());
        entities.zones = table_of(&scenario.zones, |row| row.id.clone());
        entities.zone_edges = table_of(&scenario.edges, |row| row.id.clone());
    }
    for name in OWNED_NAMESPACES {
        world.slots.add(text(name), Place::Namespace)?;
    }
    for name in OWNED_FIELDS {
        world.slots.add(text(name), Place::Field)?;
    }
    let mut resources = IdMap::<i64>::default();
    if let Some(stock) = &scenario.initial_stock {
        for resource in namespaces::RESOURCE_IDS {
            resources.insert(text(resource), int_of(&stock[resource], resource)?);
        }
    }
    put_row(&mut world, world::RESOURCES, resources)?;
    let event_counter = scenario.events.iter().map(|event| event.sequence).max().unwrap_or(-1) + 1;
    let counters = vmap([("agent", V::Int(scenario.agents.len() as i64)), ("event", V::Int(event_counter))]);
    put_row(&mut world, world::ID_COUNTERS, ValueRow(counters))?;
    put_row(&mut world, world::METRIC_STATE, IdMap::<f64>::default())?;
    put_row(&mut world, world::MODEL_CONFIGS, ValueRow(V::Map(scenario.configs.clone())))?;
    for (name, state) in namespaces::namespaces(scenario, table)? {
        load::put_namespace(&mut world, &name, state)?;
    }
    let mut queue = Queue::new();
    for event in &scenario.events {
        if queue.schedule_row(event.clone()) != 0 {
            return Err(bad("an event ID or a sequence is there twice"));
        }
    }
    world.hold_queue(Arc::new(queue));
    world.hold_log(Arc::new(EventLog::new()));
    Ok(world)
}

// ---- The entry -------------------------------------------------------------------------------------

/// `controller.zone_neighbors`: each zone's neighbors over the open edges, sorted.
fn zone_neighbors(scenario: &Scenario) -> V {
    let mut sets: IndexMap<Arc<str>, Vec<Arc<str>>> = scenario.zones.iter().map(|zone| (zone.id.clone(), Vec::new())).collect();
    for edge in scenario.edges.iter().filter(|edge| !edge.closed) {
        for (from, to) in [(&edge.from_zone_id, &edge.to_zone_id), (&edge.to_zone_id, &edge.from_zone_id)] {
            if let Some(set) = sets.get_mut(from) {
                if !set.contains(to) {
                    set.push(to.clone());
                }
            }
        }
    }
    V::Map(
        sets.into_iter()
            .map(|(zone, mut neighbors)| {
                neighbors.sort_by(|first, second| first.as_bytes().cmp(second.as_bytes()));
                (zone, V::List(neighbors.into_iter().map(V::Str).collect()))
            })
            .collect(),
    )
}

/// The sections: the table's, with the floor plan's two from the configs as they are (the
/// builder's configs are their contracts' dumps) and whether queues are independent.
fn sections(scenario: &Scenario, table: &TableEntry) -> Result<V> {
    let mut sections = table.sections.clone();
    let configs = &scenario.configs;
    let geometry = match configs.get("facility_geometry") {
        None => vmap([("absent", V::Bool(true))]),
        Some(V::Map(geometry)) => {
            if !matches!(geometry.get("arrival_approach_routes"), Some(V::List(routes)) if routes.is_empty()) {
                return Err(Error::unsupported("arrival approach routes are not built in the core"));
            }
            if namespaces::independent_queues(geometry)? {
                return Err(Error::unsupported("independent service-point queues are not built in the core"));
            }
            vmap([("value", V::Map(geometry.clone()))])
        }
        Some(_) => return Err(bad("facility_geometry is not a dict")),
    };
    sections.insert(text("facility_geometry"), geometry);
    let spatial = configs.get("spatial_constraints").ok_or_else(|| Error::unsupported("a Scenario without spatial constraints"))?;
    sections.insert(text("spatial_constraints"), vmap([("value", spatial.clone())]));
    match sections.get_mut("tick_plan") {
        Some(V::Map(plan)) => match plan.get_mut("value") {
            Some(V::Map(fields)) => {
                fields.insert(text("independent_queues"), V::Bool(false));
            }
            _ => return Err(Error::unsupported("the table's tick plan is a fault")),
        },
        _ => return Err(Error::Value("the table has no tick plan".into())),
    }
    Ok(V::Map(sections))
}

/// The entry of the Scenario's world at minute 0 (`export_world(world, controller)`), as the
/// browser's sample writes it: `json.dumps(entry, sort_keys=True, ensure_ascii=False,
/// separators=(",", ":"))`.
pub fn entry(scenario: &[u8]) -> Result<Vec<u8>> {
    let scenario = read_scenario(scenario)?;
    let table = entry_of(&scenario.configs)?;
    let world = build_world(&scenario, &table)?;
    let rng_state = vmap([("algorithm", vstr("sha256-keyed-v1")), ("seed", value_of(&scenario.seed)?)]);
    let document = crate::canonical::canonical_json(&world, &rng_state)?;
    let slots = V::List(
        world
            .slots
            .names()
            .map(|name| {
                let field = OWNED_FIELDS.contains(&&**name);
                V::List(vec![V::Str(name.clone()), V::Bool(field)])
            })
            .collect(),
    );
    let run = vmap([
        ("policies", table.policies.clone()),
        ("route_capacities", table.route_capacities.clone()),
        ("zone_neighbors", zone_neighbors(&scenario)),
    ]);
    let mut out = Vec::with_capacity(document.len() + 64 * 1024);
    out.extend_from_slice(b"{\"run\":");
    out.extend_from_slice(&canonical(&run)?);
    out.extend_from_slice(b",\"sections\":");
    out.extend_from_slice(&canonical(&sections(&scenario, &table)?)?);
    out.extend_from_slice(b",\"slots\":");
    out.extend_from_slice(&canonical(&slots)?);
    out.extend_from_slice(b",\"world\":");
    out.extend_from_slice(&document);
    out.push(b'}');
    Ok(out)
}
