//! Lane P8 a2: the leaves of the people rules in the core (`rules::agent_dynamics`,
//! `action_lifecycle`, `body_state`, `social_network`, `social_dialogue`), on worlds built here.
//! The PyO3 crate's files of the same names are glue over these, so the gate and the offline tests
//! run them on every Run; these say what each does with no Python.

use std::sync::Arc;

use indexmap::IndexMap;
use serde_json::{json, Value};
use sheltersim_core::config::agent_dynamics::Awareness;
use sheltersim_core::config::body::{AffectModel, BodyModels, TrustModel};
use sheltersim_core::config::{Fault, FaultClass, Selection};
use sheltersim_core::entities::{Activity, AgentRow, AnyTable, Entities, FacilityRow, Mobility, NeedRow, ProfileRow, Table};
use sheltersim_core::error::Error;
use sheltersim_core::events::log::EventLog;
use sheltersim_core::events::queue::Queue;
use sheltersim_core::events::{EventKind, EventRow};
use sheltersim_core::row::{IdMap, Row, ValueRow};
use sheltersim_core::rules::action_lifecycle::{self as lifecycle, ActiveActions, Proposal};
use sheltersim_core::rules::agent_dynamics::{self as dynamics, Decision};
use sheltersim_core::rules::body_state::{self as body, SocialEvent};
use sheltersim_core::rules::kit::Kept;
use sheltersim_core::rules::social_dialogue as dialogue;
use sheltersim_core::rules::social_network as network;
use sheltersim_core::state::agent_dynamics::{AgentDynamicsRow, AgentMemory, POLICY_V1, POLICY_V2, POLICY_V3};
use sheltersim_core::state::body_state::{BodyRecord, BodyRow, History, Relationships as BodyRelationships};
use sheltersim_core::state::social_network::{Relationships, SocialNetworkRow};
use sheltersim_core::value::V;
use sheltersim_core::world::{Fit, Place, World, ID_COUNTERS};

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

fn map(entries: &[(&str, V)]) -> V {
    V::Map(entries.iter().map(|(key, value)| (text(key), value.clone())).collect())
}

fn string(value: &str) -> V {
    V::Str(text(value))
}

fn unsupported(reason: &str) -> Error {
    Error::Unsupported(reason.to_owned().into())
}

fn needs(value: f64) -> NeedRow {
    NeedRow { toilet_urgency: value, hunger: value, fatigue: value, hydration_need: value }
}

fn agent(id: &str, zone: &str) -> AgentRow {
    AgentRow {
        profile: ProfileRow {
            id: text(id),
            group_id: None,
            initial_zone_id: text(zone),
            mobility: Mobility::Independent,
            support_need: 0.0,
            stress_sensitivity: 1.0,
            rng_key: text(id),
            initial_needs: needs(0.0),
            provenance: V::Map(IndexMap::new()),
        },
        zone_id: text(zone),
        activity: Activity::Idle,
        target_id: None,
        needs: needs(0.25),
        stress: 0.5,
        trust: 0.75,
        next_decision_at: 7,
        support_request_id: None,
        support_cooldown_until: 0,
    }
}

fn facility(id: &str, kind: &str, zone: &str) -> FacilityRow {
    FacilityRow {
        id: text(id),
        kind: text(kind),
        zone_id: text(zone),
        capacity: 1,
        working_units: 1,
        active_agent_ids: Vec::new(),
        queue_agent_ids: Vec::new(),
        resource_active_agent_ids: Vec::new(),
        resource_queue_agent_ids: Vec::new(),
    }
}

fn put<R: Clone>(table: &mut Table<R>, id: &str, row: R) {
    let slot = table.reserve_slot();
    table.fill_slot(slot, row);
    assert_eq!(table.bind(text(id), slot), None);
}

