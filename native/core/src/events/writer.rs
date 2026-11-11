//! What a rule does to schedule and record an event over rows: `WorldState.schedule_event` and
//! `EventLogRecord(...)` with `EventLog.append`, with their errors and in their order of checks.
//! The event counter (`id_counters["event"]`) is still a Python dict, so the PyO3 crate's
//! `EventWriter` (`native/src/state/events.rs`) reads and writes it around these.

use std::cell::Cell;
use std::fmt::Display;

use super::log::EventLog;
use super::queue::Queue;
use super::{EventRow, EventStatus};
use crate::error::Error;

/// The largest event counter a rule starts from, so that adding its few sequences to it cannot
/// leave an i64, as Python's int can. A rule's planning stage adds to it without a check. Beyond
/// this is Python's, whose int goes on.
pub const MAX_RULE_COUNTER: i64 = 1 << 62;

/// What a write refuses: a core error, or `DuplicateEventError(text)` of `events/queue.py`,
/// which is not one of the core's classes.
#[derive(Clone, Debug, PartialEq)]
pub enum WriteError {
    Error(Error),
    Duplicate(String),
}

impl From<Error> for WriteError {
    fn from(error: Error) -> Self {
        WriteError::Error(error)
    }
}

pub type WriteResult<T> = std::result::Result<T, WriteError>;

/// Sequences a plan has promised to `allocate_reserved` before what it plans next.
#[derive(Debug, Default)]
pub struct Reserved(Cell<i64>);

impl Reserved {
    /// What `allocate_sequence` would return from this counter, past the sequences reserved.
    pub fn after(&self, counter: i64) -> i64 {
        counter + self.0.get()
    }

    /// Sets how many sequences will be taken before what is planned from now on.
    pub fn set(&self, count: usize) {
        self.0.set(count as i64);
    }

    /// How many sequences are reserved now.
    pub fn count(&self) -> usize {
        self.0.get().max(0) as usize
    }

    /// One reserved sequence is taken.
    pub fn take_one(&self) {
        self.0.set((self.0.get() - 1).max(0));
    }
}

/// The error `EventLog.append` raises for a refusal code of `EventLog::add`, if any.
pub fn realized(code: u8, event_id: &str, sequence: impl Display) -> WriteResult<()> {
    match code {
        1 => Err(WriteError::Duplicate(format!("event '{event_id}' already realized"))),
        2 => Err(WriteError::Duplicate(format!("event sequence {sequence} already realized"))),
        _ => Ok(()),
    }
}

/// `WorldState.schedule_event` of a row, with its errors: the queue gets it, and the event counter
/// must then be at least the value returned.
pub fn schedule(queue: &mut Queue, log: &EventLog, duration: i64, row: EventRow) -> WriteResult<i64> {
    let Some(after) = row.sequence.checked_add(1) else {
        return Err(Error::unsupported("the counter after this sequence does not fit an i64").into());
    };
    if row.due_minute >= duration {
        return Err(Error::Value(format!("event '{}' is outside the scenario", row.event_id)).into());
    }
    realized(log.refusal(&row.event_id, row.sequence), &row.event_id, row.sequence)?;
    let (event_id, sequence) = (row.event_id.clone(), row.sequence);
    match queue.schedule_row(row) {
        1 => Err(WriteError::Duplicate(format!("duplicate event ID '{event_id}'"))),
        2 => Err(WriteError::Duplicate(format!("duplicate event sequence {sequence}"))),
        _ => Ok(after),
    }
}

/// `EventLogRecord(...)` and `EventLog.append` of an outcome, with their errors.
pub fn record(
    log: &mut EventLog,
    event: &EventRow,
    realized_minute: i64,
    status: EventStatus,
    detail: Option<&str>,
) -> WriteResult<()> {
    // EventLogRecord refuses this before EventLog.append sees it, and the log could not restore it.
    if realized_minute < 0 {
        return Err(Error::Value("realized_minute must be a non-negative integer".into()).into());
    }
    let code = log.append_row(event, realized_minute, status, detail)?;
    realized(code, &event.event_id, event.sequence)
}
