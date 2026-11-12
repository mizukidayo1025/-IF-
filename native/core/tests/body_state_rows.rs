//! `record_social_event` of `body_state_v1` (`rules::body_state`), and `observe_event` of
//! `social_network_v1` reading what it wrote, on the worlds of `integration/test_body_state_row.py`:
//! the Project screen's generated Scenario at minute 0 with a new body state, under each pair of
//! the affect and relationship models (`people_rows.json`'s `body*` worlds, written by
//! `tests/support/core_people_rows.py`), with the inputs that file made with `random.Random`
//! (Issue #225, lane M5). The PyO3 entry (`native/src/rules/body_state.rs`) reads the event and the
//! configs' models through the host and calls this kernel; the pytest file keeps an entry test, and
//! the tests of the boundary: the row and its tree, what taints it, each way of writing that the
//! check must see, a model the Python selection names, and the comparisons with the Python
//! validation still in `src/`.
//!
//! The digests the pytest file pinned from the Python rule (`PYTHON_GAVE`: `json.dumps` of the
//! trees it left, each history item's keys sorted) are kept: the tests write the same text with
//! `Dumps`, each record's fields in the order of the tree (`NAMES`) and the config's keys in the
//! order its rule made them (`body_config_order`), as the PyO3 crate gives the trees to Python.

mod support;

use std::sync::{Arc, OnceLock};

use serde_json::Value;
use sheltersim_core::cell::Shard;
use sheltersim_core::error::Error;
use sheltersim_core::json::Encoder;
use sheltersim_core::row::Field;
use sheltersim_core::rules::body_state::{self as body, Inputs, SocialEvent};
use sheltersim_core::rules::social_network;
use sheltersim_core::state::body_state::{BodyRecord, BodyRow, Relationship, MAX_SOCIAL_RELATIONSHIPS, NAME as BODY};
use sheltersim_core::state::social_network::{Relationship as SocialRelationship, SocialNetworkRow, NAME as NETWORK};
use sheltersim_core::value::V;
use sheltersim_core::world::{World, WorldCell};

use support::dumps::Dumps;
use support::{cases, load, texts};

const FILE: &str = "people_rows";

// ---- The fixture --------------------------------------------------------------------------------

fn data() -> &'static Value {
    static DATA: OnceLock<Value> = OnceLock::new();
    DATA.get_or_init(|| load::data(FILE))
}

fn inputs() -> &'static Value {
    &data()["inputs"]
}

/// A named world of the file, and what the PyO3 entry reads of its configs through the host.
fn held(name: &str) -> (Shard<World>, Inputs) {
    let entry = cases::resolve(&data()["worlds"], name);
    let cell = texts::cell(&entry);
    let inputs = Inputs::read(&cell, &mut load::host(&entry)).unwrap();
    (cell, inputs)
}

/// The first two people, by ID (`_pair`).
fn pair(world: &World) -> (String, String) {
    let mut ids: Vec<String> = world.rows().unwrap().agents.iter().map(|(id, _)| id.to_string()).collect();
    ids.sort();
    (ids[0].clone(), ids[1].clone())
}

fn digest(text: &str) -> String {
    texts::sha(text)[..16].to_owned()
}

fn body_json(world: &World) -> Vec<u8> {
    let mut encoder = Encoder::with_capacity(4096);
    world.row::<BodyRow>(BODY).unwrap().write_json(&mut encoder).unwrap();
    encoder.out
}

/// The payload's fields the rule reads of an event of this type, as its PyO3 entry reads them.
fn keys_of(event_type: &str) -> &'static [&'static str] {
    match event_type {
        "proximity_contact_update" => &[
            "contact_minutes", "message_id", "topic_id", "interaction_duration_minutes", "first_agent_id", "second_agent_id", "observed_minute", "distance_m",
        ],
        "information_contact_update" => &["contact_minutes", "message_id", "topic_id", "interaction_duration_minutes", "sender_agent_id", "recipient_agent_id"],
        "social_interaction" => &[
            "contact_minutes", "message_id", "topic_id", "interaction_duration_minutes", "interaction_type", "agent_id", "participant_agent_id",
        ],
        _ => &["contact_minutes", "message_id", "topic_id", "interaction_duration_minutes", "zone_id"],
    }
}

