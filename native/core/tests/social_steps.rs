//! The social model's step (`rules::social_propagation::update`, the kernel of the PyO3 entry
//! `update_social_propagation`) on the worlds of `integration/test_social_propagation_row.py`: the
//! Project screen's generated Scenario at minute 0 with a new row and each person's stress
//! `(index % 10) / 10` (`model_steps.json`'s `social`), and the policies and the values that file
//! made with `random.Random` (`tests/support/core_model_steps.py` writes them; Issue #225, lane
//! M10a). The pytest file keeps the entry test (`test_the_step_writes_what_the_python_rule_wrote`
//! for one model), the row and its tree, what the PyO3 entry turns down, the bits of
//! `plan_of_values` on random values, and the Runs.
//!
//! The digests are the ones the pytest file pinned from the deleted Python rules
//! (`policies/social.py` and `policies/alternatives.py` at 2d8fb46): the tests write the same texts
//! with `Dumps` (`json.dumps` of the namespace's tree, the trace's document, the deltas with each
//! stress by `float.hex`), so each value is still the Python rule's answer.

mod support;

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use indexmap::IndexMap;
use serde_json::Value;
use sheltersim_core::cell::Shard;
use sheltersim_core::config::{Section, SectionId};
use sheltersim_core::error::Error;
use sheltersim_core::host::{Trace, TraceLevel};
use sheltersim_core::row::Field;
use sheltersim_core::rules::psychology::{Bad, Delta};
use sheltersim_core::rules::social_propagation::{self as social, Neighbors, Settings};
use sheltersim_core::rules::world_view::{self as view, ViewCache, ViewData};
use sheltersim_core::run::held::{Class, Held, Obj};
use sheltersim_core::run::stress::Stress;
use sheltersim_core::state::social_propagation::{SocialRow, NAME};
use sheltersim_core::value::V;
use sheltersim_core::world::{Fit, World, WorldCell};

use support::dumps::Dumps;
use support::{cases, load, sections, texts, Call, TestHost};

const FILE: &str = "model_steps";

