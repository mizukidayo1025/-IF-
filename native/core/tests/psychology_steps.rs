//! Each person's minute (`rules::psychology::update`, the kernel of the PyO3 entry
//! `update_psychology`) and its trace's document (`rules::trace::record_psychology`, the kernel of
//! `record_psychology_trace`) on the worlds of `integration/test_psychology_update.py` and
//! `test_trace_documents.py`: the Project screen's generated Scenario at minute 0 whose people
//! those files shuffled with `random.Random` (`model_steps.json`'s `psychology-<seed>` and
//! `trace-<seed>`), and the policies they drew (`tests/support/core_model_steps.py` writes them;
//! Issue #225, lane M10a). Each policy is what the Run reads of `controller.psychology`
//! (`RunCore.policies_document`). The pytest files keep an entry test each
//! (`test_people_of_a_group_are_separated_as_the_deleted_rule_said`,
//! `test_int_values_of_a_scenario_give_the_deleted_rule_s_document`), what the PyO3 entries turn
//! down, the writing of the values, and the Runs.
//!
//! The digests are the ones the pytest files pinned from the deleted Python rules
//! (`policies/psychology.py`, `policies/alternatives.py` and the update phase's Python steps at
//! 45a9bc9): the tests write the same texts with `Dumps` (`json.dumps` of each delta's fields, and
//! of each trace's document with its contributions), so each value is still the Python rule's.

mod support;

use std::sync::{Arc, OnceLock};

use serde_json::Value;
use sheltersim_core::cell::Shard;
use sheltersim_core::config::{Section, SectionId};
use sheltersim_core::host::{Trace, TraceLevel};
use sheltersim_core::rules::psychology::{self as psychology, Bad, Delta, PsychologyResult};
use sheltersim_core::rules::trace::{self as trace, TraceInputs};
use sheltersim_core::rules::world_view::{self as view, ViewCache, ViewData};
use sheltersim_core::run::held::{Class, Held, Obj};
use sheltersim_core::run::psychology::Psychology;
use sheltersim_core::value::V;
use sheltersim_core::world::{World, WorldCell};

use support::dumps::Dumps;
use support::{cases, load, sections, texts, Call, TestHost};

const FILE: &str = "model_steps";

/// What the deleted Python rules answered, as `test_psychology_update.py` pinned it (`ANSWERS`).
const ANSWERS: &[(&str, &str)] = &[
    ("ints-0", "fb6093685a0aa159c4b2e34c33d8ef488d2d6cfab9824db8d154f4ae4bf4e266"),
    ("ints-1", "b5123a4fdd6fda5f6037c332e421c683a21100f62637c045a7a92860660154a4"),
    ("ints-2", "81330efb55356eb79d75658b9f7100999f40d621f77a20106bef2345e76c553f"),
    ("models-0", "b3a010870ea3f7b624b0dd09629fd0ff9407012d28754b7f5572fcdc3178c7d6"),
    ("models-1", "de4291d76b22c58d22c97b451851622bc4b0dfbe4764f87d5151450a5965913b"),
    ("models-2", "bbc661e8d439b21b4b2ab67dbd9b43a5cefb1764dad97666fe641945199f06ba"),
    ("models-3", "8776482f8b3df765a9964ef1508045b147c8b3c8e84ae4f41a5cbf3b8605f08b"),
    ("models-4", "d23a08311e64f92397a925e05d3f00cf6a285924c3ff994c19cdf99167d7ff44"),
    ("models-5", "640b058ed3f594e8f55cbc9f691d1ea2864850be70a7adb54a45a29fecf08421"),
    ("models-6", "68f5b75c362c695af2a75ef8012d9fd3b977b26bedc4adac54efa2844409e6d9"),
    ("models-7", "1fe904d07fdb20f7f7b50ea6b9d4f49cc566b65e0d3d48099ee4c9b3cd2ce57e"),
    ("one-percent", "747ac51334eb2003d4a5e5f7cd53df787172656c655d9ca0ea3f0299829fc413"),
];

