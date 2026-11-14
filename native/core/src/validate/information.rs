//! The information_spread block of `WorldState.validate`, with Python's errors and texts:
//! `InformationSpreadConfig.from_mapping` and its canonical form (`domain/models.py`), and
//! `validate_information_runtime_state` (`domain/information.py`) with `TypedMessage.from_dict` and
//! `AgentInformationState.from_dict`. It raises the `StateInvariantError`s of `WorldState.validate`
//! itself, its own wrapping included.

use std::collections::HashSet;
use std::sync::Arc;

use indexmap::IndexMap;

use super::py::{dumps_texts, equal, finite, is_stable_id, type_name, unit_interval};
use super::{repr, repr_texts, wrap_value, View};
use crate::error::{Error, Result};
use crate::value::V;

const STATE: &str = "information_spread_v1";

/// The fields of `InformationSpreadConfig`, in its order.
const FIELDS: [&str; 29] = [
    "enabled",
    "max_contacts_per_agent",
    "minimum_contact_minutes",
    "contact_duration_scale_minutes",
    "base_transmission_probability",
    "correction_transmission_multiplier",
    "trust_sensitivity",
    "family_contact_multiplier",
    "adjacent_zone_contact_multiplier",
    "query_pressure_per_claim",
    "query_pressure_decay_per_minute",
    "query_pressure_relief_per_correction",
    "query_threshold",
    "query_utility_weight",
    "correction_acceptance_probability",
    "correction_trust_sensitivity",
    "correction_source_sensitivity",
    "rumor_belief_probability",
    "rumor_trust_sensitivity",
    "rumor_source_sensitivity",
    "default_source_reliability",
    "correction_source_reliability_by_id",
    "distress_sharing_sensitivity",
    "share_propensity_decay_per_minute",
    "forgetting_rate_per_minute",
    "schema_version",
    "policy_version",
    "source_type",
    "source_id",
];

/// The int fields and their bounds.
const INTS: [(&str, i64, i64); 3] =
    [("max_contacts_per_agent", 0, 16), ("minimum_contact_minutes", 1, 10_080), ("contact_duration_scale_minutes", 1, 10_080)];

/// The float fields and their upper bounds (as Python writes them).
const FLOATS: [(&str, f64, &str); 20] = [
    ("base_transmission_probability", 1.0, "1.0"),
    ("correction_transmission_multiplier", 2.0, "2.0"),
    ("trust_sensitivity", 1.0, "1.0"),
    ("family_contact_multiplier", 2.0, "2.0"),
    ("adjacent_zone_contact_multiplier", 2.0, "2.0"),
    ("query_pressure_per_claim", 1.0, "1.0"),
    ("query_pressure_decay_per_minute", 1.0, "1.0"),
    ("query_pressure_relief_per_correction", 1.0, "1.0"),
    ("query_threshold", 1.0, "1.0"),
    ("query_utility_weight", 4.0, "4.0"),
    ("correction_acceptance_probability", 1.0, "1.0"),
    ("correction_trust_sensitivity", 1.0, "1.0"),
    ("correction_source_sensitivity", 1.0, "1.0"),
    ("rumor_belief_probability", 1.0, "1.0"),
    ("rumor_trust_sensitivity", 1.0, "1.0"),
    ("rumor_source_sensitivity", 1.0, "1.0"),
    ("default_source_reliability", 1.0, "1.0"),
    ("distress_sharing_sensitivity", 1.0, "1.0"),
    ("share_propensity_decay_per_minute", 1.0, "1.0"),
    ("forgetting_rate_per_minute", 1.0, "1.0"),
];

const TRUTH: [&str; 3] = ["unknown", "true", "false"];
const BELIEF: [&str; 4] = ["unknown", "heard", "verified", "rejected"];

fn value_error(text: impl Into<String>) -> Error {
    Error::Value(text.into())
}

/// The check, as `validate` (`validate/mod.rs`) calls it in Python's order.
pub fn validate(view: &View<'_>) -> Result<()> {
    let config = view.config("information_spread")?;
    let enabled = from_mapping(config.as_ref()).map_err(wrap_value("invalid information_spread config: "))?;
    // The values are the config's own, so it is the canonical form when it has every field.
    if let Some(V::Map(config)) = &config {
        if config.len() != FIELDS.len() {
            return Err(Error::Invariant("information_spread config must contain the complete canonical v1 values".to_owned()));
        }
    }
    if enabled {
        let state = view.state(STATE)?;
        return runtime_state(view, state.as_deref()).map_err(wrap_value("invalid information-spread state: "));
    }
    if view.contains(STATE)? {
        return Err(Error::Invariant("information-spread state requires an enabled information_spread model".to_owned()));
    }
    Ok(())
}

