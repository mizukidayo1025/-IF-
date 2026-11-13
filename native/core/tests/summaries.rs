//! The summary of a world for the intervention's reading in the core (#302): `sheltersim_core::summary`
//! on a Run without Python (`plain::PlainRun`, the browser's host) gives, byte for byte, the
//! canonical JSON of the summary Python's `scenario_summary_from_world` made of the same world at
//! the same minute.
//!
//! The worlds and minutes are those of `tests/support/core_summaries.py`, which wrote
//! `tests/data/summaries.json` (by `benchmarks/core_fixtures.py`): the two worlds of
//! `tick_core.json`, a world with a staff member away, a pending shipment and people who come, go
//! and step out (`operations`), and a world of one zone (`one_zone`). Each is run from minute 0 in
//! the core, and its summary read at each minute.

use std::fs;

use serde_json::Value;
use sheltersim_core::checkpoint::sha256_hex;
use sheltersim_core::plain::PlainRun;

fn read(name: &str) -> Vec<u8> {
    fs::read(format!("{}/tests/data/{name}.json", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// The world run in the core to the minute.
fn run_to(run: &mut PlainRun, minute: i64) {
    while run.minute().unwrap() < minute {
        run.step().unwrap_or_else(|error| panic!("minute {}: {error:?}", run.minute().unwrap()));
    }
}

#[test]
fn the_core_s_summary_is_python_s_at_each_minute() {
    let fixture: Value = serde_json::from_slice(&read("summaries")).unwrap();
    for (name, summaries) in fixture["expected"].as_object().unwrap() {
        let file = fixture["files"][name].as_str().unwrap();
        let mut run = PlainRun::of_worlds_json(&read(file), name).unwrap();
        for item in summaries.as_array().unwrap() {
            let minute = item[0].as_i64().unwrap();
            run_to(&mut run, minute);
            let made = run.scenario_summary().unwrap();
            if name == fixture["full"]["world"].as_str().unwrap() && minute == fixture["full"]["minute"].as_i64().unwrap() {
                let made: Value = serde_json::from_slice(&made).unwrap();
                assert_eq!(made, fixture["full"]["summary"], "{name} at {minute}");
            }
            assert_eq!(sha256_hex(&made), item[1].as_str().unwrap(), "{name} at {minute}");
        }
    }
}

#[test]
fn a_summary_changes_nothing() {
    let mut run = PlainRun::of_worlds_json(&read("summaries"), "operations").unwrap();
    run_to(&mut run, 12);
    let before = run.digests().unwrap();
    let first = run.scenario_summary().unwrap();
    assert_eq!(run.scenario_summary().unwrap(), first);
    assert_eq!(run.digests().unwrap(), before);
}
