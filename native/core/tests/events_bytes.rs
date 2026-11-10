//! The core's Checkpoint byte kernels and events give what the Python-visible functions gave before
//! they moved. The expected values come from `sheltersim_native.chunk_hashes`,
//! `content_chunk_starts` and `split_json_array` of the build of `main` at `0085254`, run with
//! CPython 3.12; the queue's order is `EventQueue.pending()`'s: due minute, priority, sequence, and
//! for equal keys the order scheduled (`bisect.insort`).

use std::sync::Arc;

use indexmap::IndexMap;
use sheltersim_core::checkpoint::{
    chunk_hashes, content_chunk_starts, dumps_length, sha256_hex, split_json_array,
};
use sheltersim_core::error::Error;
use sheltersim_core::events::log::{fields_of_bytes, EventLog};
use sheltersim_core::events::queue::Queue;
use sheltersim_core::events::writer::{self, WriteError};
use sheltersim_core::events::{EventKind, EventRow, EventStatus};
use sheltersim_core::value::V;

const ITEMS: [&[u8]; 8] = [
    br#"{"a":1}"#,
    br#""x""#,
    b"[1,2,3]",
    b"null",
    br#"{"event_id":"e-1","n":12345678}"#,
    b"true",
    b"0.5",
    br#""\u00e9t\u00e9""#,
];

