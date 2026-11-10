//! `domain/social_network.py`: what an applied event records in `social_network_v1`
//! (docs/rust-port/lanes/m3-social-network.md). `observe_event` has no Python body left
//! (`kernel_only`): it skips what the Python rule skipped and raises its ValueError at the bound
//! itself. Unsupported, a `KernelRefusal` in Python, is left for what no applied event holds: an ID
//! with a character JSON escapes, a str subclass, a tainted namespace, a count past i64.
//!
//! `observe_event` reads the event and `body_state_v1` first, which may give Unsupported, and only
//! then writes the row and the tree, where nothing can refuse any more. Both still are Python
//! objects (the event until lane E, the body state until its lane): they are read through
//! `as_readable_dict`, so a tree of a Rust row reads the same.

use std::sync::Arc;

use pyo3::exceptions::PyKeyError;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyFloat, PyInt, PyString};

use crate::core::errors;
use crate::core::mathx::{py_max, py_min};
use crate::core::row::{attribute, exact_float, exact_int, exact_str, unsupported, Field};
use crate::core::tracked::as_readable_dict;
use crate::core::value::V;
use crate::core::world_core::{held_state, mirror_of, raw_dict_get, Held, NamespaceMirror, Tree};
use crate::state::body_state::BodyRow;
use crate::state::social_network::{
    is_plain_id, Relationship, SocialNetworkRow, MAX_RELATIONSHIPS, NAME,
};
use crate::Unsupported;

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

/// `mapping[key]`, or None for a key it does not have.
pub(crate) fn mapping_get<'py>(
    mapping: &Bound<'py, PyAny>,
    key: &Bound<'py, PyString>,
) -> PyResult<Option<Bound<'py, PyAny>>> {
    match mapping.get_item(key) {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.is_instance_of::<PyKeyError>(mapping.py()) => Ok(None),
        Err(_) => unsupported("a payload cannot be read"),
    }
}

/// An ID the keys of the rules are built from. An exact str `json.dumps` writes as it is; another
/// value decides nothing here, and the Python rule says what it means.
pub(crate) fn plain_id<'a>(value: &'a Bound<'_, PyAny>) -> PyResult<&'a str> {
    let id = exact_str(value)?;
    if !is_plain_id(id) {
        return unsupported("an ID has a character that JSON escapes");
    }
    Ok(id)
}

/// `ordered_pair_key` of two plain IDs.
pub(crate) fn pair_key(first: &str, second: &str) -> String {
    let mut key = String::with_capacity(first.len() + second.len() + 7);
    key.push_str("[\"");
    key.push_str(first);
    key.push_str("\",\"");
    key.push_str(second);
    key.push_str("\"]");
    key
}

/// `get(name, {})` of `body_state_v1`, read as a dict: None for no such key.
fn body_part<'py>(
    body: Option<&Bound<'py, PyAny>>,
    name: &Bound<'py, PyString>,
) -> PyResult<Option<Bound<'py, PyAny>>> {
    let Some(body) = body else {
        return Ok(None);
    };
    let Some(part) = raw_dict_get(body, name.as_any())? else {
        return Ok(None);
    };
    if as_readable_dict(&part).is_none() {
        return unsupported("a part of the body state is not a plain dict");
    }
    Ok(Some(part))
}

/// What the rules here read of `body_state_v1`, or of a part of it: its row, which holds what the
/// tree holds (and is all there is while the namespace is detached), or the dict Python holds past
/// the row.
pub(crate) enum Body<'py> {
    Row(Bound<'py, NamespaceMirror>),
    Tree(Bound<'py, PyAny>),
}

/// `world.model_state.get("body_state_v1")`: its row, a readable dict, or None.
pub(crate) fn body_state_of<'py>(world: &Bound<'py, PyAny>) -> PyResult<Option<Body<'py>>> {
    match held_state(world, "body_state_v1")? {
        None => Ok(None),
        Some(Held::Row(mirror)) => Ok(Some(Body::Row(mirror))),
        Some(Held::Value(body)) => {
            if as_readable_dict(&body).is_none() {
                return unsupported("the body state is not a plain dict");
            }
            Ok(Some(Body::Tree(body)))
        }
    }
}

