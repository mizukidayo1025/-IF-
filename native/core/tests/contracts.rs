//! The input contracts (policy E15) without Python (Issue #346): `contracts::validate` gives, for
//! each input of `tests/data/contracts_*.json`, what the native host's pydantic gave (recorded by
//! `tests/support/core_contracts.py`): the same dump, or the same class of error with the same
//! text, byte for byte.
//!
//! A case's input is its own (`input`), a guard nested `chain` levels deep, or a valid input of the
//! file (`bases`) with a patch: each change a path and the value to set there (a new key goes
//! last, as Python's dict puts it; the index past a list's end appends), or a path alone to remove.

mod support;

use serde_json::Value;
use sheltersim_core::contracts;
use sheltersim_core::error::Error;
use sheltersim_core::host::Contract;
use sheltersim_core::value::V;

use support::load;

/// The contract `host_call` names (`call_on` of native/src/host.rs).
fn contract(name: &str) -> Contract {
    match name {
        "route_access" => Contract::RouteAccess,
        "equipment_transfer" => Contract::EquipmentTransfer,
        "move_facility" => Contract::MoveFacility,
        "partition" => Contract::Partition,
        "temporary_room" => Contract::TemporaryRoom,
        "guard" => Contract::Guard,
        "zone_environment" => Contract::ZoneEnvironment,
        "infrastructure" => Contract::Infrastructure,
        "population_spec" => Contract::PopulationSpec,
        "arrival_context" => Contract::ArrivalContext,
        "person_traits" => Contract::PersonTraits,
        "presence_command" => Contract::PresenceCommand,
        "facility_geometry" => Contract::FacilityGeometry,
        "spatial_constraints" => Contract::SpatialConstraints,
        operator => Contract::EffectParameters(operator.into()),
    }
}

/// `_guard_chain(levels)` of the builder.
fn chain(levels: u64) -> Value {
    let leaf = serde_json::json!({"operator": "eq", "field": "zone.occupancy", "value": 1});
    let mut node = leaf.clone();
    for _ in 0..levels {
        node = serde_json::json!({"operator": "and", "operands": [node, leaf.clone()]});
    }
    node
}

fn step<'a>(value: &'a mut V, key: &Value) -> &'a mut V {
    match (value, key) {
        (V::Map(map), Value::String(key)) => map.get_mut(key.as_str()).unwrap_or_else(|| panic!("no key {key}")),
        (V::List(items), Value::Number(index)) => &mut items[index.as_u64().unwrap() as usize],
        (_, key) => panic!("no step {key}"),
    }
}

fn patched(mut value: V, patch: &Value) -> V {
    for change in patch.as_array().unwrap() {
        let change = change.as_array().unwrap();
        let path = change[0].as_array().unwrap();
        let (last, parents) = path.split_last().unwrap();
        let mut parent = &mut value;
        for key in parents {
            parent = step(parent, key);
        }
        match (parent, last, change.get(1)) {
            (V::Map(map), Value::String(key), None) => {
                map.shift_remove(key.as_str());
            }
            (V::Map(map), Value::String(key), Some(item)) => {
                map.insert(key.as_str().into(), load::v(item));
            }
            (V::List(items), Value::Number(index), Some(item)) => {
                let index = index.as_u64().unwrap() as usize;
                if index == items.len() {
                    items.push(load::v(item));
                } else {
                    items[index] = load::v(item);
                }
            }
            (_, key, _) => panic!("no change at {key}"),
        }
    }
    value
}

fn input(case: &Value, bases: &Value) -> V {
    if let Some(levels) = case.get("chain") {
        return load::v(&chain(levels.as_u64().unwrap()));
    }
    match case.get("base") {
        Some(base) => patched(load::v(&bases[base.as_str().unwrap()]), &case["patch"]),
        None => load::v(&case["input"]),
    }
}

fn class(error: &Error) -> (&'static str, String) {
    match error {
        Error::Value(text) => ("Value", text.clone()),
        Error::Key(text) => ("Key", text.clone()),
        Error::Type(text) => ("Type", text.clone()),
        Error::Unsupported(text) => ("Unsupported", text.to_string()),
        other => ("other", format!("{other:?}")),
    }
}

/// Each case of the file: `validate` gives what Python gave.
fn answers_as_python(file: &str) {
    let data = load::data(file);
    assert_eq!(data["pydantic"], "2.11.9", "the fixture was recorded with another pydantic");
    let cases = data["cases"].as_array().unwrap();
    let mut differ = Vec::new();
    for (index, case) in cases.iter().enumerate() {
        let name = case["contract"].as_str().unwrap();
        let given = input(case, &data["bases"]);
        let got = contracts::validate(&contract(name), &given);
        let same = match (&got, case.get("dump")) {
            (Ok(dump), Some(expected)) => *dump == load::v(expected),
            (Err(error), None) => {
                let (class, text) = class(error);
                case["raises"][0] == class && case["raises"][1] == text.as_str()
            }
            _ => false,
        };
        if !same {
            let expected = case.get("dump").map_or_else(|| case["raises"][1].as_str().unwrap().to_owned(), |dump| format!("dump {dump}"));
            let got = match &got {
                Ok(dump) => format!("dump {dump:?}"),
                Err(error) => format!("{:?}", class(error)),
            };
            differ.push(format!("case {index} ({name} {}):\n  expected {expected}\n  got      {got}", case.get("patch").or(case.get("input")).map_or(String::new(), |value| value.to_string().chars().take(160).collect())));
        }
    }
    assert!(differ.is_empty(), "{} of {} cases of {file} differ:\n{}", differ.len(), cases.len(), differ.iter().take(15).cloned().collect::<Vec<_>>().join("\n"));
}

#[test]
fn the_guard_effects_and_presence_contracts_answer_as_python() {
    answers_as_python("contracts_effects");
}

#[test]
fn the_overlay_and_room_contracts_answer_as_python() {
    answers_as_python("contracts_overlay");
}

#[test]
fn the_geometry_contracts_answer_as_python() {
    answers_as_python("contracts_geometry");
}

#[test]
fn the_population_and_config_contracts_answer_as_python() {
    answers_as_python("contracts_population");
}

/// Each contract of `Contract` has cases (the files name them all).
#[test]
fn every_contract_has_cases() {
    let mut named: Vec<String> = Vec::new();
    for file in ["contracts_effects", "contracts_overlay", "contracts_geometry", "contracts_population"] {
        let data = load::data(file);
        for name in data["contracts"].as_array().unwrap() {
            let name = name.as_str().unwrap();
            assert!(data["cases"].as_array().unwrap().iter().any(|case| case["contract"] == name), "{file}: no case of {name}");
            named.push(name.to_owned());
        }
    }
    named.sort();
    let models: std::collections::BTreeSet<&str> =
        named.iter().map(|name| contract(name)).filter(|which| !matches!(which, Contract::EffectParameters(_))).map(|which| which.model().1).collect();
    assert_eq!(models.len(), 14, "{models:?}");
    assert_eq!(named.iter().filter(|name| matches!(contract(name), Contract::EffectParameters(_))).count(), 11);
}
