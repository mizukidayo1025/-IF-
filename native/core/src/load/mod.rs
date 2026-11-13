//! A World and a Run from JSON, without Python (Issue #244, stage 4 of #132; first written for the
//! core's tests in #152 lane P10-1b and #225).
//!
//! The Python side writes one world as JSON (`sheltersim.core.world_export.export_world`): the
//! world's canonical document at its minute, the slots its core owns (`[name, is_field]`), what the
//! Run read of the controller (`RunCore.policies_document`, the route capacities, the zone
//! neighbors) and the Python-parsed sections of its configs. This module builds the core's values
//! from such an entry, so that a host without Python (the tests' `TestHost`, the browser's) runs
//! ticks on it:
//!
//! - `world` builds the World the PyO3 crate's core holds for a document: the scalars, the entity
//!   rows, one slot per owned name (the four fields with their rows, each namespace with the row its
//!   document has), the queue's events and the log's records. A namespace of `model_state` becomes
//!   its typed row where it has one (`typed`, each read with `Field::from_value`), and a `ValueRow`
//!   otherwise (`put_namespace`).
//! - `run` (`policies`, `route_capacities`, `neighbors`) makes the Run's `Policies` again from the
//!   document of what was read; `loaded` builds the World and the Run of an entry together
//!   (`Loaded`).
//! - `sections` reads the Python-parsed sections (`Sections`), which a host fills the configs with
//!   (`Host::fill_section`), and builds the floor plan of the world's overlay from them
//!   (`Sections::floor_plan`, `FloorPlans`).
//! - `find_record` is `event_log.find` on the world's log, read back from the bytes it wrote.
//!
//! The JSON is untrusted input (the browser reads it): every function here gives an `Error` for a
//! value that is not what the exporter writes, and never panics on it.

mod run;
mod sections;
mod typed;

use std::cell::RefCell;
use std::sync::Arc;

use indexmap::IndexMap;
use serde_json::Value;

pub use run::{fault, neighbors, obj, policies, route_capacities};
pub use sections::{overlay, FloorPlans, Sections, OVERLAY};
pub use typed::{typed_row, TYPED};

use crate::cell::Shard;
use crate::entities::{AgentRow, EdgeRow, FacilityRow, NeedRow, ProfileRow, RequestRow, Skills, StaffRow, Table, TaskRow, ZoneRow};
use crate::error::{Error, Result};
use crate::events::log::EventLog;
use crate::events::queue::Queue;
use crate::events::{EventKind, EventRow, EventStatus};
use crate::row::{IdMap, Row, StrEnum, ValueRow};
use crate::rules::common_operations::Logged;
use crate::rules::tick::{Seed, ZoneNeighbors};
use crate::run::{Policies, Run, RunConfig};
use crate::value::V;
use crate::world::{self, Fit, Place, World};

// ---- Reading the JSON ------------------------------------------------------------------------------

/// The error of a value that is not what the exporter writes.
pub(crate) fn bad(what: impl std::fmt::Display) -> Error {
    Error::Value(format!("not an exported world: {what}"))
}

pub(crate) fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

/// A JSON value as the core's `V` (`V::from_json`): a number with a point or an exponent is a
/// float, as Python's `json` reads it.
pub fn value(json: &Value) -> Result<V> {
    V::from_json(json).ok_or_else(|| bad("a number Python does not read"))
}

pub(crate) fn map(json: &Value) -> Result<IndexMap<Arc<str>, V>> {
    match value(json)? {
        V::Map(map) => Ok(map),
        _ => Err(bad("a value is not a dict")),
    }
}

pub(crate) fn object<'a>(json: &'a Value, what: &str) -> Result<&'a serde_json::Map<String, Value>> {
    json.as_object().ok_or_else(|| bad(format_args!("{what} is not a dict")))
}

pub(crate) fn array<'a>(json: &'a Value, what: &str) -> Result<&'a Vec<Value>> {
    json.as_array().ok_or_else(|| bad(format_args!("{what} is not a list")))
}

pub(crate) fn str_of<'a>(json: &'a Value, what: &str) -> Result<&'a str> {
    json.as_str().ok_or_else(|| bad(format_args!("{what} is not a str")))
}

pub(crate) fn bool_of(json: &Value, what: &str) -> Result<bool> {
    json.as_bool().ok_or_else(|| bad(format_args!("{what} is not a bool")))
}

pub(crate) fn int_of(json: &Value, what: &str) -> Result<i64> {
    json.as_i64().ok_or_else(|| bad(format_args!("{what} is not an int")))
}

pub(crate) fn texts(json: &Value, what: &str) -> Result<Vec<Arc<str>>> {
    array(json, what)?.iter().map(|item| str_of(item, what).map(text)).collect()
}

