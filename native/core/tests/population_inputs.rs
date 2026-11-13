//! Lane P9-3a: what the population rules read through the host (`population::Read`,
//! `population_lifecycle::Read`), the arguments a population primitive's payload gives them with
//! Python's semantics, and a presence command's contract and `selected_ids` (policy E15). Before,
//! the PyO3 crate read these (`PopulationIn`, `LifecycleIn`, the handler's `tuple(payload.get(...))`);
//! the texts are the same, and the gate and the offline tests run them on every Run.

mod support;

use std::sync::Arc;

use indexmap::IndexMap;
use sheltersim_core::cell::Shard;
use sheltersim_core::config::geometry::FacilityGeometry;
use sheltersim_core::config::{Fault, FaultClass, Section, SectionId};
use sheltersim_core::entities::{AnyTable, Entities, ZoneRow};
use sheltersim_core::error::Error;
use sheltersim_core::events::log::EventLog;
use sheltersim_core::events::queue::Queue;
use sheltersim_core::events::{EventKind, EventRow};
use sheltersim_core::host::{Contract, Hold, Holding, Key, HostFailure, TABLE_NOT_PLAIN};
use sheltersim_core::row::ValueRow;
use sheltersim_core::rules::handlers;
use sheltersim_core::rules::kit::Held;
use sheltersim_core::rules::overlay::PLAN_UNREAD;
use sheltersim_core::rules::population::{self, Alternates, DepartureGiven, Given, Language, Overrides, Read, DYNAMICS, PRESENCE};
use sheltersim_core::rules::population_lifecycle::{self as lifecycle, Command};
use sheltersim_core::value::V;
use sheltersim_core::world::{Fit, Place, World, ID_COUNTERS};
use support::{Call, TestHost};

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

fn s(value: &str) -> V {
    V::Str(text(value))
}

fn map(entries: Vec<(&str, V)>) -> V {
    V::Map(entries.into_iter().map(|(key, value)| (text(key), value)).collect())
}

fn failed(text: &str) -> Error {
    Error::failed(text)
}

fn add_value(world: &mut World, name: &str, place: Place, value: V) {
    let index = world.slots.add(text(name), place).unwrap();
    world.slots.at(index).unwrap().try_borrow_mut().unwrap().row = Some(Box::new(ValueRow(value)));
}

/// A world at minute 7 with zone z0, its queue and log, and the two population namespaces.
fn world() -> World {
    let mut world = World::default();
    {
        let scalars = world.scalars.get_mut();
        scalars.duration_minutes = Fit::Ok(600);
        scalars.minute_index = Fit::Ok(7);
        scalars.schema_version = Fit::Ok(6);
    }
    let mut entities = Entities::default();
    let slot = entities.zones.reserve_slot();
    entities.zones.fill_slot(slot, ZoneRow { id: text("z0"), name: text("z0"), capacity: Some(2), display_x: None, display_y: None });
    assert_eq!(entities.zones.bind(text("z0"), slot), None);
    *world.entities.get_mut() = entities;
    add_value(&mut world, ID_COUNTERS, Place::Field, map(vec![("event", V::Int(10))]));
    world.hold_queue(Arc::new(Queue::new()));
    world.hold_log(Arc::new(EventLog::new()));
    add_value(&mut world, DYNAMICS, Place::Namespace, map(vec![]));
    add_value(&mut world, PRESENCE, Place::Namespace, map(vec![]));
    world
}

fn event(payload: V) -> EventRow {
    let V::Map(payload) = payload else { panic!("a payload is a map") };
    EventRow::new(text("e-1"), EventKind::Intervention, 7, 0, 3, text("effect_program_population"), payload, Some(text("SRC")), None, IndexMap::new()).unwrap()
}

/// A holding whose `model_state` holds these keys.
fn holding(keys: Vec<(&str, Hold)>) -> Arc<Holding> {
    let mut holding = Holding::default();
    for (name, hold) in keys {
        holding.keys.insert(text(name), Key::new(hold, false));
    }
    Arc::new(holding)
}