/// Agents a2 and a1 (in that order) in z0, a3 in z1; f1 a toilet in z0, f2 an intake desk and f3 a
/// store in z1. A world of 100 minutes at minute 10 that holds its queue, log and event counter.
fn world() -> World {
    let mut entities = Entities::default();
    put(&mut entities.agents, "a2", agent("a2", "z0"));
    put(&mut entities.agents, "a1", agent("a1", "z0"));
    put(&mut entities.agents, "a3", agent("a3", "z1"));
    put(&mut entities.facilities, "f1", facility("f1", "toilet", "z0"));
    put(&mut entities.facilities, "f2", facility("f2", "intake", "z1"));
    put(&mut entities.facilities, "f3", facility("f3", "store", "z1"));
    let mut world = World::default();
    *world.entities.get_mut() = entities;
    let scalars = world.scalars.get_mut();
    scalars.duration_minutes = Fit::Ok(100);
    scalars.minute_index = Fit::Ok(10);
    add_row(&mut world, ID_COUNTERS, Place::Field, ValueRow(map(&[("event", V::Int(5))])));
    world.hold_queue(Arc::new(Queue::new()));
    world.hold_log(Arc::new(EventLog::new()));
    world
}

fn add_row<T: Row>(world: &mut World, name: &str, place: Place, row: T) {
    let index = world.slots.add(text(name), place).unwrap();
    world.slots.at(index).unwrap().try_borrow_mut().unwrap().row = Some(Box::new(row));
}

fn written(world: &mut World) -> Vec<String> {
    let names: Vec<String> = world.slots.written_names().iter().map(|name| name.to_string()).collect();
    world.slots.take_written();
    names
}

fn counter(world: &World) -> V {
    world.row::<ValueRow>(ID_COUNTERS).unwrap().0.clone()
}

/// The records of the log, as JSON.
fn records(world: &World) -> Vec<Value> {
    let log = world.log().unwrap();
    (0..log.len()).map(|index| serde_json::from_slice(log.written(index).unwrap()).unwrap()).collect()
}

fn refused(reason: &str) -> Fault {
    Fault::new(FaultClass::Refused, reason)
}

// ---- agent_dynamics ----------------------------------------------------------------------------

const DYNAMICS: &str = "agent_dynamics_v1";

fn memory(known: &[&str]) -> AgentMemory {
    AgentMemory {
        known_facility_ids: known.iter().map(|id| text(id)).collect(),
        last_action: None,
        last_target_id: None,
        last_decision_minute: None,
        decision_due_minute: 7,
        decision_reason: None,
    }
}

fn with_dynamics(policy: &str, records: &[(&str, &[&str])]) -> World {
    let mut world = world();
    let mut agents = IdMap::default();
    for (id, known) in records {
        agents.insert(text(id), memory(known));
    }
    add_row(&mut world, DYNAMICS, Place::Namespace, AgentDynamicsRow { schema_version: 1, policy_version: text(policy), agents });
    world
}

fn known(world: &World, id: &str) -> Vec<String> {
    let row = world.row::<AgentDynamicsRow>(DYNAMICS).unwrap();
    row.agents.get(id).unwrap().known_facility_ids.iter().map(|id| id.to_string()).collect()
}

#[test]
fn a_new_state_has_each_agent_in_id_order_with_what_its_policy_and_awareness_give() {
    let world = world();
    let row = dynamics::create_state(&world, POLICY_V1, None).unwrap();
    assert_eq!(row.agents.keys().map(|id| &**id).collect::<Vec<_>>(), ["a1", "a2", "a3"]);
    assert_eq!(row.agents["a1"].known_facility_ids, [text("f1")]);
    assert_eq!(row.agents["a3"].known_facility_ids, [text("f2"), text("f3")]);
    assert_eq!(row.agents["a1"].decision_due_minute, 7);
    // An orienting policy adds the facilities of the orientation kinds; the awareness its own.
    let awareness = Awareness { initial: vec![text("f3")], discover: false };
    let row = dynamics::create_state(&world, POLICY_V2, Some(&awareness)).unwrap();
    assert_eq!(row.agents["a1"].known_facility_ids, [text("f1"), text("f2"), text("f3")]);
    assert_eq!(&*row.policy_version, POLICY_V2);
}

