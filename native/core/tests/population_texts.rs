//! The saved texts of the population's rules, byte for byte (Issue #225, lane M2; moved from
//! `tests/unit/domain/test_population_texts.py`).
//!
//! A rule's `EventSuppressed` and `EventApplicationError` texts go to the event's record as
//! `detail`, so the core must raise what the Python rule raised, in its order of checks, after its
//! writes and no others. Each case reaches one check. The Python rules are gone (lane L10,
//! `kernel_only`): each case was run with them once (2d8fb46), and the class they raised and the
//! world they left were pinned in the pytest file's `PYTHON_GAVE`, as 16 hex digits of the sha256 of
//! the class's JSON and the sha256 of the world's canonical JSON. `PYTHON_GAVE` here is that table:
//! the core's world writes the same canonical text (`support/texts.rs`), so the digests are the
//! pytest file's, unchanged.
//!
//! The worlds are the generated Scenario the Project screen makes (`tests/data/generated_texts.json`,
//! `tests/support/core_fixtures_texts.py`), and its variants with an arrival approach route in the
//! floor plan (the configs' sections Python parses change with it) and with the population programs
//! the tests schedule, built and validated with pydantic. The commands are what
//! `PopulationPresenceCommand` made of the pytest file's (`model_dump(mode="json")`). The other edits
//! are made here, as the pytest file made them. Each argument of a rule is what the PyO3 crate reads
//! of the Python value the pytest file passed (`native/src/rules/population.rs`). Where a rule asks
//! pydantic's contracts of an arrival's language or person context, the host gives what the fixture
//! recorded they gave; where it starts a namespace in `model_state`, the host puts it in the world,
//! as the native host does (`load::following_host`).

mod support;

use std::rc::Rc;
use std::sync::Arc;

use serde_json::{json, Value};
use sheltersim_core::checkpoint::sha256_hex;
use sheltersim_core::entities::Activity;
use sheltersim_core::error::Error;
use sheltersim_core::rules::handlers;
use sheltersim_core::rules::kit;
use sheltersim_core::rules::population::{self, Alternates, DepartureGiven, Given, Language, Overrides, OTHER_PROGRAM};
use sheltersim_core::rules::population_lifecycle::{self as lifecycle, Command, Logged, Read};
use sheltersim_core::row::ValueRow;
use sheltersim_core::value::V;
use sheltersim_core::host::Contract;
use sheltersim_core::world::{Fit, World, WorldCell};

use support::cases;
use support::load::{self, Loaded};
use support::texts::{self, changed};
use support::TestHost;

const FILE: &str = "generated_texts";
const DYNAMICS: &str = "population_dynamics_v1";
const LIFECYCLE: &str = "population_lifecycle_v1";
const PRESENCE: &str = "population_presence_v1";
const ROUTE: &str = "arrival-route-test";

/// What the Python rules gave, per test and in call order (the pytest file's `PYTHON_GAVE`).
const PYTHON_GAVE: &[(&str, &[&str])] = &[
    ("a_command_outside_its_contract_or_minute", &["bbded8a4299217b9", "330cb0ce4aca5606", "a34be4debf2ed930"]),
    ("a_departure", &["0717aceb0e0c255a"]),
    ("a_first_departure", &["0717aceb0e0c255a"]),
    ("a_relocation_completion_without_its_route", &["0717aceb0e0c255a"]),
    ("a_relocation_of_someone_busy", &["0717aceb0e0c255a"]),
    ("a_relocation", &["0717aceb0e0c255a", "0717aceb0e0c255a", "62c908964806b306", "1a2791ee21c6d1f5"]),
    ("a_relocation_without_a_route", &["1ebe95b9e59bbd40"]),
    ("a_return", &["0717aceb0e0c255a", "0717aceb0e0c255a", "1f944ffd14f66dcd", "1f944ffd14f66dcd"]),
    ("a_reunion", &["0717aceb0e0c255a", "6416570b8ed318d5"]),
    ("a_route_into_another_zone", &["75e22870dd41adf1"]),
    ("a_waiting_approach_with_a_record", &["868d27782577864d"]),
    ("a_waiting_arrival_with_a_record", &["96d651595c7e09bf"]),
    ("alternate_zones", &["0717aceb0e0c255a", "0717aceb0e0c255a", "0717aceb0e0c255a", "f51474277b1eedfb"]),
    (
        "an_approach_completion",
        &[
            "41712a52628b2785",
            "41712a52628b2785",
            "41712a52628b2785",
            "41712a52628b2785",
            "41712a52628b2785",
            "41712a52628b2785",
            "41712a52628b2785",
            "5f7a1afad2f1df22",
        ],
    ),
    ("an_approach_completion_without_its_presence", &["b17ba2fd2b969f08"]),
    ("an_approach_outside_an_approved_program", &["0459e0eae290c15a"]),
    ("an_approach_without_its_program_s_provenance", &["eb2e36d9115c46a1"]),
    ("an_arrival", &["0717aceb0e0c255a"]),
    ("an_exit_or_transfer_of_someone_absent", &["0717aceb0e0c255a", "0717aceb0e0c255a"]),
    ("the_bounds", &["2f8abb4cf454e76a", "86452fe232195d46", "2adaba550e488fa6"]),
    ("the_same_arrival_again", &["b111d5818c2946b7"]),
];

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

