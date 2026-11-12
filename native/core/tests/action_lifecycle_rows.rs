//! The rules of `agent_dynamics_v1` and of the open actions (`rules::agent_dynamics`,
//! `rules::action_lifecycle`) on the worlds of `integration/test_action_lifecycle_rows.py`: the
//! Project screen's generated Scenario at minute 0 (`people_rows.json`, written by
//! `tests/support/core_people_rows.py`), with the inputs that file made with `random.Random`
//! (Issue #225, lane M5). The PyO3 entries (`native/src/rules/agent_dynamics.rs`,
//! `action_lifecycle.rs`) read their arguments and call these kernels; the pytest file keeps an
//! entry test for each, and the tests of the boundary: the rows and their trees, what taints them,
//! the store's Python object, and the Python validation of a saved namespace.
//!
//! The digests the pytest file pinned from the Python rules (`PYTHON_DIGESTS`, of `json.dumps` of
//! the log's bytes, of the namespace's tree and of the store's snapshot) are kept: the tests write
//! the same text with `Dumps`, the trees with each record's fields in the order of the tree
//! (`NAMES`), as the PyO3 crate gives them to Python.

mod support;

use std::sync::{Arc, OnceLock};

use serde_json::{json, Value};
use sheltersim_core::cell::Shard;
use sheltersim_core::config::agent_dynamics::{
    awareness, canonical_policy, policy, MUST_CITE, UNSUPPORTED_FIELDS, UNSUPPORTED_VERSION, V1_SOURCE,
};
use sheltersim_core::error::Error;
use sheltersim_core::events::{EventRow, EventStatus};
use sheltersim_core::json::Encoder;
use sheltersim_core::row::{Field, IdMap, ValueRow};
use sheltersim_core::rules::action_lifecycle::{self as lifecycle, ActiveAction, ActiveActions, Ending, Proposal};
use sheltersim_core::rules::agent_dynamics::{self as dynamics, Decision};
use sheltersim_core::state::agent_dynamics::{AgentDynamicsRow, AgentMemory, NAME as DYNAMICS, POLICY_V1, POLICY_V2, POLICY_V3};
use sheltersim_core::value::V;
use sheltersim_core::world::{self, Fit, World, WorldCell};

use support::dumps::Dumps;
use support::{cases, load, texts, TestHost};

const FILE: &str = "people_rows";
/// `ANNOUNCED_FACILITY_AGENT_DYNAMICS_POLICY_VERSION`.
const ANNOUNCED: &str = POLICY_V3;

// ---- The fixture --------------------------------------------------------------------------------

fn data() -> &'static Value {
    static DATA: OnceLock<Value> = OnceLock::new();
    DATA.get_or_init(|| load::data(FILE))
}

fn inputs() -> &'static Value {
    &data()["inputs"]
}

fn entry(name: &str) -> Value {
    cases::resolve(&data()["worlds"], name)
}

/// The World of a named world of the file.
fn world(name: &str) -> World {
    let entry = entry(name);
    load::world(&entry["world"], &entry["slots"])
}

/// A named world, held for the rules that read their sections through the host, and its host.
fn held(name: &str) -> (Shard<World>, TestHost) {
    let entry = entry(name);
    (texts::cell(&entry), load::host(&entry))
}

/// The people of the world, by ID.
fn agents(world: &World) -> Vec<String> {
    let mut ids: Vec<String> = world.rows().unwrap().agents.iter().map(|(id, _)| id.to_string()).collect();
    ids.sort();
    ids
}

fn facilities(world: &World) -> Vec<String> {
    let mut ids: Vec<String> = world.rows().unwrap().facilities.iter().map(|(id, _)| id.to_string()).collect();
    ids.sort();
    ids
}

fn digest(text: &[u8]) -> String {
    texts::sha(std::str::from_utf8(text).unwrap())[..16].to_owned()
}

fn json_of<T: Field>(value: &T) -> Vec<u8> {
    let mut encoder = Encoder::with_capacity(1024);
    value.write_json(&mut encoder).unwrap();
    encoder.out
}

