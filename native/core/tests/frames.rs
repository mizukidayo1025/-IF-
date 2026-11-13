//! The frame of a minute in the core (#264): `sheltersim_core::frame` on a Run without Python
//! (`plain::PlainRun`, the browser's host) gives, byte for byte, the frame the native path's
//! `world_frame` gave on the same world at the same minute, and the world's digest
//! (`canonical::state_digest`) is that frame's `state_hash` (`world.state_digest()`).
//!
//! The worlds and minutes are those of `tests/support/core_frames.py`, which wrote
//! `tests/data/frames.json` (by `benchmarks/core_fixtures.py`): the two worlds of `tick_core.json`
//! (one without a floor plan, whose people are at their zones' display points, and one whose
//! facilities an EffectProgram moves) and the world `arrivals` (people walking in on an approach
//! route and waiting at the door, open care requests, a log longer than the frame's scan of
//! interactions). Each is run from minute 0 in the core, and its frame read at each minute.

use std::fs;

use serde_json::Value;
use sheltersim_core::checkpoint::sha256_hex;
use sheltersim_core::frame::Request;
use sheltersim_core::plain::PlainRun;

fn read(name: &str) -> Vec<u8> {
    fs::read(format!("{}/tests/data/{name}.json", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// What `frames.json` expects: each world, the file it is in, and its minutes with the frame's
/// sha256 and the world's digest there.
fn expected() -> Vec<(String, String, Vec<(i64, String, String)>)> {
    let fixture: Value = serde_json::from_slice(&read("frames")).unwrap();
    let mut worlds = Vec::new();
    for (name, frames) in fixture["expected"].as_object().unwrap() {
        let file = fixture["files"][name].as_str().unwrap().to_owned();
        let frames = frames
            .as_array()
            .unwrap()
            .iter()
            .map(|frame| (frame[0].as_i64().unwrap(), frame[1].as_str().unwrap().to_owned(), frame[2].as_str().unwrap().to_owned()))
            .collect();
        worlds.push((name.clone(), file, frames));
    }
    worlds
}

fn request(minute: i64, state_hash: Option<&str>) -> Request<'_> {
    Request { run_id: "run", branch_id: "main", computed_until: minute, state_hash }
}

/// The world run in the core to the minute.
fn run_to(run: &mut PlainRun, minute: i64) {
    while run.minute().unwrap() < minute {
        run.step().unwrap_or_else(|error| panic!("minute {}: {error:?}", run.minute().unwrap()));
    }
}

#[test]
fn the_core_s_frame_is_the_native_path_s_at_each_minute() {
    for (name, file, frames) in expected() {
        let mut run = PlainRun::of_worlds_json(&read(&file), &name).unwrap();
        for (minute, frame, state_hash) in frames {
            run_to(&mut run, minute);
            let made = run.frame(&request(minute, None)).unwrap();
            assert_eq!(sha256_hex(&made), frame, "{name} at {minute}");
            assert_eq!(run.state_digest().unwrap(), state_hash, "{name} at {minute}");
        }
    }
}

#[test]
fn a_frame_names_the_digest_its_caller_holds_and_changes_nothing() {
    let mut run = PlainRun::of_worlds_json(&read("tick_core"), "generated").unwrap();
    run_to(&mut run, 40);
    let before = run.digests().unwrap();
    let digest = run.state_digest().unwrap();
    let given = "0".repeat(64);
    let made: Value = serde_json::from_slice(&run.frame(&request(40, Some(&given))).unwrap()).unwrap();
    assert_eq!(made["state_hash"], given.as_str());
    assert_eq!((&made["run_id"], &made["branch_id"], &made["minute"], &made["computed_until"]), (&Value::from("run"), &Value::from("main"), &Value::from(40), &Value::from(40)));
    let again: Value = serde_json::from_slice(&run.frame(&request(40, None)).unwrap()).unwrap();
    assert_eq!(again["state_hash"], digest.as_str());
    // Apart from the digest, the two frames are the same.
    let (mut first, mut second) = (made, again);
    first["state_hash"] = Value::Null;
    second["state_hash"] = Value::Null;
    assert_eq!(first, second);
    // A frame is a read: the world is as it was, and its ticks go on as before.
    assert_eq!(run.digests().unwrap(), before);
    run_to(&mut run, 41);
}
