//! The psychology of lane P8 a3 (`rules::psychology`, `rules::trace`) over a world built here, with
//! no Python.
//!
//! The world is the one `psych_world.py` builds in Python (lane notes of P8 a3; the view's world of
//! `world_view.rs` at the last minute of a day, with body records, a day of distress and monitor
//! values). `data/psychology.json` holds what the PyO3 kernels before this lane (`main` `b508cb1`)
//! gave for it with each model: the deltas' bits, the digests of the trace document and of its
//! `agent_contributions` (their canonical JSON and the order of their keys), and the people, body
//! records, distress records and monitor values after the long-term step and the writing of the
//! final deltas. Here the core gives the same.

mod support;

use std::sync::Arc;

use indexmap::IndexMap;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sheltersim_core::config::{Fault, FaultClass, Selection};
use sheltersim_core::entities::{
    Activity, AgentRow, AnyTable, FacilityRow, Mobility, NeedRow, ProfileRow, RequestRow, RequestStatus, Table, TaskRow, ZoneRow,
};
use sheltersim_core::error::Error;
use sheltersim_core::host::{Trace, TraceLevel};
use sheltersim_core::row::{IdMap, Row, ValueRow};
use sheltersim_core::rules::psychology::{self as psychology, Applied, Bad};
use sheltersim_core::rules::trace::{self, TraceInputs};
use sheltersim_core::rules::world_view::{self as view, Inputs, ViewCache, ViewData};
use sheltersim_core::run::held::{Class, Held, Obj};
use sheltersim_core::run::psychology::{Model, Psychology};
use sheltersim_core::state::body_state::{BodyRecord, BodyRow, History, Relationships};
use sheltersim_core::state::information::{BeliefRow, CountersRow, InformationRow, MessageRow};
use sheltersim_core::state::metric_state::MetricRow;
use sheltersim_core::state::psychological_distress::{AgentRecord, DistressRow};
use sheltersim_core::value::V;
use sheltersim_core::world::{Fit, Place, World, METRIC_STATE, MODEL_CONFIGS};
use support::{Call, TestHost};

const MINUTE: i64 = 1439;

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

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The keys of every map in order, depth first, as `order` of `psych_world.py` writes them.
fn order(value: &V, out: &mut Vec<String>) {
    match value {
        V::Map(map) => {
            out.push(format!("{{{}}}", map.keys().map(|key| &**key).collect::<Vec<_>>().join(",")));
            for item in map.values() {
                order(item, out);
            }
        }
        V::List(items) => {
            for item in items {
                order(item, out);
            }
        }
        _ => {}
    }
}

fn digests(value: &V) -> (String, String) {
    let mut keys = Vec::new();
    order(value, &mut keys);
    (sha(&value.to_json().unwrap()), sha(keys.join("|").as_bytes()))
}

fn expected() -> Value {
    let raw = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/psychology.json")).unwrap();
    serde_json::from_str(&raw).unwrap()
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
    let mut query_pressure: IdMap<f64> = IdMap::default();
    query_pressure.insert(text("A1"), 0.125);
    query_pressure.insert(text("A2"), 0.5);
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

/// The world of `psych_world.py`.
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
        a1.activity = Activity::Queued;
        a1.target_id = Some(text("f-toilet"));
        a1.next_decision_at = 31;
        put(&mut rows.agents, "A1", a1);
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
            "start_event_id": "e-z1", "source_id": "src-z", "assumption_ids": ["asm-z"]}}}),
    );
    world
}

fn view_of(world: &World) -> ViewData {
    let selection = Ok(Arc::new(Selection::new(vec![(text("psychology"), text("life-state-feedback-v1"))])));
    let inputs = Inputs { selection: &selection, configs: None };
    view::build(world, &inputs, &mut ViewCache::default()).unwrap().finish(false)
}

// ---- The policies, as the Run reads them ---------------------------------------------------------

fn attribute_error(name: &str) -> Result<Held, Fault> {
    Err(Fault::new(FaultClass::Other(text("builtins.AttributeError")), format!("object has no attribute '{name}'")))
}

fn object(module: &str, name: &str, attrs: Vec<(&str, Result<Held, Fault>)>) -> Held {
    let mut found = Obj::new(Class::named(module, name));
    for (key, held) in attrs {
        found.attrs.insert(text(key), held);
    }
    Held::Object(Arc::new(found))
}

fn value(value: V) -> Result<Held, Fault> {
    Ok(Held::Value(value))
}

