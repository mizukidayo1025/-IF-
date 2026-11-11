//! Lane c7: the rules of `domain/population_lifecycle.py` (`rules::population_lifecycle`) and of
//! `AgentTravelCompletionHandler` (`rules::travel_completion`) on worlds built here with no Python.
//! The PyO3 crate's kernels of the same names are glue over these, so the gate and the offline
//! tests run them on every Run; these say what each reads, decides and writes.

use std::sync::Arc;

use indexmap::IndexMap;
use sheltersim_core::entities::{Activity, AgentRow, AnyTable, Entities, Mobility, NeedRow, ProfileRow, Table, TaskRow, ZoneRow};
use sheltersim_core::error::Error;
use sheltersim_core::events::log::EventLog;
use sheltersim_core::events::queue::Queue;
use sheltersim_core::events::{EventKind, EventRow};
use sheltersim_core::row::{IdMap, Row, ValueRow};
use sheltersim_core::rules::kit::Held;
use sheltersim_core::rules::population::{self, Built, Namespaces, PRESENCE};
use sheltersim_core::rules::population_lifecycle::{self as lifecycle, Acted, Command, Facts, Inputs, Logged, TaskFacts, Tree, KEY};
use sheltersim_core::rules::travel_completion as travel;
use sheltersim_core::state::agent_dynamics::{AgentDynamicsRow, AgentMemory, NAME as AGENT_DYNAMICS};
use sheltersim_core::state::information::{BeliefRow, ContactRow, CountersRow, InformationRow, MessageRow, NAME as INFORMATION};
use sheltersim_core::state::metric_state::MetricRow;
use sheltersim_core::state::social_network::{Relationship, Relationships, SocialNetworkRow, NAME as SOCIAL_NETWORK};
use sheltersim_core::value::V;
use sheltersim_core::world::{Fit, Place, Seed, World, ID_COUNTERS, METRIC_STATE};

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

fn s(value: &str) -> V {
    V::Str(text(value))
}

fn map(entries: Vec<(&str, V)>) -> V {
    V::Map(entries.into_iter().map(|(key, value)| (text(key), value)).collect())
}

fn get<'a>(value: &'a V, path: &[&str]) -> &'a V {
    path.iter().fold(value, |node, key| match node {
        V::Map(map) => map.get(*key).unwrap_or_else(|| panic!("no {key}")),
        _ => panic!("not a map at {key}"),
    })
}

fn keys(value: &V) -> Vec<String> {
    match value {
        V::Map(map) => map.keys().map(|key| key.to_string()).collect(),
        _ => panic!("not a map"),
    }
}

fn put<R: Clone>(table: &mut Table<R>, id: &str, row: R) {
    let slot = table.reserve_slot();
    table.fill_slot(slot, row);
    assert_eq!(table.bind(text(id), slot), None);
}

fn needs(level: f64) -> NeedRow {
    NeedRow { toilet_urgency: level, hunger: level, fatigue: level, hydration_need: level }
}

fn agent(id: &str, zone: &str, activity: Activity) -> AgentRow {
    AgentRow {
        profile: ProfileRow {
            id: text(id),
            group_id: Some(text("household-1")),
            initial_zone_id: text(zone),
            mobility: Mobility::Assisted,
            support_need: 0.25,
            stress_sensitivity: 0.75,
            rng_key: text(id),
            initial_needs: needs(0.0),
            provenance: map(vec![("source_id", s("SRC-PROFILE"))]),
        },
        zone_id: text(zone),
        activity,
        target_id: None,
        needs: needs(0.5),
        stress: 0.125,
        trust: 0.625,
        next_decision_at: 3,
        support_request_id: None,
        support_cooldown_until: 0,
    }
}

fn add_row<T: Row>(world: &mut World, name: &str, place: Place, row: T) {
    let index = world.slots.add(text(name), place).unwrap();
    world.slots.at(index).unwrap().try_borrow_mut().unwrap().row = Some(Box::new(row));
}