#[test]
fn a_decision_is_remembered_in_the_row_and_marks_it() {
    let mut world = with_dynamics(POLICY_V1, &[("a1", &["f1"])]);
    written(&mut world);
    let decision =
        Decision { agent_id: "a1", action: "rest", target_id: Some("f1"), minute: 10, next_decision_at: 15, trigger: None };
    dynamics::remember_decision(&world, &decision).unwrap();
    {
        let row = world.row::<AgentDynamicsRow>(DYNAMICS).unwrap();
        let record = &row.agents["a1"];
        assert_eq!(record.last_action.as_deref(), Some("rest"));
        assert_eq!(record.last_target_id.as_deref(), Some("f1"));
        assert_eq!((record.last_decision_minute, record.decision_due_minute, record.decision_reason.as_deref()), (Some(10), 15, None));
    }
    assert_eq!(written(&mut world), [DYNAMICS]);
    // An agent without a record has nothing to remember, and nothing is marked.
    dynamics::remember_decision(&world, &Decision { agent_id: "a2", ..decision }).unwrap();
    assert!(written(&mut world).is_empty());
    // Without the namespace's row, for the Python rule to create it.
    assert_eq!(dynamics::remember_decision(&self::world(), &decision), Err(unsupported("the namespace is not owned")));
}

#[test]
fn an_agent_learns_a_facility_and_with_discovery_its_zone() {
    let mut world = with_dynamics(POLICY_V1, &[("a1", &["f1"])]);
    written(&mut world);
    let plain: Kept<Option<Awareness>> = Ok(Arc::new(None));
    // A list that comes out the same is not written.
    dynamics::learn_facility(&world, Some("a1"), Some("f1"), &plain).unwrap();
    assert!(written(&mut world).is_empty());
    dynamics::learn_facility(&world, Some("a1"), Some("f3"), &plain).unwrap();
    assert_eq!(known(&world, "a1"), ["f1", "f3"]);
    assert_eq!(written(&mut world), [DYNAMICS]);
    let discover: Kept<Option<Awareness>> = Ok(Arc::new(Some(Awareness { initial: Vec::new(), discover: true })));
    dynamics::learn_facility(&world, Some("a1"), Some("f2"), &discover).unwrap();
    assert_eq!(known(&world, "a1"), ["f1", "f2", "f3"]);
    // The config is read only for an agent with a record, as the Python rule reads it.
    let bad: Kept<Option<Awareness>> = Err(refused("a bad awareness"));
    dynamics::learn_facility(&world, Some("a2"), Some("f2"), &bad).unwrap();
    assert_eq!(dynamics::learn_facility(&world, Some("a1"), Some("f2"), &bad), Err(unsupported("a bad awareness")));
    // An ID that is not a str is refused only past the namespace's absence.
    assert_eq!(dynamics::learn_facility(&self::world(), None, None, &bad), Ok(()));
    assert_eq!(dynamics::learn_facility(&world, None, Some("f1"), &plain), Err(unsupported("a value is not a str")));
    let corrupt: Kept<Option<Awareness>> = Err(Fault::new(FaultClass::Value, "raised"));
    assert!(matches!(dynamics::learn_facility(&world, Some("a1"), Some("f2"), &corrupt), Err(Error::Corrupt { .. })));
}

#[test]
fn knowing_and_announcing_follow_the_policy() {
    let world = with_dynamics(POLICY_V3, &[("a1", &["f1"]), ("a3", &["f2", "f3"])]);
    let limited: Kept<bool> = Ok(Arc::new(true));
    assert_eq!(dynamics::knows_facility(&world, &Ok(Arc::new(false)), None, None), Ok(true));
    assert_eq!(dynamics::knows_facility(&world, &limited, Some("a1"), Some("f1")), Ok(true));
    assert_eq!(dynamics::knows_facility(&world, &limited, Some("a1"), Some("f2")), Ok(false));
    assert_eq!(dynamics::knows_facility(&self::world(), &limited, Some("a1"), Some("f1")), Ok(false));
    let policy: Kept<&'static str> = Ok(Arc::new(POLICY_V3));
    dynamics::announce_installed_facility(&world, Some("f2"), &policy).unwrap();
    assert_eq!(known(&world, "a1"), ["f1", "f2"]);
    assert_eq!(known(&world, "a3"), ["f2", "f3"]);
    // A store is no orientation kind: the policy is not even read.
    dynamics::announce_installed_facility(&world, Some("f3"), &Err(refused("bad"))).unwrap();
    assert_eq!(known(&world, "a1"), ["f1", "f2"]);
}

