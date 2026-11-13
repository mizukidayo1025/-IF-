//! The realized EventLog behind `sheltersim.events.queue.EventLog` (docs/rust-port.md, stage B1).
//!
//! The records' index fields, their bytes written here and the indexes are
//! `sheltersim_core::events::log::EventLog`: the store's own, or, while the store is a world's, the
//! world's (`World::log`, lane 6-5), which the store reaches through the world's core. `EventStore`
//! is a handle that keeps beside it, at the same positions, the Python objects of each record: its
//! `EventLogRecord`, and its bytes object once Python asked for the bytes or the record was read back
//! from a saved Checkpoint. A position the core added with no object yet (a record a Rust rule
//! appended) has none in `handles`, which can be shorter than the log. The record object is made by
//! the store's factory when Python reads it.
//!
//! When Python first asks for the bytes of a record written here, they move from the core into one
//! Python bytes object, which the save's caches then know by identity: the same object for as long
//! as the record lives (policy E9).

use std::collections::HashSet;

use pyo3::gc::PyVisit;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::PyTraverseError;
use pyo3::types::{PyBytes, PyDict, PyList, PyString, PyTuple};
use sheltersim_core::cell::{CowShard, Shard, ShardMut};

pub(crate) use sheltersim_core::events::log::{EventLog, Fields, ACTION_LIFECYCLE_EVENT_TYPE, EVENT_FIELDS, RECORD_FIELDS};

use crate::core::errors::OrPy;
use crate::state::events::{home_clear, home_traverse, share_part, with_part, with_part_mut, EventRow, EventStatus, Home};

/// The Python objects of one record, at the same position in `LogSide::handles`.
#[derive(Default)]
struct Handles {
    record: Option<Py<PyAny>>,
    bytes: Option<Py<PyBytes>>,
}

/// What the store keeps beside the log: the Python objects by position, and the codecs.
#[derive(Default)]
struct LogSide {
    /// By position; a position past the end has no object.
    handles: Vec<Handles>,
    // How many record objects the factory made from bytes (`release_records`).
    rebuilt: usize,
    // record -> canonical bytes, and canonical bytes -> record (set from Python).
    encoder: Option<Py<PyAny>>,
    factory: Option<Py<PyAny>>,
    // The exact types whose records `checkpoint::encode_record` may write.
    types: Option<(Py<PyAny>, Py<PyAny>)>,
}

impl LogSide {
    fn handles_mut(&mut self, index: usize) -> &mut Handles {
        if self.handles.len() <= index {
            self.handles.resize_with(index + 1, Handles::default);
        }
        &mut self.handles[index]
    }
}

#[pyclass(frozen, module = "sheltersim_native")]
pub struct EventStore {
    home: Shard<Home<EventLog>>,
    side: Shard<LogSide>,
}

fn mapping_get<'py>(
    mapping: &Bound<'py, PyAny>,
    key: &Bound<'py, PyString>,
) -> PyResult<Option<Bound<'py, PyAny>>> {
    let value = mapping.call_method1(intern!(mapping.py(), "get"), (key,))?;
    Ok(if value.is_none() { None } else { Some(value) })
}

fn as_str(value: &Option<Bound<'_, PyAny>>) -> Option<String> {
    value
        .as_ref()
        .and_then(|item| item.downcast::<PyString>().ok())
        .and_then(|text| text.to_str().ok().map(str::to_owned))
}

impl EventStore {
    fn empty() -> Self {
        Self { home: Home::own(EventLog::new()), side: Shard::default() }
    }

    /// Where the log is, for `WorldCore::attach_events` and `release`.
    pub(crate) fn home(&self) -> &Shard<Home<EventLog>> {
        &self.home
    }

