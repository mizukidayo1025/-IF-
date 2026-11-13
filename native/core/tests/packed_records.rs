//! The binary form of a realized record (`events::packed`, #252 PR 6-2): a record packed and
//! unpacked is the same row and writes the same canonical JSON, byte for byte, for every record a
//! Run of the core writes and every record the fixtures hold; the values `json.dumps` tells apart
//! stay apart; and bytes that are not a record are an error, never a panic, since the browser reads
//! them back from its storage.

mod support;

use std::sync::Arc;

use indexmap::IndexMap;
use serde_json::Value;
use sheltersim_core::events::packed::{Packer, Unpacked};
use sheltersim_core::events::{EventKind, EventRow, EventStatus};
use sheltersim_core::plain::PlainRun;
use sheltersim_core::value::V;
use sheltersim_core::world::WorldCell;
use support::load;

/// Packs each record's row into one buffer, one after another, then unpacks them in turn: each is
/// the record it was, and writes the bytes it was read from.
fn round_trip(records: &[Vec<u8>]) -> (Packer, usize) {
    let mut packer = Packer::new();
    let mut packed = Vec::new();
    let mut rows = Vec::with_capacity(records.len());
    for raw in records {
        let (event, realized_minute, status, detail) = EventRow::from_saved_record(raw).expect("a record");
        let unpacked = Unpacked { event, realized_minute, status, detail };
        assert_eq!(&*unpacked.record_json().unwrap(), &raw[..], "the record is canonical JSON already");
        packer.pack(&mut packed, &unpacked.event, unpacked.realized_minute, unpacked.status, unpacked.detail.as_deref()).unwrap();
        rows.push(unpacked);
    }
    let mut at = 0;
    for (row, raw) in rows.iter().zip(records) {
        let (again, used) = packer.unpack(&packed[at..]).unwrap();
        assert_eq!(&again, row);
        assert_eq!(&*again.record_json().unwrap(), &raw[..]);
        at += used;
    }
    assert_eq!(at, packed.len());
    (packer, packed.len())
}

/// Every record the core writes in the ticks of the two worlds of `tick_core.json`.
#[test]
fn the_records_of_a_run_come_back_byte_for_byte() {
    for name in ["small_replay", "generated"] {
        let fixture = load::entry("tick_core", name);
        let ticks = fixture["expected"]["ticks"].as_i64().unwrap();
        let mut run = PlainRun::of_entry(&fixture).unwrap();
        for _ in 0..ticks {
            run.step().unwrap();
        }
        let records: Vec<Vec<u8>> = run
            .loaded
            .cell
            .with(|world| {
                let log = world.log().unwrap();
                (0..log.len()).map(|index| log.written(index).unwrap().to_vec()).collect()
            })
            .unwrap();
        assert!(records.len() > 300, "{name}: {} records", records.len());
        let json: usize = records.iter().map(Vec::len).sum();
        let (packer, packed) = round_trip(&records);
        assert!(packed * 4 < json, "{name}: {packed} bytes packed of {json}");
        // An ID that ends in its number is its prefix in the table, not the ID.
        let table: std::collections::HashSet<&str> = packer.strings().iter().map(|text| &**text).collect();
        for raw in &records {
            let (event, ..) = EventRow::from_saved_record(raw).unwrap();
            let digits = event.event_id.bytes().rev().take_while(u8::is_ascii_digit).count();
            let number = &event.event_id[event.event_id.len() - digits..];
            if digits > 0 && digits < event.event_id.len() && !(number.len() > 1 && number.starts_with('0')) {
                assert!(!table.contains(&*event.event_id), "{name}: {}", event.event_id);
                assert!(table.contains(&event.event_id[..event.event_id.len() - digits]), "{name}: {}", event.event_id);
            }
        }
    }
}

