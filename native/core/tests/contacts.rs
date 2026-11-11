//! Lane P8 a4: the social model (`rules::social_propagation`) and the information model's contacts
//! (`rules::information`) in the core, on worlds built here, with no Python.
//!
//! `data/social_plan.json` holds what the PyO3 kernel before this lane (`main` `6694e74`) gave for
//! the plain inputs of `social_plan` (its `plan_of_values`, the script in the lane's notes): the
//! stresses' bits, the contacts in their order, the cooldowns and the observed pairs.

mod support;

use std::collections::HashMap;
use std::sync::Arc;

use indexmap::IndexMap;
use serde_json::Value;
use sheltersim_core::config::{Fault, FaultClass, Selection};
use sheltersim_core::contacts::Basis;
use sheltersim_core::entities::{Activity, AgentRow, AnyTable, Entities, Mobility, NeedRow, ProfileRow, Table, ZoneRow};
use sheltersim_core::error::Error;
use sheltersim_core::events::log::EventLog;
use sheltersim_core::events::queue::Queue;
use sheltersim_core::events::writer::WriteError;
use sheltersim_core::host::{Trace, TraceLevel};
use sheltersim_core::mathx::{expm1, py_min, Num};
use sheltersim_core::row::{IdMap, Row, ValueRow};
use sheltersim_core::rules::information::{self as information, Settings as InfoSettings};
use sheltersim_core::rules::psychology::{Bad, Delta};
use sheltersim_core::rules::social_propagation::{self as social, Neighbors, Person, Settings};
use sheltersim_core::rules::world_view::{self as view, Inputs as ViewInputs, ViewCache, ViewData};
use sheltersim_core::run::held::{Class, Held, Obj};
use sheltersim_core::run::information::{Information, Model as InfoModel};
use sheltersim_core::run::stress::{Model, Stress, UNKNOWN};
use sheltersim_core::state::information::{BeliefRow, ContactRow, CountersRow, InformationRow, MessageRow, NAME as INFORMATION};
use sheltersim_core::state::social_network::{Relationship, Relationships, SocialNetworkRow, NAME as NETWORK};
use sheltersim_core::state::social_propagation::{ContactRecord, SocialRow, NAME as SOCIAL};
use sheltersim_core::value::V;
use sheltersim_core::world::{Fit, Place, Seed, World, ID_COUNTERS, MODEL_CONFIGS};
use support::{Call, TestHost};

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

fn unsupported(reason: &str) -> Error {
    Error::Unsupported(reason.to_owned().into())
}

fn needs(value: f64) -> NeedRow {
    NeedRow { toilet_urgency: value, hunger: value, fatigue: value, hydration_need: value }
}

fn agent(id: &str, zone: &str, group: Option<&str>, stress: f64, sensitivity: f64) -> AgentRow {
    AgentRow {
        profile: ProfileRow {
            id: text(id),
            group_id: group.map(text),
            initial_zone_id: text(zone),
            mobility: Mobility::Independent,
            support_need: 0.0,
            stress_sensitivity: sensitivity,
            rng_key: text(id),
            initial_needs: needs(0.0),
            provenance: V::Map(IndexMap::new()),
        },
        zone_id: text(zone),
        activity: Activity::Idle,
        target_id: None,
        needs: needs(0.25),
        stress,
        trust: 0.75,
        next_decision_at: 7,
        support_request_id: None,
        support_cooldown_until: 0,
    }
}

fn put<R: Clone>(table: &mut Table<R>, id: &str, row: R) {
    let slot = table.reserve_slot();
    table.fill_slot(slot, row);
    assert_eq!(table.bind(text(id), slot), None);
}

fn add_row<T: Row>(world: &mut World, name: &str, place: Place, row: Option<T>) {
    let index = world.slots.add(text(name), place).unwrap();
    world.slots.at(index).unwrap().try_borrow_mut().unwrap().row = row.map(|row| Box::new(row) as Box<dyn Row>);
}

fn written(world: &mut World) -> Vec<String> {
    let names: Vec<String> = world.slots.written_names().iter().map(|name| name.to_string()).collect();
    world.slots.take_written();
    names
}

