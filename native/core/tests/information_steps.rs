//! The information model's contacts (`rules::information::plan` and `sample`, the kernels of the
//! PyO3 entries `planned_contacts` and `sample_contacts`) on the world of
//! `integration/test_information_row.py` with its claims (`_world_with_claims(believers=5)`: the
//! Project screen's generated Scenario at minute 0, `model_steps.json`'s `claims`), under each of
//! the three models (`tests/support/core_model_steps.py` writes them; Issue #225, lane M10a). Each
//! policy is what the Run reads of `controller.information_policy` (`RunCore.policies_document`).
//! The pytest file keeps the tests of the row and its tree, an entry test
//! (`test_fewer_contacts_release_the_nodes_left_over`), what the PyO3 entry turns down, the
//! validator, the Runs, and the bits on random states and values (their inputs are too large for
//! a fixture of this size).
//!
//! The digests are the ones the pytest file pinned (`ANSWERS`), recorded from the kernels of model
//! version 2 (step 4, L11): the tests write the same text with `Dumps` (`json.dumps` of the draws,
//! the contacts, the events the step scheduled and the namespace with its keys sorted).

mod support;

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use serde_json::Value;
use sheltersim_core::cell::Shard;
use sheltersim_core::json::Encoder;
use sheltersim_core::row::Field;
use sheltersim_core::rules::information::{self as information, Inputs};
use sheltersim_core::rules::psychology::Bad;
use sheltersim_core::run::held::{Class, Held, Obj};
use sheltersim_core::run::information::{Information, Model};
use sheltersim_core::state::information::{InformationRow, NAME};
use sheltersim_core::value::V;
use sheltersim_core::world::{World, WorldCell};

use support::dumps::Dumps;
use support::{cases, load, texts};

const FILE: &str = "model_steps";

/// What the pytest file pinned (`ANSWERS`).
const ANSWERS: &[(&str, &str)] = &[
    ("claims-EvidenceAwareInformationPolicy", "7e17a31e26288bf5119da239fa024163dd67d7ad4a4fc8f90664d23dd75c2605"),
    ("claims-HazardInformationPolicy", "b8f50d81a075e2def89cdd61b085f16c64ed2a1cf6f5ff6bd3d1a74f73fe5f77"),
    ("claims-InformationSpreadPolicy", "950769ea58b9c61423b630b0b92245baa6534be789f95d1ed7ed23aa8c338be6"),
    ("seed-18446744073709551623", "dd2527a47cd6e7a981e2e309af5e6c1bf1375e0ef86d4727cd3a3e2988390b8e"),
    ("seed-9223372036854775808", "cfa1e5f6a6f20cc215da760e12079e097cd02c07c59e48f8609a6c5208b45cf8"),
];

/// `_answers`: the sha256 of the texts, each with a newline, against the pinned digest.
fn answers(name: &str, texts: &[String]) {
    let wanted = ANSWERS.iter().find(|(key, _)| *key == name).unwrap_or_else(|| panic!("no answer {name}")).1;
    let joined: String = texts.iter().map(|text| format!("{text}\n")).collect();
    assert_eq!(texts::sha(&joined), wanted, "{name}");
}

fn data() -> &'static Value {
    static DATA: OnceLock<Value> = OnceLock::new();
    DATA.get_or_init(|| load::data(FILE))
}

/// The models of the file: the class's name and its policy as the Run read it.
fn models() -> Vec<(String, Information)> {
    data()["information"]["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|pair| {
            let document = cases::resolve(&data()["policies"], pair[1].as_str().unwrap());
            let mut controller = Obj::new(Class::named("sheltersim.experiments.runner", "OfflineSimulationController"));
            controller.attrs.insert(Arc::from("information_policy"), Ok(Held::Object(Arc::new(load::obj(&document["object"])))));
            (pair[0].as_str().unwrap().to_owned(), Information::of(&controller))
        })
        .collect()
}

fn got<T>(read: Result<T, Bad>) -> T {
    read.unwrap_or_else(|bad| panic!("the policy was not read: {bad:?}"))
}

fn text(value: &str) -> V {
    V::Str(Arc::from(value))
}