fn each_log<'a>(value: &'a Value, found: &mut Vec<&'a Value>) {
    match value {
        Value::Object(map) => {
            for (key, item) in map {
                match (key.as_str(), item) {
                    ("event_log", Value::Array(records)) => found.extend(records),
                    _ => each_log(item, found),
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|item| each_log(item, found)),
        _ => {}
    }
}

/// Every record of every `event_log` in the fixture files: records Python wrote.
#[test]
fn the_records_of_the_fixtures_come_back_byte_for_byte() {
    let directory = format!("{}/tests/data", env!("CARGO_MANIFEST_DIR"));
    let mut total = 0;
    for path in std::fs::read_dir(&directory).unwrap() {
        let path = path.unwrap().path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let document: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let mut found = Vec::new();
        each_log(&document, &mut found);
        let records: Vec<Vec<u8>> = found.into_iter().map(|record| serde_json::to_vec(record).unwrap()).collect();
        total += records.len();
        round_trip(&records);
    }
    assert!(total > 100, "{total} records");
}

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

fn map(items: &[(&str, V)]) -> V {
    V::Map(items.iter().map(|(key, value)| (text(key), value.clone())).collect::<IndexMap<_, _>>())
}

fn row(event_id: &str, payload: V, provenance: V) -> EventRow {
    EventRow {
        event_id: text(event_id),
        kind: EventKind::Intervention,
        due_minute: 0,
        priority: -7,
        sequence: i64::MAX,
        event_type: text("t"),
        payload,
        source_id: None,
        causation_id: Some(text("cause-00")),
        provenance,
    }
}

/// The values `json.dumps` tells apart, the ends of each number, a map's key order, and strings
/// that end in digits but are not a prefix and a number as `u64` writes it.
#[test]
fn every_kind_of_value_and_string_comes_back() {
    let strings = [
        "",
        "123",
        "a-0",
        "a-007",
        "a-18446744073709551615",
        "a-18446744073709551616",
        "-5",
        "x99999999999999999999999",
        "日本語-3",
        "tab\tand\"quote\\",
        "\u{1F600}",
    ];
    let floats = [0.0, -0.0, 1.0, -1.5, f64::MIN_POSITIVE, 5e-324, f64::MAX, 0.1 + 0.2, 1e22, 123456789.0];
    let ints = [0, 1, -1, 63, 64, -64, -65, i64::MAX, i64::MIN];
    let payload = map(&[
        ("z", V::Null),
        ("a", V::Bool(true)),
        ("m", V::Bool(false)),
        ("ints", V::List(ints.iter().map(|value| V::Int(*value)).collect())),
        ("floats", V::List(floats.iter().map(|value| V::Float(*value)).collect())),
        ("big", V::Big(Box::from("-123456789012345678901234567890"))),
        ("strings", V::List(strings.iter().map(|value| V::Str(text(value))).collect())),
        ("empty", V::List(vec![])),
        ("nothing", map(&[])),
        ("nested", V::List(vec![map(&[("b", V::Int(1)), ("a", V::Int(2))]), map(&[("a", V::Int(2)), ("b", V::Int(1))]), V::List(vec![V::List(vec![])])])),
    ]);
    let mut packer = Packer::new();
    let mut packed = Vec::new();
    let mut rows = Vec::new();
    for (index, id) in strings.iter().chain(["action-lifecycle-136663"].iter()).enumerate() {
        let event = row(id, payload.clone(), map(&[("source_id", V::Str(text("ASM-1")))]));
        let status = EventStatus::ALL[index % 3];
        let detail = (index % 2 == 0).then(|| format!("detail {index}"));
        packer.pack(&mut packed, &event, index as i64, status, detail.as_deref()).unwrap();
        rows.push(Unpacked { event, realized_minute: index as i64, status, detail: detail.map(Arc::from) });
    }
    let mut at = 0;
    for row in &rows {
        let (again, used) = packer.unpack(&packed[at..]).unwrap();
        // `V`'s equality calls 0.0 and -0.0 equal: the bits are compared through the JSON.
        assert_eq!(&again, row);
        assert_eq!(again.record_json().unwrap(), row.record_json().unwrap());
        let V::Map(fields) = &again.event.payload else { panic!("a map") };
        assert_eq!(fields.keys().map(|key| &**key).collect::<Vec<_>>(), ["z", "a", "m", "ints", "floats", "big", "strings", "empty", "nothing", "nested"]);
        let V::List(floats_back) = &fields["floats"] else { panic!("a list") };
        for (back, sent) in floats_back.iter().zip(floats) {
            assert!(matches!(back, V::Float(value) if value.to_bits() == sent.to_bits()));
        }
        at += used;
    }
    assert_eq!(at, packed.len());
    // Only the prefix of an ID that ends in its number is in the table, not the ID.
    assert!(packer.strings().iter().any(|text| &**text == "action-lifecycle-"));
    assert!(!packer.strings().iter().any(|text| &**text == "action-lifecycle-136663"));
    assert!(packer.strings().iter().any(|text| &**text == "a-007"));
}

/// A negative minute or sequence is not a record of the log: an error, with nothing appended.
#[test]
fn a_record_the_form_cannot_hold_is_an_error() {
    let mut packer = Packer::new();
    let mut packed = Vec::new();
    assert!(packer.pack(&mut packed, &row("e-1", V::Null, V::Null), -1, EventStatus::Applied, None).is_err());
    let mut event = row("e-1", V::Null, V::Null);
    event.due_minute = -1;
    assert!(packer.pack(&mut packed, &event, 0, EventStatus::Applied, None).is_err());
    assert!(packed.is_empty());
}

/// Bytes cut short, changed or made up are an error or some record, never a panic; a packer
/// without the tables the bytes were packed against reads none of their strings.
#[test]
fn bytes_that_are_not_a_record_are_an_error() {
    let mut packer = Packer::new();
    let mut packed = Vec::new();
    let payload = map(&[("list", V::List(vec![V::Float(0.5), V::Str(text("x-1"))])), ("big", V::Big(Box::from("99999999999999999999")))]);
    packer.pack(&mut packed, &row("e-1", payload, map(&[])), 3, EventStatus::Failed, Some("why")).unwrap();
    for end in 0..packed.len() {
        assert!(packer.unpack(&packed[..end]).is_err(), "cut at {end}");
    }
    for at in 0..packed.len() {
        for flip in [0x01, 0x80, 0xff] {
            let mut changed = packed.clone();
            changed[at] ^= flip;
            let _ = packer.unpack(&changed);
        }
    }
    let made_up: [&[u8]; 6] = [
        &[0xff],
        &[0, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x02],
        &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 9],
        &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 6, 0xff, 0xff, 0xff, 0xff, 0x0f],
        &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 7, 0x7f],
        &[0, 0, 2],
    ];
    for bytes in made_up {
        assert!(packer.unpack(bytes).is_err(), "{bytes:?}");
    }
    assert!(Packer::new().unpack(&packed).is_err());
}
