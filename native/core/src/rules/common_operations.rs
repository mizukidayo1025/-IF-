//! `domain/common_operations.py`: `record_shared_operation`, on the rows of a `World` (moved to the
//! core in P8b-1). The plan makes every check of the Python rule, in its order, and raises its
//! `ValueError` with its text before it changes anything (docs/rust-port/lanes/m4-staff.md); the
//! writes go to the row of `shared_operations_v1`, whose tree follows the row when the world is
//! given back (lane P8-0c). What only a state the rule's own checks never let through reaches is
//! Unsupported, which the entry raises as `KernelRefusal`.
//!
//! The receipts are typed here (`Receipt`). A receipt is data a kernel built, so the contracts of
//! `contracts/operations.py` that read it (`OperationHistoryRecord`, `NumericStateChange`,
//! `SharedOperationsState`) are Rust checks (policy E15): `Receipt::check` takes what the contract
//! takes and gives what `model_dump(mode="json")` gives (`Receipt::document`). What the contract
//! turns down, or what this check cannot tell (a value the contract would coerce, such as a str for
//! a float), is refused; inside `SharedOperationsState` a receipt the contract turns down is the
//! rule's `ValueError`, as it was.
//!
//! The events of the receipts come from the EventLog, whose records the core keeps as bytes; the
//! PyO3 crate looks up the records a receipt needs (`needed_records`) and hands them in (`Found`).

use std::sync::Arc;

use indexmap::IndexMap;

use crate::error::{Error, Result};
use crate::events::{EventRow, EventStatus};
use crate::json::Encoder;
use crate::row::Row;
use crate::rules::staff_dynamics::namespace;
use crate::state::shared_operations::{
    is_identifier, is_support_kind, CareHead, CareStatus, Receipt as Record, SharedOperationsRow, MAX_HISTORY, MAX_REQUESTS, NAME,
};
use crate::state::staff_operations::{StaffOperationsRow, NAME as OPERATIONS};
use crate::value::V;
use crate::host::{Hold, Holding, Host, NOT_OWNED, TABLE_NOT_PLAIN};
use crate::rules::kit;
use crate::world::{World, WorldCell, RESOURCES};

pub const MINUTE: &str = "shared operation receipt must use the current simulation boundary minute";
pub const NO_EVENT: &str = "shared operation receipt requires a matching realized applied event";
pub const RECORDED: &str = "shared operation event ID has already been recorded";
pub const NOT_THE_PAYLOAD: &str = "shared operation receipt must exactly match the applied event payload";
pub const STATE_INVALID: &str = "shared_operations_v1 state is invalid before operation append";
const AFTER_VALUE: &str = "care receipt after-value does not match the current saved world state";
const DISAGREES: &str = "staff operation receipt disagrees with retained completion history";

pub const CARE_EVENT: &str = "care_support_transition";
pub const CONTINUITY_EVENT: &str = "staff_support_continuity";
pub const HANDOFF_EVENT: &str = "staff_handoff";
pub const RECOVERY_EVENT: &str = "facility_recovery_transition";
pub const OPERATION_EVENT: &str = "staff_operation_transition";
/// The event types whose receipts `ingest_applied_shared_operation_records` records.
pub const RECEIPT_EVENTS: [&str; 5] = [CARE_EVENT, CONTINUITY_EVENT, RECOVERY_EVENT, HANDOFF_EVENT, OPERATION_EVENT];

// ---- Values -------------------------------------------------------------------------------------------

/// What `json.loads` reads, as a `V`.
pub fn json_value(bytes: &[u8]) -> Result<V> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| Error::unsupported("bytes that are not JSON"))?;
    of_json(&value).ok_or_else(|| Error::unsupported("a number that is not Python's"))
}

fn of_json(value: &serde_json::Value) -> Option<V> {
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
        Value::Array(items) => V::List(items.iter().map(of_json).collect::<Option<Vec<_>>>()?),
        Value::Object(object) => {
            let mut map = IndexMap::with_capacity(object.len());
            for (key, item) in object {
                map.insert(Arc::from(key.as_str()), of_json(item)?);
            }
            V::Map(map)
        }
    })
}

/// Whether an int and a float are equal as Python compares them: exactly.
fn int_equals_float(int: i64, float: f64) -> bool {
    // 2^63, exactly.
    const LIMIT: f64 = 9_223_372_036_854_775_808.0;
    float.is_finite() && float.fract() == 0.0 && (-LIMIT..LIMIT).contains(&float) && float as i64 == int
}

/// Python's `==` of two JSON values: numbers by value (`True == 1 == 1.0`), a dict by its keys and
/// values in any order, a list item by item.
pub fn py_eq(first: &V, second: &V) -> bool {
    let number = |value: &V| -> Option<std::result::Result<i64, f64>> {
        match value {
            V::Bool(flag) => Some(Ok(i64::from(*flag))),
            V::Int(number) => Some(Ok(*number)),
            V::Float(number) => Some(Err(*number)),
            _ => None,
        }
    };
    match (first, second) {
        (V::Null, V::Null) => true,
        (V::Str(one), V::Str(two)) => one == two,
        (V::Big(one), V::Big(two)) => one == two,
        (V::List(one), V::List(two)) => one.len() == two.len() && one.iter().zip(two).all(|(a, b)| py_eq(a, b)),
        (V::Map(one), V::Map(two)) => {
            one.len() == two.len() && one.iter().all(|(key, value)| two.get(key).is_some_and(|other| py_eq(value, other)))
        }
        _ => match (number(first), number(second)) {
            (Some(Ok(a)), Some(Ok(b))) => a == b,
            (Some(Err(a)), Some(Err(b))) => a == b,
            (Some(Ok(a)), Some(Err(b))) | (Some(Err(b)), Some(Ok(a))) => int_equals_float(a, b),
            _ => false,
        },
    }
}

fn text(value: &str) -> V {
    V::Str(Arc::from(value))
}

fn optional(value: &Option<Arc<str>>) -> V {
    match value {
        Some(text) => V::Str(text.clone()),
        None => V::Null,
    }
}

fn texts(values: &[Arc<str>]) -> V {
    V::List(values.iter().map(|value| V::Str(value.clone())).collect())
}

// ---- The receipts, as their contracts take them ------------------------------------------------------

/// Why a value is not a receipt: the contract turns it down, or this check cannot tell what the
/// contract would do (a value it may coerce).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Invalid,
    Unsure,
}

type Checked<T> = std::result::Result<T, Verdict>;

/// The keys of a document as a model reads them: every key must be read once (`extra="forbid"`).
struct Fields<'a> {
    map: &'a IndexMap<Arc<str>, V>,
    read: usize,
}

impl<'a> Fields<'a> {
    fn of(value: &'a V) -> Checked<Self> {
        match value {
            V::Map(map) => Ok(Self { map, read: 0 }),
            _ => Err(Verdict::Invalid),
        }
    }