fn dynamics_json(world: &World) -> Vec<u8> {
    json_of(&*world.row::<AgentDynamicsRow>(DYNAMICS).unwrap())
}

fn text(value: &str) -> V {
    V::Str(Arc::from(value))
}

fn optional(value: &Option<Arc<str>>) -> V {
    value.as_deref().map_or(V::Null, text)
}

fn record(names: &[&str], values: impl IntoIterator<Item = V>) -> V {
    V::Map(names.iter().zip(values).map(|(name, value)| (Arc::from(*name), value)).collect())
}

/// The namespace's tree as Python holds it: each record's fields in the order of the tree
/// (`NAMES`), the agents in the row's order.
fn dynamics_tree(row: &AgentDynamicsRow) -> V {
    let agents = row
        .agents
        .iter()
        .map(|(id, memory)| {
            let values = [
                V::List(memory.known_facility_ids.iter().map(|id| text(id)).collect()),
                optional(&memory.last_action),
                optional(&memory.last_target_id),
                memory.last_decision_minute.map_or(V::Null, V::Int),
                V::Int(memory.decision_due_minute),
                optional(&memory.decision_reason),
            ];
            (id.clone(), record(AgentMemory::NAMES, values))
        })
        .collect();
    record(AgentDynamicsRow::NAMES, [V::Int(row.schema_version), text(&row.policy_version), V::Map(agents)])
}

/// `_text` of the namespace: `json.dumps` of its tree.
fn dynamics_text(row: &AgentDynamicsRow) -> V {
    V::Str(Arc::from(Dumps::default().text(&dynamics_tree(row))))
}

// ---- agent_dynamics_v1: the configs ---------------------------------------------------------------

/// `test_the_canonical_config_is_the_python_rules` and
/// `test_a_config_the_python_rule_refuses_gets_its_text`: the policy of each config, or the text
/// of its ValueError, written down from the Python rule. (A config of a key that is not a str, which
/// JSON cannot hold, stays in the pytest file, as does the dict the PyO3 entry builds of the policy.)
#[test]
fn the_canonical_config_is_the_python_rule_s_policy_or_its_text() {
    let read = |raw: Value| canonical_policy(Some(&load::v(&raw))).unwrap();
    assert_eq!(canonical_policy(None), Ok(Ok(POLICY_V1)));
    for raw in [
        json!({}),
        json!({"schema_version": 1}),
        json!({"policy_version": POLICY_V1}),
        json!({"schema_version": 1.0}),
        json!({"schema_version": true}),
    ] {
        assert_eq!(read(raw.clone()), Ok(POLICY_V1), "{raw}");
    }
    assert_eq!(read(json!({"policy_version": POLICY_V2})), Ok(POLICY_V2));
    assert_eq!(read(json!({"policy_version": ANNOUNCED, "source_id": "ASM-INTAKE-FACILITY-ORIENTATION-V1"})), Ok(ANNOUNCED));

    const FIELDS: &str = "agent_dynamics config has unsupported fields";
    const VERSION: &str = "agent_dynamics config has an unsupported version";
    assert_eq!((UNSUPPORTED_FIELDS, UNSUPPORTED_VERSION), (FIELDS, VERSION));
    assert_eq!(V1_SOURCE, "agent_dynamics v1 has no orientation source");
    assert_eq!(MUST_CITE, "agent_dynamics v2/v3 must cite the intake orientation assumption");
    for (raw, text) in [
        (json!([]), FIELDS),
        (json!("x"), FIELDS),
        (json!({"other": 1}), FIELDS),
        (json!({"schema_version": 2}), VERSION),
        (json!({"schema_version": "1"}), VERSION),
        (json!({"schema_version": null}), VERSION),
        (json!({"schema_version": 1180591620717411303424_u128}), VERSION),
        (json!({"schema_version": false}), VERSION),
        (json!({"policy_version": "later"}), VERSION),
        (json!({"policy_version": 1}), VERSION),
        (json!({"policy_version": null}), VERSION),
        // The schema is checked first, so the unhashable policy is never looked up.
        (json!({"schema_version": 2, "policy_version": []}), VERSION),
        (json!({"policy_version": POLICY_V1, "source_id": "x"}), V1_SOURCE),
        (json!({"policy_version": POLICY_V1, "source_id": null}), V1_SOURCE),
        (json!({"policy_version": POLICY_V2, "source_id": "x"}), MUST_CITE),
        (json!({"policy_version": POLICY_V3, "source_id": 1}), MUST_CITE),
    ] {
        assert_eq!(read(raw.clone()), Err(text), "{raw}");
    }
}