/// a1 and a2 (household g1) and a3 in z0, a4 in z1, z2 empty; minute 10 of 100, seed 7. The queue,
/// the log and `id_counters` are the world's.
fn world() -> World {
    let mut entities = Entities::default();
    put(&mut entities.agents, "a2", agent("a2", "z0", Some("g1"), 0.7, 0.5));
    put(&mut entities.agents, "a1", agent("a1", "z0", Some("g1"), 0.2, 1.0));
    put(&mut entities.agents, "a3", agent("a3", "z0", None, 0.4, 0.8));
    put(&mut entities.agents, "a4", agent("a4", "z1", None, 0.9, 1.0));
    for zone in ["z0", "z1", "z2"] {
        put(&mut entities.zones, zone, ZoneRow { id: text(zone), name: text(zone), capacity: None, display_x: None, display_y: None });
    }
    let mut world = World::default();
    *world.entities.get_mut() = entities;
    let scalars = world.scalars.get_mut();
    scalars.duration_minutes = Fit::Ok(100);
    scalars.minute_index = Fit::Ok(10);
    scalars.schema_version = Fit::Ok(6);
    scalars.seed = Fit::Ok(Seed::from_text("7").expect("a seed"));
    add_row(&mut world, MODEL_CONFIGS, Place::Field, Some(ValueRow(V::Map(IndexMap::new()))));
    let mut counters = IndexMap::new();
    counters.insert(text("event"), V::Int(40));
    add_row(&mut world, ID_COUNTERS, Place::Field, Some(ValueRow(V::Map(counters))));
    world.hold_queue(Arc::new(Queue::new()));
    world.hold_log(Arc::new(EventLog::new()));
    world
}

fn view_of(world: &World) -> ViewData {
    let selection = Ok(Arc::new(Selection::new(vec![(text("psychology"), text("life-state-feedback-v1"))])));
    let inputs = ViewInputs { selection: &selection, configs: None };
    view::build(world, &inputs, &mut ViewCache::default()).unwrap().finish(false)
}

fn delta(id: &str, stress: f64) -> Delta {
    Delta {
        agent_id: text(id),
        minute_index: 10,
        toilet_urgency: 0.125,
        hunger: 0.25,
        fatigue: 0.375,
        hydration_need: 0.5,
        stress,
        trust: 0.625,
        body: None,
    }
}

fn base_deltas() -> Vec<Delta> {
    vec![delta("a1", 0.25), delta("a2", 0.6), delta("a3", 0.45), delta("a4", 0.9)]
}

// ---- The social model ----------------------------------------------------------------------------

fn settings(policy: Model) -> Settings {
    Settings {
        policy,
        maximum: 3,
        minimum_contact_minutes: 1,
        cooldown_minutes: 5,
        spread: 0.05,
        recovery: 0.001,
        pair_cap: Num::Float(0.05),
        agent_cap: Num::Float(0.08),
        family_strength: 1.0,
        zone_strength: 0.75,
        nearby_strength: 0.5,
    }
}

fn neighbors() -> Neighbors {
    let mut table = HashMap::new();
    table.insert("z0".to_owned(), Ok(vec!["z1".to_owned()]));
    table.insert("z1".to_owned(), Ok(vec!["z0".to_owned(), "z2".to_owned()]));
    table.insert("z2".to_owned(), Err("a neighbor is not a str"));
    Ok(table)
}

fn social_row(contacts: &[(&str, i64, i64)], cooldowns: &[(&str, i64)]) -> SocialRow {
    SocialRow {
        schema_version: 1,
        contacts: contacts
            .iter()
            .map(|&(key, last, minutes)| (text(key), ContactRecord { last_seen_minute: last, consecutive_minutes: minutes }))
            .collect(),
        agent_cooldown_until: cooldowns.iter().map(|&(id, due)| (text(id), due)).collect(),
    }
}

fn network(interactions: i64) -> SocialNetworkRow {
    let mut map: IdMap<Relationship> = IdMap::default();
    map.insert(text("[\"a1\",\"a2\"]"), Relationship::empty());
    SocialNetworkRow {
        schema_version: 1,
        policy_version: text("observed-social-network-v1"),
        relationships: Relationships::from_map(map),
        interaction_count: interactions,
        information_transfer_count: 0,
    }
}

fn social_world(state: Option<SocialRow>, with_network: bool) -> World {
    let mut world = world();
    add_row(&mut world, SOCIAL, Place::Namespace, state);
    add_row(&mut world, NETWORK, Place::Namespace, with_network.then(|| network(3)));
    written(&mut world);
    world
}

fn social_inputs<'a>(settings: &'a Settings, expressiveness: &'a social::Table, view: &'a ViewData, deltas: &'a [Delta], neighbors: &'a Neighbors, traced: bool) -> social::Inputs<'a> {
    social::Inputs { settings, expressiveness, view, deltas, minute: 10, seed: "2026", neighbors, traced }
}