    fn get(&mut self, key: &str) -> Option<&'a V> {
        let value = self.map.get(key);
        if value.is_some() {
            self.read += 1;
        }
        value
    }

    fn required(&mut self, key: &str) -> Checked<&'a V> {
        self.get(key).ok_or(Verdict::Invalid)
    }

    fn done(&self) -> Checked<()> {
        if self.read == self.map.len() {
            Ok(())
        } else {
            Err(Verdict::Invalid)
        }
    }

    /// `Literal[value] = value`.
    fn constant(&mut self, key: &str, wanted: &str) -> Checked<()> {
        match self.get(key) {
            None => Ok(()),
            Some(V::Str(found)) if &**found == wanted => Ok(()),
            Some(_) => Err(Verdict::Invalid),
        }
    }

    /// `schema_version: Literal[1] = 1`.
    fn schema(&mut self) -> Checked<()> {
        match self.get("schema_version") {
            None | Some(V::Int(1)) => Ok(()),
            Some(V::Bool(_) | V::Float(_) | V::Str(_)) => Err(Verdict::Unsure),
            Some(_) => Err(Verdict::Invalid),
        }
    }

    fn identifier(&mut self, key: &str) -> Checked<Arc<str>> {
        identifier(self.required(key)?)
    }

    fn optional_identifier(&mut self, key: &str) -> Checked<Option<Arc<str>>> {
        match self.get(key) {
            None | Some(V::Null) => Ok(None),
            Some(value) => identifier(value).map(Some),
        }
    }

    /// `Literal[...]` of str.
    fn one_of(&mut self, key: &str, allowed: &[&str], default: Option<&str>) -> Checked<Arc<str>> {
        match self.get(key) {
            None => default.map(Arc::from).ok_or(Verdict::Invalid),
            Some(V::Str(found)) if allowed.contains(&&**found) => Ok(found.clone()),
            Some(_) => Err(Verdict::Invalid),
        }
    }

    /// `int = Field(strict=True, ge=0)`.
    fn count(&mut self, key: &str) -> Checked<i64> {
        match self.required(key)? {
            V::Int(number) if *number >= 0 => Ok(*number),
            V::Big(text) if !text.starts_with('-') => Err(Verdict::Unsure),
            _ => Err(Verdict::Invalid),
        }
    }

    /// `tuple[Identifier, ...] = ()`.
    fn identifiers(&mut self, key: &str) -> Checked<Vec<Arc<str>>> {
        match self.get(key) {
            None => Ok(Vec::new()),
            Some(V::List(items)) => items.iter().map(identifier).collect(),
            Some(_) => Err(Verdict::Unsure),
        }
    }

    /// `str = Field(min_length=1, max_length=...)`.
    fn bounded_text(&mut self, key: &str, most: usize) -> Checked<Arc<str>> {
        match self.required(key)? {
            V::Str(found) if (1..=most).contains(&found.chars().count()) => Ok(found.clone()),
            _ => Err(Verdict::Invalid),
        }
    }

    /// `tuple[NumericStateChange, ...] = Field(default=(), max_length=most)`.
    fn changes(&mut self, key: &str, most: usize) -> Checked<Vec<Change>> {
        match self.get(key) {
            None => Ok(Vec::new()),
            Some(V::List(items)) if items.len() <= most => items.iter().map(Change::check).collect(),
            Some(V::List(_)) => Err(Verdict::Invalid),
            Some(_) => Err(Verdict::Unsure),
        }
    }

    /// `bool = False`.
    fn flag(&mut self, key: &str) -> Checked<bool> {
        match self.get(key) {
            None => Ok(false),
            Some(V::Bool(flag)) => Ok(*flag),
            Some(V::Int(_) | V::Float(_) | V::Str(_)) => Err(Verdict::Unsure),
            Some(_) => Err(Verdict::Invalid),
        }
    }
}

/// `Identifier`.
fn identifier(value: &V) -> Checked<Arc<str>> {
    match value {
        V::Str(found) if is_identifier(found) => Ok(found.clone()),
        _ => Err(Verdict::Invalid),
    }
}

/// `Finite`, a float: an int becomes its float, as the contract makes it.
fn finite(value: &V) -> Checked<f64> {
    match value {
        V::Float(number) if number.is_finite() => Ok(*number),
        V::Float(_) => Err(Verdict::Invalid),
        V::Int(number) if number.unsigned_abs() <= 1 << 53 => Ok(*number as f64),
        V::Int(_) | V::Big(_) | V::Bool(_) | V::Str(_) => Err(Verdict::Unsure),
        _ => Err(Verdict::Invalid),
    }
}

/// `^[A-Za-z0-9_]+(?:\.[A-Za-z0-9_]+){0,5}$`, at most 128 characters.
fn is_field_path(path: &str) -> bool {
    path.len() <= 128
        && path.split('.').count() <= 6
        && path.split('.').all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'))
}

const ENTITY_KINDS: [&str; 6] = ["agent", "staff", "facility", "zone", "resource", "model_state"];
const CARE_STATUSES: [&str; 6] = ["requested", "assigned", "started", "completed", "interrupted", "declined"];
const OBSERVATIONS: [&str; 4] = ["not_applicable", "observed_change", "observed_no_change", "not_measured"];
const TASK_KINDS: [&str; 5] = ["cleaning", "maintenance", "information_outreach", "intake", "patrol"];

/// `NumericStateChange`.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub entity_id: Arc<str>,
    pub entity_kind: Arc<str>,
    pub field_path: Arc<str>,
    pub before: f64,
    pub after: f64,
    pub unit: Arc<str>,
}

impl Change {
    fn check(value: &V) -> Checked<Self> {
        let mut fields = Fields::of(value)?;
        let change = Self {
            entity_id: fields.identifier("entity_id")?,
            entity_kind: fields.one_of("entity_kind", &ENTITY_KINDS, None)?,
            field_path: match fields.required("field_path")? {
                V::Str(path) if is_field_path(path) => path.clone(),
                _ => return Err(Verdict::Invalid),
            },
            before: finite(fields.required("before")?)?,
            after: finite(fields.required("after")?)?,
            unit: fields.bounded_text("unit", 80)?,
        };
        fields.done()?;
        Ok(change)
    }

    /// `check`, where what the contract turns down or can't be told is Unsupported.
    #[track_caller]
    pub fn of(value: &V) -> Result<Self> {
        Self::check(value).map_err(|_| Error::unsupported("a state change its contract does not take as it is"))
    }

    pub fn document(&self) -> V {
        let mut document = IndexMap::with_capacity(6);
        document.insert(Arc::from("entity_id"), V::Str(self.entity_id.clone()));
        document.insert(Arc::from("entity_kind"), V::Str(self.entity_kind.clone()));
        document.insert(Arc::from("field_path"), V::Str(self.field_path.clone()));
        document.insert(Arc::from("before"), V::Float(self.before));
        document.insert(Arc::from("after"), V::Float(self.after));
        document.insert(Arc::from("unit"), V::Str(self.unit.clone()));
        V::Map(document)
    }
}

/// `CareTransitionReceipt`.
#[derive(Clone, Debug, PartialEq)]
pub struct CareReceipt {
    pub event_id: Arc<str>,
    pub request_id: Arc<str>,
    pub agent_id: Arc<str>,
    pub household_id: Option<Arc<str>>,
    pub support_kind: Arc<str>,
    pub status: CareStatus,
    pub minute: i64,
    pub staff_id: Option<Arc<str>>,
    pub outcome_observation: Arc<str>,
    pub state_changes: Vec<Change>,
    pub causation_id: Option<Arc<str>>,
    pub source_ids: Vec<Arc<str>>,
}

