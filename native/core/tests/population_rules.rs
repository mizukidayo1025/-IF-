//! The population rules on the generated world, against what their Python rules gave (Issue #225,
//! lane M8; moved from `tests/unit/domain/test_population_rules.py`).
//!
//! The gate reaches these rules at a few events, so what it can't reach is checked here. Their
//! Python rules are gone (lane L10, `kernel_only`): each case was run with them once (2d8fb46), and
//! what they returned and the world they left were pinned in the pytest file's `PYTHON_GAVE`, per
//! test and in call order, as 16 hex digits of the sha256 of the lines of each value: a world's
//! sha256 of its canonical JSON, a result's `json.dumps(result, sort_keys=True, default=repr)`.
//! `PYTHON_GAVE` here is that table, unchanged: every result is a value JSON writes without
//! `repr` (an int, a bool, None, a tuple of IDs, a receipt), which `support/dumps.rs` writes again
//! byte for byte, and the core's world writes the same canonical text (`support/texts.rs`). Each
//! digest was checked on the same kernel by the pytest file before it moved (lane M8).
//!
//! The world is the generated Scenario the Project screen makes (`generated` of
//! `tests/data/generated_texts.json`), as the pytest file's `world` fixture made it; the worlds it
//! built besides it are in `tests/data/population_rules.json`
//! (`tests/support/core_fixtures_population.py`), with the IDs it picked in Python's order and
//! what `PopulationPresenceCommand` made of its commands. The other edits are made here, as the pytest
//! file made them. Each argument of a rule is what the PyO3 crate reads of the Python value the
//! pytest file passed (`native/src/rules/population.rs`, `population_lifecycle.rs`), and each rule
//! is the core's function its entry calls, with a host that does to the world what the native host
//! does (`load::following_host`). `_apply_presence_action` of a candidate that is not the world is
//! the PyO3 crate's (`act`), done here on two loaded worlds.
//!
//! The pytest file keeps one test of each entry, and the tests of the Python boundary: a refusal
//! that is a `KernelRefusal`, events scheduled as Python objects, the registered validator's
//! refusal, and the constructors of an admitted agent.

mod support;

use std::rc::Rc;
use std::sync::Arc;

use serde_json::{json, Value};
use sheltersim_core::checkpoint::sha256_hex;
use sheltersim_core::entities::{Activity, AgentRow, Table, TaskRow};
use sheltersim_core::error::Error;
use sheltersim_core::events::EventRow;
use sheltersim_core::host::Contract;
use sheltersim_core::row::ValueRow;
use sheltersim_core::rules::kit;
use sheltersim_core::rules::population::{self, Alternates, DepartureGiven, Given, Language, Overrides, OTHER_PROGRAM};
use sheltersim_core::rules::population_lifecycle::{self as lifecycle, Acted, Command, Logged};
use sheltersim_core::value::V;
use sheltersim_core::world::{Fit, World, WorldCell};

use support::dumps::Dumps;
use support::load::{self, Loaded};
use support::texts;
use support::{cases, TestHost};

const FILE: &str = "population_rules";
const DYNAMICS: &str = "population_dynamics_v1";
const LIFECYCLE: &str = "population_lifecycle_v1";
const PRESENCE: &str = "population_presence_v1";
const OVERLAY: &str = "geometry_overlay_v1";
const MOVEMENT: &str = "movement_v1";
const SPACES: &str = "temporary_spaces_v1";
const AGENT_DYNAMICS: &str = "agent_dynamics_v1";
const EFFECT_PROGRAM: &str = "effect_program_v1";

/// What the Python rules gave, per test and in call order (the pytest file's `PYTHON_GAVE`).
const PYTHON_GAVE: &[(&str, &[&str])] = &[
    ("test_a_broken_lifecycle_state_raises_the_rule_s_text[0-0]", &["4e3e1a5cf967fc03"]),
    ("test_a_broken_lifecycle_state_raises_the_rule_s_text[1-1]", &["4e3e1a5cf967fc03"]),
    ("test_a_broken_lifecycle_state_raises_the_rule_s_text[2-2]", &["4e3e1a5cf967fc03"]),
    ("test_a_broken_lifecycle_state_raises_the_rule_s_text[3-3]", &["4e3e1a5cf967fc03"]),
    ("test_a_broken_population_state_raises_the_rule_s_text[0-0]", &["b11eeb61291f9da4"]),
    ("test_a_broken_population_state_raises_the_rule_s_text[1-1]", &["b11eeb61291f9da4"]),
    ("test_a_broken_population_state_raises_the_rule_s_text[2-2]", &["b11eeb61291f9da4"]),
    ("test_a_broken_population_state_raises_the_rule_s_text[3-3]", &["b11eeb61291f9da4"]),
    ("test_a_broken_presence_ledger_stops_the_kernels_before_a_write", &["8d05be79883df930"]),
    ("test_a_departure_of_initial_people_keeps_their_history_and_withdraws_their_events", &["66f97af9265b53d7"]),
    ("test_a_departure_of_someone_busy_waits_as_the_rule_says", &["98f7276d7f198f4a"]),
    ("test_a_full_lifecycle_history_drops_its_oldest_as_the_rule_does", &["1edd68902f5ee8c6"]),
    ("test_a_full_presence_history_drops_its_oldest_as_the_rule_does", &["7d7da063297bfc6b"]),
    ("test_a_person_leaves_and_returns_with_the_same_state", &["2fa9ff282291d89f", "3d5bcc6d4658ed49"]),
    ("test_a_relocation_walks_and_its_completion_is_the_rule_s", &["36631542c997036d", "4e17d33e56d39170"]),
    ("test_a_retry_at_the_end_of_the_scenario_is_suppressed", &["7c4adaf0460d6974"]),
    ("test_a_retry_is_the_rule_s_event[0]", &["4e66d9984e4a7b06"]),
    ("test_a_retry_is_the_rule_s_event[1]", &["6951328349cd462d"]),
    ("test_a_retry_is_the_rule_s_event[2]", &["e53f4d9c2590002a"]),
    ("test_a_retry_is_the_rule_s_event[3]", &["6f8c63ae362597a9"]),
    ("test_a_return_to_a_full_zone_waits_and_the_rule_admits_it_later", &["f7b1822a9fe74c25", "aacc2e1ff3b22fd9", "fe73ec995c8783ac", "a9651b487ec72f49"]),
    ("test_a_reunion_and_a_relocation_in_place_are_the_rule_s", &["9f1df5f07bed3c74", "c48afd95f22e66cf"]),
    ("test_a_room_admission_keeps_half_a_metre_from_who_is_there", &["1cdd844ce76f2764", "aec36ea78bf2ff29"]),
    ("test_a_whole_command_is_the_rule_s", &["7ed9330305fa2915"]),
    ("test_an_admission_after_a_relocation_is_the_kernel_s_too", &["4dd4085117e6560a", "b68db4bb3e463477", "6265176cd447a0cc", "aa4dc9fb22a285fa"]),
    ("test_an_exit_of_someone_busy_is_deferred_as_the_rule_says[0-0]", &["523977908ba29f24"]),
    ("test_an_exit_of_someone_busy_is_deferred_as_the_rule_says[1-1]", &["e6555186da1a9321"]),
    ("test_an_exit_of_someone_busy_is_deferred_as_the_rule_says[2-2]", &["23bfeb08ef3e8ae6"]),
    ("test_an_exit_past_its_expected_return_is_dropped", &["9bb91f7d68140e79"]),
    ("test_arrivals_past_capacity_wait_and_are_admitted_when_room_opens", &["8c6f0730f5e57e7d", "819214104b4d924c", "240aab53b0c2be84"]),
    ("test_arrivals_take_an_alternate_zone", &["ad8c9fd1a19ab49f"]),
    ("test_arrivals_with_room_are_the_rule_s_people", &["53cff637f7ac3153"]),
    ("test_departures_take_the_newest_state_and_admit_who_waits", &["4a3ed74258f890f0", "9f92e5c5eb8eb682"]),
    ("test_first_use_by_a_deferred_departure_keeps_the_state", &["98f7276d7f198f4a"]),
    ("test_first_use_by_a_departure_makes_the_state_as_the_rule_does", &["f4ded4ee92f119d6"]),
    ("test_first_use_makes_the_population_state_as_the_rule_does", &["1bea8b8df207f7ba"]),
    ("test_household_context_matches_the_contract_s_dump[0-0]", &["ce993f19108b2a1c"]),
    ("test_household_context_matches_the_contract_s_dump[1-1]", &["a8ce4ce59860941b"]),
    ("test_household_context_matches_the_contract_s_dump[2-2]", &["656a6d0522127b63"]),
    ("test_household_context_matches_the_contract_s_dump[3-3]", &["19343777e3639d24"]),
    ("test_people_given_a_room_in_one_event_stand_apart", &["648bf16e71dbd33c"]),
    ("test_room_in_a_zone_with_temporary_spaces[0-0]", &["c36e13eb70283f9d"]),
    ("test_room_in_a_zone_with_temporary_spaces[0-1]", &["5eeb8bc2aee197a3"]),
    ("test_room_in_a_zone_with_temporary_spaces[0-2]", &["3d632062ee9f5fae"]),
    ("test_room_in_a_zone_with_temporary_spaces[0-3]", &["3d632062ee9f5fae"]),
    ("test_room_in_a_zone_with_temporary_spaces[1-0]", &["3d632062ee9f5fae"]),
    ("test_room_in_a_zone_with_temporary_spaces[1-1]", &["3d632062ee9f5fae"]),
    ("test_room_in_a_zone_with_temporary_spaces[1-2]", &["3d632062ee9f5fae"]),
    ("test_room_in_a_zone_with_temporary_spaces[1-3]", &["3d632062ee9f5fae"]),
    ("test_room_in_a_zone_with_temporary_spaces[2-0]", &["c36e13eb70283f9d"]),
    ("test_room_in_a_zone_with_temporary_spaces[2-1]", &["5eeb8bc2aee197a3"]),
    ("test_room_in_a_zone_with_temporary_spaces[2-2]", &["3d632062ee9f5fae"]),
    ("test_room_in_a_zone_with_temporary_spaces[2-3]", &["3d632062ee9f5fae"]),
    ("test_room_in_a_zone_with_temporary_spaces[3-0]", &["5eeb8bc2aee197a3"]),
    ("test_room_in_a_zone_with_temporary_spaces[3-1]", &["3d632062ee9f5fae"]),
    ("test_room_in_a_zone_with_temporary_spaces[3-2]", &["3d632062ee9f5fae"]),
    ("test_room_in_a_zone_with_temporary_spaces[3-3]", &["3d632062ee9f5fae"]),
    ("test_room_in_a_zone_with_temporary_spaces[4-0]", &["c36e13eb70283f9d"]),
    ("test_room_in_a_zone_with_temporary_spaces[4-1]", &["5eeb8bc2aee197a3"]),
    ("test_room_in_a_zone_with_temporary_spaces[4-2]", &["3d632062ee9f5fae"]),
    ("test_room_in_a_zone_with_temporary_spaces[4-3]", &["3d632062ee9f5fae"]),
    ("test_someone_saved_without_their_dynamics_gets_a_new_record", &["4dd4085117e6560a", "9a415f2cee4d209e"]),
    ("test_the_last_retry_runs_and_the_next_is_the_rule_s_to_refuse", &["2931723a931a105a", "f24b306a32d91cce", "f24b306a32d91cce"]),
    ("test_who_waits_is_checked_before_a_departure_writes", &["ce1366fb316e196a", "049d6795ea435ef5"]),
];

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