#[test]
fn a_plan_has_the_bits_of_the_kernel_before() {
    let expected: Value = serde_json::from_str(include_str!("data/social_plan.json")).unwrap();
    let ids = ["a1", "a2", "a3", "a4", "a5", "a6"];
    let rows: [(Option<&str>, &str, f64, f64, f64, f64); 6] = [
        (Some("g1"), "z0", 0.2, 1.0, 1.0, 0.25),
        (Some("g1"), "z0", 0.7, 0.5, 0.5, 0.6),
        (None, "z0", 0.4, 0.8, 0.0, 0.45),
        (None, "z1", 0.9, 1.0, 1.0, 0.9),
        (Some("g2"), "z1", 0.1, 0.3, 1.0, 0.05),
        (Some("g2"), "z2", 0.5, 1.0, 0.7, 0.5),
    ];
    let people: Vec<Person<'_>> = ids
        .iter()
        .zip(&rows)
        .map(|(id, &(group, zone, stress, sensitivity, expressiveness, base))| Person {
            id,
            group,
            zone,
            stress,
            sensitivity,
            expressiveness,
            base,
        })
        .collect();
    let mut neighbors = HashMap::new();
    neighbors.insert("z0".to_owned(), vec!["z1".to_owned()]);
    neighbors.insert("z1".to_owned(), vec!["z0".to_owned(), "z2".to_owned()]);
    neighbors.insert("z2".to_owned(), vec!["z1".to_owned()]);
    let previous = social_row(&[("[\"a1\",\"a2\"]", 9, 4), ("[\"a4\",\"a5\"]", 9, 1), ("[\"a6\",\"a5\"]", 3, 7)], &[("a2", 12), ("a3", 8)]);
    for (name, policy) in [("sparse", Model::Sparse), ("channel", Model::Channel), ("degroot", Model::DeGroot)] {
        let settings = Settings { cooldown_minutes: 5, minimum_contact_minutes: 2, ..settings(policy) };
        let settings = Settings { recovery: 0.001, spread: 0.05, pair_cap: Num::Float(0.05), agent_cap: Num::Float(0.08), ..settings };
        let plan = social::plan("2026", 10, &settings, &people, &neighbors, &previous.contacts, &previous.agent_cooldown_until, false).unwrap();
        let want = &expected[name];
        let bits: Vec<u64> = plan.stresses.iter().map(|stress| stress.to_bits()).collect();
        let want_bits: Vec<u64> = want["stresses"].as_array().unwrap().iter().map(|bits| bits.as_u64().unwrap()).collect();
        assert_eq!(bits, want_bits, "{name}");
        let contacts: Vec<(String, i64, i64)> =
            plan.contacts.iter().map(|(key, record)| (key.to_string(), record.last_seen_minute, record.consecutive_minutes)).collect();
        let want_contacts: Vec<(String, i64, i64)> = serde_json::from_value(want["contacts"].clone()).unwrap();
        assert_eq!(contacts, want_contacts, "{name}");
        let cooldowns = plan.cooldowns.as_ref().unwrap_or(&previous.agent_cooldown_until);
        let cooldowns: Vec<(String, i64)> = cooldowns.iter().map(|(id, due)| (id.to_string(), *due)).collect();
        let want_cooldowns: Vec<(String, i64)> = serde_json::from_value(want["cooldowns"].clone()).unwrap();
        assert_eq!(cooldowns, want_cooldowns, "{name}");
        let want_observed: Vec<(usize, usize)> = serde_json::from_value(want["observed"].clone()).unwrap();
        assert_eq!(plan.observed, want_observed, "{name}");
    }
}

#[test]
fn a_minute_writes_the_row_and_the_network_and_gives_the_deltas() {
    let mut world = social_world(Some(social_row(&[("[\"a1\",\"a2\"]", 9, 2), ("[\"x\",\"y\"]", 9, 1)], &[("a9", 50)])), true);
    let view = view_of(&world);
    let deltas = base_deltas();
    let (settings, expressiveness, neighbors) = (settings(Model::Sparse), social::Table::new(), neighbors());
    let planned = social::update(&world, &social_inputs(&settings, &expressiveness, &view, &deltas, &neighbors, false)).unwrap();
    assert!(planned.is_written() && !planned.first_use);
    assert_eq!(written(&mut world), [SOCIAL, NETWORK]);
    let row = world.row::<SocialRow>(SOCIAL).unwrap();
    // The contacts of this minute only, each a pair of people here.
    assert!(row.contacts.keys().all(|key| key.starts_with("[\"a") && !key.contains('x')));
    assert!(row.contacts.values().all(|record| record.last_seen_minute == 10));
    assert_eq!(row.contacts.get("[\"a1\",\"a2\"]").map(|record| record.consecutive_minutes), Some(3));
    // The sparse model keeps a cooldown that is due, and sets the cooldown of whoever took stress.
    assert_eq!(row.agent_cooldown_until.get("a9"), Some(&50));
    drop(row);
    let finals = planned.deltas(&deltas);
    assert_eq!(finals.iter().map(|delta| &*delta.agent_id).collect::<Vec<_>>(), ["a1", "a2", "a3", "a4"]);
    for (index, (final_delta, base)) in finals.iter().zip(&deltas).enumerate() {
        assert_eq!(final_delta.stress.to_bits(), planned.plan.stresses[index].to_bits());
        assert_eq!(Delta { stress: base.stress, ..final_delta.clone() }, *base);
    }
    // a1 is below everyone, so takes stress; a4 is above everyone, so only recovers.
    assert!(finals[0].stress > 0.25 - 0.001);
    assert_eq!(finals[3].stress, 0.9 - 0.001);
    let network = world.row::<SocialNetworkRow>(NETWORK).unwrap();
    let observed = planned.plan.observed.len() as i64;
    assert!(observed > 0);
    assert_eq!(network.interaction_count, 3 + observed);
    assert_eq!(network.relationships.map.values().map(|record| record.contact_count).sum::<i64>(), observed);
}

