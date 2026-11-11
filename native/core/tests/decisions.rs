//! The decisions of lane P8 a5 (`rules::behavior`, `rules::decisions`) over a world built here, with
//! no Python.
//!
//! The world is the one `decision_world.py` builds in Python (lane notes of P8 a5): the world of
//! `psychology.rs` at the last minute of a day, with everyone idle and due, a third person in the
//! hall and two zone demand modifiers. `data/decisions.json` holds what the PyO3 kernels before this
//! lane (the build of PR #184) gave for it with three policies: each proposal's action, target, next
//! decision, draw and probabilities (as `float.hex`), its candidates with their trigger texts and
//! the draw's key, and the bytes of its indexed record. Here the core gives the same.

mod support;

use std::sync::Arc;

use indexmap::IndexMap;
use serde_json::{json, Value};
use sheltersim_core::config::{Fault, FaultClass, Selection};
use sheltersim_core::entities::{
    Activity, AgentRow, AnyTable, FacilityRow, Mobility, NeedRow, ProfileRow, RequestRow, RequestStatus, Table, TaskRow, ZoneRow,
};
use sheltersim_core::error::Error;
use sheltersim_core::host::{Decision, DecisionSink, TraceLevel};
use sheltersim_core::row::{IdMap, Row, ValueRow};
use sheltersim_core::rules::action_lifecycle::ActiveActions;
use sheltersim_core::rules::behavior::{self as behavior, g, Got, Proposal};
use sheltersim_core::rules::decisions::{self as decisions, Observed, Recording, Sunk, LIFECYCLE, MEMORY};
use sheltersim_core::rules::psychology::Bad;
use sheltersim_core::rules::world_view::{self as view, Inputs, ViewCache, ViewData};
use sheltersim_core::run::behavior::{Behavior, OTHER_POLICY, OTHER_RUNNER, OTHER_SCORER};
use sheltersim_core::run::held::{Class, Held, Obj};
use sheltersim_core::state::body_state::{BodyRecord, BodyRow, History, Relationships};
use sheltersim_core::state::information::{BeliefRow, CountersRow, InformationRow, MessageRow};
use sheltersim_core::state::metric_state::MetricRow;
use sheltersim_core::state::psychological_distress::{AgentRecord, DistressRow};
use sheltersim_core::value::V;
use sheltersim_core::cell::Shard;
use sheltersim_core::events::log::EventLog;
use sheltersim_core::events::queue::Queue;
use sheltersim_core::world::{Fit, Place, World, WorldCell, ID_COUNTERS, METRIC_STATE, MODEL_CONFIGS};
use support::{Call, TestHost};

const MINUTE: i64 = 1439;
/// `CounterRNG(world.seed)` of the world: its seed is 7.
const SEED: &str = "7";
const MODULE: &str = "sheltersim.policies.behavior";

fn expected() -> Value {
    let raw = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/decisions.json")).unwrap();
    serde_json::from_str(&raw).unwrap()
}

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

fn v(value: &Value) -> V {
    match value {
        Value::Null => V::Null,
        Value::Bool(flag) => V::Bool(*flag),
        Value::Number(number) => match number.as_i64() {
            Some(int) if !number.to_string().contains(['.', 'e', 'E']) => V::Int(int),
            _ => V::Float(number.as_f64().expect("a float")),
        },
        Value::String(text) => V::Str(Arc::from(text.as_str())),
        Value::Array(items) => V::List(items.iter().map(v).collect()),
        Value::Object(map) => V::Map(map.iter().map(|(key, item)| (Arc::from(key.as_str()), v(item))).collect::<IndexMap<_, _>>()),
    }
}

/// `float.hex` of CPython.
fn hex(value: f64) -> String {
    let sign = if value.is_sign_negative() { "-" } else { "" };
    if value == 0.0 {
        return format!("{sign}0x0.0p+0");
    }
    let bits = value.to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as i64;
    let fraction = bits & ((1 << 52) - 1);
    let (lead, exponent) = if exponent == 0 { (0, -1022) } else { (1, exponent - 1023) };
    format!("{sign}0x{lead}.{fraction:013x}p{exponent:+}")
}


// ---- The world -----------------------------------------------------------------------------------

fn put<R: Clone>(table: &mut Table<R>, id: &str, row: R) {
    let slot = table.reserve_slot();
    table.fill_slot(slot, row);
    assert_eq!(table.bind(text(id), slot), None);
}

fn add_row<T: Row>(world: &mut World, name: &str, place: Place, row: T) {
    let index = world.slots.add(text(name), place).unwrap();
    world.slots.at(index).unwrap().try_borrow_mut().unwrap().row = Some(Box::new(row));
}

