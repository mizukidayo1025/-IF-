//! The future EventQueue, and how a Rust rule schedules and records an event without a Python
//! object (docs/rust-port/lanes/e-events.md).
//!
//! `EventRow`, the queue's order and slots, and the checks and texts of scheduling and recording
//! are `sheltersim_core::events`. This file keeps what needs Python: a row read from a
//! `ScheduledEvent` and made into one (`EventRowPy`), and the Python objects of the pending events.
//! `QueueStore` holds a core `Queue` and, by the same slot serial, the objects. A slot has a row, a
//! Python object, or both: an event scheduled from Python has its object and gets its row when a
//! rule first reads it, and an event scheduled from Rust has its row and gets its object when
//! Python first reads it. That object is a `ScheduledEvent` like any other, so handlers need not
//! know.
//!
//! `EventWriter` is what a rule uses. It does what `WorldState.allocate_event_sequence`,
//! `WorldState.schedule_event` and `EventLog.append` do, with their errors. `EventWriter::of` says
//! `Unsupported` while any part of the world is not Rust's, so a rule asks for it in its planning
//! stage, before it changes anything.

use std::collections::HashMap;
use std::sync::Arc;

use indexmap::IndexMap;
use pyo3::exceptions::{PyKeyError, PyValueError};
use pyo3::ffi;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::sync::GILOnceCell;
use pyo3::types::{PyBool, PyBytes, PyDict, PyFloat, PyInt, PyString, PyTuple};

pub(crate) use sheltersim_core::events::queue::{Key, Queue};
pub(crate) use sheltersim_core::events::writer::{Reserved, WriteError, MAX_RULE_COUNTER};
pub(crate) use sheltersim_core::events::{is_stable_id, EventKind, EventRow, EventStatus};

use crate::core::errors::{to_py_err, OrPy};
use crate::core::value::{V, VPy};
use crate::core::world_core::NamespaceMirror;
use crate::eventlog::EventStore;
use crate::state::world_fields;
use crate::Unsupported;

// As `checkpoint::MAX_SAME_DEPTH`: deeper values are left to the Python paths.
const MAX_ROW_DEPTH: usize = 64;

/// The classes a row becomes, read once.
struct Models {
    event: Py<PyAny>,
    kinds: [Py<PyAny>; 4],
}

static MODELS: GILOnceCell<Models> = GILOnceCell::new();

fn models(py: Python<'_>) -> PyResult<&'static Models> {
    MODELS.get_or_try_init(py, || {
        let module = py.import("sheltersim.events.models")?;
        let kind = module.getattr("EventKind")?;
        let member = |item: EventKind| -> PyResult<Py<PyAny>> { Ok(kind.call1((item.as_str(),))?.unbind()) };
        Ok(Models {
            event: module.getattr("ScheduledEvent")?.unbind(),
            kinds: [
                member(EventKind::Exogenous)?,
                member(EventKind::Completion)?,
                member(EventKind::Endogenous)?,
                member(EventKind::Intervention)?,
            ],
        })
    })
}

/// An `EventRow` read from a `ScheduledEvent` and made into one.
pub(crate) trait EventRowPy: Sized {
    /// The row of a `ScheduledEvent`, or None for an event of another type, or with a value that
    /// is not an exact JSON scalar, tuple or mapping proxy, such as a str subclass in a payload.
    fn from_event(event: &Bound<'_, PyAny>) -> PyResult<Option<Self>>;

    /// The `ScheduledEvent` of this row. Values are frozen as `_freeze_json` freezes them.
    fn to_py<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>>;
}

impl EventRowPy for EventRow {
    fn from_event(event: &Bound<'_, PyAny>) -> PyResult<Option<Self>> {
        let py = event.py();
        if !event.get_type().is(models(py)?.event.bind(py)) {
            return Ok(None);
        }
        let text = |value: Bound<'_, PyAny>| -> Option<Arc<str>> {
            value.downcast_exact::<PyString>().ok()?.to_str().ok().map(Arc::from)
        };
        let integer = |value: Bound<'_, PyAny>| -> Option<i64> {
            value.downcast_exact::<PyInt>().ok()?.extract().ok()
        };
        let optional = |value: Bound<'_, PyAny>| -> Option<Option<Arc<str>>> {
            if value.is_none() {
                Some(None)
            } else {
                text(value).map(Some)
            }
        };
        let kind = event.getattr(intern!(py, "kind"))?.getattr(intern!(py, "_value_"))?;
        let row = (|| {
            Some(Self {
                event_id: text(event.getattr(intern!(py, "event_id")).ok()?)?,
                kind: EventKind::parse(kind.downcast_exact::<PyString>().ok()?.to_str().ok()?)?,
                due_minute: integer(event.getattr(intern!(py, "due_minute")).ok()?)?,
                priority: integer(event.getattr(intern!(py, "priority")).ok()?)?,
                sequence: integer(event.getattr(intern!(py, "sequence")).ok()?)?,
                event_type: text(event.getattr(intern!(py, "event_type")).ok()?)?,
                payload: read_frozen(&event.getattr(intern!(py, "payload")).ok()?, MAX_ROW_DEPTH).ok()??,
                source_id: optional(event.getattr(intern!(py, "source_id")).ok()?)?,
                causation_id: optional(event.getattr(intern!(py, "causation_id")).ok()?)?,
                provenance: read_frozen(&event.getattr(intern!(py, "provenance")).ok()?, MAX_ROW_DEPTH).ok()??,
            })
        })();
        Ok(row.filter(|row| matches!(row.payload, V::Map(_)) && matches!(row.provenance, V::Map(_))))
    }

    fn to_py<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let models = models(py)?;
        let class = models.event.bind(py);
        let event = class.call_method1(intern!(py, "__new__"), (class,))?;
        let fields = event.getattr(intern!(py, "__dict__"))?;
        let optional = |value: &Option<Arc<str>>| -> Bound<'py, PyAny> {
            match value {
                Some(text) => PyString::new(py, text).into_any(),
                None => py.None().into_bound(py),
            }
        };
        fields.set_item(intern!(py, "event_id"), &*self.event_id)?;
        let kind = EventKind::ALL.iter().position(|kind| *kind == self.kind).unwrap_or(0);
        fields.set_item(intern!(py, "kind"), models.kinds[kind].bind(py))?;
        fields.set_item(intern!(py, "due_minute"), self.due_minute)?;
        fields.set_item(intern!(py, "priority"), self.priority)?;
        fields.set_item(intern!(py, "sequence"), self.sequence)?;
        fields.set_item(intern!(py, "event_type"), &*self.event_type)?;
        fields.set_item(intern!(py, "payload"), frozen(py, &self.payload)?)?;
        fields.set_item(intern!(py, "source_id"), optional(&self.source_id))?;
        fields.set_item(intern!(py, "causation_id"), optional(&self.causation_id))?;
        fields.set_item(intern!(py, "provenance"), frozen(py, &self.provenance)?)?;
        Ok(event)
    }
}