#[test]
fn an_added_agent_gets_a_record_where_its_configs_are_good() {
    let world = with_dynamics(POLICY_V1, &[("a1", &["f1"])]);
    let policy: Kept<&'static str> = Ok(Arc::new(POLICY_V1));
    let awareness: Kept<Option<Awareness>> = Ok(Arc::new(None));
    dynamics::add_agent(&world, Some("a3"), &policy, &awareness).unwrap();
    assert_eq!(known(&world, "a3"), ["f2", "f3"]);
    // Nobody to add: the configs are not read.
    dynamics::add_agent(&world, Some("nobody"), &Err(refused("bad")), &awareness).unwrap();
    assert_eq!(dynamics::check_add_agent(&world, &Err(refused("bad")), &awareness), Err(unsupported("bad")));
    assert_eq!(dynamics::check_add_agent(&world, &policy, &awareness), Ok(()));
}

// ---- action_lifecycle ----------------------------------------------------------------------------

fn proposal<'a>(agent: &'a str, action: &'a str, target: Option<&'a str>, minute: i64) -> Proposal<'a> {
    Proposal { agent_id: agent, action, target_id: target, minute, trigger: "need" }
}

fn payload_of(record: &Value) -> &Value {
    &record["event"]["payload"]
}

#[test]
fn an_action_starts_continues_once_and_is_superseded_with_its_receipts_in_sequence() {
    let mut world = world();
    let mut active = ActiveActions::default();
    lifecycle::commit_action(&world, &mut active, &proposal("a1", "rest", None, 10)).unwrap();
    assert_eq!(counter(&world), map(&[("event", V::Int(6))]));
    let started = &records(&world)[0];
    assert_eq!(started["event"]["event_id"], json!("action-lifecycle-5"));
    assert_eq!(started["detail"], json!("autonomous action started"));
    let payload = payload_of(started);
    assert_eq!((payload["action_id"].clone(), payload["transition"].clone()), (json!("action-5"), json!("started")));
    assert_eq!(payload["start_activity"], json!("idle"));
    assert_eq!(payload["start_needs"]["fatigue"], json!(0.25));
    assert_eq!(payload["current_needs"]["sleep_debt_minutes"], json!(0.0));
    assert!(active.contains("a1") && active.len() == 1);

    lifecycle::commit_action(&world, &mut active, &proposal("a1", "rest", None, 11)).unwrap();
    lifecycle::commit_action(&world, &mut active, &proposal("a1", "rest", None, 12)).unwrap();
    assert_eq!(records(&world).len(), 2);
    assert_eq!(payload_of(&records(&world)[1])["transition"], json!("continued"));
    assert!(active.rows["a1"].continued);

    // Another action: the old one closes "interrupted" (fatigue did not fall), then the new starts.
    lifecycle::commit_action(&world, &mut active, &proposal("a1", "wait", None, 13)).unwrap();
    let all = records(&world);
    assert_eq!(all.len(), 4);
    assert_eq!(payload_of(&all[2])["transition"], json!("interrupted"));
    assert_eq!(payload_of(&all[2])["reason_code"], json!("superseded_by_new_applied_action"));
    assert_eq!(payload_of(&all[3])["action_id"], json!("action-8"));
    assert_eq!(&*active.rows["a1"].action, "wait");
    // An agent not in the world: nothing.
    lifecycle::commit_action(&world, &mut active, &proposal("nobody", "rest", None, 13)).unwrap();
    assert_eq!(records(&world).len(), 4);
    let _ = written(&mut world);
}

#[test]
fn a_receipt_the_model_has_to_decide_is_refused_with_nothing_changed() {
    let world = world();
    let mut active = ActiveActions::default();
    let long = "x".repeat(121);
    let refused = lifecycle::commit_action(&world, &mut active, &Proposal { trigger: &long, ..proposal("a1", "rest", None, 10) });
    assert_eq!(refused, Err(unsupported("a receipt the model has to decide")));
    assert!(records(&world).is_empty() && active.is_empty());
    assert_eq!(counter(&world), map(&[("event", V::Int(5))]));
}

fn ending_event(event_type: &str, agent: &str, payload: &[(&str, V)], due: i64) -> EventRow {
    let mut fields: IndexMap<Arc<str>, V> = IndexMap::new();
    fields.insert(text("agent_id"), string(agent));
    for (key, value) in payload {
        fields.insert(text(key), value.clone());
    }
    EventRow::new(text("e-1"), EventKind::Endogenous, due, 0, 1, text(event_type), fields, None, None, IndexMap::new()).unwrap()
}

