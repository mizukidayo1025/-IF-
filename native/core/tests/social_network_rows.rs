//! The rules of `social_network_v1` (`rules::social_network`), of the social dialogue's state
//! context (`rules::social_dialogue`) and the social action of `SocialInteractionHandler`
//! (`rules::social_interaction`, through `dispatch::apply_alone`) on the world of
//! `integration/test_social_network_row.py`: the Project screen's generated Scenario after 40 ticks
//! (`people_rows.json`'s `finished`, and its variants, written by
//! `tests/support/core_people_rows.py`), with the inputs that file made with `random.Random`
//! (Issue #225, lane M5). The pytest file keeps an entry test for each PyO3 entry, and the tests of
//! the boundary: the row and its tree, what taints it, each way of writing that the check must see,
//! and the comparisons with the Python rules still in `src/`.
//!
//! The digests the pytest file pinned from the Python rules (`PYTHON_GAVE`: `json.dumps` with
//! `sort_keys` of what a rule wrote or returned) are kept: the tests write the same text with
//! `Dumps`.

mod support;

use std::sync::{Arc, OnceLock};

use serde_json::{json, Value};
use sheltersim_core::checkpoint::sha256_hex;
use sheltersim_core::error::Error;
use sheltersim_core::json::Encoder;
use sheltersim_core::mathx::{py_max, py_min};
use sheltersim_core::row::Field;
use sheltersim_core::rules::social_dialogue::{
    capture_context, validate_context, AFFECT_INVALID, NEEDS_INVALID, NOT_ITS_EVENT, NOT_ONE_OR_TWO, NOT_UNIT, STATE_FIELDS,
};
use sheltersim_core::rules::social_network::{observe_event, observes_event, BOUND_REACHED};
use sheltersim_core::state::body_state::{BodyRow, NAME as BODY};
use sheltersim_core::state::social_network::{Relationship, SocialNetworkRow, NAME as NETWORK};
use sheltersim_core::value::V;
use sheltersim_core::world::{World, WorldCell};

use support::dumps::Dumps;
use support::{cases, load};

const FILE: &str = "people_rows";

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

fn world(name: &str) -> World {
    let entry = entry(name);
    load::world(&entry["world"], &entry["slots"])
}

/// The first `count` people, by ID (`_people`).
fn people(world: &World, count: usize) -> Vec<String> {
    let mut ids: Vec<String> = world.rows().unwrap().agents.iter().map(|(id, _)| id.to_string()).collect();
    ids.sort();
    ids.truncate(count);
    ids
}

fn minute(world: &World) -> i64 {
    world.minute_index().unwrap()
}

fn digest(text: &str) -> String {
    sha256_hex(text.as_bytes())[..16].to_owned()
}

fn network_json(world: &World) -> Vec<u8> {
    let mut encoder = Encoder::with_capacity(4096);
    world.row::<SocialNetworkRow>(NETWORK).unwrap().write_json(&mut encoder).unwrap();
    encoder.out
}

/// The namespace as a value: with `sort_keys`, its order does not matter.
fn network_value(world: &World) -> V {
    load::v(&serde_json::from_slice(&network_json(world)).unwrap())
}

/// `_digest(*values)`: each value's `json.dumps(sort_keys=True)`, one per line.
fn python_gave(values: &[V]) -> String {
    let lines: Vec<String> = values.iter().map(|value| Dumps::default().sorted().text(value)).collect();
    digest(&lines.join("\n"))
}

fn text(value: &str) -> V {
    V::Str(Arc::from(value))
}

fn map(entries: &[(&str, V)]) -> V {
    V::Map(entries.iter().map(|(key, value)| (Arc::from(*key), value.clone())).collect())
}

