//! The standard synthetic shelter in the core (`scenario::facility::generate`, Issue #296) gives
//! the design Python's `StandardShelterGenerator.generate` gives for the inputs of the Scenario
//! screen: `tests/data/scenario_facility.json` (`tests/support/core_scenario_facility.py`) holds
//! cases of edge values and random inputs with the sha256 of the design's canonical JSON, two
//! designs in full, and the coefficients Python's generator reads.

use serde_json::Value;
use sheltersim_core::checkpoint::sha256_hex;
use sheltersim_core::scenario::facility::{coefficients, generate, FacilityRequest};
use sheltersim_core::value::V;

fn data() -> Value {
    let path = format!("{}/tests/data/scenario_facility.json", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

fn request(input: &Value) -> FacilityRequest {
    FacilityRequest {
        population: input["population"].as_i64().unwrap(),
        gross_area_m2: input["gross_area_m2"].as_f64(),
        accessible_toilet_count: input["accessible_toilet_count"].as_i64().unwrap(),
        staff_count: input["staff_count"].as_i64(),
        facility_conditions: input["facility_conditions"].as_array().unwrap().iter().map(|item| item.as_str().unwrap().to_owned()).collect(),
    }
}

fn design_bytes(input: &Value) -> Vec<u8> {
    generate(&request(input)).unwrap_or_else(|error| panic!("{input}: {error:?}")).dump().to_json().unwrap()
}

#[test]
fn the_coefficients_are_python_s() {
    let data = data();
    let constants = &data["constants"];
    let int = |name: &str| constants[name].as_i64().unwrap_or_else(|| panic!("{name}"));
    let float = |name: &str| constants[name].as_f64().unwrap_or_else(|| panic!("{name}"));
    use coefficients::*;
    assert_eq!(float("accessible_route_min_width_m"), sheltersim_core::scenario::facility::ACCESSIBLE_ROUTE_MIN_WIDTH_M);
    assert_eq!(int("people_per_toilet_initial"), PEOPLE_PER_TOILET_INITIAL);
    assert_eq!(int("planning_toilet_uses_per_person_day"), PLANNING_TOILET_USES_PER_PERSON_DAY);
    assert_eq!(int("toilet_minimum_minutes"), TOILET_MINIMUM_MINUTES);
    assert_eq!(int("toilet_maximum_minutes"), TOILET_MAXIMUM_MINUTES);
    assert_eq!(int("operating_minutes_per_day"), OPERATING_MINUTES_PER_DAY);
    assert_eq!(float("target_utilization"), TARGET_UTILIZATION);
    assert_eq!(int("resource_service_minutes"), RESOURCE_SERVICE_MINUTES);
    assert_eq!(float("hydration_growth_per_minute"), HYDRATION_GROWTH_PER_MINUTE);
    assert_eq!(float("hydration_relief"), HYDRATION_RELIEF);
    assert_eq!(float("hydration_action_threshold"), HYDRATION_ACTION_THRESHOLD);
    assert_eq!(float("hunger_growth"), HUNGER_GROWTH);
    assert_eq!(float("hunger_relief"), HUNGER_RELIEF);
    assert_eq!(float("hunger_action_threshold"), HUNGER_ACTION_THRESHOLD);
    assert_eq!(constants.as_object().unwrap().len(), 14, "a coefficient was added in Python");
}

#[test]
fn a_sample_design_is_python_s_value_for_value() {
    let data = data();
    let cases = data["cases"].as_array().unwrap();
    for (name, index) in [("twelve_with_pets", 4), ("one_tiny_area", 2)] {
        let made = generate(&request(&cases[index]["input"])).unwrap().dump();
        let expected = V::from_json(&data["samples"][name]).unwrap();
        if made != expected {
            let (made, expected) = (String::from_utf8(made.to_json().unwrap()).unwrap(), String::from_utf8(expected.to_json().unwrap()).unwrap());
            let at = made.bytes().zip(expected.bytes()).position(|(a, b)| a != b).unwrap_or(made.len().min(expected.len()));
            let from = at.saturating_sub(120);
            panic!("{name} differs at byte {at}:\n made     {}\n expected {}", &made[from..(at + 80).min(made.len())], &expected[from..(at + 80).min(expected.len())]);
        }
    }
}

#[test]
fn every_case_gives_python_s_design_or_refusal() {
    let data = data();
    let mut failures = Vec::new();
    for case in data["cases"].as_array().unwrap() {
        let input = &case["input"];
        match (generate(&request(input)), case.get("raises")) {
            (Err(error), Some(text)) => assert!(format!("{error:?}").contains(text.as_str().unwrap()), "{input}: {error:?}"),
            (Err(error), None) => failures.push(format!("{input}: refused {error:?}")),
            (Ok(_), Some(text)) => failures.push(format!("{input}: Python raises {text}")),
            (Ok(_), None) => {
                let bytes = design_bytes(input);
                if sha256_hex(&bytes) != case["sha256"].as_str().unwrap() {
                    failures.push(format!("{input}: {} bytes, Python {}", bytes.len(), case["length"]));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{} of {} cases differ:\n{}", failures.len(), data["cases"].as_array().unwrap().len(), failures.join("\n"));
}

/// The designs of the scenario corpus (`scenario_corpus.json`, #293): the screen's requests as
/// the Python server read them give the same design here.
#[test]
fn the_corpus_designs_are_python_s() {
    let path = format!("{}/tests/data/scenario_corpus.json", env!("CARGO_MANIFEST_DIR"));
    let corpus: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let mut checked = 0;
    for (name, case) in corpus["cases"].as_object().unwrap() {
        let Some(expected) = case.get("facility_design") else { continue };
        let request = &case["answer"]["request"];
        let input = serde_json::json!({
            "population": request["evacuees"],
            "gross_area_m2": request["gross_area_m2"],
            "accessible_toilet_count": request["accessible_toilet_count"],
            "staff_count": request["staff_count"],
            "facility_conditions": request["facility_conditions"],
        });
        let bytes = design_bytes(&input);
        assert_eq!(sha256_hex(&bytes), expected["sha256"].as_str().unwrap(), "{name}: {} bytes, Python {}", bytes.len(), expected["length"]);
        checked += 1;
    }
    assert!(checked > 40, "only {checked} corpus designs");
}