/// `test_an_awareness_config_the_model_decides_is_left_to_it`: each config the model decides is
/// refused, so the Python rule (its model) decides it. (A tuple, which JSON cannot hold, stays in
/// the pytest file.)
#[test]
fn an_awareness_config_the_model_decides_is_refused() {
    for config in [
        json!({"initial_known_facility_ids": ["a", "a"]}),
        json!({"initial_known_facility_ids": ["not stable"]}),
        json!({"discover_co_located_facilities_on_arrival": 1}),
        json!({"source_type": "measured"}),
        json!({"source_id": "other"}),
        json!({"schema_version": 2}),
        json!({"more": 1}),
    ] {
        assert!(awareness(&load::v(&json!({"facility_awareness": config}))).is_err_and(|fault| fault.is_refusal()), "{config}");
    }
}

// ---- agent_dynamics_v1: the writers -------------------------------------------------------------

/// `test_a_new_namespace_and_agent_are_the_python_rules`: the namespace `create_state` makes for
/// each awareness config (with the last facility known from the start) and each policy: the digest
/// of `json.dumps` of `json.dumps` of its tree.
#[test]
fn a_new_namespace_is_the_one_the_python_rule_made() {
    let world = world("start");
    let last = facilities(&world).pop().unwrap();
    let awareness_configs = [
        ("None", None),
        ("{}", Some(json!({}))),
        ("discover", Some(json!({"discover_co_located_facilities_on_arrival": false}))),
        ("source", Some(json!({"schema_version": 1, "source_type": "assumption"}))),
    ];
    let recorded = [
        ("None.None", "86de3321654fb7bc"),
        ("{}.None", "e3cb592715432d77"),
        ("discover.None", "e3cb592715432d77"),
        ("source.None", "e3cb592715432d77"),
        ("None.v3", "dffd9bb8892d86cf"),
        ("{}.v3", "dffd9bb8892d86cf"),
        ("discover.v3", "dffd9bb8892d86cf"),
        ("source.v3", "dffd9bb8892d86cf"),
    ];
    let mut found = Vec::new();
    for policy_version in [None, Some(POLICY_V3)] {
        for (label, config) in &awareness_configs {
            let mut configs = serde_json::Map::new();
            if let Some(config) = config {
                let mut config = config.clone();
                config["initial_known_facility_ids"] = json!([last]);
                configs.insert("facility_awareness".to_owned(), config);
            }
            if let Some(version) = policy_version {
                configs.insert("agent_dynamics".to_owned(), json!({"policy_version": version}));
            }
            let configs = load::v(&Value::Object(configs));
            let read = awareness(&configs).unwrap();
            let row = dynamics::create_state(&world, policy(&configs).unwrap(), read.as_ref()).unwrap();
            let label = format!("{label}.{}", if policy_version.is_some() { "v3" } else { "None" });
            found.push((label, digest(Dumps::default().text(&dynamics_text(&row)).as_bytes())));
        }
    }
    let found: Vec<(&str, &str)> = found.iter().map(|(label, value)| (label.as_str(), value.as_str())).collect();
    assert_eq!(found, recorded);
}