/// `controller.psychology` of `psych_world.py`'s model `name`, as `Policies` reads it.
fn psychology_of(name: &str, expected: &Value) -> Psychology {
    let entry = &expected[name];
    let parameters = {
        let fields = expected["params"].as_object().unwrap();
        object(
            "sheltersim.policies.psychology",
            "PsychologyParameters",
            fields.iter().map(|(key, item)| (key.as_str(), value(v(item)))).collect(),
        )
    };
    let class = entry["class"].as_array().unwrap();
    let (module, qualname) = (class[0].as_str().unwrap(), class[1].as_str().unwrap());
    let flag = |key: &str| value(V::Bool(entry[key].as_bool().unwrap()));
    let long_term = if entry["has_long_term_method"].as_bool().unwrap() {
        Ok(Held::Opaque { callable: true })
    } else {
        attribute_error("update_long_term_state")
    };
    let mut attrs = vec![
        ("policy_version", value(V::Str(text(class_version(name))))),
        ("source_id", value(V::Str(text(entry["source_id"].as_str().unwrap())))),
        ("parameters", Ok(parameters)),
        ("update_long_term_state", long_term),
    ];
    match name {
        "life" | "integrated" => {
            let base = object("sheltersim.policies.psychology", "SimplePsychologyPolicy", vec![("include_direct_rest_stress_recovery", flag("direct_rest"))]);
            let (config, formula) = if name == "life" {
                let formula = object(
                    "sheltersim.policies.registry",
                    "BodyDynamicsFormula",
                    vec![("model_id", value(V::Str(text("body-feedback-complete-v1"))))],
                );
                (v(&expected["body_config"]), Ok(formula))
            } else {
                (v(&expected["body_config_default"]), value(V::Null))
            };
            attrs.push(("include_direct_rest_stress_recovery", attribute_error("include_direct_rest_stress_recovery")));
            attrs.push(("use_tick_start_stress_for_feedback", flag("tick_start")));
            attrs.push(("base", Ok(base)));
            attrs.push(("body_config", value(config)));
            attrs.push(("body_formula", formula));
        }
        _ => attrs.push(("include_direct_rest_stress_recovery", flag("direct_rest"))),
    }
    let policy = object(module, qualname, attrs);
    let mut controller = Obj::new(Class::named("sheltersim.experiments.runner", "OfflineSimulationController"));
    controller.attrs.insert(text("psychology"), Ok(policy));
    Psychology::of(&controller)
}

fn class_version(name: &str) -> &'static str {
    match name {
        "simple" => "causal-stress-v1",
        "bounded" => "bounded-stress-v1",
        "mean" => "mean-reverting-distress-v1",
        "life" => "life-state-feedback-v1",
        _ => "mean-reverting-life-state-v1",
    }
}

fn refused(bad: Bad) -> Error {
    match bad {
        Bad::Refused(error) => error,
        Bad::Policy(fault) => panic!("a fault of the policies: {:?}", fault.fault),
    }
}

// ---- The tests -----------------------------------------------------------------------------------