/// `observe_event` as its PyO3 entry calls it: nothing for a type it does not observe, and the
/// payload's fields the rule reads of the type.
fn observe(world: &World, event_type: &str, payload: &V, minute: i64) -> Result<(), Error> {
    if !observes_event(event_type) {
        return Ok(());
    }
    let keys: &[&str] = match event_type {
        "information_contact_update" => &["recipient_agent_id", "sender_agent_id", "message_id"],
        "proximity_contact_update" => &["first_agent_id", "second_agent_id", "observed_minute"],
        _ => &["interaction_type", "agent_id", "participant_agent_id"],
    };
    let V::Map(all) = payload else { panic!("a payload is a map") };
    let fields = keys.iter().filter_map(|key| all.get(*key).map(|value| (Arc::from(*key), value.clone()))).collect();
    observe_event(world, event_type, &V::Map(fields), minute)
}

fn set_affect(world: &World, id: &str, positive: f64, negative: f64) {
    let mut row = world.row_mut::<BodyRow>(BODY).unwrap();
    let record = row.agents.get_mut(id).unwrap();
    record.positive_affect = positive;
    record.negative_affect = negative;
}

// ---- observe_event --------------------------------------------------------------------------------

/// `test_observe_event_gives_the_bits_of_the_python_rule`: 1500 random contacts, corrections and
/// interactions of six people, each after a random affect of all six, on the relationships in the
/// order the Run left them. The namespace, and the order of its relationships, are what the Python
/// rule left.
#[test]
fn observe_event_gives_the_bits_of_the_python_rule() {
    let world = world("finished");
    {
        // The relationships in the order the Run left them, which the pytest file's world kept.
        let mut row = world.row_mut::<SocialNetworkRow>(NETWORK).unwrap();
        let map = &mut row.relationships.map;
        let order = inputs()["expected"]["network_order"].as_array().unwrap();
        assert_eq!(order.len(), map.len());
        for key in order {
            let key = key.as_str().unwrap();
            let record = map.shift_remove(key).unwrap();
            map.insert(Arc::from(key), record);
        }
    }
    let ids = people(&world, 6);
    let now = minute(&world);
    let interactions = ["reassurance_exchange", "family_care", "voluntary_support", "cooperation"];
    for item in inputs()["network"].as_array().unwrap() {
        let item: Value = serde_json::from_str(item.as_str().unwrap()).unwrap();
        for id in &ids {
            set_affect(&world, id, item[0].as_f64().unwrap(), item[1].as_f64().unwrap());
        }
        let (first, second) = (text(&ids[item[3].as_u64().unwrap() as usize]), text(&ids[item[4].as_u64().unwrap() as usize]));
        let (event_type, payload) = match item[2].as_u64().unwrap() {
            0 => {
                let mut payload = vec![("recipient_agent_id", first), ("sender_agent_id", second)];
                if let Some(message) = item[5].as_str() {
                    payload.push(("message_id", text(message)));
                }
                ("information_contact_update", map(&payload))
            }
            1 => (
                "proximity_contact_update",
                map(&[("first_agent_id", first), ("second_agent_id", second), ("observed_minute", V::Int(now))]),
            ),
            2 => ("information_correction", map(&[("message_id", text("m1"))])),
            kind => (
                "social_interaction",
                map(&[
                    ("interaction_type", text(interactions[kind as usize - 3])),
                    ("agent_id", first),
                    ("participant_agent_id", second),
                ]),
            ),
        };
        observe(&world, event_type, &payload, now).unwrap();
    }
    let keys = V::List(world.row::<SocialNetworkRow>(NETWORK).unwrap().relationships.map.keys().map(|key| V::Str(key.clone())).collect());
    assert_eq!(python_gave(&[network_value(&world), keys]), "4b665bcf9f3db776");
    // The cap of the message IDs was reached, or the test proves little.
    assert!(world.row::<SocialNetworkRow>(NETWORK).unwrap().relationships.map.values().any(|record| record.shared_message_ids.len() >= 10));
}

fn interaction(kind: &str, first: &str, second: &str) -> V {
    map(&[("interaction_type", text(kind)), ("agent_id", text(first)), ("participant_agent_id", text(second))])
}