/// `config.get("social_action_affect_transfer", 0.02)` of the body row, as `float_or` reads it.
fn transfer_of(config: &V) -> PyResult<f64> {
    match config {
        V::Map(map) => match map.get("social_action_affect_transfer") {
            None => Ok(0.02),
            Some(V::Float(value)) => Ok(*value),
            Some(_) => unsupported("a value is not a float"),
        },
        _ => unsupported("a part of the body state is not a plain dict"),
    }
}

fn float_or<'py>(record: Option<&Bound<'py, PyAny>>, name: &Bound<'py, PyString>, default: f64) -> PyResult<f64> {
    let Some(record) = record else {
        return Ok(default);
    };
    match raw_dict_get(record, name.as_any())? {
        Some(value) => exact_float(&value),
        None => Ok(default),
    }
}

/// `social_action_affect_transfer * signal` of the actor: the relief of a social action, which
/// `SocialInteractionHandler` and `observe_event` compute alike.
pub(crate) fn action_relief<'py>(body: Option<&Body<'py>>, actor_id: &Bound<'py, PyString>) -> PyResult<f64> {
    let py = actor_id.py();
    let body = match body {
        Some(Body::Row(mirror)) => {
            let actor = actor_id.to_str()?;
            return NamespaceMirror::with_row::<BodyRow, _>(mirror, |row| {
                let record = row.agents.get(actor);
                let positive = record.map_or(0.5, |record| record.positive_affect);
                let negative = record.map_or(0.0, |record| record.negative_affect);
                let signal = py_min(1.0, py_max(-1.0, positive - negative));
                Ok(transfer_of(&row.config)? * signal)
            });
        }
        Some(Body::Tree(body)) => Some(body),
        None => None,
    };
    let agents = body_part(body, intern!(py, "agents"))?;
    let record = match &agents {
        Some(agents) => raw_dict_get(agents, actor_id.as_any())?,
        None => None,
    };
    if let Some(record) = &record {
        if as_readable_dict(record).is_none() {
            return unsupported("a body record is not a plain dict");
        }
    }
    let positive = float_or(record.as_ref(), intern!(py, "positive_affect"), 0.5)?;
    let negative = float_or(record.as_ref(), intern!(py, "negative_affect"), 0.0)?;
    let signal = py_min(1.0, py_max(-1.0, positive - negative));
    let config = body_part(body, intern!(py, "config"))?;
    let transfer = float_or(config.as_ref(), intern!(py, "social_action_affect_transfer"), 0.02)?;
    Ok(transfer * signal)
}

