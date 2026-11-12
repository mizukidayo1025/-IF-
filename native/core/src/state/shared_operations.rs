//! `shared_operations_v1`: the receipts of care, staff and facility transitions, and the head of
//! each open care request (`domain/common_operations.py`). `rules/common_operations.rs` appends a
//! care receipt; the other receipts come through the forwarded writes of the Python rule
//! (docs/rust-port/lanes/m4-staff.md).
//!
//! A receipt is its canonical JSON, with what the rules read of it. `verified` says it was checked
//! against its contract and its event: by the kernel that appended it, or by the Python rules of a
//! check that passed. A tick's check then needs nothing more of it, but the retained staff
//! completions for a staff operation receipt. A receipt restored or written past the kernels isn't,
//! until the Python rules pass it.
//!
//! The PyO3 crate's `state/shared_operations.rs` reads it from Python and holds the check of a tick.

use std::collections::VecDeque;
use std::sync::Arc;

use crate::error::{Misfit, Result};
use crate::json::Encoder;
use crate::row::{value_bytes, Field, IdMap, Items};
use crate::value::V;
use crate::{state_row, str_enum};

pub const NAME: &str = "shared_operations_v1";
pub const POLICY: &str = "shared-operations-v1";
pub const MAX_HISTORY: usize = 20_000;
pub const MAX_REQUESTS: usize = 100_000;
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

str_enum! {
    pub enum CareStatus {
        Requested = "requested",
        Assigned = "assigned",
        Started = "started",
        Completed = "completed",
        Interrupted = "interrupted",
        Declined = "declined",
    }
}

impl CareStatus {
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "requested" => Self::Requested,
            "assigned" => Self::Assigned,
            "started" => Self::Started,
            "completed" => Self::Completed,
            "interrupted" => Self::Interrupted,
            "declined" => Self::Declined,
            _ => return None,
        })
    }

    /// Whether a head may hold this status.
    pub fn is_open(self) -> bool {
        matches!(self, Self::Requested | Self::Assigned | Self::Started)
    }
}

state_row! {
    pub struct CareHead {
        request_id: Arc<str>,
        agent_id: Arc<str>,
        household_id: Option<Arc<str>>,
        support_kind: Arc<str>,
        status: CareStatus,
        created_minute: i64,
        updated_minute: i64,
        latest_event_id: Arc<str>,
    }
}

