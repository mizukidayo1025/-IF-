//! Lane c5: the rules of `domain/population.py` (`rules::population`) on a world built here with no
//! Python. The PyO3 crate's kernels of the same names are glue over these, so the gate and the
//! offline tests run them on every Run (and `tests/integration/test_population_agents.py` compares
//! `build_agent` with the Python constructors); these say what each reads, decides and writes,
//! and what it asks of the host (the input contracts, policy E15).

mod support;

use std::sync::Arc;

use indexmap::IndexMap;
use sheltersim_core::cell::{Shard, ShardRef};
use sheltersim_core::entities::{Activity, AgentRow, AnyTable, Entities, Mobility, NeedRow, ProfileRow, Table, ZoneRow};
use sheltersim_core::error::Error;
use sheltersim_core::events::log::EventLog;
use sheltersim_core::events::queue::Queue;
use sheltersim_core::events::{EventKind, EventRow};
use sheltersim_core::host::Contract;
use sheltersim_core::row::ValueRow;
use sheltersim_core::rules::kit::{Events, Held};
use sheltersim_core::rules::population::{
    self, Alternates, ArrivalArgs, Built, DepartureArgs, Departures, Inputs, Language, Namespaces, Overrides, Person, DYNAMICS, PRESENCE,
};
use sheltersim_core::state::metric_state::MetricRow;
use sheltersim_core::value::V;
use sheltersim_core::world::{Fit, Place, Seed, World, WorldCell, ID_COUNTERS, METRIC_STATE};
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

fn list(items: Vec<V>) -> V {
    V::List(items)
}

fn get<'a>(value: &'a V, path: &[&str]) -> &'a V {
    path.iter().fold(value, |node, key| match node {
        V::Map(map) => map.get(*key).unwrap_or_else(|| panic!("no {key}")),
        _ => panic!("not a map at {key}"),
    })
}

fn failed(text: &str) -> Error {
    Error::failed(text)
}

fn unsupported(reason: &str) -> Error {
    Error::Unsupported(reason.to_owned().into())
}

fn put<R: Clone>(table: &mut Table<R>, id: &str, row: R) {
    let slot = table.reserve_slot();
    table.fill_slot(slot, row);
    assert_eq!(table.bind(text(id), slot), None);
}

fn needs() -> NeedRow {
    NeedRow { toilet_urgency: 0.0, hunger: 0.0, fatigue: 0.0, hydration_need: 0.0 }
}

fn agent(id: &str, zone: &str, activity: Activity) -> AgentRow {
    AgentRow {
        profile: ProfileRow {
            id: text(id),
            group_id: None,
            initial_zone_id: text(zone),
            mobility: Mobility::Independent,
            support_need: 0.0,
            stress_sensitivity: 1.0,
            rng_key: text(id),
            initial_needs: needs(),
            provenance: V::Map(IndexMap::new()),
        },
        zone_id: text(zone),
        activity,
        target_id: None,
        needs: needs(),
        stress: 0.0,
        trust: 0.5,
        next_decision_at: 0,
        support_request_id: None,
        support_cooldown_until: 0,
    }
}

fn add_value(world: &mut World, name: &str, place: Place, value: V) {
    let index = world.slots.add(text(name), place).unwrap();
    world.slots.at(index).unwrap().try_borrow_mut().unwrap().row = Some(Box::new(ValueRow(value)));
}

fn dynamics_state(initial: Vec<&str>) -> V {
    map(vec![
        ("schema_version", V::Int(1)),
        ("policy_version", s(population::POLICY_VERSION)),
        ("source_id", s(population::SOURCE_ID)),
        ("initial_agent_ids", list(initial.into_iter().map(s).collect())),
        ("profile_traits", map(vec![])),
        ("profile_context", map(vec![])),
        ("pending_arrivals", list(vec![])),
        ("arrivals", list(vec![])),
        ("departures", list(vec![])),
    ])
}

fn presence_state() -> V {
    map(vec![("schema_version", V::Int(1)), ("people", map(vec![])), ("history", list(vec![]))])
}