/// `test_observe_event_computes_trust_when_the_body_has_no_record`: a pair with no record in either
/// namespace, and each kind of interaction; then a 1% change of the body's transfer shows.
#[test]
fn observe_event_computes_trust_when_the_body_has_no_record() {
    let world = world("unknown_pair");
    let ids = people(&world, 2);
    let (first, second) = (ids[0].as_str(), ids[1].as_str());
    let kinds = ["cooperation", "voluntary_support", "family_care", "reassurance_exchange", "information_reaction"];
    let recorded = ["5223d144e68eb137", "6e056b1a622fdf84", "4848febde7b9557d", "6e4490f4644eeb43", "38953c71605d34fa"];
    for (kind, recorded) in kinds.iter().zip(recorded) {
        observe(&world, "social_interaction", &interaction(kind, first, second), minute(&world)).unwrap();
        assert_eq!(python_gave(&[network_value(&world)]), recorded, "{kind}");
    }
    {
        let row = world.row::<SocialNetworkRow>(NETWORK).unwrap();
        let record = &row.relationships.map[format!("[\"{first}\",\"{second}\"]").as_str()];
        assert_eq!((record.cooperation_count, record.last_interaction.as_deref()), (1, Some("information_reaction")));
        assert!(record.trust.is_some());
    }
    // A 1% change of the transfer shows.
    let event = interaction("family_care", first, second);
    let observed = |transfer: f64| {
        let world = self::world("unknown_pair");
        {
            let mut row = world.row_mut::<BodyRow>(BODY).unwrap();
            let V::Map(config) = &mut row.config else { panic!("the body's config is a map") };
            let V::Float(value) = config.get_mut("social_action_affect_transfer").unwrap() else { panic!("a float transfer") };
            *value *= transfer;
            row.relationships.map.clear();
        }
        observe(&world, "social_interaction", &event, minute(&world)).unwrap();
        network_json(&world)
    };
    assert_ne!(observed(1.01), observed(1.0));
}

/// `test_observe_event_skips_odd_payloads_as_the_python_rule_did`: one person with themselves, a
/// missing ID and an ID of another type record nothing; a message ID of another type records the
/// contact without a message.
#[test]
fn observe_event_skips_odd_payloads_as_the_python_rule_did() {
    let cases = [
        (json!({"recipient_agent_id": "a", "sender_agent_id": "a"}), "cac447cad753925e"),
        (json!({"recipient_agent_id": "a"}), "cac447cad753925e"),
        (json!({"recipient_agent_id": 3, "sender_agent_id": "a"}), "cac447cad753925e"),
        (json!({"recipient_agent_id": "a", "sender_agent_id": "b", "message_id": 4}), "be917f018c347bb8"),
    ];
    for (payload, recorded) in cases {
        let world = world("finished");
        let before = network_json(&world);
        observe(&world, "information_contact_update", &load::v(&payload), minute(&world)).unwrap();
        assert_eq!(python_gave(&[network_value(&world)]), recorded, "{payload}");
        if payload.get("message_id").is_some() {
            let row = world.row::<SocialNetworkRow>(NETWORK).unwrap();
            let record = &row.relationships.map["[\"a\",\"b\"]"];
            assert_eq!((record.contact_count, record.shared_message_ids.len()), (1, 0));
        } else {
            assert_eq!(network_json(&world), before, "{payload}");
        }
    }
}

/// `test_observe_event_refuses_an_id_json_escapes_with_nothing_written`.
#[test]
fn observe_event_refuses_an_id_json_escapes_with_nothing_written() {
    let world = world("finished");
    let before = network_json(&world);
    let payload = map(&[("recipient_agent_id", text("あ")), ("sender_agent_id", text("b"))]);
    assert!(matches!(observe(&world, "information_contact_update", &payload, minute(&world)), Err(Error::Unsupported(_))));
    assert_eq!(network_json(&world), before);
}