/// What the deleted Python steps wrote as the trace, as `test_trace_documents.py` pinned it
/// (`PSYCHOLOGY_ANSWERS`), by seed and level.
const DOCUMENT_ANSWERS: &[(&str, &str)] = &[
    ("models-0-full", "640ed4e7a101957606fcddcd486c7030758ff4658be73a39bf25370877272c81"),
    ("models-0-summary", "640ed4e7a101957606fcddcd486c7030758ff4658be73a39bf25370877272c81"),
    ("models-1-full", "cfa05fafeec31e3646ed5207a524a17be226b2eb2f6eab428ffbc628c49ef0e5"),
    ("models-1-summary", "cfa05fafeec31e3646ed5207a524a17be226b2eb2f6eab428ffbc628c49ef0e5"),
    ("models-2-full", "36b75b85d6acd4c719b7c3b8fbf456ac773665de29e0b669f350c6e959a19de4"),
    ("models-2-summary", "36b75b85d6acd4c719b7c3b8fbf456ac773665de29e0b669f350c6e959a19de4"),
    ("models-3-full", "787c2c165087682b368863caab8719d90719eebbfa85df6a49aca1ad25aeac2d"),
    ("models-3-summary", "787c2c165087682b368863caab8719d90719eebbfa85df6a49aca1ad25aeac2d"),
    ("models-4-full", "735c4e5d454fa51f58798bd8fd699f0aabd6c04aee6a2f25153a26f6f0753fc7"),
    ("models-4-summary", "735c4e5d454fa51f58798bd8fd699f0aabd6c04aee6a2f25153a26f6f0753fc7"),
    ("models-5-full", "4d964b10d02eafeabfc15ea50eace84a454a013e51beed194e8eb1a2ecb3338a"),
    ("models-5-summary", "4d964b10d02eafeabfc15ea50eace84a454a013e51beed194e8eb1a2ecb3338a"),
];

fn digest(texts: &[String]) -> String {
    let joined: String = texts.iter().map(|text| format!("{text}\n")).collect();
    texts::sha(&joined)
}

/// `_answers`: the sha256 of the texts, each with a newline, against the pinned digest.
fn answers(table: &[(&str, &str)], name: &str, texts: &[String]) {
    let wanted = table.iter().find(|(key, _)| *key == name).unwrap_or_else(|| panic!("no answer {name}")).1;
    assert_eq!(digest(texts), wanted, "{name}");
}

// ---- The fixture --------------------------------------------------------------------------------

fn data() -> &'static Value {
    static DATA: OnceLock<Value> = OnceLock::new();
    DATA.get_or_init(|| load::data(FILE))
}

/// A named world of the file, in a cell, with its update phase's view (`view_of(world,
/// include_state_hash=False)`, or `controller._view_after_movement(world)`: built anew).
fn world(name: &str) -> (Shard<World>, ViewData) {
    let entry = cases::resolve(&data()["worlds"], name);
    let cell = texts::cell(&entry);
    let selection = match sections::section(&entry, SectionId::Selection).selection.get() {
        Some(Section::Ok(selection)) => Ok(selection.clone()),
        Some(Section::Fault(fault)) => Err(fault.clone()),
        _ => panic!("no selection"),
    };
    let extensions = match sections::section(&entry, SectionId::ExtensionsEnabled).extensions_enabled.get() {
        Some(Section::Ok(enabled)) => **enabled,
        _ => panic!("no extensions flag"),
    };
    let inputs = view::Inputs { selection: &selection, configs: None };
    let view = cell.with(|world| view::build(world, &inputs, &mut ViewCache::default()).unwrap().finish(extensions)).unwrap();
    (cell, view)
}

/// The policy as the Run read it of `controller.psychology`.
fn policy(name: &Value) -> Psychology {
    let document = cases::resolve(&data()["policies"], name.as_str().unwrap());
    let mut controller = Obj::new(Class::named("sheltersim.experiments.runner", "OfflineSimulationController"));
    controller.attrs.insert(Arc::from("psychology"), Ok(Held::Object(Arc::new(load::obj(&document["object"])))));
    Psychology::of(&controller)
}

fn got<T>(read: Result<T, Bad>) -> T {
    read.unwrap_or_else(|bad| panic!("the policy was not read: {bad:?}"))
}

/// One call of the PyO3 entry `update_psychology` at minute 0, with a trace or not.
fn update(cell: &Shard<World>, view: &ViewData, psychology: &Psychology, traced: bool) -> PsychologyResult {
    let model = got(psychology::model_of(psychology));
    let params = got(psychology::params(psychology, model, Some(view.resource_service_enabled), traced));
    cell.with(|world| psychology::update(world, view, &params, 0, traced)).unwrap().unwrap()
}