fn empty_lifecycle() -> V {
    map(vec![
        ("schema_version", V::Int(1)),
        ("policy_version", s(lifecycle::POLICY_VERSION)),
        ("source_id", s(lifecycle::SOURCE_ID)),
        ("people", map(vec![])),
        ("relationship_archive", V::List(vec![])),
        ("messages", map(vec![])),
        ("deferred_beliefs", map(vec![])),
        ("pending_returns", V::List(vec![])),
        ("pending_relocations", map(vec![])),
        ("history", V::List(vec![])),
    ])
}

fn message(id: &str, origin: &str) -> MessageRow {
    MessageRow {
        id: text(id),
        truth_status: text("false"),
        source_id: text("SRC-RUMOR"),
        zone_id: text("z0"),
        time: 2,
        origin_event_id: text("ev-rumor"),
        origin_agent_id: text(origin),
    }
}

fn belief(message_id: &str, strength: f64) -> BeliefRow {
    BeliefRow {
        message_id: text(message_id),
        belief: text("heard"),
        source: text("SRC-RUMOR"),
        last_contact: 2,
        last_event_id: text("ev-rumor"),
        exposed: true,
        belief_strength: strength,
        share_propensity: 0.5,
        correction_exposed: false,
        correction_accepted: false,
        ever_misbelieved: false,
        ever_forgotten: false,
        last_correction_accepted: false,
    }
}

fn beliefs(items: Vec<(&str, f64)>) -> IdMap<BeliefRow> {
    items.into_iter().map(|(id, strength)| (text(id), belief(id, strength))).collect()
}

fn memory(due: i64) -> AgentMemory {
    AgentMemory {
        known_facility_ids: vec![text("f1")],
        last_action: Some(text("rest")),
        last_target_id: None,
        last_decision_minute: Some(2),
        decision_due_minute: due,
        decision_reason: None,
    }
}