/// `test_at_the_bound_the_python_rule_says_so`: 100,000 relationships, and a new pair's contact is
/// the Python rule's ValueError, with nothing written.
#[test]
fn at_the_bound_the_python_rule_says_so() {
    assert_eq!(BOUND_REACHED, "social_network_v1 reached the configured relationship bound");
    let world = world("finished");
    let ids = people(&world, 2);
    {
        let mut row = world.row_mut::<SocialNetworkRow>(NETWORK).unwrap();
        let map = &mut row.relationships.map;
        map.clear();
        for index in 0..100_000 {
            let mut record = Relationship::empty();
            record.contact_count = 1;
            record.trust = Some(0.5);
            record.last_contact_minute = Some(3);
            map.insert(Arc::from(format!("[\"p{index}\",\"q\"]")), record);
        }
    }
    let before = network_json(&world);
    let payload = map(&[("recipient_agent_id", text(&ids[0])), ("sender_agent_id", text(&ids[1]))]);
    assert_eq!(observe(&world, "information_contact_update", &payload, minute(&world)), Err(Error::value_error(BOUND_REACHED)));
    assert_eq!(network_json(&world), before);
}

// ---- The state context of a social event ------------------------------------------------------

/// `test_capture_gives_what_the_python_rule_gives`: the context of two people in either order, of
/// one, and of one given twice (`set(participant_ids)`), as `json.dumps` of what the rule returned.
#[test]
fn capture_gives_what_the_python_rule_gives() {
    let world = world("finished");
    let ids = people(&world, 2);
    let (first, second) = (ids[0].as_str(), ids[1].as_str());
    for (given, recorded) in [
        (vec![first, second], "c266c955239cf005"),
        (vec![second, first], "c266c955239cf005"),
        (vec![first], "5a9c239b8a353685"),
        (vec![first], "5a9c239b8a353685"),
    ] {
        let context = capture_context(&world, &given).unwrap();
        assert_eq!(python_gave(&[text(&Dumps::default().text(&context))]), recorded, "{given:?}");
    }
}

/// `test_capture_says_the_rule_s_text`.
#[test]
fn capture_says_the_rule_s_text() {
    assert_eq!(NOT_ONE_OR_TWO, "social dialogue context requires one or two current participants");
    let world = world("finished");
    for ids in [vec![], vec!["a", "b", "c"], vec!["nobody"]] {
        assert_eq!(capture_context(&world, &ids), Err(Error::value_error(NOT_ONE_OR_TWO)), "{ids:?}");
    }
}

fn participant<'a>(context: &'a mut V, id: &str) -> &'a mut indexmap::IndexMap<Arc<str>, V> {
    let V::Map(context) = context else { panic!("a context is a map") };
    let V::Map(participants) = context.get_mut("participants").unwrap() else { panic!("participants are a map") };
    let V::Map(state) = participants.get_mut(id).unwrap() else { panic!("a state is a map") };
    state
}