/// What the pytest file picked in Python's order, and what the contracts made of its commands.
fn inputs() -> Value {
    load::data(FILE)["inputs"].take()
}

fn input(key: &str) -> String {
    inputs()[key].as_str().unwrap_or_else(|| panic!("no input {key}")).to_owned()
}

/// What `PopulationPresenceCommand` made of a command of the pytest file.
fn dumped(name: &str) -> V {
    load::v(&inputs()["commands"][name])
}

/// `_event` of the pytest file: an event of the test's source, its sequence from its ID.
fn event(event_id: &str, minute: i64, payload: Value) -> Value {
    let digest = sha256_hex(event_id.as_bytes());
    json!({
        "event_id": event_id,
        "kind": "intervention",
        "due_minute": minute,
        "priority": 0,
        "sequence": 900_000 + i64::from_str_radix(&digest[..4], 16).unwrap(),
        "event_type": "effect_program_population",
        "payload": payload,
        "source_id": "ASM-TEST-POPULATION-RULES",
        "causation_id": null,
        "provenance": {},
    })
}

/// `_retry_event` of the pytest file.
fn retry_event(provenance: Value) -> Value {
    json!({
        "event_id": "retried",
        "kind": "intervention",
        "due_minute": 0,
        "priority": 4,
        "sequence": 880_000,
        "event_type": "effect_program_population",
        "payload": {"op": "depart_evacuees", "nested": {"a": [1, 2.5]}},
        "source_id": "ASM-TEST-POPULATION-RULES",
        "causation_id": null,
        "provenance": provenance,
    })
}

/// A part of a digest: `json.dumps(value, sort_keys=True)`.
fn json_part(value: &V) -> String {
    Dumps::default().sorted().text(value)
}

fn ids(items: &[Arc<str>]) -> V {
    V::List(items.iter().map(|id| V::Str(id.clone())).collect())
}

/// A value in a `V` map, by key.
fn get<'a>(value: &'a V, key: &str) -> &'a V {
    let V::Map(fields) = value else { panic!("not a map") };
    fields.get(key).unwrap_or_else(|| panic!("no {key}"))
}

fn s(value: &str) -> V {
    V::Str(text(value))
}

/// A value in a `V` map, by key, to change.
fn field<'a>(value: &'a mut V, key: &str) -> &'a mut V {
    let V::Map(fields) = value else { panic!("not a map") };
    fields.get_mut(key).unwrap_or_else(|| panic!("no {key}"))
}

fn list(value: &mut V) -> &mut Vec<V> {
    match value {
        V::List(items) => items,
        other => panic!("not a list: {other:?}"),
    }
}

fn first_value(value: &mut V) -> &mut V {
    let V::Map(fields) = value else { panic!("not a map") };
    fields.values_mut().next().expect("a value")
}

/// The ValueError text of a validation.
fn value_error(result: Result<(), Error>) -> String {
    match result {
        Err(Error::Value(text) | Error::MarkedValue(text)) => text,
        other => panic!("not a ValueError: {other:?}"),
    }
}

fn unsupported<T: std::fmt::Debug>(result: Result<T, Error>) {
    assert!(matches!(result, Err(Error::Unsupported(_))), "not a refusal: {result:?}");
}

// ---- A case ----------------------------------------------------------------------------------------

/// A world in a cell, with a host that does to it what the native host does, and the digests the
/// Python rule gave in this test, in turn.
struct Case {
    entry: Value,
    loaded: Rc<Loaded>,
    host: TestHost,
    gave: std::slice::Iter<'static, &'static str>,
}

/// The pytest file's `world` (`generated` of `generated_texts.json`), or a world of
/// `population_rules.json`.
fn entry_of(world: &str) -> Value {
    if world == "generated" {
        cases::entry("generated_texts", "generated")
    } else {
        cases::entry(FILE, world)
    }
}

fn case(world: &str, test: &str) -> Case {
    let entry = entry_of(world);
    let loaded = Rc::new(load::loaded(&entry));
    let host = load::following_host(&entry, &loaded);
    // A test that compares no digest has none.
    let gave = PYTHON_GAVE.iter().find(|(name, _)| *name == test).map_or(NONE, |(_, gave)| *gave).iter();
    Case { entry, loaded, host, gave }
}

const NONE: &[&str] = &[];

impl Drop for Case {
    /// Every digest of the test was compared.
    fn drop(&mut self) {
        if !std::thread::panicking() {
            assert_eq!(self.gave.len(), 0, "digests left");
        }
    }
}

impl Case {
    fn edit<T>(&self, edit: impl FnOnce(&mut World) -> T) -> T {
        self.loaded.cell.with(edit).unwrap()
    }

    fn bytes(&self) -> String {
        texts::canonical_of(&self.loaded.cell, &self.entry)
    }

    /// The world's canonical document, to read.
    fn doc(&self) -> Value {
        serde_json::from_str(&self.bytes()).unwrap()
    }

    fn world_part(&self) -> String {
        texts::sha(&self.bytes())
    }

    /// `_python_gave(*values)`: the next digest of the test is these parts'.
    fn gave(&mut self, parts: &[String]) {
        let digest = sha256_hex(parts.join("\n").as_bytes());
        assert_eq!(&digest[..16], *self.gave.next().expect("a digest of this call"), "{parts:?}");
    }

    /// `_python_gave(result, world)`.
    fn gave_result(&mut self, result: V) {
        let parts = [json_part(&result), self.world_part()];
        self.gave(&parts);
    }

    /// `_python_gave(world)`.
    fn gave_world(&mut self) {
        let parts = [self.world_part()];
        self.gave(&parts);
    }

    fn minute(&self) -> i64 {
        self.edit(|world| world.minute_index().unwrap())
    }

    fn set_minute(&self, minute: i64) {
        self.edit(|world| world.scalars.get_mut().minute_index = Fit::Ok(minute));
    }