    #[inline]
    fn side(&self) -> PyResult<ShardMut<'_, LogSide>> {
        self.side.try_borrow_mut().or_py()
    }

    /// Run f on the log, borrowed shared. f must not run Python code.
    #[inline]
    pub(crate) fn with_log<R>(&self, py: Python<'_>, f: impl FnOnce(&EventLog) -> R) -> PyResult<R> {
        with_part(&self.home, py, f)
    }

    /// `with_log`, borrowed mutably.
    #[inline]
    pub(crate) fn with_log_mut<R>(&self, py: Python<'_>, f: impl FnOnce(&mut EventLog) -> R) -> PyResult<R> {
        with_part_mut(&self.home, py, f)
    }

    fn record_at(&self, py: Python<'_>, index: usize) -> PyResult<Py<PyAny>> {
        if let Some(record) = self.side.try_borrow().or_py()?.handles.get(index).and_then(|handles| handles.record.as_ref()) {
            return Ok(record.clone_ref(py));
        }
        let bytes = self.bytes_at(py, index)?;
        let factory = self
            .side
            .try_borrow()
            .or_py()?
            .factory
            .as_ref()
            .map(|factory| factory.clone_ref(py))
            .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("no record factory"))?;
        let record = factory.call1(py, (bytes,))?;
        let mut side = self.side()?;
        side.handles_mut(index).record = Some(record.clone_ref(py));
        side.rebuilt += 1;
        Ok(record)
    }

    fn bytes_at(&self, py: Python<'_>, index: usize) -> PyResult<Py<PyBytes>> {
        if let Some(bytes) = self.side.try_borrow().or_py()?.handles.get(index).and_then(|handles| handles.bytes.as_ref()) {
            return Ok(bytes.clone_ref(py));
        }
        let bytes: Py<PyBytes> = if let Some(written) = self.with_log_mut(py, |log| log.take_written(index))? {
            PyBytes::new(py, &written).unbind()
        } else {
            let (record, encoder) = {
                let side = self.side.try_borrow().or_py()?;
                let record = side.handles.get(index).and_then(|handles| handles.record.as_ref()).unwrap().clone_ref(py);
                let encoder = side.encoder.as_ref().map(|encoder| encoder.clone_ref(py));
                (record, encoder)
            };
            let encoder = encoder.ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("no record encoder"))?;
            encoder.call1(py, (record,))?.extract(py)?
        };
        self.side()?.handles_mut(index).bytes = Some(bytes.clone_ref(py));
        Ok(bytes)
    }

    /// `EventLog::add`, keeping the objects at the new position.
    fn add(&self, py: Python<'_>, fields: Fields, handles: Handles, written: Option<Box<[u8]>>) -> PyResult<u8> {
        let has_bytes = handles.bytes.is_some();
        let (index, refused) = self.with_log_mut(py, |log| (log.len(), log.add(fields, has_bytes, written)))?;
        if refused == 0 && (handles.record.is_some() || handles.bytes.is_some()) {
            *self.side()?.handles_mut(index) = handles;
        }
        Ok(refused)
    }

    fn records_of(
        &self,
        py: Python<'_>,
        indices: impl Iterator<Item = usize>,
    ) -> PyResult<Vec<Py<PyAny>>> {
        indices.map(|index| self.record_at(py, index)).collect()
    }

    /// The distinct ids of the str event types an iterable gives, in the order given.
    fn type_ids_of(&self, event_types: &Bound<'_, PyAny>, scan: bool) -> PyResult<Vec<u32>> {
        let py = event_types.py();
        let mut ids: Vec<u32> = Vec::new();
        for name in event_types.try_iter()? {
            let name = name?;
            let Ok(text) = name.downcast::<PyString>() else {
                continue;
            };
            let text = text.to_str()?;
            let id = if scan { self.with_log_mut(py, |log| log.scan_type(text))? } else { self.with_log(py, |log| log.type_id(text))? };
            if let Some(id) = id {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
        Ok(ids)
    }

    /// The transaction of the world's core rolled back, and the core cut the log at its mark
    /// (`World::restore`): the objects of the records it took out go too.
    pub(crate) fn cut_to_log(&self, py: Python<'_>) -> PyResult<()> {
        let len = self.with_log(py, EventLog::len)?;
        let mut side = self.side()?;
        let gone: Vec<Handles> = if side.handles.len() > len { side.handles.split_off(len) } else { Vec::new() };
        drop(side);
        drop(gone);
        Ok(())
    }
}

#[pymethods]
impl EventStore {
    #[new]
    fn new() -> Self {
        Self::empty()
    }

    /// The encoder (record -> canonical JSON bytes), the factory (bytes -> record), and the
    /// only types of record and event whose bytes are written on appending.
    #[pyo3(signature = (encoder, factory, record_type=None, event_type=None))]
    fn set_codecs(
        &self,
        encoder: Py<PyAny>,
        factory: Py<PyAny>,
        record_type: Option<Py<PyAny>>,
        event_type: Option<Py<PyAny>>,
    ) -> PyResult<()> {
        let mut side = self.side()?;
        let old = (
            side.encoder.replace(encoder),
            side.factory.replace(factory),
            std::mem::replace(&mut side.types, record_type.zip(event_type)),
        );
        drop(side);
        drop(old);
        Ok(())
    }

    /// Append one EventLogRecord: 0, or 1 for a realized ID, 2 for a realized sequence.
    fn append(&self, py: Python<'_>, record: &Bound<'_, PyAny>) -> PyResult<u8> {
        let fields = fields_of_record(record)?;
        let refused = self.with_log(py, |log| log.refusal(&fields.event_id, fields.sequence))?;
        if refused != 0 {
            return Ok(refused);
        }
        let types = self.side.try_borrow().or_py()?.types.as_ref().map(|(record, event)| (record.clone_ref(py), event.clone_ref(py)));
        let written = match &types {
            // Anything this can not write waits for the Python encoder, which raises its own
            // error at the save, as it always did.
            Some((record_type, event_type)) => {
                crate::checkpoint::encode_record(record, record_type.bind(py), event_type.bind(py))
                    .unwrap_or(None)
                    .map(Vec::into_boxed_slice)
            }
            None => None,
        };
        let handles = Handles { record: Some(record.clone().unbind()), bytes: None };
        self.add(py, fields, handles, written)
    }

    /// Whether every record has bytes written or checked here, so restoring this log gives
    /// the log itself.
    fn all_encoded(&self, py: Python<'_>) -> PyResult<bool> {
        self.with_log(py, EventLog::all_encoded)
    }

    /// Append records saved as canonical JSON bytes (event_log documents of a
    /// Checkpoint), in order: 0, or the code of the first refused one.
    fn extend_saved(&self, py: Python<'_>, items: &Bound<'_, PyAny>) -> PyResult<u8> {
        let mut saved: Vec<Py<PyBytes>> = Vec::new();
        for item in items.try_iter()? {
            saved.push(item?.downcast_into::<PyBytes>()?.unbind());
        }
        // Records are parsed and checked in parallel without the GIL (the bytes are
        // immutable and held here), then added in order, as one by one.
        let slices: Vec<&[u8]> = saved.iter().map(|bytes| bytes.as_bytes(py)).collect();
        let parsed: Vec<Result<Fields, String>> = py.allow_threads(|| {
            use rayon::prelude::*;
            slices
                .par_iter()
                .map(|bytes| sheltersim_core::events::log::fields_of_bytes(bytes))
                .collect()
        });
        for (fields, bytes) in parsed.into_iter().zip(saved) {
            let fields = fields.map_err(pyo3::exceptions::PyValueError::new_err)?;
            let refused = self.add(py, fields, Handles { record: None, bytes: Some(bytes) }, None)?;
            if refused != 0 {
                return Ok(refused);
            }
        }
        Ok(0)
    }

    /// The canonical JSON bytes of the records from start to end.
    fn items_bytes(&self, py: Python<'_>, start: usize, end: usize) -> PyResult<Vec<Py<PyBytes>>> {
        let end = end.min(self.with_log(py, EventLog::len)?);
        (start.min(end)..end).map(|index| self.bytes_at(py, index)).collect()
    }

    /// How many records have no object yet (read back and not read since).
    fn unread(&self, py: Python<'_>) -> PyResult<usize> {
        let len = self.with_log(py, EventLog::len)?;
        Ok(len - self.side.try_borrow().or_py()?.handles.iter().filter(|handles| handles.record.is_some()).count())
    }

    /// Let go of the objects of the records before `end` whose bytes were written or read
    /// back here; a read makes each again from its bytes, equal to the one let go. The
    /// bytes stay. Records of the types `of_types` was asked for keep their objects: their
    /// readers read them all again. Returns how many objects were let go (#138).
    fn release_records(&self, py: Python<'_>, end: usize) -> PyResult<usize> {
        let releasable: Vec<usize> = self.with_log(py, |log| log.releasable(end).collect())?;
        let mut released = Vec::new();
        let mut side = self.side()?;
        for index in releasable {
            if let Some(handles) = side.handles.get_mut(index) {
                if let Some(record) = handles.record.take() {
                    released.push(record);
                }
            }
        }
        drop(side);
        let count = released.len();
        drop(released);
        Ok(count)
    }

    /// How many record objects the factory has made from bytes.
    #[getter]
    fn rebuilt(&self) -> PyResult<usize> {
        Ok(self.side.try_borrow().or_py()?.rebuilt)
    }

    fn __len__(&self, py: Python<'_>) -> PyResult<usize> {
        self.with_log(py, EventLog::len)
    }

    fn contains_id(&self, py: Python<'_>, event_id: &str) -> PyResult<bool> {
        self.with_log(py, |log| log.has_event_id(event_id))
    }

    fn contains_sequence(&self, py: Python<'_>, sequence: i64) -> PyResult<bool> {
        self.with_log(py, |log| log.has_sequence(sequence))
    }

    #[getter]
    fn max_sequence(&self, py: Python<'_>) -> PyResult<i64> {
        self.with_log(py, EventLog::max_sequence)
    }

    fn find(&self, py: Python<'_>, event_id: &Bound<'_, PyAny>) -> PyResult<Option<Py<PyAny>>> {
        let Ok(text) = event_id.downcast::<PyString>() else {
            return Ok(None);
        };
        let text = text.to_str()?;
        match self.with_log(py, |log| log.index_of(text))? {
            Some(index) => Ok(Some(self.record_at(py, index)?)),
            None => Ok(None),
        }
    }

    fn first_care_request_event(&self, py: Python<'_>, request_id: &str) -> PyResult<Option<String>> {
        self.first_care_request_event_of(py, request_id)
    }

    fn completed_support_kinds(&self, py: Python<'_>, agent_id: &str) -> PyResult<Vec<String>> {
        self.completed_kinds(py, agent_id)
    }

    fn applied_facility_event_ids(&self, py: Python<'_>, facility_id: &str) -> PyResult<Vec<String>> {
        self.with_log(py, |log| log.applied_facility_event_ids(facility_id).to_vec())
    }

    fn at_minute(&self, py: Python<'_>, minute: i64) -> PyResult<Vec<Py<PyAny>>> {
        let indices = self.with_log(py, |log| log.at_minute(minute).to_vec())?;
        self.records_of(py, indices.into_iter())
    }

    fn event_types(&self, py: Python<'_>) -> PyResult<Vec<String>> {
        self.with_log(py, EventLog::event_types)
    }

    fn with_provenance_object(&self, py: Python<'_>, key: &str) -> PyResult<Vec<Py<PyAny>>> {
        let indices = self.with_log(py, |log| log.with_provenance_object(key).to_vec())?;
        self.records_of(py, indices.into_iter())
    }

    fn of_types(&self, py: Python<'_>, event_types: &Bound<'_, PyAny>) -> PyResult<Vec<Py<PyAny>>> {
        let ids = self.type_ids_of(event_types, true)?;
        let indices = self.with_log(py, |log| log.of_type_ids_from(&ids, 0))?;
        self.records_of(py, indices.into_iter())
    }

    /// The agent_action_lifecycle records that decide each agent's active action, in
    /// the order a pass over all of them inserts the agents into a dict: a pass over
    /// just these gives the same dict, without making the other records' objects.
    fn active_lifecycle_records(&self, py: Python<'_>) -> PyResult<Vec<Py<PyAny>>> {
        let indices = self.with_log(py, EventLog::active_lifecycle_positions)?;
        self.records_of(py, indices.into_iter())
    }

    /// The records of event_types at position start or later, in append order.
    fn of_types_from(
        &self,
        py: Python<'_>,
        event_types: &Bound<'_, PyAny>,
        start: usize,
    ) -> PyResult<Vec<Py<PyAny>>> {
        let ids = self.type_ids_of(event_types, false)?;
        let indices = self.with_log(py, |log| log.of_type_ids_from(&ids, start))?;
        self.records_of(py, indices.into_iter())
    }

    /// The records from start on, in append order.
    #[pyo3(signature = (start=0))]
    fn records_from(&self, py: Python<'_>, start: usize) -> PyResult<Vec<Py<PyAny>>> {
        let end = self.with_log(py, EventLog::len)?;
        let start = start.min(end);
        self.records_of(py, start..end)
    }

    fn last(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        match self.with_log(py, EventLog::len)? {
            0 => Ok(None),
            length => Ok(Some(self.record_at(py, length - 1)?)),
        }
    }

    fn event_ids<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        self.with_log(py, |log| PyList::new(py, log.event_ids()))?
    }

    /// The event IDs of `at_minute(minute)`, making no record object.
    fn event_ids_at_minute<'py>(&self, py: Python<'py>, minute: i64) -> PyResult<Bound<'py, PyList>> {
        self.with_log(py, |log| PyList::new(py, log.event_ids_at(minute)))?
    }

    fn recent(&self, py: Python<'_>, limit: usize) -> PyResult<Vec<Py<PyAny>>> {
        let end = self.with_log(py, EventLog::len)?;
        let start = end.saturating_sub(limit);
        self.records_of(py, start..end)
    }

    /// Up to limit applied records of event_types from the end, scanning at most
    /// scan_limit records, in append order; and whether records were left unscanned.
    fn recent_matching(
        &self,
        py: Python<'_>,
        event_types: &Bound<'_, PyAny>,
        limit: usize,
        scan_limit: usize,
    ) -> PyResult<(Vec<Py<PyAny>>, bool)> {
        let wanted: HashSet<u32> = self.type_ids_of(event_types, false)?.into_iter().collect();
        let (found, more) = self.with_log(py, |log| log.recent_matching(&wanted, limit, scan_limit))?;
        Ok((self.records_of(py, found.into_iter())?, more))
    }

    /// Audit realization times: 0, 3 for a future realization, 4 for one outside the
    /// duration. IDs and sequences are unique by construction. Records an earlier
    /// audit checked at an earlier or the same minute and duration are not checked
    /// again.
    #[pyo3(signature = (minute_index=None, duration_minutes=None))]
    fn audit(&self, py: Python<'_>, minute_index: Option<i64>, duration_minutes: Option<i64>) -> PyResult<u8> {
        self.with_log_mut(py, |log| log.audit(minute_index, duration_minutes))
    }

    /// An independent store sharing the frozen records, which belongs to no world. The log itself
    /// is shared until either writes it.
    fn copy(&self, py: Python<'_>) -> PyResult<Self> {
        let log = share_part(&self.home, py)?;
        let side = self.side.try_borrow().or_py()?;
        Ok(Self {
            home: Shard::new(Home::Own(CowShard::from_shared(log))),
            side: Shard::new(LogSide {
                handles: side
                    .handles
                    .iter()
                    .map(|handles| Handles {
                        record: handles.record.as_ref().map(|record| record.clone_ref(py)),
                        bytes: handles.bytes.as_ref().map(|bytes| bytes.clone_ref(py)),
                    })
                    .collect(),
                rebuilt: side.rebuilt,
                encoder: side.encoder.as_ref().map(|encoder| encoder.clone_ref(py)),
                factory: side.factory.as_ref().map(|factory| factory.clone_ref(py)),
                types: side.types.as_ref().map(|(record, event)| (record.clone_ref(py), event.clone_ref(py))),
            }),
        })
    }

    /// The indexes' sizes, for tests that compare them with the records.
    fn index_sizes<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let (records, ids, sequences) = self.with_log(py, EventLog::index_sizes)?;
        let sizes = PyDict::new(py);
        sizes.set_item("records", records)?;
        sizes.set_item("ids", ids)?;
        sizes.set_item("sequences", sequences)?;
        Ok(sizes)
    }

    /// Whether the log is a world's (its store attached to a core).
    #[getter]
    fn attached(&self, py: Python<'_>) -> PyResult<bool> {
        Ok(self.home.try_borrow().or_py()?.core(py).is_some())
    }

    fn __traverse__(&self, visit: PyVisit<'_>) -> Result<(), PyTraverseError> {
        home_traverse(&self.home, &visit)
    }

    fn __clear__(&self) {
        home_clear(&self.home);
    }
}