/// What `_freeze_json` makes of a value read from Python, read back: Ok(None) for anything else.
fn read_frozen(value: &Bound<'_, PyAny>, depth: usize) -> PyResult<Option<V>> {
    if depth == 0 {
        return Ok(None);
    }
    if value.is_none() {
        return Ok(Some(V::Null));
    }
    if let Ok(text) = value.downcast_exact::<PyString>() {
        // A lone surrogate has no UTF-8 form, and could not be saved.
        return Ok(text.to_str().ok().map(|text| V::Str(Arc::from(text))));
    }
    if let Ok(number) = value.downcast_exact::<PyFloat>() {
        return Ok(number.value().is_finite().then_some(V::Float(number.value())));
    }
    if let Ok(flag) = value.downcast_exact::<PyBool>() {
        return Ok(Some(V::Bool(flag.is_true())));
    }
    if let Ok(number) = value.downcast_exact::<PyInt>() {
        return Ok(Some(match number.extract::<i64>() {
            Ok(small) => V::Int(small),
            Err(_) => V::Big(number.repr()?.to_str()?.into()),
        }));
    }
    if let Ok(items) = value.downcast_exact::<PyTuple>() {
        let mut list = Vec::with_capacity(items.len());
        for item in items.iter() {
            match read_frozen(&item, depth - 1)? {
                Some(item) => list.push(item),
                None => return Ok(None),
            }
        }
        return Ok(Some(V::List(list)));
    }
    let proxy = std::ptr::addr_of_mut!(ffi::PyDictProxy_Type);
    if value.get_type().as_ptr() == proxy.cast() {
        let mut map = IndexMap::with_capacity(value.len()?);
        for entry in value.call_method0(intern!(value.py(), "items"))?.try_iter()? {
            let (key, item): (Bound<'_, PyAny>, Bound<'_, PyAny>) = entry?.extract()?;
            let Some(key) = key.downcast_exact::<PyString>().ok().and_then(|key| key.to_str().ok()) else {
                return Ok(None);
            };
            match read_frozen(&item, depth - 1)? {
                Some(item) => {
                    map.insert(Arc::from(key), item);
                }
                None => return Ok(None),
            }
        }
        return Ok(Some(V::Map(map)));
    }
    Ok(None)
}

/// `_freeze_json` of a value: a map is a mapping proxy and a list a tuple.
fn frozen<'py>(py: Python<'py>, value: &V) -> PyResult<Bound<'py, PyAny>> {
    match value {
        V::List(items) => {
            let items = items.iter().map(|item| frozen(py, item)).collect::<PyResult<Vec<_>>>()?;
            Ok(PyTuple::new(py, items)?.into_any())
        }
        V::Map(map) => {
            let copy = PyDict::new(py);
            for (key, item) in map {
                copy.set_item(&**key, frozen(py, item)?)?;
            }
            // SAFETY: the mapping is a live dict; the call returns a new reference or sets an error.
            unsafe { Bound::from_owned_ptr_or_err(py, ffi::PyDictProxy_New(copy.as_ptr())) }
        }
        other => other.to_py(py),
    }
}

/// A due event as a rule reads it: its row, or its object when Python made it and no row was read.
/// Either is what the event is, and reading the object costs less than making its row once.
#[derive(Clone, Copy)]
pub(crate) enum EventRef<'a, 'py> {
    Row(&'a EventRow),
    Object(&'a Bound<'py, PyAny>),
}

impl EventRef<'_, '_> {
    /// `payload.get(key)` when `isinstance(value, str)`; Unsupported for a subclass of str, which
    /// a row cannot hold.
    pub(crate) fn text(&self, key: &str) -> PyResult<Option<Arc<str>>> {
        match self {
            EventRef::Row(row) => Ok(match &row.payload {
                V::Map(payload) => match payload.get(key) {
                    Some(V::Str(value)) => Some(value.clone()),
                    _ => None,
                },
                _ => None,
            }),
            EventRef::Object(event) => {
                let py = event.py();
                let payload = event.getattr(intern!(py, "payload"))?;
                let value = payload.call_method1(intern!(py, "get"), (PyString::intern(py, key),))?;
                let Ok(value) = value.downcast::<PyString>() else {
                    return Ok(None);
                };
                if !value.is_exact_instance_of::<PyString>() {
                    return Err(Unsupported::new_err("a payload holds a subclass of str"));
                }
                Ok(Some(Arc::from(value.to_str().map_err(|_| Unsupported::new_err("a str is not UTF-8"))?)))
            }
        }
    }