fn agent(id: &str, zone: &str, group: Option<&str>, stress: f64, needs: [f64; 4], mobility: Mobility) -> AgentRow {
    let [toilet_urgency, hunger, fatigue, hydration_need] = needs;
    AgentRow {
        profile: ProfileRow {
            id: text(id),
            group_id: group.map(text),
            initial_zone_id: text(zone),
            mobility,
            support_need: 0.25,
            stress_sensitivity: 1.5,
            rng_key: text(&format!("key-{id}")),
            initial_needs: NeedRow { toilet_urgency: 0.0, hunger: 0.0, fatigue: 0.0, hydration_need: 0.0 },
            provenance: V::Map(IndexMap::new()),
        },
        zone_id: text(zone),
        activity: Activity::Idle,
        target_id: None,
        needs: NeedRow { toilet_urgency, hunger, fatigue, hydration_need },
        stress,
        trust: 0.5,
        next_decision_at: 0,
        support_request_id: None,
        support_cooldown_until: 0,
    }
}

fn facility(id: &str, kind: &str, zone: &str, active: &[&str], queue: &[&str]) -> FacilityRow {
    FacilityRow {
        id: text(id),
        kind: text(kind),
        zone_id: text(zone),
        capacity: 2,
        working_units: 2,
        active_agent_ids: active.iter().map(|id| text(id)).collect(),
        queue_agent_ids: queue.iter().map(|id| text(id)).collect(),
        resource_active_agent_ids: Vec::new(),
        resource_queue_agent_ids: Vec::new(),
    }
}

fn zone(id: &str, name: &str, capacity: Option<i64>) -> ZoneRow {
    ZoneRow { id: text(id), name: text(name), capacity, display_x: None, display_y: None }
}

fn add_value(world: &mut World, name: &str, place: Place, value: Value) {
    add_row(world, name, place, ValueRow(v(&value)));
}

fn information() -> InformationRow {
    let mut messages: IdMap<MessageRow> = IdMap::default();
    messages.insert(
        text("m1"),
        MessageRow {
            id: text("m1"),
            truth_status: text("false"),
            source_id: text("rumor"),
            zone_id: text("hall"),
            time: 3,
            origin_event_id: text("e-m1"),
            origin_agent_id: text("A1"),
        },
    );
    let mut believed: IdMap<BeliefRow> = IdMap::default();
    believed.insert(
        text("m1"),
        BeliefRow {
            message_id: text("m1"),
            belief: text("heard"),
            source: text("A1"),
            last_contact: 4,
            last_event_id: text("e-b1"),
            exposed: true,
            belief_strength: 0.75,
            share_propensity: 0.5,
            correction_exposed: false,
            correction_accepted: false,
            ever_misbelieved: true,
            ever_forgotten: false,
            last_correction_accepted: false,
        },
    );
    let mut beliefs: IdMap<IdMap<BeliefRow>> = IdMap::default();
    beliefs.insert(text("A1"), IdMap::default());
    beliefs.insert(text("A2"), believed);
    beliefs.insert(text("A3"), IdMap::default());
    let mut query_pressure: IdMap<f64> = IdMap::default();
    query_pressure.insert(text("A1"), 0.125);
    query_pressure.insert(text("A2"), 0.5);
    query_pressure.insert(text("A3"), 0.75);
    InformationRow {
        schema_version: 2,
        messages,
        beliefs,
        contacts: IdMap::default(),
        query_pressure,
        support_requests: Vec::new(),
        counters: CountersRow { contact_attempts: 0, contact_deliveries: 0, correction_deliveries: 0, support_requests: 0 },
    }
}

fn body_record(values: [f64; 7]) -> BodyRecord {
    let [sleep_debt_minutes, uncertainty, reassurance, discontent, positive_affect, negative_affect, hygiene_need] = values;
    BodyRecord { sleep_debt_minutes, uncertainty, reassurance, discontent, positive_affect, negative_affect, hygiene_need }
}