#[test]
fn deltas_are_sorted_except_for_degroot() {
    let world = social_world(Some(social_row(&[], &[])), false);
    let view = view_of(&world);
    let mut deltas = base_deltas();
    deltas.reverse();
    let (expressiveness, neighbors) = (social::Table::new(), neighbors());
    let settings = settings(Model::Channel);
    let planned = social::update(&world, &social_inputs(&settings, &expressiveness, &view, &deltas, &neighbors, false)).unwrap();
    assert_eq!(planned.order, [3, 2, 1, 0]);
    assert_eq!(&*planned.ids[0], "a1");
    let settings = settings_for(Model::DeGroot);
    let world = social_world(Some(social_row(&[], &[])), false);
    let refused = social::update(&world, &social_inputs(&settings, &expressiveness, &view, &deltas, &neighbors, false));
    assert_eq!(refused.err(), Some(unsupported("the deltas are not in id order")));
    let twice = [delta("a1", 0.1), delta("a1", 0.2), delta("a3", 0.3), delta("a4", 0.4)];
    let settings = settings_for(Model::Sparse);
    let refused = social::update(&world, &social_inputs(&settings, &expressiveness, &view, &twice, &neighbors, false));
    assert_eq!(refused.err(), Some(unsupported("two deltas for one person")));
}

fn settings_for(policy: Model) -> Settings {
    settings(policy)
}

#[test]
fn the_first_tick_plans_and_writes_once_the_namespace_is_there() {
    let mut world = social_world(None, false);
    let view = view_of(&world);
    let deltas = base_deltas();
    let (settings, expressiveness, neighbors) = (settings(Model::Sparse), social::Table::new(), neighbors());
    let mut planned = social::update(&world, &social_inputs(&settings, &expressiveness, &view, &deltas, &neighbors, false)).unwrap();
    assert!(planned.first_use && !planned.is_written());
    assert!(written(&mut world).is_empty());
    // Python stores the empty namespace; then the plan is written.
    world.slots.get(SOCIAL).unwrap().try_borrow_mut().unwrap().row = Some(Box::new(social_row(&[], &[])));
    social::write(&world, &mut planned).unwrap();
    assert!(planned.is_written() && !planned.first_use);
    assert_eq!(written(&mut world), [SOCIAL]);
    assert!(!world.row::<SocialRow>(SOCIAL).unwrap().contacts.is_empty());
    assert_eq!(social::write(&world, &mut planned).err(), Some(Error::Runtime("the plan is written already".to_owned())));
}

#[test]
fn what_the_step_turns_down_changes_nothing() {
    let deltas = base_deltas();
    let (settings, expressiveness) = (settings(Model::Sparse), social::Table::new());
    let neighbors = neighbors();
    let check = |mut world: World, deltas: &[Delta], neighbors: &Neighbors, reason: &str| {
        let view = view_of(&world);
        let refused = social::update(&world, &social_inputs(&settings, &expressiveness, &view, deltas, neighbors, false));
        assert_eq!(refused.err(), Some(unsupported(reason)));
        assert!(written(&mut world).is_empty(), "{reason}");
    };
    check(social_world(Some(social_row(&[("[\"a1\",\"a2\"]", -1, 1)], &[])), true), &deltas, &neighbors, "a contact field is negative");
    check(social_world(Some(social_row(&[], &[("a1", -3)])), true), &deltas, &neighbors, "a cooldown is negative");
    let mut other = social_row(&[], &[]);
    other.schema_version = 2;
    check(social_world(Some(other), true), &deltas, &neighbors, "another schema version");
    check(social_world(Some(social_row(&[], &[])), true), &deltas[1..], &neighbors, "the view and the deltas have different people");
    let mut outside = base_deltas();
    outside[2].stress = 1.5;
    check(social_world(Some(social_row(&[], &[])), true), &outside, &neighbors, "a stress is outside 0..1");
    // The zone neighbors are read only for the zones somebody is in.
    let mut bad = neighbors.clone().unwrap();
    bad.insert("z1".to_owned(), Err("the neighbors of a zone are not a tuple or a list"));
    check(social_world(Some(social_row(&[], &[])), true), &deltas, &Ok(bad), "the neighbors of a zone are not a tuple or a list");
    check(social_world(Some(social_row(&[], &[])), true), &deltas, &Err("the zone neighbors are not a dict"), "the zone neighbors are not a dict");
    // `social_network_v1` refuses before the step writes its own namespace.
    let world = social_world(Some(social_row(&[], &[])), false);
    world.slots.get(NETWORK).unwrap().try_borrow_mut().unwrap().row = Some(Box::new(network(i64::MAX - 1)));
    check(world, &deltas, &neighbors, "a count is too large");
    let world = social_world(Some(social_row(&[], &[])), false);
    world.slots.get(NETWORK).unwrap().try_borrow_mut().unwrap().taint = Some("a write that does not fit".to_owned());
    check(world, &deltas, &neighbors, "the namespace has no row");
}

