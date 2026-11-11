//! `domain/social_network.py`: what an applied event records in `social_network_v1`
//! (docs/rust-port/lanes/m3-social-network.md). `observe_event` has no Python body left
//! (`kernel_only`): it skips what the Python rule skipped and raises its ValueError at the bound
//! itself. Unsupported, a `KernelRefusal` in Python, is left for what no applied event holds: an ID
//! with a character JSON escapes, a tainted namespace, a count past i64. Moved to the core in lane
//! P8 a2.
//!
//! `observe_event` reads the event's row and `body_state_v1` first, which may give Unsupported, and
//! only then writes the row of `social_network_v1`, where nothing can refuse any more. A tree Python
//! reads follows the row (lane P8-0c).

use std::sync::Arc;

use crate::error::{Error, Result};
use crate::mathx::{py_max, py_min};
use crate::rules::kit;
use crate::state::body_state::{BodyRow, NAME as BODY_STATE};
use crate::state::social_network::{is_plain_id, Relationship, SocialNetworkRow, MAX_RELATIONSHIPS, NAME};
use crate::value::V;
use crate::world::World;

const MAX_SHARED_MESSAGE_IDS: usize = 64;
/// The largest count a call adds to, so no `+= 1` here can overflow.
const LAST_SAFE_COUNT: i64 = i64::MAX - 4;
/// `observe_event`: "Cooperation applies the existing +0.01 per-Agent trust update in its handler".
const COOPERATION_TRUST_DELTA: f64 = 0.01;
/// `_relationship`'s trust of a record that has none yet, for `record_interaction`.
const UNSET_TRUST: f64 = 0.5;
/// The share of the relief that reaches the participant's and the actor's directed trust.
const PARTICIPANT_RELIEF_SHARE: f64 = 0.5;
const ACTOR_RELIEF_SHARE: f64 = 0.25;
/// `body_state_v1.config.get("social_action_affect_transfer", 0.02)`.
const DEFAULT_TRANSFER: f64 = 0.02;

/// The text of the bound, a `ValueError` of the Python rule.
pub const BOUND_REACHED: &str = "social_network_v1 reached the configured relationship bound";

/// The event types the rule records; any other changes nothing.
pub fn observes_event(event_type: &str) -> bool {
    matches!(event_type, "information_contact_update" | "proximity_contact_update" | "social_interaction")
}

/// An ID the keys of the rules are built from: one `json.dumps` writes as it is.
#[track_caller]
pub fn plain_id(id: &str) -> Result<&str> {
    if !is_plain_id(id) {
        return Err(Error::unsupported("an ID has a character that JSON escapes"));
    }
    Ok(id)
}

/// `ordered_pair_key` of two plain IDs.
pub fn pair_key(first: &str, second: &str) -> String {
    let mut key = String::with_capacity(first.len() + second.len() + 7);
    key.push_str("[\"");
    key.push_str(first);
    key.push_str("\",\"");
    key.push_str(second);
    key.push_str("\"]");
    key
}

/// `social_action_affect_transfer * signal` of an actor's positive and negative affect: the relief
/// of a social action, which `SocialInteractionHandler` and `observe_event` compute alike.
pub fn relief(positive: f64, negative: f64, transfer: f64) -> f64 {
    let signal = py_min(1.0, py_max(-1.0, positive - negative));
    transfer * signal
}

/// `config.get("social_action_affect_transfer", 0.02)` of the body row, as `float_or` reads it.
#[track_caller]
fn transfer_of(config: &V) -> Result<f64> {
    match config {
        V::Map(map) => match map.get("social_action_affect_transfer") {
            None => Ok(DEFAULT_TRANSFER),
            Some(V::Float(value)) => Ok(*value),
            Some(_) => Err(Error::unsupported("a value is not a float")),
        },
        _ => Err(Error::unsupported("a part of the body state is not a plain dict")),
    }
}

