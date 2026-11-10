//! The core's errors give the texts the Python exceptions had, and `V` writes the bytes of
//! Python's canonical JSON: `json.dumps(value, sort_keys=True, separators=(",", ":"),
//! ensure_ascii=False, allow_nan=False).encode()` in CPython 3.12.

use std::sync::Arc;

use indexmap::IndexMap;
use sheltersim_core::error::{Error, Misfit, Outcome};
use sheltersim_core::json::Encoder;
use sheltersim_core::mathx::MathError;
use sheltersim_core::value::V;

fn map(entries: Vec<(&str, V)>) -> V {
    V::Map(entries.into_iter().map(|(key, value)| (Arc::from(key), value)).collect::<IndexMap<_, _>>())
}

#[test]
fn error_texts_are_the_exits_texts() {
    assert_eq!(
        Error::corrupt("body_state_v1", "a history is not a list").to_string(),
        "body_state_v1: corrupt state: a history is not a list",
    );
    assert_eq!(
        Error::refused("rules.tick.run_tick", Error::unsupported("value is not a plain JSON value")).to_string(),
        "rules.tick.run_tick refused: value is not a plain JSON value",
    );
    assert_eq!(Error::suppressed("agent is absent"), Error::Outcome(Outcome::Suppressed, "agent is absent".into()));
    assert_eq!(Error::failed("no such zone").to_string(), "no such zone");
    assert_eq!(Error::ledger("stock below zero").to_string(), "stock below zero");
    assert_eq!(Error::from(MathError::Range).to_string(), "math range error");
    assert_eq!(Misfit::new("not a list").0, "not a list");
}

#[test]
fn a_refusal_of_another_error_is_that_error() {
    let error = Error::value_error("minute must be an int");
    assert_eq!(Error::refused("rules.x.y", error.clone()), error);
}

#[test]
fn v_writes_pythons_canonical_json() {
    let value = map(vec![
        ("z", V::List(vec![
            V::Int(1),
            V::Int(-7),
            V::Int(0),
            V::Big("1180591620717411303424".into()),
            V::Big("-1180591620717411303424".into()),
        ])),
        ("a", map(vec![("b", V::Bool(true)), ("A", V::Bool(false)), ("", V::Null)])),
        ("é", V::Str(Arc::from("line\n\"quoted\" \\ tab\t\u{1} 日本"))),
        ("\u{1F600}", V::List(
            [0.1, 1e16, 1e-05, -0.0, 1.5e300, 123.456, 2.5e-07, 1e15].into_iter().map(V::Float).collect(),
        )),
        ("\u{ffff}", V::List(vec![])),
        ("m", map(vec![])),
    ]);
    let expected = "{\"a\":{\"\":null,\"A\":false,\"b\":true},\"m\":{},\
        \"z\":[1,-7,0,1180591620717411303424,-1180591620717411303424],\
        \"é\":\"line\\n\\\"quoted\\\" \\\\ tab\\t\\u0001 日本\",\"\u{ffff}\":[],\
        \"\u{1F600}\":[0.1,1e+16,1e-05,-0.0,1.5e+300,123.456,2.5e-07,1000000000000000.0]}";
    assert_eq!(String::from_utf8(value.to_json().unwrap()).unwrap(), expected);
}

#[test]
fn v_leaves_non_finite_floats_to_python() {
    for number in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut encoder = Encoder::default();
        let error = V::List(vec![V::Float(number)]).write_json(&mut encoder).unwrap_err();
        assert_eq!(error, Error::Unsupported("NaN and infinity are left to json".into()));
    }
}

#[test]
fn truthiness_is_pythons() {
    assert!(!V::Null.is_truthy());
    assert!(!V::Float(-0.0).is_truthy());
    assert!(!V::Str(Arc::from("")).is_truthy());
    assert!(V::Big("1180591620717411303424".into()).is_truthy());
    assert!(map(vec![("a", V::Null)]).is_truthy());
}
