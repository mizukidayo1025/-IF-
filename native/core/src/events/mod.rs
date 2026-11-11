//! Events without Python (docs/rust-port/lanes/e-events.md): `EventRow` is one `ScheduledEvent`,
//! `Queue` the pending events in the queue's order, `EventLog` the realized records with their
//! indexes, and `writer` what a rule does to schedule and record an event, with the errors of
//! `WorldState.schedule_event` and `EventLog.append`.
//!
//! A world's queue and log are parts of its `World` (`World::queue`, `World::log`, lane 6-5). The
//! PyO3 crate keeps the Python objects beside them, in side tables indexed the same way:
//! `QueueStore` (`native/src/state/events.rs`) by slot serial, `EventStore`
//! (`native/src/eventlog.rs`) by record position.

pub mod log;
pub mod queue;
pub mod row;
pub mod writer;

pub use row::EventRow;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    Exogenous,
    Completion,
    Endogenous,
    Intervention,
}

impl EventKind {
    pub const ALL: [EventKind; 4] =
        [EventKind::Exogenous, EventKind::Completion, EventKind::Endogenous, EventKind::Intervention];

    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::Exogenous => "exogenous",
            EventKind::Completion => "completion",
            EventKind::Endogenous => "endogenous",
            EventKind::Intervention => "intervention",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == text)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventStatus {
    Applied,
    Failed,
    Suppressed,
}

impl EventStatus {
    pub const ALL: [EventStatus; 3] = [EventStatus::Applied, EventStatus::Failed, EventStatus::Suppressed];

    pub fn as_str(self) -> &'static str {
        match self {
            EventStatus::Applied => "applied",
            EventStatus::Failed => "failed",
            EventStatus::Suppressed => "suppressed",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|status| status.as_str() == text)
    }
}

/// `models._STABLE_ID.fullmatch(value)`: `[A-Za-z0-9][A-Za-z0-9._:-]{0,63}`.
pub fn is_stable_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && bytes[0].is_ascii_alphanumeric()
        && bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}