/// `test_validation_of_a_context_says_the_rule_s_text`: the captured context of two people passes;
/// each way of spoiling it gets the Python rule's text, in its order of checks.
#[test]
fn validation_of_a_context_says_the_rule_s_text() {
    assert_eq!(
        [NOT_ITS_EVENT, NOT_UNIT, NEEDS_INVALID, AFFECT_INVALID, STATE_FIELDS],
        [
            "social dialogue state context does not match its event",
            "social dialogue state values must be finite and within 0..1",
            "social dialogue needs context is invalid",
            "social dialogue affect context is invalid",
            "social dialogue participant state has unsupported fields",
        ]
    );
    let world = world("finished");
    let ids = people(&world, 2);
    let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
    let due = minute(&world);
    let captured = capture_context(&world, &ids).unwrap();
    assert_eq!(validate_context(&captured, &ids, due), Ok(()));
    let first: &'static str = Box::leak(ids[0].to_owned().into_boxed_str());
    type Spoil = Box<dyn Fn(&mut V)>;
    let top = |key: &'static str, value: V| -> Spoil {
        Box::new(move |context: &mut V| {
            let V::Map(context) = context else { panic!() };
            context.insert(Arc::from(key), value.clone());
        })
    };
    let state = |key: &'static str, value: V| -> Spoil {
        Box::new(move |context: &mut V| {
            participant(context, first).insert(Arc::from(key), value.clone());
        })
    };
    let participants = |change: fn(&mut indexmap::IndexMap<Arc<str>, V>, &str)| -> Spoil {
        Box::new(move |context: &mut V| {
            let V::Map(context) = context else { panic!() };
            let V::Map(participants) = context.get_mut("participants").unwrap() else { panic!() };
            change(participants, first);
        })
    };
    let spoils: Vec<(Spoil, &str)> = vec![
        (top("schema_version", V::Int(2)), NOT_ITS_EVENT),
        (top("captured_minute", V::Int(-1)), NOT_ITS_EVENT),
        (top("captured_minute", V::Int(1_000_000)), NOT_ITS_EVENT),
        (top("captured_minute", V::Bool(true)), NOT_ITS_EVENT),
        (top("more", V::Int(1)), NOT_ITS_EVENT),
        (participants(|participants, id| drop(participants.shift_remove(id))), NOT_ITS_EVENT),
        (
            participants(|participants, id| {
                let state = participants.shift_remove(id).unwrap();
                participants.insert(Arc::from("other"), state);
            }),
            NOT_ITS_EVENT,
        ),
        (state("stress", V::Float(1.5)), NOT_UNIT),
        (state("trust", V::Float(f64::NAN)), NOT_UNIT),
        (state("stress", V::Bool(true)), NOT_UNIT),
        (
            Box::new(move |context: &mut V| {
                let V::Map(needs) = participant(context, first).get_mut("needs").unwrap() else { panic!() };
                needs.insert(Arc::from("hunger"), V::Float(-0.5));
            }),
            NOT_UNIT,
        ),
        (
            Box::new(move |context: &mut V| {
                let V::Map(needs) = participant(context, first).get_mut("needs").unwrap() else { panic!() };
                needs.shift_remove("hunger");
            }),
            NEEDS_INVALID,
        ),
        (state("affect", map(&[("positive_affect", V::Float(0.5))])), AFFECT_INVALID),
        (Box::new(move |context: &mut V| drop(participant(context, first).shift_remove("affect"))), STATE_FIELDS),
    ];
    for (index, (spoil, wanted)) in spoils.iter().enumerate() {
        let mut context = captured.clone();
        spoil(&mut context);
        assert_eq!(validate_context(&context, &ids, due), Err(Error::value_error(*wanted)), "spoil {index}");
    }
}

// ---- The social action of the handler ---------------------------------------------------------------

/// The handler of the social action, as the cases of its texts hold it.
fn handler() -> &'static Value {
    &data()["cases"]["a_bad_payload_is_failed_by_the_kernel_in_python_s_text"][0]["handler"]
}

/// stress, trust and fatigue of each person, as their bits.
fn values(world: &World, ids: &[&str]) -> Vec<u64> {
    let rows = world.rows().unwrap();
    ids.iter()
        .flat_map(|id| {
            let agent = rows.agents.get(id).unwrap();
            [agent.stress.to_bits(), agent.trust.to_bits(), agent.needs.fatigue.to_bits()]
        })
        .collect()
}

/// `_apply_formula`: the deleted Python rule's writes of a social action whose checks all pass,
/// written down from it. The participant is relieved by the transfer times the actor's affect
/// signal, clipped to -1..1: stress down by it, trust up by half of it, both within 0..1. Except in
/// a reassurance exchange, the actor gains 0.002 fatigue and a quarter of a positive relief as trust.
fn formula(world: &World, kind: &str, actor: &str, participant: &str) -> Vec<u64> {
    let (signal, transfer) = {
        let body = world.row::<BodyRow>(BODY).unwrap();
        let record = body.agents.get(actor).unwrap();
        let V::Map(config) = &body.config else { panic!("the body's config is a map") };
        let transfer = match config.get("social_action_affect_transfer") {
            Some(V::Float(value)) => *value,
            None => 0.02,
            Some(other) => panic!("a transfer of another type: {other:?}"),
        };
        (py_min(1.0, py_max(-1.0, record.positive_affect - record.negative_affect)), transfer)
    };
    let relief = transfer * signal;
    let rows = world.rows().unwrap();
    let (actor_row, participant_row) = (rows.agents.get(actor).unwrap(), rows.agents.get(participant).unwrap());
    let mut actor_values = (actor_row.stress, actor_row.trust, actor_row.needs.fatigue);
    let participant_values = (
        py_min(1.0, py_max(0.0, participant_row.stress - relief)),
        py_min(1.0, py_max(0.0, participant_row.trust + relief * 0.5)),
        participant_row.needs.fatigue,
    );
    if kind != "reassurance_exchange" {
        actor_values.2 = py_min(1.0, actor_values.2 + 0.002);
        actor_values.1 = py_min(1.0, actor_values.1 + py_max(0.0, relief * 0.25));
    }
    [actor_values, participant_values].iter().flat_map(|(stress, trust, fatigue)| [stress.to_bits(), trust.to_bits(), fatigue.to_bits()]).collect()
}