/// `test_the_writers_are_the_python_rules_on_random_calls`: 400 random calls of the writers (the
/// announced policy, and the first person knowing nothing), and after each the namespace: the
/// digest of `json.dumps` of the list of each answer of `agent_knows_facility` (`str(bool)`) and
/// each `json.dumps` of the namespace's tree.
#[test]
fn the_writers_on_random_calls_are_the_python_rules() {
    let (cell, mut host) = held("announced");
    let policy_kept = dynamics::Configs::policy(&cell, &mut host).unwrap();
    let limited = dynamics::Configs::knowledge_is_limited(&cell, &mut host).unwrap();
    assert_eq!(policy_kept.as_deref(), Ok(&ANNOUNCED));
    let policy_before = cell.with(|world| world.row::<AgentDynamicsRow>(DYNAMICS).unwrap().policy_version.clone()).unwrap();
    let mut texts = Vec::new();
    for (step, call) in inputs()["writers"].as_array().unwrap().iter().enumerate() {
        let step = step as i64;
        let kind = call[0].as_str().unwrap();
        let (agent_id, facility_id) = (call[1].as_str(), call[2].as_str());
        match kind {
            "propose" => {
                let decision = Decision {
                    agent_id: agent_id.unwrap(),
                    action: call[3].as_str().unwrap(),
                    target_id: call[4].as_str(),
                    minute: step,
                    next_decision_at: step + 5,
                    trigger: Some("t"),
                };
                cell.with(|world| dynamics::remember_decision(world, &decision)).unwrap().unwrap();
            }
            "learn" => dynamics::learn_facility_through(&cell, &mut host, agent_id, facility_id).unwrap(),
            "announce" => cell.with(|world| dynamics::announce_installed_facility(world, facility_id, &policy_kept)).unwrap().unwrap(),
            "knows" => {
                let knows = cell.with(|world| dynamics::knows_facility(world, &limited, agent_id, facility_id)).unwrap().unwrap();
                texts.push(V::Str(Arc::from(if knows { "True" } else { "False" })));
            }
            _ => assert_eq!(kind, "none"),
        }
        texts.push(cell.with(|world| dynamics_text(&world.row::<AgentDynamicsRow>(DYNAMICS).unwrap())).unwrap());
    }
    let policy_after = cell.with(|world| world.row::<AgentDynamicsRow>(DYNAMICS).unwrap().policy_version.clone()).unwrap();
    assert_eq!(policy_after, policy_before);
    assert_eq!(digest(Dumps::default().text(&V::List(texts)).as_bytes()), "e83cfb888c9bc24f");
}

/// `test_a_known_list_out_of_order_is_sorted_as_the_python_rule_does`: the list comes out as
/// `sorted(set(known) | discovered)`, which the pytest file computed in Python, and nothing else
/// of the namespace changes.
#[test]
fn a_known_list_out_of_order_is_sorted_as_the_python_rule_does() {
    let (cell, mut host) = held("start");
    let (agent, two) = cell.with(|world| (agents(world)[0].clone(), facilities(world)[..2].to_vec())).unwrap();
    let before = cell
        .with(|world| {
            let mut row = world.row_mut::<AgentDynamicsRow>(DYNAMICS).unwrap();
            row.agents.get_mut(agent.as_str()).unwrap().known_facility_ids = two.iter().rev().map(|id| Arc::from(id.as_str())).collect();
            drop(row);
            world.row::<AgentDynamicsRow>(DYNAMICS).unwrap().clone()
        })
        .unwrap();
    dynamics::learn_facility_through(&cell, &mut host, Some(&agent), Some(&two[0])).unwrap();
    let mut expected = before;
    expected.agents.get_mut(agent.as_str()).unwrap().known_facility_ids =
        inputs()["expected"]["known_sorted"].as_array().unwrap().iter().map(|id| Arc::from(id.as_str().unwrap())).collect();
    assert_eq!(cell.with(|world| dynamics_json(world)).unwrap(), json_of(&expected));
}

/// `test_without_a_config_every_facility_is_known_to_older_scenarios`.
#[test]
fn without_a_config_every_facility_is_known() {
    let configs = load::v(&json!({}));
    let limited = Ok(Arc::new(sheltersim_core::config::agent_dynamics::knowledge_is_limited(&configs).unwrap()));
    assert_eq!(dynamics::knows_facility(&world("start"), &limited, Some("nobody"), Some("nowhere")), Ok(true));
}