/// A world of 600 minutes at minute 7, seed 2026, v6: a1 idle and a2 in service in z0 (2 places),
/// z1 without a capacity, its queue, log, event counter and metrics, `population_dynamics_v1`
/// (a1 and a2 initial) and the presence ledger.
fn world() -> World {
    let mut world = World::default();
    {
        let scalars = world.scalars.get_mut();
        scalars.duration_minutes = Fit::Ok(600);
        scalars.minute_index = Fit::Ok(7);
        scalars.schema_version = Fit::Ok(6);
        scalars.seed = Fit::Ok(Seed::from_text("2026").unwrap());
    }
    let mut entities = Entities::default();
    put(&mut entities.agents, "a1", agent("a1", "z0", Activity::Idle));
    put(&mut entities.agents, "a2", agent("a2", "z0", Activity::InService));
    for (id, capacity) in [("z0", Some(2)), ("z1", None)] {
        put(&mut entities.zones, id, ZoneRow { id: text(id), name: text(id), capacity, display_x: None, display_y: None });
    }
    *world.entities.get_mut() = entities;
    add_value(&mut world, ID_COUNTERS, Place::Field, map(vec![("event", V::Int(10))]));
    let index = world.slots.add(text(METRIC_STATE), Place::Field).unwrap();
    world.slots.at(index).unwrap().try_borrow_mut().unwrap().row = Some(Box::new(MetricRow::default()));
    world.hold_queue(Arc::new(Queue::new()));
    world.hold_log(Arc::new(EventLog::new()));
    add_value(&mut world, DYNAMICS, Place::Namespace, dynamics_state(vec!["a1", "a2"]));
    add_value(&mut world, PRESENCE, Place::Namespace, presence_state());
    world
}

fn namespaces(dynamics: Held) -> Namespaces {
    Namespaces {
        dynamics,
        presence: Held::Held,
        information: Held::Absent,
        body: Held::Absent,
        agent_dynamics: Held::Absent,
        movement: Held::Absent,
        overlay: Held::Absent,
        spaces: Held::Absent,
        social_network: Held::Absent,
        social_runtime: Held::Absent,
        resource_service: Held::Absent,
        effect_program: Held::Absent,
    }
}

fn inputs() -> Inputs {
    Inputs {
        namespaces: namespaces(Held::Held),
        schema: Ok(Arc::new(None)),
        policy: Ok(Arc::new("facility-awareness-v1")),
        awareness: Ok(Arc::new(None)),
        geo: None,
        routes: None,
    }
}

fn state(world: &World, name: &str) -> V {
    world.row::<ValueRow>(name).unwrap().0.clone()
}

fn arrival_event(id: &str) -> EventRow {
    EventRow::new(
        text(id),
        EventKind::Intervention,
        7,
        0,
        3,
        text("effect_program_population"),
        IndexMap::new(),
        Some(text("SRC-ARRIVAL")),
        None,
        IndexMap::new(),
    )
    .unwrap()
}

fn args(zone: &str, count: i64) -> ArrivalArgs {
    ArrivalArgs {
        zone: Ok(Some(text(zone))),
        alternates: Ok(Alternates::NotGiven),
        route_id: None,
        count: Ok(Some(count)),
        household_size: None,
        mobility: None,
        language: None,
        overrides: None,
        rooms: (false, Ok(false)),
        population: None,
    }
}

/// The world, borrowed for a rule that takes it as it is.
fn w(cell: &Shard<World>) -> ShardRef<'_, World> {
    cell.try_borrow().unwrap()
}

/// `plan_arrivals` and `write_arrivals` on the world, as the PyO3 crate runs them.
fn arrive(cell: &Shard<World>, host: &mut TestHost, inputs: &Inputs, event: &EventRow, args: ArrivalArgs) -> Result<i64, Error> {
    let plan = population::plan_arrivals(cell, host, inputs, event, args)?;
    assert!(plan.new_state.is_none() && plan.fails.is_none());
    cell.with(|world| population::write_arrivals(world, inputs, &plan)).unwrap()
}

fn agent_ids(world: &World) -> Vec<String> {
    world.rows().unwrap().agents.iter().map(|(id, _)| id.to_string()).collect()
}

fn metric(world: &World, name: &str) -> Option<f64> {
    world.metric_state().unwrap().get(name).copied()
}

// ---- The agent the constructors make -------------------------------------------------------------

