//! The synthetic population in the core (Issue #297) is Python's: for each count and seed, the
//! canonical JSON of the whole `GeneratedPopulation` dump and of its parts has the sha256 Python's
//! has (`tests/support/core_scenario_population.py`, `data/scenario_population.json`).

use serde_json::Value;
use sheltersim_core::checkpoint::sha256_hex;
use sheltersim_core::scenario::population::{
    default_household_composition, generate_population, AgeBand, HouseholdSizeMix, PopulationInput,
};
use sheltersim_core::value::V;

fn fixture() -> Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/scenario_population.json");
    serde_json::from_slice(&std::fs::read(path).expect("the fixture")).expect("its JSON")
}

fn strings(value: &Value) -> Vec<String> {
    value.as_array().unwrap().iter().map(|item| item.as_str().unwrap().to_string()).collect()
}

fn input(case: &Value) -> PopulationInput {
    let mut input = PopulationInput::for_request(case["count"].as_i64().unwrap(), case["seed"].as_i64().unwrap());
    if let Some(bands) = case["age_bands"].as_array() {
        input.age_bands = bands
            .iter()
            .map(|band| AgeBand {
                label: band["label"].as_str().unwrap().into(),
                age_min: band["age_min"].as_i64().unwrap(),
                age_max: band["age_max"].as_i64().unwrap(),
                population_count: band["population_count"].as_i64().unwrap(),
                source_ids: strings(&band["source_ids"]),
            })
            .collect();
    }
    if let Some(mix) = case["household_mix"].as_object() {
        input.household_mix = Some(HouseholdSizeMix {
            size_weights: mix["size_weights"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(size, weight)| (size.parse().unwrap(), weight.as_f64().unwrap()))
                .collect(),
            source_ids: strings(&mix["source_ids"]),
        });
    }
    input.household_composition = case["composition"].as_str().map(|_| default_household_composition());
    input
}

fn digest(value: &V) -> String {
    sha256_hex(&value.to_json().unwrap())
}

#[test]
fn each_population_is_python_s() {
    let document = fixture();
    let mut wrong = Vec::new();
    for case in document["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let generated = generate_population(&input(case)).unwrap_or_else(|error| panic!("{name}: {error}"));
        let dump = generated.dump();
        let V::Map(parts) = &dump else { unreachable!() };
        for (part, expected) in case["parts"].as_object().unwrap() {
            let got = digest(&parts[part.as_str()]);
            if got != expected.as_str().unwrap() {
                wrong.push(format!("{name}: {part}"));
            }
        }
        if digest(&dump) != case["sha256"].as_str().unwrap() {
            wrong.push(format!("{name}: the whole dump"));
        }
        let minors = generated.people.iter().filter(|(_, traits)| traits.age_years < 18).count();
        assert_eq!(minors as i64, case["minors"].as_i64().unwrap(), "{name}");
    }
    assert!(wrong.is_empty(), "{} parts differ:\n{}", wrong.len(), wrong.join("\n"));
}

#[test]
fn the_builder_s_population_has_one_profile_per_person_in_household_order() {
    let generated = generate_population(&PopulationInput::for_request(100, 2026)).unwrap();
    assert_eq!(generated.profile_set.profiles.len(), 100);
    assert_eq!(generated.person_requirements.len(), 100);
    assert_eq!(generated.household_sizes.iter().map(|(_, size)| size).sum::<i64>(), 100);
    for (profile, (id, traits)) in generated.profile_set.profiles.iter().zip(&generated.people) {
        assert_eq!(&profile.id, id);
        assert_eq!(profile.group_id.as_deref(), Some(traits.household_id.as_str()));
    }
    assert!(generate_population(&PopulationInput::for_request(0, 2026)).is_err());
    assert!(generate_population(&PopulationInput::for_request(10_001, 2026)).is_err());
}