#[test]
fn an_applied_event_ends_the_action_it_serves_and_no_other() {
    let world = world();
    let mut active = ActiveActions::default();
    lifecycle::commit_action(&world, &mut active, &proposal("a1", "request_toilet", Some("f1"), 10)).unwrap();
    // Another target, an event before the start, a type that ends nothing: nothing.
    let other = ending_event("toilet_service_complete", "a1", &[("facility_id", string("f9"))], 11);
    lifecycle::complete_for_event(&world, &mut active, &other, 11).unwrap();
    let early = ending_event("toilet_service_complete", "a1", &[("facility_id", string("f1"))], 9);
    lifecycle::complete_for_event(&world, &mut active, &early, 11).unwrap();
    let travel = ending_event("agent_travel_complete", "a1", &[], 11);
    lifecycle::complete_for_event(&world, &mut active, &travel, 11).unwrap();
    assert_eq!(records(&world).len(), 1);
    let done = ending_event("toilet_service_complete", "a1", &[("facility_id", string("f1"))], 11);
    lifecycle::complete_for_event(&world, &mut active, &done, 12).unwrap();
    let receipt = &records(&world)[1];
    assert_eq!(payload_of(receipt)["transition"], json!("completed"));
    assert_eq!(payload_of(receipt)["reason_code"], json!("toilet_service_complete"));
    assert_eq!(receipt["event"]["causation_id"], json!("e-1"));
    assert!(active.is_empty());
}

#[test]
fn at_the_end_every_open_action_closes_in_agent_order() {
    let mut world = world();
    let mut active = ActiveActions::default();
    lifecycle::commit_action(&world, &mut active, &proposal("a3", "rest", None, 10)).unwrap();
    lifecycle::commit_action(&world, &mut active, &proposal("a1", "wait", None, 3)).unwrap();
    // a3 left the world: censored.
    world.entities.get_mut().agents.remove_row("a3");
    lifecycle::finalize(&world, &mut active, 20).unwrap();
    let all = records(&world);
    assert_eq!(all.len(), 4);
    assert_eq!((payload_of(&all[2])["agent_id"].clone(), payload_of(&all[2])["transition"].clone()), (json!("a1"), json!("completed")));
    assert_eq!((payload_of(&all[3])["agent_id"].clone(), payload_of(&all[3])["transition"].clone()), (json!("a3"), json!("censored")));
    assert_eq!(payload_of(&all[3])["current_needs"], json!({}));
    assert!(active.is_empty());
}

#[test]
fn a_population_event_interrupts_the_actions_of_those_it_took_away() {
    let mut world = world();
    let history = map(&[(
        "history",
        V::List(vec![map(&[("event_id", string("pop-1")), ("action", string("temporary_exit")), ("agent_id", string("a3"))])]),
    )]);
    add_row(&mut world, "population_lifecycle_v1", Place::Value, ValueRow(history));
    let mut active = ActiveActions::default();
    lifecycle::commit_action(&world, &mut active, &proposal("a3", "rest", None, 10)).unwrap();
    lifecycle::commit_action(&world, &mut active, &proposal("a1", "rest", None, 10)).unwrap();
    lifecycle::population_exit(&world, &mut active, "pop-1", 11).unwrap();
    let receipt = &records(&world)[2];
    assert_eq!(payload_of(receipt)["reason_code"], json!("population_temporary_exit"));
    assert_eq!(payload_of(receipt)["activity"], json!(null));
    assert!(!active.contains("a3") && active.contains("a1"));
}

#[test]
fn active_actions_must_be_under_their_own_agents() {
    let mut world = world();
    let mut active = ActiveActions::default();
    lifecycle::commit_action(&world, &mut active, &proposal("a1", "rest", None, 10)).unwrap();
    let mut rows = active.rows.clone();
    let action = rows.shift_remove("a1").unwrap();
    rows.insert(text("a2"), action);
    assert!(matches!(ActiveActions::from_rows(rows), Err(Error::Unsupported(_))));
    let _ = written(&mut world);
}

// ---- body_state ----------------------------------------------------------------------------------

const BODY: &str = "body_state_v1";

fn body_record(positive: f64, negative: f64) -> BodyRecord {
    BodyRecord {
        sleep_debt_minutes: 0.0,
        uncertainty: 0.5,
        reassurance: 0.5,
        discontent: 0.25,
        positive_affect: positive,
        negative_affect: negative,
        hygiene_need: 0.0,
    }
}