// ---- What the world holds, the floor plan, the routes ----------------------------------------------

#[test]
fn the_inputs_say_what_the_world_holds_with_each_namespace_s_text() {
    let cell = Shard::new(world());
    let mut host = TestHost::default();
    let read = Read::read(&cell, &mut host, false, false).unwrap();
    let namespaces = &read.inputs.namespaces;
    assert_eq!((namespaces.dynamics.clone(), namespaces.presence.clone(), namespaces.movement.clone()), (Held::Held, Held::Held, Held::Absent));
    // Without movement or the overlay the floor plan is not read.
    assert!(!host.calls.contains(&Call::FloorPlan));
    assert!(read.inputs.geo.is_none() && read.inputs.routes.is_none());

    let mut host = TestHost { holding: Some(holding(vec![(DYNAMICS, Hold::Other), ("population_presence_v1", Hold::Held)])), ..TestHost::default() };
    let read = Read::read(&cell, &mut host, false, false).unwrap();
    assert_eq!(read.inputs.namespaces.dynamics, Held::Other(population::OTHER_DYNAMICS));
    assert_eq!(population::OTHER_DYNAMICS, "the world holds another population dynamics state");
    assert_eq!(read.inputs.namespaces.effect_program, Held::Absent);
    let mut host = TestHost { holding: Some(holding(vec![("effect_program_v1", Hold::Other)])), ..TestHost::default() };
    let read = Read::read(&cell, &mut host, false, false).unwrap();
    assert_eq!(read.inputs.namespaces.effect_program, Held::Other("the world holds another effect program state"));
    assert_eq!(read.inputs.namespaces.dynamics, Held::Absent);
}

#[test]
fn the_floor_plan_s_refusal_is_raised_where_a_rule_reads_the_plan() {
    let cell = Shard::new(world());
    let mut host = TestHost::default();
    let read = Read::read(&cell, &mut host, false, true).unwrap();
    assert!(host.calls.contains(&Call::FloorPlan));
    assert!(read.inputs.geo.is_none());
    let unread: Result<(), Error> = Err(Error::unsupported(PLAN_UNREAD));
    assert_eq!(read.finish(unread), Err(Error::Unsupported(sheltersim_core::host::NO_PLAN.into())));
    let other: Result<(), Error> = Err(failed("another"));
    assert_eq!(read.finish(other), Err(failed("another")));
}

fn with_geometry(geometry: Section<FacilityGeometry>) -> TestHost {
    let geometry = std::sync::Mutex::new(Some(geometry));
    TestHost {
        sections: vec![(
            SectionId::FacilityGeometry,
            Box::new(move |configs: &sheltersim_core::config::Configs| {
                let section = geometry.lock().unwrap().take().unwrap();
                let _ = configs.facility_geometry.get_or_parse(|| section);
            }),
        )],
        ..TestHost::default()
    }
}

fn geometry(routes: Result<Arc<[sheltersim_core::config::geometry::ApproachRoute]>, Fault>) -> Section<FacilityGeometry> {
    let configs = sheltersim_core::config::Configs::new(None);
    support::fill_section(&configs, SectionId::FacilityGeometry);
    let Some(Section::Ok(found)) = configs.facility_geometry.get() else { panic!("the test host fills the geometry") };
    Section::Ok(Arc::new(FacilityGeometry { plan: found.plan.clone(), approach_routes: routes, independent_queues: found.independent_queues.clone(), document: found.document.clone() }))
}