/// A world of the fixture in a cell, with a host of its sections and floor plan, and the digests
/// the Python rule gave in this test, in turn.
struct Case {
    entry: Value,
    loaded: Rc<Loaded>,
    host: TestHost,
    gave: std::slice::Iter<'static, &'static str>,
    /// The case, for the messages.
    label: String,
}

fn case(name: &str, test: &str) -> Case {
    let entry = cases::entry(FILE, name);
    let loaded = Rc::new(load::loaded(&entry));
    let host = load::host(&entry);
    let gave = PYTHON_GAVE.iter().find(|(name, _)| *name == test).unwrap_or_else(|| panic!("{test}")).1.iter();
    Case { entry, loaded, host, gave, label: String::new() }
}

/// What the pytest file picked in Python's order and what the contracts made (on `generated`).
fn inputs() -> Value {
    load::entry(FILE, "generated")["population"].take()
}

impl Case {
    fn edit<T>(&self, edit: impl FnOnce(&mut World) -> T) -> T {
        self.loaded.cell.with(edit).unwrap()
    }

    fn bytes(&self) -> String {
        texts::canonical_of(&self.loaded.cell, &self.entry)
    }

    /// `_says`: the rule raises `expected`, its own outcome, of the class the Python rule raised
    /// and after the writes it made: the class and the world are `PYTHON_GAVE`'s next digest.
    fn says<T>(&mut self, expected: &str, rule: impl FnOnce(&mut Self) -> Result<T, Error>) {
        let (class, said) = texts::outcome(rule(self));
        assert_eq!(said, expected, "{}", self.label);
        let digest = sha256_hex(format!("\"{class}\"\n{}", texts::sha(&self.bytes())).as_bytes());
        assert_eq!(&digest[..16], *self.gave.next().expect("a digest of this call"), "{expected} {}", self.label);
    }

    /// `_unchanged`: `says`, for a check before any write: the world is as it was.
    fn unchanged<T>(&mut self, expected: &str, rule: impl FnOnce(&mut Self) -> Result<T, Error>) {
        let before = self.bytes();
        self.says(expected, rule);
        assert!(self.bytes() == before, "the world changed: {expected} {}", self.label);
    }

    /// `_handler_says`: `EffectProgramPopulationHandler().apply(world, event)` raises `class` with
    /// `expected`, with the world unchanged.
    fn handler_says(&mut self, event: &Value, class: &str, expected: &str) {
        let before = self.bytes();
        let row = texts::row(event);
        let said = texts::outcome(handlers::population_event(&self.loaded.cell, &mut self.host, &row));
        assert_eq!(said, (class_name(class), expected.to_owned()));
        assert!(self.bytes() == before, "the world changed: {expected}");
    }

    fn seed(&self) -> String {
        self.entry["world"]["seed"].to_string()
    }

    fn zone(&self) -> String {
        inputs_zone(&self.entry)
    }

    fn other_zone(&self) -> String {
        let here = self.zone();
        self.sorted("zones").into_iter().find(|zone| *zone != here).unwrap()
    }

    fn sorted(&self, table: &str) -> Vec<String> {
        let mut ids: Vec<String> = self.entry["world"][table].as_array().unwrap().iter().map(|row| row["id"].as_str().unwrap().to_owned()).collect();
        ids.sort();
        ids
    }

    /// A namespace's value, to change (`ValueRow`s: the population's).
    fn state<T>(&self, name: &str, change: impl FnOnce(&mut V) -> T) -> T {
        self.edit(|world| change(&mut world.row_mut::<ValueRow>(name).unwrap().0))
    }

    fn drop_namespace(&self, name: &str) {
        self.edit(|world| world.slots.get(name).unwrap().try_borrow_mut().unwrap().row = None);
    }

    fn set_minute(&self, minute: i64) {
        self.edit(|world| world.scalars.get_mut().minute_index = Fit::Ok(minute));
    }

    /// `population.queue_arrivals(world, event=..., zone_id=..., count=..., **options)`.
    fn arrivals(&mut self, event: &Value, given: Given) -> Result<i64, Error> {
        population::queue_arrivals(&self.loaded.cell, &mut self.host, &texts::row(event), given)
    }

    /// `population.queue_departures(...)`.
    fn departures(&mut self, event: &Value, given: DepartureGiven) -> Result<usize, Error> {
        population::queue_departures(&self.loaded.cell, &mut self.host, &texts::row(event), given)
    }

    /// `apply_population_presence_command(world, event, fields)`, with what the contract made of
    /// `fields`.
    fn command(&mut self, event: &Value, command: &str) -> Result<V, Error> {
        let dumped = load::v(&inputs()["commands"][command]);
        lifecycle::presence_command_typed(&self.loaded.cell, &mut self.host, &texts::row(event), &dumped)
    }