    /// `payload.get(key)` when an int and no other type; Unsupported for one that doesn't fit.
    pub(crate) fn int(&self, key: &str) -> PyResult<Option<i64>> {
        match self {
            EventRef::Row(row) => Ok(match &row.payload {
                V::Map(payload) => match payload.get(key) {
                    Some(V::Int(value)) => Some(*value),
                    Some(V::Big(_)) => return Err(Unsupported::new_err("an int is too large")),
                    _ => None,
                },
                _ => None,
            }),
            EventRef::Object(event) => {
                let py = event.py();
                let payload = event.getattr(intern!(py, "payload"))?;
                let value = payload.call_method1(intern!(py, "get"), (PyString::intern(py, key),))?;
                match value.downcast_exact::<PyInt>() {
                    Ok(number) => Ok(Some(number.extract().map_err(|_| Unsupported::new_err("an int is too large"))?)),
                    Err(_) => Ok(None),
                }
            }
        }
    }

    /// `len(payload)`.
    pub(crate) fn payload_len(&self) -> PyResult<usize> {
        match self {
            EventRef::Row(row) => Ok(match &row.payload {
                V::Map(payload) => payload.len(),
                _ => 0,
            }),
            EventRef::Object(event) => event.getattr(intern!(event.py(), "payload"))?.len(),
        }
    }

    /// `event.kind is EventKind.COMPLETION`.
    pub(crate) fn is_completion(&self) -> PyResult<bool> {
        match self {
            EventRef::Row(row) => Ok(row.kind == EventKind::Completion),
            EventRef::Object(event) => {
                let py = event.py();
                let kind = event.getattr(intern!(py, "kind"))?.getattr(intern!(py, "_value_"))?;
                Ok(kind.downcast_exact::<PyString>().map_err(|_| Unsupported::new_err("a kind is not a str"))?.to_str()? == "completion")
            }
        }
    }

    /// The first and last of a payload's list when both are str, or None for anything else.
    /// `Some((None, None))`, for a list with other values, says what is there is not the rule's.
    pub(crate) fn list_ends(&self, key: &str) -> PyResult<Option<(Option<Arc<str>>, Option<Arc<str>>)>> {
        match self {
            EventRef::Row(row) => {
                let V::Map(payload) = &row.payload else {
                    return Ok(None);
                };
                let Some(V::List(items)) = payload.get(key) else {
                    return Ok(None);
                };
                let text = |item: Option<&V>| match item {
                    Some(V::Str(value)) => Some(value.clone()),
                    _ => None,
                };
                Ok(Some((text(items.first()), text(items.last()))))
            }
            EventRef::Object(event) => {
                let py = event.py();
                let payload = event.getattr(intern!(py, "payload"))?;
                let value = payload.call_method1(intern!(py, "get"), (PyString::intern(py, key),))?;
                let Ok(items) = value.downcast_exact::<PyTuple>() else {
                    return Ok(None);
                };
                let text = |item: PyResult<Bound<'_, PyAny>>| -> Option<Arc<str>> {
                    item.ok()?.downcast_exact::<PyString>().ok()?.to_str().ok().map(Arc::from)
                };
                let count = items.len();
                if count == 0 {
                    return Ok(Some((None, None)));
                }
                Ok(Some((text(items.get_item(0)), text(items.get_item(count - 1)))))
            }
        }
    }

    /// `key in event.provenance`.
    pub(crate) fn has_provenance(&self, key: &str) -> PyResult<bool> {
        match self {
            EventRef::Row(row) => Ok(matches!(&row.provenance, V::Map(provenance) if provenance.contains_key(key))),
            EventRef::Object(event) => {
                let py = event.py();
                event.getattr(intern!(py, "provenance"))?.contains(PyString::intern(py, key))
            }
        }
    }

    pub(crate) fn event_id(&self) -> PyResult<Arc<str>> {
        match self {
            EventRef::Row(row) => Ok(row.event_id.clone()),
            EventRef::Object(event) => {
                let id = event.getattr(intern!(event.py(), "event_id"))?;
                Ok(Arc::from(id.downcast_exact::<PyString>().map_err(|_| Unsupported::new_err("an ID is not a str"))?.to_str()?))
            }
        }
    }

    pub(crate) fn event_type(&self) -> PyResult<Arc<str>> {
        match self {
            EventRef::Row(row) => Ok(row.event_type.clone()),
            EventRef::Object(event) => {
                let id = event.getattr(intern!(event.py(), "event_type"))?;
                Ok(Arc::from(id.downcast_exact::<PyString>().map_err(|_| Unsupported::new_err("a type is not a str"))?.to_str()?))
            }
        }
    }

    pub(crate) fn due_minute(&self) -> PyResult<i64> {
        match self {
            EventRef::Row(row) => Ok(row.due_minute),
            EventRef::Object(event) => {
                let due = event.getattr(intern!(event.py(), "due_minute"))?;
                due.downcast_exact::<PyInt>().map_err(|_| Unsupported::new_err("a minute is not an int"))?.extract().map_err(|_| Unsupported::new_err("a minute is too large"))
            }
        }
    }
}

// ---- The queue ----------------------------------------------------------------------------------