#[test]
fn the_approach_routes_are_the_section_s_its_refusal_or_what_the_parse_raises_again() {
    let cell = Shard::new(world());
    let held = Holding::of_world(&cell.try_borrow().unwrap()).unwrap();
    let routes = |host: &mut TestHost| population::approach_routes(&cell, host, &held).unwrap();

    let found = routes(&mut TestHost::default()).unwrap();
    assert!(found.is_empty());

    let refused = routes(&mut with_geometry(geometry(Err(Fault::refused("a route is not a mapping"))))).unwrap_err();
    assert!(!refused.caught);
    assert_eq!(refused.refusal, Error::Unsupported("a route is not a mapping".into()));

    let raised = routes(&mut with_geometry(geometry(Err(Fault::new(FaultClass::Value, "bad route"))))).unwrap_err();
    assert!(!raised.caught);
    assert_eq!(raised.refusal, Error::Unsupported("the Python rule's parse raises".into()));

    // No config: the parse of `{}` raises again; a host without Python raises nothing, which is the
    // corrupt state's error, a ValueError the arrivals' `try` catches.
    let absent = routes(&mut with_geometry(Section::Absent)).unwrap_err();
    assert!(absent.caught);
    assert_eq!(absent.refusal, Error::Unsupported("the plan read: model_configs: corrupt state: a config fault did not reproduce".into()));
}

#[test]
fn a_plan_s_read_error_and_a_write_s_refusal_say_python_s_texts() {
    assert_eq!(population::plan_read_of(&Error::unsupported("no")), Error::Unsupported("no".into()));
    assert_eq!(population::plan_read_of(&Error::Key("k".to_owned())), Error::Unsupported("the plan read: k".into()));
    assert_eq!(population::planning(failed("text")), failed("text"));
    assert_eq!(population::planning(Error::MarkedValue("v".to_owned())), Error::MarkedValue("v".to_owned()));
    assert_eq!(population::planning(Error::Value("v".to_owned())), Error::Unsupported("the plan read: v".into()));
    assert_eq!(
        population::writing(Error::unsupported("late")),
        Error::Runtime("a kernel refused after it began writing: Unsupported: late".to_owned())
    );
    assert_eq!(population::writing(failed("text")), failed("text"));
}

// ---- A primitive's arguments --------------------------------------------------------------------------

fn add_payload(extra: Vec<(&str, V)>) -> V {
    let mut entries = vec![("zone_id", s("z0")), ("count", V::Int(2)), ("household_size", V::Int(1)), ("mobility", s("assisted")), ("language", s("ja"))];
    entries.extend(extra);
    map(entries)
}

#[test]
fn an_add_primitive_s_arguments_are_read_as_the_handler_s_call_reads_them() {
    let given = Given::of_payload(&add_payload(vec![])).unwrap();
    assert_eq!(given.zone, Ok(Some(text("z0"))));
    assert_eq!(given.count, Ok(Some(2)));
    assert!(matches!(given.alternates, Ok(Alternates::Zones(ref zones)) if zones.is_empty()));
    assert!(matches!(given.overrides, Some(Ok(Overrides::Items(ref items))) if items.is_empty()));
    assert!(given.route_id.is_none());
    assert_eq!(given.mobility, Some(Ok(Some("assisted"))));
    assert!(matches!(given.language, Some(Ok(Language::Valid(ref language))) if &**language == "ja"));

    // `tuple()` of a str is its characters, of a mapping its keys; anything else is not iterable.
    let given = Given::of_payload(&add_payload(vec![("admission_zone_ids", s("ab")), ("person_context_overrides", map(vec![("k", V::Int(1))]))])).unwrap();
    assert!(matches!(given.alternates, Ok(Alternates::Zones(ref zones)) if zones.iter().map(|zone| &**zone).collect::<Vec<_>>() == ["a", "b"]));
    assert!(matches!(given.overrides, Some(Ok(Overrides::Items(ref items))) if items == &vec![s("k")]));
    let given = Given::of_payload(&add_payload(vec![("admission_zone_ids", V::List(vec![s("z0"), V::Int(1)]))])).unwrap();
    assert!(matches!(given.alternates, Ok(Alternates::Invalid)));
    for (value, name) in [(V::Int(3), "int"), (V::Null, "NoneType"), (V::Bool(true), "bool"), (V::Float(1.5), "float")] {
        let raised = Given::of_payload(&add_payload(vec![("admission_zone_ids", value.clone())])).err();
        assert_eq!(raised, Some(failed(&format!("'{name}' object is not iterable"))));
        let raised = Given::of_payload(&add_payload(vec![("person_context_overrides", value)])).err();
        assert_eq!(raised, Some(failed(&format!("'{name}' object is not iterable"))));
    }

    // The values each read refuses, raised where the rule reads them.
    let given = Given::of_payload(&add_payload(vec![("approach_route_id", V::Int(4))])).unwrap();
    assert_eq!(given.route_id, Some(None));
    let odd = map(vec![
        ("zone_id", V::List(vec![map(vec![])])),
        ("count", V::Big("18446744073709551616".into())),
        ("household_size", V::Bool(true)),
        ("mobility", map(vec![])),
        ("language", s("not an id!")),
    ]);
    let given = Given::of_payload(&odd).unwrap();
    assert_eq!(given.zone, Err(Error::Unsupported("the plan read: TypeError: unhashable type: 'mappingproxy'".into())));
    assert_eq!(given.count, Err(Error::Unsupported("an int is too large".into())));
    assert_eq!(given.household_size, Some(Ok(None)));
    assert_eq!(given.mobility, Some(Err(Error::Unsupported("a mobility of another type".into()))));
    assert!(matches!(given.language, Some(Ok(Language::Ask(ref value))) if value == &s("not an id!")));
    let given = Given::of_payload(&map(vec![("zone_id", V::Int(1)), ("count", s("2")), ("mobility", s("flying"))])).unwrap();
    assert_eq!((given.zone, given.count, given.mobility), (Ok(None), Ok(None), Some(Ok(None))));

    let depart = DepartureGiven::of_payload(&map(vec![("zone_id", s("z0")), ("count", V::Int(1))]));
    assert_eq!(depart.include_initial, Some(Some(false)));
    let depart = DepartureGiven::of_payload(&map(vec![("zone_id", s("z0")), ("count", V::Int(1)), ("include_initial", V::Int(1))]));
    assert_eq!(depart.include_initial, Some(None));
}

