//! The operational conditions, context and service capacity of a generated Scenario in the core
//! (Issue #298) are Python's (`tests/support/core_scenario_conditions.py`,
//! `data/scenario_conditions.json`).

use std::sync::Arc;

use serde_json::Value;
use sheltersim_core::checkpoint::sha256_hex;
use sheltersim_core::scenario::conditions::{
    with_standard_provisioning, Delivery, FacilityKind, RESOURCE_SERVICE_CONFIG_JSON,
};
use sheltersim_core::scenario::connections::{population_context, spatial_constraints};
use sheltersim_core::scenario::population::{generate_population, PopulationInput};
use sheltersim_core::scenario::request::InitialScenarioRequest;
use sheltersim_core::scenario::service_capacity::service_capacity_diagnostic;
use sheltersim_core::value::V;

fn fixture() -> Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/scenario_conditions.json");
    serde_json::from_slice(&std::fs::read(path).expect("the fixture")).expect("its JSON")
}

fn v(value: &Value) -> V {
    V::from_json(value).expect("a value Python reads")
}

fn map(entries: Vec<(&str, V)>) -> V {
    V::Map(entries.into_iter().map(|(key, value)| (Arc::from(key), value)).collect())
}

fn texts(values: &[String]) -> V {
    V::List(values.iter().map(|value| V::Str(Arc::from(value.as_str()))).collect())
}

/// What `provisioning_answer` writes, made by the core.
fn answer(document: &Value, case: &Value) -> V {
    let request = serde_json::json!({
        "municipality": "墨田区",
        "disaster": "earthquake",
        "evacuees": case["people"],
        "operational_conditions": {"initial_resources": document["stocks"][case["stock"].as_str().unwrap()]},
    });
    let request = InitialScenarioRequest::of_value(&v(&request)).expect("a valid request");
    let conditions = &request.operational_conditions;
    let minutes = case["minutes"].as_i64().unwrap();
    let (provisioned, record) =
        with_standard_provisioning(conditions, case["people"].as_i64().unwrap(), minutes, case["supply_day"].as_i64().unwrap());
    let kinds: Vec<String> = document["facility_sets"][case["facilities"].as_str().unwrap_or("both")]
        .as_array()
        .unwrap()
        .iter()
        .map(|kind| kind.as_str().unwrap().to_string())
        .collect();
    let ids: Vec<String> = kinds.iter().enumerate().map(|(index, kind)| format!("{kind}_{index}")).collect();
    let facilities: Vec<FacilityKind> =
        kinds.iter().zip(&ids).map(|(kind, id)| FacilityKind { id, kind }).collect();
    let compiled = provisioned.compile(minutes, &facilities).expect("compiled");
    let requested = sheltersim_core::scenario::conditions::Conditions::of_request(conditions);
    map(vec![
        ("provisioned_scheduled", V::List(provisioned.scheduled.iter().map(Delivery::dump).collect())),
        ("record", record.unwrap_or(V::Null)),
        ("initial_stock", compiled.initial_stock.as_ref().map_or(V::Null, |stock| stock.dump())),
        ("events", V::List(compiled.scheduled_events.clone())),
        ("model_configs", V::Map(compiled.model_configs.iter().map(|(name, config)| (Arc::from(name.as_str()), config.clone())).collect())),
        ("blockers", texts(&compiled.blockers)),
        ("request_sources", V::List(requested.provenance_sources())),
        ("provisioned_sources", V::List(provisioned.provenance_sources())),
        ("request_referenced", texts(&requested.referenced_source_ids())),
        ("provisioned_referenced", texts(&provisioned.referenced_source_ids())),
    ])
}

#[test]
fn the_resource_service_config_is_python_s() {
    assert_eq!(RESOURCE_SERVICE_CONFIG_JSON.len(), fixture()["resource_service_config"].as_str().unwrap().len());
    let parsed: Value = serde_json::from_str(RESOURCE_SERVICE_CONFIG_JSON).unwrap();
    let python: Value = serde_json::from_str(fixture()["resource_service_config"].as_str().unwrap()).unwrap();
    assert_eq!(v(&parsed).to_json().unwrap(), v(&python).to_json().unwrap());
}

#[test]
fn provisioning_and_compiling_are_python_s() {
    let document = fixture();
    let example = &document["provisioning_example"];
    let made = answer(&document, &example["case"]);
    assert_eq!(
        String::from_utf8(made.to_json().unwrap()).unwrap(),
        String::from_utf8(v(&example["answer"]).to_json().unwrap()).unwrap()
    );
    let mut wrong = Vec::new();
    let cases = document["provisioning"].as_array().unwrap();
    for case in cases {
        if sha256_hex(&answer(&document, case).to_json().unwrap()) != case["sha256"].as_str().unwrap() {
            wrong.push(case.to_string());
        }
    }
    assert!(cases.len() > 300);
    assert!(wrong.is_empty(), "{} cases differ:\n{}", wrong.len(), wrong.join("\n"));
}

#[test]
fn the_context_and_the_diagnostic_of_built_scenarios_are_python_s() {
    let document = fixture();
    for case in document["built"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        if let Some(evacuees) = case["evacuees"].as_i64() {
            let population = generate_population(&PopulationInput::for_request(evacuees, case["seed"].as_i64().unwrap())).unwrap();
            assert_eq!(sha256_hex(&population_context(&population).to_json().unwrap()), case["population_context"].as_str().unwrap(), "{name}");
            let geometry = map(vec![("routes", v(&case["routes"]))]);
            let constraints = spatial_constraints(Some(&v(&case["editable_geometry"])), &geometry).unwrap();
            assert_eq!(sha256_hex(&constraints.to_json().unwrap()), case["spatial_constraints"].as_str().unwrap(), "{name}");
        }
        let input = &case["diagnostic_input"];
        let count = input["profile_count"].as_i64().unwrap();
        let profiles = (1..=count).map(|number| map(vec![("id", V::Str(Arc::from(format!("A{number:03}").as_str())))])).collect();
        let scenario = map(vec![
            ("id", v(&input["id"])),
            ("profiles", V::List(profiles)),
            ("facilities", v(&input["facilities"])),
            ("staff", v(&input["staff"])),
            ("model_configs", v(&input["model_configs"])),
        ]);
        let diagnostic = service_capacity_diagnostic(&scenario).unwrap();
        assert_eq!(
            String::from_utf8(diagnostic.to_json().unwrap()).unwrap(),
            String::from_utf8(v(&case["diagnostic"]).to_json().unwrap()).unwrap(),
            "{name}"
        );
    }
}

#[test]
fn without_a_plan_the_constraints_are_empty() {
    let constraints = spatial_constraints(None, &V::Null).unwrap();
    assert_eq!(
        String::from_utf8(constraints.to_json().unwrap()).unwrap(),
        r#"{"accessible_facility_ids":[],"equipment_footprints":[],"obstacles":[],"openings":[],"planning_capacity_by_zone":{},"schema_version":1,"walls":[]}"#
    );
}