/// `StaffSupportContinuityReceipt`.
#[derive(Clone, Debug, PartialEq)]
pub struct ContinuityReceipt {
    pub event_id: Arc<str>,
    pub request_id: Arc<str>,
    pub agent_id: Arc<str>,
    pub task_id: Arc<str>,
    pub previous_staff_id: Arc<str>,
    pub status: Arc<str>,
    pub cause_type: Arc<str>,
    pub minute: i64,
    pub causation_id: Arc<str>,
    pub source_ids: Vec<Arc<str>>,
}

/// `StaffHandoffReceipt`.
#[derive(Clone, Debug, PartialEq)]
pub struct HandoffReceipt {
    pub event_id: Arc<str>,
    pub task_id: Arc<str>,
    pub previous_staff_id: Arc<str>,
    pub replacement_staff_id: Option<Arc<str>>,
    pub status: Arc<str>,
    pub minute: i64,
    pub reason_code: Arc<str>,
    pub causation_id: Option<Arc<str>>,
    pub source_ids: Vec<Arc<str>>,
}

/// `FacilityRecoveryReceipt`.
#[derive(Clone, Debug, PartialEq)]
pub struct RecoveryReceipt {
    pub event_id: Arc<str>,
    pub facility_id: Arc<str>,
    pub component_id: Arc<str>,
    pub status: Arc<str>,
    pub minute: i64,
    pub previous_working_units: i64,
    pub working_units: i64,
    pub source_ids: Vec<Arc<str>>,
    pub causation_id: Option<Arc<str>>,
}

/// `StaffOperationReceipt`.
#[derive(Clone, Debug, PartialEq)]
pub struct OperationReceipt {
    pub event_id: Arc<str>,
    pub task_id: Arc<str>,
    pub staff_id: Arc<str>,
    pub task_kind: Arc<str>,
    pub target_id: Option<Arc<str>>,
    pub target_zone_id: Arc<str>,
    pub reason: Arc<str>,
    pub status: Arc<str>,
    pub minute: i64,
    pub start_minute: i64,
    pub arrival_minute: i64,
    pub travel_minutes: i64,
    pub outcome_observation: Arc<str>,
    pub state_changes: Vec<Change>,
    pub total_state_change_count: i64,
    pub state_changes_truncated: bool,
    pub causation_id: Arc<str>,
    pub source_ids: Vec<Arc<str>>,
}

/// `OperationHistoryRecord`: a receipt of `shared_operations_v1`'s history.
#[derive(Clone, Debug, PartialEq)]
pub enum Receipt {
    Care(CareReceipt),
    Continuity(ContinuityReceipt),
    Handoff(HandoffReceipt),
    Recovery(RecoveryReceipt),
    Operation(OperationReceipt),
}

fn any_change(changes: &[Change]) -> bool {
    changes.iter().any(|change| change.before != change.after)
}

impl Receipt {
    /// `TypeAdapter(OperationHistoryRecord).validate_python(value)`, by its `event_type`.
    pub fn check(value: &V) -> Checked<Self> {
        let mut fields = Fields::of(value)?;
        let receipt = match fields.required("event_type")? {
            V::Str(kind) if &**kind == CARE_EVENT => Self::Care(CareReceipt::check(&mut fields)?),
            V::Str(kind) if &**kind == CONTINUITY_EVENT => Self::Continuity(ContinuityReceipt::check(&mut fields)?),
            V::Str(kind) if &**kind == HANDOFF_EVENT => Self::Handoff(HandoffReceipt::check(&mut fields)?),
            V::Str(kind) if &**kind == RECOVERY_EVENT => Self::Recovery(RecoveryReceipt::check(&mut fields)?),
            V::Str(kind) if &**kind == OPERATION_EVENT => Self::Operation(OperationReceipt::check(&mut fields)?),
            _ => return Err(Verdict::Invalid),
        };
        fields.done()?;
        Ok(receipt)
    }

    /// `check`, where a value the contract turns down, or one it can't tell, is Unsupported: a
    /// receipt in an event a kernel made always passes.
    #[track_caller]
    pub fn of(value: &V) -> Result<Self> {
        Self::check(value).map_err(|_| Error::unsupported("a receipt its contract does not take as it is"))
    }