#[test]
fn a_primitive_of_another_operation_fails_with_the_handler_s_text() {
    let cell = Shard::new(world());
    let mut host = TestHost::default();
    let row = event(map(vec![("op", s("teleport"))]));
    assert_eq!(handlers::population_event(&cell, &mut host, &row), Err(failed("population primitive operation is invalid")));
}

// ---- A presence command's contract ---------------------------------------------------------------------

fn command(entries: Vec<(&str, V)>) -> V {
    let mut base = vec![
        ("action", s("temporary_exit")),
        ("agent_id", V::Null),
        ("agent_ids", V::List(vec![])),
        ("zone_id", V::Null),
        ("household_id", V::Null),
        ("expected_return_minute", V::Null),
        ("source_ids", V::List(vec![])),
    ];
    for (key, value) in entries {
        match base.iter_mut().find(|(name, _)| *name == key) {
            Some(found) => found.1 = value,
            None => base.push((key, value)),
        }
    }
    map(base)
}

#[test]
fn selected_ids_are_sorted_once_each_and_their_count_fits_the_action() {
    let ids = |entries| lifecycle::selected_ids(&command(entries)).unwrap();
    assert_eq!(ids(vec![("agent_id", s("b")), ("agent_ids", V::List(vec![s("b")]))]), Ok(vec![text("b")]));
    assert_eq!(ids(vec![]), Err("population command requires at least one agent id".to_owned()));
    assert_eq!(ids(vec![("agent_ids", V::List(vec![s("b"), s("a")]))]), Err("temporary_exit requires exactly one agent id".to_owned()));
    assert_eq!(ids(vec![("action", s("reunite")), ("agent_id", s("a"))]), Err("reunite requires at least two agent ids".to_owned()));
    assert_eq!(
        ids(vec![("action", s("reunite")), ("agent_id", s("c")), ("agent_ids", V::List(vec![s("b"), s("a"), s("c")]))]),
        Ok(vec![text("a"), text("b"), text("c")])
    );
    let parsed = Command::of_dumped(&command(vec![("expected_return_minute", V::Int(30)), ("source_ids", V::List(vec![s("SRC-1")]))]), vec![text("a")]).unwrap();
    assert_eq!((parsed.expected_return, parsed.source_ids.clone(), &*parsed.action), (Some(30), vec![text("SRC-1")], "temporary_exit"));
    assert_eq!(Command::of_dumped(&command(vec![("expected_return_minute", s("30"))]), vec![]).err(), Some(Error::Unsupported("a value is not an int".into())));
}