#[test]
fn chunk_hashes_are_the_sha256_of_the_joined_items() {
    let chunks: Vec<Vec<&[u8]>> = [(0, 3), (3, 2), (5, 3), (0, 0)]
        .iter()
        .map(|&(start, count)| ITEMS[start..start + count].to_vec())
        .collect();
    assert_eq!(
        chunk_hashes(&chunks),
        [
            "a05ff4df262798097c4b8ce8bdf2716913d2acd3d4f722874b4d8203e1f5d6ac",
            "55ba65d7ff93ce4a69e997b027b5aba8f3692dc8cdb8d4ea5cba95b79e0f97ab",
            "adc939b804d10a6d74700759375e0ad258dc58a1568ce133a032654768e62f3e",
            "4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945",
        ]
    );
    assert_eq!(sha256_hex(br#"[{"a":1},"x",[1,2,3]]"#), chunk_hashes(&chunks[..1])[0]);
}

#[test]
fn content_chunk_starts_depend_only_on_the_items() {
    let texts: Vec<String> = (0..600).map(|index| format!("{{\"i\":{index}}}")).collect();
    let items: Vec<&[u8]> = texts.iter().map(|text| text.as_bytes()).collect();
    assert_eq!(content_chunk_starts(&items, 128, 512).unwrap(), [0, 7, 221, 336, 541, 563, 595]);
    assert_eq!(
        content_chunk_starts(&items, 4, 8).unwrap()[..20],
        [0, 1, 3, 7, 10, 14, 18, 24, 31, 35, 38, 39, 42, 50, 52, 54, 57, 58, 59, 60]
    );
    assert_eq!(content_chunk_starts(&ITEMS, 2, 3).unwrap(), [0, 1, 3, 6, 7]);
    assert_eq!(
        content_chunk_starts(&ITEMS, 0, 3),
        Err(Error::Value("chunk sizes must be positive".into()))
    );
}

#[test]
fn split_json_array_cuts_at_top_level_commas() {
    let raw = br#"[{"a":[1,2]},"x,y",3,"q\""]"#;
    let items = split_json_array(raw).unwrap();
    assert_eq!(items, [&br#"{"a":[1,2]}"#[..], br#""x,y""#, b"3", br#""q\"""#]);
    assert!(split_json_array(b"[]").unwrap().is_empty());
    for bad in [&b""[..], b"[", b"[}]", br#"["x]"#, b"{}"] {
        assert_eq!(split_json_array(bad), Err(Error::Value("not a JSON array".into())));
    }
    // json.dumps({"a": [1, "x,y"], "b": None}, ensure_ascii=False) is 28 bytes.
    assert_eq!(dumps_length(br#"{"a":[1,"x,y"],"b":null}"#), 28);
}

fn row(event_id: &str, due: i64, priority: i64, sequence: i64) -> EventRow {
    let mut payload = IndexMap::new();
    payload.insert(Arc::from("agent_id"), V::Str(Arc::from("a-1")));
    EventRow::new(
        Arc::from(event_id),
        EventKind::Endogenous,
        due,
        priority,
        sequence,
        Arc::from("test_event"),
        payload,
        None,
        None,
        IndexMap::new(),
    )
    .expect("a valid row")
}

#[test]
fn the_queue_orders_by_due_minute_priority_and_sequence() {
    let mut queue = Queue::new();
    for (id, due, priority, sequence) in [("c", 5, 0, 3), ("a", 1, 2, 4), ("b", 1, 0, 9), ("d", 5, 0, 1), ("e", 1, 2, 2)] {
        assert_eq!(queue.schedule_row(row(id, due, priority, sequence)), 0);
    }
    let ids = |queue: &Queue, minute| -> Vec<String> {
        queue.pending_ids(minute).iter().map(|id| id.to_string()).collect()
    };
    assert_eq!(ids(&queue, None), ["b", "e", "a", "d", "c"]);
    assert_eq!(ids(&queue, Some(1)), ["b", "e", "a"]);
    assert_eq!(queue.schedule_row(row("a", 0, 0, 10)), 1);
    assert_eq!(queue.schedule_row(row("f", 0, 0, 9)), 2);

    // Equal keys keep the order scheduled; a replaced event goes after its equals.
    let generation = queue.generation();
    let first = queue.add(Arc::from("g"), (5, 0, 7), None);
    let second = queue.add(Arc::from("h"), (5, 0, 7), None);
    assert!(queue.generation() > generation);
    assert_eq!(ids(&queue, None), ["b", "e", "a", "d", "c", "g", "h"]);
    queue.rekey(first, (5, 0, 7));
    assert_eq!(ids(&queue, None), ["b", "e", "a", "d", "c", "h", "g"]);
    assert!(queue.slot(first).row.is_none() && queue.slot(second).key == (5, 0, 7));

    let (_, slot) = queue.take_slot("e").expect("pending");
    assert_eq!(slot.key, (1, 2, 2));
    assert!(!queue.has_event("e") && !queue.has_sequence(2));
    assert_eq!(queue.len(), 6);
    assert_eq!(queue.validate(), 0);
}

#[test]
fn a_repeated_identity_from_the_constructor_is_refused() {
    let mut queue = Queue::new();
    queue.add_loaded(Arc::from("a"), (0, 0, 1));
    queue.add_loaded(Arc::from("a"), (0, 0, 2));
    assert_eq!(queue.validate(), 1);
    let mut queue = Queue::new();
    queue.add_loaded(Arc::from("a"), (0, 0, 1));
    queue.add_loaded(Arc::from("b"), (0, 0, 1));
    assert_eq!(queue.validate(), 2);
}

#[test]
fn the_writer_schedules_and_records_with_the_python_errors() {
    let mut queue = Queue::new();
    let mut log = EventLog::new();
    assert_eq!(writer::schedule(&mut queue, &log, 60, row("e-1", 10, 0, 0)), Ok(1));
    assert_eq!(
        writer::schedule(&mut queue, &log, 60, row("e-2", 60, 0, 1)),
        Err(WriteError::Error(Error::Value("event 'e-2' is outside the scenario".into())))
    );
    assert_eq!(
        writer::schedule(&mut queue, &log, 60, row("e-1", 20, 0, 5)),
        Err(WriteError::Duplicate("duplicate event ID 'e-1'".into()))
    );
    let event = row("e-1", 10, 0, 0);
    writer::record(&mut log, &event, 10, EventStatus::Applied, None).unwrap();
    assert_eq!(
        writer::record(&mut log, &event, 10, EventStatus::Applied, None),
        Err(WriteError::Duplicate("event 'e-1' already realized".into()))
    );
    assert_eq!(
        writer::record(&mut log, &row("e-3", 10, 0, 0), 10, EventStatus::Applied, None),
        Err(WriteError::Duplicate("event sequence 0 already realized".into()))
    );
    assert_eq!(
        writer::record(&mut log, &row("e-4", 10, 0, 4), -1, EventStatus::Applied, None),
        Err(WriteError::Error(Error::Value("realized_minute must be a non-negative integer".into())))
    );
    assert_eq!(
        writer::schedule(&mut queue, &log, 60, row("e-5", 20, 0, 0)),
        Err(WriteError::Duplicate("event sequence 0 already realized".into()))
    );

    // The bytes written are a saved record that reads back to the same event.
    assert_eq!(log.len(), 1);
    let written = log.written(0).expect("written here").to_vec();
    assert_eq!(
        written,
        br#"{"detail":null,"event":{"causation_id":null,"due_minute":10,"event_id":"e-1","event_type":"test_event","kind":"endogenous","payload":{"agent_id":"a-1"},"priority":0,"provenance":{},"sequence":0,"source_id":null},"realized_minute":10,"status":"applied"}"#
    );
    let fields = fields_of_bytes(&written).expect("a saved record");
    assert_eq!((fields.event_id.as_str(), fields.sequence, fields.minute, fields.applied), ("e-1", 0, 10, true));
    let (read, minute, status, detail) = EventRow::from_saved_record(&written).expect("a saved record");
    assert_eq!((read, minute, status, detail), (event, 10, EventStatus::Applied, None));
    assert!(log.all_encoded());
    assert_eq!(log.take_written(0).as_deref(), Some(&written[..]));
    assert!(log.written(0).is_none());
}