/// a2, a1 and a3 in z0, which has no capacity, and z1. A world of 600 minutes at minute 7 that
/// holds its queue, log, event counter and metrics, the lifecycle, the presence ledger, the
/// information state (m1 a claim of a1's that a2 and a3 believe, m2 one of a3's that a1 believes,
/// the contacts of a1 and a2 and of a2 and a3), a tie of a1 and a3 and each one's memory.
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
    for id in ["a2", "a1", "a3"] {
        put(&mut entities.agents, id, agent(id, "z0", Activity::Idle));
    }
    for id in ["z0", "z1"] {
        put(&mut entities.zones, id, ZoneRow { id: text(id), name: text(id), capacity: None, display_x: None, display_y: None });
    }
    *world.entities.get_mut() = entities;
    add_row(&mut world, ID_COUNTERS, Place::Field, ValueRow(map(vec![("event", V::Int(10))])));
    add_row(&mut world, METRIC_STATE, Place::Field, MetricRow::default());
    world.hold_queue(Arc::new(Queue::new()));
    world.hold_log(Arc::new(EventLog::new()));
    add_row(&mut world, KEY, Place::Namespace, ValueRow(empty_lifecycle()));
    add_row(&mut world, PRESENCE, Place::Namespace, ValueRow(map(vec![("schema_version", V::Int(1)), ("people", map(vec![])), ("history", V::List(vec![]))])));
    let mut messages = IdMap::default();
    messages.insert(text("m1"), message("m1", "a1"));
    messages.insert(text("m2"), message("m2", "a3"));
    let mut held = IdMap::default();
    held.insert(text("a2"), beliefs(vec![("m1", 0.5)]));
    held.insert(text("a1"), beliefs(vec![("m2", 0.25)]));
    held.insert(text("a3"), beliefs(vec![("m1", 0.75)]));
    let mut contacts = IdMap::default();
    contacts.insert(text(r#"["a2","a3"]"#), ContactRow { last_seen_minute: 6, consecutive_minutes: 2 });
    contacts.insert(text(r#"["a1","a2"]"#), ContactRow { last_seen_minute: 6, consecutive_minutes: 3 });
    let pressure: IdMap<f64> = [("a2", 0.0), ("a1", 0.375), ("a3", 0.0)].into_iter().map(|(id, value)| (text(id), value)).collect();
    add_row(
        &mut world,
        INFORMATION,
        Place::Namespace,
        InformationRow {
            schema_version: 2,
            messages,
            beliefs: held,
            contacts,
            query_pressure: pressure,
            support_requests: Vec::new(),
            counters: CountersRow { contact_attempts: 0, contact_deliveries: 0, correction_deliveries: 0, support_requests: 0 },
        },
    );
    let mut ties = IdMap::default();
    let mut tie = Relationship::empty();
    tie.contact_count = 4;
    tie.trust = Some(0.5);
    ties.insert(text(r#"["a1","a3"]"#), tie);
    add_row(
        &mut world,
        SOCIAL_NETWORK,
        Place::Namespace,
        SocialNetworkRow {
            schema_version: 1,
            policy_version: text("directed-contact-relationships-v1"),
            relationships: Relationships::from_map(ties),
            interaction_count: 0,
            information_transfer_count: 0,
        },
    );
    let agents: IdMap<AgentMemory> = [("a2", 4), ("a1", 5), ("a3", 6)].into_iter().map(|(id, due)| (text(id), memory(due))).collect();
    add_row(&mut world, AGENT_DYNAMICS, Place::Namespace, AgentDynamicsRow { schema_version: 1, policy_version: text("need-led-autonomy-memory-v2"), agents });
    world
}

fn population_inputs() -> population::Inputs {
    population::Inputs {
        namespaces: Namespaces {
            dynamics: Held::Absent,
            presence: Held::Held,
            information: Held::Held,
            body: Held::Absent,
            agent_dynamics: Held::Held,
            movement: Held::Absent,
            overlay: Held::Absent,
            spaces: Held::Absent,
            social_network: Held::Held,
            social_runtime: Held::Absent,
            resource_service: Held::Absent,
            effect_program: Held::Absent,
        },
        schema: Ok(Arc::new(None)),
        policy: Ok(Arc::new("need-led-autonomy-memory-v2")),
        awareness: Ok(Arc::new(None)),
        geo: None,
        routes: None,
    }
}

fn inputs(population: &population::Inputs) -> Inputs<'_> {
    Inputs { population, lifecycle: Held::Held, relocation: None }
}

fn event(id: &str) -> EventRow {
    EventRow::new(text(id), EventKind::Intervention, 7, 0, 3, text("effect_program_population"), IndexMap::new(), Some(text("SRC-COMMAND")), None, IndexMap::new()).unwrap()
}

fn command(action: &str, ids: &[&str], zone: Option<&str>) -> Command {
    Command {
        action: text(action),
        zone_id: zone.map(text),
        household_id: None,
        expected_return: None,
        source_ids: vec![text("SRC-ORDER")],
        ids: ids.iter().map(|id| text(id)).collect(),
    }
}

fn act(world: &World, inputs: &Inputs<'_>, row: &EventRow, command: &Command) -> Result<Acted, Error> {
    let reads = lifecycle::world_reads(world, &Held::Absent, &Logged::new(), row, command);
    lifecycle::presence_action(world, inputs, row, command, &reads)
}

fn state(world: &World, name: &str) -> V {
    world.row::<ValueRow>(name).unwrap().0.clone()
}

fn agent_ids(world: &World) -> Vec<String> {
    world.rows().unwrap().agents.iter().map(|(id, _)| id.to_string()).collect()
}

// ---- The records as their trees hold them -------------------------------------------------------

#[test]
fn a_record_goes_to_a_value_in_the_row_s_order_and_back_only_as_its_types() {
    let record = belief("m1", 0.5);
    let value = record.to_v();
    assert_eq!(keys(&value), BeliefRow::NAMES.iter().map(|name| name.to_string()).collect::<Vec<_>>());
    assert_eq!(get(&value, &["belief_strength"]), &V::Float(0.5));
    let back = BeliefRow::from_v(&value).unwrap();
    assert_eq!(back.to_v(), value);
    // An int where the row holds a float, a missing field, or another field: the row's misfit.
    let mut wrong = value.clone();
    if let V::Map(fields) = &mut wrong {
        fields.insert(text("belief_strength"), V::Int(1));
    }
    assert!(matches!(BeliefRow::from_v(&wrong), Err(Error::Unsupported(_))));
    let mut extra = value.clone();
    if let V::Map(fields) = &mut extra {
        fields.shift_remove("exposed");
        fields.insert(text("other"), V::Bool(true));
    }
    assert!(matches!(BeliefRow::from_v(&extra), Err(Error::Unsupported(_))));
    let tie = Relationship::empty();
    assert_eq!(keys(&tie.to_v()), Relationship::NAMES.iter().map(|name| name.to_string()).collect::<Vec<_>>());
    assert_eq!(get(&tie.to_v(), &["trust"]), &V::Null);
    let memory = memory(9);
    assert_eq!(AgentMemory::from_v(&memory.to_v()).unwrap().to_v(), memory.to_v());
}

// ---- Leaving and coming back --------------------------------------------------------------------

#[test]
fn a_person_who_leaves_is_archived_and_comes_back_at_the_end_of_the_table() {
    let world = world();
    let population = population_inputs();
    let inputs = inputs(&population);
    let exit = command("temporary_exit", &["a1"], None);
    let Acted::Done(written) = act(&world, &inputs, &event("ev-exit"), &exit).unwrap() else {
        panic!("deferred");
    };
    assert!(written.taken.is_empty());
    assert_eq!(agent_ids(&world), ["a2", "a3"]);
    let archived = state(&world, KEY);
    let person = get(&archived, &["people", "a1"]);
    assert_eq!(keys(person), ["agent", "agent_dynamics", "body_agent", "information", "since_minute", "source_event_ids"]);
    assert_eq!(get(person, &["agent", "activity"]), &s("idle"));
    assert_eq!(get(person, &["agent", "profile", "mobility"]), &s("assisted"));
    assert_eq!(get(person, &["agent_dynamics"]), &memory(5).to_v());
    assert_eq!(get(person, &["body_agent"]), &V::Null);
    assert_eq!(get(person, &["information", "query_pressure"]), &V::Float(0.375));
    assert_eq!(keys(get(person, &["information", "beliefs"])), ["m2"]);
    assert_eq!(get(person, &["source_event_ids"]), &V::List(vec![s("ev-exit")]));
    // The claim a1 started is kept, and everyone's belief in it deferred.
    assert_eq!(keys(get(&archived, &["messages"])), ["m1"]);
    assert_eq!(keys(get(&archived, &["deferred_beliefs"])), ["a2", "a3"]);
    assert_eq!(get(&archived, &["deferred_beliefs", "a3", "m1", "belief_strength"]), &V::Float(0.75));
    // The ties, in key order of each namespace: the contact, then the social tie.
    let V::List(ties) = get(&archived, &["relationship_archive"]) else { panic!() };
    let named: Vec<(V, V)> = ties.iter().map(|item| (get(item, &["namespace"]).clone(), get(item, &["pair_key"]).clone())).collect();
    assert_eq!(named, [(s(INFORMATION), s(r#"["a1","a2"]"#)), (s(SOCIAL_NETWORK), s(r#"["a1","a3"]"#))]);
    {
        let info = world.row::<InformationRow>(INFORMATION).unwrap();
        assert_eq!(info.messages.keys().map(|key| &**key).collect::<Vec<_>>(), ["m2"]);
        assert!(info.beliefs.get("a2").unwrap().is_empty());
        assert!(!info.beliefs.contains_key("a1"));
        assert_eq!(info.contacts.keys().map(|key| &**key).collect::<Vec<_>>(), [r#"["a2","a3"]"#]);
    }
    assert!(world.row::<SocialNetworkRow>(SOCIAL_NETWORK).unwrap().relationships.map.is_empty());
    let presence = state(&world, PRESENCE);
    assert_eq!(get(&presence, &["people", "a1", "status"]), &s("temporarily_absent"));
    assert_eq!(get(&presence, &["people", "a1", "return_zone_id"]), &s("z0"));
    assert_eq!(get(&presence, &["people", "a1", "household_id"]), &V::Null);
    let V::List(history) = get(&archived, &["history"]) else { panic!() };
    assert_eq!(written.receipt, history[0]);
    assert_eq!(get(&written.receipt, &["action"]), &s("temporary_exit"));
    assert_eq!(get(&written.receipt, &["household_id"]), &V::Null);
    assert_eq!(get(&written.receipt, &["source_ids"]), &V::List(vec![s("SRC-COMMAND"), s("SRC-ORDER")]));

    // Coming back to z1: a new row at the end, the records and ties back.
    let back = command("return", &["a1"], Some("z1"));
    let Acted::Done(written) = act(&world, &inputs, &event("ev-return"), &back).unwrap() else {
        panic!("deferred");
    };
    assert_eq!(agent_ids(&world), ["a2", "a3", "a1"]);
    let restored = world.rows().unwrap().agents.get("a1").unwrap().clone();
    assert_eq!((&*restored.zone_id, restored.activity, restored.next_decision_at, restored.stress, restored.trust), ("z1", Activity::Idle, 7, 0.125, 0.625));
    assert_eq!(restored.needs.hunger, 0.5);
    let archived = state(&world, KEY);
    assert!(keys(get(&archived, &["people"])).is_empty());
    assert!(keys(get(&archived, &["messages"])).is_empty());
    assert!(keys(get(&archived, &["deferred_beliefs"])).is_empty());
    assert_eq!(get(&archived, &["relationship_archive"]), &V::List(vec![]));
    {
        let info = world.row::<InformationRow>(INFORMATION).unwrap();
        assert_eq!(info.messages.keys().map(|key| &**key).collect::<Vec<_>>(), ["m2", "m1"]);
        assert_eq!(info.beliefs.keys().map(|key| &**key).collect::<Vec<_>>(), ["a2", "a3", "a1"]);
        assert_eq!(info.beliefs.get("a3").unwrap().get("m1").unwrap().belief_strength, 0.75);
        assert_eq!(info.query_pressure.get("a1"), Some(&0.375));
        assert_eq!(info.contacts.keys().map(|key| &**key).collect::<Vec<_>>(), [r#"["a2","a3"]"#, r#"["a1","a2"]"#]);
    }
    let dynamics = world.row::<AgentDynamicsRow>(AGENT_DYNAMICS).unwrap();
    assert_eq!(dynamics.agents.keys().map(|key| &**key).collect::<Vec<_>>(), ["a2", "a3", "a1"]);
    assert_eq!(dynamics.agents.get("a1").unwrap().decision_due_minute, 7);
    drop(dynamics);
    assert_eq!(world.row::<SocialNetworkRow>(SOCIAL_NETWORK).unwrap().relationships.map.get(r#"["a1","a3"]"#).unwrap().contact_count, 4);
    assert_eq!(get(&state(&world, PRESENCE), &["people", "a1", "status"]), &s("admitted"));
    assert_eq!(get(&written.receipt, &["action"]), &s("returned"));
    assert_eq!(get(&written.receipt, &["to_zone_id"]), &s("z1"));
}

#[test]
fn a_busy_person_s_exit_waits_and_a_return_needs_someone_away() {
    let world = world();
    world.rows_mut().unwrap().agents.get_mut("a1").unwrap().activity = Activity::Queued;
    let population = population_inputs();
    let inputs = inputs(&population);
    let Acted::Deferred(reason) = act(&world, &inputs, &event("ev-exit"), &command("temporary_exit", &["a1"], None)).unwrap() else {
        panic!("not deferred");
    };
    assert_eq!(reason, "the person is queued");
    assert_eq!(lifecycle::deferred_text(&reason), "population exit waits until the person is free: the person is queued");
    assert_eq!(agent_ids(&world), ["a2", "a1", "a3"]);
    let refused = act(&world, &inputs, &event("ev-return"), &command("return", &["a2"], Some("z1"))).err().unwrap();
    assert_eq!(refused, Error::failed("return requires a retained absent person"));
    let refused = act(&world, &inputs, &event("ev-exit"), &command("transfer", &["a9"], None)).err().unwrap();
    assert_eq!(refused, Error::failed("transfer requires an admitted person"));
    let refused = act(&world, &inputs, &event("ev-move"), &command("reunite", &["a1", "a2"], None)).err().unwrap();
    assert_eq!(refused, Error::failed("reunion requires a target household id"));
}

#[test]
fn a_command_s_minute_and_its_expected_return_are_checked_first() {
    let world = world();
    let late = EventRow::new(text("ev"), EventKind::Intervention, 8, 0, 3, text("effect_program_population"), IndexMap::new(), Some(text("S")), None, IndexMap::new()).unwrap();
    assert_eq!(lifecycle::check_command(&world, &late, None), Err(Error::failed("population command minute must match the current event boundary")));
    assert_eq!(lifecycle::check_command(&world, &event("ev"), Some(6)), Err(Error::failed("expected return minute precedes the departure event")));
    let mut retried = event("ev");
    retried.provenance = map(vec![("population_retry", map(vec![]))]);
    assert_eq!(lifecycle::check_command(&world, &retried, Some(6)), Ok(true));
    assert_eq!(lifecycle::check_command(&world, &event("ev"), Some(7)), Ok(false));
}

#[test]
fn the_household_is_the_ledger_s_and_without_the_ledger_the_profile_s_group() {
    let world = world();
    assert_eq!(lifecycle::household_of(&world, &Held::Held, "a1"), Ok(None));
    assert_eq!(lifecycle::household_of(&world, &Held::Absent, "a1"), Ok(Some(text("household-1"))));
    assert_eq!(lifecycle::household_of(&world, &Held::Absent, "a9"), Ok(None));
    world.row_mut::<ValueRow>(PRESENCE).unwrap().0 = map(vec![("people", s("no"))]);
    assert_eq!(lifecycle::household_of(&world, &Held::Held, "a1"), Err(Error::unsupported("the presence's people are not a dict")));
    world.row_mut::<ValueRow>(PRESENCE).unwrap().0 = map(vec![("people", map(vec![("a1", map(vec![("household_id", V::Int(3))]))]))]);
    assert_eq!(lifecycle::household_of(&world, &Held::Held, "a1"), Err(Error::unsupported("a household is not a str")));
}

#[test]
fn a_restored_agent_raises_the_constructors_value_errors_in_their_order() {
    let world = world();
    let raw = {
        let population = population_inputs();
        let inputs = inputs(&population);
        act(&world, &inputs, &event("ev-exit"), &command("temporary_exit", &["a1"], None)).unwrap();
        get(&state(&world, KEY), &["people", "a1", "agent"]).clone()
    };
    let with = |path: &[&str], value: V| {
        let mut changed = raw.clone();
        let mut node = &mut changed;
        for key in &path[..path.len() - 1] {
            let V::Map(fields) = node else { panic!() };
            node = fields.get_mut(*key).unwrap();
        }
        let V::Map(fields) = node else { panic!() };
        fields.insert(text(path[path.len() - 1]), value);
        changed
    };
    let raises = |value: &V, minute: i64| match lifecycle::restored_agent(value, &text("z1"), minute, 6).unwrap() {
        Built::Raises(error) => error.to_string(),
        Built::Row(_) => "row".to_owned(),
    };
    assert_eq!(raises(&raw, 7), "row");
    assert_eq!(raises(&with(&["stress"], V::Float(1.5)), 7), "stress must be finite and between 0 and 1");
    assert_eq!(raises(&with(&["trust"], V::Float(-0.5)), 7), "trust must be finite and between 0 and 1");
    assert_eq!(raises(&raw, -1), "next_decision_at must be a non-negative integer");
    assert_eq!(raises(&with(&["support_cooldown_until"], V::Int(-2)), 7), "support_cooldown_until must be a non-negative integer");
    // The profile's own first: the mobility before the person's values.
    let both = {
        let mut both = with(&["stress"], V::Float(1.5));
        if let V::Map(fields) = &mut both {
            let profile = fields.get_mut("profile").unwrap();
            if let V::Map(profile) = profile {
                profile.insert(text("mobility"), s("x"));
            }
        }
        both
    };
    assert_eq!(raises(&both, 7), "'x' is not a valid Mobility");
    // v5 has no hydration: 0.0.
    let Built::Row(agent) = lifecycle::restored_agent(&raw, &text("z1"), 7, 5).unwrap() else { panic!() };
    assert_eq!(agent.needs.hydration_need, 0.0);
}

// ---- The validation ------------------------------------------------------------------------------

fn facts_of<'a>(state: &'a V, presence: Option<Option<&'a V>>, check: impl FnOnce(&Facts<'_>)) {
    let has_agent = |id: &str| Ok(id == "a2");
    let has_zone = |id: &str| Ok(id == "z0" || id == "z1");
    let tasks = |_: &str| -> Result<Vec<TaskFacts>, Error> { Ok(Vec::new()) };
    let event = |_: &str| -> Result<Option<Arc<EventRow>>, Error> { Ok(None) };
    check(&Facts { state: Some(state), presence, has_agent: &has_agent, has_zone: &has_zone, tasks: &tasks, event: &event });
}

#[test]
fn the_validation_says_its_texts_in_its_order() {
    let invalid = |text: &str| Err(Error::MarkedValue(text.to_owned()));
    facts_of(&empty_lifecycle(), None, |facts| assert_eq!(lifecycle::validate_facts(facts, None), Ok(())));
    let mut wrong = empty_lifecycle();
    if let V::Map(fields) = &mut wrong {
        fields.insert(text("schema_version"), V::Int(2));
    }
    facts_of(&wrong, None, |facts| assert_eq!(lifecycle::validate_facts(facts, None), invalid("population_lifecycle_v1 has an invalid exact schema")));
    // A tie whose value json.dumps refuses: its text, of the first value in key order.
    let mut tie = empty_lifecycle();
    if let V::Map(fields) = &mut tie {
        fields.insert(
            text("relationship_archive"),
            V::List(vec![map(vec![
                ("namespace", s(SOCIAL_NETWORK)),
                ("pair_key", s(r#"["a1","a2"]"#)),
                ("agent_ids", V::List(vec![s("a1"), s("a2")])),
                ("value", map(vec![("z", V::Float(f64::NAN)), ("a", V::Float(f64::NEG_INFINITY))])),
                ("restore_when_both_active", V::Bool(true)),
            ])]),
        );
    }
    facts_of(&tie, None, |facts| assert_eq!(lifecycle::validate_facts(facts, None), invalid("Out of range float values are not JSON compliant: -inf")));
    // A pending return of someone not archived; then one archived but not waiting in the ledger.
    let mut waiting = empty_lifecycle();
    let item = map(vec![("agent_id", s("a1")), ("zone_id", s("z1")), ("event_id", s("ev")), ("minute", V::Int(3)), ("household_id", V::Null), ("source_ids", V::List(vec![]))]);
    if let V::Map(fields) = &mut waiting {
        fields.insert(text("pending_returns"), V::List(vec![item]));
    }
    facts_of(&waiting, None, |facts| assert_eq!(lifecycle::validate_facts(facts, None), invalid("population_lifecycle_v1 pending return is invalid")));
    // A relocation of someone here, whose task and event are not there.
    let mut moving = empty_lifecycle();
    let relocation = map(vec![
        ("task_id", s("t")),
        ("event_id", s("e")),
        ("from_zone_id", s("z0")),
        ("to_zone_id", s("z1")),
        ("household_id", V::Null),
        ("source_ids", V::List(vec![])),
        ("started_minute", V::Int(1)),
        ("due_minute", V::Int(4)),
    ]);
    if let V::Map(fields) = &mut moving {
        fields.insert(text("pending_relocations"), map(vec![("a2", relocation)]));
    }
    facts_of(&moving, None, |facts| {
        assert_eq!(lifecycle::validate_facts(facts, None), invalid("population_lifecycle_v1 relocation task/event is inconsistent"));
        // The person whose relocation is about to be dropped is not asked.
        assert_eq!(lifecycle::validate_facts(facts, Some("a2")), Ok(()));
    });
}

// ---- Travel completion ---------------------------------------------------------------------------

fn walk(task: &str, purpose: &str, destination: &str) -> EventRow {
    let payload: IndexMap<Arc<str>, V> = [("agent_id", s("a1")), ("task_id", s(task)), ("destination_zone_id", s(destination)), ("purpose", s(purpose))]
        .into_iter()
        .map(|(key, value)| (text(key), value))
        .collect();
    EventRow::new(text("walk-1"), EventKind::Completion, 7, 0, 4, text("agent_travel_complete"), payload, None, None, IndexMap::new()).unwrap()
}

fn travel_inputs() -> travel::Inputs<'static> {
    travel::Inputs { movement: Held::Absent, queues: Held::Absent, overlay: Held::Absent, geo: None, section: None, resource_service: false, lifecycle: None }
}

#[test]
fn a_walk_home_ends_its_task_and_leaves_the_person_idle_there() {
    let world = world();
    {
        let mut rows = world.rows_mut().unwrap();
        let agent = rows.agents.get_mut("a1").unwrap();
        agent.activity = Activity::Moving;
        agent.target_id = Some(text("z1"));
        rows.active_tasks.insert(
            text("t1"),
            TaskRow { id: text("t1"), kind: text("travel_return"), agent_id: Some(text("a1")), resource_id: None, start_minute: 5, due_minute: 7, staff_id: None },
        );
    }
    let inputs = travel_inputs();
    let checks = |row: &EventRow| match travel::apply(&world, &inputs, row) {
        Err(travel::Applied::Checks(error)) => error,
        other => panic!("{other:?}"),
    };
    assert_eq!(checks(&walk("t9", "return", "z1")), Error::failed("travel completion does not match an active task"));
    assert_eq!(checks(&walk("t1", "return", "zz")), Error::failed("destination zone 'zz' does not exist"));
    assert_eq!(checks(&walk("t1", "elsewhere", "z1")), Error::failed("unsupported travel purpose 'elsewhere'"));
    assert_eq!(checks(&walk("t1", "return", "z0")), Error::failed("return travel completion does not match its target"));
    let mut bare = walk("t1", "return", "z1");
    bare.payload = map(vec![("agent_id", s("a1"))]);
    assert_eq!(checks(&bare), Error::failed("travel completion requires Agent, task, destination, and purpose"));
    let walked = travel::apply(&world, &inputs, &walk("t1", "return", "z1")).unwrap().unwrap();
    assert_eq!(&*walked.destination, "z1");
    let rows = world.rows().unwrap();
    let agent = rows.agents.get("a1").unwrap();
    assert_eq!((&*agent.zone_id, agent.activity, agent.target_id.clone()), ("z1", Activity::Idle, None));
    assert!(rows.active_tasks.get("t1").is_none());
}
