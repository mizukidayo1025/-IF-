//! The Scenario builder in the core (`scenario::builder::assemble`, Issue #299) joins the parts of
//! a Scenario-screen request as Python's `InitialScenarioBuilder.build` and
//! `ProjectWorkspace.preview` do: `tests/data/scenario_builder.json`
//! (`tests/support/core_scenario_builder.py`) holds, for small cases of the scenario corpus (#293),
//! the parts Python made (request, research, population, compiled conditions, generated context,
//! service-capacity warnings) and the server's answer. The core makes the standard shelter itself
//! (`facility::generate`) and must give the same Scenario bytes (approved_hash), warnings and
//! blockers; the corpus's Scenario parts point at what differs.

use serde_json::Value;
use sheltersim_core::scenario::builder::{assemble, constants, Built, Conditions, Parts, ScenarioInput};
use sheltersim_core::scenario::facility::{generate, FacilityRequest};
use sheltersim_core::value::V;

fn read(name: &str) -> Value {
    let path = format!("{}/tests/data/{name}.json", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

fn v(value: &Value) -> V {
    V::from_json(value).unwrap()
}

fn strings(value: &Value) -> Vec<String> {
    value.as_array().unwrap().iter().map(|item| item.as_str().unwrap().to_owned()).collect()
}

fn build(case: &Value) -> Built {
    let request = v(&case["request"]);
    let research = v(&case["research"]);
    let population = v(&case["population"]);
    let raw = &case["request"];
    let design = generate(&FacilityRequest {
        population: raw["evacuees"].as_i64().unwrap(),
        gross_area_m2: raw["gross_area_m2"].as_f64(),
        accessible_toilet_count: raw["accessible_toilet_count"].as_i64().unwrap(),
        staff_count: raw["staff_count"].as_i64(),
        facility_conditions: strings(&raw["facility_conditions"]),
    })
    .unwrap();
    let c = &case["conditions"];
    let conditions = Conditions {
        initial_stock: if c["initial_stock"].is_null() { None } else { Some(v(&c["initial_stock"])) },
        scheduled_events: c["scheduled_events"].as_array().unwrap().iter().map(v).collect(),
        model_configs: c["model_configs"].as_array().unwrap().iter().map(|pair| (pair[0].as_str().unwrap().to_owned(), v(&pair[1]))).collect(),
        blockers: strings(&c["blockers"]),
        provenance_sources: c["provenance_sources"].as_array().unwrap().iter().map(v).collect(),
        provisioning: if c["provisioning"].is_null() { None } else { Some(v(&c["provisioning"])) },
        request_provenance_sources: c["request_provenance_sources"].as_array().unwrap().iter().map(v).collect(),
        request_referenced_source_ids: strings(&c["request_referenced_source_ids"]),
    };
    let connections: Vec<(String, V)> = case["connections"].as_object().unwrap().iter().map(|(key, value)| (key.clone(), v(value))).collect();
    let input = ScenarioInput {
        sha256: case["input"]["sha256"].as_str().unwrap().to_owned(),
        extraction_source: case["input"]["extraction_source"].as_str().unwrap().to_owned(),
    };
    let parts = Parts {
        request: &request,
        research: &research,
        design: &design,
        population: &population,
        conditions: &conditions,
        connections: &connections,
        input: &input,
        duration_days_given: case["duration_days_given"].as_bool().unwrap(),
    };
    let warnings = strings(&case["service_capacity_warnings"]);
    assemble(&parts, |_| Ok(warnings)).unwrap()
}

/// The corpus's parts of the Scenario that differ from the built one (`scenario_parts` of
/// `tests/support/scenario_corpus.py`).
fn differing_parts(name: &str, built: &Built) -> Vec<String> {
    let corpus = read("scenario_corpus");
    let mut expected = corpus["common"]["scenario_parts"].as_object().unwrap().clone();
    for (key, value) in corpus["cases"][name]["scenario"]["parts"].as_object().unwrap() {
        expected.insert(key.clone(), value.clone());
    }
    let document: Value = serde_json::from_slice(&built.canonical).unwrap();
    let short = |value: &Value| sheltersim_core::checkpoint::sha256_hex(&v(value).to_json().unwrap())[..16].to_owned();
    let mut made = serde_json::Map::new();
    for (key, value) in document.as_object().unwrap() {
        if key == "model_configs" || key == "provenance" {
            for (inner, part) in value.as_object().unwrap() {
                made.insert(format!("{key}.{inner}"), Value::String(short(part)));
            }
        } else {
            made.insert(key.clone(), Value::String(short(value)));
        }
    }
    let mut keys: Vec<&String> = expected.keys().chain(made.keys()).collect();
    keys.sort();
    keys.dedup();
    keys.into_iter().filter(|key| expected.get(*key) != made.get(*key)).cloned().collect()
}

#[test]
fn the_constants_load() {
    assert!(matches!(constants(), V::Map(_)), "constants.json does not parse");
}

#[test]
fn every_case_builds_python_s_scenario() {
    let data = read("scenario_builder");
    let mut failures = Vec::new();
    for (name, case) in data["cases"].as_object().unwrap() {
        let built = build(case);
        let expected = &case["expected"];
        if built.approved_hash != expected["approved_hash"].as_str().unwrap() {
            failures.push(format!("{name}: {} bytes, Python {}; parts differ: {:?}", built.canonical.len(), expected["length"], differing_parts(name, &built)));
        }
        let warnings = strings(&expected["warnings"]);
        // The server's warnings start with the text parser's (#294); the builder's end them.
        assert!(built.warnings.len() >= 3, "{name}: {:?}", built.warnings);
        if !warnings.ends_with(&built.warnings) {
            failures.push(format!("{name}: warnings {:?}, Python {:?}", built.warnings, warnings));
        }
        if built.approval_blockers != strings(&expected["approval_blockers"]) || built.can_start != expected["can_start"].as_bool().unwrap() {
            failures.push(format!("{name}: blockers {:?}", built.approval_blockers));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Every Scenario of the scenario corpus (#293), built in the core from the request the server
/// read and the research it made: the same approved_hash, design, geometry hash, warnings and
/// blockers.
#[test]
fn the_corpus_scenarios_are_built_in_the_core() {
    use sheltersim_core::checkpoint::sha256_hex;
    use sheltersim_core::scenario::builder::build as build_request;
    let corpus = read("scenario_corpus");
    let mut failures = Vec::new();
    let mut checked = 0;
    for (name, case) in corpus["cases"].as_object().unwrap() {
        let Some(scenario) = case.get("scenario") else { continue };
        let answer = &case["answer"];
        let text = case["body"]["text"].as_str().unwrap_or("");
        // What the server validated: the form's request as posted, or the text parser's.
        let posted = case["body"].get("request").filter(|value| !value.is_null()).unwrap_or(&answer["request"]);
        let request = serde_json::to_vec(posted).unwrap();
        let input = if text.trim().is_empty() {
            let dumped = sheltersim_core::scenario::request::InitialScenarioRequest::of_json(&request).unwrap().input_sha256();
            ScenarioInput { sha256: dumped, extraction_source: "structured_form".to_owned() }
        } else {
            ScenarioInput { sha256: sha256_hex(text.as_bytes()), extraction_source: "rules_fallback".to_owned() }
        };
        let research = serde_json::to_vec(&corpus["research"][case["research"].as_str().unwrap()]).unwrap();
        let preview = match build_request(&request, &research, &input) {
            Ok(preview) => preview,
            Err(refusal) => {
                failures.push(format!("{name}: {refusal:?}"));
                continue;
            }
        };
        checked += 1;
        let built = &preview.built;
        if built.approved_hash != answer["approved_hash"].as_str().unwrap() {
            failures.push(format!("{name}: {} bytes, Python {}; parts differ: {:?}", built.canonical.len(), scenario["length"], differing_parts(name, built)));
        }
        if sha256_hex(&preview.facility_design.to_json().unwrap()) != case["facility_design"]["sha256"].as_str().unwrap() {
            failures.push(format!("{name}: facility_design"));
        }
        if preview.geometry_hash != answer["geometry_hash"].as_str().unwrap() {
            failures.push(format!("{name}: geometry_hash"));
        }
        if preview.request != v(&answer["request"]) {
            failures.push(format!("{name}: request dump"));
        }
        if !strings(&answer["warnings"]).ends_with(&built.warnings) || built.warnings.len() < 3 {
            failures.push(format!("{name}: warnings {:?}", built.warnings));
        }
        if built.approval_blockers != strings(&answer["approval_blockers"]) || built.can_start != answer["can_start"].as_bool().unwrap() {
            failures.push(format!("{name}: blockers {:?}", built.approval_blockers));
        }
    }
    assert!(failures.is_empty(), "{} of {checked}:\n{}", failures.len(), failures.join("\n"));
    assert!(checked > 40, "only {checked} corpus Scenarios");
}