/// `record_social_event` of `_event(kind, payload, number)` at `minute`, as its PyO3 entry calls it:
/// the event `event-<number>`, caused by `cause` for an even number.
fn record(cell: &Shard<World>, inputs: &Inputs, event_type: &str, payload: &Value, number: usize, minute: i64) -> Result<(), Error> {
    let models = inputs.models.as_ref().unwrap_or_else(|fault| panic!("the models raised: {fault:?}"));
    let fields = keys_of(event_type)
        .iter()
        .filter_map(|key| payload.get(*key).map(|value| (Arc::from(*key), load::v(value))))
        .collect();
    let payload = V::Map(fields);
    let event_id = format!("event-{number}");
    let event = SocialEvent { event_id: &event_id, event_type, causation_id: if number % 2 == 1 { None } else { Some("cause") }, payload: &payload };
    cell.with(|world| body::record_social_event(world, &event, minute, &inputs.selection, models)).unwrap()
}

// ---- The trees the pytest file pinned -------------------------------------------------------------

fn text(value: &str) -> V {
    V::Str(Arc::from(value))
}

fn optional(value: &Option<Arc<str>>) -> V {
    value.as_deref().map_or(V::Null, text)
}

fn strings(values: &[Arc<str>]) -> V {
    V::List(values.iter().map(|value| text(value)).collect())
}

fn named(names: &[&str], values: impl IntoIterator<Item = V>) -> V {
    V::Map(names.iter().zip(values).map(|(name, value)| (Arc::from(*name), value)).collect())
}

/// The body state's tree as Python holds it (each record's fields in the order of the tree,
/// `NAMES`), with each history item's keys sorted (`_with_canonical_history`).
fn body_tree(row: &BodyRow) -> V {
    let agents = row
        .agents
        .iter()
        .map(|(id, record): (&Arc<str>, &BodyRecord)| {
            let values = [
                record.sleep_debt_minutes,
                record.uncertainty,
                record.reassurance,
                record.discontent,
                record.positive_affect,
                record.negative_affect,
                record.hygiene_need,
            ];
            (id.clone(), named(BodyRecord::NAMES, values.map(V::Float)))
        })
        .collect();
    let relationships = row
        .relationships
        .map
        .iter()
        .map(|(key, record): (&Arc<str>, &Relationship)| {
            let values = [
                V::Int(record.contact_count),
                record.trust.map_or(V::Null, V::Float),
                V::Int(record.contact_minutes),
                record.last_contact_minute.map_or(V::Null, V::Int),
                optional(&record.last_interaction),
                strings(&record.topic_ids),
            ];
            (key.clone(), named(Relationship::NAMES, values))
        })
        .collect();
    let history = row.history.items.iter().map(|item| load::v(&serde_json::from_slice(item).unwrap())).collect();
    named(
        BodyRow::NAMES,
        [
            V::Int(row.schema_version),
            text(&row.policy_version),
            text(&row.source_id),
            config_tree(&row.config),
            V::Map(agents),
            V::Map(relationships),
            V::List(history),
        ],
    )
}

/// The body config's tree: its keys in the order its rule made the dict (`body_config_order`).
fn config_tree(config: &V) -> V {
    let V::Map(config) = config else { panic!("the body's config is a map") };
    let order = inputs()["expected"]["body_config_order"].as_array().unwrap();
    assert_eq!(order.len(), config.len());
    V::Map(order.iter().map(|key| (Arc::from(key.as_str().unwrap()), config[key.as_str().unwrap()].clone())).collect())
}