fn s(json: &Value, key: &str) -> Result<Arc<str>> {
    str_of(&json[key], key).map(text)
}

fn opt_s(json: &Value, key: &str) -> Option<Arc<str>> {
    json.get(key).and_then(Value::as_str).map(text)
}

fn i(json: &Value, key: &str) -> Result<i64> {
    int_of(&json[key], key)
}

fn opt_i(json: &Value, key: &str) -> Option<i64> {
    json.get(key).and_then(Value::as_i64)
}

fn f(json: &Value, key: &str) -> Result<f64> {
    json[key].as_f64().ok_or_else(|| bad(format_args!("{key} is not a number")))
}

fn opt_f(json: &Value, key: &str) -> Option<f64> {
    json.get(key).and_then(Value::as_f64)
}

fn strings(json: &Value, key: &str) -> Result<Vec<Arc<str>>> {
    texts(&json[key], key)
}

fn member<T: StrEnum>(json: &Value, key: &str) -> Result<T> {
    T::from_text(str_of(&json[key], key)?).ok_or_else(|| bad(format_args!("{key} is not a member")))
}

// ---- The entity rows -------------------------------------------------------------------------------

/// A WorldState v5 document leaves `hydration_need` out; the object holds 0.0 then.
fn needs(json: &Value) -> Result<NeedRow> {
    Ok(NeedRow {
        toilet_urgency: f(json, "toilet_urgency")?,
        hunger: f(json, "hunger")?,
        fatigue: f(json, "fatigue")?,
        hydration_need: opt_f(json, "hydration_need").unwrap_or(0.0),
    })
}

fn agent(json: &Value) -> Result<AgentRow> {
    let profile = &json["profile"];
    Ok(AgentRow {
        profile: ProfileRow {
            id: s(profile, "id")?,
            group_id: opt_s(profile, "group_id"),
            initial_zone_id: s(profile, "initial_zone_id")?,
            mobility: member(profile, "mobility")?,
            support_need: f(profile, "support_need")?,
            stress_sensitivity: f(profile, "stress_sensitivity")?,
            rng_key: s(profile, "rng_key")?,
            initial_needs: needs(&profile["initial_needs"])?,
            provenance: value(&profile["provenance"])?,
        },
        zone_id: s(json, "zone_id")?,
        activity: member(json, "activity")?,
        target_id: opt_s(json, "target_id"),
        needs: needs(&json["needs"])?,
        stress: f(json, "stress")?,
        trust: f(json, "trust")?,
        next_decision_at: i(json, "next_decision_at")?,
        support_request_id: opt_s(json, "support_request_id"),
        support_cooldown_until: i(json, "support_cooldown_until")?,
    })
}

fn facility(json: &Value) -> Result<FacilityRow> {
    let optional = |key: &str| {
        if json.get(key).is_some() {
            strings(json, key)
        } else {
            Ok(Vec::new())
        }
    };
    Ok(FacilityRow {
        id: s(json, "id")?,
        kind: s(json, "kind")?,
        zone_id: s(json, "zone_id")?,
        capacity: i(json, "capacity")?,
        working_units: i(json, "working_units")?,
        active_agent_ids: strings(json, "active_agent_ids")?,
        queue_agent_ids: strings(json, "queue_agent_ids")?,
        resource_active_agent_ids: optional("resource_active_agent_ids")?,
        resource_queue_agent_ids: optional("resource_queue_agent_ids")?,
    })
}

fn staff(json: &Value) -> Result<StaffRow> {
    Ok(StaffRow {
        id: s(json, "id")?,
        zone_id: s(json, "zone_id")?,
        skills: Skills(strings(json, "skills")?),
        work_queue: strings(json, "work_queue")?,
        busy_until: i(json, "busy_until")?,
        active_task_id: opt_s(json, "active_task_id"),
    })
}

fn request(json: &Value) -> Result<RequestRow> {
    Ok(RequestRow {
        id: s(json, "id")?,
        agent_id: s(json, "agent_id")?,
        requested_minute: i(json, "requested_minute")?,
        timeout_minute: i(json, "timeout_minute")?,
        status: member(json, "status")?,
        assigned_staff_id: opt_s(json, "assigned_staff_id"),
        task_id: opt_s(json, "task_id"),
        resolved_minute: opt_i(json, "resolved_minute"),
        cooldown_until: i(json, "cooldown_until")?,
        last_assignment_attempt_minute: opt_i(json, "last_assignment_attempt_minute"),
        last_assignment_wait_reason_code: opt_s(json, "last_assignment_wait_reason_code"),
    })
}