fn profile(entries: Vec<(&str, V)>) -> V {
    let mut base = vec![
        ("id", s("arrival-1")),
        ("group_id", s("household-1")),
        ("initial_zone_id", s("z0")),
        ("mobility", s("assisted")),
        ("support_need", V::Float(0.25)),
        ("stress_sensitivity", V::Float(0.5)),
        ("rng_key", s("arrival:1")),
        ("initial_needs", map(vec![("toilet_urgency", V::Float(0.1)), ("hunger", V::Float(0.2)), ("fatigue", V::Float(0.3)), ("hydration_need", V::Float(0.4))])),
        ("provenance", map(vec![("source_id", s("SRC"))])),
    ];
    for (key, value) in entries {
        match base.iter_mut().find(|(name, _)| *name == key) {
            Some(entry) => entry.1 = value,
            None => base.push((key, value)),
        }
    }
    map(base)
}

fn without(value: V, key: &str) -> V {
    let V::Map(mut fields) = value else { panic!("a map") };
    fields.shift_remove(key);
    V::Map(fields)
}

fn row_of(built: Result<Built, Error>) -> AgentRow {
    match built {
        Ok(Built::Row(row)) => *row,
        _ => panic!("not a row"),
    }
}

fn raised(built: Result<Built, Error>) -> Error {
    match built {
        Ok(Built::Raises(error)) => error,
        _ => panic!("no ValueError"),
    }
}

#[test]
fn an_agent_is_what_the_constructors_make_of_its_profile() {
    let zone = text("z1");
    let agent = row_of(population::build_agent(&profile(vec![]), &zone, 9, 6));
    assert_eq!(&*agent.profile.id, "arrival-1");
    assert_eq!(agent.profile.group_id.as_deref(), Some("household-1"));
    assert_eq!(agent.profile.mobility, Mobility::Assisted);
    assert_eq!((agent.profile.support_need, agent.profile.stress_sensitivity), (0.25, 0.5));
    assert_eq!(agent.profile.provenance, map(vec![("source_id", s("SRC"))]));
    // `NeedState(**initial)` and `InitialNeedProfile(**initial)` hold the same levels.
    for needs in [&agent.needs, &agent.profile.initial_needs] {
        assert_eq!((needs.toilet_urgency, needs.hunger, needs.fatigue, needs.hydration_need), (0.1, 0.2, 0.3, 0.4));
    }
    assert_eq!(&*agent.zone_id, "z1");
    assert_eq!((agent.activity, agent.target_id.clone(), agent.stress, agent.trust), (Activity::Idle, None, 0.0, 0.5));
    assert_eq!((agent.next_decision_at, agent.support_request_id.clone(), agent.support_cooldown_until), (9, None, 0));
    // A level not given is the default; v5 has no hydration: 0.0, whatever the profile says.
    let partial = profile(vec![("initial_needs", map(vec![("hunger", V::Float(1.0))]))]);
    let agent = row_of(population::build_agent(&partial, &zone, 0, 6));
    assert_eq!((agent.needs.toilet_urgency, agent.needs.hunger, agent.needs.hydration_need), (0.0, 1.0, 0.0));
    let agent = row_of(population::build_agent(&profile(vec![]), &zone, 0, 5));
    assert_eq!(agent.needs.hydration_need, 0.0);
    let agent = row_of(population::build_agent(&profile(vec![("group_id", V::Null)]), &zone, 0, 6));
    assert_eq!(agent.profile.group_id, None);
}

#[test]
fn the_constructors_value_errors_come_in_their_order() {
    let zone = text("z0");
    let mobility = |value: &str| raised(population::build_agent(&profile(vec![("mobility", s(value))]), &zone, 0, 6));
    assert_eq!(mobility("flying"), Error::Value("'flying' is not a valid Mobility".to_owned()));
    assert_eq!(mobility("it's"), Error::Value("\"it's\" is not a valid Mobility".to_owned()));
    let level = |name: &str, value: V| {
        let needs = map(vec![(name, value)]);
        raised(population::build_agent(&profile(vec![("initial_needs", needs)]), &zone, 0, 6))
    };
    for value in [V::Float(1.5), V::Float(-0.1), V::Float(f64::NAN), V::Float(f64::INFINITY), V::Bool(true), V::Int(2), s("0.5"), V::Null] {
        assert_eq!(level("fatigue", value), Error::Value("fatigue must be finite and between 0 and 1".to_owned()));
    }
    // The first level the check reads raises; the mobility comes before the levels.
    let needs = map(vec![("hydration_need", V::Float(2.0)), ("hunger", V::Float(2.0))]);
    assert_eq!(raised(population::build_agent(&profile(vec![("initial_needs", needs.clone())]), &zone, 0, 6)), Error::Value("hunger must be finite and between 0 and 1".to_owned()));
    let both = profile(vec![("initial_needs", needs), ("mobility", s("x"))]);
    assert_eq!(raised(population::build_agent(&both, &zone, 0, 6)), Error::Value("'x' is not a valid Mobility".to_owned()));
    assert_eq!(raised(population::build_agent(&profile(vec![]), &zone, -1, 6)), Error::Value("next_decision_at must be a non-negative integer".to_owned()));
}