fn optional(value: &Option<Arc<str>>) -> V {
    value.as_deref().map_or(V::Null, text)
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

/// Each zone next to every other zone, by ID.
fn neighbors(world: &World) -> HashMap<String, Vec<String>> {
    let mut zones: Vec<String> = world.rows().unwrap().zones.iter().map(|(id, _)| id.to_string()).collect();
    zones.sort();
    zones.iter().map(|zone| (zone.clone(), zones.iter().filter(|other| *other != zone).cloned().collect())).collect()
}

/// `_answer` of the pytest file: what `planned_contacts` gives (each draw, each contact, the
/// model's version and forgetting probability), then what `sample_contacts` schedules and leaves,
/// as the text of its answer. Whether it planned a contact.
fn answer(information: &Information, seed: &str) -> (String, bool) {
    let entry = cases::resolve(&data()["worlds"], "claims");
    let cell: Shard<World> = texts::cell(&entry);
    let model = got(information::model_of(information)).expect("a model");
    let settings = got(information::settings(information, model));
    let neighbors = cell.with(|world| neighbors(world)).unwrap();
    let minute = cell.with(|world| world.minute_index().unwrap()).unwrap();
    let inputs = |recording| Inputs { model, settings: &settings, minute, seed, neighbors: &neighbors, recording };

    sheltersim_core::rng::start_draw_log();
    let plan = cell.with(|world| information::plan(world, &inputs(true))).unwrap();
    let made = sheltersim_core::rng::take_draw_log();
    let plan = plan.unwrap();
    let mut draws: Vec<(String, String)> = made.into_iter().map(|(key, value)| (key, hex(value))).collect();
    draws.sort();
    let mut contacts: Vec<Vec<V>> = plan
        .traces
        .iter()
        .map(|trace| {
            vec![
                V::Str(plan.ids[trace.sender].clone()),
                V::Str(plan.ids[trace.recipient].clone()),
                text(trace.basis),
                V::Int(trace.minutes),
                optional(&trace.message),
                optional(&trace.belief),
                V::Float(trace.probability),
                V::Bool(trace.selected),
                optional(&trace.source),
                optional(&trace.causation),
            ]
        })
        .collect();
    let key = |item: &Vec<V>| {
        let part = |value: &V| match value {
            V::Str(text) => text.to_string(),
            _ => String::new(),
        };
        (part(&item[1]), part(&item[0]), part(&item[4]))
    };
    contacts.sort_by_key(key);
    let forget = if model == Model::EvidenceAware { text(&hex(settings.forget_probability)) } else { V::Null };

    let pending: Vec<Arc<str>> = cell.with(|world| world.queue().unwrap().pending_ids(None)).unwrap();
    cell.with(|world| information::sample(world, &inputs(false))).unwrap().unwrap_or_else(|error| panic!("{error:?}"));
    let scheduled = cell
        .with(|world| {
            let queue = world.queue().unwrap();
            let mut rows: Vec<_> = queue
                .ordered()
                .iter()
                .filter_map(|(_, serial)| queue.slot(*serial).row.clone())
                .filter(|row| !pending.contains(&row.event_id))
                .collect();
            rows.sort_by_key(|row| row.sequence);
            rows
        })
        .unwrap();
    let field = |value: &V, name: &str| match value {
        V::Map(map) => map[name].clone(),
        _ => panic!("not a map"),
    };
    let proposals: Vec<V> = scheduled
        .iter()
        .map(|event| {
            let payload = &event.payload;
            V::List(vec![
                V::Int(minute),
                field(payload, "message_id"),
                field(payload, "sender_agent_id"),
                field(payload, "recipient_agent_id"),
                field(payload, "belief"),
                field(payload, "contact_basis"),
                field(payload, "contact_minutes"),
                field(&event.provenance, "transmission_probability"),
                optional(&event.source_id),
                optional(&event.causation_id),
            ])
        })
        .collect();

    // The namespace with its keys sorted (`json.loads(json.dumps(state, sort_keys=True))`), and
    // its contacts in their order.
    let (sorted, keys) = cell
        .with(|world| {
            let row = world.row::<InformationRow>(NAME).unwrap();
            let mut encoder = Encoder::with_capacity(4096);
            row.write_json(&mut encoder).unwrap();
            let sorted = load::v(&serde_json::from_slice::<Value>(&encoder.out).unwrap());
            (sorted, row.contacts.keys().map(|key| V::Str(key.clone())).collect::<Vec<_>>())
        })
        .unwrap();
    let planned = !contacts.is_empty();
    let answer = V::List(vec![
        V::List(draws.into_iter().map(|(key, value)| V::List(vec![text(&key), text(&value)])).collect()),
        V::List(contacts.into_iter().map(V::List).collect()),
        text(information::version(model)),
        forget,
        V::List(proposals),
        sorted,
        V::List(keys),
    ]);
    (Dumps::default().text(&answer), planned)
}

/// `test_claims_pass_as_the_python_rule_passed_them`: eight seeds of each model.
#[test]
fn claims_pass_as_the_python_rule_passed_them() {
    for (name, information) in models() {
        let mut texts = Vec::new();
        let mut proposals = 0;
        for seed in 0..8 {
            let (answer, planned) = answer(&information, &seed.to_string());
            assert!(planned, "{name} {seed}");
            proposals += serde_json::from_str::<Value>(&answer).unwrap()[4].as_array().unwrap().len();
            texts.push(answer);
        }
        assert!(proposals > 0, "{name}");
        answers(&format!("claims-{name}"), &texts);
    }
}

/// `test_a_seed_past_64_bits_gives_the_python_rule_s_answers`: `CounterRNG` keys its draws with
/// `str(seed)`, and a Scenario's seed is any int from 0.
#[test]
fn a_seed_past_64_bits_gives_the_python_rule_s_answers() {
    let models = models();
    for seed in data()["information"]["big_seeds"].as_array().unwrap() {
        let seed = seed.as_str().unwrap();
        let texts: Vec<String> = models.iter().map(|(_, information)| answer(information, seed).0).collect();
        answers(&format!("seed-{seed}"), &texts);
    }
}