fn task(json: &Value) -> Result<TaskRow> {
    Ok(TaskRow {
        id: s(json, "id")?,
        kind: s(json, "kind")?,
        agent_id: opt_s(json, "agent_id"),
        resource_id: opt_s(json, "resource_id"),
        start_minute: i(json, "start_minute")?,
        due_minute: i(json, "due_minute")?,
        staff_id: opt_s(json, "staff_id"),
    })
}

fn zone(json: &Value) -> Result<ZoneRow> {
    Ok(ZoneRow {
        id: s(json, "id")?,
        name: s(json, "name")?,
        capacity: opt_i(json, "capacity"),
        display_x: opt_f(json, "display_x"),
        display_y: opt_f(json, "display_y"),
    })
}

fn edge(json: &Value) -> Result<EdgeRow> {
    Ok(EdgeRow {
        id: s(json, "id")?,
        from_zone_id: s(json, "from_zone_id")?,
        to_zone_id: s(json, "to_zone_id")?,
        travel_minutes: i(json, "travel_minutes")?,
        closed: bool_of(&json["closed"], "closed")?,
    })
}

/// An event of the queue (`ScheduledEvent.to_dict()`), with `ScheduledEvent`'s checks.
pub fn event(json: &Value) -> Result<EventRow> {
    let kind = EventKind::parse(str_of(&json["kind"], "kind")?).ok_or_else(|| bad("kind is not an event kind"))?;
    EventRow::new(
        s(json, "event_id")?,
        kind,
        i(json, "due_minute")?,
        i(json, "priority")?,
        i(json, "sequence")?,
        s(json, "event_type")?,
        map(&json["payload"])?,
        opt_s(json, "source_id"),
        opt_s(json, "causation_id"),
        map(&json["provenance"])?,
    )
    .ok_or_else(|| bad("an event ScheduledEvent refuses"))
}

fn rows<R: Clone>(items: &Value, what: &str, row: impl Fn(&Value) -> Result<R>, id: impl Fn(&R) -> Arc<str>) -> Result<Table<R>> {
    let mut table = Table::default();
    for item in array(items, what)? {
        let made = row(item)?;
        table.insert(id(&made), made);
    }
    Ok(table)
}

// ---- The World -------------------------------------------------------------------------------------

fn put_row<T: Row>(world: &mut World, name: &str, row: T) -> Result<()> {
    let slot = world.slots.get(name).ok_or_else(|| bad(format_args!("{name} has no slot")))?;
    slot.try_borrow_mut()?.row = Some(Box::new(row));
    Ok(())
}

/// `model_state[name] = value` in the World the PyO3 crate's core holds: the namespace's typed row
/// where it has one (`typed_row`; an owned namespace, whose slot the world has), and a `ValueRow`
/// otherwise, in a slot of its own where the world has none. A value that does not fit its typed
/// row is an error.
pub fn put_namespace(world: &mut World, name: &str, value: V) -> Result<()> {
    if let Some(row) = typed_row(name, &value) {
        let row = row.map_err(|misfit| bad(format_args!("{name} does not fit its row: {}", misfit.0)))?;
        let slot = world.slots.get(name).ok_or_else(|| bad(format_args!("{name} has no slot")))?;
        slot.try_borrow_mut()?.row = Some(row);
        return Ok(());
    }
    if world.slots.get(name).is_none() {
        world.slots.add(text(name), Place::Value)?;
    }
    put_row(world, name, ValueRow(value))
}