/// `test_a_social_action_gives_the_bits_of_the_rule_s_formula`: two people in one zone and
/// household, 400 random states of each, and each kind of social action: the kernel writes the
/// bits of the rule's formula.
#[test]
fn a_social_action_gives_the_bits_of_the_rule_s_formula() {
    let entry = entry("pair");
    for kind in ["reassurance_exchange", "voluntary_support", "family_care"] {
        let loaded = load::loaded(&entry);
        let mut host = cases::host(&entry);
        let template = inputs()["events"][format!("action.{kind}")].clone();
        let ids = loaded.cell.with(|world| people(world, 2)).unwrap();
        let (actor, participant) = (ids[0].as_str(), ids[1].as_str());
        for (number, item) in inputs()["formula"].as_array().unwrap().iter().enumerate() {
            let item: Value = serde_json::from_str(item.as_str().unwrap()).unwrap();
            let [stress, trust, fatigue, positive, negative] = [0, 1, 2, 3, 4].map(|index| item[index].as_f64().unwrap());
            let expected = loaded
                .cell
                .with(|world| {
                    {
                        let mut rows = world.rows_mut().unwrap();
                        let agent = rows.agents.get_mut(actor).unwrap();
                        agent.stress = fatigue;
                        agent.trust = trust;
                        agent.needs.fatigue = fatigue;
                        agent.needs.hunger = 0.0;
                        agent.needs.toilet_urgency = 0.0;
                        agent.needs.hydration_need = 0.0;
                        let other = rows.agents.get_mut(participant).unwrap();
                        other.stress = py_max(stress, 0.6);
                        other.trust = trust;
                    }
                    set_affect(world, actor, positive, negative);
                    formula(world, kind, actor, participant)
                })
                .unwrap();
            let mut event = template.clone();
            event["event_id"] = json!(format!("e{}", number + 1));
            event["sequence"] = json!(number + 1);
            assert_eq!(cases::apply_handler(&loaded, &mut host, handler(), &load::event(&event)), Ok(None), "{kind} {number}");
            assert_eq!(loaded.cell.with(|world| values(world, &[actor, participant])).unwrap(), expected, "{kind} {number}");
        }
    }
}

/// `test_a_suppressed_action_says_python_s_text_with_nothing_written`,
/// `test_a_helper_who_needs_more_than_the_target_is_suppressed`,
/// `test_a_different_household_is_suppressed` and
/// `test_a_bad_payload_is_failed_by_the_kernel_in_python_s_text`: the kernel suppresses or fails
/// the action with the Python rule's text, written down from it, and changes nothing.
#[test]
fn a_social_action_the_rule_turns_down_says_python_s_text() {
    cases::check_group(FILE, "a_suppressed_action_says_python_s_text_with_nothing_written", 1);
    cases::check_group(FILE, "a_helper_who_needs_more_than_the_target_is_suppressed", 1);
    cases::check_group(FILE, "a_different_household_is_suppressed", 1);
    cases::check_group(FILE, "a_bad_payload_is_failed_by_the_kernel_in_python_s_text", 6);
}