#[test]
fn what_no_row_holds_or_another_error_is_refused() {
    let zone = text("z0");
    let refused = |profile: V| population::build_agent(&profile, &zone, 0, 6).err().unwrap();
    assert_eq!(refused(without(profile(vec![]), "rng_key")), unsupported("a saved profile lacks a field"));
    assert_eq!(refused(profile(vec![("initial_needs", map(vec![("thirst", V::Float(0.1))]))])), unsupported("saved initial needs have another key"));
    // `InitialNeedProfile` takes 0 and 1; the store would then taint.
    assert_eq!(refused(profile(vec![("initial_needs", map(vec![("hunger", V::Int(1))]))])), unsupported("a saved need is an int"));
    assert_eq!(refused(profile(vec![("support_need", V::Int(0))])), unsupported("a saved profile has a value no row holds"));
    assert_eq!(refused(profile(vec![("group_id", V::Int(3))])), unsupported("a saved group is not a str"));
    assert_eq!(refused(profile(vec![("mobility", V::Int(3))])), unsupported("a saved mobility is not a str"));
    // A ValueError the constructors raise first comes before what the store would refuse.
    let both = profile(vec![("support_need", V::Int(0)), ("mobility", s("x"))]);
    assert!(matches!(population::build_agent(&both, &zone, 0, 6), Ok(Built::Raises(_))));
}

// ---- Arrivals ------------------------------------------------------------------------------------

#[test]
fn arrivals_with_room_are_admitted_as_rows_and_the_others_wait() {
    let cell = Shard::new(world());
    w(&cell).rows_mut().unwrap().agents.remove_row("a2");
    let mut host = TestHost::default();
    let event = arrival_event("arrive-1");
    assert_eq!(arrive(&cell, &mut host, &inputs(), &event, args("z0", 3)), Ok(1));
    let world = w(&cell);
    assert_eq!(host.calls, Vec::<Call>::new());
    let ids = agent_ids(&world);
    assert_eq!(ids.len(), 2);
    assert_eq!(ids[0], "a1");
    // The first generated person, `arrival-` and 28 hex of SHA-256 of the seed, event and index.
    let first = format!("arrival-{}", &population::sha_hex("2026\0arrive-1\00")[..28]);
    assert_eq!(ids[1], first);
    let admitted = world.rows().unwrap().agents.get(&*first).unwrap().clone();
    assert_eq!((&*admitted.zone_id, admitted.next_decision_at, admitted.activity), ("z0", 7, Activity::Idle));
    assert_eq!(admitted.profile.provenance, map(vec![
        ("source_type", s("assumption")),
        ("source_id", s(population::SOURCE_ID)),
        ("event_id", s("arrive-1")),
        ("seed", V::Int(2026)),
    ]));
    let dynamics = state(&world, DYNAMICS);
    let V::List(pending) = get(&dynamics, &["pending_arrivals"]) else { panic!("a list") };
    assert_eq!(pending.len(), 2);
    let V::List(arrivals) = get(&dynamics, &["arrivals"]) else { panic!("a list") };
    assert_eq!(get(&arrivals[0], &["agent_id"]), &s(&first));
    assert_eq!(arrivals[1], map(vec![
        ("event_id", s("arrive-1")),
        ("zone_id", s("z0")),
        ("requested_count", V::Int(3)),
        ("admitted_count", V::Int(1)),
        ("waiting_count", V::Int(2)),
        ("minute", V::Int(7)),
    ]));
    let V::Map(traits) = get(&dynamics, &["profile_traits"]) else { panic!("a map") };
    assert_eq!(traits.len(), 3);
    let V::Map(contexts) = get(&dynamics, &["profile_context"]) else { panic!("a map") };
    assert_eq!(contexts.len(), 3);
    assert_eq!((metric(&world, "population_arrival_count"), metric(&world, "population_waiting_count")), (Some(1.0), Some(2.0)));
    // The ledger: one arrived, two waiting, each with its transition.
    let presence = state(&world, PRESENCE);
    let person = Person::parse(get(&presence, &["people", &first])).unwrap();
    assert_eq!((&*person.status, person.transition_zone_id.as_deref()), ("admitted", Some("z0")));
    let V::List(history) = get(&presence, &["history"]) else { panic!("a list") };
    let actions: Vec<&V> = history.iter().map(|item| get(item, &["action"])).collect();
    assert_eq!(actions, [&s("arrived"), &s("waiting"), &s("waiting")]);
    assert_eq!(world.id_counter("event").unwrap(), Some(10));
}