/// What the deleted Python rules answered, as the pytest file pinned it (`ANSWERS`).
const ANSWERS: &[(&str, &str)] = &[
    ("group", "dd7c12c930b88477983e7ef0b47333e405e2dc251446845d128e4d1ab0d1ac5e"),
    ("ints-channel-[('family_channel_strength', 1), ('same_zone_channel_strength', 0)]", "f27599cb9e7be64bb0e04d3732512345eeb9379ef80ce5b9deea7ca3ec854164"),
    ("ints-channel-[('max_agent_stress_increase_per_minute', 0)]", "78e08c31dd81258ece624b9625ff9f75c2056e444f6ed8caffb035d88e4b839c"),
    ("ints-channel-[('max_pair_stress_increase', 0), ('nearby_zone_channel_strength', 1)]", "cbc200bf68ff866b3711a7e64733be0b206569a7d8ba11abc091c5ad32bb42bf"),
    ("ints-channel-[('max_pair_stress_increase', 0)]", "d08ee46091c2329f707b75c73ad6d4e8e19df45ad9270a8a9e573063e76a0d9c"),
    ("ints-channel-[('natural_recovery_per_minute', 0), ('stress_spread_per_minute', 0)]", "8b9d1886b023f0e87d5697ea2eba532852c554fb630fa81e45de6a9012f185c8"),
    ("ints-degroot-[('family_channel_strength', 1), ('same_zone_channel_strength', 0)]", "071ace780e0a06f1c99488d2fee78bc95064ac8fe8566d67093e513e09ccd2fd"),
    ("ints-degroot-[('max_agent_stress_increase_per_minute', 0)]", "6d2117d806b1b12e7b5213639112aa22b7d32c1e035dc79fc496804ffbf77556"),
    ("ints-degroot-[('max_pair_stress_increase', 0), ('nearby_zone_channel_strength', 1)]", "d386c14a045db9f35648e564e32835f508bb923a993727bde0ed15b38d24e7e1"),
    ("ints-degroot-[('max_pair_stress_increase', 0)]", "d386c14a045db9f35648e564e32835f508bb923a993727bde0ed15b38d24e7e1"),
    ("ints-degroot-[('natural_recovery_per_minute', 0), ('stress_spread_per_minute', 0)]", "ebe9af54bc1d4aba7429d7ed14063790a212bab2e3f4c7ac57006b2676bb2bf8"),
    ("ints-sparse-[('family_channel_strength', 1), ('same_zone_channel_strength', 0)]", "d7e36b1286cc9c9e344025fa620f5956ad7a92f19ade2aeb91c5e63e7c93e53c"),
    ("ints-sparse-[('max_agent_stress_increase_per_minute', 0)]", "af75a4dfd95ebd5d7dbd9ddb8d07fa7b73e53c9d133dfac2098f490d0f2742c0"),
    ("ints-sparse-[('max_pair_stress_increase', 0), ('nearby_zone_channel_strength', 1)]", "9000a526e3201281762b9f7e5a5e6391799f96194a775d0efbaa074b25812d96"),
    ("ints-sparse-[('max_pair_stress_increase', 0)]", "9000a526e3201281762b9f7e5a5e6391799f96194a775d0efbaa074b25812d96"),
    ("ints-sparse-[('natural_recovery_per_minute', 0), ('stress_spread_per_minute', 0)]", "c611db881b258e0203118183e718ae3f3980001bf1c3f85746256215366c74d3"),
    ("no-weight-channel_strength", "ee34cc5baccaf04b28e8e07db7e40f6f44f0bb802d16be353aff841ca7afc76e"),
    ("no-weight-source_expressiveness", "ee34cc5baccaf04b28e8e07db7e40f6f44f0bb802d16be353aff841ca7afc76e"),
    ("no-weight-stress_sensitivity", "ee34cc5baccaf04b28e8e07db7e40f6f44f0bb802d16be353aff841ca7afc76e"),
    ("restored", "410610de6cf26039c13f61fba9fd4183998e4be8623ba1ed44bc27e26697156a"),
    ("step-channel", "7f8ea0412eff688936fc0644a8f464773230ac675108c0eb0b37e7f8c349f289"),
    ("step-degroot", "70c26453be7b10a908dc0407375e76dada5998b7250b7acd259358f1092f483d"),
    ("step-sparse", "a805ab722e0a4f0b9b8223f82c66b388eb126180849b2b52045c59d1f5e39ea2"),
    ("trace-channel-0", "361ae546e0a0329277715bef8c63a62b45d1657562332487aafdef8b0ae5c4dd"),
    ("trace-channel-1", "1516096d50a09fad18da50faf43e730ced552e104c8fd51504e29844cdac22b0"),
    ("trace-channel-2", "5935d8fdcedd095fcca3261ec945889d15f6a5e8ec2a0252f92978f0c00a92c5"),
    ("trace-channel-3", "e43a1f504dcb29830f58cd04eb49a400e79cb57dc40e08890816a3bdef29cdc8"),
    ("trace-degroot-0", "8a8aac06e4021f51c912648f3e433091476021c9cf774225bf5a42e039c383c1"),
    ("trace-degroot-1", "76f74f88fe26ad98214844ba0b18c0764350d0bf10e21193b6b767d30538ca99"),
    ("trace-degroot-2", "3e7181475396149c842fe5e9556b28ec41b5558a79a3e7d6dab4e02b8b454808"),
    ("trace-degroot-3", "711fdf38ca20c7e6b85caa167e8033107d3bc178ffb1975ac8d4ef0baa167d34"),
    ("trace-sparse-0", "2d8bbe76417ddfcad2b6b7c3c11700ed23dfb0fefc6b53df7c1eba9a1e575f62"),
    ("trace-sparse-1", "7baf14348b0a758c011765886c7000e28b72d2c6461d390e3fa926eebb689d8c"),
    ("trace-sparse-2", "d54b14c13f866cfe0a37347b843760e7c4861a0a486cda53dff1f3e517badbc2"),
    ("trace-sparse-3", "5a87b37453a94349e6d4cfab0c996bd93b855aa78ce470bc6ae7a9b26887364b"),
];

/// `_answers`: the sha256 of the texts, each with a newline, against the pinned digest.
fn answers(name: &str, texts: &[String]) {
    let wanted = ANSWERS.iter().find(|(key, _)| *key == name).unwrap_or_else(|| panic!("no answer {name}")).1;
    let joined: String = texts.iter().map(|text| format!("{text}\n")).collect();
    assert_eq!(texts::sha(&joined), wanted, "{name}");
}

