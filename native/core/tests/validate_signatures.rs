//! The extensions' signatures and the head of their validation (lane D of #250, #349):
//! `simulator_signatures(world.model_configs)` and the first checks of
//! `validate_simulator_extensions` (`temporary_spaces_v1`, the legacy queue marker, the required
//! states). Each broken world of `validate_signatures.json`
//! (`tests/support/core_validate_signatures.py`) gives Python's class and text at each entry point,
//! and the signatures of each world's configs are Python's, key for key in its order.

mod support;

use serde_json::Value;
use sheltersim_core::validate::signatures::simulator_signatures;
use sheltersim_core::validate::{py_str, View};
use support::validate::{self, class_of, load_entry, same_outcome, Files, Found};

const FILE: &str = "validate_signatures";

#[test]
fn a_broken_head_or_config_gives_python_s_first_error() {
    assert!(validate::check_file(FILE) >= 24);
}

#[test]
fn the_signatures_are_python_s() {
    let mut files = Files::default();
    let signed = files.data(FILE)["signatures"].as_array().cloned().expect("the file has its signatures");
    let mut failures = Vec::new();
    for item in &signed {
        let name = item["name"].as_str().unwrap();
        let world = load_entry(&files.entry(FILE, item["world"].as_str().unwrap())).unwrap_or_else(|error| panic!("{name}: {error}"));
        let got: Value = match simulator_signatures(&View::new(&world, true, None)) {
            Ok(signatures) => Value::Array(signatures.iter().map(|(key, value)| Value::from(vec![key.to_string(), value.to_string()])).collect()),
            Err(error) => Value::from(vec![class_of(&error), py_str(&error)]),
        };
        // The core has no class of its own for pydantic's ValidationError (#360).
        let pair = |value: &Value| -> Found { value.as_array().filter(|pair| pair.len() == 2 && pair[0].is_string()).map(|pair| (pair[0].as_str().unwrap().to_owned(), pair[1].as_str().unwrap_or_default().to_owned())) };
        let same = match (pair(&item["expected"]), pair(&got)) {
            (Some(wanted), Some(found)) => same_outcome(&Some(wanted), &Some(found)),
            _ => got == item["expected"],
        };
        if !same {
            failures.push(format!("{name}: Python {}, the core {got}", item["expected"]));
        }
    }
    assert!(failures.is_empty(), "{} of {} differ:\n{}", failures.len(), signed.len(), failures.join("\n"));
    assert!(signed.len() >= 40);
}