#[test]
fn an_arrival_the_rule_refuses_says_its_text_in_its_order() {
    let cell = Shard::new(world());
    let event = arrival_event("arrive-1");
    let plan = |args: ArrivalArgs, host: &mut TestHost| population::plan_arrivals(&cell, host, &inputs(), &event, args).err();
    let mut host = TestHost::default();
    assert_eq!(plan(args("nowhere", 1), &mut host), Some(failed("population arrival references an unknown zone")));
    let mut bad = args("nowhere", 0);
    bad.zone = Ok(None);
    assert_eq!(plan(bad, &mut host), Some(failed("population arrival references an unknown zone")));
    let mut alternates = args("z0", 1);
    alternates.alternates = Ok(Alternates::Zones(vec![text("z0")]));
    assert_eq!(plan(alternates, &mut host), Some(failed("population arrival alternate admission zones are invalid")));
    let mut rooms = args("z0", 1);
    rooms.alternates = Ok(Alternates::Zones(vec![text("z1")]));
    assert_eq!(plan(rooms, &mut host), Some(failed("alternate room admission requires geometry_overlay schema_version=3 or 4")));
    assert_eq!(plan(args("z0", 1001), &mut host), Some(failed("population arrival count is outside its supported range")));
    let mut household = args("z0", 1);
    household.household_size = Some(Ok(Some(13)));
    assert_eq!(plan(household, &mut host), Some(failed("population arrival household_size is outside its supported range")));
    let mut mobility = args("z0", 1);
    mobility.mobility = Some(Ok(None));
    assert_eq!(plan(mobility, &mut host), Some(failed("population arrival mobility is invalid")));
    // A refusal the PyO3 crate read for an argument is raised where the rule reads it.
    let mut both = args("z0", 0);
    both.mobility = Some(Err(unsupported("a mobility of another type")));
    assert_eq!(plan(both, &mut host), Some(failed("population arrival count is outside its supported range")));
    assert_eq!(host.calls, Vec::<Call>::new());
    // The contract decides a language Rust doesn't take; this host's takes it, which Rust refuses.
    let mut language = args("z0", 1);
    language.language = Some(Ok(Language::Ask(V::Int(5))));
    assert_eq!(plan(language, &mut host), Some(unsupported("a language the contract takes and Rust does not")));
    let fields = map(vec![("age_years", V::Int(30)), ("household_role", s("adult")), ("languages", list(vec![V::Int(5)]))]);
    assert_eq!(host.calls, vec![Call::Contract(Contract::PersonTraits, fields)]);
    // Each person context goes through its contract, then its index is checked.
    let mut host = TestHost::default();
    let mut contexts = args("z0", 2);
    let context = map(vec![("member_index", V::Int(2)), ("care_support_needs", list(vec![s("hygiene_support")]))]);
    contexts.overrides = Some(Ok(Overrides::Items(vec![context.clone()])));
    assert_eq!(plan(contexts, &mut host), Some(failed("population arrival person context index is invalid")));
    assert_eq!(host.calls, vec![Call::Contract(Contract::ArrivalContext, context)]);
    let mut caught = args("z0", 2);
    caught.overrides = Some(Ok(Overrides::Caught));
    assert_eq!(plan(caught, &mut host), Some(failed("population arrival person context is invalid")));
}