/// How the log holds an event a rule asks about (`EventStore::applied_outcome`).
pub(crate) enum AppliedOutcome {
    /// Not realized, or realized but not applied.
    No,
    /// Applied, and the record holds this very object.
    ThisEvent,
    /// Applied, in a record that holds another object of the same ID, with no bytes to read.
    AnotherObject,
    /// Applied, in a record of another object or none yet: the event its bytes hold.
    SavedRow(EventRow),
}

/// What a Rust rule does with the log: no Python record is made or read.
impl EventStore {
    /// `record = log.find(event_id)`, `record.status is APPLIED` and `record.event is event`.
    pub(crate) fn applied_outcome(&self, event_id: &str, event: &Bound<'_, PyAny>) -> PyResult<AppliedOutcome> {
        let py = event.py();
        let found = self.with_log(py, |log| log.index_of(event_id).map(|index| (index, log.is_applied(index))))?;
        let Some((index, true)) = found else {
            return Ok(AppliedOutcome::No);
        };
        let record = self.side.try_borrow().or_py()?.handles.get(index).and_then(|handles| handles.record.as_ref().map(|record| record.clone_ref(py)));
        if let Some(record) = record {
            if record.bind(py).getattr(intern!(py, "event"))?.is(event) {
                return Ok(AppliedOutcome::ThisEvent);
            }
        }
        Ok(match self.saved_document(py, index)? {
            Some((row, ..)) => AppliedOutcome::SavedRow(row),
            None => AppliedOutcome::AnotherObject,
        })
    }