/// One event of a due snapshot, as `EventQueue.due` gives the dispatcher: holds what it was then.
pub(crate) struct DueEvent {
    pub(crate) event_id: Arc<str>,
    serial: u64,
    pub(crate) row: Option<Arc<EventRow>>,
    pub(crate) object: Option<Py<PyAny>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum DueState {
    /// It left the queue: `current is None`.
    Gone,
    /// Still the same event.
    Same,
    /// Another event of its ID, of the same values or not: `current is not event`.
    Changed,
}

/// A pending event as `QueueStore::pending_events` found it.
pub(crate) struct PendingEvent<'py> {
    pub(crate) event_id: Arc<str>,
    pub(crate) due_minute: i64,
    pub(crate) sequence: i64,
    pub(crate) source: PendingSource<'py>,
}

pub(crate) enum PendingSource<'py> {
    Row(Arc<EventRow>),
    Object(Bound<'py, PyAny>),
}

impl PendingEvent<'_> {
    /// `event.kind is EventKind.COMPLETION`.
    pub(crate) fn is_completion(&self) -> PyResult<bool> {
        match &self.source {
            PendingSource::Row(row) => Ok(row.kind == EventKind::Completion),
            PendingSource::Object(object) => {
                let py = object.py();
                Ok(object.getattr(intern!(py, "kind"))?.is(models(py)?.kinds[1].bind(py)))
            }
        }
    }
}

/// The queue behind `sheltersim.events.queue.EventQueue`: the core `Queue`, and the Python objects
/// of its slots by the same serial.
#[pyclass(module = "sheltersim_native.state.events")]
pub struct QueueStore {
    queue: Queue,
    /// Python objects by serial; apart, so that a slot's row is free of handles.
    objects: HashMap<u64, Py<PyAny>>,
    /// The tuple of `pending()`, while the queue's generation is the one it was made at.
    pending: Option<(u64, Py<PyTuple>)>,
}

/// The identity and order of an event object; None for an int outside i64.
fn read_key(event: &Bound<'_, PyAny>) -> PyResult<Option<(Arc<str>, Key)>> {
    let py = event.py();
    let integer = |name: &Bound<'_, PyString>| -> PyResult<Option<i64>> { Ok(event.getattr(name)?.extract::<i64>().ok()) };
    let event_id = event.getattr(intern!(py, "event_id"))?;
    let event_id: Arc<str> = Arc::from(event_id.downcast::<PyString>()?.to_str()?);
    let (due, priority, sequence) = (
        integer(intern!(py, "due_minute"))?,
        integer(intern!(py, "priority"))?,
        integer(intern!(py, "sequence"))?,
    );
    Ok(match (due, priority, sequence) {
        (Some(due), Some(priority), Some(sequence)) => Some((event_id, (due, priority, sequence))),
        _ => None,
    })
}

impl QueueStore {
    fn empty() -> Self {
        Self { queue: Queue::new(), objects: HashMap::new(), pending: None }
    }

    fn object(&mut self, py: Python<'_>, serial: u64) -> PyResult<Py<PyAny>> {
        if let Some(object) = self.objects.get(&serial) {
            return Ok(object.clone_ref(py));
        }
        let row = self.queue.slot(serial).row.as_ref().expect("a slot has a row or an object").clone();
        let object = row.to_py(py)?.unbind();
        self.objects.insert(serial, object.clone_ref(py));
        Ok(object)
    }

    fn objects_of(&mut self, py: Python<'_>, serials: Vec<u64>) -> PyResult<Vec<Py<PyAny>>> {
        serials.into_iter().map(|serial| self.object(py, serial)).collect()
    }

    // ---- For Rust rules ----

    /// The row of a pending event, read from its object the first time. Ok(None) for no such
    /// event or an event with no row: a rule then says `Unsupported`.
    pub(crate) fn row(&mut self, py: Python<'_>, event_id: &str) -> PyResult<Option<Arc<EventRow>>> {
        let Some(serial) = self.queue.serial_of(event_id) else {
            return Ok(None);
        };
        if let Some(row) = &self.queue.slot(serial).row {
            return Ok(Some(row.clone()));
        }
        let Some(row) = EventRow::from_event(self.objects[&serial].bind(py))?.map(Arc::new) else {
            return Ok(None);
        };
        self.queue.set_row(serial, row.clone());
        Ok(Some(row))
    }

    /// `EventQueue.due`, for the dispatch loop: what each event due through `minute` is now.
    pub(crate) fn due_events(&self, py: Python<'_>, minute: i64) -> Vec<DueEvent> {
        let end = self.queue.due_end(minute);
        self.queue.ordered()[..end]
            .iter()
            .map(|(_, serial)| {
                let slot = self.queue.slot(*serial);
                DueEvent {
                    event_id: slot.event_id.clone(),
                    serial: *serial,
                    row: slot.row.clone(),
                    object: self.objects.get(serial).map(|object| object.clone_ref(py)),
                }
            })
            .collect()
    }

    /// Where an event of a due snapshot stands now. An event has a row and an object at different
    /// times, and an event that was replaced has a new object and no row.
    pub(crate) fn state_of_due(&self, due: &DueEvent) -> DueState {
        let Some(serial) = self.queue.serial_of(&due.event_id) else {
            return DueState::Gone;
        };
        if serial != due.serial {
            return DueState::Changed;
        }
        let same = match (&due.object, self.objects.get(&serial)) {
            (Some(then), Some(now)) => then.as_ptr() == now.as_ptr(),
            _ => match (&due.row, &self.queue.slot(serial).row) {
                (Some(then), Some(now)) => Arc::ptr_eq(then, now),
                _ => false,
            },
        };
        if same {
            DueState::Same
        } else {
            DueState::Changed
        }
    }