/// The world of `decision_world.py`: `psych_world.py`'s of lane a3, with A1 idle, A3 in the hall
/// and two demand modifiers.
fn world() -> World {
    let mut world = World::default();
    {
        let scalars = world.scalars.get_mut();
        scalars.minute_index = Fit::Ok(MINUTE);
        scalars.schema_version = Fit::Ok(6);
    }
    {
        let rows = world.entities.get_mut();
        let mut a2 = agent("A2", "hall", Some("g1"), 0.5, [0.1, 0.2, 0.3, 0.4], Mobility::Independent);
        a2.support_request_id = Some(text("r1"));
        a2.support_cooldown_until = 40;
        put(&mut rows.agents, "A2", a2);
        let mut a1 = agent("A1", "room", None, 0.0, [0.0, 0.5, 1.0, 0.0], Mobility::Wheelchair);
        a1.next_decision_at = 31;
        put(&mut rows.agents, "A1", a1);
        put(&mut rows.agents, "A3", agent("A3", "hall", Some("g1"), 0.875, [0.75, 0.9, 0.1, 0.85], Mobility::Independent));
        put(&mut rows.zones, "room", zone("room", "Room", Some(5)));
        put(&mut rows.zones, "hall", zone("hall", "Hall", None));
        put(&mut rows.facilities, "f-water", facility("f-water", "Water_Station", "hall", &["A2"], &[]));
        put(&mut rows.facilities, "f-toilet", facility("f-toilet", "toilet", "room", &[], &["A1"]));
        put(
            &mut rows.support_requests,
            "r1",
            RequestRow {
                id: text("r1"),
                agent_id: text("A2"),
                requested_minute: 20,
                timeout_minute: 90,
                status: RequestStatus::Pending,
                assigned_staff_id: None,
                task_id: None,
                resolved_minute: None,
                cooldown_until: 0,
                last_assignment_attempt_minute: None,
                last_assignment_wait_reason_code: None,
            },
        );
        put(
            &mut rows.active_tasks,
            "t1",
            TaskRow {
                id: text("t1"),
                kind: text("toilet_queue"),
                agent_id: Some(text("A1")),
                resource_id: Some(text("f-toilet")),
                start_minute: 25,
                due_minute: 60,
                staff_id: None,
            },
        );
    }
    add_value(
        &mut world,
        MODEL_CONFIGS,
        Place::Field,
        json!({
            "resource_service": {"a": 1.5, "b": [1, 2.5, "x"]},
            "information_spread": {"query_pressure_per_claim": 0.25},
            "policy_registry": {"schema_version": 2, "psychology": "life-state-feedback-v1", "zone_rest": "zone-rest-limiting-v1"},
            "zone_environment": {
                "policy_version": "zone-environment-v2",
                "temperature_stress_per_degree_minute": 0.001,
                "baseline_by_zone": {"hall": {"temperature_c": 30.0}}
            },
            "population_context": {"people": {"A1": {"roles": ["parent"], "usual_wake_minute": 420, "care_for_ids": ["A2"]}}}
        }),
    );
    add_row(&mut world, "information_spread_v1", Place::Namespace, information());
    let mut distress: IdMap<AgentRecord> = IdMap::default();
    distress.insert(
        text("A2"),
        AgentRecord {
            baseline_distress: 0.125,
            long_term_distress: 0.25,
            daily_acute_sum: 2.5,
            daily_high_distress_minutes: 2,
            daily_observed_minutes: 5,
            completed_days: 3,
        },
    );
    add_row(&mut world, "psychological_distress_v1", Place::Namespace, DistressRow { schema_version: 1, last_updated_minute: MINUTE - 1, agents: distress });
    let mut bodies: IdMap<BodyRecord> = IdMap::default();
    bodies.insert(text("A1"), body_record([120.0, 0.25, 0.125, 0.5, 0.375, 0.25, 0.5]));
    bodies.insert(text("A2"), body_record([30.5, 0.75, 0.0, 0.125, 0.625, 0.5, 0.0625]));
    add_row(
        &mut world,
        "body_state_v1",
        Place::Namespace,
        BodyRow {
            schema_version: 1,
            policy_version: text("life-state-feedback-v1"),
            source_id: text("ASM-LIFE-STATE-FEEDBACK-V1"),
            config: V::Map(IndexMap::new()),
            agents: bodies,
            relationships: Relationships::from_map(IdMap::default()),
            history: History::default(),
        },
    );
    let mut metrics: MetricRow = IdMap::default();
    metrics.insert(text("acute_distress_monitor:A2:current_run"), 3.0);
    metrics.insert(text("acute_distress_monitor:A2:longest_run"), 4.0);
    metrics.insert(text("acute_distress_monitor:A2:episodes"), 1.0);
    add_row(&mut world, METRIC_STATE, Place::Field, metrics);
    add_value(&mut world, "population_presence_v1", Place::Namespace, json!({"people": {"A1": {"household_id": "h1"}}}));
    add_value(
        &mut world,
        "infrastructure_v1",
        Place::Value,
        json!({
            "environmental_stressors": {
                "s2": {"active": true, "zone_id": "hall", "stress_per_minute": 0.1, "start_event_id": "e-s2", "source_id": "src-2"},
                "s1": {"active": true, "zone_id": "hall", "stress_per_minute": 0.2, "start_event_id": "e-s1", "source_id": "src-1"},
                "s3": {"active": false, "zone_id": "room", "stress_per_minute": 9.0, "start_event_id": "e-s3", "source_id": null}
            },
            "outages": {"o1": {"active": true, "affected_facility_ids": ["f-toilet"], "start_event_id": "e-o1"}}
        }),
    );
    add_value(
        &mut world,
        "operational_events_v1",
        Place::Value,
        json!({"water_outages": {"w1": {"outage_event_id": "e-w1", "active": true, "target_kind": "zone", "target_id": "hall"}}}),
    );
    add_value(
        &mut world,
        "zone_environment_v1",
        Place::Value,
        json!({"schema_version": 1, "modifiers": {"z1": {
            "zone_id": "room", "op": "set_lighting", "parameters": {"level": 0.25}, "start_minute": 0,
            "start_event_id": "e-z1", "source_id": "src-z", "assumption_ids": ["asm-z"]},
          "z2": {"zone_id": "room", "op": "modify_demand_rate",
            "parameters": {"action": "rest", "probability_multiplier": 1.5, "duration_multiplier": 1.5},
            "start_minute": 0, "start_event_id": "e-z2", "source_id": "src-z2", "assumption_ids": ["asm-z2"]},
          "z3": {"zone_id": "hall", "op": "modify_demand_rate",
            "parameters": {"action": "request_toilet", "probability_multiplier": 0.5, "duration_multiplier": 1.0},
            "start_minute": 0, "start_event_id": "e-z3", "source_id": "src-z3", "assumption_ids": []}}}),
    );
    world
}