    /// The support kinds with applied completion records for one person.
    pub(crate) fn completed_kinds(&self, py: Python<'_>, agent_id: &str) -> PyResult<Vec<String>> {
        self.with_log(py, |log| log.completed_kinds(agent_id).to_vec())
    }

    /// The event of a request's first care record, if it has one.
    pub(crate) fn first_care_request_event_of(&self, py: Python<'_>, request_id: &str) -> PyResult<Option<String>> {
        self.with_log(py, |log| log.first_care_request_event_of(request_id).map(str::to_owned))
    }

    pub(crate) fn has_event_id(&self, py: Python<'_>, event_id: &str) -> PyResult<bool> {
        self.with_log(py, |log| log.has_event_id(event_id))
    }

    /// The positions of a type's records in append order; empty for a type with none.
    pub(crate) fn positions_of_type(&self, py: Python<'_>, event_type: &str) -> PyResult<Vec<usize>> {
        self.with_log(py, |log| log.positions_of_type(event_type).to_vec())
    }

    /// The saved document of the record at a position: `(event, realized_minute, status, detail)`,
    /// read from its bytes. None for a record whose bytes only Python can make.
    pub(crate) fn saved_document(
        &self,
        py: Python<'_>,
        index: usize,
    ) -> PyResult<Option<(EventRow, i64, EventStatus, Option<std::sync::Arc<str>>)>> {
        let side = self.side.try_borrow().or_py()?;
        self.with_log(py, |log| {
            if index >= log.len() {
                return None;
            }
            let bytes = match (log.written(index), side.handles.get(index).and_then(|handles| handles.bytes.as_ref())) {
                (Some(written), _) => written,
                (None, Some(bytes)) => bytes.as_bytes(py),
                (None, None) => return None,
            };
            EventRow::from_saved_record(bytes)
        })
    }
}