/// The body state's relief of an actor's social action: 0.5 and 0.0 for a person without a body
/// record, and no body state reads as an empty one.
pub fn action_relief(body: Option<&BodyRow>, actor_id: &str) -> Result<f64> {
    let Some(row) = body else {
        return Ok(relief(0.5, 0.0, DEFAULT_TRANSFER));
    };
    let record = row.agents.get(actor_id);
    let positive = record.map_or(0.5, |record| record.positive_affect);
    let negative = record.map_or(0.0, |record| record.negative_affect);
    Ok(relief(positive, negative, transfer_of(&row.config)?))
}

/// What `_sync_body_trust` copies for a key: None for no body record, or the body's trust.
fn body_trust(body: Option<&BodyRow>, key: &str) -> Option<Option<f64>> {
    body?.relationships.map.get(key).map(|record| record.trust)
}

/// One record's change: `(recipient, source)`, in the order of the Python rule.
struct RecordWrite {
    key: String,
    /// `record_contact` and `record_observed_contact` count a contact.
    contact: bool,
    /// `record_contact` and `record_interaction` count a conversation.
    conversation: bool,
    minute: i64,
    /// `record_interaction`'s `last_interaction`.
    interaction: Option<Arc<str>>,
    message: Option<Arc<str>>,
    /// The directional delta of `record_interaction`.
    trust_delta: Option<f64>,
    /// What `_sync_body_trust` then writes over it.
    body_trust: Option<Option<f64>>,
}

/// Everything a call writes, known to be writable.
struct NetworkWrite {
    records: Vec<RecordWrite>,
    interactions: i64,
    cooperation: bool,
}

impl NetworkWrite {
    /// Unsupported for a count a write would take past what is safe.
    fn check_counts(&self, row: &SocialNetworkRow) -> Result<()> {
        if row.interaction_count > LAST_SAFE_COUNT - self.interactions || row.information_transfer_count > LAST_SAFE_COUNT {
            return Err(Error::unsupported("a count is too large"));
        }
        for write in &self.records {
            if let Some(record) = row.relationships.map.get(write.key.as_str()) {
                if record.contact_count > LAST_SAFE_COUNT
                    || record.conversation_count > LAST_SAFE_COUNT
                    || record.cooperation_count > LAST_SAFE_COUNT
                {
                    return Err(Error::unsupported("a count is too large"));
                }
            }
        }
        Ok(())
    }

    fn apply(&self, world: &World) -> Result<()> {
        // Says Unsupported for a tainted namespace. The Python rule's ValueError at the bound comes
        // before anything is written here; the Python rule wrote the records before that one.
        {
            let row = world.row::<SocialNetworkRow>(NAME)?;
            let relationships = &row.relationships.map;
            let mut created: Vec<&str> = Vec::new();
            for write in &self.records {
                if !relationships.contains_key(write.key.as_str()) && !created.contains(&write.key.as_str()) {
                    if relationships.len() + created.len() >= MAX_RELATIONSHIPS {
                        return Err(Error::value_error(BOUND_REACHED));
                    }
                    created.push(write.key.as_str());
                }
            }
            self.check_counts(&row)?;
        }
        let mut row = world.row_mut::<SocialNetworkRow>(NAME)?;
        let mut transfers = 0;
        for write in &self.records {
            if !row.relationships.map.contains_key(write.key.as_str()) {
                row.relationships.map.insert(Arc::from(write.key.as_str()), Relationship::empty());
            }
            transfers += write_record(&mut row, write, self.cooperation)?;
        }
        if transfers != 0 {
            row.information_transfer_count += transfers;
        }
        row.interaction_count += self.interactions;
        Ok(())
    }
}

/// One record. Returns the information transfers it adds.
fn write_record(row: &mut SocialNetworkRow, write: &RecordWrite, cooperation: bool) -> Result<i64> {
    row.relationships.note_write(&write.key);
    let record = row
        .relationships
        .map
        .get_mut(write.key.as_str())
        .ok_or_else(|| Error::unsupported("the record is not in the row"))?;
    if write.contact {
        record.contact_count += 1;
    }
    if write.conversation {
        record.conversation_count += 1;
    }
    record.last_contact_minute = Some(write.minute);
    if let Some(interaction) = &write.interaction {
        record.last_interaction = Some(interaction.clone());
    }
    if cooperation {
        record.cooperation_count += 1;
    }
    let mut trust = record.trust;
    if let Some(delta) = write.trust_delta {
        trust = Some(py_min(1.0, py_max(0.0, trust.unwrap_or(UNSET_TRUST) + delta)));
    }
    if let Some(body) = write.body_trust {
        trust = body;
    }
    record.trust = trust;
    let mut transfers = 0;
    if let Some(message) = &write.message {
        if !record.shared_message_ids.iter().any(|known| known == message) {
            if record.shared_message_ids.len() >= MAX_SHARED_MESSAGE_IDS {
                record.shared_message_ids.remove(0);
            }
            record.shared_message_ids.push(message.clone());
            transfers = 1;
        }
    }
    Ok(transfers)
}