#[test]
fn the_trace_of_a_minute_goes_to_the_host() {
    let world = social_world(Some(social_row(&[], &[])), false);
    let view = view_of(&world);
    let deltas = base_deltas();
    let (settings, expressiveness, neighbors) = (settings(Model::Channel), social::Table::new(), neighbors());
    let untraced = social::update(&world, &social_inputs(&settings, &expressiveness, &view, &deltas, &neighbors, false)).unwrap();
    let mut host = TestHost::default();
    social::record_social(&mut host, &untraced, &V::Int(10), &V::Str(text("expressiveness-channel-openness-v1"))).unwrap();
    assert!(host.calls.is_empty());

    let world = social_world(Some(social_row(&[], &[])), false);
    let traced = social::update(&world, &social_inputs(&settings, &expressiveness, &view, &deltas, &neighbors, true)).unwrap();
    // A trace changes no value of the plan.
    assert_eq!(traced.plan.stresses, untraced.plan.stresses);
    social::record_social(&mut host, &traced, &V::Int(10), &V::Str(text("expressiveness-channel-openness-v1"))).unwrap();
    let [Call::Trace(Trace::SocialContact(V::Map(document)))] = &host.calls[..] else {
        panic!("one social document: {:?}", host.calls);
    };
    let keys: Vec<&str> = document.keys().map(|key| &**key).collect();
    assert_eq!(
        keys,
        [
            "minute_index",
            "policy_version",
            "selected_contact_count",
            "contributing_contact_count",
            "total_stress_added",
            "aggregation",
            "truncated_contact_count",
            "contacts",
            "contributions"
        ]
    );
    assert_eq!(document["minute_index"], V::Int(10));
    assert_eq!(
        document["aggregation"],
        V::Str(text("snapshot weighted synchrony; pair cap then recipient cap then [0,1] bound"))
    );
    let trace = traced.plan.trace.as_ref().unwrap();
    assert_eq!(document["selected_contact_count"], V::Int(trace.contacts.len() as i64));
}

fn held(value: V) -> Result<Held, Fault> {
    Ok(Held::Value(value))
}

fn stress_of(class: Class, parameters: Vec<(&str, Result<Held, Fault>)>) -> Stress {
    let mut fields = Obj::new(Class::named("sheltersim.policies.social", "SocialPropagationParameters"));
    for (name, value) in parameters {
        fields.attrs.insert(text(name), value);
    }
    let mut policy = Obj::new(class);
    policy.attrs.insert(text("policy_version"), held(V::Str(text("sparse-family-zone-contact-v1"))));
    policy.attrs.insert(text("parameters"), Ok(Held::Object(Arc::new(fields))));
    let mut controller = Obj::new(Class::named("sheltersim.experiments.runner", "OfflineSimulationController"));
    controller.attrs.insert(text("stress_policy"), Ok(Held::Object(Arc::new(policy))));
    Stress::of(&controller)
}

fn parameters() -> Vec<(&'static str, Result<Held, Fault>)> {
    vec![
        ("enabled", held(V::Bool(true))),
        ("max_contacts_per_agent", held(V::Int(3))),
        ("minimum_contact_minutes", held(V::Int(5))),
        ("stress_spread_per_minute", held(V::Float(0.001))),
        ("natural_recovery_per_minute", held(V::Float(0.0005))),
        ("cooldown_minutes", held(V::Int(30))),
        ("max_pair_stress_increase", held(V::Int(0))),
        ("max_agent_stress_increase_per_minute", held(V::Float(0.05))),
        ("source_expressiveness_by_agent", held(V::Map(IndexMap::new()))),
        ("family_channel_strength", held(V::Int(1))),
        ("same_zone_channel_strength", held(V::Float(0.75))),
        ("nearby_zone_channel_strength", held(V::Float(0.5))),
    ]
}