    /// `_command`: the world at `minute`, the command's event at `event_minute` (`minute` unless
    /// given), and the rule's text with the world unchanged.
    fn command_says(&mut self, expected: &str, minute: i64, event_minute: Option<i64>, action: &str, command: &str) {
        self.set_minute(minute);
        let event = event(&format!("command-{action}"), event_minute.unwrap_or(minute), json!({}));
        self.unchanged(expected, |case| case.command(&event, command));
    }

    fn person(&self) -> String {
        inputs()["person"].as_str().unwrap().to_owned()
    }

    fn there(&self) -> String {
        inputs()["there"].as_str().unwrap().to_owned()
    }

    fn agent_zone(&self, agent_id: &str) -> Arc<str> {
        self.edit(|world| world.rows().unwrap().agents.get(agent_id).unwrap().zone_id.clone())
    }

    fn set_capacity(&self, zone: &str, capacity: Option<i64>) {
        self.edit(|world| world.rows_mut().unwrap().zones.get_mut(zone).unwrap().capacity = capacity);
    }

    /// `_departed_record`: a presence record for someone not in the shelter.
    fn departed_record(&self, agent_id: &str) {
        let mut record = load::v(&inputs()["departed_record"]);
        let V::Map(fields) = &mut record else { panic!() };
        fields.insert(text("agent_id"), V::Str(text(agent_id)));
        self.state(PRESENCE, |state| map(state, "people").insert(text(agent_id), record));
    }

    fn pending_arrivals(&self) -> Vec<V> {
        self.state(DYNAMICS, |state| match field(state, "pending_arrivals") {
            V::List(items) => items.clone(),
            other => panic!("{other:?}"),
        })
    }
}

fn class_name(class: &str) -> &'static str {
    match class {
        "EventSuppressed" => "EventSuppressed",
        _ => "EventApplicationError",
    }
}

/// `_zone`: the zone of the person the pytest file picked first (`next(iter(world.agents))`).
fn inputs_zone(entry: &Value) -> String {
    let person = inputs()["person"].as_str().unwrap().to_owned();
    entry["world"]["agents"].as_array().unwrap().iter().find(|agent| agent["profile"]["id"] == person.as_str()).unwrap()["zone_id"].as_str().unwrap().to_owned()
}

/// A map in a `V` map, by key, to change.
fn map<'a>(value: &'a mut V, key: &str) -> &'a mut indexmap::IndexMap<Arc<str>, V> {
    match field(value, key) {
        V::Map(found) => found,
        other => panic!("{key} is not a map: {other:?}"),
    }
}

/// A value in a `V` map, by key, to change.
fn field<'a>(value: &'a mut V, key: &str) -> &'a mut V {
    let V::Map(fields) = value else { panic!("not a map") };
    fields.get_mut(key).unwrap_or_else(|| panic!("no {key}"))
}

/// `_event` of the pytest file: an event of the test's source, its sequence from its ID.
fn event(event_id: &str, minute: i64, payload: Value) -> Value {
    event_of(event_id, minute, "effect_program_population", payload)
}

fn event_of(event_id: &str, minute: i64, event_type: &str, payload: Value) -> Value {
    let digest = sha256_hex(event_id.as_bytes());
    json!({
        "event_id": event_id,
        "kind": "intervention",
        "due_minute": minute,
        "priority": 0,
        "sequence": 900_000 + i64::from_str_radix(&digest[..4], 16).unwrap(),
        "event_type": event_type,
        "payload": payload,
        "source_id": "ASM-TEST-POPULATION-TEXTS",
        "causation_id": null,
        "provenance": {},
    })
}

/// `_arrival_id`: the ID an arrival of this event generates.
fn arrival_id(seed: &str, event_id: &str, index: i64) -> String {
    format!("arrival-{}", &sha256_hex(format!("{seed}\0{event_id}\0{index}").as_bytes())[..28])
}

/// The arguments of `queue_arrivals(event=..., zone_id=zone, count=1)`.
fn given(zone: &str) -> Given {
    Given {
        zone: Ok(Some(text(zone))),
        alternates: Ok(Alternates::NotGiven),
        route_id: None,
        count: Ok(Some(1)),
        household_size: None,
        mobility: None,
        language: None,
        overrides: None,
    }
}

fn zones(ids: &[&str]) -> Alternates {
    Alternates::Zones(ids.iter().map(|id| text(id)).collect())
}

fn route(id: &str) -> Option<Option<Arc<str>>> {
    Some(Some(text(id)))
}

/// The scheduled event of a population program (`_program`).
fn program_event(entry: &Value, op: &str) -> Value {
    let program = format!("population-texts-{op}");
    let found: Vec<&Value> =
        entry["world"]["event_queue"].as_array().unwrap().iter().filter(|event| event["event_type"] == "effect_program_population" && event["payload"]["program_id"] == program.as_str()).collect();
    assert_eq!(found.len(), 1);
    found[0].clone()
}