/// `InformationSpreadConfig.from_mapping(document)`: whether it is enabled, or Python's error.
pub fn from_mapping(document: Option<&V>) -> Result<bool> {
    let document = match document {
        None | Some(V::Null) => return Ok(false),
        Some(V::Map(document)) => document,
        Some(_) => return Err(value_error("information_spread model config must be an object")),
    };
    let mut unknown: Vec<&str> = document.keys().map(|key| &**key).filter(|key| !FIELDS.contains(key)).collect();
    if !unknown.is_empty() {
        unknown.sort();
        return Err(value_error(format!("unknown information_spread settings: {}", repr_texts(unknown))));
    }
    let enabled = match document.get("enabled") {
        None => false,
        Some(V::Bool(enabled)) => *enabled,
        Some(_) => return Err(value_error("information_spread enabled must be boolean")),
    };
    for (name, minimum, maximum) in INTS {
        let ok = match document.get(name) {
            None => true,
            Some(V::Int(value)) => (minimum..=maximum).contains(value),
            Some(_) => false,
        };
        if !ok {
            return Err(value_error(format!("information_spread {name} must be between {minimum} and {maximum}")));
        }
    }
    for (name, maximum, written) in FLOATS {
        let ok = match document.get(name) {
            None => true,
            Some(value) => finite(value)?.is_some_and(|value| (0.0..=maximum).contains(&value)),
        };
        if !ok {
            return Err(value_error(format!("information_spread {name} must be finite and between 0 and {written}")));
        }
    }
    let reliability_error = || value_error("correction source reliability must map up to 100 source IDs to [0, 1]");
    match document.get("correction_source_reliability_by_id") {
        None => {}
        Some(V::Map(sources)) => {
            if sources.len() > 100 {
                return Err(reliability_error());
            }
            for (source_id, value) in sources {
                let ok = is_stable_id(source_id) && finite(value)?.is_some_and(|value| (0.0..=1.0).contains(&value));
                if !ok {
                    return Err(reliability_error());
                }
            }
        }
        Some(V::Str(text)) if text.chars().count() > 100 => return Err(reliability_error()),
        Some(V::List(items)) if items.len() > 100 => return Err(reliability_error()),
        // Python raises an AttributeError (`.items()`), which no rule catches.
        Some(V::Str(_) | V::List(_)) => return Err(Error::unsupported("AttributeError: the reliabilities have no items()")),
        Some(other) => return Err(value_error(format!("invalid information_spread settings: object of type '{}' has no len()", type_name(other)))),
    }
    let schema_ok = match document.get("schema_version") {
        None => true,
        Some(V::Int(version)) => *version == 1,
        Some(_) => false,
    };
    let policy_ok = document.get("policy_version").is_none_or(|policy| equal(policy, &V::Str(Arc::from("sparse-information-contact-v1"))));
    if !schema_ok || !policy_ok {
        return Err(value_error("unsupported information_spread schema or policy version"));
    }
    let source_type_ok = document.get("source_type").is_none_or(|value| equal(value, &V::Str(Arc::from("assumption"))));
    let source_id_ok = document.get("source_id").is_none_or(|value| equal(value, &V::Str(Arc::from("ASM-INFORMATION-SPREAD-V1"))));
    if !source_type_ok || !source_id_ok {
        return Err(value_error("information_spread provenance must use ASM-INFORMATION-SPREAD-V1"));
    }
    Ok(enabled)
}

/// `Enum(value)` of a str enum: the member's value, or `ValueError("{value!r} is not a valid {name}")`.
fn member<'a>(value: &V, name: &str, members: &[&'a str]) -> Result<&'a str> {
    if let V::Str(text) = value {
        if let Some(member) = members.iter().find(|member| **member == &**text) {
            return Ok(member);
        }
    }
    Err(value_error(format!("{} is not a valid {name}", repr(value))))
}

/// `_validate_information_id(name, value)`.
fn stable_id(name: &str, value: &V) -> Result<()> {
    match value {
        V::Str(text) if is_stable_id(text) => Ok(()),
        _ => Err(value_error(format!("{name} must be a stable ID"))),
    }
}