#[test]
fn the_policy_is_read_as_the_kernel_read_it() {
    let sparse = Class::named("sheltersim.policies.social", "StressPropagationPolicy");
    let stress = stress_of(sparse.clone(), parameters());
    assert!(matches!(social::enabled(&stress), Ok(true)));
    let read = social::settings(&stress).unwrap();
    assert_eq!(read.policy, Model::Sparse);
    // An int cap stays an int; an int strength is its float.
    assert!(matches!(read.pair_cap, Num::Int(0)));
    assert_eq!(read.family_strength, 1.0);
    assert_eq!((read.maximum, read.minimum_contact_minutes, read.cooldown_minutes), (3, 5, 30));
    assert!(social::expressiveness(&stress).unwrap().is_empty());
    assert_eq!(social::policy_version(&stress).ok(), Some(V::Str(text("sparse-family-zone-contact-v1"))));

    let mut off = parameters();
    off[0].1 = held(V::Bool(false));
    assert!(matches!(social::enabled(&stress_of(sparse.clone(), off)), Ok(false)));

    let other = stress_of(Class::named("tests", "OtherPolicy"), parameters());
    let Err(Bad::Policy(fault)) = social::settings(&other) else { panic!("a refusal of the class") };
    assert_eq!(fault.refusal(), Some(UNKNOWN));

    let mut big = parameters();
    big[6].1 = held(V::Int(2));
    let Err(Bad::Refused(error)) = social::settings(&stress_of(sparse.clone(), big)) else { panic!("a refusal") };
    assert_eq!(error, unsupported("an int coefficient is outside 0..1"));

    let mut raising = parameters();
    raising[1].1 = Err(Fault::new(FaultClass::Other(text("builtins.AttributeError")), "no".to_owned()));
    let Err(Bad::Policy(fault)) = social::settings(&stress_of(sparse.clone(), raising)) else { panic!("a read to raise again") };
    assert_eq!(fault.path, ["stress_policy", "parameters", "max_contacts_per_agent"]);
    assert_eq!(fault.refusal(), None);

    let mut listed = parameters();
    listed[8].1 = held(V::List(Vec::new()));
    let Err(Bad::Refused(error)) = social::expressiveness(&stress_of(sparse, listed)) else { panic!("a refusal") };
    assert_eq!(error, unsupported("the expressiveness is not a dict"));
}

// ---- The information model -----------------------------------------------------------------------

fn belief(message: &str, status: &str, share: f64) -> BeliefRow {
    BeliefRow {
        message_id: text(message),
        belief: text(status),
        source: text("a1"),
        last_contact: 9,
        last_event_id: text("ev-origin"),
        exposed: true,
        belief_strength: 0.5,
        share_propensity: share,
        correction_exposed: false,
        correction_accepted: false,
        ever_misbelieved: false,
        ever_forgotten: false,
        last_correction_accepted: false,
    }
}

/// m1, heard by a1; a1 and a2 met last minute.
fn information_row() -> InformationRow {
    let mut messages = IdMap::default();
    messages.insert(
        text("m1"),
        MessageRow {
            id: text("m1"),
            truth_status: text("false"),
            source_id: text("src-1"),
            zone_id: text("z0"),
            time: 3,
            origin_event_id: text("ev-origin"),
            origin_agent_id: text("a1"),
        },
    );
    let mut beliefs: IdMap<IdMap<BeliefRow>> = IdMap::default();
    let mut pressure = IdMap::default();
    for person in ["a1", "a2", "a3", "a4"] {
        let mut held = IdMap::default();
        if person == "a1" {
            held.insert(text("m1"), belief("m1", "heard", 0.5));
        }
        beliefs.insert(text(person), held);
        pressure.insert(text(person), 0.25);
    }
    let mut contacts = IdMap::default();
    contacts.insert(text("[\"a1\",\"a2\"]"), ContactRow { last_seen_minute: 9, consecutive_minutes: 2 });
    InformationRow {
        schema_version: 2,
        messages,
        beliefs,
        contacts,
        query_pressure: pressure,
        support_requests: Vec::new(),
        counters: CountersRow { contact_attempts: 7, contact_deliveries: 0, correction_deliveries: 0, support_requests: 0 },
    }
}

fn information_world(row: InformationRow) -> World {
    let mut world = world();
    add_row(&mut world, INFORMATION, Place::Namespace, Some(row));
    written(&mut world);
    world
}

/// Every claim passes at once: a probability of 1 is never below a draw.
fn certain() -> InfoSettings {
    InfoSettings {
        maximum_contacts: 2,
        minimum_contact_minutes: 1,
        duration_scale: 1.0,
        base_probability: 1.0,
        correction_multiplier: 1.25,
        trust_sensitivity: 0.0,
        family_multiplier: 1.0,
        adjacent_multiplier: 1.0,
        pressure_decay: 0.125,
        distress_sensitivity: 0.5,
        share_decay: 0.0,
        forget_probability: 0.0,
    }
}

fn info_inputs<'a>(model: InfoModel, settings: &'a InfoSettings, neighbors: &'a HashMap<String, Vec<String>>, recording: bool) -> information::Inputs<'a> {
    information::Inputs { model, settings, minute: 10, seed: "7", neighbors, recording }
}