fn fields_of_record(record: &Bound<'_, PyAny>) -> PyResult<Fields> {
    let py = record.py();
    let event = record.getattr(intern!(py, "event"))?;
    let status: String = record.getattr(intern!(py, "status"))?.getattr(intern!(py, "_value_"))?.extract()?;
    let minute = record.getattr(intern!(py, "realized_minute"))?.extract()?;
    fields_of_outcome(&event, minute, status == "applied")
}

fn fields_of_outcome(event: &Bound<'_, PyAny>, minute: i64, applied: bool) -> PyResult<Fields> {
    let py = event.py();
    let payload = event.getattr(intern!(py, "payload"))?;
    let mut provenance_objects = Vec::new();
    // isinstance(value, collections.abc.Mapping), as the Python EventLog checks. Nearly every
    // value is a str, which the costly check would only reject.
    let mut mapping_type = None;
    for item in event.getattr(intern!(py, "provenance"))?.call_method0(intern!(py, "items"))?.try_iter()? {
        let item = item?;
        let pair = item.downcast::<PyTuple>()?;
        let value = pair.get_item(1)?;
        if value.is_exact_instance_of::<PyString>() {
            continue;
        }
        if mapping_type.is_none() {
            mapping_type = Some(py.import("collections.abc")?.getattr(intern!(py, "Mapping"))?);
        }
        if value.is_instance(mapping_type.as_ref().expect("set above"))? {
            provenance_objects.push(pair.get_item(0)?.extract()?);
        }
    }
    let event_type: String = event.getattr(intern!(py, "event_type"))?.extract()?;
    let mut fields = Fields {
        event_id: event.getattr(intern!(py, "event_id"))?.extract()?,
        sequence: event.getattr(intern!(py, "sequence"))?.extract()?,
        minute,
        applied,
        request_id: None,
        agent_id: None,
        support_kind: None,
        payload_status_completed: false,
        facility_id: None,
        provenance_objects,
        transition: None,
        event_type,
    };
    // `EventLog::add` reads the payload of these types only.
    match fields.event_type.as_str() {
        "care_support_transition" => {
            fields.request_id = as_str(&mapping_get(&payload, intern!(py, "request_id"))?);
            fields.agent_id = as_str(&mapping_get(&payload, intern!(py, "agent_id"))?);
            fields.support_kind = as_str(&mapping_get(&payload, intern!(py, "support_kind"))?);
            fields.payload_status_completed = mapping_get(&payload, intern!(py, "status"))?
                .map(|value| value.eq("completed"))
                .transpose()?
                .unwrap_or(false);
        }
        "facility_outage" | "facility_recovery" => {
            fields.facility_id = as_str(&mapping_get(&payload, intern!(py, "facility_id"))?);
        }
        ACTION_LIFECYCLE_EVENT_TYPE => {
            fields.agent_id = as_str(&mapping_get(&payload, intern!(py, "agent_id"))?);
            fields.transition = as_str(&mapping_get(&payload, intern!(py, "transition"))?);
        }
        _ => {}
    }
    Ok(fields)
}