#[test]
fn a_first_arrival_makes_the_state_through_the_population_contract() {
    let cell = Shard::new(world_without_state());
    let mut inputs = inputs();
    inputs.namespaces.dynamics = Held::Absent;
    let mut host = TestHost::default();
    let config = map(vec![("people", map(vec![("b", map(vec![("age_years", V::Int(3))])), ("a", map(vec![("age_years", V::Int(4))]))]))]);
    let mut first = args("z1", 1);
    first.population = Some(config.clone());
    let plan = population::plan_arrivals(&cell, &mut host, &inputs, &arrival_event("arrive-1"), first).unwrap();
    assert_eq!(host.calls, vec![Call::Contract(Contract::PopulationSpec, config)]);
    let made = plan.new_state.clone().unwrap();
    assert_eq!(get(&made, &["initial_agent_ids"]), &list(vec![s("a1"), s("a2")]));
    let V::Map(traits) = get(&made, &["profile_traits"]) else { panic!("a map") };
    assert_eq!(traits.keys().map(|key| &**key).collect::<Vec<_>>(), ["a", "b"]);
    // The PyO3 crate puts the state in, and the rule writes on it.
    {
        let world = w(&cell);
        world.slots.at(world.slots.index_of(DYNAMICS).unwrap()).unwrap().try_borrow_mut().unwrap().row = Some(Box::new(ValueRow(made)));
    }
    inputs.namespaces.dynamics = Held::Held;
    assert_eq!(cell.with(|world| population::write_arrivals(world, &inputs, &plan)).unwrap(), Ok(1));
    assert_eq!(agent_ids(&w(&cell)).len(), 3);
    // A bound past `_state`: the state it made goes in first, then the text.
    let cell = Shard::new(world_without_state());
    let mut absent = self::inputs();
    absent.namespaces.dynamics = Held::Absent;
    let mut many = args("z0", 1000);
    many.count = Ok(Some(1000));
    {
        let world = w(&cell);
        let mut rows = world.rows_mut().unwrap();
        for index in 0..9001 {
            let id = format!("p{index}");
            rows.agents.insert(text(&id), agent(&id, "z1", Activity::Idle));
        }
    }
    let plan = population::plan_arrivals(&cell, &mut TestHost::default(), &absent, &arrival_event("arrive-2"), many).unwrap();
    assert!(plan.new_state.is_some());
    assert_eq!(plan.fails.as_deref(), Some("population arrival exceeds the 10000-resident runtime bound"));
}

fn world_without_state() -> World {
    let world = world();
    world.slots.at(world.slots.index_of(DYNAMICS).unwrap()).unwrap().try_borrow_mut().unwrap().row = None;
    world
}

// ---- Admission of the waiting --------------------------------------------------------------------

#[test]
fn the_waiting_are_admitted_in_order_when_room_opens_and_nothing_is_written_otherwise() {
    let cell = Shard::new(world());
    let mut host = TestHost::default();
    let inputs = inputs();
    assert_eq!(arrive(&cell, &mut host, &inputs, &arrival_event("arrive-1"), args("z0", 2)), Ok(0));
    let world = w(&cell);
    // The first pass puts the waiting in their order of admission (the event, then the ID).
    assert_eq!(population::admit_pending(&world, &inputs, 8), Ok(Vec::new()));
    let stamp = world.stamp_of(DYNAMICS).unwrap();
    assert_eq!(population::admit_pending(&world, &inputs, 8), Ok(Vec::new()));
    // The list and the summaries are the same: the namespace is not written.
    assert_eq!(world.stamp_of(DYNAMICS).unwrap(), stamp);
    assert_eq!(metric(&world, "population_waiting_count"), Some(2.0));
    population::check_waiting_for_admission(&world, &inputs).unwrap();
    world.rows_mut().unwrap().agents.remove_row("a2");
    let admitted = population::admit_pending(&world, &inputs, 9).unwrap();
    assert_eq!(admitted.len(), 1);
    let dynamics = state(&world, DYNAMICS);
    let V::List(pending) = get(&dynamics, &["pending_arrivals"]) else { panic!("a list") };
    assert_eq!(pending.len(), 1);
    let V::List(arrivals) = get(&dynamics, &["arrivals"]) else { panic!("a list") };
    assert_eq!((get(&arrivals[0], &["waiting_count"]), get(&arrivals[0], &["admitted_count"])), (&V::Int(1), &V::Int(1)));
    assert_eq!(get(&arrivals[1], &["admitted_minute"]), &V::Int(9));
    assert_eq!(world.rows().unwrap().agents.get(&*admitted[0]).unwrap().next_decision_at, 9);
    let presence = state(&world, PRESENCE);
    assert_eq!(get(&presence, &["people", &admitted[0], "status"]), &s("admitted"));
    assert_eq!((metric(&world, "population_arrival_count"), metric(&world, "population_waiting_count")), (Some(1.0), Some(1.0)));
    // A saved profile the constructors turn down raises their ValueError where the agent is made.
    world.rows_mut().unwrap().agents.remove_row("a1");
    {
        let mut row = world.row_mut::<ValueRow>(DYNAMICS).unwrap();
        let V::Map(state) = &mut row.0 else { panic!("a map") };
        let V::List(pending) = state.get_mut("pending_arrivals").unwrap() else { panic!("a list") };
        let V::Map(item) = &mut pending[0] else { panic!("a map") };
        let V::Map(profile) = item.get_mut("profile").unwrap() else { panic!("a map") };
        profile.insert(text("mobility"), s("flying"));
    }
    assert_eq!(population::admit_pending(&world, &inputs, 10), Err(Error::Value("'flying' is not a valid Mobility".to_owned())));
}

