//! A World and a Run from JSON, without Python (Issue #152, lane P10-1b; orchestrator decision (b)
//! on design-p8.md section 4.3: a test-only loader, not part of the crate).
//!
//! A fixture file under `tests/data/` holds named worlds (`{"worlds": {name: entry}}`), which
//! `tests/support/core_fixtures.py` exports and `benchmarks/core_fixtures.py` writes (Issue #225):
//! each entry has the world's canonical document at its minute, the slots its core owns, what the
//! Run read of the controller (`RunCore.policies_document`), and the Python-parsed sections of its
//! configs (`sections.rs`). `entry` reads one; `fixture` is the small world of
//! `tests/support/core_tick.py` at minute 0 (`tick_core.json`'s `small_replay`). `world` builds the
//! World the PyO3 crate's core holds for a document: the scalars, the entity rows, one slot per owned
//! name (the four fields with their rows, each namespace with the row its document has), the queue's
//! events and the log's records. A namespace of `model_state` becomes its typed row where it has one
//! (`support/typed.rs`, each read with `Field::from_value`), and a `ValueRow` otherwise (the
//! population's, the temporary spaces', the overlay's, and keys no lane owns).
//!
//! `policies` makes the Run's `Policies` again from the document of what was read: the objects, the
//! values, the faults and the dispatch table, as the PyO3 crate read them.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use indexmap::IndexMap;
use serde_json::Value;
use sheltersim_core::cell::Shard;
use sheltersim_core::config::{Fault, FaultClass};
use sheltersim_core::entities::{
    AgentRow, EdgeRow, FacilityRow, NeedRow, ProfileRow, RequestRow, Skills, StaffRow, TaskRow, ZoneRow,
};
use sheltersim_core::error::Error;
use sheltersim_core::events::log::EventLog;
use sheltersim_core::events::queue::Queue;
use sheltersim_core::events::{EventKind, EventRow, EventStatus};
use sheltersim_core::geometry::{Geo, OverlayKey};
use sheltersim_core::row::{IdMap, StrEnum, ValueRow};
use sheltersim_core::rules::common_operations::Logged;
use sheltersim_core::rules::tick::{Seed, ZoneNeighbors};
use sheltersim_core::run::dispatch::{Dispatch, Handler, HandlerKind, Native};
use sheltersim_core::run::held::{Class, Held, Obj};
use sheltersim_core::run::{Capacity, Policies, RouteCapacities, Run, RunConfig};
use sheltersim_core::value::V;
use sheltersim_core::world::{self, Fit, Place, World, WorldCell};

/// A fixture file of `tests/data/` (`<name>.json`).
pub fn data(name: &str) -> Value {
    let path = format!("{}/tests/data/{name}.json", env!("CARGO_MANIFEST_DIR"));
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path}: {error}"));
    serde_json::from_str(&raw).unwrap()
}

/// The named world of a fixture file, as `core_fixtures.export_world` wrote it (with the keys its
/// builder added).
pub fn entry(file: &str, world: &str) -> Value {
    let mut data = data(file);
    let names: Vec<String> = data["worlds"].as_object().unwrap_or_else(|| panic!("{file} has no worlds")).keys().cloned().collect();
    data["worlds"].get_mut(world).unwrap_or_else(|| panic!("{file} has no world {world}: {names:?}")).take()
}

/// The small world of the core's tick test (`tick_core.json`'s `small_replay`).
pub fn fixture() -> Value {
    entry("tick_core", "small_replay")
}

/// The overlay's namespace (`GEOMETRY_OVERLAY_KEY`).
const OVERLAY: &str = "geometry_overlay_v1";

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

/// `model_state[name] = value` in the World the PyO3 crate's core holds: the namespace's typed row
/// where it has one (`support/typed.rs`; an owned namespace, whose slot the world has), and a
/// `ValueRow` otherwise, in a slot of its own where the world has none.
pub fn put_namespace(world: &mut World, name: &str, value: V) {
    if let Some(row) = super::typed::typed_row(name, &value) {
        let row = row.unwrap_or_else(|misfit| panic!("{name} does not fit its row: {}", misfit.0));
        let slot = world.slots.get(name).unwrap_or_else(|| panic!("{name} has no slot"));
        slot.try_borrow_mut().unwrap().row = Some(row);
        return;
    }
    if world.slots.get(name).is_none() {
        world.slots.add(text(name), Place::Value).unwrap();
    }
    put_row(world, name, ValueRow(value));
}

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
        put_namespace(&mut world, name, v(value));
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