fn view_of(world: &World) -> ViewData {
    let selection = Ok(Arc::new(Selection::new(vec![(text("psychology"), text("life-state-feedback-v1"))])));
    let inputs = Inputs { selection: &selection, configs: None };
    view::build(world, &inputs, &mut ViewCache::default()).unwrap().finish(false)
}



// ---- The policies, as the Run reads them ---------------------------------------------------------

fn object(module: &str, name: &str, attrs: Vec<(&str, Result<Held, Fault>)>) -> Arc<Obj> {
    let mut found = Obj::new(Class::named(module, name));
    for (key, held) in attrs {
        found.attrs.insert(text(key), held);
    }
    Arc::new(found)
}

fn value(value: V) -> Result<Held, Fault> {
    Ok(Held::Value(value))
}

/// The fields of `scorer(**more)` of `decision_world.py`, in the dataclass's order.
fn scorer_fields(more: &[(&str, V)]) -> Vec<(&'static str, Result<Held, Fault>)> {
    let mut fields: Vec<(&'static str, V)> = vec![
        ("temperature", V::Float(0.35)),
        ("decision_interval_minutes", V::Int(15)),
        ("decision_interval_minutes_by_agent", v(&json!({"A3": 20}))),
        ("temperature_by_agent", v(&json!({"A2": 0.5}))),
        ("sleep_debt_utility_weight", V::Float(1.0)),
        ("policy_version", V::Str(text("utility-softmax-v1"))),
        ("resource_service_enabled", V::Bool(true)),
        ("hydration_action_threshold", V::Float(0.25)),
        ("hunger_action_threshold", V::Float(0.7)),
        ("water_utility_base", V::Float(0.18)),
        ("water_utility_need_weight", V::Float(1.0)),
        ("meal_utility_base", V::Float(0.18)),
        ("meal_utility_need_weight", V::Float(0.85)),
        ("information_query_threshold", V::Float(0.4)),
        ("information_query_utility_weight", V::Float(0.3)),
        ("autonomy_enabled", V::Bool(true)),
    ];
    for (name, changed) in more {
        fields.iter_mut().find(|(field, _)| field == name).unwrap().1 = changed.clone();
    }
    fields.into_iter().map(|(name, item)| (name, value(item))).collect()
}

fn controller_of(runner: Result<Held, Fault>) -> Obj {
    let mut controller = Obj::new(Class::named("sheltersim.experiments.runner", "OfflineSimulationController"));
    controller.attrs.insert(text("decisions"), runner);
    controller
}

fn runner_of(policy: Result<Held, Fault>) -> Result<Held, Fault> {
    Ok(Held::Object(object(MODULE, "BehaviorDecisionRunner", vec![("policy", policy)])))
}