#[test]
fn a_minute_writes_the_row_and_schedules_each_transfer() {
    let mut world = information_world(information_row());
    let settings = certain();
    let neighbors = HashMap::new();
    let plan = information::sample(&world, &info_inputs(InfoModel::Sparse, &settings, &neighbors, true)).unwrap();
    assert_eq!(written(&mut world), [ID_COUNTERS, INFORMATION]);
    let row = world.row::<InformationRow>(INFORMATION).unwrap();
    // Each person's pressure decays; the contacts are this minute's, counted.
    assert!(row.query_pressure.values().all(|&pressure| pressure == 0.125));
    assert!(row.contacts.values().all(|contact| contact.last_seen_minute == 10));
    assert_eq!(row.counters.contact_attempts, 7 + row.contacts.len() as i64);
    // a1 and a2 met last minute too.
    if let Some(contact) = row.contacts.get("[\"a2\",\"a1\"]") {
        assert_eq!(contact.consecutive_minutes, 1);
    }
    drop(row);
    // Everyone in z0 who met a1 gets m1 next minute, one event each, in the order of the plan.
    let pending = world.queue().unwrap().pending_ids(None);
    assert_eq!(pending.len(), plan.proposed());
    assert!(!pending.is_empty());
    assert_eq!(&*pending[0], "information-contact-40");
    assert_eq!(world.id_counter("event").unwrap(), Some(40 + pending.len() as i64));
    // The trace keeps every contact with a full trace, 20 at most with a summary.
    let document = information::information_document(&plan, InfoModel::Sparse, &V::Int(10), TraceLevel::Full);
    let V::Map(document) = document else { panic!("a document") };
    let keys: Vec<&str> = document.keys().map(|key| &**key).collect();
    assert_eq!(
        keys,
        ["minute_index", "policy_version", "selected_contact_count", "truncated_contact_count", "contacts", "proposed_transfer_count"]
    );
    assert_eq!(document["policy_version"], V::Str(text("sparse-information-contact-v1")));
    assert_eq!(document["selected_contact_count"], V::Int(plan.traces.len() as i64));
    assert_eq!(document["proposed_transfer_count"], V::Int(pending.len() as i64));
    let V::List(contacts) = &document["contacts"] else { panic!("contacts") };
    let V::Map(first) = &contacts[0] else { panic!("a contact") };
    let keys: Vec<&str> = first.keys().map(|key| &**key).collect();
    assert_eq!(
        keys,
        [
            "sender_agent_id",
            "recipient_agent_id",
            "contact_basis",
            "contact_minutes",
            "message_id",
            "belief",
            "transmission_probability",
            "transmission_selected",
            "message_source_id",
            "causation_id"
        ]
    );
    let mut host = TestHost { level: TraceLevel::Summary, ..TestHost::default() };
    information::record_information(&mut host, &plan, InfoModel::Sparse, &V::Int(10)).unwrap();
    assert!(matches!(&host.calls[..], [Call::Trace(Trace::InformationSpread(_))]));
    let mut off = TestHost::default();
    information::record_information(&mut off, &plan, InfoModel::Sparse, &V::Int(10)).unwrap();
    assert!(off.calls.is_empty());
}

#[test]
fn an_evidence_aware_model_forgets_and_fades() {
    let mut world = information_world(information_row());
    let settings = InfoSettings { share_decay: 0.125, forget_probability: 1.0, ..certain() };
    let neighbors = HashMap::new();
    let plan = information::sample(&world, &info_inputs(InfoModel::EvidenceAware, &settings, &neighbors, false)).unwrap();
    written(&mut world);
    let row = world.row::<InformationRow>(INFORMATION).unwrap();
    let belief = &row.beliefs["a1"]["m1"];
    assert_eq!((&*belief.belief, belief.share_propensity, belief.ever_forgotten), ("unknown", 0.0, true));
    // A forgotten claim passes to no one.
    assert_eq!(plan.proposed(), 0);
    drop(row);
    let settings = InfoSettings { share_decay: 0.125, ..certain() };
    let mut world = information_world(information_row());
    information::sample(&world, &info_inputs(InfoModel::EvidenceAware, &settings, &neighbors, false)).unwrap();
    written(&mut world);
    let belief = world.row::<InformationRow>(INFORMATION).unwrap().beliefs["a1"]["m1"].clone();
    assert_eq!((&*belief.belief, belief.share_propensity, belief.ever_forgotten), ("heard", 0.375, false));
}

#[test]
fn what_the_sample_step_turns_down_changes_nothing() {
    let settings = certain();
    let neighbors = HashMap::new();
    let mut other = information_row();
    other.schema_version = 1;
    let mut world = information_world(other);
    let refused = information::sample(&world, &info_inputs(InfoModel::Sparse, &settings, &neighbors, false));
    assert!(matches!(refused, Err(WriteError::Error(error)) if error == unsupported("the state has another schema")));
    assert!(written(&mut world).is_empty());
    let mut missing = information_row();
    missing.query_pressure.shift_remove("a3");
    let mut world = information_world(missing);
    let refused = information::sample(&world, &info_inputs(InfoModel::Sparse, &settings, &neighbors, false));
    assert!(matches!(refused, Err(WriteError::Error(error)) if error == unsupported("a person has no beliefs or no query pressure")));
    assert!(written(&mut world).is_empty());
    let mut world = information_world(information_row());
    world.queue_held = false;
    let refused = information::sample(&world, &info_inputs(InfoModel::Sparse, &settings, &neighbors, false));
    assert!(matches!(refused, Err(WriteError::Error(error)) if error == unsupported("the event queue is not in Rust")));
    assert!(written(&mut world).is_empty());
}