/// The social network's tree as Python holds it.
fn network_tree(row: &SocialNetworkRow) -> V {
    let relationships = row
        .relationships
        .map
        .iter()
        .map(|(key, record): (&Arc<str>, &SocialRelationship)| {
            let values = [
                V::Int(record.contact_count),
                V::Int(record.conversation_count),
                V::Int(record.cooperation_count),
                record.trust.map_or(V::Null, V::Float),
                strings(&record.shared_message_ids),
                record.last_contact_minute.map_or(V::Null, V::Int),
                optional(&record.last_interaction),
            ];
            (key.clone(), named(SocialRelationship::NAMES, values))
        })
        .collect();
    named(
        SocialNetworkRow::NAMES,
        [
            V::Int(row.schema_version),
            text(&row.policy_version),
            V::Map(relationships),
            V::Int(row.interaction_count),
            V::Int(row.information_transfer_count),
        ],
    )
}

/// `_digest(*values)`: each value's `json.dumps`, one per line.
fn python_gave(values: &[V]) -> String {
    let lines: Vec<String> = values.iter().map(|value| Dumps::default().text(value)).collect();
    digest(&lines.join("\n"))
}

fn body_gave(cell: &Shard<World>) -> String {
    cell.with(|world| python_gave(&[body_tree(&world.row::<BodyRow>(BODY).unwrap())])).unwrap()
}

// ---- The rule against its Python base ------------------------------------------------------------

/// `test_each_event_is_python_s_bit_for_bit`: under each pair of models, two people's random records
/// and three rounds of every kind of event, each 7 minutes after the last.
#[test]
fn each_event_is_python_s_bit_for_bit() {
    // By affect, then relationship model (the pytest file's ids are `[relationship-affect]`).
    let recorded = [["005dcea969a92e5b", "8f1caed430a305f8"], ["fdb7e9d9e11c0aad", "681c8392395a1d95"]];
    let payloads = inputs()["body_payloads"].as_array().unwrap();
    for affect in 0..2 {
        for relationship in 0..2 {
            let (cell, inputs) = held(&format!("body_models.{affect}.{relationship}"));
            for (number, item) in payloads.iter().chain(payloads).chain(payloads).enumerate() {
                record(&cell, &inputs, item[0].as_str().unwrap(), &item[1], number, 10 + 7 * number as i64).unwrap();
            }
            assert_eq!(body_gave(&cell), recorded[affect][relationship], "{affect} {relationship}");
        }
    }
}

/// `test_many_random_events_are_python_s_bit_for_bit`: eight people's random records, and 168
/// events of random pairs with random distances, contact minutes and durations.
#[test]
fn many_random_events_are_python_s_bit_for_bit() {
    let (cell, inputs) = held("body_random");
    for (number, item) in self::inputs()["random_body_events"].as_array().unwrap().iter().enumerate() {
        record(&cell, &inputs, item[0].as_str().unwrap(), &item[1], number, 10 + number as i64).unwrap();
    }
    assert_eq!(body_gave(&cell), "3a1ec129bd6d269a");
}

/// `test_a_positions_distance_reaches_trust_and_the_deltas`.
#[test]
fn a_positions_distance_reaches_trust_and_the_deltas() {
    let (cell, inputs) = held("body_positions");
    let (first, second) = cell.with(|world| pair(world)).unwrap();
    let payload = serde_json::json!({"sender_agent_id": first, "recipient_agent_id": second});
    record(&cell, &inputs, "information_contact_update", &payload, 1, 10).unwrap();
    assert_eq!(body_gave(&cell), "f734bed8be073263");
    let last: Value = cell.with(|world| serde_json::from_slice(world.row::<BodyRow>(BODY).unwrap().history.items.back().unwrap()).unwrap()).unwrap();
    assert!(last["deltas"]["contact_distance_m"].as_f64().unwrap() > 0.0);
}

