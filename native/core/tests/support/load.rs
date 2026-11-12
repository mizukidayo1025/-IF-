//! A World and a Run from JSON, without Python (Issue #152, lane P10-1b; orchestrator decision (b)
//! on design-p8.md section 4.3: a test-only loader, not part of the crate).
//!
//! `fixture` reads `tests/data/tick_core.json`, which `benchmarks/core_tick_fixture.py` writes from
//! the small world of `tests/support/core_tick.py`: the world's canonical document at minute 0 (after
//! the controller and `TickEngine` set it up), the slots its core owns, and what the Run read of the
//! controller (`RunCore.policies_document`). `world` builds the World the PyO3 crate's core holds for
//! that document: the scalars, the entity rows, one slot per owned name (the four fields with their
//! rows, each namespace with the row its document has), the queue's events and the log's records.
//! It reads only what a world of that fixture has: a namespace of `model_state` becomes a
//! `ValueRow` slot, which fits the namespaces whose rows are `ValueRow`s (the population's, the
//! temporary spaces', the overlay's, and keys no lane owns); a namespace with a typed row needs that
//! row's loader, which this one does not have (the fixture's `model_state` is empty).
//!
//! `policies` makes the Run's `Policies` again from the document of what was read: the objects, the
//! values, the faults and the dispatch table, as the PyO3 crate read them.

use std::collections::HashMap;
use std::sync::Arc;

use indexmap::IndexMap;
use serde_json::Value;
use sheltersim_core::cell::Shard;
use sheltersim_core::config::{Fault, FaultClass};
use sheltersim_core::entities::{
    AgentRow, EdgeRow, FacilityRow, NeedRow, ProfileRow, RequestRow, Skills, StaffRow, TaskRow, ZoneRow,
};
use sheltersim_core::events::log::EventLog;
use sheltersim_core::events::queue::Queue;
use sheltersim_core::events::{EventKind, EventRow, EventStatus};
use sheltersim_core::row::{IdMap, StrEnum, ValueRow};
use sheltersim_core::rules::tick::{Seed, ZoneNeighbors};
use sheltersim_core::run::dispatch::{Dispatch, Handler, HandlerKind, Native};
use sheltersim_core::run::held::{Class, Held, Obj};
use sheltersim_core::run::{Capacity, Policies, RouteCapacities, Run, RunConfig};
use sheltersim_core::value::V;
use sheltersim_core::world::{self, Fit, Place, World};

/// The fixture of the core's tick test.
pub fn fixture() -> Value {
    let raw = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/tick_core.json")).unwrap();
    serde_json::from_str(&raw).unwrap()
}

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

/// A JSON value as the core's `V`: a number with a point or an exponent is a float, as Python's
/// `json` reads it.
pub fn v(value: &Value) -> V {
    match value {
        Value::Null => V::Null,
        Value::Bool(flag) => V::Bool(*flag),
        Value::Number(number) => {
            let written = number.to_string();
            if written.contains(['.', 'e', 'E']) {
                V::Float(number.as_f64().expect("a float"))
            } else {
                match number.as_i64() {
                    Some(int) => V::Int(int),
                    None => V::Big(written.into()),
                }
            }
        }
        Value::String(text) => V::Str(Arc::from(text.as_str())),
        Value::Array(items) => V::List(items.iter().map(v).collect()),
        Value::Object(map) => V::Map(map.iter().map(|(key, item)| (Arc::from(key.as_str()), v(item))).collect::<IndexMap<_, _>>()),
    }
}

fn map(value: &Value) -> IndexMap<Arc<str>, V> {
    match v(value) {
        V::Map(map) => map,
        other => panic!("not a map: {other:?}"),
    }
}

fn s(value: &Value, key: &str) -> Arc<str> {
    text(value[key].as_str().unwrap_or_else(|| panic!("{key} is not a str")))
}

fn opt_s(value: &Value, key: &str) -> Option<Arc<str>> {
    value.get(key).and_then(Value::as_str).map(text)
}

fn i(value: &Value, key: &str) -> i64 {
    value[key].as_i64().unwrap_or_else(|| panic!("{key} is not an int"))
}

fn opt_i(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(Value::as_i64)
}