/// The field names a saved record and its event must have, for a test against
/// the Python models.
#[pyfunction]
pub fn saved_record_fields() -> (Vec<&'static str>, Vec<&'static str>) {
    (RECORD_FIELDS.to_vec(), EVENT_FIELDS.to_vec())
}

/// The bytes of each item of a JSON array, split where JSON does: at commas outside
/// strings and nested values. raw must be one canonical JSON array.
#[pyfunction]
pub fn split_json_array<'py>(py: Python<'py>, raw: &[u8]) -> PyResult<Vec<Bound<'py, PyBytes>>> {
    let items = sheltersim_core::checkpoint::split_json_array(raw).or_py()?;
    Ok(items.into_iter().map(|item| PyBytes::new(py, item)).collect())
}

// ---- The EventLog's Python records, as the core reads them (lane P10-1c) ------------------------------

/// `event_log.find(event_id)` of each ID, as the core reads a record (`Host::find_record`).
/// The canonical JSON of the world's log's record at each position (`bytes_at`: the bytes the core
/// wrote, or Python's), given to `each`: the native host's `read_records` (#264).
pub(crate) fn read_records(
    world: &Bound<'_, PyAny>,
    positions: &[usize],
    each: &mut dyn FnMut(usize, &[u8]) -> sheltersim_core::error::Result<()>,
) -> PyResult<()> {
    let py = world.py();
    let log = crate::core::row::attribute(world, intern!(py, "event_log"))?;
    let store = log.getattr(intern!(py, "_store"))?;
    let store = store.downcast::<EventStore>()?.get();
    for &index in positions {
        let bytes = store.bytes_at(py, index)?;
        each(index, bytes.as_bytes(py)).or_py()?;
    }
    Ok(())
}

