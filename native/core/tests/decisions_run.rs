//! The decisions a Run records, in the core (Issue #225, lane M11; moved from
//! `tests/integration/test_decisions_row.py`).
//!
//! The world is the generated Scenario the Project screen makes, at minute 0 with the Run's
//! policies (`generated_texts.json`'s `generated`). The core runs its first 90 ticks as
//! `TickEngine.run(steps=90)` does (`support/ticks.rs`), with a host whose sink is an indexed one
//! (`_IndexedMetricSink`: each decision's record as JSON bytes). The policy of this Run is not in
//! the model catalog, so the sink keeps no source for it (`source_id` null), as the host answers.
//!
//! The digests are the pytest file's: what the Python steps `_propose_due`, `_observed_state` and
//! `_record_outcome` recorded of the same Run on 45a9bc9 (model version 2 re-recorded some digests
//! of that file from the kernels): the first 16 hex of the sha256 of `json.dumps(value,
//! sort_keys=True)`, of `_exact` of each record (each dict as its pairs in order, the types' names,
//! the floats' `float.hex`) and of the world's `canonical_json()`. The core writes the same texts
//! (`Dumps`, `support/canonical.rs`, with the `rng_state` the engine gives the world it steps), so
//! the digests are the original ones.

mod support;

use std::rc::Rc;
use std::sync::Arc;

use sheltersim_core::host::Decision;
use sheltersim_core::run::sinks::DecisionSink;
use sheltersim_core::value::V;
use sheltersim_core::world::WorldCell;

use support::canonical::canonical_json;
use support::dumps::Dumps;
use support::load;
use support::texts::sha;
use support::{cases, ticks, Call};

/// `_python_s` of the records and of the world in `test_a_run_gives_the_rows_and_world_of_python`.
const ROWS: &str = "33be50e2458c9bf3";
const WORLD: &str = "9a356ae3bed7843c";

/// `float.hex()`.
fn float_hex(value: f64) -> String {
    let bits = value.to_bits();
    let sign = if bits >> 63 == 1 { "-" } else { "" };
    let exponent = ((bits >> 52) & 0x7ff) as i64;
    let mantissa = bits & ((1 << 52) - 1);
    match exponent {
        0 if mantissa == 0 => format!("{sign}0x0.0p+0"),
        0 => format!("{sign}0x0.{mantissa:013x}p-1022"),
        _ => format!("{sign}0x1.{mantissa:013x}p{:+}", exponent - 1023),
    }
}

/// `_exact` of `test_decisions_row.py` of a value `json.loads` read.
fn exact(value: &V) -> V {
    let named = |name: &str, value: V| V::List(vec![V::Str(Arc::from(name)), value]);
    match value {
        V::Float(number) => V::Str(Arc::from(float_hex(*number))),
        V::List(items) => V::List(items.iter().map(exact).collect()),
        V::Map(map) => V::List(map.iter().map(|(key, item)| V::List(vec![V::Str(key.clone()), exact(item)])).collect()),
        V::Int(_) => named("int", value.clone()),
        V::Str(_) => named("str", value.clone()),
        V::Bool(_) => named("bool", value.clone()),
        V::Null => named("NoneType", value.clone()),
        other => panic!("not a value of JSON: {other:?}"),
    }
}

/// `json.loads` of a record's bytes, each object's keys in their order (serde_json's `Value` sorts
/// them): a number with a point or an exponent is a float, any other an int.
fn loads(raw: &[u8]) -> V {
    let mut at = 0;
    let value = parse(raw, &mut at);
    assert_eq!(at, raw.len(), "bytes after the value");
    value
}

fn parse(raw: &[u8], at: &mut usize) -> V {
    let literal = |at: &mut usize, word: &str, value: V| {
        assert!(raw[*at..].starts_with(word.as_bytes()), "not JSON at {at}");
        *at += word.len();
        value
    };
    match raw[*at] {
        b'{' => {
            *at += 1;
            let mut map = indexmap::IndexMap::new();
            while raw[*at] != b'}' {
                if !map.is_empty() {
                    assert_eq!(raw[*at], b',');
                    *at += 1;
                }
                let V::Str(key) = parse(raw, at) else { panic!("a key that is not a str") };
                assert_eq!(raw[*at], b':');
                *at += 1;
                let value = parse(raw, at);
                map.insert(key, value);
            }
            *at += 1;
            V::Map(map)
        }
        b'[' => {
            *at += 1;
            let mut items = Vec::new();
            while raw[*at] != b']' {
                if !items.is_empty() {
                    assert_eq!(raw[*at], b',');
                    *at += 1;
                }
                items.push(parse(raw, at));
            }
            *at += 1;
            V::List(items)
        }
        b'"' => {
            let start = *at;
            *at += 1;
            while raw[*at] != b'"' {
                *at += if raw[*at] == b'\\' { 2 } else { 1 };
            }
            *at += 1;
            V::Str(Arc::from(serde_json::from_slice::<String>(&raw[start..*at]).unwrap()))
        }
        b't' => literal(at, "true", V::Bool(true)),
        b'f' => literal(at, "false", V::Bool(false)),
        b'n' => literal(at, "null", V::Null),
        _ => {
            let start = *at;
            while *at < raw.len() && matches!(raw[*at], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
                *at += 1;
            }
            let number = std::str::from_utf8(&raw[start..*at]).unwrap();
            if number.contains(['.', 'e', 'E']) {
                V::Float(number.parse().unwrap())
            } else {
                V::Int(number.parse().unwrap())
            }
        }
    }
}

/// `_digest`: the first 16 hex of the sha256 of `json.dumps(value, sort_keys=True)`.
fn digest(value: &V) -> String {
    sha(&Dumps::default().sorted().text(value))[..16].to_owned()
}

#[test]
fn a_run_gives_the_records_and_the_world_of_python() {
    let entry = cases::entry("generated_texts", "generated");
    let loaded = Rc::new(load::loaded(&entry));
    let mut host = load::following_host(&entry, &loaded);
    host.sink = DecisionSink::Indexed;
    ticks::run(&loaded, &mut host, 90, &mut |_, _| {});
    let records: Vec<V> = host
        .calls
        .iter()
        .filter_map(|call| match call {
            Call::Decision(Decision::Indexed(raw)) => Some(exact(&loads(raw))),
            Call::Decision(other) => panic!("not an indexed record: {other:?}"),
            _ => None,
        })
        .collect();
    assert!(!records.is_empty());
    assert_eq!(digest(&V::List(records)), ROWS);
    // The `rng_state` the engine gives the world it steps: its `CounterRNG`'s state.
    let rng_state = serde_json::json!({"algorithm": "sha256-keyed-v1", "seed": entry["world"]["seed"]});
    let canonical = loaded.cell.with(|world| canonical_json(world, &rng_state)).unwrap();
    assert_eq!(digest(&V::Str(Arc::from(String::from_utf8(canonical).unwrap()))), WORLD);
}
