//! `social_network_v1`: who has met whom (`domain/social_network.py`). Its rules are
//! `rules/social_network.rs`; `domain/population.py` and `domain/population_lifecycle.py` take a
//! departing person's records out and put them back through the forwarded writes.
//!
//! A value of another type than the rules write taints the namespace, and the Python rules and
//! their errors run on the tree: an int `trust` or a message ID that is not a str, which validation
//! accepts, only costs the row.
//!
//! The row knows which records were written since the last check, so the check of each tick reads
//! those and not the thousands of others: a forwarded write and a rule's write are the only ways
//! into the row, and both say which record they touch (docs/rust-port/lanes/m3-social-network.md).
//!
//! The PyO3 crate's `state/social_network.rs` reads it from Python and holds its validator.

use std::sync::Arc;

use crate::error::Result;
use crate::json::Encoder;
use crate::row::{Field, IdMap};
use crate::state_row;

pub const NAME: &str = "social_network_v1";
const SCHEMA_VERSION: i64 = 1;
const POLICY_VERSION: &str = "directed-contact-relationships-v1";
/// `MAX_RELATIONSHIPS` of `domain/social_network.py`.
pub const MAX_RELATIONSHIPS: usize = 100_000;
/// Past this many unchecked writes, the next check reads every record instead of a long list.
const MAX_UNCHECKED_KEYS: usize = 4096;

state_row! {
    pub struct Relationship {
        contact_count: i64 => set_contact_count,
        conversation_count: i64 => set_conversation_count,
        cooperation_count: i64 => set_cooperation_count,
        trust: Option<f64> => set_trust,
        shared_message_ids: Vec<Arc<str>>,
        last_contact_minute: Option<i64> => set_last_contact_minute,
        last_interaction: Option<Arc<str>> => set_last_interaction,
    }
}

impl Relationship {
    pub fn empty() -> Self {
        Self {
            contact_count: 0,
            conversation_count: 0,
            cooperation_count: 0,
            trust: None,
            shared_message_ids: Vec::new(),
            last_contact_minute: None,
            last_interaction: None,
        }
    }

    /// The rules of `validate_social_network_state` that read the record's values. NaN fails.
    pub fn is_valid(&self) -> bool {
        self.contact_count >= 0
            && self.conversation_count >= 0
            && self.cooperation_count >= 0
            && self.trust.is_none_or(|trust| (0.0..=1.0).contains(&trust))
            && self.last_contact_minute.is_none_or(|minute| minute >= 0)
    }
}

/// A char of an ID that `json.dumps` writes as it is: ASCII, printable, not `"` nor `\`.
pub fn is_plain_id(id: &str) -> bool {
    id.bytes().all(|byte| (0x20..0x7f).contains(&byte) && byte != b'"' && byte != b'\\')
}

/// `_pair_key_problem` found nothing wrong: the compact JSON of two different strings. A key with
/// anything `json.dumps` escapes says false here, and the Python rule then decides.
pub fn pair_key_is_good(key: &str) -> bool {
    let Some(inner) = key.strip_prefix("[\"").and_then(|rest| rest.strip_suffix("\"]")) else {
        return false;
    };
    let Some((first, second)) = inner.split_once("\",\"") else {
        return false;
    };
    first != second && is_plain_id(first) && is_plain_id(second)
}

/// The records and which of them no check has read yet.
#[derive(Clone)]
pub struct Relationships {
    pub map: IdMap<Relationship>,
    unchecked_keys: Vec<Arc<str>>,
    all_unchecked: bool,
}

impl Relationships {
    pub fn note_write(&mut self, key: &str) {
        if self.all_unchecked {
            return;
        }
        if self.unchecked_keys.len() >= MAX_UNCHECKED_KEYS {
            self.all_unchecked = true;
            self.unchecked_keys = Vec::new();
            return;
        }
        // A rule's record has its key in the map: share that text.
        match self.map.get_key_value(key) {
            Some((shared, _)) => self.unchecked_keys.push(shared.clone()),
            None => self.unchecked_keys.push(Arc::from(key)),
        }
    }

    /// Whether the unchecked records pass. On false they stay unchecked, so the next check says
    /// false again until the bad value is gone.
    pub fn check(&mut self) -> bool {
        let good = |key: &str, record: &Relationship| pair_key_is_good(key) && record.is_valid();
        if self.all_unchecked {
            if !self.map.iter().all(|(key, record)| good(key, record)) {
                return false;
            }
        } else if !self.unchecked_keys.iter().all(|key| {
            // A record that has left the map since its write has nothing to check.
            self.map.get(&**key).is_none_or(|record| good(key, record))
        }) {
            return false;
        }
        self.all_unchecked = false;
        self.unchecked_keys.clear();
        true
    }
}

impl Relationships {
    /// Records read whole: the next check reads them all.
    pub fn from_map(map: IdMap<Relationship>) -> Self {
        Self { map, unchecked_keys: Vec::new(), all_unchecked: true }
    }
}

impl Field for Relationships {
    fn write_json(&self, encoder: &mut Encoder) -> Result<()> {
        Field::write_json(&self.map, encoder)
    }

    /// The keys sorted. The unchecked keys are a set, whose order no check reads.
    fn canonicalize(&mut self) {
        self.map.canonicalize();
    }
}

state_row! {
    pub struct SocialNetworkRow {
        schema_version: i64,
        policy_version: Arc<str>,
        relationships: Relationships,
        interaction_count: i64 => set_interaction_count,
        information_transfer_count: i64 => set_information_transfer_count,
    }
}

impl SocialNetworkRow {
    pub fn is_valid(&mut self) -> bool {
        self.schema_version == SCHEMA_VERSION
            && &*self.policy_version == POLICY_VERSION
            && self.relationships.map.len() <= MAX_RELATIONSHIPS
            && self.relationships.check()
    }
}