/// One model's minute as the update phase runs it: the step with a full trace, the trace with A1's
/// final stress changed (as the social step may), the long-term step and the writing.
fn run_model(name: &str, expected: &Value) {
    let entry = &expected[name];
    let world = world();
    let data = view_of(&world);
    let policy = psychology_of(name, expected);
    let model = psychology::model_of(&policy).map_err(refused).unwrap();
    let params = psychology::params(&policy, model, Some(data.resource_service_enabled), true).map_err(refused).unwrap();
    let result = psychology::update(&world, &data, &params, MINUTE, true).unwrap();
    assert_eq!(&*result.policy_version, entry["policy_version"].as_str().unwrap(), "{name}");

    let mut deltas = Vec::new();
    for delta in result.deltas.iter() {
        let mut found = vec![Value::String(delta.agent_id.to_string())];
        let values = [delta.toilet_urgency, delta.hunger, delta.fatigue, delta.hydration_need, delta.stress, delta.trust];
        found.extend(values.iter().map(|value| Value::String(hex(*value))));
        match &delta.body {
            Some(body) => found.extend(body.iter().map(|value| Value::String(hex(*value)))),
            None => found.extend(std::iter::repeat_n(Value::Null, 7)),
        }
        deltas.push(Value::Array(found));
    }
    assert_eq!(Value::Array(deltas), entry["deltas"], "{name}: the deltas");

    // The final deltas: A1's stress is 0.75.
    let finals: Vec<(&str, f64)> =
        result.deltas.iter().map(|delta| (&*delta.agent_id, if &*delta.agent_id == "A2" { delta.stress } else { 0.75 })).collect();
    let mut host = TestHost { level: TraceLevel::Full, ..TestHost::default() };
    let source_id = V::Str(text(entry["source_id"].as_str().unwrap()));
    let version = V::Str(result.policy_version.clone());
    let inputs = TraceInputs { source_id: &source_id, policy_version: &version, minute: &V::Int(MINUTE) };
    trace::record_psychology(&mut host, &data, result.traces.as_deref().unwrap(), &finals, &inputs).unwrap();
    let [Call::Trace(Trace::Psychology { document, contributions })] = &host.calls[..] else {
        panic!("{name}: not one psychology trace: {:?}", host.calls)
    };
    let (canonical, keys) = digests(document);
    assert_eq!(canonical, entry["document"].as_str().unwrap(), "{name}: the document");
    assert_eq!(keys, entry["document_order"].as_str().unwrap(), "{name}: the document's order");
    let (canonical, keys) = digests(contributions);
    assert_eq!(canonical, entry["contributions"].as_str().unwrap(), "{name}: the contributions");
    assert_eq!(keys, entry["contributions_order"].as_str().unwrap(), "{name}: their order");
    if let Some(text) = entry.get("document_text") {
        assert_eq!(String::from_utf8(document.to_json().unwrap()).unwrap(), text.as_str().unwrap());
    }

    if psychology::keeps_long_term(&policy).map_err(refused).unwrap() {
        let long_term = psychology::long_term_of(&policy).map_err(refused).unwrap();
        psychology::update_long_term(&world, &long_term, &finals, MINUTE, true).unwrap();
    }
    let applied: Vec<Applied<'_>> = result
        .deltas
        .iter()
        .zip(&finals)
        .map(|(delta, (_, stress))| Applied {
            agent_id: &delta.agent_id,
            values: [delta.toilet_urgency, delta.hunger, delta.fatigue, delta.hydration_need, *stress, delta.trust],
            body: delta.body.map_or([None; 7], |body| body.map(Some)),
        })
        .collect();
    psychology::apply(&world, &applied, true).unwrap();

    let rows = world.rows().unwrap();
    for (id, values) in entry["agents"].as_object().unwrap() {
        let row = rows.agents.get(id.as_str()).unwrap();
        let found = [row.stress, row.trust, row.needs.toilet_urgency, row.needs.hunger, row.needs.fatigue, row.needs.hydration_need];
        let found: Vec<Value> = found.iter().map(|value| Value::String(hex(*value))).collect();
        assert_eq!(&Value::Array(found), values, "{name}: {id}");
    }
    let body = world.row::<BodyRow>("body_state_v1").unwrap();
    for (id, values) in entry["body"].as_object().unwrap() {
        let record = body.agents.get(id.as_str()).unwrap();
        let found = [
            record.sleep_debt_minutes,
            record.uncertainty,
            record.reassurance,
            record.discontent,
            record.positive_affect,
            record.negative_affect,
            record.hygiene_need,
        ];
        let found: Vec<Value> = found.iter().map(|value| Value::String(hex(*value))).collect();
        assert_eq!(&Value::Array(found), values, "{name}: the body record of {id}");
    }
    let distress = world.row::<DistressRow>("psychological_distress_v1").unwrap();
    assert_eq!(distress.last_updated_minute, entry["distress_last"].as_i64().unwrap(), "{name}");
    let records: serde_json::Map<String, Value> = distress
        .agents
        .iter()
        .map(|(id, record)| {
            (
                id.to_string(),
                json!([
                    hex(record.baseline_distress),
                    hex(record.long_term_distress),
                    hex(record.daily_acute_sum),
                    record.daily_high_distress_minutes,
                    record.daily_observed_minutes,
                    record.completed_days
                ]),
            )
        })
        .collect();
    assert_eq!(Value::Object(records), entry["distress"], "{name}: the distress records and their order");
    let metrics = world.metric_state().unwrap();
    let monitor: Vec<Value> = metrics.iter().map(|(key, value)| json!([key.to_string(), hex(*value)])).collect();
    assert_eq!(Value::Array(monitor), entry["monitor"], "{name}: the monitor's values and their order");
}

#[test]
fn each_model_gives_the_kernel_before_s_deltas_trace_and_writes() {
    let expected = expected();
    for name in ["simple", "bounded", "mean", "life", "integrated"] {
        run_model(name, &expected);
    }
}

