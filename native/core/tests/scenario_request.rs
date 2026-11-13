//! `InitialScenarioRequest` in the core (Issue #295) answers each request as Python does: the same
//! dump bytes where Python accepts it, a refusal where Python refuses it, and `Unsupported` where
//! Python accepts a field the browser computes only at its default. The cases and Python's answers
//! are `tests/support/core_scenario_request.py`'s (`data/scenario_request.json`).

use serde_json::Value;
use sheltersim_core::checkpoint::sha256_hex;
use sheltersim_core::scenario::request::{InitialScenarioRequest, Refusal};
use sheltersim_core::value::V;

fn fixture() -> Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/scenario_request.json");
    serde_json::from_slice(&std::fs::read(path).expect("the fixture")).expect("its JSON")
}

/// The browser's answer: "ok:<input_sha256>", "invalid" or "unsupported".
fn answer(request: &Value) -> (String, Option<InitialScenarioRequest>) {
    let value = V::from_json(request).expect("a value Python reads");
    match InitialScenarioRequest::of_value(&value) {
        Ok(parsed) => (format!("ok:{}", parsed.input_sha256()), Some(parsed)),
        Err(Refusal::Invalid(_)) => ("invalid".to_string(), None),
        Err(Refusal::Unsupported(_)) => ("unsupported".to_string(), None),
    }
}

#[test]
fn each_case_is_answered_as_python_answers_it() {
    let document = fixture();
    let mut wrong = Vec::new();
    for case in document["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let python = &case["python"];
        let expected = match case["browser"].as_str().unwrap() {
            "ok" => format!("ok:{}", python["input_sha256"].as_str().unwrap()),
            other => other.to_string(),
        };
        let (got, parsed) = answer(&case["request"]);
        if got != expected {
            wrong.push(format!("{name}: {got} (expected {expected})"));
            continue;
        }
        if let (Some(dump), Some(parsed)) = (python["dump"].as_str(), parsed) {
            assert_eq!(String::from_utf8(parsed.dump_json()).unwrap(), dump, "{name}");
            assert_eq!(sha256_hex(dump.as_bytes()), python["input_sha256"].as_str().unwrap(), "{name}");
        }
    }
    assert!(wrong.is_empty(), "{} cases differ:\n{}", wrong.len(), wrong.join("\n"));
}

#[test]
fn each_coercion_of_a_scalar_field_is_python_s() {
    let document = fixture();
    let base = document["base"].as_object().unwrap();
    let scalars = document["scalars"].as_array().unwrap();
    let mut wrong = Vec::new();
    let mut count = 0;
    for (field, answers) in document["coercions"].as_object().unwrap() {
        let answers = answers.as_array().unwrap();
        assert_eq!(answers.len(), scalars.len(), "{field}");
        for (value, expected) in scalars.iter().zip(answers) {
            let mut request = base.clone();
            request.insert(field.clone(), value.clone());
            let (got, _) = answer(&Value::Object(request));
            count += 1;
            if got != expected.as_str().unwrap() {
                wrong.push(format!("{field} = {value}: {got} (expected {expected})"));
            }
        }
    }
    assert!(count > 1000, "{count} coercions");
    assert!(wrong.is_empty(), "{} coercions differ:\n{}", wrong.len(), wrong.join("\n"));
}

#[test]
fn the_dump_of_the_example_form_is_the_server_s_request() {
    let raw = r#"{"municipality":"墨田区","disaster":"earthquake","evacuees":100,"duration_days":7}"#.as_bytes();
    let parsed = InitialScenarioRequest::of_json(raw).unwrap();
    assert_eq!(parsed.duration_minutes(), 10_080);
    let dump = String::from_utf8(parsed.dump_json()).unwrap();
    assert!(dump.contains(r#""duration_days":7.0,"#), "{dump}");
    assert!(matches!(InitialScenarioRequest::of_json(b"{"), Err(Refusal::Invalid(_))));
}