// ---- queue_arrivals --------------------------------------------------------------------------

#[test]
fn an_arrival_the_rule_refuses_says_its_text() {
    let unknown = "population arrival references an unknown zone";
    let alternates = "population arrival alternate admission zones are invalid";
    let route_invalid = "population arrival approach route is invalid";
    let count = "population arrival count is outside its supported range";
    let household = "population arrival household_size is outside its supported range";
    let mobility = "population arrival mobility is invalid";
    let language = "population arrival language is invalid";
    let context = "population arrival person context is invalid";
    let index = "population arrival person context index is invalid";
    let needs = |index: i64, need: &str| V::Map([(text("member_index"), V::Int(index)), (text("care_support_needs"), V::List(vec![V::Str(text(need))]))].into_iter().collect());
    // Each option as the PyO3 crate reads the pytest file's Python value of it.
    let cases: Vec<(&str, Box<dyn Fn(&mut Given)>, &str)> = vec![
        ("zone_id='no-such-zone'", Box::new(|given| given.zone = Ok(Some(text("no-such-zone")))), unknown),
        ("zone_id=7", Box::new(|given| given.zone = Ok(None)), unknown),
        ("admission_zone_ids='abc'", Box::new(|given| given.alternates = Ok(Alternates::Invalid)), alternates),
        ("admission_zone_ids=None", Box::new(|given| given.alternates = Ok(Alternates::Invalid)), alternates),
        ("admission_zone_ids=(5,)", Box::new(|given| given.alternates = Ok(Alternates::Invalid)), alternates),
        ("admission_zone_ids=['no-such-zone']", Box::new(|given| given.alternates = Ok(zones(&["no-such-zone"]))), alternates),
        ("approach_route_id='no-such-route'", Box::new(|given| given.route_id = route("no-such-route")), route_invalid),
        ("approach_route_id=5", Box::new(|given| given.route_id = Some(None)), route_invalid),
        ("count=0", Box::new(|given| given.count = Ok(Some(0))), count),
        ("count=1001", Box::new(|given| given.count = Ok(Some(1001))), count),
        // `True`, `"3"` and `2.0` are no int to the rule.
        ("count=True", Box::new(|given| given.count = Ok(None)), count),
        ("count='3'", Box::new(|given| given.count = Ok(None)), count),
        ("count=2.0", Box::new(|given| given.count = Ok(None)), count),
        ("household_size=13", Box::new(|given| given.household_size = Some(Ok(Some(13)))), household),
        ("household_size=None", Box::new(|given| given.household_size = Some(Ok(None))), household),
        // No member of `Mobility`.
        ("mobility='flying'", Box::new(|given| given.mobility = Some(Ok(None))), mobility),
        ("mobility=None", Box::new(|given| given.mobility = Some(Ok(None))), mobility),
        ("mobility=5", Box::new(|given| given.mobility = Some(Ok(None))), mobility),
        // No identifier: the rule asks the contract of a person's traits.
        ("language='not a language'", Box::new(|given| given.language = Some(Ok(Language::Ask(V::Str(text("not a language")))))), language),
        ("language=''", Box::new(|given| given.language = Some(Ok(Language::Ask(V::Str(text("")))))), language),
        ("language=None", Box::new(|given| given.language = Some(Ok(Language::Ask(V::Null)))), language),
        ("language=5", Box::new(|given| given.language = Some(Ok(Language::Ask(V::Int(5))))), language),
        (
            "person_context_overrides=({'member_index': 0},)",
            Box::new(|given| given.overrides = Some(Ok(Overrides::Items(vec![V::Map([(text("member_index"), V::Int(0))].into_iter().collect())])))),
            context,
        ),
        // Iterating an int raises the TypeError the rule words.
        ("person_context_overrides=5", Box::new(|given| given.overrides = Some(Ok(Overrides::Caught))), context),
        ("person_context_overrides=(index 1 of one,)", Box::new(move |given| given.overrides = Some(Ok(Overrides::Items(vec![needs(1, "medical_assessment")])))), index),
        (
            "count=3, person_context_overrides=(index 1 twice)",
            Box::new(move |given| {
                given.count = Ok(Some(3));
                given.overrides = Some(Ok(Overrides::Items(vec![needs(1, "medical_assessment"), needs(1, "language_access")])));
            }),
            index,
        ),
    ];
    for (options, set, expected) in cases {
        let mut case = case("generated", "an_arrival");
        case.host.contracts = Some(Box::new(arrival_contracts));
        case.label = options.to_owned();
        let mut arguments = given(&case.zone());
        set(&mut arguments);
        case.unchanged(expected, |case| case.arrivals(&event("arrival-event", 0, json!({})), arguments));
    }
}