    fn zone(&self) -> String {
        input("zone")
    }

    fn other_zone(&self) -> String {
        input("other")
    }

    fn person(&self) -> String {
        input("person")
    }

    fn occupancy(&self, zone: &str) -> i64 {
        self.edit(|world| world.rows().unwrap().agents.iter().filter(|(_, agent)| &*agent.zone_id == zone).count() as i64)
    }

    fn set_capacity(&self, zone: &str, capacity: Option<i64>) {
        self.edit(|world| world.rows_mut().unwrap().zones.get_mut(zone).unwrap().capacity = capacity);
    }

    fn capacity(&self, zone: &str) -> Option<i64> {
        self.edit(|world| world.rows().unwrap().zones.get(zone).unwrap().capacity)
    }

    fn agent<T>(&self, agent_id: &str, change: impl FnOnce(&mut AgentRow) -> T) -> T {
        self.edit(|world| change(world.rows_mut().unwrap().agents.get_mut(agent_id).unwrap_or_else(|| panic!("no {agent_id}"))))
    }

    fn has_agent(&self, agent_id: &str) -> bool {
        self.edit(|world| world.rows().unwrap().agents.contains_key(agent_id))
    }

    /// The people's IDs, in the world's order (`world.agents`).
    fn agent_ids(&self) -> Vec<Arc<str>> {
        self.edit(|world| world.rows().unwrap().agents.iter().map(|(id, _)| id.clone()).collect())
    }

    /// A namespace whose row is a `ValueRow` (the population's, the overlay's), to change.
    fn state<T>(&self, name: &str, change: impl FnOnce(&mut V) -> T) -> T {
        self.edit(|world| change(&mut world.row_mut::<ValueRow>(name).unwrap().0))
    }

    /// `model_state[name] = value` of a namespace whose row is typed: its document, changed.
    fn typed_state(&self, name: &str, change: impl FnOnce(&mut Value)) {
        let mut document = self.doc()["model_state"][name].clone();
        change(&mut document);
        self.edit(|world| load::put_namespace(world, name, load::v(&document)));
    }

    fn drop_namespace(&self, name: &str) {
        self.edit(|world| world.slots.get(name).unwrap().try_borrow_mut().unwrap().row = None);
    }

    fn has_namespace(&self, name: &str) -> bool {
        self.edit(|world| world.slots.get(name).is_some_and(|slot| slot.try_borrow().unwrap().row.is_some()))
    }

    /// `world.schedule_event(event)`.
    fn schedule(&self, event: &Value) {
        self.edit(|world| kit::Events::of(world).unwrap().schedule(texts::row(event)).map_err(|_| "a refused event").unwrap());
    }

    fn pending(&self) -> Vec<Value> {
        self.doc()["event_queue"].as_array().unwrap().clone()
    }

    fn pending_ids(&self) -> Vec<String> {
        self.pending().iter().map(|event| event["event_id"].as_str().unwrap().to_owned()).collect()
    }

    // ---- population.py ---------------------------------------------------------------------------

    fn arrivals(&mut self, event: &Value, given: Given) -> Result<i64, Error> {
        population::queue_arrivals(&self.loaded.cell, &mut self.host, &texts::row(event), given)
    }

    /// `_arrive`: `count` arrivals of households of `household_size` into the first person's zone,
    /// at the world's minute, with the result and the world the rule's.
    fn arrive(&mut self, count: i64, household_size: i64, alternates: Alternates) -> i64 {
        let event = event("arrival-event", self.minute(), json!({}));
        let given = Given { alternates: Ok(alternates), household_size: Some(Ok(Some(household_size))), ..given(&self.zone(), count) };
        let arrived = self.arrivals(&event, given).unwrap();
        self.gave_result(V::Int(arrived));
        arrived
    }

    fn departures(&mut self, event: &Value, given: DepartureGiven) -> Result<usize, Error> {
        population::queue_departures(&self.loaded.cell, &mut self.host, &texts::row(event), given)
    }

    /// `_depart`: `count` departures from the first person's zone, with the result and the world
    /// the rule's.
    fn depart(&mut self, count: i64, include_initial: Option<Option<bool>>) -> usize {
        let event = event("departure-event", self.minute(), json!({}));
        let departed = self.departures(&event, DepartureGiven { zone: Ok(Some(text(&self.zone()))), count: Ok(Some(count)), include_initial }).unwrap();
        self.gave_result(V::Int(departed as i64));
        departed
    }

    /// `admit_pending_arrivals(world, minute)`, with the result and the world the rule's.
    fn admit_arrivals(&mut self, minute: i64) -> Vec<Arc<str>> {
        let admitted = population::admit_arrivals(&self.loaded.cell, &mut self.host, minute).unwrap();
        self.gave_result(ids(&admitted));
        admitted
    }

    /// `schedule_population_retry(world, event, id_prefix=..., subject=...)`.
    fn retry(&mut self, event: &Value, id_prefix: &str, subject: &str) -> Result<(), Error> {
        population::schedule_retry_through(&self.loaded.cell, &mut self.host, &texts::row(event), id_prefix, subject)
    }

    /// `_same_suppression`: the rule stops with its text, and the world keeps what it scheduled.
    fn same_suppression(&mut self, expected: &str, event: &Value, id_prefix: &str, subject: &str) {
        let said = texts::suppressed(self.retry(event, id_prefix, subject));
        assert_eq!(said, expected);
        self.gave_world();
    }

    /// `population.zone_has_room`.
    fn has_room(&mut self, zone: &str) -> bool {
        let read = population::Read::read(&self.loaded.cell, &mut self.host, false, false).unwrap();
        read.run::<_, Error, _>(&self.loaded.cell, |world, inputs| population::zone_has_room(world, inputs, zone)).unwrap()
    }

    /// `validate_population_dynamics_state` of a world the core holds.
    fn validate_dynamics(&mut self) -> Result<(), Error> {
        let dynamics = kit::holding(&self.loaded.cell, &mut self.host)?.held(DYNAMICS, population::OTHER_DYNAMICS);
        let routes = self.loaded.cell.with(|world| population::validation_reads_routes(world, &dynamics))??;
        let read = population::Read::read(&self.loaded.cell, &mut self.host, routes, false)?;
        read.run::<_, Error, _>(&self.loaded.cell, population::validate)
    }

    /// `_waiting_for_the_person`: a departure with the busy person selected waits, as the rule
    /// did.
    fn waiting_for_the_person(&mut self, zone: &str) {
        let event = event("departure-event", self.minute(), json!({}));
        let given = DepartureGiven { zone: Ok(Some(text(zone))), count: Ok(Some(1)), include_initial: Some(Some(true)) };
        let said = texts::suppressed(self.departures(&event, given));
        assert_eq!(said, "departure waits for the selected residents' active tasks");
        let pending = V::List(self.pending_ids().iter().map(|id| V::Str(text(id))).collect());
        let parts = [self.world_part(), json_part(&pending)];
        self.gave(&parts);
    }

    /// The first person busy and everyone else in another zone.
    fn alone_and_busy(&self) -> String {
        let person = self.agent_ids().into_iter().min().unwrap().to_string();
        self.agent(&person, |agent| agent.activity = Activity::Queued);
        let other = self.other_zone();
        for id in self.agent_ids() {
            if *id != *person {
                self.agent(&id, |agent| agent.zone_id = text(&other));
            }
        }
        person
    }

    // ---- population_lifecycle.py -----------------------------------------------------------------

    /// `population_lifecycle.zone_has_room(world, zone, excluding=...)`.
    fn lifecycle_has_room(&mut self, zone: &str, excluding: Option<&str>) -> bool {
        let read = lifecycle::Read::read(&self.loaded.cell, &mut self.host, false).unwrap();
        read.run::<_, Error, _>(&self.loaded.cell, |world, inputs| lifecycle::zone_has_room(world, inputs, zone, excluding)).unwrap()
    }

    /// A copy of the world (`copy.deepcopy`), loaded with a host of its own.
    fn candidate(&self) -> (Rc<Loaded>, TestHost) {
        let fork = self.edit(|world| world.fork().unwrap());
        let candidate = Rc::new(load::loaded(&self.entry));
        candidate.cell.with(|world| *world = fork).unwrap();
        let host = load::following_host(&self.entry, &candidate);
        (candidate, host)
    }