fn f(value: &Value, key: &str) -> f64 {
    value[key].as_f64().unwrap_or_else(|| panic!("{key} is not a number"))
}

fn opt_f(value: &Value, key: &str) -> Option<f64> {
    value.get(key).and_then(Value::as_f64)
}

fn strings(value: &Value, key: &str) -> Vec<Arc<str>> {
    value[key].as_array().unwrap_or_else(|| panic!("{key} is not a list")).iter().map(|item| text(item.as_str().unwrap())).collect()
}

fn member<T: StrEnum>(value: &Value, key: &str) -> T {
    T::from_text(value[key].as_str().unwrap()).unwrap_or_else(|| panic!("{key} is not a member"))
}

/// A WorldState v5 document leaves `hydration_need` out; the object holds 0.0 then.
fn needs(value: &Value) -> NeedRow {
    NeedRow {
        toilet_urgency: f(value, "toilet_urgency"),
        hunger: f(value, "hunger"),
        fatigue: f(value, "fatigue"),
        hydration_need: opt_f(value, "hydration_need").unwrap_or(0.0),
    }
}

fn agent(value: &Value) -> AgentRow {
    let profile = &value["profile"];
    AgentRow {
        profile: ProfileRow {
            id: s(profile, "id"),
            group_id: opt_s(profile, "group_id"),
            initial_zone_id: s(profile, "initial_zone_id"),
            mobility: member(profile, "mobility"),
            support_need: f(profile, "support_need"),
            stress_sensitivity: f(profile, "stress_sensitivity"),
            rng_key: s(profile, "rng_key"),
            initial_needs: needs(&profile["initial_needs"]),
            provenance: v(&profile["provenance"]),
        },
        zone_id: s(value, "zone_id"),
        activity: member(value, "activity"),
        target_id: opt_s(value, "target_id"),
        needs: needs(&value["needs"]),
        stress: f(value, "stress"),
        trust: f(value, "trust"),
        next_decision_at: i(value, "next_decision_at"),
        support_request_id: opt_s(value, "support_request_id"),
        support_cooldown_until: i(value, "support_cooldown_until"),
    }
}

fn facility(value: &Value) -> FacilityRow {
    let optional = |key: &str| if value.get(key).is_some() { strings(value, key) } else { Vec::new() };
    FacilityRow {
        id: s(value, "id"),
        kind: s(value, "kind"),
        zone_id: s(value, "zone_id"),
        capacity: i(value, "capacity"),
        working_units: i(value, "working_units"),
        active_agent_ids: strings(value, "active_agent_ids"),
        queue_agent_ids: strings(value, "queue_agent_ids"),
        resource_active_agent_ids: optional("resource_active_agent_ids"),
        resource_queue_agent_ids: optional("resource_queue_agent_ids"),
    }
}

fn staff(value: &Value) -> StaffRow {
    StaffRow {
        id: s(value, "id"),
        zone_id: s(value, "zone_id"),
        skills: Skills(strings(value, "skills")),
        work_queue: strings(value, "work_queue"),
        busy_until: i(value, "busy_until"),
        active_task_id: opt_s(value, "active_task_id"),
    }
}

fn request(value: &Value) -> RequestRow {
    RequestRow {
        id: s(value, "id"),
        agent_id: s(value, "agent_id"),
        requested_minute: i(value, "requested_minute"),
        timeout_minute: i(value, "timeout_minute"),
        status: member(value, "status"),
        assigned_staff_id: opt_s(value, "assigned_staff_id"),
        task_id: opt_s(value, "task_id"),
        resolved_minute: opt_i(value, "resolved_minute"),
        cooldown_until: i(value, "cooldown_until"),
        last_assignment_attempt_minute: opt_i(value, "last_assignment_attempt_minute"),
        last_assignment_wait_reason_code: opt_s(value, "last_assignment_wait_reason_code"),
    }
}

fn task(value: &Value) -> TaskRow {
    TaskRow {
        id: s(value, "id"),
        kind: s(value, "kind"),
        agent_id: opt_s(value, "agent_id"),
        resource_id: opt_s(value, "resource_id"),
        start_minute: i(value, "start_minute"),
        due_minute: i(value, "due_minute"),
        staff_id: opt_s(value, "staff_id"),
    }
}