    /// The object of a pending event of a due snapshot, made if it has none: Python then sees it.
    pub(crate) fn object_of_due(&mut self, py: Python<'_>, due: &DueEvent) -> PyResult<Py<PyAny>> {
        self.object(py, due.serial)
    }

    /// `EventQueue.remove` of an event that may have left already; any object goes with it.
    fn remove_if_pending(&mut self, event_id: &str) {
        if self.queue.has_event(event_id) {
            if let Some((serial, _)) = self.queue.take_slot(event_id) {
                self.objects.remove(&serial);
            }
        }
    }

    /// The pending events in queue order as they are held, turning no object into a row: reading
    /// the whole of an event costs more than the few fields a check needs. Ok(None) if an object
    /// is not a plain `ScheduledEvent`.
    pub(crate) fn pending_events<'py>(&self, py: Python<'py>) -> PyResult<Option<Vec<PendingEvent<'py>>>> {
        let class = models(py)?.event.bind(py);
        let mut events = Vec::with_capacity(self.queue.ordered().len());
        for (key, serial) in self.queue.ordered() {
            let slot = self.queue.slot(*serial);
            let source = match &slot.row {
                Some(row) => PendingSource::Row(row.clone()),
                None => {
                    let object = self.objects[serial].bind(py);
                    if !object.get_type().is(class) {
                        return Ok(None);
                    }
                    PendingSource::Object(object.clone())
                }
            };
            events.push(PendingEvent { event_id: slot.event_id.clone(), due_minute: key.0, sequence: key.2, source });
        }
        Ok(Some(events))
    }

    /// Whether a pending event's provenance has this key, without a copy of anything. Ok(None) if
    /// an object is not a plain `ScheduledEvent`.
    pub(crate) fn any_pending_has_provenance(&self, py: Python<'_>, key: &Bound<'_, PyString>) -> PyResult<Option<bool>> {
        let class = models(py)?.event.bind(py);
        for (_, serial) in self.queue.ordered() {
            let found = match &self.queue.slot(*serial).row {
                Some(row) => matches!(&row.provenance, V::Map(provenance) if provenance.contains_key(key.to_str()?)),
                None => {
                    let object = self.objects[serial].bind(py);
                    if !object.get_type().is(class) {
                        return Ok(None);
                    }
                    object.getattr(intern!(py, "provenance"))?.contains(key)?
                }
            };
            if found {
                return Ok(Some(true));
            }
        }
        Ok(Some(false))
    }

    /// Whether this event is pending.
    pub(crate) fn has_event(&self, event_id: &str) -> bool {
        self.queue.has_event(event_id)
    }

    /// The IDs of the pending events in queue order, through `minute` if given.
    pub(crate) fn pending_ids(&self, minute: Option<i64>) -> Vec<Arc<str>> {
        self.queue.pending_ids(minute)
    }
}

#[pymethods]
impl QueueStore {
    #[new]
    fn new() -> Self {
        Self::empty()
    }

    /// Start from these events: false, with nothing kept, when one's order does not fit an i64.
    fn load(&mut self, py: Python<'_>, events: Vec<Bound<'_, PyAny>>) -> PyResult<bool> {
        let mut read = Vec::with_capacity(events.len());
        for event in &events {
            match read_key(event)? {
                Some(item) => read.push(item),
                None => return Ok(false),
            }
        }
        *self = Self::empty();
        // sorted() over the whole list is stable, and so is adding after equal keys.
        for ((event_id, key), event) in read.into_iter().zip(&events) {
            let serial = self.queue.add_loaded(event_id, key);
            self.objects.insert(serial, event.clone().unbind());
        }
        let _ = py;
        Ok(true)
    }

    fn __len__(&self) -> usize {
        self.queue.len()
    }