    pub fn event_type(&self) -> &'static str {
        match self {
            Self::Care(_) => CARE_EVENT,
            Self::Continuity(_) => CONTINUITY_EVENT,
            Self::Handoff(_) => HANDOFF_EVENT,
            Self::Recovery(_) => RECOVERY_EVENT,
            Self::Operation(_) => OPERATION_EVENT,
        }
    }

    pub fn event_id(&self) -> &Arc<str> {
        match self {
            Self::Care(receipt) => &receipt.event_id,
            Self::Continuity(receipt) => &receipt.event_id,
            Self::Handoff(receipt) => &receipt.event_id,
            Self::Recovery(receipt) => &receipt.event_id,
            Self::Operation(receipt) => &receipt.event_id,
        }
    }

    pub fn minute(&self) -> i64 {
        match self {
            Self::Care(receipt) => receipt.minute,
            Self::Continuity(receipt) => receipt.minute,
            Self::Handoff(receipt) => receipt.minute,
            Self::Recovery(receipt) => receipt.minute,
            Self::Operation(receipt) => receipt.minute,
        }
    }

    pub fn causation_id(&self) -> Option<&Arc<str>> {
        match self {
            Self::Care(receipt) => receipt.causation_id.as_ref(),
            Self::Continuity(receipt) => Some(&receipt.causation_id),
            Self::Handoff(receipt) => receipt.causation_id.as_ref(),
            Self::Recovery(receipt) => receipt.causation_id.as_ref(),
            Self::Operation(receipt) => Some(&receipt.causation_id),
        }
    }

    pub fn source_ids(&self) -> &[Arc<str>] {
        match self {
            Self::Care(receipt) => &receipt.source_ids,
            Self::Continuity(receipt) => &receipt.source_ids,
            Self::Handoff(receipt) => &receipt.source_ids,
            Self::Recovery(receipt) => &receipt.source_ids,
            Self::Operation(receipt) => &receipt.source_ids,
        }
    }

    /// `model_dump(mode="json")`: every field in the order of the model.
    pub fn document(&self) -> V {
        let mut document: IndexMap<Arc<str>, V> = IndexMap::with_capacity(21);
        let mut put = |name: &str, value: V| {
            document.insert(Arc::from(name), value);
        };
        let record_type = if matches!(self, Self::Care(_) | Self::Continuity(_)) { "care" } else { "operations" };
        put("record_type", text(record_type));
        put("schema_version", V::Int(1));
        put("event_type", text(self.event_type()));
        put("event_id", V::Str(self.event_id().clone()));
        match self {
            Self::Care(receipt) => {
                put("request_id", V::Str(receipt.request_id.clone()));
                put("agent_id", V::Str(receipt.agent_id.clone()));
                put("household_id", optional(&receipt.household_id));
                put("support_kind", V::Str(receipt.support_kind.clone()));
                put("status", text(receipt.status.text()));
                put("minute", V::Int(receipt.minute));
                put("staff_id", optional(&receipt.staff_id));
                put("outcome_observation", V::Str(receipt.outcome_observation.clone()));
                put("state_changes", V::List(receipt.state_changes.iter().map(Change::document).collect()));
                put("causation_id", optional(&receipt.causation_id));
                put("source_ids", texts(&receipt.source_ids));
            }
            Self::Continuity(receipt) => {
                put("request_id", V::Str(receipt.request_id.clone()));
                put("agent_id", V::Str(receipt.agent_id.clone()));
                put("task_id", V::Str(receipt.task_id.clone()));
                put("previous_staff_id", V::Str(receipt.previous_staff_id.clone()));
                put("status", V::Str(receipt.status.clone()));
                put("cause_type", V::Str(receipt.cause_type.clone()));
                put("minute", V::Int(receipt.minute));
                put("causation_id", V::Str(receipt.causation_id.clone()));
                put("source_ids", texts(&receipt.source_ids));
            }
            Self::Handoff(receipt) => {
                put("task_id", V::Str(receipt.task_id.clone()));
                put("previous_staff_id", V::Str(receipt.previous_staff_id.clone()));
                put("replacement_staff_id", optional(&receipt.replacement_staff_id));
                put("status", V::Str(receipt.status.clone()));
                put("minute", V::Int(receipt.minute));
                put("reason_code", V::Str(receipt.reason_code.clone()));
                put("causation_id", optional(&receipt.causation_id));
                put("source_ids", texts(&receipt.source_ids));
            }
            Self::Recovery(receipt) => {
                put("facility_id", V::Str(receipt.facility_id.clone()));
                put("component_id", V::Str(receipt.component_id.clone()));
                put("status", V::Str(receipt.status.clone()));
                put("minute", V::Int(receipt.minute));
                put("previous_working_units", V::Int(receipt.previous_working_units));
                put("working_units", V::Int(receipt.working_units));
                put("source_ids", texts(&receipt.source_ids));
                put("causation_id", optional(&receipt.causation_id));
            }
            Self::Operation(receipt) => {
                put("task_id", V::Str(receipt.task_id.clone()));
                put("staff_id", V::Str(receipt.staff_id.clone()));
                put("task_kind", V::Str(receipt.task_kind.clone()));
                put("target_id", optional(&receipt.target_id));
                put("target_zone_id", V::Str(receipt.target_zone_id.clone()));
                put("reason", V::Str(receipt.reason.clone()));
                put("status", V::Str(receipt.status.clone()));
                put("minute", V::Int(receipt.minute));
                put("start_minute", V::Int(receipt.start_minute));
                put("arrival_minute", V::Int(receipt.arrival_minute));
                put("travel_minutes", V::Int(receipt.travel_minutes));
                put("outcome_observation", V::Str(receipt.outcome_observation.clone()));
                put("state_changes", V::List(receipt.state_changes.iter().map(Change::document).collect()));
                put("total_state_change_count", V::Int(receipt.total_state_change_count));
                put("state_changes_truncated", V::Bool(receipt.state_changes_truncated));
                put("causation_id", V::Str(receipt.causation_id.clone()));
                put("source_ids", texts(&receipt.source_ids));
            }
        }
        V::Map(document)
    }
}

impl CareReceipt {
    fn check(fields: &mut Fields<'_>) -> Checked<Self> {
        fields.constant("record_type", "care")?;
        fields.schema()?;
        let receipt = Self {
            event_id: fields.identifier("event_id")?,
            request_id: fields.identifier("request_id")?,
            agent_id: fields.identifier("agent_id")?,
            household_id: fields.optional_identifier("household_id")?,
            support_kind: match fields.required("support_kind")? {
                V::Str(kind) if is_support_kind(kind) => kind.clone(),
                _ => return Err(Verdict::Invalid),
            },
            status: CareStatus::parse(&fields.one_of("status", &CARE_STATUSES, None)?).ok_or(Verdict::Invalid)?,
            minute: fields.count("minute")?,
            staff_id: fields.optional_identifier("staff_id")?,
            outcome_observation: fields.one_of("outcome_observation", &OBSERVATIONS, Some("not_applicable"))?,
            state_changes: fields.changes("state_changes", 32)?,
            causation_id: fields.optional_identifier("causation_id")?,
            source_ids: fields.identifiers("source_ids")?,
        };
        // `result_evidence_matches_transition`.
        let (observation, changes) = (&*receipt.outcome_observation, &receipt.state_changes);
        let valid = if receipt.status == CareStatus::Completed {
            match observation {
                "observed_change" => !changes.is_empty() && any_change(changes),
                "observed_no_change" => !changes.is_empty() && !any_change(changes),
                _ => changes.is_empty(),
            }
        } else {
            observation == "not_applicable" && changes.is_empty()
        };
        if !valid {
            return Err(Verdict::Invalid);
        }
        Ok(receipt)
    }
}

impl ContinuityReceipt {
    fn check(fields: &mut Fields<'_>) -> Checked<Self> {
        fields.constant("record_type", "care")?;
        fields.schema()?;
        let receipt = Self {
            event_id: fields.identifier("event_id")?,
            request_id: fields.identifier("request_id")?,
            agent_id: fields.identifier("agent_id")?,
            task_id: fields.identifier("task_id")?,
            previous_staff_id: fields.identifier("previous_staff_id")?,
            status: fields.one_of("status", &["requeued", "unresolved"], None)?,
            cause_type: fields.one_of("cause_type", &["staff_unavailable", "staff_shift_change", "route_unreachable"], Some("staff_unavailable"))?,
            minute: fields.count("minute")?,
            causation_id: fields.identifier("causation_id")?,
            source_ids: fields.identifiers("source_ids")?,
        };
        // `only_support_continuity_is_recorded`.
        if receipt.event_id == receipt.causation_id {
            return Err(Verdict::Invalid);
        }
        Ok(receipt)
    }
}

impl HandoffReceipt {
    fn check(fields: &mut Fields<'_>) -> Checked<Self> {
        fields.constant("record_type", "operations")?;
        fields.schema()?;
        let receipt = Self {
            event_id: fields.identifier("event_id")?,
            task_id: fields.identifier("task_id")?,
            previous_staff_id: fields.identifier("previous_staff_id")?,
            replacement_staff_id: fields.optional_identifier("replacement_staff_id")?,
            status: fields.one_of("status", &["reassigned", "unfilled", "rejected"], None)?,
            minute: fields.count("minute")?,
            reason_code: fields.identifier("reason_code")?,
            causation_id: fields.optional_identifier("causation_id")?,
            source_ids: fields.identifiers("source_ids")?,
        };
        // `status_matches_assignment`.
        if (&*receipt.status == "reassigned") != receipt.replacement_staff_id.is_some()
            || receipt.replacement_staff_id.as_ref() == Some(&receipt.previous_staff_id)
        {
            return Err(Verdict::Invalid);
        }
        Ok(receipt)
    }
}

