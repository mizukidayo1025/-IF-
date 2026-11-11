//! `ScheduledEvent` as a row with no Python handle: made by a rule, read back from a saved
//! record, and written as the canonical JSON of the event and of its outcome's record. The PyO3
//! crate reads a row from a `ScheduledEvent` and makes one from a row (`EventRowPy`).

use std::sync::Arc;

use indexmap::IndexMap;

use super::{is_stable_id, EventKind, EventStatus};
use crate::error::Result;
use crate::json::{write_json_string, Encoder};
use crate::value::V;

/// `ScheduledEvent` as a row. Everything `ScheduledEvent._check` asks has been checked, by
/// `EventRow::new` or by the constructor of the object the row was read from.
#[derive(Clone, Debug, PartialEq)]
pub struct EventRow {
    pub event_id: Arc<str>,
    pub kind: EventKind,
    pub due_minute: i64,
    pub priority: i64,
    pub sequence: i64,
    pub event_type: Arc<str>,
    pub payload: V,
    pub source_id: Option<Arc<str>>,
    pub causation_id: Option<Arc<str>>,
    pub provenance: V,
}

const _: fn() = || {
    fn send_and_sync<T: Send + Sync>() {}
    send_and_sync::<EventRow>();
};

fn all_finite(value: &V) -> bool {
    match value {
        V::Float(number) => number.is_finite(),
        V::List(items) => items.iter().all(all_finite),
        V::Map(map) => map.values().all(all_finite),
        _ => true,
    }
}