/// `controller.decisions` with a utility policy of these fields.
fn utility(fields: Vec<(&'static str, Result<Held, Fault>)>) -> Behavior {
    Behavior::of(&controller_of(runner_of(Ok(Held::Object(object(MODULE, "UtilityBehaviorPolicy", fields))))))
}

/// The policy `name` of `decision_world.py`.
fn behavior_of(name: &str) -> Behavior {
    match name {
        "utility" => utility(scorer_fields(&[])),
        "sleep" => utility(scorer_fields(&[
            ("policy_version", V::Str(text("sleep-aware-utility-softmax-v2"))),
            ("sleep_debt_utility_weight", V::Float(1.25)),
        ])),
        _ => {
            let scorer = object(MODULE, "UtilityBehaviorPolicy", scorer_fields(&[("hydration_action_threshold", V::Float(0.5))]));
            let policy = object(
                MODULE,
                "PriorityRuleBehaviorPolicy",
                vec![("policy_version", value(V::Str(text("priority-rule-v1")))), ("scorer", Ok(Held::Object(scorer)))],
            );
            Behavior::of(&controller_of(runner_of(Ok(Held::Object(policy)))))
        }
    }
}

fn due(data: &ViewData, behavior: &Behavior) -> Got<Option<Vec<Proposal>>> {
    let scoring = behavior::scoring_of(behavior)?;
    behavior::due(data, &scoring, SEED, text(&"0".repeat(64)))
}

/// The text of a refusal: the rule's, or one the Run's reading of the policy kept.
fn refusal<T: std::fmt::Debug>(got: Got<T>) -> String {
    match got {
        Err(Bad::Refused(Error::Unsupported(reason))) => reason.into_owned(),
        Err(Bad::Policy(fault)) if fault.refusal().is_some() => fault.refusal().unwrap().to_owned(),
        other => panic!("not a refusal: {other:?}"),
    }
}

// ---- The tests -----------------------------------------------------------------------------------

#[test]
fn the_proposals_and_records_are_the_kernels_before() {
    let expected = expected();
    for name in ["utility", "sleep", "priority"] {
        let entry = &expected[name];
        let world = world();
        let data = view_of(&world);
        let state_hash = text(entry["state_hash"].as_str().unwrap());
        let behavior = behavior_of(name);
        let scoring = behavior::scoring_of(&behavior).map_err(|_| "the policy").unwrap();
        let proposals = behavior::due(&data, &scoring, SEED, state_hash).map_err(|_| "a refusal").unwrap().unwrap();
        let wanted = entry["proposals"].as_array().unwrap();
        assert_eq!(proposals.len(), wanted.len(), "{name}");
        let cell = Shard::new(world);
        for (proposal, wanted) in proposals.iter().zip(wanted) {
            let outcome = &proposal.outcome;
            let agent = &data.agents[proposal.index];
            let candidates: Vec<Value> = outcome
                .candidates
                .iter()
                .map(|item| {
                    json!([
                        item.action.value(),
                        item.eligible,
                        proposal.trigger_of(&data, item).unwrap(),
                        hex(item.utility),
                        item.priority,
                        hex(item.probability)
                    ])
                })
                .collect();
            let key: Vec<Value> = proposal
                .rng_key(&data)
                .into_iter()
                .map(|part| match part {
                    Ok(text) => json!(text),
                    Err(number) => json!(number),
                })
                .collect();
            let found = json!({
                "agent_id": &*agent.id,
                "action": outcome.action.value(),
                "choice": hex(outcome.choice_value),
                "selected": hex(outcome.selection_probability),
                "next": outcome.next_decision_at,
                "target": outcome.target.map(|target| data.agents[target].id.to_string()),
                "rng_key": key,
                "policy_version": &*proposal.common.policy_version,
                "candidates": candidates,
            });
            for field in ["agent_id", "action", "choice", "selected", "next", "target", "rng_key", "policy_version", "candidates"] {
                assert_eq!(found[field], wanted[field], "{name}: {} {field}", agent.id);
            }

            // The indexed record, uncommitted, with the sink's source of the utility policy alone.
            let source = (name == "utility").then(|| text("SRC-U"));
            let mut host = TestHost { sink: DecisionSink::Indexed, source: Some(source), ..TestHost::default() };
            let observed = Observed::Row { data: &data, index: proposal.index };
            let target = outcome.target.map(|target| &*data.agents[target].id);
            let recording = Recording {
                data: &data,
                proposal,
                target_id: target,
                next_decision_at: outcome.next_decision_at,
                committed: false,
                observed: &observed,
                remembers: false,
            };
            assert_eq!(decisions::record(&cell, &mut host, &recording), Ok(Sunk::Recorded));
            let [Call::Decision(Decision::Indexed(raw))] = &host.calls[..] else { panic!("{:?}", host.calls) };
            assert_eq!(std::str::from_utf8(raw).unwrap(), wanted["record"].as_str().unwrap(), "{name}: {}", agent.id);
        }
    }
}

#[test]
fn the_policy_is_read_with_the_refusals_and_order_of_the_kernel() {
    let data = view_of(&world());
    // The classes, by `is`.
    let other = Behavior::of(&controller_of(Ok(Held::Object(object("tests", "OtherRunner", vec![])))));
    assert_eq!(refusal(due(&data, &other)), OTHER_RUNNER);
    assert_eq!(refusal(due(&data, &Behavior::of(&controller_of(runner_of(value(V::Null)))))), OTHER_POLICY);
    let foreign = object("tests", "Counting", vec![]);
    assert_eq!(refusal(due(&data, &Behavior::of(&controller_of(runner_of(Ok(Held::Object(foreign.clone()))))))), OTHER_POLICY);
    let priority = object(MODULE, "PriorityRuleBehaviorPolicy", vec![("scorer", Ok(Held::Object(foreign)))]);
    assert_eq!(refusal(due(&data, &Behavior::of(&controller_of(runner_of(Ok(Held::Object(priority))))))), OTHER_SCORER);

    // A read that raised is raised again by its path from the controller.
    let raised = Fault::new(FaultClass::Other(text("builtins.AttributeError")), "no attribute 'temperature'");
    let mut fields = scorer_fields(&[]);
    fields.iter_mut().find(|(name, _)| *name == "temperature").unwrap().1 = Err(raised.clone());
    match due(&data, &utility(fields)) {
        Err(Bad::Policy(fault)) => {
            assert_eq!(fault.path, ["decisions", "policy", "temperature"]);
            assert_eq!(fault.fault, raised);
        }
        other => panic!("{other:?}"),
    }
    let missing = Behavior::of(&controller_of(Err(raised.clone())));
    let Err(Bad::Policy(fault)) = due(&data, &missing) else { panic!() };
    assert_eq!(fault.path, ["decisions"]);

    // The values, as the kernel read them.
    let refused_with = |name: &str, changed: V| refusal(due(&data, &utility(scorer_fields(&[(name, changed)]))));
    assert_eq!(refused_with("temperature", V::Big("1".repeat(30).into())), "an int is too large");
    assert_eq!(refused_with("temperature", V::Int(1 << 54)), "a policy value is too large");
    assert_eq!(refused_with("temperature", V::Bool(true)), "a policy value is not an int");
    assert_eq!(refused_with("decision_interval_minutes", V::Float(15.0)), "a policy value is not an int");
    assert_eq!(refused_with("temperature_by_agent", V::List(Vec::new())), "the per-person coefficients are not a plain dict");
    assert_eq!(refused_with("policy_version", V::Int(1)), "the policy version is not a str");
    assert_eq!(refused_with("policy_version", V::Str(text(""))), "the policy version is empty or not a str");
    assert_eq!(refused_with("temperature", V::Float(-1e-300)), "an exponent is out of range");
    assert_eq!(refused_with("decision_interval_minutes", V::Int(0)), "a proposal has invalid decision timing");
    let mut subclass = scorer_fields(&[]);
    subclass.iter_mut().find(|(name, _)| *name == "policy_version").unwrap().1 = Ok(Held::Text(text("utility-softmax-v1")));
    assert_eq!(refusal(due(&data, &utility(subclass))), "the policy version is not a str");
    let mut opaque = scorer_fields(&[]);
    opaque.iter_mut().find(|(name, _)| *name == "autonomy_enabled").unwrap().1 = Ok(Held::Opaque { callable: false });
    assert_eq!(refusal(due(&data, &utility(opaque))), "a flag is not a plain value");
    // An int counts as its float, and a person's own coefficients win.
    let as_ints = utility(scorer_fields(&[("water_utility_base", V::Int(0)), ("temperature_by_agent", v(&json!({"A2": 1})))]));
    assert!(due(&data, &as_ints).map_err(|_| "refused").unwrap().is_some());

    // Nobody due: nothing is read past the runner's classes.
    let mut world = world();
    for id in ["A1", "A2", "A3"] {
        world.entities.get_mut().agents.get_mut(id).unwrap().next_decision_at = MINUTE + 1;
    }
    let data = view_of(&world);
    assert!(due(&data, &utility(scorer_fields(&[("temperature", V::Bool(true))]))).map_err(|_| "refused").unwrap().is_none());
}

#[test]
fn the_trigger_texts_format_numbers_as_python_s_g() {
    for (number, text) in [
        (0.25, "0.25"),
        (1.0, "1"),
        (0.7, "0.7"),
        (1.1, "1.1"),
        (1e-5, "1e-05"),
        (1234567.0, "1.23457e+06"),
        (123456.0, "123456"),
        (-0.0, "-0"),
        (f64::INFINITY, "inf"),
        (f64::NEG_INFINITY, "-inf"),
        (f64::NAN, "nan"),
    ] {
        assert_eq!(g(number), text, "{number}");
    }
}

/// The first proposal of the utility policy, and the world.
fn first_proposal() -> (Shard<World>, ViewData, Proposal) {
    let world = world();
    let data = view_of(&world);
    let proposal = due(&data, &behavior_of("utility")).map_err(|_| "refused").unwrap().unwrap().remove(0);
    (Shard::new(world), data, proposal)
}

fn recording<'a>(data: &'a ViewData, proposal: &'a Proposal, observed: &'a Observed<'a>) -> Recording<'a> {
    Recording {
        data,
        proposal,
        target_id: Some("X"),
        next_decision_at: MINUTE + 15,
        committed: false,
        observed,
        remembers: false,
    }
}

#[test]
fn the_record_goes_where_the_sink_takes_it() {
    let (cell, data, proposal) = first_proposal();
    let row = Observed::Row { data: &data, index: proposal.index };
    let plain = Observed::Value(V::Null);
    let other = Observed::Other;
    let run = |sink: DecisionSink, level: TraceLevel, source: Option<Option<Arc<str>>>, observed: &Observed<'_>| {
        let mut host = TestHost { sink, level, source, ..TestHost::default() };
        let sunk = decisions::record(&cell, &mut host, &recording(&data, &proposal, observed)).unwrap();
        (sunk, host.calls)
    };
    assert_eq!(run(DecisionSink::Silent, TraceLevel::Off, Some(None), &row), (Sunk::Recorded, vec![]));
    assert_eq!(run(DecisionSink::Python, TraceLevel::Off, Some(None), &row), (Sunk::Python, vec![]));
    // The indexed record needs the view's person, and a source the bytes can write.
    assert_eq!(run(DecisionSink::Indexed, TraceLevel::Off, Some(None), &plain), (Sunk::Python, vec![]));
    assert_eq!(run(DecisionSink::Indexed, TraceLevel::Off, None, &row), (Sunk::Python, vec![]));
    let (sunk, calls) = run(DecisionSink::Indexed, TraceLevel::Off, Some(None), &row);
    assert_eq!(sunk, Sunk::Recorded);
    let [Call::Decision(Decision::Indexed(raw))] = &calls[..] else { panic!() };
    let record: Value = serde_json::from_slice(raw).unwrap();
    assert_eq!((&record["source_id"], &record["target_id"], &record["next_decision_at"]), (&Value::Null, &json!("X"), &json!(MINUTE + 15)));
    assert_eq!(record["observed_state"], serde_json::from_slice::<Value>(&behavior::observed_value(&data, proposal.index).to_json().unwrap()).unwrap());

    // A trace's writer takes the document and the observed state; candidates only in full.
    assert_eq!(run(DecisionSink::Artifacts, TraceLevel::Full, Some(None), &other), (Sunk::Python, vec![]));
    for (level, observed) in [(TraceLevel::Summary, &plain), (TraceLevel::Full, &row)] {
        let (sunk, calls) = run(DecisionSink::Artifacts, level, Some(None), observed);
        assert_eq!(sunk, Sunk::Recorded);
        let [Call::Decision(Decision::Document { document, observed: kept })] = &calls[..] else { panic!() };
        let V::Map(document) = document else { panic!() };
        let mut keys: Vec<&str> = document.keys().map(|key| &**key).collect();
        let full = level == TraceLevel::Full;
        assert_eq!(keys.last() == Some(&"candidates"), full);
        if full {
            keys.pop();
            assert_eq!(*kept, behavior::observed_value(&data, proposal.index));
        } else {
            assert_eq!(*kept, V::Null);
        }
        assert_eq!(
            keys,
            [
                "minute_index", "agent_id", "action", "target_id", "observed_state_hash", "trigger", "selected_action",
                "selected_probability", "choice_value", "rng_key", "next_decision_at", "policy_version", "source_type",
                "source_id"
            ]
        );
        assert_eq!(document["source_id"], V::Str(text("ASM-UTILITY-BEHAVIOR-V1")));
    }

    // What the bytes do not write is the sink's own: a long observed state, a NaN, a person gone.
    let long = {
        let mut long = data.clone();
        long.agents[proposal.index].target_id = Some(text(&"x".repeat(9000)));
        long
    };
    let long_row = Observed::Row { data: &long, index: proposal.index };
    assert_eq!(run(DecisionSink::Indexed, TraceLevel::Off, Some(None), &long_row), (Sunk::Python, vec![]));
    let nan = {
        let mut nan = data.clone();
        nan.agents[proposal.index].stress = f64::NAN;
        nan
    };
    let nan_row = Observed::Row { data: &nan, index: proposal.index };
    assert_eq!(run(DecisionSink::Indexed, TraceLevel::Off, Some(None), &nan_row), (Sunk::Python, vec![]));
}