/// The native host's contracts, as the fixture recorded what they gave for the arguments of
/// `an_arrival_the_rule_refuses_says_its_text` (pydantic's models); any other input is given back.
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
fn alternate_zones_the_rule_refuses_say_its_texts() {
    let mut case = case("generated", "alternate_zones");
    let (here, other) = (case.zone(), case.other_zone());
    let invalid = "population arrival alternate admission zones are invalid";
    for alternates in [zones(&[&here]), zones(&[&other, &other])] {
        let arguments = Given { alternates: Ok(alternates), ..given(&here) };
        case.unchanged(invalid, |case| case.arrivals(&event("arrival-event", 0, json!({})), arguments));
    }
    let arguments = Given { alternates: Ok(zones(&[&other])), route_id: route("any-route"), ..given(&here) };
    case.unchanged("one arrival approach route cannot combine with alternate admission zones", |case| case.arrivals(&event("arrival-event", 0, json!({})), arguments));
    // The configs without `geometry_overlay` (a world of the fixture: the sections change).
    let mut without = self::case("no_overlay_config", "alternate_zones");
    without.gave = case.gave;
    let arguments = Given { alternates: Ok(zones(&[&other])), ..given(&here) };
    without.unchanged("alternate room admission requires geometry_overlay schema_version=3 or 4", |case| case.arrivals(&event("arrival-event", 0, json!({})), arguments));
}

#[test]
fn a_route_into_another_zone_says_the_rule_s_text() {
    let mut case = case("route_other", "a_route_into_another_zone");
    let arguments = Given { route_id: route(ROUTE), ..given(&case.zone()) };
    case.unchanged("population arrival approach route targets a different zone", |case| case.arrivals(&event("arrival-event", 0, json!({})), arguments));
}

#[test]
fn the_bounds_say_the_rule_s_texts() {
    let mut case = case("generated", "the_bounds");
    let zone = case.zone();
    let thousand = || Given { count: Ok(Some(1000)), ..given(&zone) };
    let empties = |count: usize| V::List(vec![V::Map(Default::default()); count]);
    case.state(DYNAMICS, |state| *field(state, "pending_arrivals") = empties(9001));
    case.unchanged("population arrival exceeds the pending capacity bound", |case| case.arrivals(&event("arrival-event", 0, json!({})), thousand()));
    case.state(DYNAMICS, |state| *field(state, "pending_arrivals") = empties(9000));
    case.unchanged("population arrival exceeds the 10000-resident runtime bound", |case| case.arrivals(&event("arrival-event", 0, json!({})), thousand()));
    case.state(DYNAMICS, |state| {
        *field(state, "pending_arrivals") = V::List(Vec::new());
        let traits = map(state, "profile_traits");
        for index in 0..9001 {
            traits.insert(text(&format!("person-{index}")), V::Map(Default::default()));
        }
    });
    case.unchanged("population arrival exceeds the 10000-profile history bound", |case| case.arrivals(&event("arrival-event", 0, json!({})), thousand()));
}

#[test]
fn the_same_arrival_again_says_the_rule_s_text() {
    let mut case = case("generated", "the_same_arrival_again");
    let zone = case.zone();
    case.set_capacity(&zone, None);
    let twice = event("arrival-twice", 0, json!({}));
    let two = || Given { count: Ok(Some(2)), ..given(&zone) };
    assert_eq!(case.arrivals(&twice, two()).unwrap(), 2);
    case.unchanged("population arrival generated a duplicate stable ID", |case| case.arrivals(&twice, two()));
}

#[test]
fn a_waiting_arrival_with_a_record_stops_after_it_joins_the_waiting() {
    let mut case = case("generated", "a_waiting_arrival_with_a_record");
    let zone = case.zone();
    case.set_capacity(&zone, Some(0));
    let recorded = event("arrival-recorded", 0, json!({}));
    case.departed_record(&arrival_id(&case.seed(), "arrival-recorded", 1));
    // The first waits and is written; the second has a record: the rule stops there.
    case.says("waiting arrival already has a presence record", |case| case.arrivals(&recorded, Given { count: Ok(Some(2)), ..given(&zone) }));
    assert_eq!(case.pending_arrivals().len(), 2);
}

#[test]
fn an_approach_outside_an_approved_program_stops_after_the_first_writes() {
    let mut case = case("route_here", "an_approach_outside_an_approved_program");
    let zone = case.zone();
    let unapproved = event("arrival-unapproved", 0, json!({"program_id": "none", "effect_id": "none"}));
    case.says("arrival approach is not part of an approved EffectProgram", |case| {
        case.arrivals(&unapproved, Given { count: Ok(Some(2)), route_id: route(ROUTE), ..given(&zone) })
    });
}

#[test]
fn an_approach_without_its_program_s_provenance_stops_after_its_sequence() {
    let mut case = case("route_here_program", "an_approach_without_its_program_s_provenance");
    let zone = case.zone();
    let program = changed(&program_event(&case.entry, "add_evacuees"), json!({}), json!({"provenance": {}}));
    let sequence = case.edit(|world| world.id_counter("event").unwrap());
    case.says("arrival event lacks its approved EffectProgram provenance", |case| {
        case.arrivals(&program, Given { count: Ok(Some(2)), route_id: route(ROUTE), ..given(&zone) })
    });
    assert_ne!(case.edit(|world| world.id_counter("event").unwrap()), sequence);
}

