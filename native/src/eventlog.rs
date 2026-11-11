//! The realized EventLog behind `sheltersim.events.queue.EventLog` (docs/rust-port.md, stage B1).
//!
//! The records' index fields, their bytes written here and the indexes are
//! `sheltersim_core::events::log::EventLog`. `EventStore` keeps beside it, at the same positions,
//! the Python objects of each record: its `EventLogRecord`, and its bytes object once Python asked
//! for the bytes or the record was read back from a saved Checkpoint. A position the core added
//! with no object yet (a record a Rust rule appended) has none in `handles`, which can be shorter
//! than the log. The record object is made by the store's factory when Python reads it.
//!
//! When Python first asks for the bytes of a record written here, they move from the core into one
//! Python bytes object, which the save's caches then know by identity: the same object for as long
//! as the record lives.

use std::collections::HashSet;

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList, PyString, PyTuple};

pub(crate) use sheltersim_core::events::log::{EventLog, Fields, ACTION_LIFECYCLE_EVENT_TYPE, EVENT_FIELDS, RECORD_FIELDS};

use crate::core::errors::OrPy;
use crate::state::events::{EventRow, EventStatus};

/// The Python objects of one record, at the same position in `EventStore::handles`.
#[derive(Default)]
struct Handles {
    record: Option<Py<PyAny>>,
    bytes: Option<Py<PyBytes>>,
}