// ---- The open actions -----------------------------------------------------------------------------

fn event(name: &str) -> EventRow {
    load::event(&inputs()["events"][name])
}

/// `_applied`: the log holds the event applied, and the event counter is past its sequence.
fn applied(world: &World, event: &EventRow, minute: i64) {
    let next = world.id_counter("event").unwrap().unwrap_or(0).max(event.sequence + 1);
    world::set_id_counter(&mut world.row_mut::<ValueRow>(world::ID_COUNTERS).unwrap(), "event", next).unwrap();
    world.log_mut().unwrap().append_row(event, minute, EventStatus::Applied, None).unwrap();
}

fn propose(world: &World, active: &mut ActiveActions, agent: &str, action: &str, target: Option<&str>, minute: i64) {
    lifecycle::commit_action(world, active, &Proposal { agent_id: agent, action, target_id: target, minute, trigger: "t" }).unwrap();
}

/// `world.minute_index = minute`.
fn set_minute(world: &mut World, minute: i64) {
    world.scalars.get_mut().minute_index = Fit::Ok(minute);
}

fn log_len(world: &World) -> usize {
    world.log().unwrap().len()
}

/// The records of the log from `from`, as the bytes it keeps.
fn raws(world: &World, from: usize) -> Vec<V> {
    let log = world.log().unwrap();
    (from..log.len()).map(|index| V::Str(Arc::from(std::str::from_utf8(log.written(index).unwrap()).unwrap()))).collect()
}

fn rows_json(active: &ActiveActions) -> String {
    String::from_utf8(json_of::<IdMap<ActiveAction>>(&active.rows)).unwrap()
}

/// `store.snapshot()`: the open actions as Python's dicts hold them, by agent in the store's order,
/// each with its fields in the order of the tree (`ActiveAction::NAMES`) and its needs in the order
/// the rule took them.
fn snapshot(active: &ActiveActions) -> V {
    let mut out = indexmap::IndexMap::new();
    for (agent_id, action) in &active.rows {
        let needs = action.start_needs.iter().map(|(name, value)| (name.clone(), V::Float(*value))).collect();
        let values = [
            text(&action.action_id),
            text(&action.agent_id),
            text(&action.action),
            optional(&action.target_id),
            V::Int(action.start_minute),
            text(&action.reason_code),
            optional(&action.start_activity),
            V::Map(needs),
            text(&action.model_id),
            text(&action.model_source_id),
            V::Bool(action.continued),
        ];
        out.insert(agent_id.clone(), record(ActiveAction::NAMES, values));
    }
    V::Map(out)
}

/// What a rule wrote from the log's record `from` on, as `_receipts` took it: the digest of
/// `json.dumps` of its receipts' bytes, the event counter and `json.dumps` of the open actions.
fn receipts_digest(world: &World, from: usize, active: &ActiveActions) -> String {
    let counter = world.id_counter("event").unwrap().unwrap_or(0);
    let open = Dumps::default().text(&snapshot(active));
    let value = V::List(vec![V::List(raws(world, from)), V::Int(counter), V::Str(Arc::from(open))]);
    digest(Dumps::default().text(&value).as_bytes())
}

/// `test_a_sequence_of_actions_gives_the_python_rules_receipts`: 300 random proposals of six people
/// (a random fatigue now and then), and the end of the Run. The log is the one the Python rules
/// wrote: the digest of `json.dumps` of its records' bytes is the pytest file's, kept.
#[test]
fn a_sequence_of_actions_gives_the_python_rules_receipts() {
    let mut world = world("start");
    let mut active = ActiveActions::default();
    for step in inputs()["sequence"].as_array().unwrap() {
        let minute = step[0].as_i64().unwrap();
        set_minute(&mut world, minute);
        let agent = step[1].as_str().unwrap();
        if let Some(fatigue) = step[2].as_f64() {
            world.rows_mut().unwrap().agents.get_mut(agent).unwrap().needs.fatigue = fatigue;
        }
        if step[5].as_bool().unwrap() {
            propose(&world, &mut active, agent, step[3].as_str().unwrap(), step[4].as_str(), minute);
        }
    }
    set_minute(&mut world, 300);
    lifecycle::finalize(&world, &mut active, 300).unwrap();
    assert!(active.is_empty());
    let all = V::List(vec![V::List(raws(&world, 0))]);
    assert_eq!(digest(Dumps::default().text(&all).as_bytes()), "2ae9f2d61d0c4fba");
}

