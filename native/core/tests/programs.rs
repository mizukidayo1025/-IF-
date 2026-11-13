//! The EffectProgram of an intervention before it is scheduled, in the core (#304): the contract
//! (`sheltersim_core::program::EffectProgram`) reads each program of `tests/data/programs.json`
//! as `EffectProgram.model_validate` did, or refuses what it refused, and its dump has the sha256
//! of `program.model_dump(mode="json")`'s canonical JSON (the approval's hash); `program::validate`
//! on the same world at the same minute (`plain::PlainRun`, the browser's host) gives the issues
//! `validate_effect_program` gave, in the same order and texts.
//!
//! The cases are those of `tests/support/core_programs.py` (by `benchmarks/core_fixtures.py`):
//! the six operators the offline reading of a text makes, on `small_replay` and `generated` of
//! `tick_core.json` and on `operations`, a world with pending outages. A case marked `unsupported`
//! is one the browser does not check (another operator of the registry, or parameters whose issue
//! holds pydantic's text): the core refuses it.

use std::collections::BTreeMap;
use std::fs;

use serde_json::Value;
use sheltersim_core::checkpoint::sha256_hex;
use sheltersim_core::error::Error;
use sheltersim_core::plain::PlainRun;
use sheltersim_core::program::EffectProgram;
use sheltersim_core::world::WorldCell;

fn read(name: &str) -> Vec<u8> {
    fs::read(format!("{}/tests/data/{name}.json", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn run_to(run: &mut PlainRun, minute: i64) {
    while run.minute().unwrap() < minute {
        run.step().unwrap_or_else(|error| panic!("minute {}: {error:?}", run.minute().unwrap()));
    }
}

#[test]
fn the_core_reads_dumps_and_checks_each_program_as_python_did() {
    let fixture: Value = serde_json::from_slice(&read("programs")).unwrap();
    let mut files = BTreeMap::new();
    let mut runs: BTreeMap<String, PlainRun> = BTreeMap::new();
    let (mut refused, mut unsupported, mut checked, mut with_issues) = (0, 0, 0, 0);
    for case in fixture["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let world = case["world"].as_str().unwrap();
        let minute = case["minute"].as_i64().unwrap();
        let raw = serde_json::to_vec(&case["program"]).unwrap();
        let read = EffectProgram::of_json(&raw);
        if case["refused"].as_bool() == Some(true) {
            assert!(matches!(read, Err(Error::Value(_))), "{name}: {read:?}");
            refused += 1;
            continue;
        }
        let program = read.unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(sha256_hex(&program.dump_json().unwrap()), case["dump_sha256"].as_str().unwrap(), "{name}");
        let file = fixture["files"][world].as_str().unwrap();
        let bytes = files.entry(file.to_owned()).or_insert_with(|| self::read(file));
        let run = runs.entry(world.to_owned()).or_insert_with(|| PlainRun::of_worlds_json(bytes, world).unwrap());
        run_to(run, minute);
        let before = run.digests().unwrap();
        let issues = run.validate_effect_program(&program);
        assert_eq!(run.digests().unwrap(), before, "{name}: the check changed the world");
        if case["unsupported"].as_bool() == Some(true) {
            assert!(matches!(issues, Err(Error::Unsupported(_))), "{name}: {issues:?}");
            unsupported += 1;
            continue;
        }
        let expected: Vec<String> = case["issues"].as_array().unwrap().iter().map(|issue| issue.as_str().unwrap().to_owned()).collect();
        assert_eq!(issues.unwrap_or_else(|error| panic!("{name}: {error}")), expected, "{name}");
        checked += 1;
        with_issues += usize::from(!expected.is_empty());
    }
    assert_eq!((refused, unsupported), (10, 2));
    assert!(checked >= 35 && with_issues >= 25, "{checked} {with_issues}");
}

/// The world's digests by part, as `core_tick.world_digests` writes them.
fn digests(run: &PlainRun) -> Value {
    let digests = run.loaded.cell.with(|world| sheltersim_core::canonical::run_digests(world)).unwrap().unwrap();
    Value::Object(digests.into_iter().map(|(part, digest)| (part.to_owned(), Value::from(digest))).collect())
}

/// A program is scheduled on the world as `BranchRuntime.apply_effect_program` scheduled it (#305):
/// the same `command_log` entry, the same world after it and at the end of the Run; a program
/// with issues is refused with Python's text, the world as it was.
#[test]
fn the_core_schedules_each_program_as_python_did() {
    let fixture: Value = serde_json::from_slice(&read("programs")).unwrap();
    let (mut scheduled, mut refused) = (0, 0);
    for case in fixture["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let issues = match case["issues"].as_array() {
            Some(issues) if case["unsupported"].as_bool() != Some(true) => issues,
            _ => continue,
        };
        let world = case["world"].as_str().unwrap();
        let program = EffectProgram::of_json(&serde_json::to_vec(&case["program"]).unwrap()).unwrap();
        let mut run = PlainRun::of_worlds_json(&read(fixture["files"][world].as_str().unwrap()), world).unwrap();
        run_to(&mut run, case["minute"].as_i64().unwrap());
        let before = digests(&run);
        let applied = run.apply_effect_program(&program);
        if !issues.is_empty() {
            let texts: Vec<&str> = issues.iter().map(|issue| issue.as_str().unwrap()).collect();
            let expected = format!("EffectProgram could not be scheduled: {}", texts.join("; "));
            assert!(matches!(&applied, Err(Error::Value(text)) if *text == expected), "{name}: {applied:?}");
            assert_eq!(digests(&run), before, "{name}: a refused program changed the world");
            refused += 1;
            continue;
        }
        let expected = &case["scheduled"];
        let receipt = applied.unwrap_or_else(|error| panic!("{name}: {error}"));
        let command: Value = serde_json::from_slice(&receipt.command().to_json().unwrap()).unwrap();
        assert_eq!(command, expected["command"], "{name}");
        assert_eq!(digests(&run), expected["digests"], "{name}: after scheduling");
        let end = run.duration().unwrap();
        run_to(&mut run, end);
        assert_eq!(digests(&run), expected["end_digests"], "{name}: at the end");
        scheduled += 1;
    }
    assert!(scheduled >= 10 && refused >= 20, "{scheduled} {refused}");
}