impl RecoveryReceipt {
    fn check(fields: &mut Fields<'_>) -> Checked<Self> {
        fields.constant("record_type", "operations")?;
        fields.schema()?;
        let receipt = Self {
            event_id: fields.identifier("event_id")?,
            facility_id: fields.identifier("facility_id")?,
            component_id: fields.identifier("component_id")?,
            status: fields.one_of("status", &["degraded", "outage", "repair_started", "repaired", "restored"], None)?,
            minute: fields.count("minute")?,
            previous_working_units: fields.count("previous_working_units")?,
            working_units: fields.count("working_units")?,
            source_ids: fields.identifiers("source_ids")?,
            causation_id: fields.optional_identifier("causation_id")?,
        };
        // `recovery_record_changes_capacity`.
        if matches!(&*receipt.status, "degraded" | "outage" | "restored") && receipt.working_units == receipt.previous_working_units {
            return Err(Verdict::Invalid);
        }
        Ok(receipt)
    }
}

impl OperationReceipt {
    fn check(fields: &mut Fields<'_>) -> Checked<Self> {
        fields.constant("record_type", "operations")?;
        fields.schema()?;
        let receipt = Self {
            event_id: fields.identifier("event_id")?,
            task_id: fields.identifier("task_id")?,
            staff_id: fields.identifier("staff_id")?,
            task_kind: fields.one_of("task_kind", &TASK_KINDS, None)?,
            target_id: fields.optional_identifier("target_id")?,
            target_zone_id: fields.identifier("target_zone_id")?,
            reason: fields.bounded_text("reason", 240)?,
            status: fields.one_of("status", &["completed", "blocked"], None)?,
            minute: fields.count("minute")?,
            start_minute: fields.count("start_minute")?,
            arrival_minute: fields.count("arrival_minute")?,
            travel_minutes: fields.count("travel_minutes")?,
            outcome_observation: fields.one_of("outcome_observation", &OBSERVATIONS[1..], None)?,
            state_changes: fields.changes("state_changes", 256)?,
            total_state_change_count: fields.count("total_state_change_count")?,
            state_changes_truncated: fields.flag("state_changes_truncated")?,
            causation_id: fields.identifier("causation_id")?,
            source_ids: fields.identifiers("source_ids")?,
        };
        // `completion_evidence_is_consistent`.
        let sampled = receipt.state_changes.len() as i64;
        let total = receipt.total_state_change_count;
        let changed = any_change(&receipt.state_changes);
        let valid = receipt.start_minute <= receipt.arrival_minute
            && receipt.arrival_minute <= receipt.minute
            && receipt.travel_minutes <= receipt.arrival_minute - receipt.start_minute
            && total >= sampled
            && receipt.state_changes_truncated == (total > sampled)
            && if &*receipt.status == "blocked" {
                sampled == 0 && total == 0 && &*receipt.outcome_observation == "observed_no_change"
            } else {
                match &*receipt.outcome_observation {
                    "observed_change" => total != 0 && changed,
                    "observed_no_change" => total == 0 && !changed,
                    _ => sampled == 0 && total == 0,
                }
            };
        if !valid {
            return Err(Verdict::Invalid);
        }
        Ok(receipt)
    }
}

// ---- The records of the EventLog a receipt reads ------------------------------------------------------

/// A record of the EventLog, as `event_log.find` gives it.
#[derive(Clone, Debug)]
pub struct Logged {
    pub status: EventStatus,
    pub realized_minute: i64,
    pub event: EventRow,
}

/// The records a receipt's rule reads, by event ID (None: no such record), looked up by the PyO3
/// crate for `needed_records`.
#[derive(Default)]
pub struct Found(pub Vec<(Arc<str>, Option<Logged>)>);

impl Found {
    fn get(&self, event_id: &str) -> Result<Option<&Logged>> {
        match self.0.iter().find(|(id, _)| &**id == event_id) {
            Some((_, logged)) => Ok(logged.as_ref()),
            None => Err(Error::Runtime(format!("a record the plan did not look up: {event_id}"))),
        }
    }
}

/// The IDs of the records `record_shared_operation` reads for this receipt: its event, and for a
/// continuity receipt its cause and the event of the request's head, in that order.
pub fn needed_records(world: &World, receipt: &Receipt) -> Vec<Arc<str>> {
    let mut ids = vec![receipt.event_id().clone()];
    if let Receipt::Continuity(continuity) = receipt {
        ids.push(continuity.causation_id.clone());
        if let Ok(row) = world.row::<SharedOperationsRow>(NAME) {
            if let Some(head) = row.support_requests.map.get(&*continuity.request_id) {
                ids.push(head.latest_event_id.clone());
            }
        }
    }
    ids
}

// ---- record_shared_operation -----------------------------------------------------------------------------

/// The row of the namespace, to read and to check without marking it written.
fn with_row_unmarked<R>(world: &World, f: impl FnOnce(&mut SharedOperationsRow) -> Result<R>) -> Result<R> {
    let slot = world.slots.get(NAME).ok_or_else(|| Error::unsupported("the namespace is not owned"))?;
    let mut slot = slot.try_borrow_mut()?;
    f(slot.row_mut::<SharedOperationsRow>()?)
}

/// `SharedOperationsState.model_validate({**state, "history": recent})` of the world's minute:
/// `STATE_INVALID` when the contract turns it down, Unsupported when this check can't tell.
pub fn state_holds(world: &World, minute: i64) -> Result<()> {
    with_row_unmarked(world, |row| {
        if row.history.recent_verified(minute).is_none() {
            let start = row.history.recent_start(minute);
            for item in row.history.items.iter().skip(start) {
                match Receipt::check(&json_value(&item.bytes)?) {
                    Ok(_) => {}
                    Err(Verdict::Invalid) => return Err(Error::value_error(STATE_INVALID)),
                    Err(Verdict::Unsure) => return Err(Error::unsupported("a receipt only the Python contract can judge")),
                }
            }
        }
        if !row.header_is_valid() {
            return Err(Error::value_error(STATE_INVALID));
        }
        Ok(())
    })
}

/// Whether a receipt of this event is among the records since the previous tick, or among all of
/// them unless `recent`.
pub fn is_recorded(world: &World, event_id: &str, recent: bool, minute: i64) -> Result<bool> {
    let row = world.row::<SharedOperationsRow>(NAME)?;
    let start = if recent { row.history.recent_start(minute) } else { 0 };
    Ok(row.history.items.iter().skip(start).any(|item| item.event_id.as_deref() == Some(event_id)))
}

/// `record_shared_operation(world, receipt)`, of a world whose `model_state` holds the namespace's
/// tree (the PyO3 crate asks that first, with the Python rule's first error), with the records of
/// `needed_records`.
pub fn record_shared_operation(world: &mut World, receipt: &Receipt, found: &Found) -> Result<()> {
    match receipt {
        Receipt::Care(care) => record_care(world, care, found),
        other => record_other(world, other, found),
    }
}