/// The contract's `Identifier`: `^[A-Za-z0-9][A-Za-z0-9._:-]{0,63}$`.
pub fn is_identifier(text: &str) -> bool {
    let bytes = text.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && bytes[0].is_ascii_alphanumeric()
        && bytes.iter().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

/// The contract's `SupportKind`.
pub fn is_support_kind(text: &str) -> bool {
    SUPPORT_KINDS.contains(&text)
}

impl CareHead {
    /// What `CareRequestState` and `SharedOperationsState` ask of a head under this key.
    fn is_valid(&self, key: &str) -> bool {
        &*self.request_id == key
            && is_identifier(&self.request_id)
            && is_identifier(&self.agent_id)
            && self.household_id.as_deref().is_none_or(is_identifier)
            && SUPPORT_KINDS.contains(&&*self.support_kind)
            && self.status.is_open()
            && self.created_minute >= 0
            && self.updated_minute >= self.created_minute
            && is_identifier(&self.latest_event_id)
    }
}

/// The heads, and whether a forwarded write reached them since they last passed.
#[derive(Clone, Default)]
pub struct Heads {
    pub map: IdMap<CareHead>,
    unchecked: bool,
}

impl Heads {
    /// Heads read from a tree, which `are_valid` reads before it trusts them.
    pub fn unchecked(map: IdMap<CareHead>) -> Self {
        Self { map, unchecked: true }
    }

    /// A forwarded write reached the heads.
    pub fn mark_unchecked(&mut self) {
        self.unchecked = true;
    }

    /// Whether every head is one the contract takes. Reads them only after a forwarded write.
    pub fn are_valid(&mut self) -> bool {
        if self.unchecked {
            if self.map.len() > MAX_REQUESTS || !self.map.iter().all(|(key, head)| head.is_valid(key)) {
                return false;
            }
            self.unchecked = false;
        }
        true
    }
}

impl Field for Heads {
    fn write_json(&self, encoder: &mut Encoder) -> Result<()> {
        self.map.write_json(encoder)
    }

    fn from_value(value: &V) -> std::result::Result<Self, Misfit> {
        Ok(Self::unchecked(Field::from_value(value)?))
    }

    /// The keys sorted; heads that passed stay passed.
    fn canonicalize(&mut self) {
        self.map.canonicalize();
    }
}

/// One receipt of the history.
#[derive(Clone)]
pub struct Receipt {
    pub minute: Option<i64>,
    pub event_id: Option<Arc<str>>,
    pub verified: bool,
    /// Its rule also reads the retained `staff_operations_v1.history`, so a tick's check reads that
    /// again as long as it is recent (`staff_operations::completion_agrees`).
    pub reads_staff_history: bool,
    pub bytes: Box<[u8]>,
}

/// A deque because at its limit each new receipt drops the oldest. A write into a receipt doesn't
/// fit, and taints the namespace.
#[derive(Clone, Default)]
pub struct Receipts {
    pub items: VecDeque<Receipt>,
}

impl Receipts {
    /// Where `records_since_previous_tick` starts.
    /// `validate_shared_operations_state` of this tick passed, reading these.
    pub fn recent_passed(&mut self, minute_index: i64) {
        let start = self.recent_start(minute_index);
        for item in self.items.iter_mut().skip(start) {
            item.verified = true;
        }
    }

    /// The rules have just replayed the whole history and passed it.
    pub fn all_passed(&mut self) {
        for item in self.items.iter_mut() {
            item.verified = true;
        }
    }

    /// The receipt of this event, which the kernel that appended it has checked.
    pub fn appended_passed(&mut self, event_id: &str) {
        if let Some(item) = self.items.iter_mut().rev().find(|item| item.event_id.as_deref() == Some(event_id)) {
            item.verified = true;
        }
    }

    pub fn recent_start(&self, minute_index: i64) -> usize {
        let mut start = self.items.len();
        while start > 0 {
            if self.items[start - 1].minute.is_some_and(|minute| minute < minute_index - 1) {
                break;
            }
            start -= 1;
        }
        start.saturating_sub(1)
    }

    /// The receipts of `records_since_previous_tick`, if the kernel checked each and they are in order.
    pub fn recent_verified(&self, minute_index: i64) -> Option<impl Iterator<Item = &Receipt>> {
        let start = self.recent_start(minute_index);
        let recent = || self.items.iter().skip(start);
        let mut last = None;
        for item in recent() {
            let (true, Some(minute), Some(_)) = (item.verified, item.minute, &item.event_id) else {
                return None;
            };
            if last.is_some_and(|last| last > minute) || minute > minute_index {
                return None;
            }
            last = Some(minute);
        }
        Some(recent())
    }
}

impl Receipt {
    /// A receipt read from its value: its bytes, and what a tick's checks read of it if it is a
    /// record that has them (an int minute, a str event ID, a str event type), as the PyO3 crate
    /// reads one from Python. A new receipt is not verified.
    pub fn from_value(value: &V) -> std::result::Result<Self, Misfit> {
        let mut receipt =
            Receipt { minute: None, event_id: None, verified: false, reads_staff_history: true, bytes: value_bytes(value)? };
        if let V::Map(record) = value {
            if let Some(V::Int(minute)) = record.get("minute") {
                receipt.minute = Some(*minute);
            }
            if let Some(V::Str(event_id)) = record.get("event_id") {
                receipt.event_id = Some(event_id.clone());
            }
            if let Some(V::Str(event_type)) = record.get("event_type") {
                receipt.reads_staff_history = &**event_type == "staff_operation_transition";
            }
        }
        Ok(receipt)
    }
}

impl Items for Receipts {
    fn count(&self) -> usize {
        self.items.len()
    }

    fn item_json<'a>(&'a self, index: usize, _scratch: &'a mut Encoder) -> Result<&'a [u8]> {
        Ok(&self.items[index].bytes)
    }
}

impl Field for Receipts {
    fn items(&self) -> Option<&dyn Items> {
        Some(self)
    }

    /// Each receipt is its canonical JSON already, and a list keeps its order.
    fn canonicalize(&mut self) {}

    fn from_value(value: &V) -> std::result::Result<Self, Misfit> {
        let V::List(items) = value else {
            return Err(Misfit::new("not a list"));
        };
        Ok(Self { items: items.iter().map(Receipt::from_value).collect::<std::result::Result<_, _>>()? })
    }

    fn write_json(&self, encoder: &mut Encoder) -> Result<()> {
        encoder.out.push(b'[');
        for (index, item) in self.items.iter().enumerate() {
            if index > 0 {
                encoder.out.push(b',');
            }
            encoder.out.extend_from_slice(&item.bytes);
        }
        encoder.out.push(b']');
        Ok(())
    }
}

state_row! {
    pub struct SharedOperationsRow {
        schema_version: i64,
        policy_version: Arc<str>,
        support_requests: Heads,
        history: Receipts,
        history_truncated: bool,
    }
}

impl SharedOperationsRow {
    /// The part of `SharedOperationsState` that doesn't read the history.
    pub fn header_is_valid(&mut self) -> bool {
        self.schema_version == 1 && &*self.policy_version == POLICY && self.support_requests.are_valid()
    }
}