#[test]
fn without_a_trace_nothing_is_kept_and_the_version_is_the_default() {
    let expected = expected();
    let world = world();
    let data = view_of(&world);
    let policy = psychology_of("integrated", &expected);
    let params = psychology::params(&policy, Model::Integrated, Some(data.resource_service_enabled), false).map_err(refused).unwrap();
    let result = psychology::update(&world, &data, &params, MINUTE, false).unwrap();
    assert!(result.traces.is_none());
    assert_eq!(&*result.policy_version, psychology::DEFAULT_POLICY_VERSION);
    // The same deltas as with a trace.
    let traced = psychology::params(&policy, Model::Integrated, Some(data.resource_service_enabled), true).map_err(refused).unwrap();
    assert_eq!(psychology::update(&world, &data, &traced, MINUTE, true).unwrap().deltas, result.deltas);
    // A trace that is off records nothing.
    let mut host = TestHost::default();
    let inputs = TraceInputs { source_id: &V::Null, policy_version: &V::Null, minute: &V::Int(MINUTE) };
    let traces = psychology::update(&world, &data, &traced, MINUTE, true).unwrap().traces.unwrap();
    trace::record_psychology(&mut host, &data, &traces, &[], &inputs).unwrap();
    assert!(host.calls.is_empty());
    // A person without a final delta is refused.
    host.level = TraceLevel::Summary;
    let error = trace::record_psychology(&mut host, &data, &traces, &[("A2", 0.5)], &inputs).unwrap_err();
    assert_eq!(error.to_string(), "a person has no final delta");
    assert!(host.calls.is_empty());
}

#[test]
fn what_the_kernel_turned_down_is_refused_before_anything_changes() {
    let expected = expected();
    let world = world();
    let data = view_of(&world);
    let policy = psychology_of("integrated", &expected);
    // The view's flag, a view Python made, another minute's namespace.
    let error = psychology::params(&policy, Model::Integrated, None, false).map(|_| ()).map_err(refused).unwrap_err();
    assert_eq!(error.to_string(), "a flag is not a bool");
    let params = psychology::params(&policy, Model::Integrated, Some(false), true).map_err(refused).unwrap();
    let mut partial = data.clone();
    partial.partial = true;
    let error = psychology::update(&world, &partial, &params, MINUTE, true).unwrap_err();
    assert_eq!(error.to_string(), "a trace reads what a view Python made doesn't fill here");
    let long_term = psychology::long_term_of(&policy).map_err(refused).unwrap();
    let finals = [("A1", 0.5), ("A2", 0.5)];
    let error = psychology::update_long_term(&world, &long_term, &finals, MINUTE - 1, true).unwrap_err();
    assert_eq!(error.to_string(), "the namespace is not at the previous minute");
    let error = psychology::update_long_term(&world, &long_term, &finals, MINUTE, false).unwrap_err();
    assert_eq!(error.to_string(), "the field holds another dict");
    let error = psychology::update_long_term(&world, &long_term, &[("A1", 0.5), ("A1", 0.5)], MINUTE, true).unwrap_err();
    assert_eq!(error.to_string(), "two deltas for one person");
    let error = psychology::update_long_term(&world, &long_term, &[("A1", 0.5)], MINUTE, true).unwrap_err();
    assert_eq!(error.to_string(), "the world and the deltas have different people");
    // Two deltas of one person, body values without the namespace, a person the world lacks.
    let one = Applied { agent_id: "A1", values: [0.5; 6], body: [Some(1.0); 7] };
    let error = psychology::apply(&world, &[one, one], true).unwrap_err();
    assert_eq!(error.to_string(), "two deltas for one person");
    let error = psychology::apply(&world, &[one], false).unwrap_err();
    assert_eq!(error.to_string(), "the delta has body values and the world no body records");
    let nobody = Applied { agent_id: "A9", values: [0.5; 6], body: [None; 7] };
    let error = psychology::apply(&world, &[one, nobody], true).unwrap_err();
    assert_eq!(error.to_string(), "a delta names nobody in the world");
    // Nothing changed.
    let rows = world.rows().unwrap();
    assert_eq!(rows.agents.get("A1").unwrap().stress, 0.0);
    drop(rows);
    assert_eq!(world.row::<BodyRow>("body_state_v1").unwrap().agents.get("A1").unwrap().sleep_debt_minutes, 120.0);
    assert_eq!(world.row::<DistressRow>("psychological_distress_v1").unwrap().last_updated_minute, MINUTE - 1);
}