/// The World of a canonical document, with a slot for each owned name (`slots`: `[name, is_field]`).
pub fn world(document: &Value, slots: &Value) -> Result<World> {
    let mut world = World::default();
    {
        let scalars = world.scalars.get_mut();
        scalars.scenario_id = Fit::Ok(s(document, "scenario_id")?);
        let seed = world::Seed::from_text(&document["seed"].to_string()).ok_or_else(|| bad("seed is not an int"))?;
        scalars.seed = Fit::Ok(seed);
        scalars.duration_minutes = Fit::Ok(i(document, "duration_minutes")?);
        scalars.minute_index = Fit::Ok(i(document, "minute_index")?);
        scalars.schema_version = Fit::Ok(i(document, "schema_version")?);
    }
    {
        let entities = world.entities.get_mut();
        entities.agents = rows(&document["agents"], "agents", agent, |row| row.profile.id.clone())?;
        entities.facilities = rows(&document["facilities"], "facilities", facility, |row| row.id.clone())?;
        entities.staff = rows(&document["staff"], "staff", staff, |row| row.id.clone())?;
        entities.support_requests = rows(&document["support_requests"], "support_requests", request, |row| row.id.clone())?;
        entities.active_tasks = rows(&document["active_tasks"], "active_tasks", task, |row| row.id.clone())?;
        entities.zones = rows(&document["zones"], "zones", zone, |row| row.id.clone())?;
        entities.zone_edges = rows(&document["zone_edges"], "zone_edges", edge, |row| row.id.clone())?;
    }
    for slot in array(slots, "slots")? {
        let name = str_of(&slot[0], "a slot's name")?;
        let place = if bool_of(&slot[1], "a slot's is_field")? { Place::Field } else { Place::Namespace };
        world.slots.add(text(name), place)?;
    }
    let mut resources = IdMap::<i64>::default();
    for (key, stock) in map(&document["resources"])? {
        match stock {
            V::Int(number) => resources.insert(key, number),
            _ => return Err(bad("a stock is not an int")),
        };
    }
    put_row(&mut world, world::RESOURCES, resources)?;
    put_row(&mut world, world::ID_COUNTERS, ValueRow(value(&document["id_counters"])?))?;
    let mut metrics = IdMap::<f64>::default();
    for (key, metric) in map(&document["metric_state"])? {
        match metric {
            V::Float(number) => metrics.insert(key, number),
            _ => return Err(bad("a metric is not a float")),
        };
    }
    put_row(&mut world, world::METRIC_STATE, metrics)?;
    put_row(&mut world, world::MODEL_CONFIGS, ValueRow(value(&document["model_configs"])?))?;
    for (name, namespace) in object(&document["model_state"], "model_state")? {
        put_namespace(&mut world, name, value(namespace)?)?;
    }
    let mut queue = Queue::new();
    for item in array(&document["event_queue"], "event_queue")? {
        if queue.schedule_row(event(item)?) != 0 {
            return Err(bad("the queue has an event ID or a sequence twice"));
        }
    }
    world.hold_queue(Arc::new(queue));
    let mut log = EventLog::new();
    for record in array(&document["event_log"], "event_log")? {
        let status = EventStatus::parse(str_of(&record["status"], "status")?).ok_or_else(|| bad("status is not an event status"))?;
        let detail = record["detail"].as_str();
        if log.append_row(&event(&record["event"])?, i(record, "realized_minute")?, status, detail)? != 0 {
            return Err(bad("the log has an event ID or a sequence twice"));
        }
    }
    world.hold_log(Arc::new(log));
    Ok(world)
}

// ---- The World and the Run of an entry -------------------------------------------------------------

/// What an entry loads: the World, the Run, its policies, the neighbors and the tick's random
/// source (`CounterRNG(world.seed)`), as a `Tick` takes them.
pub struct Loaded {
    pub cell: Shard<World>,
    pub run: RefCell<Run>,
    pub policies: Arc<Policies>,
    pub neighbors: Arc<ZoneNeighbors>,
    pub seed: Seed,
}

/// The World and the Run of an exported entry (`world`, `slots` and `run`).
pub fn loaded(entry: &Value) -> Result<Loaded> {
    let world = world(&entry["world"], &entry["slots"])?;
    let run_document = &entry["run"];
    if !run_document.is_object() {
        return Err(bad("the entry was exported without a controller"));
    }
    let policies = policies(&run_document["policies"])?;
    let config = RunConfig { route_capacities: Ok(Arc::new(route_capacities(&run_document["route_capacities"])?)) };
    let run = Run::with_policies(config, policies);
    let policies = run.policies.clone();
    let neighbors = neighbors(&run_document["zone_neighbors"])?;
    // `world` read the seed as an int already.
    let seed_text = entry["world"]["seed"].to_string();
    let seed = Seed::Counter { negative: seed_text.starts_with('-'), text: Some(text(&seed_text)) };
    Ok(Loaded { cell: Shard::new(world), run: RefCell::new(run), policies, neighbors, seed })
}

// ---- The log ---------------------------------------------------------------------------------------

/// `event_log.find`: the record of an event in the world's log now, read back from the bytes the
/// log wrote of it or from its packed form (`EventLog::record`, #252), as the native host finds a
/// record the Python world's log holds (the ones a rule wrote in the same call too). An error for
/// a record the log holds neither of (only Python takes its bytes) or cannot read back.
pub fn find_record(world: &World, event_id: &str) -> Result<Option<Logged>> {
    let log = world.log()?;
    let Some(index) = log.index_of(event_id) else {
        return Ok(None);
    };
    let record = log.record(index)?.ok_or_else(|| Error::unsupported("the log does not hold the record's bytes"))?;
    Ok(Some(Logged { status: record.status, realized_minute: record.realized_minute, event: record.event }))
}