/// `test_a_correction_is_python_s_bit_for_bit`: a correction in the first person's zone, where the
/// first took the claim from this event and the last from another; with the evidence-aware model only
/// the first is moved.
#[test]
fn a_correction_is_python_s_bit_for_bit() {
    let zone = inputs()["expected"]["correction_zone"].clone();
    for (evidence_aware, recorded) in [(0, "34db09179e3303c2"), (1, "42dcc7a70bd10934")] {
        let (cell, inputs) = held(&format!("body_correction.{evidence_aware}"));
        let payload = serde_json::json!({"zone_id": zone, "message_id": "M"});
        record(&cell, &inputs, "information_correction", &payload, 1, 10).unwrap();
        assert_eq!(body_gave(&cell), recorded, "{evidence_aware}");
        let actors: Vec<Value> = cell
            .with(|world| {
                let row = world.row::<BodyRow>(BODY).unwrap();
                row.history.items.iter().map(|item| serde_json::from_slice::<Value>(item).unwrap()["actor_ids"].clone()).collect()
            })
            .unwrap();
        assert_eq!(Value::Array(actors), self::inputs()["expected"][format!("correction_actors.{evidence_aware}")], "{evidence_aware}");
    }
}

/// `test_the_relationship_limit_stops_new_pairs_only`: with the relationships at their limit, a
/// contact of a new pair moves the affect and leaves its history item, without a relationship.
#[test]
fn the_relationship_limit_stops_new_pairs_only() {
    let (cell, inputs) = held("body");
    let (first, second, others) = cell
        .with(|world| {
            let mut ids: Vec<String> = world.rows().unwrap().agents.iter().map(|(id, _)| id.to_string()).collect();
            ids.sort();
            (ids[0].clone(), ids[1].clone(), (ids[2].clone(), ids[3].clone()))
        })
        .unwrap();
    cell.with(|world| {
        let mut row = world.row_mut::<BodyRow>(BODY).unwrap();
        for number in 0..MAX_SOCIAL_RELATIONSHIPS {
            let record = Relationship {
                contact_count: 0,
                trust: Some(0.5),
                contact_minutes: 0,
                last_contact_minute: None,
                last_interaction: None,
                topic_ids: Vec::new(),
            };
            row.relationships.map.insert(Arc::from(format!("[\"{}\",\"{}-{number}\"]", others.0, others.1)), record);
        }
    })
    .unwrap();
    let payload = serde_json::json!({"sender_agent_id": first, "recipient_agent_id": second});
    record(&cell, &inputs, "information_contact_update", &payload, 1, 10).unwrap();
    cell.with(|world| {
        let row = world.row::<BodyRow>(BODY).unwrap();
        assert!(!row.relationships.map.contains_key(format!("[\"{second}\",\"{first}\"]").as_str()));
        let kinds: Vec<Value> = row.history.items.iter().map(|item| serde_json::from_slice::<Value>(item).unwrap()["kind"].clone()).collect();
        assert_eq!(kinds, [Value::from("information_contact")]);
    })
    .unwrap();
}

// ---- social_network_v1 reads what this rule wrote ---------------------------------------------------

