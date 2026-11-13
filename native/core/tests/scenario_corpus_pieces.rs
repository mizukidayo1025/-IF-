//! The pieces of the browser's Scenario builder against the scenario corpus (#293): for each
//! answer the Python server gave the screen (`data/scenario_corpus.json`), the core's request
//! contract (#295) and population (#297) give the same request dump and the same parts of the
//! Scenario. A part's digest is the first 16 hex digits of the sha256 of its canonical JSON, kept
//! in the case or, when every case has the same, in `common.scenario_parts`.

use serde_json::Value;
use sheltersim_core::checkpoint::sha256_hex;
use sheltersim_core::scenario::population::{generate_population, PopulationInput};
use sheltersim_core::scenario::request::{InitialScenarioRequest, Refusal};
use sheltersim_core::value::V;

fn corpus() -> Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/scenario_corpus.json");
    serde_json::from_slice(&std::fs::read(path).expect("the corpus")).expect("its JSON")
}

fn v(value: &Value) -> V {
    V::from_json(value).expect("a value Python reads")
}

fn short(value: &V) -> String {
    sha256_hex(&value.to_json().unwrap())[..16].to_string()
}

fn part<'a>(corpus: &'a Value, case: &'a Value, name: &str) -> Option<&'a str> {
    case["scenario"]["parts"][name].as_str().or_else(|| corpus["common"]["scenario_parts"][name].as_str())
}

/// The request the server validated: the form's when the body has one, else the text's.
fn validated(case: &Value) -> Option<Result<InitialScenarioRequest, Refusal>> {
    match case["body"].get("request") {
        Some(request) if request.as_object().is_some_and(|fields| !fields.is_empty()) => {
            Some(InitialScenarioRequest::of_value(&v(request)))
        }
        _ => case["answer"].get("request").map(|request| InitialScenarioRequest::of_value(&v(request))),
    }
}

#[test]
fn the_request_of_each_case_is_the_server_s() {
    let corpus = corpus();
    let mut checked = 0;
    for (name, case) in corpus["cases"].as_object().unwrap() {
        let Some(outcome) = validated(case) else { continue };
        match case["status"].as_i64().unwrap() {
            200 => {
                let request = outcome.unwrap_or_else(|refusal| panic!("{name}: {refusal:?}"));
                let expected = v(&case["answer"]["request"]).to_json().unwrap();
                assert_eq!(String::from_utf8(request.dump_json()).unwrap(), String::from_utf8(expected).unwrap(), "{name}");
                checked += 1;
            }
            // A refused body: the request is refused when it is what the server refused.
            400 if case["answer"]["error"] == "invalid project request" && case["body"].get("request").is_some() => {
                assert!(matches!(outcome, Err(Refusal::Invalid(_))), "{name}: {outcome:?}");
                checked += 1;
            }
            _ => {}
        }
    }
    assert!(checked > 40, "{checked} requests");
}

#[test]
fn the_population_of_each_scenario_is_the_server_s() {
    let corpus = corpus();
    let mut checked = 0;
    for (name, case) in corpus["cases"].as_object().unwrap() {
        if case["status"] != 200 || case["scenario"].is_null() {
            continue;
        }
        let request = InitialScenarioRequest::of_value(&v(&case["answer"]["request"])).unwrap();
        let population = generate_population(&PopulationInput::for_request(request.evacuees, request.seed)).unwrap();
        assert_eq!(Some(short(&population.population_dump()).as_str()), part(&corpus, case, "model_configs.population"), "{name}");
        // The Scenario's canonical JSON sorts its profiles by ID: A1000 comes before A101.
        let mut sorted: Vec<_> = population.profile_set.profiles.iter().collect();
        sorted.sort_by(|first, second| first.id.as_bytes().cmp(second.id.as_bytes()));
        let profiles = V::List(sorted.into_iter().map(|profile| profile.dump()).collect());
        assert_eq!(Some(short(&profiles).as_str()), part(&corpus, case, "profiles"), "{name}");
        assert_eq!(
            Some(short(&population.profile_set.manifest.dump()).as_str()),
            part(&corpus, case, "provenance.profile_generation"),
            "{name}"
        );
        checked += 1;
    }
    assert!(checked > 40, "{checked} scenarios");
}