/// What `_sync_body_trust` copies for a key: None for no body record, or the body's trust.
fn body_trust(relationships: Option<&Body<'_>>, key: &str) -> PyResult<Option<Option<f64>>> {
    let relationships = match relationships {
        None => return Ok(None),
        Some(Body::Row(mirror)) => {
            return NamespaceMirror::with_row::<BodyRow, _>(mirror, |row| {
                Ok(row.relationships.map.get(key).map(|record| record.trust))
            });
        }
        Some(Body::Tree(relationships)) => relationships,
    };
    let py = relationships.py();
    let Some(record) = raw_dict_get(relationships, PyString::new(py, key).as_any())? else {
        return Ok(None);
    };
    if record.is_none() {
        return Ok(None);
    }
    let Some(record) = as_readable_dict(&record) else {
        return unsupported("a body relationship is not a plain dict");
    };
    let Some(trust) = record.get_item(intern!(py, "trust"))? else {
        return unsupported("a body relationship has no trust");
    };
    if trust.is_none() {
        return Ok(Some(None));
    }
    Ok(Some(Some(exact_float(&trust)?)))
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
    fn apply(&self, mirror: &Bound<'_, NamespaceMirror>) -> PyResult<()> {
        let py = mirror.py();
        // Says Unsupported for a tainted namespace. The Python rule's ValueError at the bound comes
        // before anything is written here; the Python rule wrote the records before that one.
        let new_keys = NamespaceMirror::with_row::<SocialNetworkRow, _>(mirror, |row| {
            let relationships = &row.relationships.map;
            let mut created: Vec<&str> = Vec::new();
            for write in &self.records {
                if !relationships.contains_key(write.key.as_str()) && !created.contains(&write.key.as_str()) {
                    if relationships.len() + created.len() >= MAX_RELATIONSHIPS {
                        return Err(errors::value_error("social_network_v1 reached the configured relationship bound"));
                    }
                    created.push(write.key.as_str());
                }
            }
            if row.interaction_count > LAST_SAFE_COUNT - self.interactions
                || row.information_transfer_count > LAST_SAFE_COUNT
            {
                return unsupported("a count is too large");
            }
            let mut new_keys = Vec::new();
            for write in &self.records {
                match relationships.get(write.key.as_str()) {
                    Some(record) => {
                        if record.contact_count > LAST_SAFE_COUNT
                            || record.conversation_count > LAST_SAFE_COUNT
                            || record.cooperation_count > LAST_SAFE_COUNT
                        {
                            return unsupported("a count is too large");
                        }
                    }
                    None => new_keys.push(write.key.as_str()),
                }
            }
            Ok(new_keys)
        })?;
        for key in new_keys {
            let record = Relationship::empty();
            let value = Field::to_py(&record, py)?;
            NamespaceMirror::with_row::<SocialNetworkRow, _>(mirror, |row| {
                row.relationships.map.insert(Arc::from(key), record);
                Ok(())
            })?;
            NamespaceMirror::write_through(mirror, &["relationships"], key, &value)?;
        }
        NamespaceMirror::with_row_tree::<SocialNetworkRow, _>(mirror, |row, tree| {
            let records = tree.child(intern!(py, "relationships"))?;
            let mut transfers = 0;
            for write in &self.records {
                transfers += write_record(row, &records, write, self.cooperation)?;
            }
            if transfers != 0 {
                let total = row.information_transfer_count + transfers;
                row.set_information_transfer_count(tree, total)?;
            }
            let total = row.interaction_count + self.interactions;
            row.set_interaction_count(tree, total)?;
            Ok(())
        })
    }
}

/// One record, row and tree. Returns the information transfers it adds.
fn write_record(
    row: &mut SocialNetworkRow,
    records: &Tree<'_>,
    write: &RecordWrite,
    cooperation: bool,
) -> PyResult<i64> {
    let py = records.py();
    let tree = records.child_str(&write.key)?;
    row.relationships.note_write(&write.key);
    let record = row
        .relationships
        .map
        .get_mut(write.key.as_str())
        .ok_or_else(|| Unsupported::new_err("the record is not in the row"))?;
    if write.contact {
        let contacts = record.contact_count + 1;
        record.set_contact_count(&tree, contacts)?;
    }
    if write.conversation {
        let conversations = record.conversation_count + 1;
        record.set_conversation_count(&tree, conversations)?;
    }
    record.set_last_contact_minute(&tree, Some(write.minute))?;
    if let Some(interaction) = &write.interaction {
        record.set_last_interaction(&tree, Some(interaction.clone()))?;
    }
    if cooperation {
        let cooperations = record.cooperation_count + 1;
        record.set_cooperation_count(&tree, cooperations)?;
    }
    let mut trust = record.trust;
    if let Some(delta) = write.trust_delta {
        trust = Some(py_min(1.0, py_max(0.0, trust.unwrap_or(UNSET_TRUST) + delta)));
    }
    if let Some(body) = write.body_trust {
        trust = body;
    }
    record.set_trust(&tree, trust)?;
    let mut transfers = 0;
    if let Some(message) = &write.message {
        if !record.shared_message_ids.iter().any(|known| known == message) {
            let list = tree.child(intern!(py, "shared_message_ids"))?;
            if record.shared_message_ids.len() >= MAX_SHARED_MESSAGE_IDS {
                record.shared_message_ids.remove(0);
                list.list_remove_first()?;
            }
            list.list_append(message)?;
            record.shared_message_ids.push(message.clone());
            transfers = 1;
        }
    }
    Ok(transfers)
}