/// A non-negative int: `isinstance(value, int) and not bool and value >= 0`.
fn count(value: &V) -> bool {
    match value {
        V::Int(number) => *number >= 0,
        V::Big(text) => !text.starts_with('-'),
        _ => false,
    }
}

/// `value >= duration` for a non-negative int.
fn at_least(value: &V, duration: i64) -> bool {
    match value {
        V::Int(number) => *number >= duration,
        V::Big(text) => !text.starts_with('-'),
        _ => false,
    }
}

fn has_fields(map: &IndexMap<Arc<str>, V>, fields: &[&str]) -> bool {
    map.len() == fields.len() && fields.iter().all(|field| map.contains_key(*field))
}

/// `TypedMessage.from_dict(document)`.
fn typed_message(document: &V) -> Result<&IndexMap<Arc<str>, V>> {
    let fields = ["id", "truth_status", "source_id", "zone_id", "time", "origin_event_id", "origin_agent_id"];
    let document = match document {
        V::Map(document) if has_fields(document, &fields) => document,
        _ => return Err(value_error("TypedMessage fields do not match the supported schema")),
    };
    for name in ["id", "source_id", "zone_id", "origin_event_id", "origin_agent_id"] {
        stable_id(name, &document[name])?;
    }
    member(&document["truth_status"], "InformationTruthStatus", &TRUTH)?;
    if !count(&document["time"]) {
        return Err(value_error("message time must be a non-negative integer"));
    }
    Ok(document)
}

/// `AgentInformationState.from_dict(document)`: its message ID and last contact.
fn agent_information(document: &V) -> Result<(Arc<str>, V)> {
    let legacy = ["message_id", "belief", "source", "last_contact", "last_event_id"];
    let current = [&legacy[..], &["exposed", "belief_strength", "share_propensity", "correction_exposed", "correction_accepted"]].concat();
    let historical = [&current[..], &["ever_misbelieved", "ever_forgotten"]].concat();
    let extended = [&historical[..], &["last_correction_accepted"]].concat();
    let V::Map(document) = document else {
        return Err(value_error("AgentInformationState fields do not match the supported schema"));
    };
    let mut values: IndexMap<&str, V> = document.iter().map(|(key, value)| (&**key, value.clone())).collect();
    if has_fields(document, &legacy) {
        let belief = member(&document["belief"], "InformationBeliefStatus", &BELIEF)?;
        let corrected = matches!(belief, "verified" | "rejected");
        let strength = match belief {
            "verified" => 1.0,
            "rejected" => 0.0,
            _ => 0.5,
        };
        values.insert("exposed", V::Bool(true));
        values.insert("belief_strength", V::Float(strength));
        values.insert("share_propensity", V::Float(if belief == "unknown" { 0.0 } else { 0.5 }));
        values.insert("correction_exposed", V::Bool(corrected));
        values.insert("correction_accepted", V::Bool(corrected));
        values.insert("ever_misbelieved", V::Bool(false));
        values.insert("ever_forgotten", V::Bool(belief == "unknown"));
        values.insert("last_correction_accepted", V::Bool(corrected));
    } else if has_fields(document, &current) {
        let unknown = equal(&document["belief"], &V::Str(Arc::from("unknown")));
        values.insert("ever_misbelieved", V::Bool(false));
        values.insert("ever_forgotten", V::Bool(unknown));
        values.insert("last_correction_accepted", document["correction_accepted"].clone());
    } else if has_fields(document, &historical) {
        values.insert("last_correction_accepted", document["correction_accepted"].clone());
    } else if !has_fields(document, &extended) {
        return Err(value_error("AgentInformationState fields do not match the supported schema"));
    }
    for name in ["message_id", "source", "last_event_id"] {
        stable_id(name, &values[name])?;
    }
    let belief = member(&values["belief"], "InformationBeliefStatus", &BELIEF)?;
    if !count(&values["last_contact"]) {
        return Err(value_error("last_contact must be a non-negative integer"));
    }
    let is_bool = |name: &str| matches!(values[name], V::Bool(_));
    let truthy = |name: &str| matches!(values[name], V::Bool(true));
    if !is_bool("exposed") {
        return Err(value_error("information exposure state must be boolean"));
    }
    if !is_bool("correction_exposed") || !is_bool("correction_accepted") {
        return Err(value_error("correction exposure and acceptance must be boolean"));
    }
    if !is_bool("ever_misbelieved") || !is_bool("ever_forgotten") {
        return Err(value_error("historical information flags must be boolean"));
    }
    if !is_bool("last_correction_accepted") {
        return Err(value_error("last correction outcome must be boolean"));
    }
    if truthy("correction_accepted") && !truthy("correction_exposed") {
        return Err(value_error("a correction cannot be accepted before exposure"));
    }
    if truthy("last_correction_accepted") && !truthy("correction_exposed") {
        return Err(value_error("a correction outcome requires correction exposure"));
    }
    unit_interval("belief_strength", &values["belief_strength"])?;
    unit_interval("share_propensity", &values["share_propensity"])?;
    if !truthy("exposed") {
        return Err(value_error("stored information states must identify an exposure"));
    }
    if belief == "unknown" && !equal(&values["share_propensity"], &V::Float(0.0)) {
        return Err(value_error("unknown or forgotten information must have zero share propensity"));
    }
    let V::Str(message_id) = &values["message_id"] else {
        return Err(Error::unsupported("a checked message ID is not a str"));
    };
    Ok((message_id.clone(), values["last_contact"].clone()))
}