fn config(fields: Vec<(&str, Result<Held, Fault>)>) -> Information {
    let mut config = Obj::new(Class::named("sheltersim.domain.models", "InformationSpreadConfig"));
    for (name, value) in fields {
        config.attrs.insert(text(name), value);
    }
    let mut policy = Obj::new(Class::named("sheltersim.policies.information", "EvidenceAwareInformationPolicy"));
    policy.attrs.insert(text("policy_version"), held(V::Str(text("evidence-aware-rumor-v1"))));
    policy.attrs.insert(text("config"), Ok(Held::Object(Arc::new(config))));
    let mut controller = Obj::new(Class::named("sheltersim.experiments.runner", "OfflineSimulationController"));
    controller.attrs.insert(text("information_policy"), Ok(Held::Object(Arc::new(policy))));
    Information::of(&controller)
}

fn config_fields() -> Vec<(&'static str, Result<Held, Fault>)> {
    vec![
        ("enabled", held(V::Bool(true))),
        ("max_contacts_per_agent", held(V::Int(2))),
        ("minimum_contact_minutes", held(V::Int(2))),
        ("contact_duration_scale_minutes", held(V::Int(5))),
        ("base_transmission_probability", held(V::Float(0.2))),
        ("correction_transmission_multiplier", held(V::Float(1.25))),
        ("trust_sensitivity", held(V::Float(0.5))),
        ("family_contact_multiplier", held(V::Float(1.5))),
        ("adjacent_zone_contact_multiplier", held(V::Float(0.5))),
        ("query_pressure_decay_per_minute", held(V::Float(0.002))),
        ("distress_sharing_sensitivity", held(V::Float(0.5))),
        ("share_propensity_decay_per_minute", held(V::Float(0.002))),
        ("forgetting_rate_per_minute", held(V::Int(1))),
    ]
}

#[test]
fn the_config_is_read_as_the_kernel_read_it() {
    let information = config(config_fields());
    assert_eq!(information::model_of(&information).ok(), Some(Some(InfoModel::EvidenceAware)));
    let read = information::settings(&information, InfoModel::EvidenceAware).unwrap();
    assert_eq!(read.forget_probability.to_bits(), (-expm1(-1.0).unwrap()).to_bits());
    assert_eq!((read.maximum_contacts, read.duration_scale), (2, 5.0));
    // Only the evidence-aware model forgets.
    assert_eq!(information::settings(&information, InfoModel::Sparse).unwrap().forget_probability, 0.0);

    let mut off = config_fields();
    off[0].1 = held(V::Int(1));
    let Err(Bad::Refused(error)) = information::settings(&config(off), InfoModel::Sparse) else { panic!("a refusal") };
    assert_eq!(error, unsupported("the model is not enabled"));
    let mut wide = config_fields();
    wide[1].1 = held(V::Int(17));
    let Err(Bad::Refused(error)) = information::settings(&config(wide), InfoModel::Sparse) else { panic!("a refusal") };
    assert_eq!(error, unsupported("a contact setting is out of range"));
    let mut raising = config_fields();
    raising[4].1 = Err(Fault::new(FaultClass::Other(text("builtins.AttributeError")), "no".to_owned()));
    let Err(Bad::Policy(fault)) = information::settings(&config(raising), InfoModel::Sparse) else { panic!("a read to raise again") };
    assert_eq!(fault.path, ["information_policy", "config", "base_transmission_probability"]);

    let mut controller = Obj::new(Class::named("sheltersim.experiments.runner", "OfflineSimulationController"));
    controller.attrs.insert(text("information_policy"), held(V::Null));
    assert_eq!(information::model_of(&Information::of(&controller)).ok(), Some(None));
}

#[test]
fn a_probability_keeps_the_order_of_the_python_expression() {
    let settings = InfoSettings { duration_scale: 5.0, base_probability: 0.3, trust_sensitivity: 0.5, family_multiplier: 1.5, ..certain() };
    let plain = information::transmission_probability(&settings, InfoModel::Sparse, 0.75, Basis::Family, 3, true).unwrap();
    let expected = py_min(1.0, 0.3 * py_min(1.0, 3.0 / 5.0) * (1.0 + 0.5 * (2.0 * 0.75 - 1.0)) * 1.5 * 1.25);
    assert_eq!(plain.to_bits(), expected.to_bits());
    let hazard = information::transmission_probability(&settings, InfoModel::Hazard, 0.75, Basis::Family, 3, true).unwrap();
    assert_eq!(hazard.to_bits(), (-expm1(-expected).unwrap()).to_bits());
}