/// The body's relationships: its row's, or the dict under `relationships` of the dict Python holds.
fn body_relationships<'py>(body: Option<Body<'py>>) -> PyResult<Option<Body<'py>>> {
    match body {
        Some(Body::Tree(body)) => {
            Ok(body_part(Some(&body), intern!(body.py(), "relationships"))?.map(Body::Tree))
        }
        other => Ok(other),
    }
}

/// `record_observed_contact` of each (recipient, source), for `rules/social_propagation.rs`, which
/// has written its own namespace by now: on Unsupported it calls the Python rule, and nothing has
/// been written here.
pub(crate) fn record_observed_contacts(
    world: &Bound<'_, PyAny>,
    pairs: &[(&str, &str)],
    minute: i64,
) -> PyResult<()> {
    let mirror = mirror_of(world, NAME)?;
    let mut records = Vec::with_capacity(pairs.len());
    for (recipient, source) in pairs {
        if recipient == source || !is_plain_id(recipient) || !is_plain_id(source) {
            return unsupported("a pair the kernel leaves to Python");
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
    NetworkWrite { interactions: records.len() as i64, records, cooperation: false }.apply(&mirror)
}

/// `value if isinstance(value, str) else None` of a payload's value: None for an absent key or a
/// value of another type. A str subclass is outside the contract.
fn str_or_none<'py>(value: Option<Bound<'py, PyAny>>) -> PyResult<Option<Bound<'py, PyAny>>> {
    match value {
        Some(value) if value.is_exact_instance_of::<PyString>() => Ok(Some(value)),
        Some(value) if value.is_instance_of::<PyString>() => unsupported("a value is a subclass of str"),
        _ => Ok(None),
    }
}

/// `isinstance(message_id, str)`: None when the payload has none or another value.
fn optional_message(value: Option<Bound<'_, PyAny>>) -> PyResult<Option<Arc<str>>> {
    match str_or_none(value)? {
        Some(value) => Ok(Some(Arc::from(exact_str(&value)?))),
        None => Ok(None),
    }
}

/// `payload.get(key)` when it is a str: the rule records nothing without one.
fn str_field<'py>(
    payload: &Bound<'py, PyAny>,
    key: &Bound<'py, PyString>,
) -> PyResult<Option<Bound<'py, PyAny>>> {
    str_or_none(mapping_get(payload, key)?)
}

/// `isinstance(observed_minute, int) and not isinstance(observed_minute, bool)`: the int, or None.
fn int_field(value: Option<Bound<'_, PyAny>>) -> PyResult<Option<i64>> {
    match value {
        Some(value) if value.is_exact_instance_of::<PyInt>() => Ok(Some(exact_int(&value)?)),
        Some(value) if value.is_exact_instance_of::<PyBool>() => Ok(None),
        Some(value) if value.is_instance_of::<PyInt>() => unsupported("a minute is a subclass of int"),
        _ => Ok(None),
    }
}