    /// `_command`: one presence command, on a candidate, as the rule did; the candidate is the world
    /// after.
    fn command(&mut self, minute: i64, name: &str) -> V {
        self.set_minute(minute);
        let command = dumped(name);
        let event = command_event(&command, minute);
        let (candidate, mut candidate_host) = self.candidate();
        let receipt = action(&self.loaded, &mut self.host, &candidate, &mut candidate_host, &texts::row(&event), &command).unwrap();
        let parts = [json_part(&receipt), texts::sha(&texts::canonical_of(&candidate.cell, &self.entry))];
        self.gave(&parts);
        validate_lifecycle(&candidate, &mut candidate_host).unwrap();
        self.loaded = candidate;
        self.host = candidate_host;
        receipt
    }

    /// `_command_suppressed`: a deferred command, by the rule's text, and the world it left.
    fn command_suppressed(&mut self, reason: &str, name: &str) {
        let minute = 3;
        self.set_minute(minute);
        let command = dumped(name);
        let event = command_event(&command, minute);
        let (candidate, mut candidate_host) = self.candidate();
        let said = texts::suppressed(action(&self.loaded, &mut self.host, &candidate, &mut candidate_host, &texts::row(&event), &command));
        assert_eq!(said, format!("population exit waits until the person is free: {reason}"));
        self.gave_world();
    }

    /// `admit_pending_returns(world, minute)`, with the result and the world the rule's.
    fn admit_returns(&mut self, minute: i64) -> Vec<Arc<str>> {
        let admitted = lifecycle::admit_returns_through(&self.loaded.cell, &mut self.host, minute).unwrap();
        self.gave_result(ids(&admitted));
        admitted
    }

    /// `complete_presence_relocation(world, event)`, with the result (None) and the world the rule's.
    fn complete_relocation(&mut self, event: &Value) {
        lifecycle::complete_relocation_through(&self.loaded.cell, &mut self.host, &texts::row(event)).unwrap();
        self.gave_result(V::Null);
    }

    /// `apply_population_presence_command(world, event, fields)`, with what the contract made of
    /// `fields`.
    fn presence_command(&mut self, event: &Value, name: &str) -> Result<V, Error> {
        lifecycle::presence_command_typed(&self.loaded.cell, &mut self.host, &texts::row(event), &dumped(name))
    }

    fn lifecycle_state(&self) -> Value {
        self.doc()["model_state"][LIFECYCLE].clone()
    }

    /// `_return_waiting`: the person away and back to their full zone, waiting.
    fn return_waiting(&mut self, person: &str) -> String {
        let zone = self.agent(person, |agent| agent.zone_id.to_string());
        self.command(1, "exit_9");
        self.set_capacity(&zone, Some(self.occupancy(&zone)));
        self.command(2, "return");
        let waiting: Vec<Value> = self.lifecycle_state()["pending_returns"].as_array().unwrap().iter().map(|item| item["agent_id"].clone()).collect();
        assert_eq!(waiting, vec![json!(person)]);
        zone
    }

    /// The pending relocation of a person, and its completion as the queue holds it; then the
    /// person is at the destination, idle, with the task gone, at the completion's minute.
    fn arrive_at_destination(&self, person: &str, destination: &str) -> Value {
        let relocation = self.lifecycle_state()["pending_relocations"][person].clone();
        let completion = self.pending().into_iter().find(|item| item["event_id"] == relocation["event_id"]).expect("the completion");
        self.set_minute(completion["due_minute"].as_i64().unwrap());
        self.agent(person, |agent| {
            agent.zone_id = text(destination);
            agent.activity = Activity::Idle;
            agent.target_id = None;
        });
        self.edit(|world| assert!(world.rows_mut().unwrap().active_tasks.remove_row(relocation["task_id"].as_str().unwrap())));
        completion
    }

    /// The destination of a relocation the pytest file picked: the first zone a route from the
    /// person's zone leads to (`next(edge.to_zone_id ...)`, in Python's order).
    fn first_step(&self, name: &str) -> String {
        inputs()["commands"][name]["zone_id"].as_str().unwrap().to_owned()
    }
}

/// The arguments of `queue_arrivals(event=..., zone_id=zone, count=count)`.
fn given(zone: &str, count: i64) -> Given {
    Given {
        zone: Ok(Some(text(zone))),
        alternates: Ok(Alternates::NotGiven),
        route_id: None,
        count: Ok(Some(count)),
        household_size: None,
        mobility: None,
        language: None,
        overrides: None,
    }
}

/// The event of a command at `minute` (`command-{action}-{minute}`).
fn command_event(command: &V, minute: i64) -> Value {
    let V::Map(fields) = command else { panic!("not a command") };
    let V::Str(action) = &fields["action"] else { panic!("no action") };
    event(&format!("command-{action}-{minute}"), minute, json!({}))
}

/// `apply_presence_action(world, candidate, event, typed, typed.selected_ids())` of the PyO3 crate:
/// the plan reads `world` and the candidate, the writes go to the candidate, and a deferral
/// schedules its retry on `world`.
fn action(world: &Loaded, world_host: &mut TestHost, candidate: &Loaded, candidate_host: &mut TestHost, row: &EventRow, dumped: &V) -> Result<V, Error> {
    let ids = lifecycle::selected_ids(dumped).unwrap().unwrap();
    let command = Command::of_dumped(dumped, ids).unwrap();
    let read = lifecycle::Read::read(&candidate.cell, &mut *candidate_host, &*command.action == "relocate")?;
    let effects = kit::holding(&world.cell, &mut *world_host)?.held(EFFECT_PROGRAM, OTHER_PROGRAM);
    let logged = if matches!(&*command.action, "temporary_exit" | "return") { lifecycle::logged_through(&world.cell, &mut *world_host, &effects, row)? } else { Logged::new() };
    let reads = world.cell.with(|world| lifecycle::world_reads(world, &effects, &logged, row, &command))?;
    match read.run::<_, Error, _>(&candidate.cell, |world, inputs| lifecycle::presence_action(world, inputs, row, &command, &reads))? {
        Acted::Done(done) => Ok(done.receipt),
        Acted::Deferred(reason) => {
            population::schedule_retry_through(&world.cell, &mut *world_host, row, "presence-retry", "population exit")?;
            Err(Error::suppressed(lifecycle::deferred_text(&reason)))
        }
    }
}

/// `validate_population_lifecycle_state` of a loaded world.
fn validate_lifecycle(loaded: &Loaded, host: &mut TestHost) -> Result<(), Error> {
    let holding = kit::holding(&loaded.cell, &mut *host)?;
    let (state, presence) = (holding.held(LIFECYCLE, lifecycle::OTHER_LIFECYCLE), holding.held(PRESENCE, "the world holds another presence state"));
    loaded.cell.with(|world| lifecycle::validate(world, &state, &presence, None))?
}

// ---- Arrivals ----------------------------------------------------------------------------------

#[test]
fn arrivals_with_room_are_the_rule_s_people() {
    let mut case = case("generated", "test_arrivals_with_room_are_the_rule_s_people");
    case.set_capacity(&case.zone(), None);
    assert_eq!(case.arrive(5, 2, Alternates::NotGiven), 5);
    assert_eq!(case.doc()["model_state"][DYNAMICS]["profile_context"].as_object().unwrap().len(), 5);
}

#[test]
fn household_context_matches_the_contract_s_dump() {
    for (index, (size, count)) in [(1, 4), (2, 7), (12, 30), (3, 1)].into_iter().enumerate() {
        let mut case = case("generated", &format!("test_household_context_matches_the_contract_s_dump[{index}-{index}]"));
        case.arrive(count, size, Alternates::NotGiven);
    }
}

#[test]
fn arrivals_past_capacity_wait_and_are_admitted_when_room_opens() {
    let mut case = case("generated", "test_arrivals_past_capacity_wait_and_are_admitted_when_room_opens");
    let zone = case.zone();
    case.set_capacity(&zone, Some(case.occupancy(&zone) + 1));
    assert_eq!(case.arrive(4, 2, Alternates::NotGiven), 1);
    let state = &case.doc()["model_state"][DYNAMICS];
    assert_eq!(state["pending_arrivals"].as_array().unwrap().len(), 3);
    assert_eq!(state["arrivals"].as_array().unwrap().last().unwrap()["waiting_count"], 3);
    // Nobody can come in: nothing changes but the count.
    assert!(case.admit_arrivals(0).is_empty());
    case.set_capacity(&zone, Some(case.capacity(&zone).unwrap() + 2));
    assert_eq!(case.admit_arrivals(0).len(), 2);
    let doc = case.doc();
    assert_eq!(doc["model_state"][DYNAMICS]["pending_arrivals"].as_array().unwrap().len(), 1);
    assert_eq!(doc["metric_state"]["population_waiting_count"], json!(1.0));
}