fn zone(value: &Value) -> ZoneRow {
    ZoneRow {
        id: s(value, "id"),
        name: s(value, "name"),
        capacity: opt_i(value, "capacity"),
        display_x: opt_f(value, "display_x"),
        display_y: opt_f(value, "display_y"),
    }
}

fn edge(value: &Value) -> EdgeRow {
    EdgeRow {
        id: s(value, "id"),
        from_zone_id: s(value, "from_zone_id"),
        to_zone_id: s(value, "to_zone_id"),
        travel_minutes: i(value, "travel_minutes"),
        closed: value["closed"].as_bool().unwrap(),
    }
}

/// An event of the queue (`ScheduledEvent.to_dict()`).
pub fn event(value: &Value) -> EventRow {
    EventRow::new(
        s(value, "event_id"),
        EventKind::parse(value["kind"].as_str().unwrap()).unwrap(),
        i(value, "due_minute"),
        i(value, "priority"),
        i(value, "sequence"),
        s(value, "event_type"),
        map(&value["payload"]),
        opt_s(value, "source_id"),
        opt_s(value, "causation_id"),
        map(&value["provenance"]),
    )
    .expect("an event ScheduledEvent takes")
}

fn rows<R: Clone>(items: &Value, row: impl Fn(&Value) -> R, id: impl Fn(&R) -> Arc<str>) -> sheltersim_core::entities::Table<R> {
    let mut table = sheltersim_core::entities::Table::default();
    for item in items.as_array().unwrap() {
        let made = row(item);
        table.insert(id(&made), made);
    }
    table
}

fn put_row<T: sheltersim_core::row::Row>(world: &mut World, name: &str, row: T) {
    let slot = world.slots.get(name).unwrap_or_else(|| panic!("{name} has no slot"));
    slot.try_borrow_mut().unwrap().row = Some(Box::new(row));
}

/// The owned namespaces whose rows are typed (`OWNED_NAMESPACES` of `core/state_mirror.py` but the
/// population's, the temporary spaces' and the overlay's, which are `ValueRow`s).
const TYPED: [&str; 14] = [
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
    "resource_service_v1",
    "effect_program_v1",
    "facility_queues_v1",
];

/// The World of a canonical document, with a slot for each owned name (`[name, is_field]`).
pub fn world(document: &Value, slots: &Value) -> World {
    let mut world = World::default();
    {
        let scalars = world.scalars.get_mut();
        scalars.scenario_id = Fit::Ok(s(document, "scenario_id"));
        scalars.seed = Fit::Ok(world::Seed::from_text(&document["seed"].to_string()).expect("an int seed"));
        scalars.duration_minutes = Fit::Ok(i(document, "duration_minutes"));
        scalars.minute_index = Fit::Ok(i(document, "minute_index"));
        scalars.schema_version = Fit::Ok(i(document, "schema_version"));
    }
    {
        let entities = world.entities.get_mut();
        entities.agents = rows(&document["agents"], agent, |row| row.profile.id.clone());
        entities.facilities = rows(&document["facilities"], facility, |row| row.id.clone());
        entities.staff = rows(&document["staff"], staff, |row| row.id.clone());
        entities.support_requests = rows(&document["support_requests"], request, |row| row.id.clone());
        entities.active_tasks = rows(&document["active_tasks"], task, |row| row.id.clone());
        entities.zones = rows(&document["zones"], zone, |row| row.id.clone());
        entities.zone_edges = rows(&document["zone_edges"], edge, |row| row.id.clone());
    }
    for slot in slots.as_array().unwrap() {
        let name = slot[0].as_str().unwrap();
        let place = if slot[1].as_bool().unwrap() { Place::Field } else { Place::Namespace };
        world.slots.add(text(name), place).unwrap();
    }
    let resources: IdMap<i64> = map(&document["resources"])
        .into_iter()
        .map(|(key, value)| match value {
            V::Int(number) => (key, number),
            other => panic!("a stock is not an int: {other:?}"),
        })
        .collect();
    put_row(&mut world, world::RESOURCES, resources);
    put_row(&mut world, world::ID_COUNTERS, ValueRow(v(&document["id_counters"])));
    let metrics: IdMap<f64> = map(&document["metric_state"])
        .into_iter()
        .map(|(key, value)| match value {
            V::Float(number) => (key, number),
            other => panic!("a metric is not a float: {other:?}"),
        })
        .collect();
    put_row(&mut world, world::METRIC_STATE, metrics);
    put_row(&mut world, world::MODEL_CONFIGS, ValueRow(v(&document["model_configs"])));
    for (name, value) in document["model_state"].as_object().unwrap() {
        // Only a namespace whose row is a `ValueRow` fits here (see the module's note).
        assert!(!TYPED.contains(&name.as_str()), "{name} needs the loader of its typed row");
        if world.slots.get(name.as_str()).is_none() {
            world.slots.add(text(name), Place::Value).unwrap();
        }
        put_row(&mut world, name, ValueRow(v(value)));
    }
    let mut queue = Queue::new();
    for item in document["event_queue"].as_array().unwrap() {
        assert_eq!(queue.schedule_row(event(item)), 0);
    }
    world.hold_queue(Arc::new(queue));
    let mut log = EventLog::new();
    for record in document["event_log"].as_array().unwrap() {
        let status = EventStatus::ALL.into_iter().find(|status| Some(status.as_str()) == record["status"].as_str()).unwrap();
        let detail = record["detail"].as_str();
        assert_eq!(log.append_row(&event(&record["event"]), i(record, "realized_minute"), status, detail).unwrap(), 0);
    }
    world.hold_log(Arc::new(log));
    world
}