/// `observe_event`: the event's contact or interaction, recorded, and the body's trust copied.
/// What the Python rule skipped is skipped: an ID that is not a str, a minute that is not an int,
/// one person with themselves. Nothing is written before an error or Unsupported is known.
#[pyfunction]
pub(crate) fn observe_event<'py>(
    world: &Bound<'py, PyAny>,
    event: &Bound<'py, PyAny>,
    minute: &Bound<'py, PyAny>,
) -> PyResult<()> {
    let py = world.py();
    let event_type = attribute(event, intern!(py, "event_type"))?;
    let event_type = exact_str(&event_type)?;
    if !matches!(
        event_type,
        "information_contact_update" | "proximity_contact_update" | "social_interaction"
    ) {
        return Ok(());
    }
    let payload = attribute(event, intern!(py, "payload"))?;
    let write = match event_type {
        "information_contact_update" => {
            let recipient = str_field(&payload, intern!(py, "recipient_agent_id"))?;
            let sender = str_field(&payload, intern!(py, "sender_agent_id"))?;
            let message = optional_message(mapping_get(&payload, intern!(py, "message_id"))?)?;
            let (Some(recipient), Some(sender)) = (recipient, sender) else {
                return Ok(());
            };
            let (recipient, sender) = (plain_id(&recipient)?, plain_id(&sender)?);
            // `record_contact` returns at once, and `_sync_body_trust` finds no such record.
            if recipient == sender {
                return Ok(());
            }
            let body = body_state_of(world)?;
            let relationships = body_relationships(body)?;
            let key = pair_key(recipient, sender);
            NetworkWrite {
                records: vec![RecordWrite {
                    body_trust: body_trust(relationships.as_ref(), &key)?,
                    key,
                    contact: true,
                    conversation: true,
                    minute: exact_int(minute)?,
                    interaction: None,
                    message,
                    trust_delta: None,
                }],
                interactions: 1,
                cooperation: false,
            }
        }
        "proximity_contact_update" => {
            let first = str_field(&payload, intern!(py, "first_agent_id"))?;
            let second = str_field(&payload, intern!(py, "second_agent_id"))?;
            let observed = int_field(mapping_get(&payload, intern!(py, "observed_minute"))?)?;
            let (Some(first), Some(second), Some(observed)) = (first, second, observed) else {
                return Ok(());
            };
            let (first, second) = (plain_id(&first)?, plain_id(&second)?);
            // `record_observed_contact` returns at once, and `_sync_body_trust` finds no record.
            if first == second {
                return Ok(());
            }
            let body = body_state_of(world)?;
            let relationships = body_relationships(body)?;
            let mut records = Vec::with_capacity(2);
            for (recipient, source) in [(first, second), (second, first)] {
                let key = pair_key(recipient, source);
                records.push(RecordWrite {
                    body_trust: body_trust(relationships.as_ref(), &key)?,
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
            let Some(interaction) = mapping_get(&payload, intern!(py, "interaction_type"))? else {
                return Ok(());
            };
            // `interaction in {...}`: a value of another type than a str is in no set of strs.
            if interaction.is_none()
                || interaction.is_exact_instance_of::<PyBool>()
                || interaction.is_exact_instance_of::<PyInt>()
                || interaction.is_exact_instance_of::<PyFloat>()
            {
                return Ok(());
            }
            let interaction = exact_str(&interaction)?;
            let social_action =
                matches!(interaction, "reassurance_exchange" | "family_care" | "voluntary_support");
            if !social_action && !matches!(interaction, "cooperation" | "information_reaction") {
                return Ok(());
            }
            let actor_object = str_field(&payload, intern!(py, "agent_id"))?;
            let participant = str_field(&payload, intern!(py, "participant_agent_id"))?;
            let (Some(actor_object), Some(participant)) = (actor_object, participant) else {
                return Ok(());
            };
            let (actor, participant) = (plain_id(&actor_object)?, plain_id(&participant)?);
            // `record_interaction` returns at once, and `_sync_body_trust` finds no record. The
            // relief the rule computes before that is not written anywhere.
            if actor == participant {
                return Ok(());
            }
            let body = body_state_of(world)?;
            let deltas = if social_action {
                let relief = action_relief(body.as_ref(), actor_object.downcast_exact::<PyString>()?)?;
                let actor_delta = if interaction == "reassurance_exchange" {
                    0.0
                } else {
                    py_max(0.0, relief * ACTOR_RELIEF_SHARE)
                };
                [Some(actor_delta), Some(relief * PARTICIPANT_RELIEF_SHARE)]
            } else if interaction == "cooperation" {
                [Some(COOPERATION_TRUST_DELTA); 2]
            } else {
                [None; 2]
            };
            let relationships = body_relationships(body)?;
            let minute = exact_int(minute)?;
            let interaction_text: Arc<str> = Arc::from(interaction);
            let mut records = Vec::with_capacity(2);
            for ((recipient, source), trust_delta) in
                [(actor, participant), (participant, actor)].into_iter().zip(deltas)
            {
                let key = pair_key(recipient, source);
                records.push(RecordWrite {
                    body_trust: body_trust(relationships.as_ref(), &key)?,
                    key,
                    contact: false,
                    conversation: true,
                    minute,
                    interaction: Some(interaction_text.clone()),
                    message: None,
                    trust_delta,
                });
            }
            NetworkWrite { records, interactions: 1, cooperation: interaction == "cooperation" }
        }
    };
    let mirror = mirror_of(world, NAME)?;
    write.apply(&mirror)
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(observe_event, m)?)?;
    Ok(())
}
