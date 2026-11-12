//! The rules of temporary spaces, rooms, beds, installed facilities and changes of the floor plan on
//! the generated Scenario's world (Issue #225, lane M3): the tests of
//! `tests/integration/test_overlay_rules.py` that check the core's rules, moved here.
//!
//! The world is the generated Scenario (the Project screen's, 20 people) at minute 0 with the
//! simulator's extensions, read from `tests/data/overlay_rules.json`
//! (`tests/support/core_fixtures_overlay.py` writes it with what the pytest read in Python: the
//! order of the world's dicts, the rules' limits, the points of the tests next to a limit, and the
//! contracts' answers for the parameters the tests give). A test edits the world as the pytest did
//! and calls the core's rule with what its PyO3 entry reads (`native/src/rules/overlay.rs`): whether
//! the world holds each namespace, the sections of the configs, and the floor plan of the world now
//! (`sections::plan_of` with its overlay). The `TestHost` answers each contract as the PyO3 crate's
//! host does (`Model.model_validate(input).model_dump(mode="json")`, from the export).
//!
//! The pytest ran a rule on a copy of the world (`WorldState.from_dict(world.to_dict())`), and
//! compared it with another copy that the deleted Python rule's writes were made on by hand. Here a
//! copy is the world put in that order (`World::canonicalize`) and given back after
//! (`World::snapshot`, `World::restore`), and two worlds are the same when their bytes
//! (`support/world_bytes.rs`) and the key order of the overlay and the spaces (Python's
//! `json.dumps`) are. The pytest's digests of what the deleted rule answered and left (`RECORDED`)
//! are recorded again here from the core (decision 1 of #225).

mod support;

use std::sync::{Arc, OnceLock};

use serde_json::{json, Value};
use sheltersim_core::cell::Shard;
use sheltersim_core::checkpoint::sha256_hex;
use sheltersim_core::config::movement::Movement;
use sheltersim_core::config::overlay::overlay_schema;
use sheltersim_core::config::service::ResourceService;
use sheltersim_core::config::{Configs, Lazy, SectionId};
use sheltersim_core::entities::{Activity, FacilityRow};
use sheltersim_core::error::Error;
use sheltersim_core::events::{EventKind, EventRow};
use sheltersim_core::geometry::{Geo, Pt};
use sheltersim_core::host::{Contract, Holding};
use sheltersim_core::row::ValueRow;
use sheltersim_core::rules::kit::{self, Held, Kept};
use sheltersim_core::rules::overlay::{self, Bed, Beds, Capacity, Change, Intake, Places, OVERLAY_KEY, SPACES_KEY};
use sheltersim_core::rules::tick::read::kept_of;
use sheltersim_core::state::movement::{MovementRow, Point};
use sheltersim_core::value::V;
use sheltersim_core::world::{Fit, Snapshot, World, WorldCell, MODEL_CONFIGS};

use support::dumps::dumps;
use support::load;
use support::sections;
use support::world_bytes::{world_bytes, Answers};
use support::TestHost;

const FILE: &str = "overlay_rules";
const ROOM_ZONE: &str = "sleeping";
const MOVEMENT: &str = "movement_v1";
const NO_ROOM: &str = "temporary room admission is no longer available";
const INTAKE_SOURCE: &str = "ASM-TEMPORARY-ROOM-INTAKE-V1";
const PLACEMENT_SOURCE: &str = "ASM-INSTALLED-FACILITY-PLACEMENT-V1";

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

fn entry() -> Value {
    static ENTRY: OnceLock<Value> = OnceLock::new();
    ENTRY.get_or_init(|| load::entry(FILE, "generated")).clone()
}

/// A JSON value as the core's `V`.
fn v(value: Value) -> V {
    load::v(&value)
}

/// A value written with its dicts' order (`ordered` of the exporter) as the core's `V`.
fn ordered(value: &Value) -> V {
    match value {
        Value::Object(map) if map.len() == 1 && map.contains_key("pairs") => V::Map(
            map["pairs"].as_array().unwrap().iter().map(|pair| (text(pair[0].as_str().unwrap()), ordered(&pair[1]))).collect(),
        ),
        Value::Array(items) => V::List(items.iter().map(ordered).collect()),
        other => load::v(other),
    }
}

/// A `V` as JSON, through Python's `json.dumps` text.
fn json_of(value: &V) -> Value {
    serde_json::from_str(&dumps(value)).unwrap()
}

fn value_error(said: &str) -> Error {
    Error::Value(said.to_owned())
}

/// The item of a `V` at this path of keys.
fn at<'a>(value: &'a V, path: &[&str]) -> &'a V {
    path.iter().fold(value, |value, key| match value {
        V::Map(map) => map.get(*key).unwrap_or_else(|| panic!("no {key}")),
        other => panic!("not a map at {key}: {other:?}"),
    })
}

fn at_mut<'a>(value: &'a mut V, path: &[&str]) -> &'a mut V {
    path.iter().fold(value, |value, key| match value {
        V::Map(map) => map.get_mut(*key).unwrap_or_else(|| panic!("no {key}")),
        other => panic!("not a map at {key}: {other:?}"),
    })
}

fn map_mut(value: &mut V) -> &mut indexmap::IndexMap<Arc<str>, V> {
    match value {
        V::Map(map) => map,
        other => panic!("not a map: {other:?}"),
    }
}

fn list_mut(value: &mut V) -> &mut Vec<V> {
    match value {
        V::List(items) => items,
        other => panic!("not a list: {other:?}"),
    }
}

fn float(value: &V) -> f64 {
    match value {
        V::Float(number) => *number,
        V::Int(number) => *number as f64,
        other => panic!("not a number: {other:?}"),
    }
}

/// A point of a JSON object `{"x": .., "y": ..}`, ints read as floats (`float(value)`).
fn point_v(x: f64, y: f64) -> V {
    v(json!({"x": x, "y": y}))
}

fn pt(value: &V) -> Pt {
    Pt { x: float(at(value, &["x"])), y: float(at(value, &["y"])) }
}