/// `test_an_applied_event_gives_the_python_rules_receipt`: an action open since minute 2 for the
/// target T1, and an applied event of each type that may end it, from its agent or its sender.
/// The same for an agent and a sender.
#[test]
fn an_applied_event_gives_the_python_rules_receipt() {
    let actions = ["request_toilet", "fetch_water", "seek_help", "seek_help", "socialize", "collect_meal", "collect_meal", "collect_meal", "rest", "rest"];
    let recorded = [
        "608d02a896f0b406",
        "77fe70358ce22375",
        "017cb551e27c7304",
        "c03ffd51c163cf93",
        "f2b126883535bed3",
        "9d776d98181f1ff2",
        "a6f74e2e980f0754",
        "81a953b53781a70f",
        "609e15dd62e42f5b",
        "609e15dd62e42f5b",
    ];
    for (index, action) in actions.iter().enumerate() {
        for sender in [0, 1] {
            let world = world("start");
            let agent = agents(&world)[0].clone();
            let mut active = ActiveActions::default();
            propose(&world, &mut active, &agent, action, Some("T1"), 2);
            let event = event(&format!("applied.{sender}.{index}"));
            applied(&world, &event, 3);
            let from = log_len(&world);
            lifecycle::complete_for_event(&world, &mut active, &Ending::of(&event), 3).unwrap();
            assert_eq!(receipts_digest(&world, from, &active), recorded[index], "{action} {sender}");
        }
    }
}

/// The part of `test_events_that_end_nothing_change_nothing` that reaches the kernel: an event due
/// before the action started, and one whose agent ID is not a str, end nothing. (A failed event and
/// one the log lacks are turned away by the PyO3 entry, and stay in the pytest file.)
#[test]
fn events_that_end_nothing_change_nothing() {
    let world = world("start");
    let agent = agents(&world)[0].clone();
    let mut active = ActiveActions::default();
    propose(&world, &mut active, &agent, "fetch_water", None, 5);
    let open = rows_json(&active);
    let length = log_len(&world);
    for (name, minute) in [("early", 6), ("nobody", 7)] {
        let event = event(name);
        applied(&world, &event, minute);
        let from = log_len(&world);
        lifecycle::complete_for_event(&world, &mut active, &Ending::of(&event), minute).unwrap();
        assert_eq!(log_len(&world), from, "{name}");
    }
    assert_eq!(log_len(&world), length + 2);
    assert_eq!(rows_json(&active), open);
}

/// `test_an_agent_gone_from_the_world_closes_without_needs`.
#[test]
fn an_agent_gone_from_the_world_closes_without_needs() {
    let mut world = world("start");
    let agent = agents(&world)[0].clone();
    let mut active = ActiveActions::default();
    propose(&world, &mut active, &agent, "fetch_water", None, 5);
    let event = event("gone");
    applied(&world, &event, 6);
    world.entities.get_mut().agents.remove_row(&agent);
    let from = log_len(&world);
    lifecycle::complete_for_event(&world, &mut active, &Ending::of(&event), 6).unwrap();
    let receipt: Value = serde_json::from_slice(world.log().unwrap().written(from).unwrap()).unwrap();
    assert_eq!(receipt["event"]["payload"]["current_needs"], json!({}));
    assert_eq!(receipt["event"]["payload"]["activity"], Value::Null);
    assert_eq!(receipts_digest(&world, from, &active), "b775ac3301d454ed");
}