// ---- Departures and retries ----------------------------------------------------------------------

fn departure_args(count: i64, include_initial: bool) -> DepartureArgs {
    DepartureArgs { zone: Ok(Some(text("z0"))), count: Ok(Some(count)), include_initial: Some(Some(include_initial)), population: None }
}

#[test]
fn departures_take_the_initial_people_and_cancel_their_events() {
    let cell = Shard::new(world());
    let inputs = inputs();
    let mut host = TestHost::default();
    let event = arrival_event("depart-1");
    let plan = |args: DepartureArgs, host: &mut TestHost| population::plan_departures(&cell, host, &inputs, &event, args);
    match plan(departure_args(1, false), &mut host) {
        Ok(Departures::Fails { new_state: None, text }) => assert_eq!(text, "only 0 admitted dynamic residents are available to leave"),
        _ => panic!("the rule's text"),
    }
    let mut not_bool = departure_args(1, false);
    not_bool.include_initial = Some(None);
    assert_eq!(plan(not_bool, &mut host).err(), Some(failed("population departure include_initial must be boolean")));
    // a2 is in service: the departure waits.
    assert!(matches!(plan(departure_args(2, true), &mut host), Ok(Departures::Busy { new_state: None })));
    let selected = match plan(departure_args(1, true), &mut host) {
        Ok(Departures::Leave { new_state: None, selected }) => selected,
        _ => panic!("people leave"),
    };
    assert_eq!(selected, [text("a1")]);
    let world = w(&cell);
    // An event that names a1 is cancelled; another stays.
    let events = Events::of(&world).unwrap();
    let named = |id: &str, agent: &str| {
        let mut payload = IndexMap::new();
        payload.insert(text("agent_id"), s(agent));
        let sequence = events.allocate_sequence().unwrap();
        EventRow::new(text(id), EventKind::Endogenous, 20, 0, sequence, text("need_check"), payload, None, None, IndexMap::new()).unwrap()
    };
    for (id, agent) in [("for-a1", "a1"), ("for-a2", "a2")] {
        assert!(events.schedule(named(id, agent)).is_ok());
    }
    let cancels = population::write_departures(&world, &inputs, &event, &selected).unwrap();
    assert_eq!(cancels.iter().map(|(row, detail)| (row.event_id.to_string(), detail.clone())).collect::<Vec<_>>(), [(
        "for-a1".to_owned(),
        "Agent a1 departed before the event was due".to_owned()
    )]);
    assert_eq!(agent_ids(&world), ["a2"]);
    let dynamics = state(&world, DYNAMICS);
    let V::List(departures) = get(&dynamics, &["departures"]) else { panic!("a list") };
    assert_eq!(get(&departures[0], &["profile", "mobility"]), &s("independent"));
    assert_eq!(get(&departures[0], &["traits"]), &V::Null);
    assert_eq!(get(&state(&world, PRESENCE), &["people", "a1", "status"]), &s("departed"));
    assert_eq!(metric(&world, "population_departure_count"), Some(1.0));
}