#[test]
fn arrivals_take_an_alternate_zone() {
    // The configs with `geometry_overlay` at schema_version 3.
    let mut case = case("overlay_3", "test_arrivals_take_an_alternate_zone");
    let (zone, other) = (case.zone(), case.other_zone());
    case.set_capacity(&zone, Some(case.occupancy(&zone)));
    assert_eq!(case.arrive(2, 2, Alternates::Zones(vec![text(&other)])), 2);
    let zones: Vec<Arc<str>> = case.edit(|world| world.rows().unwrap().agents.iter().filter(|(id, _)| id.starts_with("arrival-")).map(|(_, agent)| agent.zone_id.clone()).collect());
    assert_eq!(zones.len(), 2);
    assert!(zones.iter().all(|zone| **zone == *other));
}

#[test]
fn a_refused_arrival_changes_nothing_and_says_the_rule_s_text() {
    let item = |index: i64| V::Map([(text("member_index"), V::Int(index))].into_iter().collect());
    let cases: Vec<(Box<dyn Fn(&mut Given)>, &str)> = vec![
        (Box::new(|given| given.zone = Ok(Some(text("no-such-zone")))), "unknown zone"),
        (Box::new(|given| given.count = Ok(Some(0))), "count is outside"),
        (Box::new(|given| given.household_size = Some(Ok(Some(13)))), "household_size is outside"),
        // No member of `Mobility`.
        (Box::new(|given| given.mobility = Some(Ok(None))), "mobility is invalid"),
        // No identifier: the rule asks the contract of a person's traits.
        (Box::new(|given| given.language = Some(Ok(Language::Ask(V::Str(text("not a language")))))), "language is invalid"),
        (Box::new(|given| given.alternates = Ok(Alternates::Zones(vec![text("no-such-zone")]))), "alternate admission zones are invalid"),
        (Box::new(|given| given.route_id = Some(Some(text("no-such-route")))), "approach route is invalid"),
        (Box::new(move |given| given.overrides = Some(Ok(Overrides::Items(vec![item(9)])))), "person context is invalid"),
    ];
    for (set, expected) in cases {
        let mut case = case("generated", "test_a_refused_arrival_changes_nothing_and_says_the_rule_s_text");
        case.host.contracts = Some(Box::new(arrival_contracts));
        let mut arguments = given(&case.zone(), 1);
        set(&mut arguments);
        let before = case.bytes();
        let said = texts::failed(case.arrivals(&event("arrival-event", 0, json!({})), arguments));
        assert!(said.contains(expected), "{said}");
        assert!(case.bytes() == before, "the world changed: {expected}");
    }
}

/// The native host's contracts, as the fixture recorded what they gave for the arguments of
/// `a_refused_arrival_changes_nothing_and_says_the_rule_s_text`; any other input is given back.
fn arrival_contracts(which: &Contract, input: &V) -> Result<V, Error> {
    for asked in inputs()["contracts"].as_array().unwrap() {
        if which.model().1 != asked["model"].as_str().unwrap() || load::v(&asked["input"]) != *input {
            continue;
        }
        return match asked.get("dump") {
            Some(dump) => Ok(load::v(dump)),
            None => {
                let text = asked["raises"][1].as_str().unwrap().to_owned();
                Err(if asked["raises"][0] == "Type" { Error::Type(text) } else { Error::Value(text) })
            }
        };
    }
    Ok(input.clone())
}

#[test]
fn first_use_makes_the_population_state_as_the_rule_does() {
    let mut case = case("generated", "test_first_use_makes_the_population_state_as_the_rule_does");
    case.drop_namespace(DYNAMICS);
    case.set_capacity(&case.zone(), None);
    assert_eq!(case.arrive(2, 2, Alternates::NotGiven), 2);
    assert_eq!(case.doc()["model_state"][DYNAMICS]["schema_version"], 1);
}

#[test]
fn a_full_presence_history_drops_its_oldest_as_the_rule_does() {
    let mut case = case("generated", "test_a_full_presence_history_drops_its_oldest_as_the_rule_does");
    let person = case.person();
    let template = json!({
        "schema_version": 2,
        "event_id": "old-event",
        "minute": 0,
        "action": "arrived",
        "from_zone_id": null,
        "to_zone_id": null,
        "from_household_id": null,
        "to_household_id": null,
        "causation_id": null,
        "source_ids": [],
        "state_snapshot_hash": null,
        "time_semantics": "recorded_event_minute",
    });
    let history: Vec<V> = (0..20_000)
        .map(|index| {
            let mut record = template.clone();
            record["transition_id"] = json!(format!("old-{index}"));
            record["agent_id"] = json!(person);
            load::v(&record)
        })
        .collect();
    case.state(PRESENCE, |state| *field(state, "history") = V::List(history));
    case.set_capacity(&case.zone(), None);
    assert_eq!(case.arrive(2, 2, Alternates::NotGiven), 2);
    let history = &case.doc()["model_state"][PRESENCE]["history"];
    assert_eq!(history.as_array().unwrap().len(), 20_000);
    assert_eq!(history[0]["transition_id"], "old-2");
}

// ---- Room ----------------------------------------------------------------------------------------

#[test]
fn a_person_walking_to_a_zone_holds_their_place() {
    let mut case = case("generated", "test_a_person_walking_to_a_zone_holds_their_place");
    let people = case.agent_ids();
    let walker = people[0].to_string();
    let zone = case.agent(&people[1], |agent| agent.zone_id.to_string());
    let other = case.other_zone();
    case.agent(&walker, |agent| {
        agent.zone_id = text(&other);
        agent.activity = Activity::Moving;
        agent.target_id = Some(text(&zone));
    });
    case.set_capacity(&zone, Some(case.occupancy(&zone) + 1));
    assert!(!case.has_room(&zone));
    assert!(case.lifecycle_has_room(&zone, Some(&walker)));
}

#[test]
fn room_in_a_zone_with_temporary_spaces() {
    let spaces_cases: [&[(i64, bool)]; 5] = [&[], &[(2, true)], &[(2, false)], &[(3, false), (1, true)], &[(1, false), (5, false)]];
    for (spaces_index, spaces) in spaces_cases.into_iter().enumerate() {
        for (extra_index, extra) in [-1, 0, 1, 3].into_iter().enumerate() {
            let mut case = case("generated", &format!("test_room_in_a_zone_with_temporary_spaces[{spaces_index}-{extra_index}]"));
            let zone = case.zone();
            let mut state = json!({"schema_version": 1, "policy_version": "typed-temporary-capacity-v1", "spaces": {}, "history": []});
            for (index, (people, active)) in spaces.iter().enumerate() {
                let id = format!("space-{index}");
                state["spaces"][&id] = json!({
                    "space_id": id,
                    "zone_id": zone,
                    "people_capacity": people,
                    "sleeping_capacity": 0,
                    "start_minute": 0,
                    "end_minute": 10,
                    "active": active,
                    "start_event_id": "start",
                    "end_event_id": if *active { Value::Null } else { json!("end") },
                    "program_id": "program",
                    "effect_id": "effect",
                    "source_ids": [],
                    "geometry_status": "zone_only_capacity",
                });
            }
            case.edit(|world| load::put_namespace(world, SPACES, load::v(&state)));
            case.set_capacity(&zone, Some((case.occupancy(&zone) + extra).max(1)));
            let person = case.person();
            let parts = [
                json_part(&V::Bool(case.has_room(&zone))),
                json_part(&V::Bool(case.lifecycle_has_room(&zone, None))),
                json_part(&V::Bool(case.lifecycle_has_room(&zone, Some(&person)))),
            ];
            case.gave(&parts);
        }
    }
}

#[test]
fn a_zone_without_capacity_has_room() {
    let mut case = case("generated", "test_a_zone_without_capacity_has_room");
    let zone = case.zone();
    case.set_capacity(&zone, None);
    assert!(case.has_room(&zone));
    assert!(case.lifecycle_has_room(&zone, None));
}

// ---- Departures --------------------------------------------------------------------------------

#[test]
fn departures_take_the_newest_state_and_admit_who_waits() {
    let mut case = case("generated", "test_departures_take_the_newest_state_and_admit_who_waits");
    let zone = case.zone();
    case.set_capacity(&zone, Some(case.occupancy(&zone) + 1));
    assert_eq!(case.arrive(3, 2, Alternates::NotGiven), 1);
    assert_eq!(case.doc()["model_state"][DYNAMICS]["pending_arrivals"].as_array().unwrap().len(), 2);
    assert_eq!(case.depart(1, None), 1);
    let state = &case.doc()["model_state"][DYNAMICS];
    assert_eq!(state["departures"].as_array().unwrap().len(), 1);
    assert_eq!(state["pending_arrivals"].as_array().unwrap().len(), 1);
}