    /// The pending events in queue order; the same tuple until the queue changes.
    fn pending<'py>(&mut self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        let generation = self.queue.generation();
        if let Some((made, pending)) = &self.pending {
            if *made == generation {
                return Ok(pending.bind(py).clone());
            }
        }
        let serials: Vec<u64> = self.queue.ordered().iter().map(|(_, serial)| *serial).collect();
        let objects = self.objects_of(py, serials)?;
        let pending = PyTuple::new(py, objects)?;
        self.pending = Some((generation, pending.clone().unbind()));
        Ok(pending)
    }

    /// The events in the order they were scheduled.
    fn events(&mut self, py: Python<'_>) -> PyResult<Vec<Py<PyAny>>> {
        let serials = self.queue.insertion().to_vec();
        self.objects_of(py, serials)
    }

    /// `pending()` through a minute; None for a minute that is not an int in i64.
    fn due<'py>(&mut self, py: Python<'py>, minute: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyTuple>>> {
        let Ok(minute) = minute.downcast_exact::<PyInt>().and_then(|_| Ok(minute)) else {
            return Ok(None);
        };
        let Ok(minute) = minute.extract::<i64>() else {
            return Ok(None);
        };
        let end = self.queue.due_end(minute);
        if end == self.queue.ordered().len() {
            return self.pending(py).map(Some);
        }
        let serials: Vec<u64> = self.queue.ordered()[..end].iter().map(|(_, serial)| *serial).collect();
        let objects = self.objects_of(py, serials)?;
        Ok(Some(PyTuple::new(py, objects)?))
    }

    fn find(&mut self, py: Python<'_>, event_id: &Bound<'_, PyAny>) -> PyResult<Option<Py<PyAny>>> {
        let Some(serial) = event_id
            .downcast::<PyString>()
            .ok()
            .and_then(|text| text.to_str().ok())
            .and_then(|text| self.queue.serial_of(text))
        else {
            return Ok(None);
        };
        self.object(py, serial).map(Some)
    }

    /// 0, or 1 for a pending ID, 2 for a pending sequence, 3 when the order does not fit an i64.
    fn schedule(&mut self, event: &Bound<'_, PyAny>) -> PyResult<u8> {
        let py = event.py();
        let event_id = event.getattr(intern!(py, "event_id"))?;
        if self.queue.has_event(event_id.downcast::<PyString>()?.to_str()?) {
            return Ok(1);
        }
        if event
            .getattr(intern!(py, "sequence"))?
            .extract::<i64>()
            .is_ok_and(|sequence| self.queue.has_sequence(sequence))
        {
            return Ok(2);
        }
        let Some((event_id, key)) = read_key(event)? else {
            return Ok(3);
        };
        let serial = self.queue.add(event_id, key, None);
        self.objects.insert(serial, event.clone().unbind());
        Ok(0)
    }

    /// The removed event; KeyError for no such event.
    fn remove(&mut self, py: Python<'_>, event_id: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        let found = event_id
            .downcast::<PyString>()
            .ok()
            .and_then(|text| text.to_str().ok())
            .and_then(|text| self.queue.take_slot(text));
        let Some((serial, slot)) = found else {
            return Err(PyKeyError::new_err(event_id.clone().unbind()));
        };
        if let Some(object) = self.objects.remove(&serial) {
            return Ok(object);
        }
        Ok(slot.row.expect("a slot has a row or an object").to_py(py)?.unbind())
    }

    /// 0, or 1 when the identity differs, 2 when it is in use by another event, 3 when the order
    /// does not fit an i64; KeyError for no such event.
    fn replace(&mut self, event_id: &Bound<'_, PyAny>, event: &Bound<'_, PyAny>) -> PyResult<u8> {
        let py = event.py();
        let serial = event_id
            .downcast::<PyString>()
            .ok()
            .and_then(|text| text.to_str().ok())
            .and_then(|text| self.queue.first_scheduled(text));
        let Some(serial) = serial else {
            return Err(PyKeyError::new_err(event_id.clone().unbind()));
        };
        let (current_id, current_key) = {
            let slot = self.queue.slot(serial);
            (slot.event_id.clone(), slot.key)
        };
        let new_id = event.getattr(intern!(py, "event_id"))?;
        let same_identity = new_id.downcast::<PyString>().is_ok_and(|text| text.to_str().is_ok_and(|text| text == &*current_id))
            && event.getattr(intern!(py, "sequence"))?.extract::<i64>().is_ok_and(|sequence| sequence == current_key.2);
        if !same_identity {
            return Ok(1);
        }
        let current_object = self.objects.get(&serial).map(|object| object.as_ptr());
        let in_use = self.queue.insertion().iter().any(|other| {
            let slot = self.queue.slot(*other);
            *other != serial
                && (self.objects.get(other).map(|object| object.as_ptr()) != current_object || current_object.is_none())
                && (slot.event_id == current_id || slot.key.2 == current_key.2)
        });
        if in_use {
            return Ok(2);
        }
        let Some((_, key)) = read_key(event)? else {
            return Ok(3);
        };
        self.queue.rekey(serial, key);
        self.objects.insert(serial, event.clone().unbind());
        Ok(0)
    }

    /// 0, or 1 for a repeated ID, 2 for a repeated sequence.
    fn validate(&self) -> u8 {
        self.queue.validate()
    }

    /// An independent queue sharing the frozen events.
    fn copy(&self, py: Python<'_>) -> Self {
        Self {
            queue: self.queue.clone(),
            objects: self.objects.iter().map(|(serial, object)| (*serial, object.clone_ref(py))).collect(),
            pending: self.pending.as_ref().map(|(made, pending)| (*made, pending.clone_ref(py))),
        }
    }
}

// ---- What a rule uses ---------------------------------------------------------------------------

fn duplicate_error(py: Python<'_>, message: String) -> PyErr {
    match py.import("sheltersim.events.queue").and_then(|module| module.getattr("DuplicateEventError")) {
        Ok(class) => PyErr::from_type(class.downcast_into().expect("an exception class"), message),
        Err(error) => error,
    }
}

/// A write's error as Python's: a core error's class, or `DuplicateEventError`.
fn write_error(py: Python<'_>, error: WriteError) -> PyErr {
    match error {
        WriteError::Error(error) => to_py_err(error),
        WriteError::Duplicate(message) => duplicate_error(py, message),
    }
}

/// A world's queue, log and event counter, for one step of a rule. Holds no borrow of them, and
/// asks for each after any Python callback as a rule asks for `core_of`: a rule that calls back into
/// Python makes a new writer.
pub(crate) struct EventWriter<'py> {
    queue: Bound<'py, QueueStore>,
    log: Bound<'py, EventStore>,
    /// The mirror of `world.id_counters`, whose row the counter is read from and written to.
    counters: Bound<'py, NamespaceMirror>,
    duration: i64,
    reserved: Reserved,
}