/// `test_observe_event_copies_the_trust_this_kernel_wrote`: four interactions of each kind, 30
/// minutes apart, recorded here and then observed by the social network, which copies the trust.
#[test]
fn observe_event_copies_the_trust_this_kernel_wrote() {
    let kinds = ["cooperation", "reassurance_exchange", "family_care", "voluntary_support"];
    let recorded = ["53b1fd25f6b14c3c", "53b2eddd2e54f2af", "7c9c53d7223555b6", "618f054e67e24524"];
    for (kind, recorded) in kinds.iter().zip(recorded) {
        let (cell, inputs) = held("body");
        let (first, second) = cell.with(|world| pair(world)).unwrap();
        for number in 1..5 {
            let payload = serde_json::json!({
                "agent_id": first, "participant_agent_id": second, "interaction_type": kind,
                "topic_id": "T1", "interaction_duration_minutes": 5,
            });
            let minute = 10 + 30 * (number as i64 - 1);
            record(&cell, &inputs, "social_interaction", &payload, number, minute).unwrap();
            let observed = load::v(&serde_json::json!({"interaction_type": kind, "agent_id": first, "participant_agent_id": second}));
            cell.with(|world| social_network::observe_event(world, "social_interaction", &observed, minute)).unwrap().unwrap();
        }
        let gave = cell
            .with(|world| python_gave(&[body_tree(&world.row::<BodyRow>(BODY).unwrap()), network_tree(&world.row::<SocialNetworkRow>(NETWORK).unwrap())]))
            .unwrap();
        assert_eq!(gave, recorded, "{kind}");
        cell.with(|world| {
            let key = format!("[\"{second}\",\"{first}\"]");
            let trust = world.row::<BodyRow>(BODY).unwrap().relationships.map[key.as_str()].trust.unwrap();
            assert_ne!(trust, 0.5);
            assert_eq!(world.row::<SocialNetworkRow>(NETWORK).unwrap().relationships.map[key.as_str()].trust.unwrap().to_bits(), trust.to_bits());
        })
        .unwrap();
    }
}

// ---- What the kernel refuses: no applied event or valid state holds it ----------------------------

fn refused_with(result: Result<(), Error>, why: &str) {
    match result {
        Err(Error::Unsupported(reason)) => assert!(reason.contains(why), "{reason} has no {why}"),
        other => panic!("not refused for {why}: {other:?}"),
    }
}

/// `test_what_the_kernel_refuses_changes_nothing_here`.
#[test]
fn what_the_kernel_refuses_changes_nothing_here() {
    for (event_type, payload, why) in [
        ("information_contact_update", serde_json::json!({"sender_agent_id": 3, "recipient_agent_id": "x"}), "ID"),
        ("proximity_contact_update", serde_json::json!({"distance_m": 2}), "distance"),
        ("proximity_contact_update", serde_json::json!({"distance_m": "far"}), "distance"),
        ("social_interaction", serde_json::json!({"agent_id": ["x"]}), "ID"),
        ("information_correction", serde_json::json!({"zone_id": 5}), "zone"),
    ] {
        let (cell, inputs) = held("body");
        let before = cell.with(|world| body_json(world)).unwrap();
        refused_with(record(&cell, &inputs, event_type, &payload, 1, 10), why);
        assert_eq!(cell.with(|world| body_json(world)).unwrap(), before, "{payload}");
    }
}

/// `test_a_person_without_a_record_is_refused` and `test_a_relationship_without_trust_is_refused`.
#[test]
fn a_person_without_a_record_or_a_relationship_without_trust_is_refused() {
    for (change, why) in [("no record", "no body record"), ("no trust", "no trust")] {
        let (cell, inputs) = held("body");
        let (first, second) = cell.with(|world| pair(world)).unwrap();
        cell.with(|world| {
            let mut row = world.row_mut::<BodyRow>(BODY).unwrap();
            if change == "no record" {
                row.agents.shift_remove(second.as_str());
            } else {
                let record = Relationship {
                    contact_count: 0,
                    trust: None,
                    contact_minutes: 0,
                    last_contact_minute: None,
                    last_interaction: None,
                    topic_ids: Vec::new(),
                };
                row.relationships.map.insert(Arc::from(format!("[\"{second}\",\"{first}\"]")), record);
            }
        })
        .unwrap();
        let before = cell.with(|world| body_json(world)).unwrap();
        let payload = serde_json::json!({"sender_agent_id": first, "recipient_agent_id": second});
        refused_with(record(&cell, &inputs, "information_contact_update", &payload, 1, 10), why);
        assert_eq!(cell.with(|world| body_json(world)).unwrap(), before, "{change}");
    }
}