pub(crate) fn found_records(world: &Bound<'_, PyAny>, ids: Vec<std::sync::Arc<str>>) -> PyResult<sheltersim_core::rules::common_operations::Found> {
    let py = world.py();
    let log = crate::core::row::attribute(world, intern!(py, "event_log"))?;
    let mut found = Vec::with_capacity(ids.len());
    for id in ids {
        let record = log.call_method1(intern!(py, "find"), (&*id,))?;
        let logged = if record.is_none() { None } else { Some(logged_of(&record)?) };
        found.push((id, logged));
    }
    Ok(sheltersim_core::rules::common_operations::Found(found))
}

/// An `EventLogRecord` as the core reads it: its status, minute and event's row.
pub(crate) fn logged_of(record: &Bound<'_, PyAny>) -> PyResult<sheltersim_core::rules::common_operations::Logged> {
    use crate::core::row::{attribute, exact_int, exact_str};
    use crate::state::events::EventRowPy;
    let py = record.py();
    let status = attribute(record, intern!(py, "status"))?.getattr(intern!(py, "_value_"))?;
    let Some(status) = EventStatus::parse(exact_str(&status)?) else {
        return Err(crate::Unsupported::new_err("a record of another status"));
    };
    let realized_minute = exact_int(&attribute(record, intern!(py, "realized_minute"))?)?;
    let Some(event) = EventRow::from_event(&attribute(record, intern!(py, "event"))?)? else {
        return Err(crate::Unsupported::new_err("the receipt's event has no row"));
    };
    Ok(sheltersim_core::rules::common_operations::Logged { status, realized_minute, event })
}
