//! `body_state_v1`: the long-term body and affect records, the relationships and the social
//! history (`domain/body_state.py`). `rules/body_state.rs` and `rules/psychology.rs` write it; the
//! handlers and the population rules that still run in Python write one value or one record at a
//! time through the forwarded writes (docs/rust-port/lanes/m3-body-state.md).
//!
//! A value of another type than the rules write taints the namespace, and the Python rules and
//! their errors run on the tree: an int in a record, which validation accepts, only costs the row.
//!
//! The PyO3 crate's `state/body_state.rs` reads it from Python and holds its validator.

use std::collections::VecDeque;
use std::sync::Arc;

use crate::error::Result;
use crate::json::Encoder;
use crate::row::{Field, IdMap, Items};
use crate::state_row;
use crate::value::V;

pub const NAME: &str = "body_state_v1";
pub const MAX_SOCIAL_HISTORY: usize = 20_000;
pub const MAX_SOCIAL_RELATIONSHIPS: usize = 100_000;
pub const POLICY_VERSION: &str = "life-state-feedback-v1";
pub const SOURCE_ID: &str = "ASM-LIFE-STATE-FEEDBACK-V1";
const MAX_SLEEP_DEBT_MINUTES: f64 = 1440.0;
const MAX_TOPICS: usize = 32;
const MAX_TOPIC_CHARS: usize = 64;

state_row! {
    #[derive(Clone, Copy)]
    pub struct BodyRecord {
        sleep_debt_minutes: f64 => set_sleep_debt_minutes,
        uncertainty: f64 => set_uncertainty,
        reassurance: f64 => set_reassurance,
        discontent: f64 => set_discontent,
        positive_affect: f64 => set_positive_affect,
        negative_affect: f64 => set_negative_affect,
        hygiene_need: f64 => set_hygiene_need,
    }
}

state_row! {
    #[derive(Clone)]
    pub struct Relationship {
        contact_count: i64 => set_contact_count,
        trust: Option<f64> => set_trust,
        contact_minutes: i64 => set_contact_minutes,
        last_contact_minute: Option<i64> => set_last_contact_minute,
        last_interaction: Option<Arc<str>> => set_last_interaction,
        topic_ids: Vec<Arc<str>>,
    }
}

/// The relationships, and which were written since `rows_valid` last accepted them, whoever wrote
/// them: a tick writes a few of the thousands, and the rules of a record read only it and its key.
#[derive(Default)]
pub struct Relationships {
    pub map: IdMap<Relationship>,
    pub written: Vec<Arc<str>>,
    pub read_all: bool,
    /// The people the accepted keys were checked against, in the order of the body records.
    pub people: Vec<Arc<str>>,
}

impl Relationships {
    /// Records read whole: the next check reads them all.
    pub fn from_map(map: IdMap<Relationship>) -> Self {
        Self { map, written: Vec::new(), read_all: true, people: Vec::new() }
    }
}

impl Field for Relationships {
    fn write_json(&self, encoder: &mut Encoder) -> Result<()> {
        self.map.write_json(encoder)
    }
}

/// The social history: the canonical JSON of each item. Nothing reads an item here, only its
/// bytes, and a write into an old item doesn't fit: the namespace is then tainted.
///
/// A deque because at its limit each new item drops the oldest, which in a `Vec` would move all
/// 20,000 at each of the ~13 appends of a tick.
#[derive(Default)]
pub struct History {
    pub items: VecDeque<Box<[u8]>>,
}

impl Items for History {
    fn count(&self) -> usize {
        self.items.len()
    }

    fn item_json<'a>(&'a self, index: usize, _scratch: &'a mut Encoder) -> Result<&'a [u8]> {
        Ok(&self.items[index])
    }
}

impl Field for History {
    fn items(&self) -> Option<&dyn Items> {
        Some(self)
    }

    fn write_json(&self, encoder: &mut Encoder) -> Result<()> {
        encoder.out.push(b'[');
        for (index, item) in self.items.iter().enumerate() {
            if index > 0 {
                encoder.out.push(b',');
            }
            encoder.out.extend_from_slice(item);
        }
        encoder.out.push(b']');
        Ok(())
    }
}

state_row! {
    pub struct BodyRow {
        schema_version: i64,
        policy_version: Arc<str>,
        source_id: Arc<str>,
        config: V,
        agents: IdMap<BodyRecord>,
        relationships: Relationships,
        history: History,
    }
}

pub fn in_range(value: f64, upper: f64) -> bool {
    // A NaN fails both, like `not 0.0 <= value <= upper`.
    (0.0..=upper).contains(&value)
}

pub fn record_is_valid(record: &BodyRecord) -> bool {
    in_range(record.sleep_debt_minutes, MAX_SLEEP_DEBT_MINUTES)
        && in_range(record.uncertainty, 1.0)
        && in_range(record.reassurance, 1.0)
        && in_range(record.discontent, 1.0)
        && in_range(record.positive_affect, 1.0)
        && in_range(record.negative_affect, 1.0)
        && in_range(record.hygiene_need, 1.0)
}

/// The two IDs of a key the Python rule certainly accepts: `["a","b"]`, of IDs that JSON writes as
/// they are. None says nothing about the key, and the Python rule then reads it.
pub fn pair_of(key: &str) -> Option<(&str, &str)> {
    fn plain(id: &str) -> bool {
        !id.is_empty()
            && id.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
    }
    let (first, second) = key.strip_prefix("[\"")?.strip_suffix("\"]")?.split_once("\",\"")?;
    (plain(first) && plain(second)).then_some((first, second))
}

pub fn relationship_is_valid(record: &Relationship) -> bool {
    record.contact_count >= 0
        && record.contact_minutes >= 0
        && record.trust.is_none_or(|trust| in_range(trust, 1.0))
        && record.last_contact_minute.is_none_or(|minute| minute >= 0)
        && record.topic_ids.len() <= MAX_TOPICS
        && record.topic_ids.iter().all(|topic| {
            topic.len() <= MAX_TOPIC_CHARS || topic.chars().count() <= MAX_TOPIC_CHARS
        })
}

pub fn key_is_valid<T>(key: &str, people: &IdMap<T>) -> bool {
    pair_of(key).is_some_and(|(first, second)| {
        first != second && people.contains_key(first) && people.contains_key(second)
    })
}

/// The record `_record_relationship` starts a pair with.
pub fn new_relationship() -> Relationship {
    Relationship {
        contact_count: 0,
        trust: Some(0.5),
        contact_minutes: 0,
        last_contact_minute: None,
        last_interaction: None,
        topic_ids: Vec::new(),
    }
}