#[test]
fn a_retry_is_scheduled_next_minute_until_its_bound() {
    let world = world();
    let event = arrival_event("depart-1");
    population::schedule_retry(&world, &Held::Absent, &event, "departure-retry", "departure").unwrap();
    let pending = world.queue().unwrap().pending_ids(None);
    assert_eq!(pending.len(), 1);
    assert!(pending[0].starts_with("departure-retry-"));
    assert_eq!(world.id_counter("event").unwrap(), Some(11));
    let mut last = event.clone();
    last.provenance = map(vec![("population_retry", map(vec![("count", V::Int(1440))]))]);
    assert_eq!(population::schedule_retry(&world, &Held::Absent, &last, "departure-retry", "departure"), Err(Error::suppressed("departure reached the bounded deferral limit")));
    world.scalars.try_borrow_mut().unwrap().minute_index = Fit::Ok(599);
    assert_eq!(
        population::schedule_retry(&world, &Held::Absent, &event, "presence-retry", "population exit"),
        Err(Error::suppressed("population exit could not complete before the scenario ended"))
    );
}

// ---- The checks of the state ---------------------------------------------------------------------

#[test]
fn the_validation_raises_the_rule_s_texts_in_its_order() {
    let world = world();
    let inputs = inputs();
    assert_eq!(population::validate(&world, &inputs), Ok(()));
    let with = |change: &dyn Fn(&mut indexmap::IndexMap<Arc<str>, V>)| {
        let mut state = dynamics_state(vec!["a1", "a2"]);
        let V::Map(fields) = &mut state else { panic!("a map") };
        change(fields);
        world.row_mut::<ValueRow>(DYNAMICS).unwrap().0 = state;
        population::validate(&world, &inputs)
    };
    let marked = |text: &str| Err(Error::MarkedValue(text.to_owned()));
    assert_eq!(with(&|fields| {
        fields.insert(text("extra"), V::Null);
    }), marked("population_dynamics_v1 has an invalid exact schema"));
    assert_eq!(with(&|fields| {
        fields.insert(text("initial_agent_ids"), list(vec![s("a1")]));
    }), marked("population_dynamics_v1 is missing an active profile"));
    assert_eq!(with(&|fields| {
        fields.insert(text("profile_context"), map(vec![("stranger", map(vec![]))]));
    }), marked("population_dynamics_v1 has an invalid dynamic context index"));
    assert_eq!(with(&|fields| {
        fields.insert(text("pending_arrivals"), list(vec![map(vec![("broken", V::Bool(true))])]));
    }), marked("population_dynamics_v1 pending arrival is invalid"));
    let waiting = map(vec![
        ("profile", map(vec![("id", s("w1"))])),
        ("traits", map(vec![("age_years", V::Int(30))])),
        ("zone_id", s("z0")),
        ("created_minute", V::Int(0)),
        ("event_id", s("e")),
    ]);
    assert_eq!(with(&|fields| {
        fields.insert(text("pending_arrivals"), list(vec![waiting.clone()]));
    }), Ok(()));
    assert_eq!(with(&|fields| {
        fields.insert(text("pending_arrivals"), list(vec![waiting.clone(), waiting.clone()]));
    }), marked("population_dynamics_v1 pending arrival is invalid"));
}

#[test]
fn a_presence_record_is_the_contract_s_dump() {
    let record = map(vec![
        ("agent_id", s("a1")),
        ("household_id", V::Null),
        ("status", s("admitted")),
        ("since_minute", V::Int(3)),
        ("transition_zone_id", s("z0")),
        ("return_zone_id", V::Null),
        ("expected_return_minute", V::Null),
        ("archive_path", V::Null),
        ("source_event_ids", list(vec![s("e1")])),
    ]);
    let person = Person::parse(&record).unwrap();
    assert_eq!(person.dump(), record);
    assert_eq!(person.hash().unwrap().len(), 64);
    let mut without_zone = person.clone();
    without_zone.transition_zone_id = None;
    assert_eq!(without_zone.check(), Err(unsupported("admitted presence requires a zone")));
    assert_eq!(population::pair_of("[\"a\",\"b\"]").unwrap(), ["a".to_owned(), "b".to_owned()]);
    assert!(population::pair_of("[\"a\"]").is_err());
}