fn body_config() -> V {
    map(&[
        ("relationship_trust_gain", V::Float(0.1)),
        ("relationship_trust_decay_per_day", V::Float(0.0)),
        ("affect_transfer_rate", V::Float(0.5)),
        ("social_action_affect_transfer", V::Float(0.2)),
        ("social_interaction_minutes", V::Int(5)),
    ])
}

fn with_body(records: &[(&str, BodyRecord)]) -> World {
    let mut world = world();
    let mut agents = IdMap::default();
    for (id, record) in records {
        agents.insert(text(id), *record);
    }
    let row = BodyRow {
        schema_version: 1,
        policy_version: text("life-state-feedback-v1"),
        source_id: text("ASM-LIFE-STATE-FEEDBACK-V1"),
        config: body_config(),
        agents,
        relationships: BodyRelationships::from_map(IdMap::default()),
        history: History::default(),
    };
    add_row(&mut world, BODY, Place::Namespace, row);
    world
}

const LINEAR: BodyModels = BodyModels { affect: AffectModel::Linear, trust: TrustModel::DurationOnly };

fn social<'a>(event_type: &'a str, payload: &'a V) -> SocialEvent<'a> {
    SocialEvent { event_id: "e-7", event_type, causation_id: None, payload }
}

#[test]
fn a_contact_moves_the_recipient_s_affect_and_records_the_relationship_and_the_history() {
    let mut world = with_body(&[("a1", body_record(0.8, 0.1)), ("a2", body_record(0.4, 0.3))]);
    let payload = map(&[("sender_agent_id", string("a1")), ("recipient_agent_id", string("a2")), ("contact_minutes", V::Int(10))]);
    let selection = Selection::default();
    body::record_social_event(&world, &social("information_contact_update", &payload), 10, &selection, &LINEAR).unwrap();
    assert_eq!(written(&mut world), [BODY]);
    let row = world.row::<BodyRow>(BODY).unwrap();
    // The bounded difference at rate 0.5: +0.2 positive, -0.1 negative, then the reassurance.
    let target = row.agents["a2"];
    assert_eq!(target.positive_affect, 0.4 + 0.2);
    let negative = (0.1 - 0.3) * 0.5;
    assert_eq!(target.negative_affect, 0.3 + negative);
    assert_eq!(target.reassurance, 0.5 + (0.2 - negative * 0.25));
    assert_eq!(row.agents["a1"].positive_affect, 0.8);
    // The relationship of (recipient, sender): duration 10 of 5 minutes gives the whole gain.
    let relationship = &row.relationships.map["[\"a2\",\"a1\"]"];
    assert_eq!(relationship.trust, Some(0.5 + 0.1));
    assert_eq!((relationship.contact_count, relationship.contact_minutes, relationship.last_contact_minute), (1, 10, Some(10)));
    assert_eq!(relationship.last_interaction.as_deref(), Some("information_contact"));
    assert_eq!(row.relationships.written, [text("[\"a2\",\"a1\"]")]);
    // The transfer's item comes after the relationship's, each its canonical JSON.
    let items: Vec<Value> = row.history.items.iter().map(|item| serde_json::from_slice(item).unwrap()).collect();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["kind"], json!("relationship_update"));
    assert_eq!(items[0]["actor_ids"], json!(["a1", "a2"]));
    assert_eq!(items[0]["deltas"]["relationship_policy_id"], json!("duration-contact-trust-v1"));
    assert_eq!(items[1]["kind"], json!("information_contact"));
    assert_eq!(items[1]["deltas"]["contact_duration_minutes"], json!(10.0));
    assert_eq!(items[1]["event_id"], json!("e-7"));
    let first = std::str::from_utf8(&row.history.items[0]).unwrap();
    assert!(first.starts_with("{\"actor_ids\":[\"a1\",\"a2\"],\"causation_id\":null,\"deltas\":{"), "{first}");
}