/// `model_state.get("body_state_v1")` as its row, None without it.
fn body_of(world: &World) -> Result<Option<crate::cell::ShardRef<'_, BodyRow>>> {
    if kit::namespace_absent(world, BODY_STATE)? {
        return Ok(None);
    }
    world.row::<BodyRow>(BODY_STATE).map(Some)
}

/// What `record_observed_contacts` writes, its refusals checked (`observed_contacts`).
pub struct ObservedContacts(NetworkWrite);

impl ObservedContacts {
    /// The writes, as the Python rule wrote them: its ValueError at the bound, and Unsupported only
    /// if the row changed since the check.
    pub fn apply(&self, world: &World) -> Result<()> {
        self.0.apply(world)
    }
}

/// `record_observed_contact` of each (recipient, source), for the social propagation step, checked
/// before the step writes its own namespace: Unsupported for a pair of one person or with an ID
/// JSON escapes, for a namespace without its row, and for a count past what is safe. The bound is
/// the Python rule's ValueError, which `apply` raises after the step's own writes, as before.
pub fn observed_contacts(world: &World, pairs: &[(&str, &str)], minute: i64) -> Result<ObservedContacts> {
    let mut records = Vec::with_capacity(pairs.len());
    for (recipient, source) in pairs {
        if recipient == source || !is_plain_id(recipient) || !is_plain_id(source) {
            return Err(Error::unsupported("a pair the kernel leaves to Python"));
        }
        records.push(RecordWrite {
            key: pair_key(recipient, source),
            contact: true,
            conversation: false,
            minute,
            interaction: None,
            message: None,
            trust_delta: None,
            body_trust: None,
        });
    }
    let write = NetworkWrite { interactions: records.len() as i64, records, cooperation: false };
    write.check_counts(&*world.row::<SocialNetworkRow>(NAME)?)?;
    Ok(ObservedContacts(write))
}

/// `observed_contacts` and its writes.
pub fn record_observed_contacts(world: &World, pairs: &[(&str, &str)], minute: i64) -> Result<()> {
    observed_contacts(world, pairs, minute)?.apply(world)
}

/// `value if isinstance(value, str) else None` of a payload's value: None for an absent key or a
/// value of another type.
fn str_field<'a>(payload: &'a V, key: &str) -> Option<&'a Arc<str>> {
    match get(payload, key) {
        Some(V::Str(value)) => Some(value),
        _ => None,
    }
}

/// `payload.get(key)` of a mapping; anything else reads as an empty payload.
fn get<'a>(payload: &'a V, key: &str) -> Option<&'a V> {
    match payload {
        V::Map(map) => map.get(key),
        _ => None,
    }
}

/// `isinstance(observed_minute, int) and not isinstance(observed_minute, bool)`: the int, or None.
#[track_caller]
fn int_field(value: Option<&V>) -> Result<Option<i64>> {
    match value {
        Some(V::Int(value)) => Ok(Some(*value)),
        Some(V::Big(_)) => Err(Error::unsupported("an int is too large")),
        _ => Ok(None),
    }
}