// ---- The fixture --------------------------------------------------------------------------------

fn data() -> &'static Value {
    static DATA: OnceLock<Value> = OnceLock::new();
    DATA.get_or_init(|| load::data(FILE))
}

fn cases_of() -> &'static Value {
    &data()["social"]
}

/// A named world of the file, as an entry.
fn entry(name: &str) -> Value {
    cases::resolve(&data()["worlds"], name)
}

/// The step's policy as the Run read it of `controller.stress_policy`.
fn stress(policy: &Value) -> Stress {
    let document = cases::resolve(&data()["policies"], policy.as_str().unwrap());
    let mut controller = Obj::new(Class::named("sheltersim.experiments.runner", "OfflineSimulationController"));
    controller.attrs.insert(Arc::from("stress_policy"), Ok(Held::Object(Arc::new(load::obj(&document["object"])))));
    Stress::of(&controller)
}

fn got<T>(read: Result<T, Bad>) -> T {
    read.unwrap_or_else(|bad| panic!("the policy was not read: {bad:?}"))
}

/// `support.world.view_of(world, include_state_hash=False)`: the update phase's view, built anew.
fn view_of(world: &World, entry: &Value) -> ViewData {
    let configs = sections::section(entry, SectionId::Selection);
    let selection = match configs.selection.get() {
        Some(Section::Ok(selection)) => Ok(selection.clone()),
        Some(Section::Fault(fault)) => Err(fault.clone()),
        _ => panic!("no selection"),
    };
    let extensions = match sections::section(entry, SectionId::ExtensionsEnabled).extensions_enabled.get() {
        Some(Section::Ok(enabled)) => **enabled,
        _ => panic!("no extensions flag"),
    };
    let inputs = view::Inputs { selection: &selection, configs: None };
    view::build(world, &inputs, &mut ViewCache::default()).unwrap().finish(extensions)
}

/// The deltas of `_small`: each person's, by ID, with stress and trust 0.5.
fn deltas(world: &World, minute: i64) -> Vec<Delta> {
    let mut ids: Vec<Arc<str>> = world.rows().unwrap().agents.iter().map(|(id, _)| id.clone()).collect();
    ids.sort();
    ids.into_iter()
        .map(|agent_id| Delta {
            agent_id,
            minute_index: minute,
            toilet_urgency: 0.0,
            hunger: 0.0,
            fatigue: 0.0,
            hydration_need: 0.0,
            stress: 0.5,
            trust: 0.5,
            body: None,
        })
        .collect()
}

/// `controller.zone_neighbors`: each zone with no neighbor, or, with `all`, every zone.
fn neighbors(world: &World, all: bool) -> Neighbors {
    let mut zones: Vec<String> = world.rows().unwrap().zones.iter().map(|(id, _)| id.to_string()).collect();
    zones.sort();
    Ok(zones.iter().map(|zone| (zone.clone(), Ok(if all { zones.clone() } else { Vec::new() }))).collect::<HashMap<_, _>>())
}

/// One call of the PyO3 entry with a controller whose policy is `stress`: the step, then the
/// trace's document to the host when it keeps one. The final deltas.
fn step(cell: &Shard<World>, stress: &Stress, view: &ViewData, deltas: &[Delta], neighbors: &Neighbors, host: &mut TestHost) -> Result<Vec<Delta>, Error> {
    assert!(got(social::enabled(stress)));
    let settings: Settings = got(social::settings(stress));
    let traced = host.level != TraceLevel::Off;
    let inputs = social::Inputs {
        settings: &settings,
        expressiveness: got(social::expressiveness(stress)),
        view,
        deltas,
        minute: view.minute_index,
        seed: "1",
        neighbors,
        traced,
    };
    let planned = cell.with(|world| social::update(world, &inputs)).unwrap()?;
    assert!(!planned.first_use, "the worlds have the namespace");
    if traced {
        social::record_social(host, &planned, &V::Int(view.minute_index), &got(social::policy_version(stress))).unwrap();
    }
    Ok(planned.deltas(deltas))
}