impl<'py> EventWriter<'py> {
    /// `Unsupported`, with nothing changed, unless the world's queue and log are Rust's, and its
    /// `id_counters` the core's row (`state/world_fields.rs`) whose event counter is an int. So a
    /// rule asks in its planning stage.
    pub(crate) fn of(world: &Bound<'py, PyAny>) -> PyResult<Self> {
        let py = world.py();
        let store = |name: &Bound<'py, PyString>| -> PyResult<Bound<'py, PyAny>> {
            world
                .getattr(name)?
                .getattr(intern!(py, "_store"))
                .map_err(|_| Unsupported::new_err("the world's events are not in Rust"))
        };
        let queue = store(intern!(py, "event_queue"))?
            .downcast_into::<QueueStore>()
            .map_err(|_| Unsupported::new_err("the event queue is not in Rust"))?;
        let log = store(intern!(py, "event_log"))?
            .downcast_into::<EventStore>()
            .map_err(|_| Unsupported::new_err("the event log is not in Rust"))?;
        let counters = world_fields::owned_counters(world)?;
        match world_fields::id_counter(&counters, "event") {
            Ok(None) => {}
            Ok(Some(number)) if number <= MAX_RULE_COUNTER => {}
            _ => return Err(Unsupported::new_err("the event counter is not an int a rule can add to")),
        }
        let duration = world
            .getattr(intern!(py, "duration_minutes"))?
            .extract::<i64>()
            .map_err(|_| Unsupported::new_err("the duration is not an int"))?;
        Ok(Self { queue, log, counters, duration, reserved: Reserved::default() })
    }

    pub(crate) fn duration(&self) -> i64 {
        self.duration
    }

    /// What `allocate_sequence` would return, changing nothing, past the sequences reserved.
    pub(crate) fn next_sequence(&self) -> PyResult<i64> {
        Ok(self.reserved.after(self.counter()?))
    }

    /// Sets how many sequences `allocate_reserved` will take before what is planned from now on.
    pub(crate) fn reserve(&self, count: usize) {
        self.reserved.set(count);
    }

    /// `allocate_sequence`, of one the plan reserved.
    pub(crate) fn allocate_reserved(&self) -> PyResult<i64> {
        self.reserved.take_one();
        self.allocate_sequence()
    }

    fn counter(&self) -> PyResult<i64> {
        Ok(world_fields::id_counter(&self.counters, "event")?.unwrap_or(0))
    }

    /// `WorldState.allocate_event_sequence`.
    pub(crate) fn allocate_sequence(&self) -> PyResult<i64> {
        let sequence = self.counter()?;
        world_fields::write_id_counter(&self.counters, "event", sequence + 1)?;
        Ok(sequence)
    }

    /// `WorldState.schedule_event`, with its errors.
    pub(crate) fn schedule(&self, row: EventRow) -> PyResult<()> {
        let py = self.counters.py();
        let after = {
            let log = self.log.borrow();
            let mut queue = self.queue.borrow_mut();
            sheltersim_core::events::writer::schedule(&mut queue.queue, log.core(), self.duration, row)
        };
        let after = after.map_err(|error| write_error(py, error))?;
        let next = self.counter()?.max(after);
        world_fields::write_id_counter(&self.counters, "event", next)
    }

    /// `EventLogRecord(...)` and `EventLog.append` of an outcome, with their errors. Python makes the record when it reads it.
    pub(crate) fn record(
        &self,
        event: &EventRow,
        realized_minute: i64,
        status: EventStatus,
        detail: Option<&str>,
    ) -> PyResult<()> {
        let py = self.counters.py();
        let mut log = self.log.borrow_mut();
        sheltersim_core::events::writer::record(log.core_mut(), event, realized_minute, status, detail)
            .map_err(|error| write_error(py, error))
    }

    /// What the dispatcher does to a handled event (`EventDispatcher.settle`): it leaves the queue if
    /// it is there, and its outcome is recorded. Ok(false) when the log holds a record of its ID
    /// already, which a handler wrote: the caller compares the two as that method does. The event
    /// may have left the queue already, and the row is the one of the due snapshot, not what is
    /// pending now.
    pub(crate) fn settle(
        &self,
        event: &EventRow,
        realized_minute: i64,
        status: EventStatus,
        detail: Option<&str>,
    ) -> PyResult<bool> {
        self.queue.borrow_mut().remove_if_pending(&event.event_id);
        if self.log.borrow().has_event_id(&event.event_id) {
            return Ok(false);
        }
        self.record(event, realized_minute, status, detail)?;
        Ok(true)
    }

    /// `settle` of an event that has its object: no row is read for it. Ok(false) also for a value
    /// the writer refuses, which is `EventDispatcher.settle`'s to record.
    pub(crate) fn settle_object(
        &self,
        event_id: &str,
        event: &Bound<'py, PyAny>,
        realized_minute: i64,
        status: EventStatus,
        detail: Option<&str>,
    ) -> PyResult<bool> {
        let py = self.counters.py();
        self.queue.borrow_mut().remove_if_pending(event_id);
        if self.log.borrow().has_event_id(event_id) {
            return Ok(false);
        }
        match self.log.borrow_mut().append_outcome(event, realized_minute, status, detail)? {
            0 => Ok(true),
            1 => Err(duplicate_error(py, format!("event '{event_id}' already realized"))),
            2 => Err(duplicate_error(
                py,
                format!("event sequence {} already realized", event.getattr(intern!(py, "sequence"))?),
            )),
            _ => Ok(false),
        }
    }

    pub(crate) fn queue(&self) -> &Bound<'py, QueueStore> {
        &self.queue
    }

    pub(crate) fn log(&self) -> &Bound<'py, EventStore> {
        &self.log
    }
}

// ---- For tests and benchmarks/rule_shadow ---------------------------------------------------------

/// The event as a row and back: another `ScheduledEvent` of the same values, or None for an event
/// with no row.
#[pyfunction]
fn row_copy<'py>(event: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyAny>>> {
    EventRow::from_event(event)?.map(|row| row.to_py(event.py())).transpose()
}

