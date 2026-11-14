//! The cases of the whole world's validation (`sheltersim_core::validate`, #250, #347): broken
//! worlds and what Python raised for each at each entry point, which the core must raise too.
//!
//! `tests/support/core_validate.py` writes the files: `validate_worlds.json` holds the valid worlds,
//! and each case file (`validate_<area>.json`) holds its cases' worlds as variants of those
//! (`{"file": "validate_worlds", "base": name, "changes"/"set": ...}`, `cases::apply_variant`) and
//! its cases: `{"name", "world", "in_flight"?, "expected": {entry: [class, text] | null}}`. A world
//! is loaded with the namespaces as found (`load::Namespaces::Lenient`), so a namespace its typed
//! row would refuse is there as a value, as Python holds it.
//!
//! `check_file` checks every case of a file and fails with all the cases that differ; a test that
//! has to change the loaded world itself (a value the loader cannot hold) uses `world_of`,
//! `outcomes` and `expected_of`.

use std::collections::{BTreeMap, HashMap};

use serde_json::Value;
use sheltersim_core::error::{Error, Outcome};
use sheltersim_core::events::EventRow;
use sheltersim_core::load::{self as core_load, Namespaces};
use sheltersim_core::validate::{self, py_str, Options};
use sheltersim_core::world::World;

use super::{cases, load};

/// The entry points a case records, as `ENTRIES` of `core_validate.py` names them.
pub const ENTRIES: [&str; 5] = ["validate", "validate_tick", "extensions", "candidate", "in_flight"];

/// What an entry point gave: None where it passed, or the Python class and `str(exc)`.
pub type Found = Option<(String, String)>;

/// The Python class of a core error.
pub fn class_of(error: &Error) -> String {
    match error {
        Error::Invariant(_) | Error::Corrupt { .. } => "StateInvariantError",
        Error::Value(_) | Error::MarkedValue(_) => "ValueError",
        Error::Validation(_) => "ValidationError",
        Error::Key(_) => "KeyError",
        Error::Type(_) => "TypeError",
        Error::Ledger(_) => "ResourceLedgerError",
        Error::Runtime(_) => "RuntimeError",
        Error::Outcome(Outcome::Failed, _) => "EventApplicationError",
        Error::Outcome(Outcome::Suppressed, _) => "EventSuppressed",
        Error::Refused { .. } => "KernelRefusal",
        Error::Unsupported(_) => "Unsupported",
        Error::Clock => "TickExecutionError",
        Error::Math(error) => return format!("{error:?}"),
    }
    .to_owned()
}

/// What a result is as a case records it.
pub fn outcome_of(result: Result<(), Error>) -> Found {
    result.err().map(|error| (class_of(&error), py_str(&error)))
}

/// What each entry point gives for a world (with an event in flight for `in_flight`).
pub fn outcomes(world: &World, in_flight: Option<&EventRow>) -> BTreeMap<&'static str, Found> {
    let mut out = BTreeMap::new();
    out.insert("validate", outcome_of(validate::world(world, &Options::default())));
    out.insert("validate_tick", outcome_of(validate::world(world, &Options { history: false, in_flight: None })));
    out.insert("extensions", outcome_of(validate::extensions(world, true)));
    out.insert("candidate", outcome_of(validate::candidate(world)));
    if let Some(event) = in_flight {
        out.insert("in_flight", outcome_of(validate::world(world, &Options { history: true, in_flight: Some(event) })));
    }
    out
}

/// The files of cases and worlds a check reads, each read once.
#[derive(Default)]
pub struct Files(HashMap<String, Value>);

impl Files {
    /// A file of `tests/data/` (`load::data`).
    pub fn data(&mut self, file: &str) -> &Value {
        self.0.entry(file.to_owned()).or_insert_with(|| load::data(file))
    }

    /// The entry of a world of a file, its variant made again (from a world of the same file, or
    /// of the file it names).
    pub fn entry(&mut self, file: &str, name: &str) -> Value {
        let world = self.data(file)["worlds"].get(name).cloned().unwrap_or_else(|| panic!("{file} has no world {name}"));
        let Some(base) = world.get("base").and_then(Value::as_str) else {
            return world;
        };
        let base_file = world.get("file").and_then(Value::as_str).unwrap_or(file).to_owned();
        let mut entry = self.entry(&base_file, base);
        cases::apply_variant(&mut entry, &world);
        entry
    }
}