#[test]
fn a_departure_of_initial_people_keeps_their_history_and_withdraws_their_events() {
    let mut case = case("generated", "test_a_departure_of_initial_people_keeps_their_history_and_withdraws_their_events");
    let person = case.person();
    case.schedule(&event("mentions-them", 5, json!({"agent_id": person})));
    case.schedule(&event("mentions-another", 5, json!({"agent_id": "someone-else"})));
    case.schedule(&event("mentions-sender", 6, json!({"sender_agent_id": person})));
    let count = case.occupancy(&case.zone());
    assert_eq!(case.depart(count, Some(Some(true))), count as usize);
    assert!(!case.has_agent(&person));
    let pending = case.pending_ids();
    assert!(pending.iter().any(|id| id == "mentions-another"));
    assert!(!pending.iter().any(|id| id == "mentions-them"));
}

#[test]
fn the_rule_says_why_a_departure_can_not_happen() {
    let mut case = case("generated", "test_the_rule_says_why_a_departure_can_not_happen");
    let before = case.bytes();
    let zone = case.zone();
    let arrival = event("arrival-event", 0, json!({}));
    let said = texts::failed(case.departures(&arrival, DepartureGiven { zone: Ok(Some(text(&zone))), count: Ok(Some(5000)), include_initial: None }));
    assert!(said.contains("count is outside"), "{said}");
    let said = texts::failed(case.departures(&arrival, DepartureGiven { zone: Ok(Some(text("no-such-zone"))), count: Ok(Some(1)), include_initial: None }));
    assert!(said.contains("unknown zone"), "{said}");
    assert!(case.bytes() == before, "the world changed");
}

#[test]
fn a_departure_of_someone_busy_waits_as_the_rule_says() {
    let mut case = case("generated", "test_a_departure_of_someone_busy_waits_as_the_rule_says");
    let person = case.alone_and_busy();
    let zone = case.agent(&person, |agent| agent.zone_id.to_string());
    case.waiting_for_the_person(&zone);
    let retry = case.pending().into_iter().find(|item| item["event_id"].as_str().unwrap().starts_with("departure-retry")).expect("a retry");
    assert_eq!(retry["provenance"]["population_retry"], json!({"count": 1}));
}

#[test]
fn first_use_by_a_departure_makes_the_state_as_the_rule_does() {
    let mut case = case("generated", "test_first_use_by_a_departure_makes_the_state_as_the_rule_does");
    case.drop_namespace(DYNAMICS);
    assert_eq!(case.depart(1, Some(Some(true))), 1);
}

#[test]
fn first_use_by_a_deferred_departure_keeps_the_state() {
    let mut case = case("generated", "test_first_use_by_a_deferred_departure_keeps_the_state");
    case.drop_namespace(DYNAMICS);
    let person = case.alone_and_busy();
    let zone = case.agent(&person, |agent| agent.zone_id.to_string());
    case.waiting_for_the_person(&zone);
    assert!(case.has_namespace(DYNAMICS));
}

/// The admission after a departure used to fall back to the rule, past the writes. A waiting
/// arrival whose minute is a bool, which no rule writes, is outside the contract: the departure is
/// refused before it writes anything (the pytest file keeps that the refusal is a `KernelRefusal`).
#[test]
fn who_waits_is_checked_before_a_departure_writes() {
    let mut case = case("generated", "test_who_waits_is_checked_before_a_departure_writes");
    // `_full_zone_with_waiters`: someone new in a full zone, and one arrival waiting.
    let zone = case.zone();
    case.set_capacity(&zone, None);
    assert_eq!(case.arrive(1, 1, Alternates::NotGiven), 1);
    case.set_capacity(&zone, Some(case.occupancy(&zone)));
    let waiting = Given { household_size: Some(Ok(Some(1))), ..given(&zone, 1) };
    let arrived = case.arrivals(&event("arrival-waiting", 0, json!({})), waiting).unwrap();
    case.gave_result(V::Int(arrived));
    case.state(DYNAMICS, |state| {
        let pending = list(field(state, "pending_arrivals"));
        assert_eq!(pending.len(), 1);
        *field(&mut pending[0], "created_minute") = V::Bool(true);
    });
    let before = case.bytes();
    unsupported(case.departures(&event("departure-event", 0, json!({})), DepartureGiven { zone: Ok(Some(text(&zone))), count: Ok(Some(1)), include_initial: None }));
    assert!(case.bytes() == before, "the world changed");
}

// ---- Retries -----------------------------------------------------------------------------------

#[test]
fn a_retry_is_the_rule_s_event() {
    let provenances = [
        json!({}),
        json!({"population_retry": {"count": 7}, "other": {"kept": true}}),
        json!({"effect_program_v1": {"program_id": "p", "effect_id": "e"}}),
        json!({"effect_program_v1": {"program_id": "not-in-state", "effect_id": "e"}}),
    ];
    for (index, provenance) in provenances.into_iter().enumerate() {
        let mut case = case("generated", &format!("test_a_retry_is_the_rule_s_event[{index}]"));
        // A program as its row holds one: the kernel reads the namespace's row.
        let program = json!({
            "schema_version": 1,
            "programs": {
                "p": {
                    "program_hash": "hash",
                    "source_text_hash": "text-hash",
                    "narrative": "",
                    "compiler_provenance": {},
                    "assumptions": [],
                    "support_mode": "simulation",
                    "effect_ids": ["e"],
                    "operators": {"e": "depart_evacuees"},
                    "scheduled_event_ids": [],
                }
            },
            "active_facility_modifiers": {},
        });
        case.edit(|world| load::put_namespace(world, EFFECT_PROGRAM, load::v(&program)));
        case.retry(&retry_event(provenance), "presence-retry", "population exit").unwrap();
        case.gave_result(V::Null);
        let retry = case.pending().into_iter().find(|item| item["event_id"].as_str().unwrap().starts_with("presence-retry-")).expect("a retry");
        assert_eq!(retry["provenance"]["deferred_from"], "retried");
        assert_eq!(retry["payload"]["nested"]["a"], json!([1, 2.5]));
    }
}

#[test]
fn the_last_retry_runs_and_the_next_is_the_rule_s_to_refuse() {
    let mut case = case("generated", "test_the_last_retry_runs_and_the_next_is_the_rule_s_to_refuse");
    case.retry(&retry_event(json!({"population_retry": {"count": 1439}})), "departure-retry", "departure").unwrap();
    case.gave_result(V::Null);
    let limit = "departure reached the bounded deferral limit";
    case.same_suppression(limit, &retry_event(json!({"population_retry": {"count": 1440}})), "departure-retry", "departure");
    case.same_suppression(limit, &retry_event(json!({"population_retry": {"count": "many"}})), "departure-retry", "departure");
}

#[test]
fn a_retry_at_the_end_of_the_scenario_is_suppressed() {
    let mut case = case("generated", "test_a_retry_at_the_end_of_the_scenario_is_suppressed");
    let duration = case.edit(|world| world.duration_minutes().unwrap());
    case.set_minute(duration - 1);
    case.same_suppression("population exit could not complete before the scenario ended", &retry_event(json!({})), "presence-retry", "population exit");
}

// ---- Presence commands -------------------------------------------------------------------------

#[test]
fn a_person_leaves_and_returns_with_the_same_state() {
    let mut case = case("generated", "test_a_person_leaves_and_returns_with_the_same_state");
    let person = case.person();
    let needs_of = |case: &Case| case.doc()["agents"].as_array().unwrap().iter().find(|agent| agent["profile"]["id"] == person.as_str()).unwrap()["needs"].clone();
    let (needs, zone) = (needs_of(&case), case.agent(&person, |agent| agent.zone_id.clone()));
    let receipt = case.command(1, "exit_9_sources");
    assert_eq!(get(&receipt, "action"), &s("temporary_exit"));
    assert!(!case.has_agent(&person));
    let doc = case.doc();
    let presence = &doc["model_state"][PRESENCE]["people"][&person];
    assert_eq!(presence["status"], "temporarily_absent");
    assert_eq!(presence["expected_return_minute"], 9);
    assert!(doc["model_state"][LIFECYCLE]["people"].get(&person).is_some());
    let receipt = case.command(2, "return");
    assert_eq!(get(&receipt, "action"), &s("returned"));
    assert_eq!(case.agent(&person, |agent| agent.zone_id.clone()), zone);
    assert_eq!(needs_of(&case), needs);
    assert_eq!(case.doc()["model_state"][PRESENCE]["people"][&person]["status"], "admitted");
}