#[pyclass(module = "sheltersim_native")]
pub struct EventStore {
    log: EventLog,
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
        Self {
            log: EventLog::new(),
            handles: Vec::new(),
            rebuilt: 0,
            encoder: None,
            factory: None,
            types: None,
        }
    }

    fn handles(&self, index: usize) -> Option<&Handles> {
        self.handles.get(index)
    }

    fn handles_mut(&mut self, index: usize) -> &mut Handles {
        if self.handles.len() <= index {
            self.handles.resize_with(index + 1, Handles::default);
        }
        &mut self.handles[index]
    }

    fn record_at(&mut self, py: Python<'_>, index: usize) -> PyResult<Py<PyAny>> {
        if let Some(record) = self.handles(index).and_then(|handles| handles.record.as_ref()) {
            return Ok(record.clone_ref(py));
        }
        let bytes = self.bytes_at(py, index)?;
        let factory = self
            .factory
            .as_ref()
            .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("no record factory"))?;
        let record = factory.call1(py, (bytes,))?;
        self.handles_mut(index).record = Some(record.clone_ref(py));
        self.rebuilt += 1;
        Ok(record)
    }

    fn bytes_at(&mut self, py: Python<'_>, index: usize) -> PyResult<Py<PyBytes>> {
        if let Some(bytes) = self.handles(index).and_then(|handles| handles.bytes.as_ref()) {
            return Ok(bytes.clone_ref(py));
        }
        let bytes: Py<PyBytes> = if let Some(written) = self.log.take_written(index) {
            PyBytes::new(py, &written).unbind()
        } else {
            let record = self.handles(index).and_then(|handles| handles.record.as_ref()).unwrap().clone_ref(py);
            let encoder = self
                .encoder
                .as_ref()
                .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("no record encoder"))?;
            encoder.call1(py, (record,))?.extract(py)?
        };
        self.handles_mut(index).bytes = Some(bytes.clone_ref(py));
        Ok(bytes)
    }

    /// `EventLog::add`, keeping the objects at the new position.
    fn add(&mut self, fields: Fields, handles: Handles, written: Option<Box<[u8]>>) -> u8 {
        let index = self.log.len();
        let refused = self.log.add(fields, handles.bytes.is_some(), written);
        if refused == 0 && (handles.record.is_some() || handles.bytes.is_some()) {
            *self.handles_mut(index) = handles;
        }
        refused
    }

    fn records_of(
        &mut self,
        py: Python<'_>,
        indices: impl Iterator<Item = usize>,
    ) -> PyResult<Vec<Py<PyAny>>> {
        indices.map(|index| self.record_at(py, index)).collect()
    }

    /// The distinct ids of the str event types an iterable gives, in the order given.
    fn type_ids_of(&mut self, event_types: &Bound<'_, PyAny>, scan: bool) -> PyResult<Vec<u32>> {
        let mut ids: Vec<u32> = Vec::new();
        for name in event_types.try_iter()? {
            let name = name?;
            let Ok(text) = name.downcast::<PyString>() else {
                continue;
            };
            let text = text.to_str()?;
            let id = if scan { self.log.scan_type(text) } else { self.log.type_id(text) };
            if let Some(id) = id {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
        Ok(ids)
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
        &mut self,
        encoder: Py<PyAny>,
        factory: Py<PyAny>,
        record_type: Option<Py<PyAny>>,
        event_type: Option<Py<PyAny>>,
    ) {
        self.encoder = Some(encoder);
        self.factory = Some(factory);
        self.types = record_type.zip(event_type);
    }

    /// Append one EventLogRecord: 0, or 1 for a realized ID, 2 for a realized sequence.
    fn append(&mut self, py: Python<'_>, record: &Bound<'_, PyAny>) -> PyResult<u8> {
        let fields = fields_of_record(record)?;
        let refused = self.log.refusal(&fields.event_id, fields.sequence);
        if refused != 0 {
            return Ok(refused);
        }
        let written = match &self.types {
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
        Ok(self.add(fields, handles, written))
    }

    /// Whether every record has bytes written or checked here, so restoring this log gives
    /// the log itself.
    fn all_encoded(&self) -> bool {
        self.log.all_encoded()
    }

    /// Append records saved as canonical JSON bytes (event_log documents of a
    /// Checkpoint), in order: 0, or the code of the first refused one.
    fn extend_saved(&mut self, py: Python<'_>, items: &Bound<'_, PyAny>) -> PyResult<u8> {
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
            let refused = self.add(fields, Handles { record: None, bytes: Some(bytes) }, None);
            if refused != 0 {
                return Ok(refused);
            }
        }
        Ok(0)
    }

    /// The canonical JSON bytes of the records from start to end.
    fn items_bytes(&mut self, py: Python<'_>, start: usize, end: usize) -> PyResult<Vec<Py<PyBytes>>> {
        let end = end.min(self.log.len());
        (start.min(end)..end).map(|index| self.bytes_at(py, index)).collect()
    }

    /// How many records have no object yet (read back and not read since).
    fn unread(&self) -> usize {
        self.log.len() - self.handles.iter().filter(|handles| handles.record.is_some()).count()
    }

    /// Let go of the objects of the records before `end` whose bytes were written or read
    /// back here; a read makes each again from its bytes, equal to the one let go. The
    /// bytes stay. Records of the types `of_types` was asked for keep their objects: their
    /// readers read them all again. Returns how many objects were let go (#138).
    fn release_records(&mut self, end: usize) -> usize {
        let mut released = 0;
        for index in self.log.releasable(end) {
            if let Some(handles) = self.handles.get_mut(index) {
                if handles.record.is_some() {
                    handles.record = None;
                    released += 1;
                }
            }
        }
        released
    }

    /// How many record objects the factory has made from bytes.
    #[getter]
    fn rebuilt(&self) -> usize {
        self.rebuilt
    }

    fn __len__(&self) -> usize {
        self.log.len()
    }

    fn contains_id(&self, event_id: &str) -> bool {
        self.log.has_event_id(event_id)
    }

    fn contains_sequence(&self, sequence: i64) -> bool {
        self.log.has_sequence(sequence)
    }

    #[getter]
    fn max_sequence(&self) -> i64 {
        self.log.max_sequence()
    }

    fn find(&mut self, py: Python<'_>, event_id: &Bound<'_, PyAny>) -> PyResult<Option<Py<PyAny>>> {
        let Ok(text) = event_id.downcast::<PyString>() else {
            return Ok(None);
        };
        match self.log.index_of(text.to_str()?) {
            Some(index) => Ok(Some(self.record_at(py, index)?)),
            None => Ok(None),
        }
    }

    fn first_care_request_event(&self, request_id: &str) -> Option<String> {
        self.log.first_care_request_event_of(request_id).map(str::to_owned)
    }

    fn completed_support_kinds(&self, agent_id: &str) -> Vec<String> {
        self.log.completed_kinds(agent_id).to_vec()
    }

    fn applied_facility_event_ids(&self, facility_id: &str) -> Vec<String> {
        self.log.applied_facility_event_ids(facility_id).to_vec()
    }

    fn at_minute(&mut self, py: Python<'_>, minute: i64) -> PyResult<Vec<Py<PyAny>>> {
        let indices = self.log.at_minute(minute).to_vec();
        self.records_of(py, indices.into_iter())
    }

    fn event_types(&self) -> Vec<String> {
        self.log.event_types()
    }

    fn with_provenance_object(&mut self, py: Python<'_>, key: &str) -> PyResult<Vec<Py<PyAny>>> {
        let indices = self.log.with_provenance_object(key).to_vec();
        self.records_of(py, indices.into_iter())
    }

    fn of_types(&mut self, py: Python<'_>, event_types: &Bound<'_, PyAny>) -> PyResult<Vec<Py<PyAny>>> {
        let ids = self.type_ids_of(event_types, true)?;
        let indices = self.log.of_type_ids_from(&ids, 0);
        self.records_of(py, indices.into_iter())
    }

    /// The agent_action_lifecycle records that decide each agent's active action, in
    /// the order a pass over all of them inserts the agents into a dict: a pass over
    /// just these gives the same dict, without making the other records' objects.
    fn active_lifecycle_records(&mut self, py: Python<'_>) -> PyResult<Vec<Py<PyAny>>> {
        let indices = self.log.active_lifecycle_positions();
        self.records_of(py, indices.into_iter())
    }

    /// The records of event_types at position start or later, in append order.
    fn of_types_from(
        &mut self,
        py: Python<'_>,
        event_types: &Bound<'_, PyAny>,
        start: usize,
    ) -> PyResult<Vec<Py<PyAny>>> {
        let ids = self.type_ids_of(event_types, false)?;
        let indices = self.log.of_type_ids_from(&ids, start);
        self.records_of(py, indices.into_iter())
    }

    /// The records from start on, in append order.
    #[pyo3(signature = (start=0))]
    fn records_from(&mut self, py: Python<'_>, start: usize) -> PyResult<Vec<Py<PyAny>>> {
        let start = start.min(self.log.len());
        let end = self.log.len();
        self.records_of(py, start..end)
    }

    fn last(&mut self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        match self.log.len() {
            0 => Ok(None),
            length => Ok(Some(self.record_at(py, length - 1)?)),
        }
    }

    fn event_ids<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        PyList::new(py, self.log.event_ids())
    }

    /// The event IDs of `at_minute(minute)`, making no record object.
    fn event_ids_at_minute<'py>(&self, py: Python<'py>, minute: i64) -> PyResult<Bound<'py, PyList>> {
        PyList::new(py, self.log.event_ids_at(minute))
    }

    fn recent(&mut self, py: Python<'_>, limit: usize) -> PyResult<Vec<Py<PyAny>>> {
        let end = self.log.len();
        let start = end.saturating_sub(limit);
        self.records_of(py, start..end)
    }

    /// Up to limit applied records of event_types from the end, scanning at most
    /// scan_limit records, in append order; and whether records were left unscanned.
    fn recent_matching(
        &mut self,
        py: Python<'_>,
        event_types: &Bound<'_, PyAny>,
        limit: usize,
        scan_limit: usize,
    ) -> PyResult<(Vec<Py<PyAny>>, bool)> {
        let wanted: HashSet<u32> = self.type_ids_of(event_types, false)?.into_iter().collect();
        let (found, more) = self.log.recent_matching(&wanted, limit, scan_limit);
        Ok((self.records_of(py, found.into_iter())?, more))
    }

    /// Audit realization times: 0, 3 for a future realization, 4 for one outside the
    /// duration. IDs and sequences are unique by construction. Records an earlier
    /// audit checked at an earlier or the same minute and duration are not checked
    /// again.
    #[pyo3(signature = (minute_index=None, duration_minutes=None))]
    fn audit(&mut self, minute_index: Option<i64>, duration_minutes: Option<i64>) -> u8 {
        self.log.audit(minute_index, duration_minutes)
    }

    /// An independent store sharing the frozen records.
    fn copy(&self, py: Python<'_>) -> Self {
        Self {
            log: self.log.clone(),
            handles: self
                .handles
                .iter()
                .map(|handles| Handles {
                    record: handles.record.as_ref().map(|record| record.clone_ref(py)),
                    bytes: handles.bytes.as_ref().map(|bytes| bytes.clone_ref(py)),
                })
                .collect(),
            rebuilt: self.rebuilt,
            encoder: self.encoder.as_ref().map(|encoder| encoder.clone_ref(py)),
            factory: self.factory.as_ref().map(|factory| factory.clone_ref(py)),
            types: self
                .types
                .as_ref()
                .map(|(record, event)| (record.clone_ref(py), event.clone_ref(py))),
        }
    }

    /// The indexes' sizes, for tests that compare them with the records.
    fn index_sizes<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let (records, ids, sequences) = self.log.index_sizes();
        let sizes = PyDict::new(py);
        sizes.set_item("records", records)?;
        sizes.set_item("ids", ids)?;
        sizes.set_item("sequences", sequences)?;
        Ok(sizes)
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
    /// The core log, for the writer of `state/events.rs`.
    pub(crate) fn core_mut(&mut self) -> &mut EventLog {
        &mut self.log
    }

    pub(crate) fn core(&self) -> &EventLog {
        &self.log
    }

    /// `record = log.find(event_id)`, `record.status is APPLIED` and `record.event is event`.
    pub(crate) fn applied_outcome(&self, event_id: &str, event: &Bound<'_, PyAny>) -> PyResult<AppliedOutcome> {
        let py = event.py();
        let Some(index) = self.log.index_of(event_id) else {
            return Ok(AppliedOutcome::No);
        };
        if !self.log.is_applied(index) {
            return Ok(AppliedOutcome::No);
        }
        if let Some(record) = self.handles(index).and_then(|handles| handles.record.as_ref()) {
            if record.bind(py).getattr(intern!(py, "event"))?.is(event) {
                return Ok(AppliedOutcome::ThisEvent);
            }
        }
        Ok(match self.saved_document(py, index) {
            Some((row, ..)) => AppliedOutcome::SavedRow(row),
            None => AppliedOutcome::AnotherObject,
        })
    }

    /// `EventLog.append` of an outcome of an event object, with no record object: its bytes come
    /// from the same writer as a record's, and Python makes the record when it reads one. 3, with
    /// nothing changed, for a value the writer refuses, which needs Python's record and encoder.
    pub(crate) fn append_outcome(
        &mut self,
        event: &Bound<'_, PyAny>,
        realized_minute: i64,
        status: EventStatus,
        detail: Option<&str>,
    ) -> PyResult<u8> {
        let py = event.py();
        let Some((record_type, event_type)) = &self.types else {
            return Ok(3);
        };
        if !event.get_type().is(event_type.bind(py)) || record_type.is_none(py) {
            return Ok(3);
        }
        let fields = fields_of_outcome(event, realized_minute, status == EventStatus::Applied)?;
        let refused = self.log.refusal(&fields.event_id, fields.sequence);
        if refused != 0 {
            return Ok(refused);
        }
        let detail = match detail {
            Some(detail) => PyString::new(py, detail).into_any(),
            None => py.None().into_bound(py),
        };
        let written = crate::checkpoint::encode_outcome(
            event,
            realized_minute.into_pyobject(py)?.as_any(),
            PyString::new(py, status.as_str()).as_any(),
            &detail,
        )
        .unwrap_or(None);
        let Some(written) = written else {
            return Ok(3);
        };
        Ok(self.add(fields, Handles::default(), Some(written.into_boxed_slice())))
    }

    /// The support kinds with applied completion records for one person.
    pub(crate) fn completed_kinds(&self, agent_id: &str) -> &[String] {
        self.log.completed_kinds(agent_id)
    }

    /// The event of a request's first care record, if it has one.
    pub(crate) fn first_care_request_event_of(&self, request_id: &str) -> Option<&str> {
        self.log.first_care_request_event_of(request_id)
    }

    pub(crate) fn max_realized_sequence(&self) -> i64 {
        self.log.max_sequence()
    }

    /// The core's log, for a core function that reads it (`sheltersim_core::rules`).
    pub(crate) fn log(&self) -> &EventLog {
        &self.log
    }

    pub(crate) fn has_event_id(&self, event_id: &str) -> bool {
        self.log.has_event_id(event_id)
    }

    pub(crate) fn has_sequence(&self, sequence: i64) -> bool {
        self.log.has_sequence(sequence)
    }

    /// The positions of a type's records in append order; empty for a type with none.
    pub(crate) fn positions_of_type(&self, event_type: &str) -> &[usize] {
        self.log.positions_of_type(event_type)
    }

    /// The saved document of the record at a position: `(event, realized_minute, status, detail)`,
    /// read from its bytes. None for a record whose bytes only Python can make.
    pub(crate) fn saved_document(
        &self,
        py: Python<'_>,
        index: usize,
    ) -> Option<(EventRow, i64, EventStatus, Option<std::sync::Arc<str>>)> {
        if index >= self.log.len() {
            return None;
        }
        let bytes = match (self.log.written(index), self.handles(index).and_then(|handles| handles.bytes.as_ref())) {
            (Some(written), _) => written,
            (None, Some(bytes)) => bytes.as_bytes(py),
            (None, None) => return None,
        };
        EventRow::from_saved_record(bytes)
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