#[test]
fn the_policy_s_reads_are_refused_or_read_again_where_the_kernel_read_them() {
    let expected = expected();
    // A class this kernel does not know, by its names.
    let mut controller = Obj::new(Class::named("sheltersim.experiments.runner", "OfflineSimulationController"));
    controller.attrs.insert(text("psychology"), Ok(object("tests", "OwnRules", vec![])));
    let error = psychology::model_of(&Psychology::of(&controller)).map_err(|bad| match bad {
        Bad::Policy(fault) => fault.refusal().map(str::to_owned),
        Bad::Refused(_) => None,
    });
    assert_eq!(error.unwrap_err().as_deref(), Some("the policy is not one this kernel knows"));
    // A controller without `psychology`: read again from the controller.
    let mut controller = Obj::new(Class::named("types", "SimpleNamespace"));
    controller.attrs.insert(text("psychology"), attribute_error("psychology"));
    let Err(Bad::Policy(fault)) = psychology::model_of(&Psychology::of(&controller)) else { panic!("not a fault") };
    assert_eq!((fault.path.as_slice(), fault.refusal()), (&["psychology"][..], None));
    // An int rate where a float is read, an int beyond 2**53, a table that is not a dict.
    for (field, item, reason) in [
        ("toilet_growth", json!(0), "a value is not a float"),
        ("hydration_growth", json!(9_007_199_254_740_993_i64), "an int value is too large"),
        ("hunger_growth_by_agent", json!([1]), "a table of the policy is not a plain dict"),
    ] {
        let mut changed = expected.clone();
        changed["params"][field] = item;
        let policy = psychology_of("simple", &changed);
        let error = psychology::params(&policy, Model::Simple, Some(false), false).map(|_| ()).map_err(refused).unwrap_err();
        assert_eq!(error.to_string(), reason, "{field}");
    }
    // Another model with long-term state of its own; a model without it does nothing.
    let mut changed = expected.clone();
    changed["simple"]["has_long_term_method"] = json!(true);
    let error = psychology::keeps_long_term(&psychology_of("simple", &changed)).map_err(refused).unwrap_err();
    assert_eq!(error.to_string(), "a model's long-term state this kernel does not know");
    assert!(!psychology::keeps_long_term(&psychology_of("simple", &expected)).map_err(refused).unwrap());
    assert!(psychology::keeps_long_term(&psychology_of("mean", &expected)).map_err(refused).unwrap());
}

#[test]
fn a_social_document_keeps_its_sample_and_its_numbers_types() {
    use sheltersim_core::contacts::Basis;
    use sheltersim_core::mathx::Num;
    let contact = |recipient: usize, contribution: Num| trace::SocialContact {
        recipient,
        source: 1 - recipient,
        basis: Basis::SameZone,
        minutes: 3,
        cooldown_remaining: 0,
        difference: 0.25,
        contribution,
        weight: 0.5,
        aggregate_rate: 0.125,
        pre_pair_cap: 0.0625,
        pair_cap: Num::Int(0),
        aggregate_scale: 1.0,
    };
    let contacts: Vec<_> = (0..25).map(|place| contact(place % 2, if place % 3 == 0 { Num::Int(0) } else { Num::Float(0.5) })).collect();
    let document =
        trace::social_document(&V::Int(7), &V::Str(text("expressiveness-channel-openness-v1")), &["A1", "A2"], &contacts, 16, 8.0);
    let V::Map(map) = &document else { panic!("not a map") };
    let keys: Vec<&str> = map.keys().map(|key| &**key).collect();
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
    assert_eq!(map["truncated_contact_count"], V::Int(5));
    assert_eq!(map["aggregation"], V::Str(text("snapshot weighted synchrony; pair cap then recipient cap then [0,1] bound")));
    let (V::List(sample), V::List(contributions)) = (&map["contacts"], &map["contributions"]) else { panic!("not lists") };
    assert_eq!((sample.len(), contributions.len()), (20, 16));
    let V::Map(first) = &sample[0] else { panic!("not a map") };
    assert_eq!((&first["stress_contribution"], &first["pair_cap_contribution"]), (&V::Int(0), &V::Int(0)));
    let legacy = trace::social_document(&V::Int(7), &V::Str(text("x")), &["A1", "A2"], &[], 0, 0.0);
    let V::Map(legacy) = legacy else { panic!("not a map") };
    assert_eq!(legacy["aggregation"], V::Str(text("legacy directed positive stress propagation")));
}