#[test]
fn the_household_is_read_as_the_sink_reads_it() {
    let (cell, data, proposal) = first_proposal();
    let id = data.agents[proposal.index].id.to_string();
    assert_eq!(id, "A1");
    let observed = Observed::Row { data: &data, index: proposal.index };
    let household = |presence: Option<Value>| {
        cell.with(|world| {
            let slot = world.slots.get("population_presence_v1").unwrap();
            slot.try_borrow_mut().unwrap().row = presence.map(|presence| Box::new(ValueRow(v(&presence))) as Box<dyn Row>);
        })
        .unwrap();
        let mut host = TestHost { sink: DecisionSink::Indexed, ..TestHost::default() };
        match decisions::record(&cell, &mut host, &recording(&data, &proposal, &observed)).unwrap() {
            Sunk::Python => None,
            Sunk::Recorded => {
                let [Call::Decision(Decision::Indexed(raw))] = &host.calls[..] else { panic!() };
                Some(serde_json::from_slice::<Value>(raw).unwrap()["household_id"].clone())
            }
        }
    };
    // A1 has no group of its profile.
    assert_eq!(household(Some(json!({"people": {"A1": {"household_id": "h1"}}}))), Some(json!("h1")));
    assert_eq!(household(Some(json!({"people": {"A1": {"household_id": null}}}))), Some(Value::Null));
    assert_eq!(household(Some(json!({"people": {}}))), Some(Value::Null));
    assert_eq!(household(Some(json!({}))), Some(Value::Null));
    assert_eq!(household(Some(json!({"people": {"A1": "not a dict"}}))), Some(Value::Null));
    assert_eq!(household(Some(json!("not a dict"))), Some(Value::Null));
    assert_eq!(household(None), Some(Value::Null));
    assert_eq!(household(Some(json!({"people": ["a list"]}))), None);
    assert_eq!(household(Some(json!({"people": {"A1": {"household_id": 3}}}))), None);
}