/// The event of a receipt: applied at this minute, of the receipt's type.
fn applied_event<'a>(found: &'a Found, receipt: &Receipt, minute: i64) -> Result<&'a Logged> {
    match found.get(receipt.event_id())? {
        Some(logged) if logged.status == EventStatus::Applied && logged.realized_minute == minute && &*logged.event.event_type == receipt.event_type() => Ok(logged),
        _ => Err(Error::value_error(NO_EVENT)),
    }
}

fn record_care(world: &mut World, care: &CareReceipt, found: &Found) -> Result<()> {
    let world_minute = world.minute_index()?;
    state_holds(world, world_minute)?;
    if care.minute != world_minute {
        return Err(Error::value_error(MINUTE));
    }
    let receipt = Receipt::Care(care.clone());
    let document = receipt.document();
    let logged = applied_event(found, &receipt, care.minute)?;
    if is_recorded(world, &care.event_id, true, care.minute)? {
        return Err(Error::value_error(RECORDED));
    }
    let event = &logged.event;
    if event.payload != document
        || event.causation_id != care.causation_id
        || !event.source_id.as_ref().is_none_or(|source| care.source_ids.contains(source))
    {
        return Err(Error::value_error(NOT_THE_PAYLOAD));
    }
    let plan = plan_record(world, care)?;
    check_changes(world, &care.state_changes)?;
    write_record(world, care, &document, plan)
}

fn record_other(world: &mut World, receipt: &Receipt, found: &Found) -> Result<()> {
    let minute = world.minute_index()?;
    state_holds(world, minute)?;
    if receipt.minute() != minute {
        return Err(Error::value_error(MINUTE));
    }
    let logged = applied_event(found, receipt, minute)?;
    let event_id = receipt.event_id().clone();
    if is_recorded(world, &event_id, true, minute)? {
        return Err(Error::value_error(RECORDED));
    }
    let (over_limit, truncated, prior) = {
        let row = world.row::<SharedOperationsRow>(NAME)?;
        let prior = match receipt {
            Receipt::Continuity(continuity) => row.support_requests.map.get(&*continuity.request_id).cloned(),
            _ => None,
        };
        (row.history.items.len() + 1 > MAX_HISTORY, row.history_truncated, prior)
    };
    let common = receipt.document();
    // `_event_payload_matches`.
    let mut expected = common.clone();
    let event = &logged.event;
    if let (Receipt::Continuity(continuity), V::Map(payload), V::Map(expected)) = (receipt, &event.payload, &mut expected) {
        if !payload.contains_key("cause_type") && &*continuity.cause_type == "staff_unavailable" {
            expected.shift_remove("cause_type");
        }
    }
    if !py_eq(&event.payload, &expected)
        || event.causation_id.as_ref() != receipt.causation_id()
        || !event.source_id.as_ref().is_none_or(|source| receipt.source_ids().contains(source))
    {
        return Err(Error::value_error(NOT_THE_PAYLOAD));
    }
    // The head: Some(Some(new)) to set, Some(None) to drop.
    let mut new_head = None;
    match receipt {
        Receipt::Continuity(continuity) => {
            check_continuity(world, continuity, prior.as_ref(), found)?;
            new_head = Some(if &*continuity.status == "requeued" {
                let prior = prior.as_ref().ok_or_else(|| Error::Runtime("the head went".to_owned()))?;
                if prior.created_minute > minute {
                    return Err(Error::unsupported("the update precedes the creation"));
                }
                Some(CareHead {
                    request_id: continuity.request_id.clone(),
                    agent_id: continuity.agent_id.clone(),
                    household_id: prior.household_id.clone(),
                    support_kind: prior.support_kind.clone(),
                    status: CareStatus::Requested,
                    created_minute: prior.created_minute,
                    updated_minute: minute,
                    latest_event_id: event_id.clone(),
                })
            } else {
                None
            });
        }
        Receipt::Handoff(handoff) => check_handoff(world, handoff)?,
        Receipt::Recovery(recovery) => {
            let rows = world.rows()?;
            let fits = rows.facilities.get(&*recovery.facility_id).is_some_and(|facility| {
                recovery.working_units <= facility.capacity && facility.working_units == recovery.working_units
            });
            if !fits {
                return Err(Error::value_error("facility recovery receipt does not match the applied facility state"));
            }
        }
        Receipt::Operation(operation) => {
            check_staff_operation(world, operation, truncated)?;
            check_changes(world, &operation.state_changes)?;
        }
        Receipt::Care(_) => {}
    }

    // Write.
    let mut encoder = Encoder::with_capacity(512);
    common.write_json(&mut encoder)?;
    let record = Record {
        minute: Some(minute),
        event_id: Some(event_id.clone()),
        verified: false,
        reads_staff_history: matches!(receipt, Receipt::Operation(_)),
        bytes: encoder.out.into_boxed_slice(),
    };
    let row = world.slots.row_mut::<SharedOperationsRow>(NAME)?;
    if let (Receipt::Continuity(continuity), Some(head)) = (receipt, new_head) {
        let key = &continuity.request_id;
        match head {
            Some(head) => match row.support_requests.map.get_mut(&**key) {
                Some(slot) => *slot = head,
                None => {
                    row.support_requests.map.insert(key.clone(), head);
                }
            },
            None => {
                row.support_requests.map.shift_remove(&**key);
            }
        }
    }
    row.history.items.push_back(record);
    if over_limit {
        row.history_truncated = true;
        let count = row.history.items.len();
        if count > MAX_HISTORY {
            row.history.items.drain(..count - MAX_HISTORY);
        }
    }
    // Checked above as a tick's check would check it, so that check needs nothing more of it but
    // the staff completions, which it reads again (`state/shared_operations.rs`).
    row.history.appended_passed(&event_id);
    Ok(())
}

/// The branch of `StaffSupportContinuityReceipt`.
fn check_continuity(world: &World, receipt: &ContinuityReceipt, prior: Option<&CareHead>, found: &Found) -> Result<()> {
    let refused = || Err(Error::value_error("support continuity receipt does not match the interrupted request"));
    let Some(prior) = prior else {
        return refused();
    };
    let (agent, assigned, task, status) = {
        let rows = world.rows()?;
        let Some(request) = rows.support_requests.get(&*receipt.request_id) else {
            return refused();
        };
        let active = rows.active_tasks.contains_key(&receipt.task_id);
        (request.agent_id.clone(), request.assigned_staff_id.is_some(), request.task_id.is_some() || active, request.status)
    };
    let Some(cause) = found.get(&receipt.causation_id)? else {
        return refused();
    };
    let previous = V::Str(receipt.previous_staff_id.clone());
    let cause_of = |key: &str| match &cause.event.payload {
        V::Map(payload) => payload.get(key).cloned().unwrap_or(V::Null),
        _ => V::Null,
    };
    let event_type = &*cause.event.event_type;
    let cause_matches = match &*receipt.cause_type {
        "route_unreachable" => {
            cause.status == EventStatus::Suppressed
                && event_type == "agent_travel_complete"
                && py_eq(&cause_of("purpose"), &text("staff_support_travel"))
                && py_eq(&cause_of("request_id"), &V::Str(receipt.request_id.clone()))
                && py_eq(&cause_of("staff_id"), &previous)
        }
        "staff_unavailable" | "staff_shift_change" => {
            cause.status == EventStatus::Applied && event_type == &*receipt.cause_type && py_eq(&cause_of("staff_id"), &previous)
        }
        _ => false,
    };
    if !matches!(prior.status, CareStatus::Assigned | CareStatus::Started)
        || prior.agent_id != receipt.agent_id
        || agent != receipt.agent_id
        || assigned
        || task
        || !cause_matches
    {
        return refused();
    }
    let expected = if &*receipt.status == "requeued" { "pending" } else { "unresolved" };
    if status.text() != expected {
        return Err(Error::value_error("support continuity receipt status disagrees with the resident request"));
    }
    let same_staff = found.get(&prior.latest_event_id)?.is_some_and(|logged| match &logged.event.payload {
        V::Map(payload) => py_eq(payload.get("staff_id").unwrap_or(&V::Null), &previous),
        _ => false,
    });
    if !same_staff {
        return Err(Error::value_error("support continuity receipt does not match its previous staff assignment"));
    }
    Ok(())
}