#[test]
fn a_person_without_a_body_record_is_refused_with_nothing_written() {
    let mut world = with_body(&[("a1", body_record(0.8, 0.1))]);
    let payload = map(&[("sender_agent_id", string("a1")), ("recipient_agent_id", string("a2"))]);
    let refused = body::record_social_event(&world, &social("information_contact_update", &payload), 10, &Selection::default(), &LINEAR);
    assert_eq!(refused, Err(unsupported("a person has no body record")));
    assert!(written(&mut world).is_empty());
    // Without the namespace, or for another event, nothing at all.
    body::record_social_event(&self::world(), &social("information_contact_update", &payload), 10, &Selection::default(), &LINEAR).unwrap();
    body::record_social_event(&world, &social("agent_travel_complete", &payload), 10, &Selection::default(), &LINEAR).unwrap();
    assert!(written(&mut world).is_empty());
}

// ---- social_network ------------------------------------------------------------------------------

const NETWORK: &str = "social_network_v1";

fn with_network(body: &[(&str, BodyRecord)]) -> World {
    let mut world = with_body(body);
    let row = SocialNetworkRow {
        schema_version: 1,
        policy_version: text("directed-contact-relationships-v1"),
        relationships: Relationships::from_map(IdMap::default()),
        interaction_count: 0,
        information_transfer_count: 0,
    };
    add_row(&mut world, NETWORK, Place::Namespace, row);
    world
}

#[test]
fn a_contact_counts_once_and_shares_its_message_once() {
    let world = with_network(&[]);
    let payload = map(&[("recipient_agent_id", string("a2")), ("sender_agent_id", string("a1")), ("message_id", string("m1"))]);
    network::observe_event(&world, "information_contact_update", &payload, 10).unwrap();
    network::observe_event(&world, "information_contact_update", &payload, 11).unwrap();
    let row = world.row::<SocialNetworkRow>(NETWORK).unwrap();
    let record = &row.relationships.map["[\"a2\",\"a1\"]"];
    assert_eq!((record.contact_count, record.conversation_count, record.last_contact_minute), (2, 2, Some(11)));
    assert_eq!(record.shared_message_ids, [text("m1")]);
    assert_eq!(record.trust, None);
    assert_eq!((row.interaction_count, row.information_transfer_count), (2, 1));
}

#[test]
fn a_social_action_moves_the_directed_trust_by_the_actor_s_relief() {
    let world = with_network(&[("a1", body_record(0.9, 0.1))]);
    let payload = map(&[("interaction_type", string("family_care")), ("agent_id", string("a1")), ("participant_agent_id", string("a2"))]);
    network::observe_event(&world, "social_interaction", &payload, 10).unwrap();
    let relief = network::relief(0.9, 0.1, 0.2);
    assert_eq!(relief, 0.2 * (0.9 - 0.1));
    let row = world.row::<SocialNetworkRow>(NETWORK).unwrap();
    assert_eq!(row.relationships.map["[\"a1\",\"a2\"]"].trust, Some(0.5 + relief * 0.25));
    assert_eq!(row.relationships.map["[\"a2\",\"a1\"]"].trust, Some(0.5 + relief * 0.5));
    assert_eq!(row.interaction_count, 1);
    // What the rule skips: one person with themselves, an interaction type that is no str.
    drop(row);
    let same = map(&[("interaction_type", string("cooperation")), ("agent_id", string("a1")), ("participant_agent_id", string("a1"))]);
    network::observe_event(&world, "social_interaction", &same, 10).unwrap();
    let number = map(&[("interaction_type", V::Int(3))]);
    network::observe_event(&world, "social_interaction", &number, 10).unwrap();
    assert_eq!(world.row::<SocialNetworkRow>(NETWORK).unwrap().interaction_count, 1);
    let listed = map(&[("interaction_type", V::List(Vec::new()))]);
    assert_eq!(network::observe_event(&world, "social_interaction", &listed, 10), Err(unsupported("a value is not a str")));
}

#[test]
fn an_id_json_escapes_is_refused_and_a_body_s_trust_is_copied() {
    let world = with_network(&[]);
    let escaped = map(&[("recipient_agent_id", string("あ")), ("sender_agent_id", string("a1"))]);
    assert_eq!(
        network::observe_event(&world, "information_contact_update", &escaped, 10),
        Err(unsupported("an ID has a character that JSON escapes"))
    );
    assert!(world.row::<SocialNetworkRow>(NETWORK).unwrap().relationships.map.is_empty());
    assert_eq!(network::action_relief(None, "a1"), Ok(0.02 * 0.5));
    network::record_observed_contacts(&world, &[("a1", "a2"), ("a2", "a1")], 9).unwrap();
    let row = world.row::<SocialNetworkRow>(NETWORK).unwrap();
    assert_eq!(row.relationships.map.len(), 2);
    assert_eq!(row.interaction_count, 2);
    drop(row);
    assert_eq!(
        network::record_observed_contacts(&world, &[("a1", "a1")], 9),
        Err(unsupported("a pair the kernel leaves to Python"))
    );
}