// ---- What the Run read of the controller ---------------------------------------------------------

fn fault(value: &Value) -> Fault {
    let class = match value[0].as_str().unwrap() {
        "Value" => FaultClass::Value,
        "Type" => FaultClass::Type,
        "Key" => FaultClass::Key,
        "Validation" => FaultClass::Validation,
        "Refused" => FaultClass::Refused,
        other => FaultClass::Other(text(other)),
    };
    Fault::new(class, value[1].as_str().unwrap())
}

fn held(value: &Value) -> Held {
    let (kind, item) = value.as_object().unwrap().iter().next().expect("one entry");
    match kind.as_str() {
        "value" => Held::Value(v(item)),
        "text" => Held::Text(text(item.as_str().unwrap())),
        "object" => Held::Object(Arc::new(obj(item))),
        "opaque" => Held::Opaque { callable: item.as_bool().unwrap() },
        "method" => Held::Method(text(item.as_str().unwrap())),
        other => panic!("not a held value: {other}"),
    }
}

fn read(value: &Value) -> Result<Held, Fault> {
    match value.get("fault") {
        Some(found) => Err(fault(found)),
        None => Ok(held(value)),
    }
}

/// An object as `RunCore.policies_document` wrote it.
pub fn obj(value: &Value) -> Obj {
    let class = &value["class"];
    let mut object = Obj::new(Class {
        module: class[0].as_str().map(text),
        qualname: class[1].as_str().map(text),
        resolves: class[2].as_bool().unwrap(),
    });
    for (name, item) in value["attrs"].as_object().unwrap() {
        object.attrs.insert(text(name), read(item));
    }
    for (name, item) in value["class_own"].as_object().unwrap() {
        object.class_own.insert(text(name), held(item));
    }
    for (name, item) in value["own"].as_object().unwrap() {
        object.own.insert(text(name), held(item));
    }
    object
}

fn dispatch(value: &Value) -> Dispatch {
    let known = match value["known"].get("fault") {
        Some(found) => Err(fault(found)),
        None => Ok(value["known"]["value"].as_bool().unwrap()),
    };
    let handlers = match value["handlers"].get("fault") {
        Some(found) => Err(fault(found)),
        None => {
            let mut table = IndexMap::new();
            for (event_type, item) in value["handlers"]["value"].as_object().unwrap() {
                let handler = if item.as_str() == Some("foreign") {
                    Handler::Foreign
                } else {
                    let class = &item["class"];
                    let kind = HandlerKind::of_class(class[0].as_str().unwrap(), class[1].as_str().unwrap()).expect("a kind the core runs");
                    Handler::Native(Native {
                        kind,
                        handler: Arc::new(obj(&item["handler"])),
                        own_managers: item["own_managers"].as_bool().unwrap(),
                        after_arrival_as_defined: item["after_arrival_as_defined"].as_bool().unwrap(),
                    })
                };
                table.insert(text(event_type), handler);
            }
            Ok(Arc::new(table))
        }
    };
    Dispatch { known, handlers }
}