#[test]
fn a_return_to_a_full_zone_waits_and_the_rule_admits_it_later() {
    let mut case = case("generated", "test_a_return_to_a_full_zone_waits_and_the_rule_admits_it_later");
    let person = case.person();
    let zone = case.zone();
    case.command(1, "transfer_other");
    case.set_capacity(&zone, Some(case.occupancy(&zone)));
    let receipt = case.command(2, "return_here");
    assert_eq!(get(&receipt, "action"), &s("waiting"));
    let waiting: Vec<Value> = case.lifecycle_state()["pending_returns"].as_array().unwrap().iter().map(|item| item["agent_id"].clone()).collect();
    assert_eq!(waiting, vec![json!(person)]);
    // Still full.
    assert!(case.admit_returns(3).is_empty());
    assert!(!case.has_agent(&person));
    case.set_capacity(&zone, Some(case.capacity(&zone).unwrap() + 1));
    assert_eq!(case.admit_returns(3), vec![text(&person)]);
    let doc = case.doc();
    assert_eq!(doc["model_state"][LIFECYCLE]["pending_returns"], json!([]));
    assert_eq!(doc["model_state"][PRESENCE]["people"][&person]["status"], "admitted");
    assert_eq!(doc["model_state"][PRESENCE]["history"].as_array().unwrap().last().unwrap()["action"], "returned");
}

#[test]
fn an_exit_of_someone_busy_is_deferred_as_the_rule_says() {
    let ways: [(&str, &str); 3] = [("queued", "the person is queued"), ("request", "the person is waiting for support"), ("task", "the person owns an active task")];
    for (index, (how, reason)) in ways.into_iter().enumerate() {
        let mut case = case("generated", &format!("test_an_exit_of_someone_busy_is_deferred_as_the_rule_says[{index}-{index}]"));
        let person = case.person();
        match how {
            "request" => case.agent(&person, |agent| agent.support_request_id = Some(text("some-request"))),
            "task" => case.edit(|world| {
                let task = TaskRow { id: text("some-task"), kind: text("travel_return"), agent_id: Some(text(&person)), resource_id: None, start_minute: 0, due_minute: 5, staff_id: None };
                world.rows_mut().unwrap().active_tasks.insert(text("some-task"), task);
            }),
            _ => case.agent(&person, |agent| agent.activity = Activity::Queued),
        }
        case.command_suppressed(reason, "transfer");
        assert!(case.pending_ids().iter().any(|id| id.starts_with("presence-retry-")));
    }
}

#[test]
fn an_exit_past_its_expected_return_is_dropped() {
    let mut case = case("generated", "test_an_exit_past_its_expected_return_is_dropped");
    let mut late = event("late-exit", 9, json!({}));
    late["provenance"] = json!({"population_retry": {"count": 1}});
    case.set_minute(9);
    let said = texts::suppressed(case.presence_command(&late, "exit_5"));
    assert_eq!(said, "temporary exit dropped: the person was not free before the expected return");
    case.gave_world();
}

#[test]
fn a_whole_command_is_the_rule_s() {
    let mut case = case("generated", "test_a_whole_command_is_the_rule_s");
    let person = case.person();
    case.set_minute(1);
    let receipt = case.presence_command(&event("whole-command", 1, json!({})), "exit_9").unwrap();
    case.gave_result(receipt);
    assert!(!case.has_agent(&person));
}

#[test]
fn a_reunion_and_a_relocation_in_place_are_the_rule_s() {
    let mut case = case("generated", "test_a_reunion_and_a_relocation_in_place_are_the_rule_s");
    let (first, second) = (input("first"), input("second"));
    let receipt = case.command(1, "reunite");
    let mut sorted = vec![first.clone(), second];
    sorted.sort();
    assert_eq!(get(&receipt, "agent_ids"), &V::List(sorted.iter().map(|id| s(id)).collect()));
    let presence = kit::holding(&case.loaded.cell, &mut case.host).unwrap().held(PRESENCE, "the world holds another presence state");
    let household = case.edit(|world| lifecycle::household_of(world, &presence, &first).unwrap());
    assert_eq!(household.as_deref(), Some("household-new"));
    let receipt = case.command(2, "relocate_in_place");
    assert_eq!(get(&receipt, "action"), &s("relocated"));
}

#[test]
fn a_relocation_walks_and_its_completion_is_the_rule_s() {
    let mut case = case("generated", "test_a_relocation_walks_and_its_completion_is_the_rule_s");
    let person = case.person();
    let destination = case.first_step("relocate");
    case.set_capacity(&destination, None);
    let receipt = case.command(1, "relocate");
    assert_eq!(get(&receipt, "action"), &s("relocation_scheduled"));
    assert_eq!(case.agent(&person, |agent| agent.activity), Activity::Moving);
    let completion = case.arrive_at_destination(&person, &destination);
    case.complete_relocation(&completion);
    let doc = case.doc();
    assert!(doc["model_state"][LIFECYCLE]["pending_relocations"].get(&person).is_none());
    assert_eq!(doc["model_state"][PRESENCE]["history"].as_array().unwrap().last().unwrap()["action"], "relocated");
}

/// It used to fall back to the rule, past the writes: `_initial_dynamics` was the rule's.
#[test]
fn an_admission_after_a_relocation_is_the_kernel_s_too() {
    let mut case = case("generated", "test_an_admission_after_a_relocation_is_the_kernel_s_too");
    let (person, walker) = (case.person(), input("walker"));
    let zone = case.return_waiting(&person);
    case.set_capacity(&zone, None);
    let destination = case.first_step("relocate_walker");
    case.set_capacity(&destination, None);
    case.command(3, "relocate_walker");
    let completion = case.arrive_at_destination(&walker, &destination);
    // The admission that follows the writes makes this person's record.
    case.state(LIFECYCLE, |state| *field(field(field(state, "people"), &person), "agent_dynamics") = V::Null);
    case.complete_relocation(&completion);
    assert!(case.has_agent(&person));
    assert_eq!(case.doc()["model_state"][AGENT_DYNAMICS]["agents"][&person]["decision_due_minute"], completion["due_minute"]);
}

#[test]
fn someone_saved_without_their_dynamics_gets_a_new_record() {
    let mut case = case("generated", "test_someone_saved_without_their_dynamics_gets_a_new_record");
    let person = case.person();
    case.command(1, "exit_9");
    case.state(LIFECYCLE, |state| *field(field(field(state, "people"), &person), "agent_dynamics") = V::Null);
    case.command(2, "return");
    assert_eq!(case.doc()["model_state"][AGENT_DYNAMICS]["agents"][&person]["decision_due_minute"], 2);
}

#[test]
fn a_full_lifecycle_history_drops_its_oldest_as_the_rule_does() {
    let mut case = case("generated", "test_a_full_lifecycle_history_drops_its_oldest_as_the_rule_does");
    let history: Vec<V> = (0..20_000).map(|index| load::v(&json!({"event_id": format!("old-{index}")}))).collect();
    case.state(LIFECYCLE, |state| *field(state, "history") = V::List(history));
    case.command(1, "exit");
    let history = &case.lifecycle_state()["history"];
    assert_eq!(history.as_array().unwrap().len(), 20_000);
    assert_eq!(history[0]["event_id"], "old-1");
}

#[test]
fn a_command_the_rule_refuses_says_its_text_with_nothing_changed() {
    let refusals = [
        ("refused_temporary_exit", "temporary exit requires an admitted person"),
        ("refused_transfer", "transfer requires an admitted person"),
        ("refused_return", "return requires a retained absent person"),
        ("refused_relocate", "relocation requires an admitted person and valid destination"),
        ("refused_reunite", "reunion requires every person to be admitted"),
    ];
    for (name, expected) in refusals {
        let mut case = case("generated", "test_a_command_the_rule_refuses_says_its_text_with_nothing_changed");
        let event = event("refused-command", 0, json!({}));
        let (candidate, mut candidate_host) = case.candidate();
        let before = texts::canonical_of(&candidate.cell, &case.entry);
        let said = texts::failed(action(&case.loaded, &mut case.host, &candidate, &mut candidate_host, &texts::row(&event), &dumped(name)));
        assert_eq!(said, expected);
        assert!(texts::canonical_of(&candidate.cell, &case.entry) == before, "the candidate changed: {expected}");
    }
}