#[test]
fn a_command_the_contract_or_its_ids_turn_down_is_invalid_with_the_rule_s_text() {
    let cell = Shard::new(world());
    let row = event(map(vec![]));
    let run = |host: &mut TestHost, given: V| lifecycle::presence_command(&cell, host, &row, &given);

    // The contract's TypeError or ValueError is the rule's text; another error is the plan's read.
    let mut host = TestHost { contracts: Some(Box::new(|_, _| Err(Error::Value("1 validation error".to_owned())))), ..TestHost::default() };
    assert_eq!(run(&mut host, map(vec![])), Err(failed("population command is invalid: 1 validation error")));
    assert_eq!(host.calls.first(), Some(&Call::Contract(Contract::PresenceCommand, map(vec![]))));
    let mut host = TestHost { contracts: Some(Box::new(|_, _| Err(Error::Type("not a dict".to_owned())))), ..TestHost::default() };
    assert_eq!(run(&mut host, s("x")), Err(failed("population command is invalid: not a dict")));
    let mut host = TestHost { contracts: Some(Box::new(|_, _| Err(Error::Key("k".to_owned())))), ..TestHost::default() };
    assert_eq!(run(&mut host, map(vec![])), Err(Error::Unsupported("the plan read: k".into())));
    let mut host = TestHost { contracts: Some(Box::new(|_, _| Err(Error::unsupported("refused")))), ..TestHost::default() };
    assert_eq!(run(&mut host, map(vec![])), Err(Error::Unsupported("refused".into())));

    // `selected_ids`, then whether the lifecycle is installed.
    let mut host = TestHost::default();
    assert_eq!(run(&mut host, command(vec![])), Err(failed("population command is invalid: population command requires at least one agent id")));
    assert_eq!(run(&mut host, command(vec![("agent_id", s("a1"))])), Err(failed("population lifecycle contract is not installed")));
    let mut host = TestHost { holding: Some(Arc::new(Holding { table: false, ..Holding::default() })), ..TestHost::default() };
    let refused = run(&mut host, command(vec![("agent_id", s("a1"))])).unwrap_err();
    assert_eq!(refused, Error::Unsupported(TABLE_NOT_PLAIN.into()));
    assert_eq!(refused.text(), "a value is not a plain dict");
}

#[test]
fn a_relocation_reads_the_lifecycle_and_the_configs_it_times_a_route_with() {
    let cell = Shard::new(world());
    let mut host = TestHost::default();
    let read = lifecycle::Read::read(&cell, &mut host, true).unwrap();
    assert_eq!(read.lifecycle, Held::Absent);
    let relocation = read.relocation.as_ref().unwrap();
    assert_eq!(relocation.transit_capacities, Ok(false));
    assert_eq!(relocation.timing, Ok(None));
    assert!(host.calls.contains(&Call::FloorPlan));

    // A refused section is its refusal where the relocation reads it.
    let mut host = TestHost {
        sections: vec![(
            SectionId::TransitCapacities,
            Box::new(|configs: &sheltersim_core::config::Configs| {
                let _ = configs.transit_capacities.get_or_parse(|| Section::Fault(Fault::refused("capacities are not a mapping")));
            }),
        )],
        holding: Some(holding(vec![(lifecycle::KEY, Hold::Other)])),
        ..TestHost::default()
    };
    let read = lifecycle::Read::read(&cell, &mut host, true).unwrap();
    assert_eq!(read.lifecycle, Held::Other(lifecycle::OTHER_LIFECYCLE));
    assert_eq!(read.relocation.as_ref().unwrap().transit_capacities, Err(Error::Unsupported("capacities are not a mapping".into())));
    let read = lifecycle::Read::read(&cell, &mut TestHost::default(), false).unwrap();
    assert!(read.relocation.is_none());
}