#[test]
fn the_lifecycle_and_the_memory_are_called_after_the_record() {
    let (cell, data, proposal) = first_proposal();
    let observed = Observed::Row { data: &data, index: proposal.index };
    let mut active = ActiveActions::default();
    let mut host = TestHost::default();
    // Nothing committed and no memory: only the record.
    assert_eq!(decisions::record_outcome(&cell, &mut host, &mut active, &recording(&data, &proposal, &observed)), Ok(Sunk::Recorded));
    // What the rules refuse is the KernelRefusal of their Python entries, after the record: the
    // world keeps no event queue for the lifecycle, and no `agent_dynamics_v1` for the memory.
    let committing = Recording { committed: true, ..recording(&data, &proposal, &observed) };
    let mut indexed = TestHost { sink: DecisionSink::Indexed, ..TestHost::default() };
    assert_eq!(
        decisions::record_outcome(&cell, &mut indexed, &mut active, &committing),
        Err(Error::Refused { label: LIFECYCLE.to_owned(), reason: "the event queue is not in Rust".into() })
    );
    assert_eq!(indexed.calls.len(), 1);
    assert!(active.is_empty());
    let remembering = Recording { remembers: true, ..recording(&data, &proposal, &observed) };
    match decisions::record_outcome(&cell, &mut host, &mut active, &remembering) {
        Err(Error::Refused { label, .. }) => assert_eq!(label, MEMORY),
        other => panic!("{other:?}"),
    }
    // A sink in Python: nothing is done here.
    let mut python = TestHost { sink: DecisionSink::Python, ..TestHost::default() };
    assert_eq!(decisions::record_outcome(&cell, &mut python, &mut active, &committing), Ok(Sunk::Python));
    assert!(python.calls.is_empty() && active.is_empty());

    // With the queue, the log and the event counter, the action starts with its receipt.
    let mut world = world();
    world.scalars.get_mut().duration_minutes = Fit::Ok(2000);
    let index = world.slots.add(text(ID_COUNTERS), Place::Field).unwrap();
    world.slots.at(index).unwrap().try_borrow_mut().unwrap().row = Some(Box::new(ValueRow(v(&json!({"event": 5})))));
    world.hold_queue(Arc::new(Queue::new()));
    world.hold_log(Arc::new(EventLog::new()));
    let cell = Shard::new(world);
    assert_eq!(decisions::record_outcome(&cell, &mut host, &mut active, &committing), Ok(Sunk::Recorded));
    assert!(active.contains("A1"));
    let receipt = cell
        .with(|world| {
            let log = world.log().unwrap();
            assert_eq!(log.len(), 1);
            serde_json::from_slice::<Value>(log.written(0).unwrap()).unwrap()
        })
        .unwrap();
    assert_eq!(receipt["event"]["event_id"], json!("action-lifecycle-5"));
    let payload = &receipt["event"]["payload"];
    assert_eq!((&payload["action_id"], &payload["action"], &payload["transition"]), (&json!("action-5"), &json!(proposal.outcome.action.value()), &json!("started")));
    assert_eq!(payload["target_id"], json!("X"));
}

#[test]
fn the_observed_state_has_the_keys_of_the_snapshot() {
    let world = world();
    let data = view_of(&world);
    let index = data.index_of("A1").unwrap();
    let observed = behavior::observed_value(&data, index);
    let V::Map(map) = &observed else { panic!() };
    assert_eq!(
        map.keys().map(|key| &**key).collect::<Vec<_>>(),
        ["agent_id", "zone_id", "activity", "target_id", "needs", "psychological", "sanitation_level", "stress", "trust", "queue", "support", "zone_environment"]
    );
    // The bytes of the indexed record are the same document, with the keys in its order.
    let mut written = sheltersim_core::json::Encoder::default();
    decisions::write_observed(&mut written, &data, index).unwrap();
    let parsed: Value = serde_json::from_slice(&written.out).unwrap();
    assert_eq!(parsed, serde_json::from_slice::<Value>(&observed.to_json().unwrap()).unwrap());
    assert!(written.out.starts_with(br#"{"agent_id":"A1","zone_id":"room","activity":"idle","target_id":null,"needs":{"toilet_urgency":0.0,"#));
}