/// A fault as the exporter writes it: `[class, text]`.
pub fn fault(value: &Value) -> Fault {
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

/// A `TestHost` whose Python-parsed sections are the exported world's (`sections::fill`) and whose
/// floor plan is the world's as exported (`sections::floor_plan`; none where `Geo::of` refuses).
pub fn host(fixture: &Value) -> super::TestHost {
    let mut host = super::TestHost::default();
    super::sections::fill(&mut host, fixture);
    host.plan = super::sections::floor_plan(fixture).ok();
    host
}

/// `host` on the loaded world, for a Run's ticks (#214): it builds the floor plan of the world as it
/// is at each call (`Following::floor_plan`) and finds the records of the world's log
/// (`Following::find_record`, #225 lane M9). It writes nothing of the world: the core writes
/// `world.resources` and the namespaces it starts itself, and only tells the host (#244, decision 2).
pub fn following_host(fixture: &Value, loaded: &Rc<Loaded>) -> super::TestHost {
    let mut host = host(fixture);
    // What `plan_of` reads of the export: the sections and the configs.
    let entry = serde_json::json!({
        "sections": fixture["sections"].clone(),
        "world": {"model_configs": fixture["world"]["model_configs"].clone()},
    });
    host.world = Some(Following { loaded: Rc::clone(loaded), entry, built: None, builds: 0 });
    host
}

/// What a host reads of the world it owns, as the PyO3 crate's `PyHost` reads the Python world, on
/// a loaded World (#214).
pub struct Following {
    loaded: Rc<Loaded>,
    /// The sections and the configs of the export, which the plan is built from.
    entry: Value,
    /// The plan last built, and the overlay's key it was built for.
    built: Option<(OverlayKey, Arc<Geo>)>,
    builds: usize,
}

impl Following {
    /// `Geo::of`: the plan of the world's overlay (`geometry_overlay_v1`) now, built again
    /// (`sections::plan_of`) when the overlay's facility moves or partitions change (its
    /// `OverlayKey`), so a tick that moves a facility walks on the new plan. An error is the refusal
    /// `Geo::of` raises.
    pub fn floor_plan(&mut self) -> Result<Arc<Geo>, Error> {
        let overlay = self.loaded.cell.with(|world| world.slots.try_value(OVERLAY).map(|value| value.map(|value| value.clone())))??;
        let key = OverlayKey::of(overlay.as_ref(), None)?;
        if let Some((held, geo)) = &self.built {
            if *held == key {
                return Ok(geo.clone());
            }
        }
        let geo = super::sections::plan_of(&self.entry, overlay.as_ref())?;
        self.built = Some((key, geo.clone()));
        self.builds += 1;
        Ok(geo)
    }

    /// How many times `floor_plan` built the plan.
    pub fn builds(&self) -> usize {
        self.builds
    }

    /// `event_log.find`: the record of an event in the world's log now, read back from the bytes
    /// the log wrote of it, as the native host finds a record the Python world's log holds (the
    /// ones a rule wrote in the same call too).
    pub fn find_record(&self, event_id: &str) -> Result<Option<Logged>, Error> {
        self.loaded.cell.with(|world| -> Result<Option<Logged>, Error> {
            let log = world.log()?;
            for index in 0..log.len() {
                let record: Value = serde_json::from_slice(log.written(index).expect("a record's bytes")).unwrap();
                if record["event"]["event_id"] == event_id {
                    let status = EventStatus::parse(record["status"].as_str().unwrap()).unwrap();
                    return Ok(Some(Logged { status, realized_minute: i(&record, "realized_minute"), event: event(&record["event"]) }));
                }
            }
            Ok(None)
        })?
    }
}
