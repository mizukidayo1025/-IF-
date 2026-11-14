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

use crate::entities::{AgentRow, Table};
use crate::error::{Misfit, Result};
use crate::json::Encoder;
use crate::row::{value_bytes, Field, IdMap, Items};
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
    #[derive(Copy)]
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
#[derive(Clone, Default)]
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

    fn from_value(value: &V) -> std::result::Result<Self, Misfit> {
        Ok(Self::from_map(Field::from_value(value)?))
    }

    /// The keys sorted. What was checked stays checked: `people` is in the order of the body
    /// records, and a check that finds another order reads every relationship again.
    fn canonicalize(&mut self) {
        self.map.canonicalize();
    }
}

/// The social history: the canonical JSON of each item. Nothing reads an item here, only its
/// bytes, and a write into an old item doesn't fit: the namespace is then tainted.
///
/// A deque because at its limit each new item drops the oldest, which in a `Vec` would move all
/// 20,000 at each of the ~13 appends of a tick.
#[derive(Clone, Default)]
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

    /// Each item is its canonical JSON already, and a list keeps its order.
    fn canonicalize(&mut self) {}

    fn from_value(value: &V) -> std::result::Result<Self, Misfit> {
        let V::List(items) = value else {
            return Err(Misfit::new("not a list"));
        };
        Ok(Self { items: items.iter().map(value_bytes).collect::<std::result::Result<_, _>>()? })
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

impl BodyRow {
    /// `_ROWS_VALID`, the validator of the rows (#152; in the core since #250, for the end-of-tick
    /// check of a world without Python too): true only if the header, the Agent records and the
    /// relationships pass every rule `validate_body_state` has for them, for a world of these
    /// people. Its checks of the config and the history read what the row doesn't hold. False
    /// decides nothing: the rules then run, with their texts. What is accepted is not read again
    /// until it is written (`Relationships::written`); after a false, the next call reads every
    /// relationship again.
    pub fn rows_valid(&mut self, people: &Table<AgentRow>) -> bool {
        let accepted = self.rows_pass(people);
        if !accepted {
            // The rules decide now. If they pass it, the next check starts over, and the list of
            // written keys does not grow at each tick of such a world.
            self.relationships.read_all = true;
            self.relationships.written.clear();
        }
        accepted
    }

    fn rows_pass(&mut self, people: &Table<AgentRow>) -> bool {
        if self.schema_version != 1
            || &*self.policy_version != POLICY_VERSION
            || &*self.source_id != SOURCE_ID
            || self.agents.len() != people.len()
            || self.relationships.map.len() > MAX_SOCIAL_RELATIONSHIPS
            || self.history.items.len() > MAX_SOCIAL_HISTORY
            || !self.agents.iter().all(|(id, record)| people.get(id).is_some() && record_is_valid(record))
        {
            return false;
        }
        let relationships = &mut self.relationships;
        let same_people = relationships.people.len() == self.agents.len()
            && relationships.people.iter().zip(self.agents.keys()).all(|(known, id)| known == id);
        if relationships.read_all || !same_people {
            let valid = relationships.map.iter().all(|(key, record)| key_is_valid(key, &self.agents) && relationship_is_valid(record));
            if !valid {
                return false;
            }
            relationships.read_all = false;
            relationships.people = self.agents.keys().cloned().collect();
        } else {
            // A written key that left the map has nothing to check any more.
            let valid = relationships
                .written
                .iter()
                .all(|key| relationships.map.get(key).is_none_or(|record| key_is_valid(key, &self.agents) && relationship_is_valid(record)));
            if !valid {
                return false;
            }
        }
        relationships.written.clear();
        true
    }
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