#[test]
fn a_waiting_approach_with_a_record_stops_after_it_joins_the_waiting() {
    let mut case = case("route_here_program", "a_waiting_approach_with_a_record");
    let zone = case.zone();
    let program = program_event(&case.entry, "add_evacuees");
    case.departed_record(&arrival_id(&case.seed(), program["event_id"].as_str().unwrap(), 0));
    case.says("waiting arrival already has a presence record", |case| case.arrivals(&program, Given { route_id: route(ROUTE), ..given(&zone) }));
}

// ---- apply_arrival_approach_completion ------------------------------------------------------

/// `_walking`: one arrival walking the approach route, and its completion event.
fn walking(test: &str) -> (Case, Value) {
    let mut case = case("route_here_program", test);
    let zone = case.zone();
    let program = program_event(&case.entry, "add_evacuees");
    case.set_minute(program["due_minute"].as_i64().unwrap());
    case.arrivals(&program, Given { route_id: route(ROUTE), ..given(&zone) }).unwrap();
    let pending = case.pending_arrivals();
    assert_eq!(pending.len(), 1);
    let V::Map(fields) = &pending[0] else { panic!() };
    let V::Str(completion_id) = &fields["approach_completion_event_id"] else { panic!() };
    // The completion as the queue holds it.
    let completion = case.edit(|world| {
        let queue = world.queue().unwrap();
        let row = queue.slot(queue.serial_of(completion_id).unwrap()).row.clone().unwrap();
        let mut encoder = sheltersim_core::json::Encoder::with_capacity(256);
        row.write_json(&mut encoder).unwrap();
        serde_json::from_slice::<Value>(&encoder.out).unwrap()
    });
    (case, completion)
}

#[test]
fn an_approach_completion_the_rule_refuses_says_its_text() {
    let (mut case, completion) = walking("an_approach_completion");
    let fields = "arrival approach completion payload fields do not match";
    let program = "arrival approach completion is outside its approved program";
    let pending = "arrival approach completion does not match pending arrival";
    let provenance = "arrival approach route provenance does not match";
    let due = completion["due_minute"].as_i64().unwrap();
    let cases: Vec<(Value, &str)> = vec![
        (changed(&completion, json!({}), json!({"event_type": "population_approach_finished"})), fields),
        (changed(&completion, json!({"extra": 1}), json!({})), fields),
        (changed(&completion, json!({"program_id": "none"}), json!({})), program),
        (changed(&completion, json!({}), json!({"causation_id": "another-event"})), program),
        (changed(&completion, json!({"agent_id": "nobody"}), json!({})), pending),
        (changed(&completion, json!({}), json!({"due_minute": due + 1})), pending),
        (changed(&completion, json!({}), json!({"source_id": "SRC-ANOTHER"})), provenance),
    ];
    for (event, expected) in cases {
        case.unchanged(expected, |case| population::approach_completion(&case.loaded.cell, &mut case.host, &texts::row(&event)));
    }
    case.state(DYNAMICS, |state| {
        let V::List(items) = field(state, "pending_arrivals") else { panic!() };
        let V::Map(pending) = &mut items[0] else { panic!() };
        pending.insert(text("approach_route_id"), V::Str(text("another-route")));
    });
    let event = changed(&completion, json!({"route_id": "another-route"}), json!({}));
    case.unchanged(provenance, |case| population::approach_completion(&case.loaded.cell, &mut case.host, &texts::row(&event)));
}

#[test]
fn an_approach_completion_without_its_presence_stops_after_two_writes() {
    let (mut case, completion) = walking("an_approach_completion_without_its_presence");
    let agent_id = match &case.pending_arrivals()[0] {
        V::Map(fields) => match &fields["profile"] {
            V::Map(profile) => profile["id"].clone(),
            _ => panic!(),
        },
        _ => panic!(),
    };
    let V::Str(agent_id) = agent_id else { panic!() };
    case.state(PRESENCE, |state| {
        let V::Map(record) = map(state, "people").get_mut(&*agent_id).unwrap() else { panic!() };
        record.insert(text("status"), V::Str(text("awaiting_admission")));
    });
    case.says("arrival approach has no matching presence state", |case| population::approach_completion(&case.loaded.cell, &mut case.host, &texts::row(&completion)));
    let V::Map(pending) = &case.pending_arrivals()[0] else { panic!() };
    assert_eq!(pending["approach_completed_minute"], V::Int(completion["due_minute"].as_i64().unwrap()));
    assert!(!pending.contains_key("approach_completion_event_id"));
}

// ---- queue_departures ------------------------------------------------------------------------