/// The trace documents the host was given, as `json.dumps(trace, ensure_ascii=False)`.
fn documents(host: &TestHost) -> Vec<String> {
    host.calls
        .iter()
        .filter_map(|call| match call {
            Call::Trace(Trace::SocialContact(document)) => Some(Dumps::default().unicode().text(document)),
            _ => None,
        })
        .collect()
}

// ---- The texts the pytest file wrote ----------------------------------------------------------

fn named(pairs: Vec<(&str, V)>) -> V {
    V::Map(pairs.into_iter().map(|(name, value)| (Arc::from(name), value)).collect::<IndexMap<_, _>>())
}

/// The namespace's tree as Python reads it (the row's order, `NAMES`).
fn tree(row: &SocialRow) -> V {
    let contacts = row
        .contacts
        .iter()
        .map(|(key, record)| {
            let record = named(vec![("last_seen_minute", V::Int(record.last_seen_minute)), ("consecutive_minutes", V::Int(record.consecutive_minutes))]);
            (key.clone(), record)
        })
        .collect();
    let cooldowns = row.agent_cooldown_until.iter().map(|(id, due)| (id.clone(), V::Int(*due))).collect();
    named(vec![("schema_version", V::Int(row.schema_version)), ("contacts", V::Map(contacts)), ("agent_cooldown_until", V::Map(cooldowns))])
}