/// A presence command's transaction reads the world in the order the copy had, where the rule read
/// the world as it was: what the plan reads of it (whether the person is busy, the minutes of the
/// program's other commands) must not depend on that order.
#[test]
fn the_plan_reads_the_world_in_any_order() {
    for case_name in ["busy", "return_due"] {
        let case = case("out_and_back", "test_the_plan_reads_the_world_in_any_order");
        let (person, other) = (case.person(), input("walker"));
        let mut exit_event = case.pending().into_iter().find(|event| event["event_type"] == "effect_program_population" && event["payload"]["command"]["action"] == "temporary_exit").expect("the exit");
        for (task_id, agent_id) in [("task-b", &person), ("task-a", &other)] {
            let task = TaskRow { id: text(task_id), kind: text("travel_return"), agent_id: Some(text(agent_id)), resource_id: None, start_minute: 0, due_minute: 5, staff_id: None };
            case.edit(|world| world.rows_mut().unwrap().active_tasks.insert(text(task_id), task));
        }
        let expected = if case_name == "busy" {
            "population exit waits until the person is free: the person owns an active task"
        } else {
            // The exit is retried at the minute its program's return is due: that is read first.
            exit_event["due_minute"] = json!(3);
            "temporary exit dropped: the person was not free before the return in the same EffectProgram"
        };
        case.set_minute(exit_event["due_minute"].as_i64().unwrap());
        let row = texts::row(&exit_event);
        let mut outcomes = Vec::new();
        let mut orders = Vec::new();
        for reverse in [false, true] {
            let (reader, mut reader_host) = case.candidate();
            reader.cell.with(|world| in_order(world, reverse)).unwrap();
            orders.push(reader.cell.with(|world| world.rows().unwrap().agents.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>()).unwrap());
            let (candidate, mut candidate_host) = case.candidate();
            let said = texts::suppressed(action(&reader, &mut reader_host, &candidate, &mut candidate_host, &row, &dumped("out")));
            outcomes.push((said, texts::canonical_of(&candidate.cell, &case.entry), texts::canonical_of(&reader.cell, &case.entry)));
        }
        assert_ne!(orders[0], orders[1]);
        assert_eq!(outcomes[0].0, expected);
        assert!(outcomes[0] == outcomes[1], "{case_name}");
    }
}

/// `_in_order`: the world's people and tasks in the canonical order (by ID), or its reverse.
fn in_order(world: &mut World, reverse: bool) {
    fn sorted<R: Clone>(table: &Table<R>, reverse: bool) -> Table<R> {
        let mut rows: Vec<(Arc<str>, R)> = table.iter().map(|(id, row)| (id.clone(), row.clone())).collect();
        rows.sort_by(|first, second| first.0.as_bytes().cmp(second.0.as_bytes()));
        if reverse {
            rows.reverse();
        }
        let mut made = Table::default();
        for (id, row) in rows {
            made.insert(id, row);
        }
        made
    }
    let mut rows = world.rows_mut().unwrap();
    let (agents, tasks) = (sorted(&rows.agents, reverse), sorted(&rows.active_tasks, reverse));
    rows.agents = agents;
    rows.active_tasks = tasks;
}

// ---- Rooms of the overlay ------------------------------------------------------------------------

/// `_arrive_in_room`: arrivals for the full medical area, given the quiet area's room.
fn arrive_in_room(case: &mut Case, count: i64) {
    let given = Given { zone: Ok(Some(text("medical"))), alternates: Ok(Alternates::Zones(vec![text("quiet")])), ..given("medical", count) };
    let arrived = case.arrivals(&event("arrival-event", 1, json!({})), given).unwrap();
    case.gave_result(V::Int(arrived));
    assert_eq!(arrived, count);
}

#[test]
fn people_given_a_room_in_one_event_stand_apart() {
    let mut case = case("room", "test_people_given_a_room_in_one_event_stand_apart");
    arrive_in_room(&mut case, 3);
    assert_eq!(case.doc()["model_state"][OVERLAY]["room_admissions"].as_object().unwrap().len(), 3);
}

#[test]
fn a_room_admission_keeps_half_a_metre_from_who_is_there() {
    let mut probe = case("room", "test_a_room_admission_keeps_half_a_metre_from_who_is_there");
    arrive_in_room(&mut probe, 1);
    let doc = probe.doc();
    let admitted: Vec<&String> = doc["model_state"][OVERLAY]["room_admissions"].as_object().unwrap().keys().collect();
    assert_eq!(admitted.len(), 1);
    let point = doc["model_state"][MOVEMENT]["positions"][admitted[0].as_str()]["point"].clone();

    let mut case = case("room", "test_a_room_admission_keeps_half_a_metre_from_who_is_there");
    case.gave = std::mem::replace(&mut probe.gave, NONE.iter());
    let resident = case.person();
    case.state(OVERLAY, |state| {
        let V::Map(admissions) = field(state, "room_admissions") else { panic!() };
        let record = json!({"agent_id": resident, "room_id": "room", "admitted_minute": 1, "event_id": "earlier", "source_ids": ["ASM-TEST-ROOM"]});
        admissions.insert(text(&resident), load::v(&record));
    });
    // Just past the rule's distance, so the same point is still free.
    let moved = json!({"x": point["x"].as_f64().unwrap() + 0.501, "y": point["y"]});
    case.typed_state(MOVEMENT, |movement| movement["positions"][&resident]["point"] = moved);
    arrive_in_room(&mut case, 1);
    let doc = case.doc();
    let new = doc["model_state"][OVERLAY]["room_admissions"].as_object().unwrap().keys().find(|key| **key != resident).unwrap().clone();
    assert_eq!(doc["model_state"][MOVEMENT]["positions"][&new]["point"], point);
}

// ---- Validation --------------------------------------------------------------------------------

#[test]
fn a_broken_population_state_raises_the_rule_s_text() {
    type Break = fn(&mut V);
    let breaks: [(Break, Option<&str>); 4] = [
        (|state| *field(&mut list(field(state, "pending_arrivals"))[0], "zone_id") = V::Str(text("no-such-zone")), Some("population_dynamics_v1 pending arrival is invalid")),
        // PersonTraits refused it: a contract's error is not the rule's own text.
        (|state| *field(field(&mut list(field(state, "pending_arrivals"))[0], "traits"), "age_years") = V::Int(500), None),
        (|state| *field(first_value(field(state, "profile_context")), "source_ids") = V::List(Vec::new()), Some("dynamic household context disagrees with generated profile provenance")),
        (
            |state| {
                let V::Map(pending) = &mut list(field(state, "pending_arrivals"))[0] else { panic!() };
                pending.insert(text("something_else"), V::Int(1));
            },
            Some("population_dynamics_v1 pending arrival is invalid"),
        ),
    ];
    for (index, (breaks, expected)) in breaks.into_iter().enumerate() {
        let mut case = case("generated", &format!("test_a_broken_population_state_raises_the_rule_s_text[{index}-{index}]"));
        let zone = case.zone();
        case.set_capacity(&zone, Some(case.occupancy(&zone)));
        case.arrive(2, 2, Alternates::NotGiven);
        case.validate_dynamics().unwrap();
        case.state(DYNAMICS, breaks);
        match expected {
            None => unsupported(case.validate_dynamics()),
            Some(expected) => assert_eq!(value_error(case.validate_dynamics()), expected),
        }
    }
}

#[test]
fn a_broken_lifecycle_state_raises_the_rule_s_text() {
    type Break = fn(&mut V);
    let breaks: [(Break, &str); 4] = [
        (
            |state| {
                let V::Map(fields) = state else { panic!() };
                fields.insert(text("extra"), V::Int(1));
            },
            "population_lifecycle_v1 has an invalid exact schema",
        ),
        (|state| *field(field(first_value(field(state, "people")), "agent"), "stress") = V::Float(2.0), "population lifecycle normalized Agent value is invalid"),
        (|state| list(field(state, "relationship_archive")).push(load::v(&json!({"namespace": "other"}))), "population_lifecycle_v1 relationship archive is invalid"),
        (|state| list(field(state, "pending_returns")).push(load::v(&json!({"agent_id": "nobody"}))), "population_lifecycle_v1 pending return is invalid"),
    ];
    for (index, (breaks, expected)) in breaks.into_iter().enumerate() {
        let mut case = case("generated", &format!("test_a_broken_lifecycle_state_raises_the_rule_s_text[{index}-{index}]"));
        case.command(1, "exit");
        validate_lifecycle(&case.loaded, &mut case.host).unwrap();
        case.state(LIFECYCLE, breaks);
        assert_eq!(value_error(validate_lifecycle(&case.loaded, &mut case.host)), expected);
    }
}

#[test]
fn a_broken_presence_ledger_stops_the_kernels_before_a_write() {
    let mut case = case("generated", "test_a_broken_presence_ledger_stops_the_kernels_before_a_write");
    let zone = case.zone();
    case.set_capacity(&zone, Some(case.occupancy(&zone)));
    case.arrive(1, 2, Alternates::NotGiven);
    case.set_capacity(&zone, Some(case.capacity(&zone).unwrap() + 1));
    case.state(PRESENCE, |state| list(field(state, "history")).push(load::v(&json!({"unknown": 1}))));
    let before = case.bytes();
    unsupported(population::admit_arrivals(&case.loaded.cell, &mut case.host, 0));
    assert!(case.bytes() == before, "the world changed");
}