#[test]
fn a_departure_the_rule_refuses_says_its_text() {
    let probe = case("generated", "a_departure");
    let zone = probe.zone();
    let people = probe.entry["world"]["agents"].as_array().unwrap().iter().filter(|agent| agent["zone_id"] == zone.as_str()).count();
    let given = |zone: &str| DepartureGiven { zone: Ok(Some(text(zone))), count: Ok(Some(1)), include_initial: None };
    let cases: Vec<(DepartureGiven, String)> = vec![
        (DepartureGiven { zone: Ok(Some(text("no-such-zone"))), ..given(&zone) }, "population departure references an unknown zone".to_owned()),
        (DepartureGiven { count: Ok(Some(0)), ..given(&zone) }, "population departure count is outside its supported range".to_owned()),
        // `False` is no int to the rule.
        (DepartureGiven { count: Ok(None), ..given(&zone) }, "population departure count is outside its supported range".to_owned()),
        // `None` and `1` are no bool.
        (DepartureGiven { include_initial: Some(None), ..given(&zone) }, "population departure include_initial must be boolean".to_owned()),
        (DepartureGiven { include_initial: Some(None), ..given(&zone) }, "population departure include_initial must be boolean".to_owned()),
        (given(&zone), "only 0 admitted dynamic residents are available to leave".to_owned()),
        (DepartureGiven { count: Ok(Some(1000)), include_initial: Some(Some(true)), ..given(&zone) }, format!("only {people} admitted dynamic residents are available to leave")),
    ];
    for (arguments, expected) in cases {
        let mut case = case("generated", "a_departure");
        case.unchanged(&expected, |case| case.departures(&event("departure", 0, json!({})), arguments));
    }
}

#[test]
fn a_first_departure_keeps_the_state_it_made_when_too_few_can_leave() {
    let mut case = case("generated", "a_first_departure");
    case.drop_namespace(DYNAMICS);
    // The native host puts the namespace the rule starts into `model_state`.
    case.host = load::following_host(&case.entry, &case.loaded);
    let zone = case.zone();
    case.says("only 0 admitted dynamic residents are available to leave", |case| {
        case.departures(&event("departure", 0, json!({})), DepartureGiven { zone: Ok(Some(text(&zone))), count: Ok(Some(1)), include_initial: None })
    });
    assert!(case.edit(|world| world.slots.get(DYNAMICS).unwrap().try_borrow().unwrap().row.is_some()));
}

// ---- apply_population_presence_command ------------------------------------------------------

#[test]
fn a_command_outside_its_contract_or_minute_says_the_rule_s_text() {
    let mut case = case("generated", "a_command_outside_its_contract_or_minute");
    case.command_says("population command minute must match the current event boundary", 1, Some(2), "transfer", "transfer");
    case.command_says("expected return minute precedes the departure event", 5, None, "temporary_exit", "exit_returning_early");
    case.drop_namespace(LIFECYCLE);
    case.command_says("population lifecycle contract is not installed", 0, None, "transfer", "transfer");
}

#[test]
fn an_exit_or_transfer_of_someone_absent_says_the_rule_s_text() {
    let mut case = case("generated", "an_exit_or_transfer_of_someone_absent");
    case.command_says("temporary exit requires an admitted person", 0, None, "temporary_exit", "exit_nobody");
    case.command_says("transfer requires an admitted person", 0, None, "transfer", "transfer_nobody");
}

#[test]
fn a_return_the_rule_refuses_says_its_text() {
    let mut case = case("generated", "a_return");
    let person = case.person();
    case.command_says("return requires a retained absent person", 0, None, "return", "return_nobody");
    case.command_says("return requires a retained absent person", 0, None, "return", "return");
    case.set_minute(1);
    case.command(&event("command-exit", 1, json!({})), "transfer").unwrap();
    assert!(case.edit(|world| world.rows().unwrap().agents.get(&person).is_none()));
    case.command_says("return requires a valid destination zone", 2, None, "return", "return_nowhere");
    case.command_says("return requires a valid destination zone", 2, None, "return", "return");
}

#[test]
fn a_relocation_the_rule_refuses_says_its_text() {
    let mut case = case("generated", "a_relocation");
    let there = case.there();
    let invalid = "relocation requires an admitted person and valid destination";
    case.command_says(invalid, 0, None, "relocate", "relocate_nobody");
    case.command_says(invalid, 0, None, "relocate", "relocate_nowhere");
    case.set_capacity(&there, Some(0));
    case.command_says("relocation destination has no available capacity", 0, None, "relocate", "relocate");
    case.set_capacity(&there, None);
    case.edit(|world| world.scalars.get_mut().duration_minutes = Fit::Ok(2));
    case.command_says("relocation route would finish after the scenario boundary", 1, None, "relocate", "relocate");
}

