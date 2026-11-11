//! The core's text operations against CPython's (P8-0a): `str.casefold` over every code point,
//! against a fixture written by CPython 3.12 (`native/core/tools/casefold_table.py`), and
//! `format(value, "g")`; and `repr` of a str over every code point, against a fixture written by
//! CPython 3.12 (`native/core/tools/repr_table.py`, P8 c3).

use std::borrow::Cow;
use std::collections::HashMap;

use sha2::{Digest, Sha256};
use sheltersim_core::error::Error;
use sheltersim_core::text::{casefold, fold_char, g_text, py_repr};

const FIXTURE: &str = include_str!("data/casefold.txt");

struct Fixture {
    digest: String,
    count: usize,
    folds: HashMap<u32, String>,
}

fn fixture() -> Fixture {
    let mut digest = String::new();
    let mut count = 0;
    let mut folds = HashMap::new();
    for line in FIXTURE.lines() {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if let Some(value) = line.strip_prefix("sha256 ") {
            digest = value.to_owned();
            continue;
        }
        if let Some(value) = line.strip_prefix("count ") {
            count = value.parse().unwrap();
            continue;
        }
        let mut points = line.split(' ').map(|part| u32::from_str_radix(part, 16).unwrap());
        let from = points.next().unwrap();
        let folded: String = points.map(|point| char::from_u32(point).unwrap()).collect();
        assert!(folds.insert(from, folded).is_none(), "{from:x} twice in the fixture");
    }
    assert_eq!(folds.len(), count);
    Fixture { digest, count, folds }
}

#[test]
fn casefold_is_cpython_s_at_every_code_point() {
    let fixture = fixture();
    let mut digest = Sha256::new();
    let mut folded_count = 0;
    let mut buffer = [0u8; 4];
    for point in (0..=0x10FFFFu32).filter(|point| !(0xD800..0xE000).contains(point)) {
        let char = char::from_u32(point).unwrap();
        let text: &str = char.encode_utf8(&mut buffer);
        let folded = casefold(text);
        match fixture.folds.get(&point) {
            Some(expected) => {
                folded_count += 1;
                assert_eq!(&*folded, expected, "the fold of U+{point:04X}");
                assert_eq!(fold_char(char), Some(expected.as_str()), "fold_char of U+{point:04X}");
                assert!(matches!(folded, Cow::Owned(_)));
            }
            None => {
                assert_eq!(&*folded, text, "U+{point:04X} folds to itself");
                assert_eq!(fold_char(char), None, "fold_char of U+{point:04X}");
            }
        }
        digest.update(folded.as_bytes());
        digest.update([0u8]);
    }
    assert_eq!(folded_count, fixture.count);
    let hex: String = digest.finalize().iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(hex, fixture.digest, "the folds of all code points");
}

#[test]
fn casefold_of_text() {
    // As CPython 3.12 folds them.
    for (text, folded) in [
        ("ﬆoilet", "stoilet"),
        ("TOİLET", "toi\u{307}let"),
        ("Straße", "strasse"),
        ("ΣΑΣ", "σασ"),
        ("ǅ", "ǆ"),
        ("ﬃ", "ffi"),
        ("Ⓣoilet", "ⓣoilet"),
        ("𐐀", "𐐨"),
        ("ꭰ", "Ꭰ"),
        ("ᏸ", "Ᏸ"),
        ("A-TOILET-Room", "a-toilet-room"),
        ("水場 Water", "水場 water"),
    ] {
        assert_eq!(casefold(text), folded, "{text}");
    }
    // Nothing to fold: the text itself, not a copy.
    for text in ["toilet", "", "避難所", "ⓣoilet"] {
        assert!(matches!(casefold(text), Cow::Borrowed(same) if same == text), "{text}");
    }
}

#[test]
fn g_text_is_format_g() {
    // `format(value, "g")` of CPython 3.12.
    for (value, text) in [
        (0.5, "0.5"),
        (0.1, "0.1"),
        (1e-05, "1e-05"),
        (123456.0, "123456"),
        (1234567.0, "1.23457e+06"),
        (0.0001, "0.0001"),
        (0.00012345678, "0.000123457"),
        (1e16, "1e+16"),
        (-0.0, "-0"),
        (0.0, "0"),
        (2.5e-05, "2.5e-05"),
        (100.0, "100"),
        (0.30000000000000004, "0.3"),
        (1.0 / 3.0, "0.333333"),
        (-7.25, "-7.25"),
        (999999.5, "1e+06"),
        (9999995.0, "1e+07"),
        (9.9999995e-05, "0.0001"),
        (1e-300, "1e-300"),
        (f64::MAX, "1.79769e+308"),
        (5e-324, "4.94066e-324"),
        (0.7, "0.7"),
        (0.65, "0.65"),
        (12.0, "12"),
    ] {
        assert_eq!(g_text(value).unwrap(), text, "{value:?}");
    }
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(g_text(value), Err(Error::Unsupported("a threshold is not finite".into())));
    }
}

const REPR_FIXTURE: &str = include_str!("data/repr.txt");

fn repr_fixture(name: &str) -> String {
    let prefix = format!("{name} ");
    REPR_FIXTURE.lines().find_map(|line| line.strip_prefix(&prefix)).unwrap().to_owned()
}

fn hex(digest: Sha256) -> String {
    digest.finalize().iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn repr_is_cpython_s_at_every_code_point() {
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 4];
    for point in (0..=0x10FFFFu32).filter(|point| !(0xD800..0xE000).contains(point)) {
        let text: &str = char::from_u32(point).unwrap().encode_utf8(&mut buffer);
        digest.update(py_repr(text).as_bytes());
        digest.update([0u8]);
    }
    assert_eq!(hex(digest), repr_fixture("sha256"), "the reprs of all code points");
}

#[test]
fn repr_of_text() {
    // The texts of `native/core/tools/repr_table.py`, in its order.
    let texts = ["", "a'b", "a\"b", "a'\"b", "\\", "tab\there\nnew\rline", "\x00\x7f\u{80}\u{ff}", "避難所", "\u{2028}\u{1f600}"];
    let mut digest = Sha256::new();
    for text in texts {
        digest.update(py_repr(text).as_bytes());
        digest.update([0u8]);
    }
    assert_eq!(hex(digest), repr_fixture("texts"));
    assert_eq!(py_repr("agent-1"), "'agent-1'");
    assert_eq!(py_repr("it's"), "\"it's\"");
    assert_eq!(py_repr("a'\"b"), "'a\\'\"b'");
    assert_eq!(py_repr("避難所\u{a0}"), "'避難所\\xa0'");
}