/// The canonical JSON a row of the event writes for a record of it, or None for an event with no row.
#[pyfunction]
#[pyo3(signature = (event, realized_minute, status, detail=None))]
fn row_record_json<'py>(
    event: &Bound<'py, PyAny>,
    realized_minute: i64,
    status: &str,
    detail: Option<&str>,
) -> PyResult<Option<Bound<'py, PyBytes>>> {
    let status = EventStatus::parse(status).ok_or_else(|| PyValueError::new_err("unknown status"))?;
    let Some(row) = EventRow::from_event(event)? else {
        return Ok(None);
    };
    Ok(Some(PyBytes::new(event.py(), &row.record_json(realized_minute, status, detail).or_py()?)))
}

/// The event as a row saved and read back, then as an object; None when bytes do not read.
#[pyfunction]
fn saved_row_copy<'py>(raw: &[u8], py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
    EventRow::from_saved_record(raw).map(|(row, ..)| row.to_py(py)).transpose()
}

/// The next sequence, allocated as a rule does; `Unsupported` as `EventWriter::of`.
#[pyfunction]
fn allocate_sequence(world: &Bound<'_, PyAny>) -> PyResult<i64> {
    EventWriter::of(world)?.allocate_sequence()
}

/// A rule scheduling the row of `event`.
#[pyfunction]
fn schedule_row(world: &Bound<'_, PyAny>, event: &Bound<'_, PyAny>) -> PyResult<()> {
    let row = EventRow::from_event(event)?.ok_or_else(|| Unsupported::new_err("the event has no row"))?;
    EventWriter::of(world)?.schedule(row)
}

/// A rule recording an outcome of the row of `event`.
#[pyfunction]
#[pyo3(signature = (world, event, realized_minute, status, detail=None))]
fn record_row(
    world: &Bound<'_, PyAny>,
    event: &Bound<'_, PyAny>,
    realized_minute: i64,
    status: &str,
    detail: Option<&str>,
) -> PyResult<()> {
    let status = EventStatus::parse(status).ok_or_else(|| PyValueError::new_err("unknown status"))?;
    let row = EventRow::from_event(event)?.ok_or_else(|| Unsupported::new_err("the event has no row"))?;
    EventWriter::of(world)?.record(&row, realized_minute, status, detail)
}

/// A rule settling a pending event: whether it did.
#[pyfunction]
#[pyo3(signature = (world, event_id, realized_minute, status, detail=None))]
fn settle_row(
    world: &Bound<'_, PyAny>,
    event_id: &str,
    realized_minute: i64,
    status: &str,
    detail: Option<&str>,
) -> PyResult<bool> {
    let status = EventStatus::parse(status).ok_or_else(|| PyValueError::new_err("unknown status"))?;
    let writer = EventWriter::of(world)?;
    let Some(row) = writer.queue().borrow_mut().row(world.py(), event_id)? else {
        return Ok(false);
    };
    writer.settle(&row, realized_minute, status, detail)
}

/// `EventWriter::settle` of the row of `event`, wherever it is now.
#[pyfunction]
#[pyo3(signature = (world, event, realized_minute, status, detail=None))]
fn settle_event(
    world: &Bound<'_, PyAny>,
    event: &Bound<'_, PyAny>,
    realized_minute: i64,
    status: &str,
    detail: Option<&str>,
) -> PyResult<bool> {
    let status = EventStatus::parse(status).ok_or_else(|| PyValueError::new_err("unknown status"))?;
    let row = EventRow::from_event(event)?.ok_or_else(|| Unsupported::new_err("the event has no row"))?;
    EventWriter::of(world)?.settle(&row, realized_minute, status, detail)
}

/// What a rule reads: the IDs due through `minute`, and each one's row as an object again.
#[pyfunction]
fn rows_due<'py>(world: &Bound<'py, PyAny>, minute: i64) -> PyResult<Vec<(String, Option<Bound<'py, PyAny>>)>> {
    let py = world.py();
    let writer = EventWriter::of(world)?;
    let mut queue = writer.queue().borrow_mut();
    let ids = queue.pending_ids(Some(minute));
    ids.iter()
        .map(|event_id| {
            let row = queue.row(py, event_id)?;
            Ok((event_id.to_string(), row.map(|row| row.to_py(py)).transpose()?))
        })
        .collect()
}

/// What a rule reads of the log: each record of a type as `(event, minute, status, detail)`.
#[pyfunction]
fn saved_rows<'py>(
    world: &Bound<'py, PyAny>,
    event_type: &str,
) -> PyResult<Vec<(Bound<'py, PyAny>, i64, &'static str, Option<String>)>> {
    let py = world.py();
    let writer = EventWriter::of(world)?;
    let log = writer.log().borrow();
    log.positions_of_type(event_type)
        .iter()
        .map(|position| {
            let (row, minute, status, detail) = log
                .saved_document(py, *position)
                .ok_or_else(|| Unsupported::new_err("a record has no bytes"))?;
            Ok((row.to_py(py)?, minute, status.as_str(), detail.map(|text| text.to_string())))
        })
        .collect()
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<QueueStore>()?;
    m.add_function(wrap_pyfunction!(row_copy, m)?)?;
    m.add_function(wrap_pyfunction!(row_record_json, m)?)?;
    m.add_function(wrap_pyfunction!(saved_row_copy, m)?)?;
    m.add_function(wrap_pyfunction!(allocate_sequence, m)?)?;
    m.add_function(wrap_pyfunction!(schedule_row, m)?)?;
    m.add_function(wrap_pyfunction!(record_row, m)?)?;
    m.add_function(wrap_pyfunction!(settle_row, m)?)?;
    m.add_function(wrap_pyfunction!(settle_event, m)?)?;
    m.add_function(wrap_pyfunction!(rows_due, m)?)?;
    m.add_function(wrap_pyfunction!(saved_rows, m)?)?;
    Ok(())
}