/// `_text`: a delta's fields in their order, so a float's bits and 1 against 1.0 show.
fn delta_text(delta: &Delta) -> String {
    let mut fields = vec![
        V::Str(delta.agent_id.clone()),
        V::Int(delta.minute_index),
        V::Float(delta.toilet_urgency),
        V::Float(delta.hunger),
        V::Float(delta.fatigue),
        V::Float(delta.stress),
        V::Float(delta.trust),
        V::Float(delta.hydration_need),
    ];
    for place in 0..7 {
        fields.push(delta.body.map_or(V::Null, |body| V::Float(body[place])));
    }
    Dumps::default().text(&V::List(fields))
}

/// `_texts_of_models`: each policy's deltas, without a trace.
fn texts_of_models(seed: usize, policies: &Value) -> Vec<String> {
    let (cell, view) = world(&format!("psychology-{seed}"));
    let mut out = Vec::new();
    for name in policies.as_array().unwrap() {
        let result = update(&cell, &view, &policy(name), false);
        assert!(result.traces.is_none());
        out.extend(result.deltas.iter().map(delta_text));
    }
    out
}

// ---- Each person's minute -----------------------------------------------------------------------

/// `test_every_model_gives_the_bits_of_the_deleted_rules`: eleven models and settings drawn on
/// worlds whose values no scenario reaches.
#[test]
fn every_model_gives_the_bits_of_the_deleted_rules() {
    for (seed, policies) in data()["psychology"]["models"].as_array().unwrap().iter().enumerate() {
        answers(ANSWERS, &format!("models-{seed}"), &texts_of_models(seed, policies));
    }
}

/// `test_a_one_percent_change_shows`.
#[test]
fn a_one_percent_change_shows() {
    let policies = data()["psychology"]["one_percent"].as_array().unwrap();
    let same = texts_of_models(1, &Value::Array(vec![policies[0].clone()]));
    answers(ANSWERS, "one-percent", &same);
    let other = texts_of_models(1, &Value::Array(vec![policies[1].clone()]));
    assert_ne!(other, same);
}

/// `test_int_values_of_a_scenario_give_the_bits_of_the_deleted_rules`: the ints a Scenario may
/// write meet floats in every expression, where they are the same f64.
#[test]
fn int_values_of_a_scenario_give_the_bits_of_the_deleted_rules() {
    for (seed, policies) in data()["psychology"]["ints"].as_array().unwrap().iter().enumerate() {
        answers(ANSWERS, &format!("ints-{seed}"), &texts_of_models(seed, policies));
    }
}

// ---- The trace's document -----------------------------------------------------------------------

/// `test_the_document_of_each_model_is_the_deleted_rule_s`: each model's document of the minute
/// and its contributions, at each level, after the social step changed every other delta's stress.
#[test]
fn the_document_of_each_model_is_the_deleted_rule_s() {
    for case in data()["trace"].as_array().unwrap() {
        let seed = case["seed"].as_u64().unwrap();
        let (cell, view) = world(case["world"].as_str().unwrap());
        for (level, label) in [(TraceLevel::Summary, "summary"), (TraceLevel::Full, "full")] {
            let mut documents = Vec::new();
            for step in case["steps"].as_array().unwrap() {
                let psychology = policy(&step["policy"]);
                let result = update(&cell, &view, &psychology, true);
                let mut drawn = step["drawn"].as_array().unwrap().iter();
                let finals: Vec<(&str, f64)> = result
                    .deltas
                    .iter()
                    .enumerate()
                    .map(|(place, delta)| (&*delta.agent_id, if place % 2 == 1 { drawn.next().unwrap().as_f64().unwrap() } else { delta.stress }))
                    .collect();
                assert!(drawn.next().is_none());
                let source_id = match psychology.policy.as_ref().unwrap().get("source_id").unwrap() {
                    Held::Value(value) => value.clone(),
                    Held::Text(text) => V::Str(text.clone()),
                    other => panic!("a source id of {other:?}"),
                };
                let inputs = TraceInputs { source_id: &source_id, policy_version: &V::Str(result.policy_version.clone()), minute: &V::Int(0) };
                let mut host = TestHost { level, ..TestHost::default() };
                trace::record_psychology(&mut host, &view, result.traces.as_ref().unwrap(), &finals, &inputs).unwrap();
                let [Call::Trace(Trace::Psychology { document, contributions })] = &host.calls[..] else {
                    panic!("not one document: {:?}", host.calls);
                };
                documents.push(Dumps::default().unicode().text(&V::List(vec![document.clone(), contributions.clone()])));
            }
            answers(DOCUMENT_ANSWERS, &format!("models-{seed}-{label}"), &documents);
        }
    }
}