/// `observe_event` of an event of this type and payload: its contact or interaction, recorded, and the body's trust copied.
/// What the Python rule skipped is skipped: an ID that is not a str, a minute that is not an int,
/// one person with themselves. Nothing is written before an error or Unsupported is known.
pub fn observe_event(world: &World, event_type: &str, payload: &V, minute: i64) -> Result<()> {
    let event = payload;
    if !observes_event(event_type) {
        return Ok(());
    }
    let write = match event_type {
        "information_contact_update" => {
            let recipient = str_field(event, "recipient_agent_id");
            let sender = str_field(event, "sender_agent_id");
            let message = str_field(event, "message_id").cloned();
            let (Some(recipient), Some(sender)) = (recipient, sender) else {
                return Ok(());
            };
            let (recipient, sender) = (plain_id(recipient)?, plain_id(sender)?);
            // `record_contact` returns at once, and `_sync_body_trust` finds no such record.
            if recipient == sender {
                return Ok(());
            }
            let body = body_of(world)?;
            let key = pair_key(recipient, sender);
            NetworkWrite {
                records: vec![RecordWrite {
                    body_trust: body_trust(body.as_deref(), &key),
                    key,
                    contact: true,
                    conversation: true,
                    minute,
                    interaction: None,
                    message,
                    trust_delta: None,
                }],
                interactions: 1,
                cooperation: false,
            }
        }
        "proximity_contact_update" => {
            let first = str_field(event, "first_agent_id");
            let second = str_field(event, "second_agent_id");
            let observed = int_field(get(event, "observed_minute"))?;
            let (Some(first), Some(second), Some(observed)) = (first, second, observed) else {
                return Ok(());
            };
            let (first, second) = (plain_id(first)?, plain_id(second)?);
            // `record_observed_contact` returns at once, and `_sync_body_trust` finds no record.
            if first == second {
                return Ok(());
            }
            let body = body_of(world)?;
            let mut records = Vec::with_capacity(2);
            for (recipient, source) in [(first, second), (second, first)] {
                let key = pair_key(recipient, source);
                records.push(RecordWrite {
                    body_trust: body_trust(body.as_deref(), &key),
                    key,
                    contact: true,
                    conversation: false,
                    minute: observed,
                    interaction: None,
                    message: None,
                    trust_delta: None,
                });
            }
            NetworkWrite { records, interactions: 2, cooperation: false }
        }
        _ => {
            // `interaction in {...}`: a value of another type than a str is in no set of strs.
            let interaction = match get(event, "interaction_type") {
                None | Some(V::Null | V::Bool(_) | V::Int(_) | V::Big(_) | V::Float(_)) => return Ok(()),
                Some(V::Str(interaction)) => interaction.clone(),
                Some(V::List(_) | V::Map(_)) => return Err(Error::unsupported("a value is not a str")),
            };
            let social_action = matches!(&*interaction, "reassurance_exchange" | "family_care" | "voluntary_support");
            if !social_action && !matches!(&*interaction, "cooperation" | "information_reaction") {
                return Ok(());
            }
            let actor = str_field(event, "agent_id");
            let participant = str_field(event, "participant_agent_id");
            let (Some(actor), Some(participant)) = (actor, participant) else {
                return Ok(());
            };
            let (actor, participant) = (plain_id(actor)?, plain_id(participant)?);
            // `record_interaction` returns at once, and `_sync_body_trust` finds no record. The
            // relief the rule computes before that is not written anywhere.
            if actor == participant {
                return Ok(());
            }
            let body = body_of(world)?;
            let deltas = if social_action {
                let relief = action_relief(body.as_deref(), actor)?;
                let actor_delta =
                    if &*interaction == "reassurance_exchange" { 0.0 } else { py_max(0.0, relief * ACTOR_RELIEF_SHARE) };
                [Some(actor_delta), Some(relief * PARTICIPANT_RELIEF_SHARE)]
            } else if &*interaction == "cooperation" {
                [Some(COOPERATION_TRUST_DELTA); 2]
            } else {
                [None; 2]
            };
            let mut records = Vec::with_capacity(2);
            for ((recipient, source), trust_delta) in [(actor, participant), (participant, actor)].into_iter().zip(deltas) {
                let key = pair_key(recipient, source);
                records.push(RecordWrite {
                    body_trust: body_trust(body.as_deref(), &key),
                    key,
                    contact: false,
                    conversation: true,
                    minute,
                    interaction: Some(interaction.clone()),
                    message: None,
                    trust_delta,
                });
            }
            NetworkWrite { records, interactions: 1, cooperation: &*interaction == "cooperation" }
        }
    };
    write.apply(world)
}