impl EventRow {
    /// A new event; None for what `ScheduledEvent` refuses. Then call the Python rule, which
    /// raises its own error. Keys keep the order given, as a dict literal does; handlers and the
    /// saved bytes do not depend on it, because a restored event has its keys sorted.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        event_id: Arc<str>,
        kind: EventKind,
        due_minute: i64,
        priority: i64,
        sequence: i64,
        event_type: Arc<str>,
        payload: IndexMap<Arc<str>, V>,
        source_id: Option<Arc<str>>,
        causation_id: Option<Arc<str>>,
        provenance: IndexMap<Arc<str>, V>,
    ) -> Option<Self> {
        let payload = V::Map(payload);
        let provenance = V::Map(provenance);
        let good = is_stable_id(&event_id)
            && is_stable_id(&event_type)
            && source_id.as_deref().is_none_or(is_stable_id)
            && causation_id.as_deref().is_none_or(is_stable_id)
            && due_minute >= 0
            && sequence >= 0
            && all_finite(&payload)
            && all_finite(&provenance);
        good.then_some(Self {
            event_id,
            kind,
            due_minute,
            priority,
            sequence,
            event_type,
            payload,
            source_id,
            causation_id,
            provenance,
        })
    }

    /// The canonical JSON of `event.to_dict()`.
    /// The keys of the payload and the provenance sorted, as an event read back from canonical
    /// JSON has them (`ScheduledEvent(**json.loads(...))`, lane 6-2).
    pub fn canonicalize(&mut self) {
        self.payload.canonicalize();
        self.provenance.canonicalize();
    }

    pub fn write_json(&self, encoder: &mut Encoder) -> Result<()> {
        let name = |encoder: &mut Encoder, text: &str| {
            encoder.out.push(b',');
            write_json_string(&mut encoder.out, text);
            encoder.out.push(b':');
        };
        let text = |encoder: &mut Encoder, value: &Option<Arc<str>>| match value {
            Some(value) => write_json_string(&mut encoder.out, value),
            None => encoder.out.extend_from_slice(b"null"),
        };
        let number = |encoder: &mut Encoder, value: i64| {
            encoder.out.extend_from_slice(itoa::Buffer::new().format(value).as_bytes());
        };
        // In sorted order.
        encoder.out.extend_from_slice(b"{\"causation_id\":");
        text(encoder, &self.causation_id);
        name(encoder, "due_minute");
        number(encoder, self.due_minute);
        name(encoder, "event_id");
        write_json_string(&mut encoder.out, &self.event_id);
        name(encoder, "event_type");
        write_json_string(&mut encoder.out, &self.event_type);
        name(encoder, "kind");
        write_json_string(&mut encoder.out, self.kind.as_str());
        name(encoder, "payload");
        self.payload.write_json(encoder)?;
        name(encoder, "priority");
        number(encoder, self.priority);
        name(encoder, "provenance");
        self.provenance.write_json(encoder)?;
        name(encoder, "sequence");
        number(encoder, self.sequence);
        name(encoder, "source_id");
        text(encoder, &self.source_id);
        encoder.out.push(b'}');
        Ok(())
    }

    /// The canonical JSON of the `EventLogRecord` of this event's outcome.
    pub fn record_json(&self, realized_minute: i64, status: EventStatus, detail: Option<&str>) -> Result<Box<[u8]>> {
        let mut encoder = Encoder { out: Vec::with_capacity(1024) };
        encoder.out.extend_from_slice(b"{\"detail\":");
        match detail {
            Some(text) => write_json_string(&mut encoder.out, text),
            None => encoder.out.extend_from_slice(b"null"),
        }
        encoder.out.extend_from_slice(b",\"event\":");
        self.write_json(&mut encoder)?;
        encoder.out.extend_from_slice(b",\"realized_minute\":");
        encoder.out.extend_from_slice(itoa::Buffer::new().format(realized_minute).as_bytes());
        encoder.out.extend_from_slice(b",\"status\":\"");
        encoder.out.extend_from_slice(status.as_str().as_bytes());
        encoder.out.extend_from_slice(b"\"}");
        Ok(encoder.out.into_boxed_slice())
    }

    /// `(event, realized_minute, status, detail)` of a record's canonical JSON, or None for bytes
    /// that are not such a record or hold a number outside i64 where the row needs one.
    pub fn from_saved_record(raw: &[u8]) -> Option<(Self, i64, EventStatus, Option<Arc<str>>)> {
        let document: serde_json::Value = serde_json::from_slice(raw).ok()?;
        let event = document.get("event")?;
        let text = |value: &serde_json::Value| value.as_str().map(Arc::from);
        let optional = |value: &serde_json::Value| -> Option<Option<Arc<str>>> {
            if value.is_null() {
                Some(None)
            } else {
                text(value).map(Some)
            }
        };
        let row = Self {
            event_id: text(event.get("event_id")?)?,
            kind: EventKind::parse(event.get("kind")?.as_str()?)?,
            due_minute: event.get("due_minute")?.as_i64()?,
            priority: event.get("priority")?.as_i64()?,
            sequence: event.get("sequence")?.as_i64()?,
            event_type: text(event.get("event_type")?)?,
            payload: from_json(event.get("payload")?)?,
            source_id: optional(event.get("source_id")?)?,
            causation_id: optional(event.get("causation_id")?)?,
            provenance: from_json(event.get("provenance")?)?,
        };
        Some((
            row,
            document.get("realized_minute")?.as_i64()?,
            EventStatus::parse(document.get("status")?.as_str()?)?,
            optional(document.get("detail")?)?,
        ))
    }
}

/// What `json.loads` reads, as a `V`.
fn from_json(value: &serde_json::Value) -> Option<V> {
    use serde_json::Value;
    Some(match value {
        Value::Null => V::Null,
        Value::Bool(flag) => V::Bool(*flag),
        Value::String(text) => V::Str(Arc::from(text.as_str())),
        Value::Number(number) => {
            let text = number.to_string();
            if text.contains(['.', 'e', 'E']) {
                V::Float(text.parse().ok()?)
            } else {
                match text.parse::<i64>() {
                    Ok(small) => V::Int(small),
                    Err(_) => V::Big(text.into()),
                }
            }
        }
        Value::Array(items) => V::List(items.iter().map(from_json).collect::<Option<Vec<_>>>()?),
        Value::Object(object) => {
            let mut map = IndexMap::with_capacity(object.len());
            for (key, item) in object {
                map.insert(Arc::from(key.as_str()), from_json(item)?);
            }
            V::Map(map)
        }
    })
}