/// `json.loads(pair_key)` as two str, or None where it is something else; Python's error where it
/// cannot read the key.
fn pair_of(key: &str) -> Result<Option<(String, String)>> {
    let parsed: serde_json::Value = match serde_json::from_str(key) {
        Ok(parsed) => parsed,
        Err(_) if key.contains("NaN") || key.contains("Infinity") => return Err(Error::unsupported("a contact key json.loads may read")),
        Err(_) => return Err(value_error("information contact key is invalid")),
    };
    Ok(match parsed.as_array().map(Vec::as_slice) {
        Some([first, second]) => match (first.as_str(), second.as_str()) {
            (Some(first), Some(second)) => Some((first.to_owned(), second.to_owned())),
            _ => None,
        },
        _ => None,
    })
}

/// `validate_information_runtime_state(state, agent_ids=, zone_ids=, duration_minutes=)`.
fn runtime_state(view: &View<'_>, state: Option<&V>) -> Result<()> {
    let fields = ["schema_version", "messages", "beliefs", "contacts", "query_pressure", "support_requests", "counters"];
    let state = match state {
        Some(V::Map(state)) if has_fields(state, &fields) => state,
        _ => return Err(value_error("information-spread runtime state fields do not match the schema")),
    };
    let schema = match &state["schema_version"] {
        V::Int(version @ (1 | 2)) => *version,
        _ => return Err(value_error("unsupported information-spread runtime state schema")),
    };
    let rows = view.rows()?;
    let duration = view.duration_minutes()?;
    let messages = match &state["messages"] {
        V::Map(messages) if messages.len() <= 1024 => messages,
        _ => return Err(value_error("information-spread messages must be a bounded object")),
    };
    for (message_id, raw) in messages {
        let message = typed_message(raw)?;
        if !matches!(&message["id"], V::Str(id) if id == message_id) {
            return Err(value_error("information message key must match its ID"));
        }
        let zone_known = matches!(&message["zone_id"], V::Str(zone) if rows.zones.contains_key(zone));
        let agent_known = matches!(&message["origin_agent_id"], V::Str(agent) if rows.agents.contains_key(agent));
        if !zone_known || !agent_known {
            return Err(value_error("information message references an unknown zone or Agent"));
        }
        if at_least(&message["time"], duration) {
            return Err(value_error("information message time is outside the scenario"));
        }
    }
    let same_agents = |map: &IndexMap<Arc<str>, V>| map.len() == rows.agents.len() && map.keys().all(|key| rows.agents.contains_key(key));
    let beliefs = match &state["beliefs"] {
        V::Map(beliefs) if same_agents(beliefs) => beliefs,
        _ => return Err(value_error("information beliefs must contain exactly the known Agents")),
    };
    let current = [
        "message_id",
        "belief",
        "source",
        "last_contact",
        "last_event_id",
        "exposed",
        "belief_strength",
        "share_propensity",
        "correction_exposed",
        "correction_accepted",
        "ever_misbelieved",
        "ever_forgotten",
        "last_correction_accepted",
    ];
    for agent_beliefs in beliefs.values() {
        let V::Map(agent_beliefs) = agent_beliefs else {
            return Err(value_error("Agent information beliefs must be an object"));
        };
        for (message_id, raw) in agent_beliefs {
            if schema == 2 && !matches!(raw, V::Map(raw) if has_fields(raw, &current)) {
                return Err(value_error("v2 Agent information belief fields do not match schema"));
            }
            let (belief_message, last_contact) = agent_information(raw)?;
            if !messages.contains_key(message_id) || belief_message != *message_id {
                return Err(value_error("Agent information belief references an unknown message"));
            }
            if at_least(&last_contact, duration) {
                return Err(value_error("information contact time is outside the scenario"));
            }
        }
    }
    let contacts = match &state["contacts"] {
        V::Map(contacts) if contacts.len() <= rows.agents.len() * 16 => contacts,
        _ => return Err(value_error("information contacts must be a bounded object")),
    };
    for (key, record) in contacts {
        let pair = pair_of(key)?;
        let pair_ok = pair.as_ref().is_some_and(|(first, second)| {
            rows.agents.contains_key(first) && rows.agents.contains_key(second) && first != second && dumps_texts(&[first, second]) == **key
        });
        if !pair_ok {
            return Err(value_error("information contact key must identify an Agent pair"));
        }
        let record = match record {
            V::Map(record) if has_fields(record, &["last_seen_minute", "consecutive_minutes"]) => record,
            _ => return Err(value_error("information contact record fields do not match")),
        };
        for name in ["last_seen_minute", "consecutive_minutes"] {
            if !count(&record[name]) {
                return Err(value_error(format!("information contact {name} must be non-negative")));
            }
        }
        let (V::Int(seen), V::Int(consecutive)) = (&record["last_seen_minute"], &record["consecutive_minutes"]) else {
            return Err(Error::unsupported("an information contact minute is past i64"));
        };
        if *seen >= duration || *consecutive < 1 || *consecutive > seen + 1 {
            return Err(value_error("information contact duration is invalid"));
        }
    }
    let pressure = match &state["query_pressure"] {
        V::Map(pressure) if same_agents(pressure) => pressure,
        _ => return Err(value_error("information query pressure must contain exactly the known Agents")),
    };
    for (agent_id, value) in pressure {
        unit_interval(&format!("information query pressure for {agent_id}"), value)?;
    }
    let V::List(requests) = &state["support_requests"] else {
        return Err(value_error("information support requests must be a list"));
    };
    let mut request_ids: HashSet<&str> = HashSet::new();
    for request in requests {
        let request = match request {
            V::Map(request) if has_fields(request, &["request_id", "agent_id", "minute", "message_ids"]) => request,
            _ => return Err(value_error("information support request fields do not match")),
        };
        let request_id = match &request["request_id"] {
            V::Str(id) if !id.is_empty() && !request_ids.contains(&**id) => Some(&**id),
            _ => None,
        };
        let agent_ok = matches!(&request["agent_id"], V::Str(agent) if rows.agents.contains_key(agent));
        let minute_ok = matches!(request["minute"], V::Int(minute) if (0..duration).contains(&minute));
        let messages_ok = match &request["message_ids"] {
            V::List(ids) if !ids.is_empty() => {
                let texts: Option<Vec<&str>> = ids.iter().map(|id| if let V::Str(id) = id { Some(&**id) } else { None }).collect();
                texts.is_some_and(|texts| {
                    let unique: HashSet<&str> = texts.iter().copied().collect();
                    unique.len() == texts.len() && texts.iter().all(|id| messages.contains_key(*id))
                })
            }
            _ => false,
        };
        let Some(request_id) = request_id.filter(|_| agent_ok && minute_ok && messages_ok) else {
            return Err(value_error("information support request reference is invalid"));
        };
        request_ids.insert(request_id);
    }
    let counter_fields = ["contact_attempts", "contact_deliveries", "correction_deliveries", "support_requests"];
    let counters = match &state["counters"] {
        V::Map(counters) if has_fields(counters, &counter_fields) => counters,
        _ => return Err(value_error("information-spread counters do not match the schema")),
    };
    for (name, value) in counters {
        if !count(value) {
            return Err(value_error(format!("information-spread counter {name} must be non-negative")));
        }
    }
    if !equal(&counters["support_requests"], &V::Int(requests.len() as i64)) {
        return Err(value_error("information support-request counter does not match its records"));
    }
    Ok(())
}