// ---- social_dialogue -----------------------------------------------------------------------------

fn state(stress: V, affect: V) -> V {
    let needs = map(&[
        ("toilet_urgency", V::Float(0.1)),
        ("hunger", V::Float(0.1)),
        ("fatigue", V::Int(0)),
        ("hydration_need", V::Float(1.0)),
    ]);
    map(&[("stress", stress), ("trust", V::Float(0.5)), ("needs", needs), ("affect", affect)])
}

fn context(captured: V, participants: &[(&str, V)]) -> V {
    map(&[("schema_version", V::Bool(true)), ("captured_minute", captured), ("participants", map(participants))])
}

#[test]
fn a_context_s_verdict_is_the_python_rule_s_in_its_order() {
    let good = context(V::Int(3), &[("a1", state(V::Float(0.2), V::Null))]);
    assert_eq!(dialogue::context_problem(&good, &["a1"], 5), Ok(None));
    assert_eq!(dialogue::validate_context(&good, &["a1"], 5), Ok(()));
    // The minute after the event, another participant, a stress out of bounds, an affect of other keys.
    assert_eq!(dialogue::context_problem(&good, &["a1"], 2), Ok(Some(dialogue::NOT_ITS_EVENT)));
    assert_eq!(dialogue::context_problem(&good, &["a2"], 5), Ok(Some(dialogue::NOT_ITS_EVENT)));
    let loud = context(V::Int(3), &[("a1", state(V::Bool(true), V::Null))]);
    assert_eq!(dialogue::context_problem(&loud, &["a1"], 5), Ok(Some(dialogue::NOT_UNIT)));
    let odd = context(V::Int(3), &[("a1", state(V::Float(0.2), map(&[("x", V::Float(0.0))])))]);
    assert_eq!(dialogue::context_problem(&odd, &["a1"], 5), Ok(Some(dialogue::AFFECT_INVALID)));
    assert_eq!(dialogue::validate_context(&odd, &["a1"], 5), Err(Error::Value(dialogue::AFFECT_INVALID.to_owned())));
    let big = context(V::Int(3), &[("a1", state(V::Big("100000000000000000000".into()), V::Null))]);
    assert!(matches!(dialogue::context_problem(&big, &["a1"], 5), Err(Error::Unsupported(_))));
    assert_eq!(dialogue::context_problem(&V::List(Vec::new()), &[], 5), Ok(Some(dialogue::NOT_ITS_EVENT)));
}

#[test]
fn a_capture_has_the_participants_in_order_and_says_the_rule_s_texts() {
    let world = with_body(&[("a2", body_record(0.6, 0.2))]);
    let captured = dialogue::capture_context(&world, &["a2", "a1"]).unwrap();
    let V::Map(fields) = &captured else { panic!("a context is a map") };
    assert_eq!(fields["captured_minute"], V::Int(10));
    let V::Map(participants) = &fields["participants"] else { panic!("participants are a map") };
    assert_eq!(participants.keys().map(|id| &**id).collect::<Vec<_>>(), ["a1", "a2"]);
    let V::Map(first) = &participants["a1"] else { panic!("a state is a map") };
    assert_eq!(first["affect"], V::Null);
    assert_eq!(first["stress"], V::Float(0.5));
    assert_eq!(dialogue::context_problem(&captured, &["a1", "a2"], 10), Ok(None));
    assert_eq!(dialogue::context_value(&world, "a2", "a1"), Ok(captured));
    assert_eq!(dialogue::capture_context(&world, &[]), Err(Error::Value(dialogue::NOT_ONE_OR_TWO.to_owned())));
    assert_eq!(dialogue::capture_context(&world, &["nobody"]), Err(Error::Value(dialogue::NOT_ONE_OR_TWO.to_owned())));
}