/// `_apply_presence_action(world, candidate, ...)`: the command on a copy of the world in which the
/// person is queued (a queued person is in a facility's queue in a whole world; the check reads the
/// activity), whose plan reads the world too.
#[test]
fn a_relocation_of_someone_busy_says_the_rule_s_text() {
    let mut case = case("generated", "a_relocation_of_someone_busy");
    let person = case.person();
    let candidate = texts::cell(&case.entry);
    candidate.with(|world| world.rows_mut().unwrap().agents.get_mut(&person).unwrap().activity = Activity::Queued).unwrap();
    let mut candidate_host = load::host(&case.entry);
    let dumped = load::v(&inputs()["commands"]["relocate"]);
    let ids = lifecycle::selected_ids(&dumped).unwrap().unwrap();
    let command = Command::of_dumped(&dumped, ids).unwrap();
    let row = texts::row(&event("command-relocate", 0, json!({})));
    case.unchanged("relocation requires a person who is not in an active task", |case| {
        let read = Read::read(&candidate, &mut candidate_host, true)?;
        let effects = kit::holding(&case.loaded.cell, &mut case.host)?.held("effect_program_v1", OTHER_PROGRAM);
        let reads = case.loaded.cell.with(|world| lifecycle::world_reads(world, &effects, &Logged::new(), &row, &command))?;
        read.run::<_, Error, _>(&candidate, |world, inputs| lifecycle::presence_action(world, inputs, &row, &command, &reads))
    });
}

#[test]
fn a_relocation_without_a_route_says_the_rule_s_text() {
    let mut case = case("generated", "a_relocation_without_a_route");
    let (here, there) = (case.agent_zone(&case.person()), case.other_zone());
    case.set_capacity(&there, None);
    case.edit(|world| {
        let mut rows = world.rows_mut().unwrap();
        let edges: Vec<Arc<str>> = rows.zone_edges.iter().filter(|(_, edge)| *edge.from_zone_id == *there || *edge.to_zone_id == *there).map(|(id, _)| id.clone()).collect();
        for edge in edges {
            assert!(rows.zone_edges.remove_row(&edge));
        }
    });
    assert_ne!(&*here, there.as_str());
    case.command_says("no accessible, physically clear relocation route is available", 0, None, "relocate", "relocate_other");
}

#[test]
fn a_reunion_the_rule_refuses_says_its_text() {
    let mut case = case("generated", "a_reunion");
    case.command_says("reunion requires every person to be admitted", 0, None, "reunite", "reunite_nobody");
    let mut people: Vec<String> = case.entry["world"]["agents"].as_array().unwrap().iter().map(|agent| agent["profile"]["id"].as_str().unwrap().to_owned()).collect();
    people.sort();
    let first_zone = case.agent_zone(&people[0]);
    let elsewhere = case.sorted("zones").into_iter().find(|zone| zone.as_str() != &*first_zone).unwrap();
    case.edit(|world| world.rows_mut().unwrap().agents.get_mut(&people[1]).unwrap().zone_id = text(&elsewhere));
    case.command_says("reunion requires people to be physically co-located", 0, None, "reunite", "reunite");
}

// ---- complete_presence_relocation -----------------------------------------------------------

#[test]
fn a_relocation_completion_without_its_route_says_the_rule_s_text() {
    let mut case = case("generated", "a_relocation_completion_without_its_route");
    let arrival = event_of("relocation-arrival", 0, "agent_travel_complete", json!({"agent_id": case.person(), "purpose": "presence_relocation"}));
    case.unchanged("relocation completion does not match its pending route", |case| lifecycle::complete_relocation_through(&case.loaded.cell, &mut case.host, &texts::row(&arrival)));
}

// ---- EffectProgramPopulationHandler ---------------------------------------------------------

#[test]
fn a_primitive_s_argument_the_rule_turns_into_its_text() {
    let mut case = case("arrival_program", "an_arrival");
    let program = program_event(&case.entry, "add_evacuees");
    case.handler_says(&changed(&program, json!({"admission_zone_ids": 5}), json!({})), "EventApplicationError", "'int' object is not iterable");
    case.handler_says(&changed(&program, json!({"person_context_overrides": null}), json!({})), "EventApplicationError", "'NoneType' object is not iterable");
    case.handler_says(&changed(&program, json!({"household_size": null}), json!({})), "EventApplicationError", "population arrival household_size is outside its supported range");
}

/// A list is no Mobility: the enum raised ValueError, which the rule worded.
#[test]
fn a_mobility_of_another_type_is_invalid_as_the_enum_says() {
    let mut case = case("arrival_program", "an_arrival");
    let program = program_event(&case.entry, "add_evacuees");
    case.handler_says(&changed(&program, json!({"mobility": [1]}), json!({})), "EventApplicationError", "population arrival mobility is invalid");
}

#[test]
fn a_departure_primitive_the_rule_refuses_says_its_text() {
    let mut case = case("departure_program", "a_departure");
    let program = program_event(&case.entry, "depart_evacuees");
    case.handler_says(&changed(&program, json!({"include_initial": null}), json!({})), "EventApplicationError", "population departure include_initial must be boolean");
    case.handler_says(&program, "EventApplicationError", "only 0 admitted dynamic residents are available to leave");
}