/// `test_a_population_exit_gives_the_python_rules_receipts`: four open actions, and the population's
/// event that took the second away (transferred, gone from the world) and the fourth (a temporary
/// exit).
#[test]
fn a_population_exit_gives_the_python_rules_receipts() {
    let mut world = world("start");
    let people = agents(&world);
    let mut active = ActiveActions::default();
    for agent in &people[..4] {
        propose(&world, &mut active, agent, "wait", None, 1);
    }
    let event = event("population");
    applied(&world, &event, 3);
    for (name, value) in inputs()["population"].as_object().unwrap() {
        load::put_namespace(&mut world, name, load::v(value));
    }
    world.entities.get_mut().agents.remove_row(&people[1]);
    let from = log_len(&world);
    lifecycle::population_exit(&world, &mut active, "pop-1", 3).unwrap();
    let reasons: Vec<Value> = (from..log_len(&world))
        .map(|index| serde_json::from_slice::<Value>(world.log().unwrap().written(index).unwrap()).unwrap()["event"]["payload"]["reason_code"].clone())
        .collect();
    assert_eq!(reasons, [json!("population_departure"), json!("population_temporary_exit")]);
    assert_eq!(receipts_digest(&world, from, &active), "220f4152957a9cd0");
}

// ---- What the kernels refuse, with nothing changed ----------------------------------------------

/// `_refused`: the proposal is refused, and the log, the counters and the open actions are as they
/// were.
fn refused(world: &World, active: &mut ActiveActions, proposal: &Proposal<'_>) {
    let before = (log_len(world), world.id_counter("event").unwrap(), rows_json(active));
    assert!(matches!(lifecycle::commit_action(world, active, proposal), Err(Error::Unsupported(_))));
    assert_eq!((log_len(world), world.id_counter("event").unwrap(), rows_json(active)), before);
}

/// The part of `test_a_reason_the_receipts_model_decides_is_refused` and
/// `test_a_target_the_receipts_model_decides_is_refused` that reaches the kernel: a reason or a
/// target the receipts' model decides. (An int or None, which the PyO3 entry refuses itself, stays
/// in the pytest file.)
#[test]
fn a_reason_or_a_target_the_receipts_model_decides_is_refused() {
    let world = world("start");
    let agent = agents(&world)[0].clone();
    let mut active = ActiveActions::default();
    let long = "x".repeat(121);
    for reason in ["", long.as_str()] {
        refused(&world, &mut active, &Proposal { agent_id: &agent, action: "wait", target_id: None, minute: 0, trigger: reason });
    }
    for target in ["not stable", "é"] {
        refused(&world, &mut active, &Proposal { agent_id: &agent, action: "wait", target_id: Some(target), minute: 0, trigger: "t" });
    }
}

/// `test_a_transition_before_its_start_is_refused`.
#[test]
fn a_transition_before_its_start_is_refused() {
    let world = world("start");
    let agent = agents(&world)[0].clone();
    let mut active = ActiveActions::default();
    propose(&world, &mut active, &agent, "wait", None, 5);
    refused(&world, &mut active, &Proposal { agent_id: &agent, action: "wait", target_id: None, minute: 4, trigger: "t" });
}

/// `test_a_sequence_the_log_holds_is_refused`.
#[test]
fn a_sequence_the_log_holds_is_refused() {
    let world = world("start");
    let agent = agents(&world)[0].clone();
    world.log_mut().unwrap().append_row(&event("taken"), 0, EventStatus::Applied, None).unwrap();
    let mut active = ActiveActions::default();
    refused(&world, &mut active, &Proposal { agent_id: &agent, action: "wait", target_id: None, minute: 0, trigger: "t" });
}

/// `test_a_counter_a_rule_could_overflow_is_refused`.
#[test]
fn a_counter_a_rule_could_overflow_is_refused() {
    let world = world("start");
    let agent = agents(&world)[0].clone();
    let mut active = ActiveActions::default();
    propose(&world, &mut active, &agent, "wait", None, 0);
    world::set_id_counter(&mut world.row_mut::<ValueRow>(world::ID_COUNTERS).unwrap(), "event", (1 << 62) + 1).unwrap();
    refused(&world, &mut active, &Proposal { agent_id: &agent, action: "rest", target_id: None, minute: 1, trigger: "t" });
}