/// The branch of `StaffHandoffReceipt`.
fn check_handoff(world: &World, receipt: &HandoffReceipt) -> Result<()> {
    let rows = world.rows()?;
    let task = rows.active_tasks.get(&*receipt.task_id);
    let holds = |wanted: Option<&Arc<str>>| task.is_some_and(|task| task.staff_id.as_ref() == wanted);
    let refused = match &*receipt.status {
        "reassigned" if !holds(receipt.replacement_staff_id.as_ref()) => "staff handoff receipt does not match the active task assignment",
        "unfilled" if task.is_none_or(|task| task.staff_id.is_some()) => "unfilled staff handoff must leave an active task unassigned",
        "rejected" if !holds(Some(&receipt.previous_staff_id)) => "rejected staff handoff must preserve the previous active assignment",
        _ => return Ok(()),
    };
    Err(Error::value_error(refused))
}

/// `_validate_staff_operation_receipt`, on the retained completions of `staff_operations_v1`.
fn check_staff_operation(world: &World, receipt: &OperationReceipt, history_truncated: bool) -> Result<()> {
    let operations = namespace::<StaffOperationsRow>(world, OPERATIONS)?;
    let history = operations.as_ref().map_or(&[][..], |row| &row.history[..]);
    let matches: Vec<_> = history
        .iter()
        .filter(|row| row.task_id == receipt.task_id && row.complete_minute == receipt.minute)
        .collect();
    let Some(row) = matches.first() else {
        // `history and all(...)`: an empty history has no later row.
        let all_later = !history.is_empty() && history.iter().all(|row| row.complete_minute > receipt.minute);
        if history_truncated && all_later {
            return Ok(());
        }
        return Err(Error::value_error("staff operation receipt has no matching retained completion"));
    };
    if matches.len() != 1 {
        return Err(Error::value_error("staff operation receipt matches duplicate completion history"));
    }
    let agrees = row.staff_id == receipt.staff_id
        && row.task_kind == receipt.task_kind
        && row.target_id == receipt.target_id
        && row.target_zone_id == receipt.target_zone_id
        && row.reason == receipt.reason
        && row.status.text() == &*receipt.status
        && row.start_minute == receipt.start_minute
        && row.arrival_minute == Some(receipt.arrival_minute)
        && row.travel_minutes == receipt.travel_minutes
        && row.causation_event_ids.contains(&receipt.causation_id);
    if !agrees {
        return Err(Error::value_error(DISAGREES));
    }
    Ok(())
}

// ---- What a state change reads now --------------------------------------------------------------------

/// The canonical JSON of a row, read back as a `V`.
fn row_value(row: &dyn Row) -> Result<V> {
    let mut encoder = Encoder::with_capacity(4096);
    row.write_json(&mut encoder)?;
    json_value(&encoder.out)
}

/// The value at `parts` under `value`, as `_validate_observed_state_changes` walks to it: a dict by
/// its keys; anything else has no such attribute.
fn walk<'a>(value: Option<&'a V>, parts: &[&str]) -> Option<&'a V> {
    let mut current = value;
    for part in parts {
        current = match current? {
            V::Map(map) => map.get(*part),
            _ => None,
        };
    }
    current
}

/// What the receipt's one state change reads now, as `_validate_observed_state_changes` walks to it.
fn observed(world: &World, kind: &str, entity: &str, field_path: &str) -> Result<Option<V>> {
    let parts: Vec<&str> = field_path.split('.').collect();
    let item = |map: &V| -> Result<Option<V>> {
        match map {
            V::Map(map) => Ok(map.get(entity).cloned()),
            _ => Err(Error::unsupported("a value is not a plain dict")),
        }
    };
    match kind {
        "model_state" => {
            let [namespace_name, collection, path @ ..] = parts.as_slice() else {
                return Err(Error::value_error("model_state change needs namespace, collection and field path"));
            };
            if path.is_empty() {
                return Err(Error::value_error("model_state change needs namespace, collection and field path"));
            }
            let Some(slot) = world.slots.get(namespace_name) else {
                return Ok(None);
            };
            let state = {
                let slot = slot.try_borrow()?;
                match (&slot.row, &slot.taint) {
                    (None, None) => return Ok(None),
                    (None, Some(_)) => return Err(Error::unsupported("the namespace has no row")),
                    (Some(row), _) => row_value(&**row)?,
                }
            };
            let V::Map(state) = &state else {
                return Ok(None);
            };
            let Some(items) = state.get(*collection) else {
                return Ok(None);
            };
            Ok(walk(item(items)?.as_ref(), path).cloned())
        }
        "resource" => {
            if parts != ["value"] {
                return Err(Error::value_error("resource state changes use field_path='value'"));
            }
            let resources = world.row::<crate::world::ResourcesRow>(RESOURCES)?;
            Ok(resources.get(entity).map(|value| V::Int(*value)))
        }
        "agent" | "staff" | "facility" | "zone" => {
            let version = world.schema_version()?;
            let rows = if kind == "zone" {
                let rows = world.entities.try_borrow()?;
                if rows.zones_tainted().is_some() {
                    return Err(Error::unsupported("the zone rows are tainted"));
                }
                rows
            } else {
                world.rows()?
            };
            let mut encoder = Encoder::with_capacity(512);
            let written = match kind {
                "agent" => rows.agents.get(entity).map(|row| crate::entities::EntityField::write_json(row, &mut encoder, version)),
                "staff" => rows.staff.get(entity).map(|row| crate::entities::EntityField::write_json(row, &mut encoder, version)),
                "facility" => rows.facilities.get(entity).map(|row| crate::entities::EntityField::write_json(row, &mut encoder, version)),
                _ => rows.zones.get(entity).map(|row| crate::entities::EntityField::write_json(row, &mut encoder, version)),
            };
            match written {
                None => Ok(None),
                Some(written) => {
                    written?;
                    let document = json_value(&encoder.out)?;
                    Ok(walk(Some(&document), &parts).cloned())
                }
            }
        }
        _ => Err(Error::unsupported("an unknown kind of entity")),
    }
}

