//! `validate_social_network_state(world)` of `domain/social_network.py`, with Python's errors and
//! texts (lane D of #250, #349). Raw errors.

use super::extension_head::py::{self, eq_int, eq_str, get, keys_are, Json};
use super::View;
use crate::error::{Error, Result};
use crate::value::V;

const NAME: &str = "social_network_v1";
const POLICY: &str = "directed-contact-relationships-v1";
const MAX_RELATIONSHIPS: usize = 100_000;
const RELATIONSHIP_FIELDS: [&str; 7] =
    ["contact_count", "conversation_count", "cooperation_count", "trust", "shared_message_ids", "last_contact_minute", "last_interaction"];

fn value_error<T>(text: &str) -> Result<T> {
    Err(Error::Value(text.to_owned()))
}

/// What `_pair_key_problem` finds: None for the compact JSON of two different strings.
enum Problem {
    Unparsable,
    NotAPair,
}

fn pair_key_problem(key: &str) -> Result<Option<Problem>> {
    let Some(pair) = py::loads(key)? else {
        return Ok(Some(Problem::Unparsable));
    };
    let Json::List(items) = pair else {
        return Ok(Some(Problem::NotAPair));
    };
    let [Json::Str(first), Json::Str(second)] = items.as_slice() else {
        return Ok(Some(Problem::NotAPair));
    };
    if first == second || py::dumps_texts(&[first, second]) != key {
        return Ok(Some(Problem::NotAPair));
    }
    Ok(None)
}

/// `isinstance(value, int) and not isinstance(value, bool) and value >= 0`.
fn is_count(value: &V) -> bool {
    py::is_exact_int(value) && !py::negative(value)
}

/// The check, as `validate` (`validate/mod.rs`) calls it in Python's order.
pub fn validate(view: &View<'_>) -> Result<()> {
    let Some(state) = view.state(NAME)? else {
        return Ok(());
    };
    let schema = || value_error("social_network_v1 has an invalid exact schema");
    let state = match &*state {
        V::Null => return Ok(()),
        V::Map(state) => state,
        _ => return schema(),
    };
    if !keys_are(state, &["schema_version", "policy_version", "relationships", "interaction_count", "information_transfer_count"])
        || !eq_int(state.get("schema_version"), 1)
        || !eq_str(state.get("policy_version"), POLICY)
    {
        return schema();
    }
    let V::Map(relationships) = get(state, "relationships") else {
        return schema();
    };
    if relationships.len() > MAX_RELATIONSHIPS {
        return schema();
    }
    for (key, record) in relationships {
        let problem = pair_key_problem(key)?;
        if matches!(problem, Some(Problem::Unparsable)) {
            return value_error("social_network_v1 relationship key is invalid");
        }
        let record = match record {
            V::Map(record)
                if problem.is_none()
                    && keys_are(record, &RELATIONSHIP_FIELDS)
                    && is_count(get(record, "contact_count"))
                    && is_count(get(record, "conversation_count"))
                    && is_count(get(record, "cooperation_count"))
                    && matches!(get(record, "shared_message_ids"), V::List(_)) =>
            {
                record
            }
            _ => return value_error("social_network_v1 relationship record is invalid"),
        };
        let trust = get(record, "trust");
        if !matches!(trust, V::Null) && !(py::is_real(trust) && py::number(trust).is_some_and(|trust| (0.0..=1.0).contains(&trust))) {
            return value_error("social_network_v1 trust value is invalid");
        }
        let minute = get(record, "last_contact_minute");
        if !matches!(minute, V::Null) && !is_count(minute) {
            return value_error("social_network_v1 contact time is invalid");
        }
    }
    Ok(())
}
