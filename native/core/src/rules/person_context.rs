//! `PersonContext.model_validate` (`contracts/population.py`) and `_fields_of_person`
//! (`policies/world_view.py`): what a view reads of a person's context (lane P8 a1).
//!
//! A context is checked as the contract checks it, so a document it refuses says `Unsupported` and
//! the Python rule raises its own text. Documents hold JSON values (`V`); pydantic's lax forms of
//! other Python types (a set for a tuple, bytes for a str) are not JSON, and no Run makes them.

use std::collections::HashSet;
use std::sync::Arc;

use crate::error::{Error, Result};
use crate::events::is_stable_id;
use crate::value::V;

/// What is read of a person's context: `_fields_of_person`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PersonFields {
    pub caregiver_ids: Vec<Arc<str>>,
    pub care_for_ids: Vec<Arc<str>>,
    pub parent_ids: Vec<Arc<str>>,
    pub roles: Vec<Arc<str>>,
    pub usual_wake_minute: Option<i64>,
    pub usual_sleep_minute: Option<i64>,
    pub required_sleep_minutes: Option<i64>,
    pub personal_context_present: bool,
}

pub const REFUSED: &str = "a person's context does not parse";

const ROLES: [&str; 6] = ["parent", "child", "caregiver", "dependent", "partner", "alone"];
const GUARDIAN_STATUSES: [&str; 3] = ["accompanied", "unaccompanied", "not_applicable"];
/// `SupportKind` of `contracts/operations.py`.
const SUPPORT_KINDS: [&str; 9] = [
    "medical_assessment",
    "medication_support",
    "caregiver_assistance",
    "privacy_accommodation",
    "mobility_assistance",
    "hygiene_support",
    "emotional_support",
    "language_access",
    "dependent_assistance",
];

#[track_caller]
fn refused<T>() -> Result<T> {
    Err(Error::unsupported(REFUSED))
}

#[track_caller]
fn text(value: &V) -> Result<&Arc<str>> {
    match value {
        V::Str(text) => Ok(text),
        _ => refused(),
    }
}

/// The items of a `tuple[..., ...]` field: a list, as documents hold it.
#[track_caller]
fn items(value: &V, limit: usize) -> Result<&[V]> {
    let V::List(found) = value else {
        return refused();
    };
    if found.len() > limit {
        return refused();
    }
    Ok(found)
}

#[track_caller]
fn identifier(value: &V) -> Result<Arc<str>> {
    let found = text(value)?;
    if is_stable_id(found) {
        Ok(found.clone())
    } else {
        refused()
    }
}

#[track_caller]
fn identifiers(value: &V, limit: usize) -> Result<Vec<Arc<str>>> {
    items(value, limit)?.iter().map(identifier).collect()
}

#[track_caller]
fn optional_identifier(value: &V) -> Result<()> {
    if !matches!(value, V::Null) {
        identifier(value)?;
    }
    Ok(())
}

/// A field of `bool`, in pydantic's lax mode: the bools, 0 and 1, and the words for them.
#[track_caller]
fn lax_bool(value: &V) -> Result<()> {
    match value {
        V::Bool(_) | V::Int(0 | 1) => Ok(()),
        V::Float(number) if *number == 0.0 || *number == 1.0 => Ok(()),
        V::Str(word) => {
            let word = word.to_ascii_lowercase();
            if matches!(&*word, "0" | "off" | "f" | "n" | "no" | "false" | "1" | "on" | "t" | "y" | "yes" | "true") {
                Ok(())
            } else {
                refused()
            }
        }
        _ => refused(),
    }
}

#[track_caller]
fn optional_lax_bool(value: &V) -> Result<()> {
    if matches!(value, V::Null) {
        Ok(())
    } else {
        lax_bool(value)
    }
}

/// A field of `int | None = Field(strict=True, ge=0, le=high)`.
#[track_caller]
fn optional_int(value: &V, high: i64) -> Result<Option<i64>> {
    match value {
        V::Null => Ok(None),
        V::Int(number) if (0..=high).contains(number) => Ok(Some(*number)),
        _ => refused(),
    }
}

/// `_fields_of_person(PersonContext.model_validate(raw))`.
pub fn parse(raw: &V) -> Result<PersonFields> {
    let V::Map(record) = raw else {
        return refused();
    };
    let mut fields = PersonFields { personal_context_present: true, ..PersonFields::default() };
    for (name, value) in record {
        match &**name {
            "household_id" | "daily_rhythm_id" => optional_identifier(value)?,
            "parent_ids" => fields.parent_ids = identifiers(value, 4)?,
            "caregiver_ids" => fields.caregiver_ids = identifiers(value, 16)?,
            "care_for_ids" => fields.care_for_ids = identifiers(value, 16)?,
            "partner_ids" => {
                identifiers(value, 16)?;
            }
            "relative_ids" => {
                identifiers(value, 32)?;
            }
            "requires_review" => lax_bool(value)?,
            "review_reasons" => {
                for reason in items(value, 32)? {
                    text(reason)?;
                }
            }
            "roles" => {
                let mut roles = Vec::new();
                for role in items(value, usize::MAX)? {
                    let role = text(role)?;
                    if !ROLES.contains(&&**role) {
                        return refused();
                    }
                    roles.push(role.clone());
                }
                fields.roles = roles;
            }
            "medical_support" | "medication_support" => optional_lax_bool(value)?,
            "guardian_status" => {
                if !matches!(value, V::Null) && !GUARDIAN_STATUSES.contains(&&**text(value)?) {
                    return refused();
                }
            }
            "care_support_needs" => {
                let mut seen = HashSet::new();
                for kind in items(value, 8)? {
                    let kind = text(kind)?;
                    if !SUPPORT_KINDS.contains(&&**kind) || !seen.insert(kind.clone()) {
                        return refused();
                    }
                }
            }
            "pet_count" => {
                optional_int(value, 20)?;
            }
            "information_access" => {
                if !matches!(value, V::Null) {
                    identifiers(value, usize::MAX)?;
                }
            }
            "usual_wake_minute" => fields.usual_wake_minute = optional_int(value, 1439)?,
            "usual_sleep_minute" => fields.usual_sleep_minute = optional_int(value, 1439)?,
            "required_sleep_minutes" => fields.required_sleep_minutes = optional_int(value, 1440)?,
            "source_ids" => {
                identifiers(value, usize::MAX)?;
            }
            _ => return refused(),
        }
    }
    Ok(fields)
}