/// The Run's policies as the PyO3 crate read them (`RunCore.policies_document`).
pub fn policies(value: &Value) -> Policies {
    let mut policies = Policies::of(Arc::new(obj(&value["controller"])));
    policies.dispatch = dispatch(&value["dispatch"]);
    policies
}

/// `controller.route_capacities` as the Run reads it: ints and None, by edge.
pub fn route_capacities(value: &Value) -> RouteCapacities {
    let mut by_edge = HashMap::new();
    for (edge, capacity) in value.as_object().unwrap() {
        let capacity = match capacity.as_i64() {
            Some(number) => Capacity::Max(number),
            None => Capacity::Unlimited,
        };
        by_edge.insert(text(edge), capacity);
    }
    RouteCapacities { any: !by_edge.is_empty(), by_edge, unmatched_keys: false }
}

/// `controller.zone_neighbors` as the tick reads it.
pub fn neighbors(value: &Value) -> Arc<ZoneNeighbors> {
    let table: HashMap<String, Vec<String>> = value
        .as_object()
        .unwrap()
        .iter()
        .map(|(zone, near)| (zone.clone(), near.as_array().unwrap().iter().map(|item| item.as_str().unwrap().to_owned()).collect()))
        .collect();
    let social = Ok(table.iter().map(|(zone, near)| (zone.clone(), Ok(near.clone()))).collect());
    Arc::new(ZoneNeighbors { social, information: Ok(table) })
}

/// What the fixture loads: the World, the Run, its policies, the neighbors and the tick's random
/// source (`CounterRNG(world.seed)`).
pub struct Loaded {
    pub cell: Shard<World>,
    pub run: std::cell::RefCell<Run>,
    pub policies: Arc<Policies>,
    pub neighbors: Arc<ZoneNeighbors>,
    pub seed: Seed,
}

/// The fixture's world and Run at minute 0.
pub fn loaded(fixture: &Value) -> Loaded {
    let world = world(&fixture["world"], &fixture["slots"]);
    let run_document = &fixture["run"];
    let policies = policies(&run_document["policies"]);
    let config = RunConfig { route_capacities: Ok(Arc::new(route_capacities(&run_document["route_capacities"]))) };
    let run = Run::with_policies(config, policies);
    let policies = run.policies.clone();
    let seed_text = fixture["world"]["seed"].to_string();
    let seed = Seed::Counter { negative: seed_text.starts_with('-'), text: Some(text(&seed_text)) };
    Loaded { cell: Shard::new(world), run: std::cell::RefCell::new(run), policies, neighbors: neighbors(&run_document["zone_neighbors"]), seed }
}

/// A `TestHost` whose Python-parsed sections are the fixture's where the fixture has them (the model
/// selection `selected_models` gives), and the test host's fixed values elsewhere, with the floor
/// plan of the fixture's configs.
pub fn host(fixture: &Value) -> super::TestHost {
    let models: Vec<(Arc<str>, Arc<str>)> =
        fixture["selection"].as_array().unwrap().iter().map(|pair| (text(pair[0].as_str().unwrap()), text(pair[1].as_str().unwrap()))).collect();
    let mut host = super::TestHost::default();
    // The floor plan of configs without a facility geometry or spatial constraints (`Geo::of` of
    // the PyO3 crate), and no overlay: no layout and no constraints.
    let configs = &fixture["world"]["model_configs"];
    assert!(configs.get("facility_geometry").is_none() && configs.get("spatial_constraints").is_none());
    assert!(fixture["world"]["model_state"].get("geometry_overlay_v1").is_none());
    host.plan = Some(Arc::new(sheltersim_core::geometry::Geo::new(None, None, None, None).unwrap()));
    host.sections.push((
        sheltersim_core::config::SectionId::Selection,
        Box::new(move |configs| {
            let _ = configs
                .selection
                .get_or_parse(|| sheltersim_core::config::Section::Ok(Arc::new(sheltersim_core::config::Selection::new(models.clone()))));
        }),
    ));
    host
}