/// `json.dumps(world.model_state[KEY])`.
fn state_text(cell: &Shard<World>) -> String {
    cell.with(|world| Dumps::default().text(&tree(&world.row::<SocialRow>(NAME).unwrap()))).unwrap()
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

/// `_deltas_text`: `asdict` of each delta, its stress by `float.hex`.
fn deltas_text(deltas: &[Delta]) -> String {
    let body_names = ["sleep_debt_minutes", "uncertainty", "reassurance", "discontent", "positive_affect", "negative_affect", "hygiene_need"];
    let items = deltas
        .iter()
        .map(|delta| {
            let mut fields = vec![
                ("agent_id", V::Str(delta.agent_id.clone())),
                ("minute_index", V::Int(delta.minute_index)),
                ("toilet_urgency", V::Float(delta.toilet_urgency)),
                ("hunger", V::Float(delta.hunger)),
                ("fatigue", V::Float(delta.fatigue)),
                ("stress", V::Str(Arc::from(hex(delta.stress)))),
                ("trust", V::Float(delta.trust)),
                ("hydration_need", V::Float(delta.hydration_need)),
            ];
            for (place, name) in body_names.into_iter().enumerate() {
                fields.push((name, delta.body.map_or(V::Null, |body| V::Float(body[place]))));
            }
            named(fields)
        })
        .collect();
    Dumps::default().text(&V::List(items))
}

// ---- Setting the world as the pytest file did ------------------------------------------------

fn set_minute(cell: &Shard<World>, minute: i64) {
    cell.with(|world| world.scalars.get_mut().minute_index = Fit::Ok(minute)).unwrap();
}

/// Each person's stress, in the order of `world.agents` (`order`).
fn set_stresses(cell: &Shard<World>, stresses: &Value) {
    let order = cases_of()["order"].as_array().unwrap();
    cell.with(|world| {
        let mut rows = world.rows_mut().unwrap();
        for (id, stress) in order.iter().zip(stresses.as_array().unwrap()) {
            rows.agents.get_mut(id.as_str().unwrap()).unwrap().stress = stress.as_f64().unwrap();
        }
    })
    .unwrap();
}

/// The minutes of a test that draws each person's stress and each delta's: the texts of each, the
/// trace's document and the namespace, and with `ints` (each zone next to every zone) the deltas.
fn minutes(name: &str, policy: &Value, minutes: &Value, ints: bool) -> Vec<String> {
    let entry = entry("social");
    let cell = texts::cell(&entry);
    let stress = stress(policy);
    let neighbors = cell.with(|world| neighbors(world, ints)).unwrap();
    let mut host = TestHost { level: TraceLevel::Full, ..TestHost::default() };
    let mut out = Vec::new();
    for (minute, drawn) in minutes.as_array().unwrap().iter().enumerate() {
        let minute = minute as i64;
        set_minute(&cell, minute);
        set_stresses(&cell, &drawn["stresses"]);
        let view = cell.with(|world| view_of(world, &entry)).unwrap();
        let mut base = cell.with(|world| deltas(world, minute)).unwrap();
        for (delta, stress) in base.iter_mut().zip(drawn["bases"].as_array().unwrap()) {
            delta.stress = stress.as_f64().unwrap();
        }
        let finals = step(&cell, &stress, &view, &base, &neighbors, &mut host).unwrap();
        out.push(documents(&host).pop().unwrap_or_else(|| panic!("{name}: no document")));
        out.push(state_text(&cell));
        if ints {
            out.push(deltas_text(&finals));
        }
    }
    assert_eq!(documents(&host).len(), minutes.as_array().unwrap().len(), "{name}");
    out
}

// ---- The tests ----------------------------------------------------------------------------------

/// `test_the_step_writes_what_the_python_rule_wrote`, of each model.
#[test]
fn the_step_writes_what_the_python_rule_wrote() {
    for (policy, name) in cases_of()["step"].as_object().unwrap() {
        let entry = entry("social");
        let cell = texts::cell(&entry);
        let view = cell.with(|world| view_of(world, &entry)).unwrap();
        let base = cell.with(|world| deltas(world, 0)).unwrap();
        let neighbors = cell.with(|world| neighbors(world, false)).unwrap();
        let finals = step(&cell, &stress(name), &view, &base, &neighbors, &mut TestHost::default()).unwrap();
        assert!(finals.iter().any(|delta| delta.stress != 0.5), "{policy}");
        answers(&format!("step-{policy}"), &[state_text(&cell), deltas_text(&finals)]);
    }
}

/// `test_the_document_of_a_trace_is_the_python_rule_s`: five minutes of each model and seed, on
/// values no scenario has, so contacts last and cooldowns count.
#[test]
fn the_document_of_a_trace_is_the_python_rule_s() {
    let cases = cases_of();
    for case in cases["trace"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let drawn = &cases["trace_minutes"][case["seed"].to_string()];
        answers(name, &minutes(name, &case["policy"], drawn, false));
    }
}

/// `test_int_coefficients_of_a_scenario_give_the_python_rule_s_answers`: a coefficient written as
/// `0` or `1` gives the Python rule's answers, an int cap staying an int in the trace.
#[test]
fn int_coefficients_of_a_scenario_give_the_python_rule_s_answers() {
    let cases = cases_of();
    for case in cases["ints"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        answers(name, &minutes(name, &case["policy"], &cases["ints_minutes"], true));
    }
}

/// `test_a_restored_namespace_gets_the_order_of_the_python_rule`: a namespace read again with its
/// keys sorted (a restore) gets the records in the order the rule writes them.
#[test]
fn a_restored_namespace_gets_the_order_of_the_python_rule() {
    let entry = entry("social");
    let cell = texts::cell(&entry);
    let stress = stress(&cases_of()["restored"]);
    let mut view = cell.with(|world| view_of(world, &entry)).unwrap();
    let neighbors = cell.with(|world| neighbors(world, false)).unwrap();
    let base = cell.with(|world| deltas(world, 0)).unwrap();
    step(&cell, &stress, &view, &base, &neighbors, &mut TestHost::default()).unwrap();
    // `json.loads(json.dumps(state, sort_keys=True))`, assigned.
    let sorted = cell
        .with(|world| {
            let mut row = world.row_mut::<SocialRow>(NAME).unwrap();
            let before: Vec<_> = row.contacts.keys().cloned().collect();
            row.canonicalize();
            before != row.contacts.keys().cloned().collect::<Vec<_>>()
        })
        .unwrap();
    assert!(sorted, "the restore changed no order");
    view.minute_index = 1;
    let base = cell.with(|world| deltas(world, 1)).unwrap();
    step(&cell, &stress, &view, &base, &neighbors, &mut TestHost::default()).unwrap();
    answers("restored", &[state_text(&cell)]);
}

/// `test_the_group_comes_from_the_view_and_not_the_profile`: the view's household, not
/// `profile.group_id`, makes a family contact.
#[test]
fn the_group_comes_from_the_view_and_not_the_profile() {
    let entry = entry("social");
    let stress = stress(&cases_of()["group"]);
    let run = |everyone: bool| {
        let cell = texts::cell(&entry);
        let mut view = cell.with(|world| view_of(world, &entry)).unwrap();
        if everyone {
            for (index, agent) in view.agents.iter_mut().enumerate() {
                agent.group_id = (index % 2 == 1).then(|| Arc::from("everyone"));
            }
        }
        let base = cell.with(|world| deltas(world, 0)).unwrap();
        let neighbors = cell.with(|world| neighbors(world, false)).unwrap();
        step(&cell, &stress, &view, &base, &neighbors, &mut TestHost::default()).unwrap();
        cell
    };
    let from_profiles = run(false);
    let from_view = run(true);
    let contacts = |cell: &Shard<World>| {
        cell.with(|world| match tree(&world.row::<SocialRow>(NAME).unwrap()) {
            V::Map(tree) => Dumps::default().text(&tree["contacts"]),
            _ => unreachable!(),
        })
        .unwrap()
    };
    assert_ne!(contacts(&from_view), contacts(&from_profiles));
    answers("group", &[state_text(&from_view)]);
}

/// `test_contacts_without_weight_shift_nothing` (#87): a total weight of 0 makes contacts and moves
/// no stress; the draws, the trace and the row are the Python rule's.
#[test]
fn contacts_without_weight_shift_nothing() {
    for case in cases_of()["no_weight"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let entry = entry(case["world"].as_str().unwrap());
        let cell = texts::cell(&entry);
        let view = cell.with(|world| view_of(world, &entry)).unwrap();
        let base = cell.with(|world| deltas(world, 0)).unwrap();
        let neighbors = cell.with(|world| neighbors(world, false)).unwrap();
        let mut host = TestHost { level: TraceLevel::Full, ..TestHost::default() };
        sheltersim_core::rng::start_draw_log();
        let finals = step(&cell, &stress(&case["policy"]), &view, &base, &neighbors, &mut host);
        let draws = sheltersim_core::rng::take_draw_log();
        let finals = finals.unwrap();
        let document = documents(&host).remove(0);
        let read: Value = serde_json::from_str(&document).unwrap();
        assert!(read["selected_contact_count"].as_i64().unwrap() > 0, "{name}");
        assert_eq!(read["contributing_contact_count"], 0, "{name}");
        assert_eq!(finals, base, "{name}");
        assert!(cell.with(|world| world.row::<SocialRow>(NAME).unwrap().agent_cooldown_until.is_empty()).unwrap(), "{name}");
        let draws = V::List(draws.into_iter().map(|(key, value)| V::List(vec![V::Str(Arc::from(key)), V::Str(Arc::from(hex(value)))])).collect());
        answers(name, &[Dumps::default().text(&draws), document, state_text(&cell)]);
    }
}

/// `test_unsorted_deltas_are_sorted_except_for_degroot`: the Python rules sorted the deltas by ID,
/// but DeGroot's, which kept their order.
#[test]
fn unsorted_deltas_are_sorted_except_for_degroot() {
    let cases = &cases_of()["unsorted"];
    let entry = entry("social");
    let run = |policy: &Value, order: &dyn Fn(Vec<Delta>) -> Vec<Delta>| {
        let cell = texts::cell(&entry);
        let view = cell.with(|world| view_of(world, &entry)).unwrap();
        let base = order(cell.with(|world| deltas(world, 0)).unwrap());
        let neighbors = cell.with(|world| neighbors(world, false)).unwrap();
        step(&cell, &stress(policy), &view, &base, &neighbors, &mut TestHost::default())
    };
    let backwards = |mut deltas: Vec<Delta>| {
        deltas.reverse();
        deltas
    };
    let sorted = run(&cases["channel"], &|deltas| deltas).unwrap();
    assert_eq!(run(&cases["channel"], &backwards).unwrap(), sorted);
    let twice = |deltas: Vec<Delta>| {
        let mut out = vec![deltas[0].clone()];
        out.extend_from_slice(&deltas[..deltas.len() - 1]);
        out
    };
    assert_eq!(run(&cases["channel"], &twice).err(), Some(Error::unsupported("two deltas for one person")));
    assert_eq!(run(&cases["degroot"], &backwards).err(), Some(Error::unsupported("the deltas are not in id order")));
}