/// The contracts as the PyO3 crate's host answers them, from the export: an input the export does
/// not have is a test's mistake.
fn contracts(entry: &Value) -> Box<dyn FnMut(&Contract, &V) -> Result<V, Error>> {
    let table: Vec<(String, Value, V)> = entry["contracts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| (row[0].as_str().unwrap().to_owned(), row[1].clone(), ordered(&row[2])))
        .collect();
    Box::new(move |which, input| {
        let name = which.model().1;
        let given = json_of(input);
        let found = table.iter().find(|(model, wanted, _)| model == name && *wanted == given);
        Ok(found.unwrap_or_else(|| panic!("no answer of {name} for {given}")).2.clone())
    })
}

/// The world of the fixture and a host with its sections, its contracts and no floor plan of its
/// own: each rule is given the plan of the world now, as its PyO3 entry reads it (`Geo::of`).
struct Case {
    entry: Value,
    cell: Shard<World>,
    host: TestHost,
}

impl Case {
    fn new() -> Self {
        let entry = entry();
        let world = load::world(&entry["world"], &entry["slots"]);
        let mut host = load::host(&entry);
        host.contracts = Some(contracts(&entry));
        Self { entry, cell: Shard::new(world), host }
    }

    fn with<T>(&self, f: impl FnOnce(&mut World) -> T) -> T {
        self.cell.with(f).unwrap()
    }

    fn holding(&self) -> Holding {
        self.with(|world| Holding::of_world(world).unwrap())
    }

    fn held(&self, name: &str) -> Held {
        self.holding().held(name, "the world holds another state")
    }

    fn section<T>(&mut self, which: SectionId, get: fn(&Configs) -> &Lazy<T>, parse: fn(&V) -> sheltersim_core::config::read::Read<T>) -> Kept<T> {
        let holding = self.holding();
        kept_of(kit::section_value(&self.cell, &mut self.host, &holding, which, get, parse).unwrap())
    }

    /// The overlay's schema of the configs (`schema_of`).
    fn schema(&mut self) -> Kept<Option<i64>> {
        self.section(SectionId::OverlaySchema, |configs| &configs.overlay_schema, overlay_schema)
    }

    fn walking(&mut self) -> Kept<Movement> {
        self.section(SectionId::Movement, |configs| &configs.movement, Movement::parse)
    }

    fn service(&mut self) -> Kept<ResourceService> {
        self.section(SectionId::ResourceService, |configs| &configs.resource_service, ResourceService::parse)
    }

    /// The floor plan of the world now (`Plan::of`): None where `Geo::of` refuses it.
    fn plan(&self) -> Option<Arc<Geo>> {
        let overlay = self.state(OVERLAY_KEY);
        sections::plan_of(&self.entry, overlay.as_ref()).ok()
    }

    /// `Plan::with`: the plan, unread without the overlay.
    fn plan_with(&self, overlay: &Held) -> Option<Arc<Geo>> {
        if matches!(overlay, Held::Absent) {
            return None;
        }
        self.plan()
    }

    /// A namespace's value, or None without it.
    fn state(&self, name: &str) -> Option<V> {
        self.with(|world| {
            let slot = world.slots.get(name)?.try_borrow().unwrap();
            slot.row.as_deref().map(|row| row.as_any().downcast_ref::<ValueRow>().expect("a value row").0.clone())
        })
    }

    fn set_state(&self, name: &str, value: Option<V>) {
        self.with(|world| {
            let slot = world.slots.get(name).unwrap_or_else(|| panic!("no slot {name}"));
            let mut slot = slot.try_borrow_mut().unwrap();
            slot.touch();
            slot.row = value.map(|value| Box::new(ValueRow(value)) as Box<dyn sheltersim_core::row::Row>);
        });
    }

    fn edit_state(&self, name: &str, edit: impl FnOnce(&mut V)) {
        self.with(|world| edit(&mut world.row_mut::<ValueRow>(name).unwrap().0));
    }

    /// The configs changed: the world's, and the export's that its sections and plan are read from.
    fn edit_configs(&mut self, edit: impl FnOnce(&mut Value)) {
        edit(&mut self.entry["world"]["model_configs"]);
        let configs = v(self.entry["world"]["model_configs"].clone());
        self.with(|world| world.row_mut::<ValueRow>(MODEL_CONFIGS).unwrap().0 = configs);
        self.host.sections.clear();
        sections::fill(&mut self.host, &self.entry);
    }

    /// The facility geometry changed, on the configs and on its section the same way (the exporter
    /// checks the section is the configs').
    fn edit_geometry(&mut self, edit: impl Fn(&mut Value)) {
        edit(&mut self.entry["sections"]["facility_geometry"]["value"]);
        self.edit_configs(|configs| edit(&mut configs["facility_geometry"]));
    }

    fn edit_spatial(&mut self, edit: impl Fn(&mut Value)) {
        edit(&mut self.entry["sections"]["spatial_constraints"]["value"]);
        self.edit_configs(|configs| edit(&mut configs["spatial_constraints"]));
    }

    fn set_minute(&self, minute: i64) {
        self.with(|world| world.scalars.get_mut().minute_index = Fit::Ok(minute));
    }

    fn minute(&self) -> i64 {
        self.with(|world| world.minute_index().unwrap())
    }

    fn duration(&self) -> i64 {
        self.with(|world| world.duration_minutes().unwrap())
    }

    /// The world's bytes, and the key order of the namespaces whose row is a `V`.
    fn fingerprint(&self) -> (Vec<u8>, Vec<String>) {
        let orders = [OVERLAY_KEY, SPACES_KEY].iter().map(|name| self.state(name).as_ref().map_or_else(|| "absent".to_owned(), dumps)).collect();
        (self.with(|world| world_bytes(world)), orders)
    }

    /// The world as `WorldState.from_dict(world.to_dict())` gives it, until `give_back`.
    fn copy(&self) -> Snapshot {
        self.with(|world| {
            let snapshot = world.snapshot();
            world.canonicalize();
            snapshot
        })
    }

    fn give_back(&self, snapshot: Snapshot) {
        self.with(|world| world.restore(snapshot));
    }

    /// `_writes`: the rule on a copy of the world, and the deleted rule's writes on another; the
    /// same world after both. What the rule gave.
    fn writes<T>(&mut self, call: impl FnOnce(&mut Case) -> T, change: impl FnOnce(&mut Case)) -> T {
        let snapshot = self.copy();
        let result = call(self);
        let ours = self.fingerprint();
        self.give_back(snapshot);
        let snapshot = self.copy();
        change(self);
        let expected = self.fingerprint();
        self.give_back(snapshot);
        assert!(ours.0 == expected.0, "the world differs");
        assert_eq!(ours.1, expected.1);
        result
    }

    /// `_raises_text`: a plain ValueError with the Python rule's text, and nothing changed.
    fn raises_text<T: std::fmt::Debug>(&mut self, call: impl FnOnce(&mut Case) -> Result<T, Error>, said: &str) {
        let before = self.fingerprint();
        let result = call(self);
        assert_eq!(result.map(drop), Err(value_error(said)));
        assert!(self.fingerprint() == before, "the world changed");
    }

    // ---- What the tests read ----

    fn first_zone(&self) -> String {
        self.entry["order"]["zones"][0].as_str().unwrap().to_owned()
    }

    fn agents(&self) -> Vec<String> {
        self.entry["order"]["agents"].as_array().unwrap().iter().map(|id| id.as_str().unwrap().to_owned()).collect()
    }

    fn zone_of(&self, agent: &str) -> String {
        self.with(|world| world.rows().unwrap().agents.get(agent).unwrap().zone_id.to_string())
    }

    /// The people in the room's zone, in the world's order.
    fn in_room_zone(&self) -> Vec<String> {
        self.agents().into_iter().filter(|agent| self.zone_of(agent) == ROOM_ZONE).collect()
    }

    /// `_person_in_room_zone`.
    fn person_in_room_zone(&self) -> String {
        self.in_room_zone().remove(0)
    }

    /// The first facility, in the world's order, of this kind or whose kind holds `part`.
    fn facility(&self, matches: impl Fn(&str) -> bool) -> String {
        let order: Vec<String> = self.entry["order"]["facilities"].as_array().unwrap().iter().map(|id| id.as_str().unwrap().to_owned()).collect();
        self.with(|world| {
            let rows = world.rows().unwrap();
            order.into_iter().find(|id| matches(&rows.facilities.get(id.as_str()).unwrap().kind)).unwrap()
        })
    }

    fn put_point(&self, agent: &str, point: Option<Pt>) {
        self.with(|world| {
            world.row_mut::<MovementRow>(MOVEMENT).unwrap().positions.get_mut(agent).unwrap().point = point.map(|point| Point { x: point.x, y: point.y });
        });
    }

    // ---- The rules, as their PyO3 entries call them ----

    fn places(&mut self) -> (Held, Held, Kept<Option<i64>>) {
        (self.held(SPACES_KEY), self.held(OVERLAY_KEY), self.schema())
    }

    /// `capacity_metrics`, as its entry makes the dict.
    fn capacity_metrics(&mut self, zone: &str, occupancy: Option<i64>) -> Value {
        let (spaces, overlay, schema) = self.places();
        let places = Places { spaces: &spaces, overlay: &overlay, schema: &schema };
        let found = self.with(|world| overlay::zone_capacity(world, &places, zone, occupancy)).unwrap();
        match found {
            None => json!({
                "nominal_capacity": null, "effective_capacity": null, "temporary_people_capacity": 0,
                "retained_expired_capacity": 0, "temporary_sleeping_capacity": 0, "physical_room_capacity": 0,
                "active_room_admissions": 0, "retained_expired_room_admissions": 0, "effective_total_capacity": null,
            }),
            Some((nominal, found)) => metrics_of(nominal, &found),
        }
    }

    fn register_space(&self, record: V, event_id: &str) -> Result<(), Error> {
        let held = self.held(SPACES_KEY);
        self.with(|world| overlay::register_space(world, &held, record, event_id))
    }

    fn expire_space(&self, space_id: &str, event_id: &str, cause: Option<&str>) -> Result<bool, Error> {
        let held = self.held(SPACES_KEY);
        self.with(|world| overlay::expire_space(world, &held, space_id, event_id, cause))
    }

    /// `reserve_sleep_bed`: the bed, written where it is new.
    fn reserve(&mut self, agent: &str) -> Result<Option<Bed>, Error> {
        let overlay = self.held(OVERLAY_KEY);
        if matches!(overlay, Held::Absent) {
            return Ok(None);
        }
        let (movement, schema, walking) = (self.held(MOVEMENT), self.schema(), self.walking());
        let plan = self.plan_with(&overlay);
        let beds = Beds { overlay: &overlay, movement: &movement, schema: &schema, walking: &walking, geo: plan.as_deref() };
        let Some(bed) = self.with(|world| overlay::plan_bed(world, &beds, agent))? else {
            return Ok(None);
        };
        if bed.is_new {
            self.with(|world| overlay::write_bed(world, &overlay, agent, &bed))?;
        }
        Ok(Some(bed))
    }

    fn release(&self, part: &str, agent: &str) -> Result<bool, Error> {
        let overlay = self.held(OVERLAY_KEY);
        self.with(|world| overlay::release(world, &overlay, part, Some(agent)))
    }

    fn intake<T>(&mut self, call: impl FnOnce(&World, &Intake<'_>) -> Result<T, Error>) -> Result<T, Error> {
        let (overlay, movement, schema) = (self.held(OVERLAY_KEY), self.held(MOVEMENT), self.schema());
        let plan = self.plan_with(&overlay);
        let intake = Intake { overlay: &overlay, movement: &movement, schema: &schema, geo: plan.as_deref() };
        self.with(|world| call(world, &intake))
    }

    fn room_admission_point(&mut self, room: &str) -> Option<Pt> {
        self.intake(|world, intake| overlay::room_admission_point(world, intake, room)).unwrap()
    }

    fn available_room(&mut self, zone: &str) -> Option<String> {
        self.intake(|world, intake| overlay::available_room_after(world, intake, zone, &[])).unwrap().map(|(room, _)| room.to_string())
    }

    fn admit(&mut self, agent: &str, room: &str, minute: i64, event_id: &str) -> Result<Pt, Error> {
        self.intake(|world, intake| overlay::admit(world, intake, agent, room, minute, event_id))
    }

    fn installed_point(&self, facility: &str) -> Option<Pt> {
        let overlay = self.held(OVERLAY_KEY);
        let plan = self.plan();
        self.with(|world| overlay::installed_point(world, &overlay, plan.as_deref(), facility)).unwrap()
    }

    fn place(&self, facility: &str, event_id: &str, sources: &[&str]) -> Result<Pt, Error> {
        let overlay = self.held(OVERLAY_KEY);
        let plan = self.plan();
        self.with(|world| overlay::place_facility(world, &overlay, plan.as_deref(), facility, event_id, sources))
    }

    fn change_inputs(&mut self) -> (Held, Held, Held, Held, Kept<ResourceService>, Option<Arc<Geo>>) {
        let overlay = self.held(OVERLAY_KEY);
        let plan = self.plan_with(&overlay);
        (overlay, self.held(MOVEMENT), self.held("staff_operations_v1"), self.held("resource_service_v1"), self.service(), plan)
    }

    /// `validate_geometry_change`.
    fn validate(&mut self, op: &str, target: &str, parameters: &Value, end: Option<i64>) -> Result<(), Error> {
        let (overlay, movement, staff_operations, resource_service, service, plan) = self.change_inputs();
        let change = Change { overlay: &overlay, movement: &movement, staff_operations: &staff_operations, resource_service: &resource_service, service: &service, geo: plan.as_deref() };
        overlay::check_change(&self.cell, &mut self.host, &change, op, target, &v(parameters.clone()), end).map(drop)
    }

    /// `apply_geometry_change`.
    fn apply(&mut self, event: &EventRow, expire: bool) -> Result<(), Error> {
        let (overlay, movement, staff_operations, resource_service, service, plan) = self.change_inputs();
        let change = Change { overlay: &overlay, movement: &movement, staff_operations: &staff_operations, resource_service: &resource_service, service: &service, geo: plan.as_deref() };
        overlay::apply_change(&self.cell, &mut self.host, &change, event, expire)
    }
}

/// `capacity_metrics`' dict of a zone with a capacity.
fn metrics_of(nominal: i64, found: &Capacity) -> Value {
    json!({
        "nominal_capacity": nominal,
        "effective_capacity": found.effective,
        "temporary_people_capacity": found.active_people,
        "retained_expired_capacity": found.retained,
        "temporary_sleeping_capacity": found.sleeping,
        "physical_room_capacity": found.room_capacity,
        "active_room_admissions": found.active_admissions,
        "retained_expired_room_admissions": found.retained_rooms,
        "effective_total_capacity": found.total().unwrap(),
    })
}

// ---- Temporary spaces ----------------------------------------------------------------------------

/// `_space`.
fn space(space_id: &str, zone: &str, active: bool, people: i64) -> V {
    v(json!({
        "space_id": space_id,
        "zone_id": zone,
        "people_capacity": people,
        "sleeping_capacity": 2,
        "start_minute": 0,
        "end_minute": 100,
        "active": active,
        "start_event_id": "start",
        "end_event_id": if active { Value::Null } else { json!("end") },
        "program_id": "program",
        "effect_id": "effect",
        "source_ids": ["ASM-A"],
        "geometry_status": "zone_only_capacity",
    }))
}

fn history_row(event_id: &str, space_id: &str, zone: &str, transition: &str, minute: i64, cause: Option<&str>) -> V {
    v(json!({
        "event_id": event_id,
        "space_id": space_id,
        "zone_id": zone,
        "minute": minute,
        "transition": transition,
        "causation_id": cause,
    }))
}

/// The `spaces` fixture: the world with a new state of temporary spaces.
fn spaces() -> Case {
    let case = Case::new();
    case.set_state(SPACES_KEY, Some(v(case.entry["spaces_state"].clone())));
    case
}

fn limit(case: &Case, name: &str) -> i64 {
    case.entry["limits"][name].as_i64().unwrap()
}

#[test]
fn a_space_registers_and_expires() {
    let case = spaces();
    let zone = case.first_zone();
    case.set_minute(4);
    case.register_space(space("a", &zone, true, 5), "e1").unwrap();
    let state = case.state(SPACES_KEY).unwrap();
    assert_eq!(at(&state, &["spaces", "a"]), &space("a", &zone, true, 5));
    assert_eq!(at(&state, &["history"]), &V::List(vec![history_row("e1", "a", &zone, "activated", 4, None)]));
    let before = case.fingerprint();
    assert_eq!(case.expire_space("b", "e2", None), Ok(false));
    assert!(case.fingerprint() == before);
    case.set_minute(9);
    assert_eq!(case.expire_space("a", "e2", Some("e1")), Ok(true));
    let state = case.state(SPACES_KEY).unwrap();
    assert_eq!(at(&state, &["spaces", "a", "active"]), &V::Bool(false));
    assert_eq!(at(&state, &["spaces", "a", "end_event_id"]), &V::Str(text("e2")));
    let V::List(history) = at(&state, &["history"]) else { panic!("a list") };
    assert_eq!(history[1..], [history_row("e2", "a", &zone, "expired", 9, Some("e1"))]);
    let before = case.fingerprint();
    assert_eq!(case.expire_space("a", "e3", None), Ok(false));
    assert!(case.fingerprint() == before);
}

#[test]
fn the_limits_of_spaces_say_the_python_rule_s_text() {
    let most = limit(&spaces(), "spaces");
    let fill = || {
        let case = spaces();
        let zone = case.first_zone();
        case.edit_state(SPACES_KEY, |state| {
            let spaces = map_mut(at_mut(state, &["spaces"]));
            for index in 0..most - 1 {
                spaces.insert(text(&format!("s{index}")), space(&format!("s{index}"), &zone, true, 5));
            }
        });
        (case, zone)
    };
    let (full, zone) = fill();
    full.register_space(space("x", &zone, true, 5), "e").unwrap();
    assert_eq!(map_mut(at_mut(&mut full.state(SPACES_KEY).unwrap(), &["spaces"])).len() as i64, most);
    let (case, zone) = fill();
    case.edit_state(SPACES_KEY, |state| {
        map_mut(at_mut(state, &["spaces"])).insert(text("full"), space("full", &zone, true, 5));
    });
    let before = case.fingerprint();
    // The ID is checked before the limit, as the Python rule did.
    for (space_id, said) in [("x", "temporary space runtime limit reached"), ("s0", "temporary space ID already exists")] {
        assert_eq!(case.register_space(space(space_id, &zone, true, 5), "e"), Err(value_error(said)));
        assert!(case.fingerprint() == before);
    }
}

#[test]
fn spaces_not_installed_say_the_python_rule_s_text() {
    let case = Case::new();
    case.set_state(SPACES_KEY, None);
    let zone = case.first_zone();
    let said = value_error("temporary_spaces_v1 is not installed");
    assert_eq!(case.register_space(space("a", &zone, true, 5), "e"), Err(said.clone()));
    assert_eq!(case.expire_space("a", "e", None), Err(said));
}

#[test]
fn a_full_history_drops_its_oldest_record() {
    let case = spaces();
    let zone = case.first_zone();
    let most = limit(&case, "space_history");
    let old: Vec<V> = (0..most).map(|index| history_row(&format!("old{index}"), "gone", &zone, "activated", 0, None)).collect();
    case.edit_state(SPACES_KEY, |state| list_mut(at_mut(state, &["history"])).extend(old.iter().cloned()));
    case.register_space(space("a", &zone, true, 5), "e").unwrap();
    let mut expected = old[1..].to_vec();
    expected.push(history_row("e", "a", &zone, "activated", 0, None));
    assert_eq!(at(&case.state(SPACES_KEY).unwrap(), &["history"]), &V::List(expected));
}

fn set_capacity(case: &Case, zone: &str, capacity: Option<i64>) {
    case.with(|world| world.rows_mut().unwrap().zones.get_mut(zone).unwrap().capacity = capacity);
}

#[test]
fn capacity_adds_active_spaces_and_keeps_expired_ones_while_people_need_them() {
    for occupancy in [None, Some(0), Some(3), Some(10_000)] {
        let mut case = spaces();
        let zones: Vec<String> = case.entry["order"]["zones"].as_array().unwrap().iter().map(|zone| zone.as_str().unwrap().to_owned()).collect();
        let zone = zones.into_iter().find(|zone| zone != "corridor").unwrap();
        case.edit_state(SPACES_KEY, |state| {
            let spaces = map_mut(at_mut(state, &["spaces"]));
            spaces.insert(text("active"), space("active", &zone, true, 5));
            spaces.insert(text("old"), space("old", &zone, false, 7));
            spaces.insert(text("other"), space("other", "corridor", true, 9));
        });
        set_capacity(&case, &zone, Some(2));
        let current = occupancy.unwrap_or_else(|| case.agents().iter().filter(|agent| case.zone_of(agent) == zone).count() as i64);
        let retained = 7.min(0.max(current - 2 - 5));
        let expected = json!({
            "nominal_capacity": 2,
            "effective_capacity": 7 + retained,
            "temporary_people_capacity": 5,
            "retained_expired_capacity": retained,
            "temporary_sleeping_capacity": 2,
            "physical_room_capacity": 0,
            "active_room_admissions": 0,
            "retained_expired_room_admissions": 0,
            "effective_total_capacity": 7 + retained,
        });
        assert_eq!(case.capacity_metrics(&zone, occupancy), expected, "{occupancy:?}");
        set_capacity(&case, &zone, None);
        let none = json!({
            "nominal_capacity": null,
            "effective_capacity": null,
            "temporary_people_capacity": 0,
            "retained_expired_capacity": 0,
            "temporary_sleeping_capacity": 0,
            "physical_room_capacity": 0,
            "active_room_admissions": 0,
            "retained_expired_room_admissions": 0,
            "effective_total_capacity": null,
        });
        assert_eq!(case.capacity_metrics(&zone, occupancy), none, "{occupancy:?}");
    }
}

// ---- Rooms and beds ------------------------------------------------------------------------------

/// `_room_record`: a room in the corner of the sleeping zone, its beds down its middle.
fn room_record(case: &Case, room_id: &str, people: i64, beds: i64) -> Value {
    let zones = case.entry["world"]["model_configs"]["facility_geometry"]["zones"].as_array().unwrap();
    let sleeping = zones.iter().find(|zone| zone["zone_id"] == ROOM_ZONE).unwrap();
    let corners = sleeping["polygon"].as_array().unwrap();
    let left = corners.iter().map(|point| point["x"].as_f64().unwrap()).fold(f64::INFINITY, f64::min);
    let bottom = corners.iter().map(|point| point["y"].as_f64().unwrap()).fold(f64::NEG_INFINITY, f64::max);
    json!({
        "schema_version": 1,
        "room_id": room_id,
        "zone_id": ROOM_ZONE,
        "polygon_m": [
            {"x": left + 0.5, "y": bottom - 4.4},
            {"x": left + 2.5, "y": bottom - 4.4},
            {"x": left + 2.5, "y": bottom - 0.4},
            {"x": left + 0.5, "y": bottom - 0.4},
        ],
        "people_capacity": people,
        "sleeping_capacity": beds,
        "bed_points_m": (0..beds).map(|index| json!({"x": left + 1.5, "y": bottom - 2.4 + 0.8 * index as f64})).collect::<Vec<_>>(),
        "route_edge_ids": ["route_corridor_sleeping", "route_sleeping_corridor"],
        "source_ids": ["ASM-TEMP-ROOM-TEST-V1"],
        "start_minute": 0,
        "end_minute": case.duration(),
        "active": true,
        "start_event_id": "start",
        "end_event_id": null,
        "program_id": "program",
        "effect_id": "effect",
    })
}

/// The keys of `_room_record` in its order: the record is put in the state in that order.
const ROOM_KEYS: [&str; 16] = [
    "schema_version",
    "room_id",
    "zone_id",
    "polygon_m",
    "people_capacity",
    "sleeping_capacity",
    "bed_points_m",
    "route_edge_ids",
    "source_ids",
    "start_minute",
    "end_minute",
    "active",
    "start_event_id",
    "end_event_id",
    "program_id",
    "effect_id",
];

/// A JSON object as a `V` map in the order of these keys.
fn in_order(value: &Value, keys: &[&str]) -> V {
    V::Map(keys.iter().map(|key| (text(key), v(value[*key].clone()))).collect())
}

/// `_room_world`: the room in the overlay's temporary rooms.
fn room_world(case: &Case, people: i64, beds: i64, room_id: &str) -> String {
    let record = in_order(&room_record(case, room_id, people, beds), &ROOM_KEYS);
    case.edit_state(OVERLAY_KEY, |state| {
        map_mut(at_mut(state, &["temporary_rooms"])).insert(text(room_id), record);
    });
    room_id.to_owned()
}

/// `_bed`: the core's bed for the person on a copy of the world: None, with the world as it was,
/// or the reservation it writes, whose answer and world `answers` chains.
fn bed(case: &mut Case, agent: &str, answers: Option<&mut Answers>) -> Option<Bed> {
    let held = case.state(OVERLAY_KEY).and_then(|state| match at(&state, &["bed_reservations"]) {
        V::Map(map) => map.get(agent).cloned(),
        _ => None,
    });
    let minute = case.minute();
    let snapshot = case.copy();
    let unchanged = case.fingerprint();
    let result = case.reserve(agent).unwrap();
    let Some(found) = result else {
        assert!(answers.is_none(), "a bed was expected");
        assert!(case.fingerprint() == unchanged, "no bed changes nothing");
        case.give_back(snapshot);
        return None;
    };
    let answers = answers.expect("no bed was expected");
    let target = overlay::bed_target_id(&found.room_id, found.bed_index);
    let reservation = held.unwrap_or_else(|| {
        v(json!({
            "agent_id": agent,
            "room_id": &*found.room_id,
            "bed_index": found.bed_index,
            "target_id": target,
            "reserved_minute": minute,
        }))
    });
    let state = case.state(OVERLAY_KEY).unwrap();
    assert_eq!(at(&state, &["bed_reservations", agent]), &reservation);
    assert_eq!(found.target_id, target);
    assert_eq!(V::Int(found.reserved_minute), at(&reservation, &["reserved_minute"]).clone());
    assert_eq!(found.path.last(), Some(&found.point));
    case.with(|world| answers.add(&format!("{found:?}"), world));
    case.give_back(snapshot);
    Some(found)
}

/// Recorded from the core in #225 lane M3; on the same kernel the pytest
/// `test_overlay_rules.py::test_a_bed_is_reserved_in_the_person_s_zone` with the digests `bed`
/// (`b440c65f6d92`) and `bed again` (`bb5a2af0d667`) passed.
#[test]
fn a_bed_is_reserved_in_the_person_s_zone() {
    let mut case = Case::new();
    room_world(&case, 4, 3, "room");
    let agent = case.person_in_room_zone();
    let mut first = Answers::default();
    let result = bed(&mut case, &agent, Some(&mut first)).unwrap();
    assert_eq!(&*result.room_id, "room");
    // Who holds a bed keeps it, with the minute it was reserved.
    case.reserve(&agent).unwrap();
    case.set_minute(3);
    let mut again = Answers::default();
    let held = bed(&mut case, &agent, Some(&mut again)).unwrap();
    assert_eq!((held.bed_index, held.reserved_minute), (result.bed_index, 0));
    assert_eq!(first.hex(), "bf3059996bd042be360517cfc5ca3de04d14eb112038ed6622c995503a73963b");
    assert_eq!(again.hex(), "ff56a33c6492c42ba27af0ab80237138cf837138cf2ccf9e2df6095f49653c68");
}

/// The rule's `state.setdefault("bed_reservations", {})` comes before the floor plan's check: a
/// state saved without the map gets it even when no bed follows.
#[test]
fn a_state_saved_before_beds_were_reserved_gets_an_empty_map_first() {
    let mut case = Case::new();
    case.edit_state(OVERLAY_KEY, |state| {
        map_mut(state).shift_remove("bed_reservations");
    });
    let agent = case.person_in_room_zone();
    case.entry["sections"]["facility_geometry"] = json!({"absent": true});
    case.edit_configs(|configs| {
        configs.as_object_mut().unwrap().remove("facility_geometry");
    });
    assert_eq!(case.reserve(&agent), Ok(None));
    assert_eq!(at(&case.state(OVERLAY_KEY).unwrap(), &["bed_reservations"]), &v(json!({})));
}

#[test]
fn no_bed_is_none() {
    let mut case = Case::new();
    let agent = case.person_in_room_zone();
    // No room, no room in the person's zone, an overlay before schema_version 2.
    bed(&mut case, &agent, None);
    room_world(&case, 2, 1, "room");
    let other = case.agents().into_iter().find(|id| *id != agent).unwrap();
    case.with(|world| world.rows_mut().unwrap().agents.get_mut(other.as_str()).unwrap().zone_id = text("corridor"));
    case.with(|world| world.row_mut::<MovementRow>(MOVEMENT).unwrap().positions.get_mut(other.as_str()).unwrap().zone_id = text("corridor"));
    bed(&mut case, &other, None);
    case.edit_configs(|configs| configs["geometry_overlay"]["schema_version"] = json!(1));
    bed(&mut case, &agent, None);
}

/// Where the bed is free, recorded from the core in #225 lane M3; on the same kernel the pytest
/// `test_overlay_rules.py::test_someone_at_a_bed_is_using_it_within_its_radius` with the digests
/// `bed beside 0.7500001` (`991c0f10289e`) and `bed beside 1.2` (`1c3bde66032b`) passed.
#[test]
fn someone_at_a_bed_is_using_it_within_its_radius() {
    for (offset, taken, recorded) in [(0.0, true, ""), (0.7499, true, ""), (0.7500001, false, "61f1a101aa58fda9ae45035317132396d731a902ce6ed91f851d647a5a227847"), (1.2, false, "8cdea670012f398730823bc68feb20dd0e31ac15be0a00b615debc7a107dec36")] {
        let mut case = Case::new();
        let record = room_record(&case, "room", 2, 1);
        room_world(&case, 2, 1, "room");
        let bed_point = &record["bed_points_m"][0];
        let people = case.in_room_zone();
        let (first, second) = (&people[0], &people[1]);
        case.put_point(second, Some(Pt { x: bed_point["x"].as_f64().unwrap() + offset, y: bed_point["y"].as_f64().unwrap() }));
        let mut answers = Answers::default();
        let result = bed(&mut case, first, (!taken).then_some(&mut answers));
        assert_eq!(result.is_none(), taken, "{offset}");
        if !taken {
            assert_eq!(answers.hex(), recorded, "{offset}");
        }
    }
}

/// Recorded from the core in #225 lane M3; on the same kernel the pytest
/// `test_overlay_rules.py::test_a_reservation_taken_by_another_is_passed_by` with the digest
/// `bed passed by` (`be2a03926e1d`) passed.
#[test]
fn a_reservation_taken_by_another_is_passed_by() {
    let mut case = Case::new();
    room_world(&case, 4, 2, "room");
    let people = case.in_room_zone();
    let held = case.reserve(&people[0]).unwrap().unwrap().bed_index;
    let mut answers = Answers::default();
    let result = bed(&mut case, &people[1], Some(&mut answers)).unwrap();
    assert_ne!(result.bed_index, held);
    assert_eq!(answers.hex(), "5b88f2c77dad7c560f77a6c376cd2df22e896ed598ddcfa24e1201df37065f53");
}

#[test]
fn a_release_drops_the_person_s_bed_only() {
    for held in [true, false] {
        let mut case = Case::new();
        room_world(&case, 4, 2, "room");
        let agent = case.person_in_room_zone();
        if held {
            case.reserve(&agent).unwrap();
        }
        let released = case.writes(
            |ours| ours.release("bed_reservations", &agent).unwrap(),
            |expected| {
                expected.edit_state(OVERLAY_KEY, |state| {
                    map_mut(at_mut(state, &["bed_reservations"])).shift_remove(agent.as_str());
                })
            },
        );
        assert_eq!(released, held);
    }
}

/// `_admission`.
fn admission(agent: &str, room: &str, minute: i64, event_id: &str) -> V {
    let mut sources = vec!["ASM-TEMP-ROOM-TEST-V1", INTAKE_SOURCE];
    sources.sort();
    // In the order of the Python rule's record.
    in_order(
        &json!({"agent_id": agent, "room_id": room, "admitted_minute": minute, "event_id": event_id, "source_ids": sources}),
        &["agent_id", "room_id", "admitted_minute", "event_id", "source_ids"],
    )
}

fn admitted(case: &mut Case, agent: &str, room: &str, event_id: &str) {
    let record = admission(agent, room, 0, event_id);
    case.edit_state(OVERLAY_KEY, |state| {
        map_mut(at_mut(state, &["room_admissions"])).insert(text(agent), record);
    });
}

fn released_admission(case: &mut Case, agent: &str) {
    case.edit_state(OVERLAY_KEY, |state| {
        map_mut(at_mut(state, &["room_admissions"])).shift_remove(agent);
    });
}

#[test]
fn a_room_takes_people_at_free_points_up_to_its_capacity() {
    let mut case = Case::new();
    let room = room_world(&case, 2, 1, "room");
    let record = case.state(OVERLAY_KEY).map(|state| at(&state, &["temporary_rooms", &room]).clone()).unwrap();
    // The bed is the middle of the room, so it is the first point the rule tries.
    let V::List(beds) = at(&record, &["bed_points_m"]) else { panic!("a list") };
    let bed_point = pt(&beds[0]);
    assert_eq!(case.room_admission_point(&room), Some(bed_point));
    assert_eq!(case.available_room(ROOM_ZONE).as_deref(), Some(room.as_str()));
    let agents = case.agents();
    let (first, second, third) = (&agents[0], &agents[1], &agents[2]);
    let admit = |agent: &str| {
        let agent = agent.to_owned();
        let room = room.clone();
        move |ours: &mut Case| ours.admit(&agent, &room, 0, "e1").unwrap()
    };
    assert_eq!(case.writes(admit(first), |expected| admitted(expected, first, &room, "e1")), bed_point);
    admit(first)(&mut case);
    // Someone standing there takes the point within half a metre, and not beyond it. Then the
    // nearest point of the 0.75 m grid that is clear of them: its first column and third row.
    let V::List(corners) = at(&record, &["polygon_m"]) else { panic!("a list") };
    let min_x = corners.iter().map(|corner| pt(corner).x).fold(f64::INFINITY, f64::min);
    let min_y = corners.iter().map(|corner| pt(corner).y).fold(f64::INFINITY, f64::min);
    let beside = Pt { x: min_x + 0.375, y: min_y + 0.375 + 0.75 + 0.75 };
    for (offset, expected) in [(0.0, beside), (0.4999, beside), (0.5, bed_point), (0.6, bed_point)] {
        case.put_point(first, Some(Pt { x: bed_point.x + offset, y: bed_point.y }));
        assert_eq!(case.room_admission_point(&room), Some(expected), "{offset}");
    }
    assert_eq!(case.writes(admit(second), |expected| admitted(expected, second, &room, "e1")), bed_point);
    admit(second)(&mut case);
    assert_eq!(case.available_room(ROOM_ZONE), None);
    assert!(case.writes(|ours| ours.release("room_admissions", first).unwrap(), |expected| released_admission(expected, first)));
    assert!(!case.writes(|ours| ours.release("room_admissions", third).unwrap(), |expected| released_admission(expected, third)));
    case.edit_state(OVERLAY_KEY, |state| {
        let room = map_mut(at_mut(state, &["temporary_rooms", "room"]));
        room.insert(text("active"), V::Bool(false));
        room.insert(text("end_event_id"), V::Str(text("end")));
    });
    let third = third.clone();
    case.raises_text(move |ours| ours.admit(&third, "room", 0, "e1"), NO_ROOM);
}

#[test]
fn an_admission_says_the_python_rule_s_text_whatever_is_missing() {
    let fresh = || {
        let case = Case::new();
        let room = room_world(&case, 1, 1, "room");
        let agent = case.person_in_room_zone();
        (case, room, agent)
    };
    let (mut case, room, agent) = fresh();
    for (agent_id, room_id, minute) in [(agent.as_str(), "no-such-room", 0), ("nobody", room.as_str(), 0), (agent.as_str(), room.as_str(), -1)] {
        let (agent_id, room_id) = (agent_id.to_owned(), room_id.to_owned());
        case.raises_text(move |ours| ours.admit(&agent_id, &room_id, minute, "e"), NO_ROOM);
    }
    let (mut without_admissions, _, _) = fresh();
    without_admissions.edit_state(OVERLAY_KEY, |state| {
        map_mut(state).shift_remove("room_admissions");
    });
    let (who, which) = (agent.clone(), room.clone());
    without_admissions.raises_text(move |ours| ours.admit(&who, &which, 0, "e"), NO_ROOM);
    let (mut without_overlay, _, _) = fresh();
    without_overlay.set_state(OVERLAY_KEY, None);
    let (who, which) = (agent.clone(), room.clone());
    without_overlay.raises_text(move |ours| ours.admit(&who, &which, 0, "e"), NO_ROOM);
    let found = case.writes(
        |ours| ours.admit(&agent, &room, 0, "e"),
        |expected| {
            let record = admission(&agent, &room, 0, "e");
            expected.edit_state(OVERLAY_KEY, |state| {
                map_mut(at_mut(state, &["room_admissions"])).insert(text(&agent), record);
            });
        },
    );
    assert!(found.is_ok());
}

#[test]
fn a_large_room_gives_its_grid_point_nearest_the_middle() {
    let mut case = Case::new();
    let room = room_world(&case, 2, 0, "room");
    let zones = case.entry["world"]["model_configs"]["facility_geometry"]["zones"].clone();
    let sleeping = zones.as_array().unwrap().iter().find(|zone| zone["zone_id"] == ROOM_ZONE).unwrap();
    let polygon = v(sleeping["polygon"].clone());
    case.edit_state(OVERLAY_KEY, |state| {
        map_mut(at_mut(state, &["temporary_rooms", &room])).insert(text("polygon_m"), polygon);
    });
    // The Python rule's point for the generated sleeping zone (lane L10).
    assert_eq!(case.room_admission_point(&room), Some(Pt { x: 4.125, y: 7.425 }));
}

// ---- A facility placed during a Run --------------------------------------------------------------

/// Where the Python rule placed four toilets in the generated toilet zone, in turn (lane L10). The
/// zone has no room for a fifth.
const PLACED: [(f64, f64); 4] = [
    (13.62417562100671, 9.737087810503356),
    (14.524175621006712, 9.737087810503356),
    (13.62417562100671, 11.21867753513065),
    (14.524175621006712, 11.21867753513065),
];

/// `_new_toilet`: a toilet with no point in the plan.
fn new_toilet(case: &Case, facility_id: &str) -> String {
    case.with(|world| {
        let row = FacilityRow {
            id: text(facility_id),
            kind: text("toilet"),
            zone_id: text("toilet"),
            capacity: 1,
            working_units: 1,
            active_agent_ids: Vec::new(),
            queue_agent_ids: Vec::new(),
            resource_active_agent_ids: Vec::new(),
            resource_queue_agent_ids: Vec::new(),
        };
        world.rows_mut().unwrap().facilities.insert(text(facility_id), row);
    });
    facility_id.to_owned()
}

/// `_placed`: what the deleted Python rule wrote for a placement.
fn placed(case: &mut Case, facility_id: &str, point: Pt, event_id: &str, sources: &[&str]) {
    let mut names: Vec<&str> = sources.to_vec();
    names.push(PLACEMENT_SOURCE);
    names.sort();
    names.dedup();
    let minute = case.minute();
    let common = |record: &mut indexmap::IndexMap<Arc<str>, V>| {
        record.insert(text("event_id"), V::Str(text(event_id)));
        record.insert(text("source_ids"), v(json!(names)));
        record.insert(text("minute"), V::Int(minute));
    };
    let mut point_record = indexmap::IndexMap::new();
    point_record.insert(text("point_m"), point_v(point.x, point.y));
    common(&mut point_record);
    let digest = sha256_hex(format!("{event_id}:{facility_id}").as_bytes());
    let mut history = indexmap::IndexMap::new();
    history.insert(text("transition_id"), V::Str(text(&format!("install-{}", &digest[..24]))));
    history.insert(text("op"), V::Str(text("install_facility")));
    history.insert(text("target_id"), V::Str(text(facility_id)));
    history.insert(text("status"), V::Str(text("applied")));
    history.insert(text("causation_id"), V::Str(text(event_id)));
    common(&mut history);
    case.edit_state(OVERLAY_KEY, |state| {
        map_mut(at_mut(state, &["facility_points"])).insert(text(facility_id), V::Map(point_record));
        list_mut(at_mut(state, &["history"])).push(V::Map(history));
    });
}

#[test]
fn a_placement_takes_the_next_free_slot_and_records_it() {
    let mut case = Case::new();
    for (index, (x, y)) in PLACED.into_iter().enumerate() {
        let facility = new_toilet(&case, &format!("extra_{index}"));
        let point = Pt { x, y };
        assert_eq!(case.installed_point(&facility), Some(point));
        let event_id = format!("e{index}");
        let placement = case.writes(|ours| ours.place(&facility, &event_id, &["ASM-A", "ASM-A"]).unwrap(), |expected| placed(expected, &facility, point, &event_id, &["ASM-A"]));
        assert_eq!(placement, point);
        case.place(&facility, &event_id, &["ASM-A"]).unwrap();
    }
    let (schema, overlay) = (case.schema(), case.held(OVERLAY_KEY));
    assert_eq!(overlay::placement_enabled(&schema, &overlay), Ok(true));
    case.edit_configs(|configs| configs["geometry_overlay"]["schema_version"] = json!(3));
    let (schema, overlay) = (case.schema(), case.held(OVERLAY_KEY));
    assert_eq!(overlay::placement_enabled(&schema, &overlay), Ok(false));
}

#[test]
fn no_free_point_says_the_python_rule_s_text() {
    let mut case = Case::new();
    // 16 grids more than fit: the search gives up where the Python rule did.
    for (index, (x, y)) in PLACED.into_iter().enumerate() {
        let facility = new_toilet(&case, &format!("extra_{index}"));
        assert_eq!(case.place(&facility, &format!("e{index}"), &[]), Ok(Pt { x, y }));
    }
    let facility = new_toilet(&case, "extra_4");
    assert_eq!(case.installed_point(&facility), None);
    case.raises_text(move |ours| ours.place(&facility, "e", &[]), "no free floor-plan point for installed facility 'extra_4'");
}

// ---- Changes of the floor plan -------------------------------------------------------------------

/// `_event`: an approved change of the floor plan, due now.
fn event(case: &Case, op: &str, target: &str, parameters: &Value, end: Option<i64>, event_id: &str) -> EventRow {
    EventRow {
        event_id: text(event_id),
        kind: EventKind::Endogenous,
        due_minute: case.minute(),
        priority: 0,
        sequence: 900,
        event_type: text("effect_program_geometry"),
        payload: V::Map(
            [
                ("program_id", v(json!("program"))),
                ("effect_id", v(json!("effect"))),
                ("modifier_id", v(json!("modifier"))),
                ("target_id", v(json!(target))),
                ("op", v(json!(op))),
                ("parameters", v(parameters.clone())),
                ("end_minute", end.map_or(V::Null, V::Int)),
            ]
            .into_iter()
            .map(|(key, value)| (text(key), value))
            .collect(),
        ),
        source_id: Some(text("ASM-SOURCE")),
        causation_id: Some(text("cause")),
        provenance: v(json!({"effect_program_v1": {"assumption_ids": ["ASM-ASSUME", "ASM-SOURCE", ""]}})),
    }
}

/// `_corner`: a small square near a zone's corner.
fn corner(case: &Case, zone_id: &str) -> Value {
    let zones = case.entry["world"]["model_configs"]["facility_geometry"]["zones"].as_array().unwrap();
    let zone = zones.iter().find(|zone| zone["zone_id"] == zone_id).unwrap();
    let corners = zone["polygon"].as_array().unwrap();
    let left = corners.iter().map(|point| point["x"].as_f64().unwrap()).fold(f64::INFINITY, f64::min) + 1.0;
    let top = corners.iter().map(|point| point["y"].as_f64().unwrap()).fold(f64::INFINITY, f64::min) + 1.0;
    json!([{"x": left, "y": top}, {"x": left + 0.4, "y": top}, {"x": left + 0.4, "y": top + 0.4}, {"x": left, "y": top + 0.4}])
}

/// The keys of the room a change asks for (`_ROOM_FIELDS`).
const ROOM_FIELDS: [&str; 9] = ["schema_version", "room_id", "zone_id", "polygon_m", "people_capacity", "sleeping_capacity", "bed_points_m", "route_edge_ids", "source_ids"];

/// `_change_cases`: each check's way through.
fn change_cases(case: &Case) -> Vec<(&'static str, String, Value, Option<i64>)> {
    let toilet = case.facility(|kind| kind == "toilet");
    let point = case.entry["world"]["model_configs"]["facility_geometry"]["facility_points"][&toilet].clone();
    let (x, y) = (point["x"].as_f64().unwrap(), point["y"].as_f64().unwrap());
    let record = room_record(case, "new-room", 2, 1);
    let room: serde_json::Map<String, Value> = ROOM_FIELDS.iter().map(|key| (key.to_string(), record[*key].clone())).collect();
    let room = Value::Object(room);
    let toilet_zone = case.with(|world| world.rows().unwrap().facilities.get(toilet.as_str()).unwrap().zone_id.to_string());
    let partition = json!({"polygon_m": corner(case, ROOM_ZONE)});
    vec![
        ("set_route_access", "route_corridor_sleeping".into(), json!({"accessible": false}), Some(50)),
        ("set_route_access", "route_corridor_sleeping".into(), json!({"accessible": true}), Some(50)),
        ("set_route_access", "route_corridor_sleeping".into(), json!({"accessible": false}), None),
        ("set_route_access", "no_route".into(), json!({"accessible": false}), Some(50)),
        ("move_facility", toilet.clone(), json!({"point_m": {"x": x + 0.1, "y": y}}), None),
        ("move_facility", toilet.clone(), json!({"point_m": {"x": x + 0.1, "y": y}}), Some(50)),
        ("move_facility", toilet.clone(), json!({"point_m": {"x": -1.0, "y": y}}), None),
        ("move_facility", "no_facility".into(), json!({"point_m": {"x": 1.0, "y": 1.0}}), None),
        ("set_partition", ROOM_ZONE.into(), partition.clone(), Some(50)),
        ("set_partition", ROOM_ZONE.into(), partition.clone(), None),
        ("set_partition", "no_zone".into(), partition, Some(50)),
        ("temporary_room_geometry", ROOM_ZONE.into(), room.clone(), Some(50)),
        ("temporary_room_geometry", ROOM_ZONE.into(), room.clone(), None),
        ("temporary_room_geometry", "corridor".into(), room, Some(50)),
        (
            "transfer_equipment",
            toilet.clone(),
            json!({"schema_version": 1, "equipment_id": toilet, "from_zone_id": toilet_zone, "to_zone_id": "sleeping", "point_m": {"x": 5.0, "y": 9.0}}),
            None,
        ),
        ("no_such_operator", ROOM_ZONE.into(), json!({}), Some(50)),
    ]
}

/// What the Python rule said of each of `change_cases`, in order: None where it passed (lane L10).
const CHANGE_TEXTS: [Option<&str>; 16] = [
    None,
    None,
    Some("expiry_required: route-access changes require a bounded end"),
    Some("target_missing: route does not exist"),
    None,
    Some("unsupported_move_expiry: same-zone moves are permanent; approve an explicit later move"),
    Some("area_invalid: cross-zone facility moves are unsupported"),
    Some("target_missing: facility has no surveyed position"),
    None,
    Some("expiry_required: partitions require an explicit bounded end"),
    Some("target_missing: zone does not exist"),
    None,
    Some("expiry_required: temporary rooms need an in-run expiry"),
    Some("target_mismatch: temporary room zone must match its selected zone"),
    None,
    Some("unsupported_geometry_operator"),
];

/// The contract's answer for a change's parameters (`Model.model_validate(...).model_dump()`).
fn dumped(case: &Case, model: &str, parameters: &Value) -> V {
    let row = case.entry["contracts"].as_array().unwrap().iter().find(|row| row[0] == model && row[1] == *parameters);
    ordered(&row.unwrap_or_else(|| panic!("no answer of {model} for {parameters}"))[2])
}

/// `_applied`: what the deleted Python rule of `apply_geometry_change` wrote for an approved change.
fn applied(case: &mut Case, event: &EventRow, expire: bool) {
    let payload = json_of(&event.payload);
    let (op, target, modifier) = (payload["op"].as_str().unwrap(), payload["target_id"].as_str().unwrap(), payload["modifier_id"].as_str().unwrap());
    let parameters = &payload["parameters"];
    let marker = json_of(&event.provenance).get("effect_program_v1").cloned().unwrap_or(json!({}));
    let mut named: Vec<String> = event.source_id.iter().map(|id| id.to_string()).collect();
    for list in [parameters.get("source_ids"), marker.get("assumption_ids")].into_iter().flatten() {
        named.extend(list.as_array().unwrap().iter().map(|id| id.as_str().unwrap().to_owned()));
    }
    named.retain(|id| !id.is_empty());
    named.sort();
    named.dedup();
    let minute = case.minute();
    let common = |record: &mut indexmap::IndexMap<Arc<str>, V>| {
        record.insert(text("event_id"), V::Str(event.event_id.clone()));
        record.insert(text("source_ids"), v(json!(named)));
        record.insert(text("minute"), V::Int(minute));
    };
    let mut history_target = target.to_owned();
    let mut overlay = case.state(OVERLAY_KEY).unwrap();
    match op {
        "move_facility" | "transfer_equipment" => {
            if op == "transfer_equipment" {
                let to = parameters["to_zone_id"].as_str().unwrap();
                case.with(|world| world.rows_mut().unwrap().facilities.get_mut(target).unwrap().zone_id = text(to));
            }
            let point = &parameters["point_m"];
            let mut record = indexmap::IndexMap::new();
            record.insert(text("point_m"), point_v(point["x"].as_f64().unwrap(), point["y"].as_f64().unwrap()));
            common(&mut record);
            map_mut(at_mut(&mut overlay, &["facility_points"])).insert(text(target), V::Map(record));
        }
        "set_partition" => {
            let partitions = map_mut(at_mut(&mut overlay, &["partitions"]));
            if expire {
                partitions.shift_remove(modifier);
            } else {
                let polygon = parameters["polygon_m"].as_array().unwrap().iter().map(|point| point_v(point["x"].as_f64().unwrap(), point["y"].as_f64().unwrap())).collect();
                let mut record = indexmap::IndexMap::new();
                record.insert(text("zone_id"), V::Str(text(target)));
                record.insert(text("polygon_m"), V::List(polygon));
                common(&mut record);
                record.insert(text("end_minute"), v(payload["end_minute"].clone()));
                partitions.insert(text(modifier), V::Map(record));
            }
        }
        "set_route_access" => {
            let closed = case.with(|world| world.rows().unwrap().zone_edges.get(target).unwrap().closed);
            let routes = map_mut(at_mut(&mut overlay, &["route_modifiers"]));
            let record = routes.entry(text(target)).or_insert_with(|| v(json!({"base_closed": closed, "modifiers": {}})));
            {
                let modifiers = map_mut(at_mut(record, &["modifiers"]));
                if expire {
                    modifiers.shift_remove(modifier);
                } else {
                    let mut item = indexmap::IndexMap::new();
                    item.insert(text("accessible"), v(parameters["accessible"].clone()));
                    common(&mut item);
                    item.insert(text("end_minute"), v(payload["end_minute"].clone()));
                    modifiers.insert(text(modifier), V::Map(item));
                }
            }
            let base = at(record, &["base_closed"]) == &V::Bool(true);
            let V::Map(modifiers) = at(record, &["modifiers"]) else { panic!("a map") };
            let shut = base || modifiers.values().any(|item| at(item, &["accessible"]) != &V::Bool(true));
            let empty = modifiers.is_empty();
            case.with(|world| world.rows_mut().unwrap().zone_edges.get_mut(target).unwrap().closed = shut);
            if empty {
                routes.shift_remove(target);
            }
        }
        _ => {
            let room = dumped(case, "TemporaryRoomGeometry", parameters);
            let room_id = match at(&room, &["room_id"]) {
                V::Str(id) => id.to_string(),
                other => panic!("a room ID: {other:?}"),
            };
            history_target = room_id.clone();
            let rooms = map_mut(at_mut(&mut overlay, &["temporary_rooms"]));
            if expire {
                let record = map_mut(rooms.get_mut(room_id.as_str()).unwrap());
                record.insert(text("active"), V::Bool(false));
                record.insert(text("end_event_id"), V::Str(event.event_id.clone()));
            } else {
                let V::Map(mut record) = room else { panic!("a map") };
                record.insert(text("start_minute"), V::Int(minute));
                record.insert(text("end_minute"), v(payload["end_minute"].clone()));
                record.insert(text("active"), V::Bool(true));
                record.insert(text("start_event_id"), V::Str(event.event_id.clone()));
                record.insert(text("end_event_id"), V::Null);
                record.insert(text("program_id"), v(payload["program_id"].clone()));
                record.insert(text("effect_id"), v(payload["effect_id"].clone()));
                record.insert(text("source_ids"), v(json!(named)));
                rooms.insert(text(&room_id), V::Map(record));
            }
        }
    }
    let mut history = indexmap::IndexMap::new();
    history.insert(text("transition_id"), V::Str(event.event_id.clone()));
    history.insert(text("op"), V::Str(text(op)));
    history.insert(text("target_id"), V::Str(text(&history_target)));
    history.insert(text("status"), V::Str(text(if expire { "expired" } else { "applied" })));
    history.insert(text("causation_id"), event.causation_id.clone().map_or(V::Null, V::Str));
    common(&mut history);
    let records = list_mut(at_mut(&mut overlay, &["history"]));
    records.push(V::Map(history));
    let excess = records.len().saturating_sub(20_000);
    records.drain(..excess);
    case.set_state(OVERLAY_KEY, Some(overlay));
}

#[test]
fn each_change_is_checked_and_applied_as_the_python_rule_did() {
    let cases = change_cases(&Case::new());
    for ((op, target, parameters, end), said) in cases.into_iter().zip(CHANGE_TEXTS) {
        let mut case = Case::new();
        let event = event(&case, op, &target, &parameters, end, "change-1");
        if let Some(said) = said {
            // The check comes first, so applying says the same.
            let (target_id, given) = (target.clone(), parameters.clone());
            case.raises_text(move |ours| ours.validate(op, &target_id, &given, end), said);
            let change = event.clone();
            case.raises_text(move |ours| ours.apply(&change, false), said);
            continue;
        }
        assert_eq!(case.validate(op, &target, &parameters, end), Ok(()), "{op} {target}");
        case.writes(|ours| ours.apply(&event, false).unwrap(), |expected| applied(expected, &event, false));
    }
}

#[test]
fn a_change_without_the_overlay_says_the_python_rule_s_text() {
    let mut case = Case::new();
    case.set_state(OVERLAY_KEY, None);
    let (op, target, parameters, end) = change_cases(&case).remove(0);
    let change = event(&case, op, &target, &parameters, end, "change-1");
    case.raises_text(move |ours| ours.apply(&change, false), "geometry_overlay_required");
    case.raises_text(
        move |ours| ours.validate(op, &target, &parameters, end),
        "geometry_overlay_required: declare the versioned geometry overlay before approving effects",
    );
}

#[test]
fn a_change_expires_as_the_python_rule_did() {
    for op in ["set_partition", "set_route_access", "temporary_room_geometry"] {
        let mut case = Case::new();
        let (_, target, parameters, end) = change_cases(&case).into_iter().find(|item| item.0 == op && item.3 == Some(50)).unwrap();
        case.apply(&event(&case, op, &target, &parameters, end, "change-1"), false).unwrap();
        let ending = event(&case, op, &target, &parameters, end, "change-2");
        case.writes(|ours| ours.apply(&ending, true).unwrap(), |expected| applied(expected, &ending, true));
    }
}

#[test]
fn a_full_overlay_history_drops_its_oldest_record() {
    let mut case = Case::new();
    let old: Vec<V> = (0..20_000)
        .map(|index| {
            v(json!({
                "transition_id": format!("old{index}"),
                "op": "set_partition",
                "target_id": ROOM_ZONE,
                "status": "applied",
                "causation_id": null,
                "event_id": format!("old{index}"),
                "source_ids": [],
                "minute": 0,
            }))
        })
        .collect();
    case.edit_state(OVERLAY_KEY, |state| list_mut(at_mut(state, &["history"])).extend(old));
    let change = event(&case, "set_partition", ROOM_ZONE, &json!({"polygon_m": corner(&case, ROOM_ZONE)}), Some(50), "change-1");
    case.writes(|ours| ours.apply(&change, false).unwrap(), |expected| applied(expected, &change, false));
    case.apply(&change, false).unwrap();
    let state = case.state(OVERLAY_KEY).unwrap();
    let V::List(history) = at(&state, &["history"]) else { panic!("a list") };
    assert_eq!(history.len(), 20_000);
    assert_eq!(at(&history[0], &["transition_id"]), &V::Str(text("old1")));
    assert_eq!(at(&history[19_999], &["transition_id"]), &V::Str(change.event_id.clone()));
}

// ---- Next to a coefficient or a limit ------------------------------------------------------------

/// Everyone else in the toilet zone this far from the first slot of its grid (the slot the pytest
/// computed in Python: `separation`), on the side away from the zone's middle.
#[test]
fn a_placed_facility_keeps_its_separation_from_the_others() {
    for distance in [0.5999, 0.6, 0.6001, 0.603] {
        let mut case = Case::new();
        let facility = new_toilet(&case, "extra");
        let separation = case.entry["separation"].clone();
        let (x, y) = (separation["slot"][0].as_f64().unwrap(), separation["slot"][1].as_f64().unwrap());
        let others: Vec<String> = separation["others"].as_array().unwrap().iter().map(|id| id.as_str().unwrap().to_owned()).collect();
        case.edit_geometry(|geometry| {
            for key in &others {
                geometry["facility_points"][key] = json!({"x": x, "y": y + distance});
            }
        });
        let point = case.installed_point(&facility).unwrap();
        // 0.6 itself is what the floats make of it: there the Python rule passed the slot by.
        assert_eq!(point == Pt { x, y }, distance > 0.6, "{distance}");
    }
}

/// Free only near a slot of the given grid (the last slots of the grids of 16 and 17 more
/// facilities than the zone has, which the pytest computed in Python): each room blocks one side.
#[test]
fn the_search_of_a_grid_ends_after_sixteen_grids() {
    let case = Case::new();
    let facility = new_toilet(&case, "extra");
    let zones = case.entry["world"]["model_configs"]["facility_geometry"]["zones"].clone();
    let zone = zones.as_array().unwrap().iter().find(|zone| zone["zone_id"] == "toilet").unwrap();
    let corners = zone["polygon"].as_array().unwrap();
    let xs: Vec<f64> = corners.iter().map(|point| point["x"].as_f64().unwrap()).collect();
    let ys: Vec<f64> = corners.iter().map(|point| point["y"].as_f64().unwrap()).collect();
    let low = |values: &[f64]| values.iter().copied().fold(f64::INFINITY, f64::min);
    let high = |values: &[f64]| values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    for grid in case.entry["sixteen_grids"].as_array().unwrap().clone() {
        let slot = Pt { x: grid["slot"][0].as_f64().unwrap(), y: grid["slot"][1].as_f64().unwrap() };
        let gap = 0.001;
        let sides = [
            (low(&xs) - 1.0, low(&ys) - 1.0, slot.x - gap, high(&ys) + 1.0),
            (slot.x + gap, low(&ys) - 1.0, high(&xs) + 1.0, high(&ys) + 1.0),
            (slot.x - gap, low(&ys) - 1.0, slot.x + gap, slot.y - gap),
            (slot.x - gap, slot.y + gap, slot.x + gap, high(&ys) + 1.0),
        ];
        case.edit_state(OVERLAY_KEY, |state| {
            let rooms = map_mut(at_mut(state, &["temporary_rooms"]));
            rooms.clear();
            for (index, (left, top, right, bottom)) in sides.into_iter().enumerate() {
                let polygon = json!([{"x": left, "y": top}, {"x": right, "y": top}, {"x": right, "y": bottom}, {"x": left, "y": bottom}]);
                rooms.insert(text(&format!("block{index}")), v(json!({"active": true, "polygon_m": polygon})));
            }
        });
        let found = grid["found"].as_bool().unwrap();
        assert_eq!(case.installed_point(&facility), found.then_some(slot), "{grid}");
    }
}

/// Points either side of each wall, just inside and just past half a metre (computed in Python:
/// `walls`), and what the Python rule said of each.
#[test]
fn a_moved_facility_keeps_half_a_metre_from_walls() {
    let mut case = Case::new();
    let toilet = case.facility(|kind| kind == "toilet");
    let mut said: std::collections::BTreeMap<(String, Option<String>), i64> = Default::default();
    for wall in case.entry["walls"].as_array().unwrap().clone() {
        let parameters = json!({"point_m": wall["point"]});
        let expected = wall["expected"].as_str().map(str::to_owned);
        match &expected {
            None => assert_eq!(case.validate("move_facility", &toilet, &parameters, None), Ok(())),
            Some(text) => {
                let target = toilet.clone();
                case.raises_text(move |ours| ours.validate("move_facility", &target, &parameters, None), text);
            }
        }
        *said.entry((wall["clearance"].to_string(), expected)).or_insert(0) += 1;
    }
    // What the Python rule said of these points of the generated plan (lane L10).
    let cross = Some("area_invalid: cross-zone facility moves are unsupported".to_owned());
    let narrow = Some("clear_width_insufficient: moved service point requires half a metre from walls".to_owned());
    let wanted: std::collections::BTreeMap<(String, Option<String>), i64> =
        [(("0.499".to_owned(), cross.clone()), 58), (("0.499".to_owned(), narrow), 8), (("0.503".to_owned(), cross), 58), (("0.503".to_owned(), None), 8)].into_iter().collect();
    assert_eq!(said, wanted);
}

/// The rule routes at most 256 beds: the free bed after 256 others is never tried. Where it is
/// found, recorded from the core in #225 lane M3; on the same kernel the pytest
/// `test_overlay_rules.py::test_a_bed_s_routes_end_where_the_python_rule_s_do[255-True]` with the
/// digest `bed routes 255` (`af665afe7480`) passed.
#[test]
fn a_bed_s_routes_end_where_the_python_rule_s_do() {
    for (blocked, found) in [(255, true), (256, false)] {
        let mut case = Case::new();
        let mut record = room_record(&case, "room", 400, 1);
        let free = record["bed_points_m"][0].clone();
        let corner = &record["polygon_m"][0];
        let (cx, cy) = (corner["x"].as_f64().unwrap() + 0.2, corner["y"].as_f64().unwrap() + 0.2);
        // Beds no route reaches, each one try, ahead of the bed that has a route.
        let mut beds = vec![json!({"x": cx, "y": cy}); blocked];
        beds.push(free);
        record["sleeping_capacity"] = json!(beds.len());
        record["bed_points_m"] = json!(beds);
        let record = in_order(&record, &ROOM_KEYS);
        case.edit_state(OVERLAY_KEY, |state| {
            map_mut(at_mut(state, &["temporary_rooms"])).insert(text("room"), record);
        });
        case.edit_spatial(|spatial| {
            let obstacle = json!({
                "id": "around-the-corner",
                "polygon": [
                    {"x": cx - 0.05, "y": cy - 0.05},
                    {"x": cx + 0.05, "y": cy - 0.05},
                    {"x": cx + 0.05, "y": cy + 0.05},
                    {"x": cx - 0.05, "y": cy + 0.05},
                ],
                "source_ids": [],
            });
            spatial.as_object_mut().unwrap().entry("obstacles").or_insert(json!([])).as_array_mut().unwrap().push(obstacle);
        });
        let agent = case.person_in_room_zone();
        let mut answers = Answers::default();
        let result = bed(&mut case, &agent, found.then_some(&mut answers));
        if found {
            assert_eq!(result.unwrap().bed_index, blocked as i64);
            assert_eq!(answers.hex(), "27a8ea689f7bd565bd3f1dbe9df4b2d198e9976cd729e061b61ae46798c0f3bd");
        } else {
            assert!(result.is_none());
        }
    }
}

/// Only the `free`-th point of the grid and later ones can be walked to.
#[test]
fn a_room_inspects_256_points_of_its_grid() {
    for (free, found) in [(250, true), (256, false)] {
        let mut case = Case::new();
        let room = room_world(&case, 2, 0, "room");
        // A hall wider than a generated zone, whose grid has more points than the rule looks at.
        let geometry = &case.entry["world"]["model_configs"]["facility_geometry"];
        let (x0, side) = (geometry["width_m"].as_f64().unwrap(), 20.0);
        let height = geometry["height_m"].as_f64().unwrap().max(side);
        let y0 = 0.0;
        let polygon = json!([{"x": x0, "y": y0}, {"x": x0 + side, "y": y0}, {"x": x0 + side, "y": y0 + side}, {"x": x0, "y": y0 + side}]);
        case.edit_geometry(|geometry| {
            geometry["width_m"] = json!(x0 + side);
            geometry["height_m"] = json!(height);
            let zone = geometry["zones"].as_array_mut().unwrap().iter_mut().find(|zone| zone["zone_id"] == ROOM_ZONE).unwrap();
            zone["polygon"] = polygon.clone();
        });
        let room_polygon = v(polygon.clone());
        case.edit_state(OVERLAY_KEY, |state| {
            map_mut(at_mut(state, &["temporary_rooms", &room])).insert(text("polygon_m"), room_polygon);
        });
        let mut rows = 0;
        let mut y = y0 + 0.375;
        while y < y0 + side {
            rows += 1;
            y += 0.75;
        }
        let (columns, partial) = (free / rows, free % rows);
        // An L around the first columns and the top of the next one.
        let (left, top, bottom) = (x0 - 1.0, y0 - 1.0, y0 + side + 1.0);
        let wall_x = x0 + 0.75 * columns as f64;
        let step_y = y0 + 0.75 * partial as f64;
        let corners = [(left, top), (wall_x + 0.75, top), (wall_x + 0.75, step_y), (wall_x, step_y), (wall_x, bottom), (left, bottom)];
        case.edit_spatial(|spatial| {
            let obstacle = json!({
                "id": "all-but-the-last",
                "polygon": corners.iter().map(|(x, y)| json!({"x": x, "y": y})).collect::<Vec<_>>(),
                "source_ids": [],
            });
            spatial.as_object_mut().unwrap().entry("obstacles").or_insert(json!([])).as_array_mut().unwrap().push(obstacle);
        });
        let point = case.room_admission_point(&room);
        if !found {
            assert_eq!(point, None);
            continue;
        }
        // The last point inspected is the free one nearest the middle, as the grid steps it.
        let last = limit(&case, "room_points") - 1;
        let (column, row) = (last / rows, last % rows);
        let mut x = x0 + 0.375;
        for _ in 0..column {
            x += 0.75;
        }
        let mut y = y0 + 0.375;
        for _ in 0..row {
            y += 0.75;
        }
        assert_eq!(point, Some(Pt { x, y }));
    }
}

#[test]
fn incoming_travelers_are_the_people_walking_to_the_zone_or_its_facilities() {
    let case = Case::new();
    assert!(case.with(|world| world.rows().unwrap().agents.iter().all(|(_, agent)| agent.activity != Activity::Moving)));
    let agents = case.agents();
    let (first, second) = (&agents[0], &agents[1]);
    let toilet = case.facility(|kind| kind.contains("toilet"));
    let toilet_zone = case.with(|world| world.rows().unwrap().facilities.get(toilet.as_str()).unwrap().zone_id.to_string());
    case.with(|world| {
        let mut rows = world.rows_mut().unwrap();
        let agent = rows.agents.get_mut(first.as_str()).unwrap();
        agent.activity = Activity::Moving;
        agent.target_id = Some(text(ROOM_ZONE));
        let agent = rows.agents.get_mut(second.as_str()).unwrap();
        agent.activity = Activity::Moving;
        agent.target_id = Some(text(&toilet));
    });
    let targets = [(first, ROOM_ZONE.to_owned()), (second, toilet_zone.clone())];
    for zone in [ROOM_ZONE.to_owned(), toilet_zone, "corridor".to_owned()] {
        for excluding in [None, Some(first), Some(second)] {
            let expected = targets.iter().filter(|(agent, target)| *target == zone && Some(*agent) != excluding).count() as i64;
            let counted = case.with(|world| overlay::incoming_travelers(world, &zone, excluding.map(String::as_str)));
            assert_eq!(counted, Ok(expected), "{zone} {excluding:?}");
        }
    }
}

#[test]
fn capacity_counts_signed_rooms_and_the_people_an_expired_room_admitted() {
    let mut case = Case::new();
    let room = room_world(&case, 3, 1, "room");
    case.edit_state(OVERLAY_KEY, |state| {
        map_mut(at_mut(state, &["room_admissions"]))
            .insert(text("A001"), v(json!({"agent_id": "A001", "room_id": room, "admitted_minute": 0, "event_id": "e", "source_ids": []})));
    });
    set_capacity(&case, ROOM_ZONE, Some(2));
    case.set_state(SPACES_KEY, None);
    let metrics = |rooms: i64, admitted: i64, retained: i64| {
        json!({
            "nominal_capacity": 2,
            "effective_capacity": 2,
            "temporary_people_capacity": 0,
            "retained_expired_capacity": 0,
            "temporary_sleeping_capacity": 0,
            "physical_room_capacity": rooms,
            "active_room_admissions": admitted,
            "retained_expired_room_admissions": retained,
            "effective_total_capacity": 2 + rooms + retained,
        })
    };
    let schema = case.entry["world"]["model_configs"]["geometry_overlay"]["schema_version"].as_i64().unwrap();
    assert!([3, 4].contains(&schema));
    assert_eq!(case.capacity_metrics(ROOM_ZONE, Some(9)), metrics(3, 1, 0));
    let set_room = |case: &Case, active: bool, end: V| {
        case.edit_state(OVERLAY_KEY, |state| {
            let record = map_mut(at_mut(state, &["temporary_rooms", "room"]));
            record.insert(text("active"), V::Bool(active));
            record.insert(text("end_event_id"), end);
        });
    };
    set_room(&case, false, V::Str(text("end")));
    assert_eq!(case.capacity_metrics(ROOM_ZONE, Some(9)), metrics(0, 0, 1));
    // Before schema 3 a room adds no capacity, though its admissions are counted.
    case.edit_configs(|configs| configs["geometry_overlay"]["schema_version"] = json!(2));
    set_room(&case, true, V::Null);
    assert_eq!(case.capacity_metrics(ROOM_ZONE, Some(9)), metrics(0, 1, 0));
}

/// A move changes the plan, its service points and footprints, for the rules that read them. The
/// deleted Python rule's walk to the moved toilet was recorded as its `repr` (`MOVED_TOILET_WALK`):
/// the zone route (from the toilet zone to itself, 1 minute) and its points, whose floats this
/// keeps as they were.
#[test]
fn a_moved_facility_keeps_its_plan_in_rust() {
    let mut case = Case::new();
    let toilet = case.facility(|kind| kind == "toilet");
    let point = case.entry["world"]["model_configs"]["facility_geometry"]["facility_points"][&toilet].clone();
    let (x, y) = (point["x"].as_f64().unwrap(), point["y"].as_f64().unwrap());
    let change = event(&case, "move_facility", &toilet, &json!({"point_m": {"x": x + 0.1, "y": y}}), None, "change-1");
    case.apply(&change, false).unwrap();
    let plan = case.plan().unwrap();
    let layout = plan.layout.as_ref().unwrap();
    assert!((layout.facility_points[toilet.as_str()].x - (x + 0.1)).abs() < 1e-9);
    let agent = case.agents().remove(0);
    let zone = case.with(|world| world.rows().unwrap().facilities.get(toilet.as_str()).unwrap().zone_id.clone());
    case.with(|world| world.rows_mut().unwrap().agents.get_mut(agent.as_str()).unwrap().zone_id = zone.clone());
    let center = layout.zones[&*zone].center.unwrap().unwrap();
    case.put_point(&agent, Some(center));
    let walking = case.walking().unwrap();
    let at = sheltersim_core::rules::movement::Where { geo: &plan, movement: case.held(MOVEMENT), queues: Held::Absent };
    let found = case.with(|world| sheltersim_core::rules::movement::approach_route(world, &at, &walking.model, &agent, &toilet, None)).unwrap();
    assert_eq!(&*zone, "toilet");
    let (minutes, path) = found.unwrap();
    assert_eq!(minutes, 1);
    assert_eq!(&*path, [Pt { x: 14.074175621006717, y: 7.091391873668907 }, Pt { x: 13.87417562100671, y: 10.477882672817003 }]);
}
