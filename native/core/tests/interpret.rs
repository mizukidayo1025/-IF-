//! The offline intervention compiler (#303): `sheltersim_core::interpret::compile_offline` answers
//! each request of `tests/data/offline_interpreter.json` (written by
//! `tests/support/core_interpret.py`) with the bytes whose sha256 Python's `compile_natural_language`
//! gave without an API key, or refuses it (`Error::Value`) where Python raised a ValueError.

use std::fs;

use serde_json::{json, Value};
use sheltersim_core::checkpoint::sha256_hex;
use sheltersim_core::error::Error;
use sheltersim_core::interpret::compile_offline;

#[test]
fn offline_answers_are_pythons() {
    let bytes = fs::read(format!("{}/tests/data/offline_interpreter.json", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let fixture: Value = serde_json::from_slice(&bytes).unwrap();
    let mut failures = Vec::new();
    let cases = fixture["cases"].as_array().unwrap();
    for case in cases {
        let request = json!({
            "text": case["text"],
            "branch_id": case["branch_id"],
            "selected_minute": case["minute"],
            "scenario_summary": fixture["summaries"][case["summary"].as_str().unwrap()],
        });
        let expected = case["answer"].as_str().unwrap();
        let got = match compile_offline(&serde_json::to_vec(&request).unwrap()) {
            Ok(body) => sha256_hex(&body),
            Err(Error::Value(_)) => "invalid".to_owned(),
            Err(error) => format!("error: {error}"),
        };
        if got != expected {
            let shown = compile_offline(&serde_json::to_vec(&request).unwrap())
                .map(|body| String::from_utf8(body).unwrap())
                .unwrap_or_else(|error| error.to_string());
            failures.push(format!("{} / {:?}: expected {expected}, got {got}\n  {shown}", case["summary"], case["text"]));
        }
    }
    assert!(failures.is_empty(), "{} of {} differ:\n{}", failures.len(), cases.len(), failures.join("\n"));
}