/// The World of an entry, its namespaces as found.
pub fn load_entry(entry: &Value) -> Result<World, Error> {
    core_load::world_with(&entry["world"], &entry["slots"], Namespaces::Lenient)
}

/// The World of a case of a file.
pub fn world_of(file: &str, case: &str) -> World {
    let mut files = Files::default();
    let world = case_of(files.data(file), case)["world"].as_str().unwrap().to_owned();
    let entry = files.entry(file, &world);
    load_entry(&entry).unwrap_or_else(|error| panic!("{case}: the loader refused the world: {error}"))
}

/// What a case of a file expects at each entry point.
pub fn expected_of(file: &str, case: &str) -> BTreeMap<String, Found> {
    expected(case_of(&load::data(file), case))
}

fn case_of<'a>(data: &'a Value, name: &str) -> &'a Value {
    data["cases"].as_array().unwrap().iter().find(|case| case["name"] == name).unwrap_or_else(|| panic!("no case {name}"))
}

fn expected(case: &Value) -> BTreeMap<String, Found> {
    case["expected"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(entry, value)| {
            let outcome = value.as_array().map(|pair| (pair[0].as_str().unwrap().to_owned(), pair[1].as_str().unwrap().to_owned()));
            (entry.clone(), outcome)
        })
        .collect()
}

/// The differences between what a case expects and what the core gave, one line each.
pub fn differences(name: &str, expected: &BTreeMap<String, Found>, got: &BTreeMap<&'static str, Found>) -> Vec<String> {
    let mut out = Vec::new();
    for (entry, wanted) in expected {
        let found = got.get(entry.as_str()).unwrap_or_else(|| panic!("{name}: no entry point {entry}"));
        if !same_outcome(wanted, found) {
            out.push(format!("{name} {entry}: Python {wanted:?}, the core {found:?}"));
        }
    }
    out
}

/// Whether the core gave what Python raised. pydantic's `ValidationError` (a `ValueError`) is
/// `Error::Validation` where a part gives that class (#350); a part that still gives the core's
/// `ValueError` with the same text passes too, until it does (#360).
pub fn same_outcome(wanted: &Found, found: &Found) -> bool {
    match (wanted, found) {
        (Some((class, text)), Some((got_class, got_text))) if class == "ValidationError" => {
            (got_class == "ValidationError" || got_class == "ValueError") && got_text == text
        }
        _ => wanted == found,
    }
}

/// Check every case of a file (`validate_<area>.json`); fails with all the cases that differ. The
/// number of cases.
pub fn check_file(file: &str) -> usize {
    check_file_without(file, &[])
}

/// `check_file`, but not at these entry points: for a file whose cases another lane's part answers
/// there too (a stub until it lands).
pub fn check_file_without(file: &str, skipped: &[&str]) -> usize {
    let mut files = Files::default();
    let cases = files.data(file)["cases"].as_array().cloned().unwrap_or_else(|| panic!("{file} has no cases"));
    let mut failures = Vec::new();
    for case in &cases {
        let name = case["name"].as_str().unwrap();
        let entry = files.entry(file, case["world"].as_str().unwrap());
        let world = match load_entry(&entry) {
            Ok(world) => world,
            Err(error) => {
                failures.push(format!("{name}: the loader refused the world: {error}"));
                continue;
            }
        };
        let in_flight = case.get("in_flight").map(load::event);
        let mut wanted = expected(case);
        wanted.retain(|entry, _| !skipped.contains(&entry.as_str()));
        failures.extend(differences(name, &wanted, &outcomes(&world, in_flight.as_ref())));
    }
    assert!(failures.is_empty(), "{file}: {} of {} cases differ:\n{}", failures.len(), cases.len(), failures.join("\n"));
    cases.len()
}

/// Check that each valid world of a file of worlds passes every entry point, as it does in Python.
pub fn check_valid_worlds(file: &str) -> usize {
    let data = load::data(file);
    let worlds = data["worlds"].as_object().unwrap();
    for (name, entry) in worlds {
        let world = load_entry(entry).unwrap_or_else(|error| panic!("{name}: {error}"));
        let got = outcomes(&world, None);
        let failures = differences(name, &expected(entry), &got);
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
    worlds.len()
}
