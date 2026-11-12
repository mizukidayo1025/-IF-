//! A rule's record of an outcome fills the log's indexes as a record Python appends does (Issue
//! #225, lane M7; moved from `tests/integration/test_event_rows.py`).
//!
//! A rule writes an outcome with `writer::record` (the PyO3 crate's `EventWriter::record`, and the
//! core's `kit::Events::record`); `EventLog.append` of a record Python made reaches the log as
//! `EventLog::append_row`. Both logs answer what the readers of `events/queue.py` ask
//! (`event_types`, `has_prior_care_request`, `completed_support_kinds`,
//! `applied_facility_event_ids`, `with_provenance_object`, `at_minute`,
//! `active_lifecycle_records`) the same, from the event's type, payload, provenance and status.

use std::sync::Arc;

use indexmap::IndexMap;
use serde_json::{json, Value};
use sheltersim_core::error::Error;
use sheltersim_core::events::log::EventLog;
use sheltersim_core::events::writer::{self, WriteError};
use sheltersim_core::events::{EventKind, EventRow, EventStatus};
use sheltersim_core::value::V;

fn v(value: &Value) -> V {
    match value {
        Value::Null => V::Null,
        Value::Bool(flag) => V::Bool(*flag),
        Value::Number(number) => V::Int(number.as_i64().expect("an int")),
        Value::String(text) => V::Str(Arc::from(text.as_str())),
        Value::Array(items) => V::List(items.iter().map(v).collect()),
        Value::Object(map) => V::Map(map.iter().map(|(key, item)| (Arc::from(key.as_str()), v(item))).collect()),
    }
}

fn map(value: Value) -> IndexMap<Arc<str>, V> {
    match v(&value) {
        V::Map(map) => map,
        other => panic!("not a map: {other:?}"),
    }
}

/// The pytest file's event: a completion of this type and payload at minute 0, with a provenance
/// whose `marker` is an object and `plain` is not.
fn event(event_type: &str, payload: Value) -> EventRow {
    EventRow::new(
        Arc::from("a"),
        EventKind::Completion,
        0,
        0,
        1,
        Arc::from(event_type),
        map(payload),
        None,
        None,
        map(json!({"marker": {"x": 1}, "plain": "y"})),
    )
    .expect("a valid row")
}

#[test]
fn a_rule_s_record_fills_the_indexes_of_a_python_record() {
    let care = json!({"request_id": "r", "agent_id": "A", "support_kind": "k", "status": "completed"});
    let cases = [
        ("care_support_transition", care.clone(), EventStatus::Applied),
        ("care_support_transition", care, EventStatus::Failed),
        ("facility_outage", json!({"facility_id": "f"}), EventStatus::Applied),
        ("facility_recovery", json!({"facility_id": "f"}), EventStatus::Failed),
        ("agent_action_lifecycle", json!({"agent_id": "A", "transition": "started"}), EventStatus::Applied),
        ("agent_action_lifecycle", json!({"agent_id": "A", "transition": "completed"}), EventStatus::Applied),
        ("other", json!({"agent_id": "A", "transition": "started", "facility_id": "f"}), EventStatus::Applied),
    ];
    for (event_type, payload, status) in cases {
        let row = event(event_type, payload);
        let mut rules = EventLog::new();
        writer::record(&mut rules, &row, 0, status, None).unwrap();
        let mut python = EventLog::new();
        assert_eq!(python.append_row(&row, 0, status, None).unwrap(), 0);
        let care = event_type == "care_support_transition";
        let applied = status == EventStatus::Applied;
        let facility = matches!(event_type, "facility_outage" | "facility_recovery");
        for log in [&rules, &python] {
            assert_eq!(log.event_types(), [event_type], "{event_type}");
            // A care request is known by its first event whatever its outcome.
            let first = log.first_care_request_event_of("r");
            assert_eq!(first.is_some_and(|first| first != "other"), care, "{event_type}");
            let kinds: &[String] = if care && applied { &["k".to_owned()] } else { &[] };
            assert_eq!(log.completed_kinds("A"), kinds, "{event_type}");
            let ids: &[String] = if facility && applied { &["a".to_owned()] } else { &[] };
            assert_eq!(log.applied_facility_event_ids("f"), ids, "{event_type}");
            assert_eq!(log.with_provenance_object("marker"), [0], "{event_type}");
            assert!(log.with_provenance_object("plain").is_empty(), "{event_type}");
            assert_eq!(log.at_minute(0), [0], "{event_type}");
        }
        assert_eq!(rules.active_lifecycle_positions(), python.active_lifecycle_positions(), "{event_type}");
        assert_eq!(rules.written(0), python.written(0), "{event_type}");
    }
}

#[test]
fn a_rule_cannot_record_a_minute_the_record_refuses() {
    let mut log = EventLog::new();
    assert_eq!(
        writer::record(&mut log, &event("record", json!({})), -1, EventStatus::Applied, None),
        Err(WriteError::Error(Error::Value("realized_minute must be a non-negative integer".into())))
    );
    assert_eq!(log.len(), 0);
}