/// `_validate_observed_state_changes`: each change's after-value is what the world holds now.
pub fn check_changes(world: &World, changes: &[Change]) -> Result<()> {
    for change in changes {
        let same = match observed(world, &change.entity_kind, &change.entity_id, &change.field_path)? {
            Some(V::Float(number)) => number == change.after,
            Some(V::Int(number)) => {
                if number.unsigned_abs() >= 1 << 53 {
                    return Err(Error::unsupported("a state change reads a large int"));
                }
                number as f64 == change.after
            }
            Some(V::Big(_)) => return Err(Error::unsupported("an int is too large")),
            _ => false,
        };
        if !same {
            return Err(Error::value_error(AFTER_VALUE));
        }
    }
    Ok(())
}

// ---- A care receipt a kernel appends ------------------------------------------------------------------

/// `_CARE_TRANSITIONS`.
fn transition_is_allowed(prior: Option<CareStatus>, next: CareStatus) -> bool {
    match prior {
        None => next == CareStatus::Requested,
        Some(CareStatus::Requested) => {
            matches!(next, CareStatus::Assigned | CareStatus::Started | CareStatus::Declined | CareStatus::Interrupted)
        }
        Some(CareStatus::Assigned) => matches!(next, CareStatus::Started | CareStatus::Declined | CareStatus::Interrupted),
        Some(CareStatus::Started) => matches!(next, CareStatus::Completed | CareStatus::Interrupted),
        Some(_) => false,
    }
}

/// What `plan_record` found: the request's first minute, and whether the history is full.
#[derive(Clone, Copy, Debug)]
pub struct PlannedRecord {
    created_minute: i64,
    overflow: bool,
}

/// The checks of `record_shared_operation` of a care receipt on the row, with the rule's texts;
/// nothing changes.
pub fn plan_record(world: &World, care: &CareReceipt) -> Result<PlannedRecord> {
    let (event_id, request_id, minute) = (&care.event_id, &care.request_id, care.minute);
    state_holds(world, minute)?;
    let first_request = if care.status == CareStatus::Requested {
        if !world.log_held {
            return Err(Error::unsupported("the event log is not in Rust"));
        }
        world.log()?.first_care_request_event_of(request_id).map(Arc::<str>::from)
    } else {
        None
    };
    let row = world.row::<SharedOperationsRow>(NAME)?;
    let start = row.history.recent_start(minute);
    if row.history.items.iter().skip(start).any(|item| item.event_id.as_deref() == Some(&**event_id)) {
        return Err(Error::value_error(RECORDED));
    }
    let prior = row.support_requests.map.get(&**request_id);
    if !transition_is_allowed(prior.map(|head| head.status), care.status) {
        return Err(Error::value_error("support lifecycle transition is out of order"));
    }
    if let Some(prior) = prior {
        if prior.agent_id != care.agent_id || prior.household_id != care.household_id || prior.support_kind != care.support_kind {
            return Err(Error::value_error("support request identity changed during its lifecycle"));
        }
    } else {
        if first_request.as_ref().is_some_and(|first| first != event_id) {
            return Err(Error::value_error("support request IDs cannot be reused across Run history"));
        }
        if row.support_requests.map.len() >= MAX_REQUESTS {
            return Err(Error::value_error("shared_operations_v1 support request capacity exceeded"));
        }
    }
    let created_minute = prior.map_or(minute, |head| head.created_minute);
    if minute < created_minute {
        return Err(Error::unsupported("the update precedes the creation"));
    }
    Ok(PlannedRecord { created_minute, overflow: row.history.items.len() + 1 > MAX_HISTORY })
}

/// The writes of `record_shared_operation` of a care receipt, on the row. `document` is its
/// `model_dump(mode="json")`. Nothing here fails on a valid state.
pub fn write_record(world: &mut World, care: &CareReceipt, document: &V, plan: PlannedRecord) -> Result<()> {
    let PlannedRecord { created_minute, overflow } = plan;
    let (event_id, request_id, minute) = (&care.event_id, &care.request_id, care.minute);
    let mut encoder = Encoder::with_capacity(512);
    document.write_json(&mut encoder)?;
    let receipt = Record {
        minute: Some(minute),
        event_id: Some(event_id.clone()),
        verified: true,
        reads_staff_history: false,
        bytes: encoder.out.into_boxed_slice(),
    };
    let new_head = care.status.is_open().then(|| CareHead {
        request_id: request_id.clone(),
        agent_id: care.agent_id.clone(),
        household_id: care.household_id.clone(),
        support_kind: care.support_kind.clone(),
        status: care.status,
        created_minute,
        updated_minute: minute,
        latest_event_id: event_id.clone(),
    });
    let row = world.slots.row_mut::<SharedOperationsRow>(NAME)?;
    match new_head {
        Some(head) => match row.support_requests.map.get_mut(&**request_id) {
            Some(slot) => *slot = head,
            None => {
                row.support_requests.map.insert(request_id.clone(), head);
            }
        },
        None => {
            row.support_requests.map.shift_remove(&**request_id);
        }
    }
    row.history.items.push_back(receipt);
    if overflow {
        row.history_truncated = true;
        row.history.items.pop_front();
    }
    Ok(())
}

// ---- The rule through the host (lane P9-1a) --------------------------------------------------------

/// The Python rule's error of a world without the namespace.
pub const NOT_SELECTED: &str = "shared_operations_v1 must be selected in the immutable Scenario";
/// The refusal of a namespace the world holds another dict of than the row's.
pub const OTHER_DOCUMENT: &str = "the world holds another document than the row's";

/// The Python rule's first check, as the host's `Holding` answers it: `model_state` holds the
/// namespace's tree (or its detached row). Another dict is refused; anything else is the rule's
/// `ValueError`.
pub fn check_held(holding: &Holding) -> Result<()> {
    if !holding.table {
        return Err(Error::unsupported(TABLE_NOT_PLAIN));
    }
    match holding.keys.get(NAME) {
        Some(key) if key.hold == Hold::Held => Ok(()),
        Some(key) if key.hold == Hold::NotOwned => Err(Error::unsupported(NOT_OWNED)),
        Some(key) if key.dict => Err(Error::unsupported(OTHER_DOCUMENT)),
        _ => Err(Error::value_error(NOT_SELECTED)),
    }
}

/// `record_shared_operation(world, receipt)` of a receipt read as the core's: the namespace's check
/// through the host's `Holding` (H1), the EventLog's records the receipt's rule reads looked up
/// through the host (`Host::find_record`), then the rule. Nothing of the world is borrowed while the
/// host runs.
pub fn record_through<C: WorldCell, H: Host>(cell: &C, host: &mut H, receipt: &Receipt) -> std::result::Result<(), H::Error> {
    let holding = kit::holding(cell, host)?;
    check_held(&holding)?;
    let ids = cell.with(|world| needed_records(world, receipt))?;
    let mut found = Vec::with_capacity(ids.len());
    for id in ids {
        let logged = host.find_record(&id)?;
        found.push((id, logged));
    }
    Ok(cell.with(|world| record_shared_operation(world, receipt, &Found(found)))??)
}
