//! The entities of a world as Rust rows: agents with their needs, facilities, staff, support
//! requests and active tasks (docs/rust-port/lanes/p6-entity-windows.md). The rows, `Table` and
//! `Entities` are the core's (`sheltersim_core::entities`); here is what needs Python.
//!
//! The Python objects stay the dataclasses of `domain/models.py`: a read is a plain attribute
//! read of the instance, and each `__dict__` is the mirror of a row, as a tracked tree is the
//! mirror of a namespace's row. An object in a world's `IdMap` is attached: its slots `_store` and
//! `_slot` say where its row is (they stay out of `__dict__`), and its `__setattr__` comes here. `IdMap` reports what is inserted and removed, and
//! `TrackedList` what happens to the ID lists of a facility or a staff member. Rust rules read
//! the rows through `with_rows`, and `write_field` writes a row and then its object.
//!
//! The rows and their taint are the core's (`sheltersim_core::world::World::entities`), held by the
//! world's `WorldCore`; the store is a handle that holds that core, and the core holds it: a cycle
//! the collector breaks (`WorldCore.__clear__`). The store holds no reference to an entity or a
//! world. Rust finds an object through the world's `IdMap`.
//!
//! A value that does not fit a typed field taints the whole store. The Python object keeps what
//! was written, so the Python rules see it and fail with their own text; `with_rows` then says
//! `Unsupported` until `refit` reads every object again. A typed field takes only its exact
//! Python type, and an ID list only exact `str` items.
//!
//! No borrow of the rows is held across anything that may run Python code: a property setter
//! (`acute_distress`) writes back through here while `set_field` is storing.

use std::ptr::null_mut;
use std::sync::atomic::{AtomicPtr, AtomicU64, Ordering};
use std::sync::Arc;

use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::ffi;
use pyo3::gc::PyVisit;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyFloat, PyInt, PyList, PyString, PyTuple, PyType};
use pyo3::IntoPyObjectExt;
use pyo3::PyTraverseError;
use rustc_hash::FxHashSet;
use sheltersim_core::cell::Shard;

pub(crate) use sheltersim_core::entities::{
    Activity, AgentRow, Entities, FacilityRow, Mobility, NeedRow, RequestStatus, TaskRow,
};
use sheltersim_core::entities::{
    EntityField, EntityRow, ProfileRow, RequestRow, Skills, StaffRow, Table, ACTIVITIES, AGENT,
    FACILITY, MOBILITIES, NEEDS, REQUEST, STAFF, STATUSES, TASK,
};
use sheltersim_core::row::{same_names, IdMap, StrEnum};

use crate::core::tracked::{as_readable_list, id_map_type};
use crate::core::errors::OrPy;
use crate::core::value::V;
use crate::core::world_core::{
    core_of, new_tracked_list, raw_dict_get, raw_dict_set, raw_list_append, Misfit, OrMisfit, WorldCore,
};
use crate::{Encoder, Unsupported};

// ---- What Python hands over ---------------------------------------------------------------

struct Installed {
    agent: Py<PyType>,
    needs: Py<PyType>,
    facility: Py<PyType>,
    staff: Py<PyType>,
    request: Py<PyType>,
    task: Py<PyType>,
    activity: Vec<Py<PyAny>>,
    mobility: Vec<Py<PyAny>>,
    status: Vec<Py<PyAny>>,
}

static INSTALLED: AtomicPtr<Installed> = AtomicPtr::new(null_mut());

fn installed() -> Result<&'static Installed, Misfit> {
    // SAFETY: the pointer comes from Box::leak in install and is never freed.
    unsafe { INSTALLED.load(Ordering::Acquire).as_ref() }
        .ok_or_else(|| Misfit::new("domain/models.py is not imported"))
}

/// Remember the classes and the members of the enums, in the order of the core's `ACTIVITIES` and
/// the others. Each member's `.value` must be the core's text, which is what a document holds.
/// They stay alive for the life of the process.
#[pyfunction]
#[allow(clippy::too_many_arguments)]
fn install(
    py: Python<'_>,
    agent: Py<PyType>,
    needs: Py<PyType>,
    facility: Py<PyType>,
    staff: Py<PyType>,
    request: Py<PyType>,
    task: Py<PyType>,
    activity: Vec<Py<PyAny>>,
    mobility: Vec<Py<PyAny>>,
    status: Vec<Py<PyAny>>,
) -> PyResult<()> {
    fn matches<T: StrEnum>(py: Python<'_>, members: &[Py<PyAny>], rows: &[T]) -> PyResult<bool> {
        if members.len() != rows.len() {
            return Ok(false);
        }
        for (member, row) in members.iter().zip(rows) {
            let value: String = member.bind(py).getattr("value")?.extract()?;
            if value != row.as_text() {
                return Ok(false);
            }
        }
        Ok(true)
    }
    if !(matches(py, &activity, ACTIVITIES)? && matches(py, &mobility, MOBILITIES)? && matches(py, &status, STATUSES)?)
    {
        return Err(PyValueError::new_err("the enums of domain/models.py do not match the rows"));
    }
    let new = Box::leak(Box::new(Installed { agent, needs, facility, staff, request, task, activity, mobility, status }));
    INSTALLED.store(new, Ordering::Release);
    Ok(())
}

// ---- Typed fields ----------------------------------------------------------------------------

/// What a field of a row does with Python. `from_py` runs no Python code and takes only the exact
/// type.
trait PyEntityField: EntityField {
    fn from_py(value: &Bound<'_, PyAny>) -> Result<Self, Misfit>;
    fn to_py(&self, py: Python<'_>) -> PyResult<Py<PyAny>>;
}

/// A forwarded attribute write into a row.
trait Forward {
    /// Take the write. A name that is no field (a property) is nothing to do here. ProfileRow's
    /// whole value is replaced instead.
    fn set(&mut self, name: &str, value: &Bound<'_, PyAny>) -> Result<(), Misfit>;
}

impl PyEntityField for f64 {
    fn from_py(value: &Bound<'_, PyAny>) -> Result<Self, Misfit> {
        Ok(value.downcast_exact::<PyFloat>().map_err(|_| Misfit::new("not a float"))?.value())
    }
    fn to_py(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        self.into_py_any(py)
    }
}

impl PyEntityField for i64 {
    fn from_py(value: &Bound<'_, PyAny>) -> Result<Self, Misfit> {
        // A bool's exact type is not int.
        Ok(value.downcast_exact::<PyInt>().map_err(|_| Misfit::new("not an int"))?.extract().or_misfit()?)
    }
    fn to_py(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        self.into_py_any(py)
    }
}

impl PyEntityField for Arc<str> {
    fn from_py(value: &Bound<'_, PyAny>) -> Result<Self, Misfit> {
        let text = value.downcast_exact::<PyString>().map_err(|_| Misfit::new("not a str"))?;
        Ok(Arc::from(text.to_str().or_misfit()?))
    }
    fn to_py(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        (&**self).into_py_any(py)
    }
}

impl<T: PyEntityField> PyEntityField for Option<T> {
    fn from_py(value: &Bound<'_, PyAny>) -> Result<Self, Misfit> {
        if value.is_none() {
            Ok(None)
        } else {
            T::from_py(value).map(Some)
        }
    }
    fn to_py(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        match self {
            Some(value) => value.to_py(py),
            None => Ok(py.None()),
        }
    }
}

/// An ID list: a list of a facility or a staff member. Its node is a `TrackedList`.
impl PyEntityField for Vec<Arc<str>> {
    fn from_py(value: &Bound<'_, PyAny>) -> Result<Self, Misfit> {
        let list = as_readable_list(value).ok_or_else(|| Misfit::new("not a list"))?;
        list.iter().map(|item| <Arc<str>>::from_py(&item)).collect()
    }
    fn to_py(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        Ok(PyList::new(py, self.iter().map(|item| &**item))?.into_any().unbind())
    }
}

/// `StaffState.skills`: a tuple of str.
impl PyEntityField for Skills {
    fn from_py(value: &Bound<'_, PyAny>) -> Result<Self, Misfit> {
        let items = value.downcast_exact::<PyTuple>().map_err(|_| Misfit::new("not a tuple"))?;
        items.iter().map(|item| <Arc<str>>::from_py(&item)).collect::<Result<_, _>>().map(Skills)
    }
    fn to_py(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        Ok(PyTuple::new(py, self.0.iter().map(|item| &**item))?.into_any().unbind())
    }
}

impl PyEntityField for V {
    fn from_py(value: &Bound<'_, PyAny>) -> Result<Self, Misfit> {
        Ok(<V as crate::core::value::VPy>::from_py(value).or_misfit()?)
    }
    fn to_py(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        Ok(crate::core::value::VPy::to_py(self, py)?.unbind())
    }
}

/// The member of the Python enum of an enum field.
pub(crate) trait Member {
    fn member<'py>(self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>>;
}

/// An enum field: a member in the row is its number, and the member object is `Installed`'s, so
/// `agent.activity is Activity.IDLE` holds for anything Rust writes.
macro_rules! member_field {
    ($name:ident, $all:ident, $table:ident) => {
        impl PyEntityField for $name {
            fn from_py(value: &Bound<'_, PyAny>) -> Result<Self, Misfit> {
                let members = &installed()?.$table;
                members
                    .iter()
                    .position(|member| value.is(member))
                    .map(|index| $all[index])
                    .ok_or_else(|| Misfit::new(<$name as StrEnum>::MISFIT))
            }
            fn to_py(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
                let members = &installed().map_err(|misfit| PyRuntimeError::new_err(misfit.0))?.$table;
                Ok(members[self.index()].clone_ref(py))
            }
        }

        impl Member for $name {
            fn member<'py>(self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
                Ok(PyEntityField::to_py(&self, py)?.into_bound(py))
            }
        }
    };
}

member_field!(Activity, ACTIVITIES, activity);
member_field!(Mobility, MOBILITIES, mobility);
member_field!(RequestStatus, STATUSES, status);

// ---- Rows ------------------------------------------------------------------------------------

/// The Python side of a row of the core's `entity_row!`: the same fields in the same order (a
/// compile-time check says so). `from_py` reads each attribute of the object; `to_py` makes a dict
/// named like the dataclass's fields.
macro_rules! py_entity_row {
    ($name:ident { $($field:ident),* $(,)? }) => {
        const _: () = assert!(
            same_names($name::NAMES, &[$(stringify!($field)),*]),
            concat!("py_entity_row! must list the fields of ", stringify!($name), " in their order"),
        );

        impl PyEntityField for $name {
            fn from_py(value: &Bound<'_, PyAny>) -> Result<Self, Misfit> {
                let py = value.py();
                Ok(Self {
                    $($field: PyEntityField::from_py(
                        &value.getattr(intern!(py, stringify!($field))).or_misfit()?,
                    )?,)*
                })
            }
            fn to_py(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
                let document = PyDict::new(py);
                $(document.set_item(intern!(py, stringify!($field)), self.$field.to_py(py)?)?;)*
                Ok(document.into_any().unbind())
            }
        }

        impl Forward for $name {
            fn set(&mut self, name: &str, value: &Bound<'_, PyAny>) -> Result<(), Misfit> {
                match name {
                    $(stringify!($field) => self.$field = PyEntityField::from_py(value)?,)*
                    _ => {}
                }
                Ok(())
            }
        }
    };
}

py_entity_row!(NeedRow { toilet_urgency, hunger, fatigue, hydration_need });
py_entity_row!(ProfileRow {
    id, group_id, initial_zone_id, mobility, support_need, stress_sensitivity, rng_key, initial_needs, provenance,
});
py_entity_row!(AgentRow {
    profile, zone_id, activity, target_id, needs, stress, trust, next_decision_at, support_request_id,
    support_cooldown_until,
});
py_entity_row!(FacilityRow {
    id, kind, zone_id, capacity, working_units, active_agent_ids, queue_agent_ids, resource_active_agent_ids,
    resource_queue_agent_ids,
});
py_entity_row!(StaffRow { id, zone_id, skills, work_queue, busy_until, active_task_id });
py_entity_row!(RequestRow {
    id, agent_id, requested_minute, timeout_minute, status, assigned_staff_id, task_id, resolved_minute,
    cooldown_until, last_assignment_attempt_minute, last_assignment_wait_reason_code,
});
py_entity_row!(TaskRow { id, kind, agent_id, resource_id, start_minute, due_minute, staff_id });


// ---- Which attributes hold what -----------------------------------------------------------

// The number after the slot in a `TrackedList`'s `_node`, which says which field the list is.
const LIST_FIELDS: [(&str, u8); 5] = [
    ("active_agent_ids", FACILITY),
    ("queue_agent_ids", FACILITY),
    ("resource_active_agent_ids", FACILITY),
    ("resource_queue_agent_ids", FACILITY),
    ("work_queue", STAFF),
];
const LIST_KINDS: u32 = LIST_FIELDS.len() as u32;

fn list_node(slot: u32, field: usize) -> u32 {
    slot * LIST_KINDS + field as u32
}

fn list_field_of(kind: u8, name: &str) -> Option<usize> {
    LIST_FIELDS.iter().position(|(field, owner)| *owner == kind && *field == name)
}

fn class_of(installed: &Installed, kind: u8) -> &Py<PyType> {
    match kind {
        AGENT => &installed.agent,
        NEEDS => &installed.needs,
        FACILITY => &installed.facility,
        STAFF => &installed.staff,
        REQUEST => &installed.request,
        _ => &installed.task,
    }
}

/// Store an attribute of an entity without its `__setattr__`.
pub(crate) fn raw_setattr(
    object: &Bound<'_, PyAny>,
    name: &Bound<'_, PyString>,
    value: *mut ffi::PyObject,
) -> PyResult<()> {
    // SAFETY: object and name are live; the call takes its own reference to value.
    if unsafe { ffi::PyObject_GenericSetAttr(object.as_ptr(), name.as_ptr(), value) } != 0 {
        return Err(PyErr::fetch(object.py()));
    }
    Ok(())
}

fn exact_type_is(value: &Bound<'_, PyAny>, class: &Py<PyType>) -> bool {
    value.get_type().is(class)
}

// ---- The store -------------------------------------------------------------------------------

/// The canonical JSON of each row of a table as `canonical_json` writes it, in the order
/// `WorldState._canonical_data` gives: by the entity's own ID, stably from the dict's order. A row
/// nothing reached for writing keeps its bytes object, so the same object means the same row.
fn table_json<'py, R: EntityRow>(
    py: Python<'py>,
    table: &mut Table<R>,
    kept: &mut Vec<Option<Py<PyBytes>>>,
    version: i64,
) -> PyResult<Bound<'py, PyList>> {
    let order = table.document_slots();
    kept.resize_with(table.slot_count(), || None);
    let mut encoder = Encoder { out: Vec::with_capacity(512) };
    let list = PyList::empty(py);
    for slot in order {
        let current = match &kept[slot as usize] {
            Some(raw) if !table.changed(slot) => raw.clone_ref(py),
            _ => {
                encoder.out.clear();
                let Some(row) = table.row_at(slot) else { continue };
                row.write_json(&mut encoder, version).or_py()?;
                let raw = PyBytes::new(py, &encoder.out).unbind();
                kept[slot as usize] = Some(raw.clone_ref(py));
                table.mark_written(slot);
                raw
            }
        };
        list.append(current)?;
    }
    Ok(list)
}

/// The handle of one world's entity rows, and the sink of its attached objects, `IdMap`s and ID
/// lists. The rows and their taint are the core's (`World::entities`, a `Shard` the handle borrows
/// on its own); here are the bytes a save was given and the counters.
#[pyclass(frozen, module = "sheltersim_native")]
pub struct EntityStore {
    core: Py<WorldCore>,
    row_json: Shard<RowJson>,
    forwards: AtomicU64,
    attaches: AtomicU64,
    detaches: AtomicU64,
    taints: AtomicU64,
}

/// The document bytes of each table's rows by slot, and the WorldState version they are for.
#[derive(Default)]
struct RowJson {
    tables: [Vec<Option<Py<PyBytes>>>; 5],
    version: i64,
}

#[inline]
fn count(counter: &AtomicU64) {
    counter.fetch_add(1, Ordering::Relaxed);
}

impl EntityStore {
    pub(crate) fn new(core: Py<WorldCore>) -> Self {
        Self {
            core,
            row_json: Shard::default(),
            forwards: AtomicU64::new(0),
            attaches: AtomicU64::new(0),
            detaches: AtomicU64::new(0),
            taints: AtomicU64::new(0),
        }
    }

    /// Run f on the rows, borrowed mutably: the core shared, the entities alone. f must not run
    /// Python code.
    #[inline]
    fn rows_mut<R>(&self, py: Python<'_>, f: impl FnOnce(&mut Entities) -> R) -> PyResult<R> {
        let core = self.core.bind(py).try_borrow()?;
        let mut rows = core.world().entities.try_borrow_mut().or_py()?;
        Ok(f(&mut rows))
    }

    #[inline]
    fn rows_ref<R>(&self, py: Python<'_>, f: impl FnOnce(&Entities) -> R) -> PyResult<R> {
        let core = self.core.bind(py).try_borrow()?;
        let rows = core.world().entities.try_borrow().or_py()?;
        Ok(f(&rows))
    }

    fn is_tainted(&self, py: Python<'_>) -> PyResult<bool> {
        self.rows_ref(py, |rows| rows.tainted().is_some())
    }

    /// Drop every row; the first reason stays the reason.
    fn taint_rows(&self, rows: &mut Entities, why: String) {
        count(&self.taints);
        rows.taint(why);
    }

    fn taint(&self, py: Python<'_>, why: String) -> PyResult<()> {
        self.rows_mut(py, |rows| self.taint_rows(rows, why))
    }

    /// Whether the object is attached to this store, and its slot if so.
    fn attached_here(slf: &Bound<'_, Self>, object: &Bound<'_, PyAny>) -> PyResult<Option<u32>> {
        let py = slf.py();
        let store = object.getattr(intern!(py, "_store"))?;
        if store.is(slf) {
            Ok(Some(object.getattr(intern!(py, "_slot"))?.extract()?))
        } else {
            Ok(None)
        }
    }

    fn mark_attached(
        slf: &Bound<'_, Self>,
        object: &Bound<'_, PyAny>,
        slot: u32,
    ) -> PyResult<()> {
        let py = slf.py();
        raw_setattr(object, intern!(py, "_store"), slf.as_ptr())?;
        let slot = slot.into_pyobject(py)?;
        raw_setattr(object, intern!(py, "_slot"), slot.as_ptr())
    }

    fn mark_detached(object: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = object.py();
        // SAFETY: None is a live object for as long as the GIL is held.
        raw_setattr(object, intern!(py, "_store"), py.None().as_ptr())
    }

    /// What a field stores for an ID list: a `TrackedList` of this store. A free one moves in as
    /// itself and anything else readable is copied, so a list has one place: a write can only
    /// mean one row. Not a list: an error for the caller to turn into a taint.
    fn adopt_list<'py>(
        slf: &Bound<'py, Self>,
        value: &Bound<'py, PyAny>,
        node: u32,
    ) -> Result<Bound<'py, PyAny>, Misfit> {
        let py = slf.py();
        let list = as_readable_list(value).ok_or_else(|| Misfit::new("not a list"))?;
        let tracked = value.is_instance_of::<PyList>() && !value.is_exact_instance_of::<PyList>();
        let reuse = tracked && value.getattr(intern!(py, "_sink")).or_misfit()?.is_none();
        let stored = if reuse {
            value.clone()
        } else {
            let new = new_tracked_list(py).or_misfit()?;
            for item in list.iter() {
                raw_list_append(&new, &item).or_misfit()?;
            }
            new
        };
        stored.setattr(intern!(py, "_sink"), slf).or_misfit()?;
        stored.setattr(intern!(py, "_node"), node).or_misfit()?;
        Ok(stored)
    }

    fn free_list(slf: &Bound<'_, Self>, value: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = slf.py();
        let tracked = value.is_instance_of::<PyList>() && !value.is_exact_instance_of::<PyList>();
        if tracked && as_readable_list(value).is_some() && value.getattr(intern!(py, "_sink"))?.is(slf) {
            value.setattr(intern!(py, "_sink"), py.None())?;
            value.setattr(intern!(py, "_node"), py.None())?;
        }
        Ok(())
    }

    /// Give an object its links: its ID lists become nodes, and an agent's needs are attached.
    fn link_children(
        slf: &Bound<'_, Self>,
        kind: u8,
        object: &Bound<'_, PyAny>,
        slot: u32,
    ) -> Result<(), Misfit> {
        let py = slf.py();
        for (index, (name, owner)) in LIST_FIELDS.iter().enumerate() {
            if *owner == kind {
                let name = PyString::intern(py, name);
                let current = object.getattr(&name).or_misfit()?;
                let stored = Self::adopt_list(slf, &current, list_node(slot, index))?;
                if !stored.is(&current) {
                    raw_setattr(object, &name, stored.as_ptr()).or_misfit()?;
                }
            }
        }
        if kind == AGENT {
            Self::link_needs(slf, &object.getattr(intern!(py, "needs")).or_misfit()?, slot)?;
        }
        Ok(())
    }

    fn link_needs(slf: &Bound<'_, Self>, needs: &Bound<'_, PyAny>, slot: u32) -> Result<(), Misfit> {
        if !exact_type_is(needs, &installed()?.needs) {
            return Err(Misfit::new("an agent's needs are not a NeedState"));
        }
        if !needs.getattr(intern!(slf.py(), "_store")).or_misfit()?.is_none() {
            return Err(Misfit::new("a NeedState has two places"));
        }
        Self::mark_attached(slf, needs, slot).or_misfit()?;
        Ok(())
    }

    fn unlink_children(slf: &Bound<'_, Self>, kind: u8, object: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = slf.py();
        for (name, owner) in LIST_FIELDS.iter() {
            if *owner == kind {
                let name = PyString::intern(py, name);
                Self::free_list(slf, &object.getattr(&name)?)?;
            }
        }
        if kind == AGENT {
            let needs = object.getattr(intern!(py, "needs"))?;
            if exact_type_is(&needs, class_of(installed().map_err(misfit_error)?, NEEDS))
                && Self::attached_here(slf, &needs)?.is_some()
            {
                Self::mark_detached(&needs)?;
            }
        }
        Ok(())
    }

    /// Read an object into a free slot. The caller knows it has the right class.
    fn load_into_table(
        slf: &Bound<'_, Self>,
        kind: u8,
        object: &Bound<'_, PyAny>,
    ) -> Result<u32, Misfit> {
        let py = slf.py();
        let this = slf.get();
        let slot = this.rows_mut(py, |rows| rows.table_mut(kind).reserve_slot()).map_err(pyo3_misfit)?;
        Self::link_children(slf, kind, object, slot)?;
        // Getting these attributes runs no Python code: the classes define no such properties.
        this.rows_mut(py, |rows| -> Result<(), Misfit> {
            match kind {
                AGENT => rows.agents.fill_slot(slot, AgentRow::from_py(object)?),
                FACILITY => rows.facilities.fill_slot(slot, FacilityRow::from_py(object)?),
                STAFF => rows.staff.fill_slot(slot, StaffRow::from_py(object)?),
                REQUEST => rows.support_requests.fill_slot(slot, RequestRow::from_py(object)?),
                _ => rows.active_tasks.fill_slot(slot, TaskRow::from_py(object)?),
            }
            Ok(())
        })
        .map_err(pyo3_misfit)??;
        Ok(slot)
    }

    /// Attach an object under a key, or taint. Never raises for a value that does not fit.
    fn attach(
        slf: &Bound<'_, Self>,
        kind: u8,
        key: &Bound<'_, PyAny>,
        value: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let py = slf.py();
        if slf.get().is_tainted(py)? {
            return Ok(());
        }
        match Self::attach_fitting(slf, kind, key, value) {
            Ok(()) => Ok(()),
            Err(misfit) => slf.get().taint(py, misfit.0),
        }
    }

    fn attach_fitting(
        slf: &Bound<'_, Self>,
        kind: u8,
        key: &Bound<'_, PyAny>,
        value: &Bound<'_, PyAny>,
    ) -> Result<(), Misfit> {
        let py = slf.py();
        let key = <Arc<str>>::from_py(key)?;
        if !exact_type_is(value, class_of(installed()?, kind)) {
            return Err(Misfit::new("not the entity class of this map"));
        }
        if !value.getattr(intern!(py, "_store")).or_misfit()?.is_none() {
            return Err(Misfit::new("an entity has two places"));
        }
        let slot = match Self::load_into_table(slf, kind, value) {
            Ok(slot) => slot,
            Err(misfit) => {
                // Half linked, it would hold a list or needs of a row that does not exist.
                Self::unlink_children(slf, kind, value).or_misfit()?;
                return Err(misfit);
            }
        };
        Self::mark_attached(slf, value, slot).or_misfit()?;
        let this = slf.get();
        this.rows_mut(py, |rows| {
            count(&this.attaches);
            let table = rows.table_mut(kind);
            if let Some(old) = table.bind(key, slot) {
                table.release_slot(old);
            }
        })
        .map_err(pyo3_misfit)
    }

    /// The object left the map, or is going to be replaced. It works on as a plain object.
    fn detach(
        slf: &Bound<'_, Self>,
        kind: u8,
        key: Option<&Bound<'_, PyAny>>,
        value: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let py = slf.py();
        if let Some(key) = key {
            if let Ok(key) = key.downcast_exact::<PyString>() {
                let key = key.to_str()?;
                slf.get().rows_mut(py, |rows| rows.table_mut(kind).remove(key))?;
            }
        }
        // Anything but the map's class was never attached: it may not have these attributes.
        if value.hasattr(intern!(py, "_store"))? && Self::attached_here(slf, value)?.is_some() {
            Self::unlink_children(slf, kind, value)?;
            Self::mark_detached(value)?;
            count(&slf.get().detaches);
        }
        Ok(())
    }

    fn write_row(
        slf: &Bound<'_, Self>,
        kind: u8,
        slot: u32,
        name: &str,
        value: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let this = slf.get();
        this.rows_mut(slf.py(), |rows| {
            count(&this.forwards);
            if rows.tainted().is_some() {
                return;
            }
            fn set_in<R>(
                table: &mut Table<R>,
                slot: u32,
                apply: impl FnOnce(&mut R) -> Result<(), Misfit>,
            ) -> Result<(), Misfit> {
                match table.slot_mut(slot) {
                    Some(row) => apply(row),
                    None => Err(Misfit::new("an attached object has no row")),
                }
            }
            let result = match kind {
                AGENT => set_in(&mut rows.agents, slot, |row| row.set(name, value)),
                NEEDS => set_in(&mut rows.agents, slot, |row| row.needs.set(name, value)),
                FACILITY => set_in(&mut rows.facilities, slot, |row| row.set(name, value)),
                STAFF => set_in(&mut rows.staff, slot, |row| row.set(name, value)),
                REQUEST => set_in(&mut rows.support_requests, slot, |row| row.set(name, value)),
                _ => set_in(&mut rows.active_tasks, slot, |row| row.set(name, value)),
            };
            if let Err(misfit) = result {
                this.taint_rows(rows, misfit.0);
            }
        })
    }

    /// `facility.queue_agent_ids = value`.
    fn set_list(
        slf: &Bound<'_, Self>,
        kind: u8,
        object: &Bound<'_, PyAny>,
        slot: u32,
        field: usize,
        name: &Bound<'_, PyString>,
        value: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let old = object.getattr(name)?;
        if old.is(value) {
            return Ok(());
        }
        // Whatever comes next, the old list leaves: nothing may write to a row by its stale node.
        Self::free_list(slf, &old)?;
        let py = slf.py();
        if slf.get().is_tainted(py)? {
            return raw_setattr(object, name, value.as_ptr());
        }
        match Self::adopt_list(slf, value, list_node(slot, field)) {
            Ok(stored) => {
                raw_setattr(object, name, stored.as_ptr())?;
                Self::write_row(slf, kind, slot, name.to_str()?, &stored)
            }
            Err(misfit) => {
                raw_setattr(object, name, value.as_ptr())?;
                slf.get().taint(py, misfit.0)
            }
        }
    }

    /// `agent.needs = value`.
    fn set_needs(
        slf: &Bound<'_, Self>,
        object: &Bound<'_, PyAny>,
        slot: u32,
        name: &Bound<'_, PyString>,
        value: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let old = object.getattr(name)?;
        if old.is(value) {
            return Ok(());
        }
        raw_setattr(object, name, value.as_ptr())?;
        // Whatever comes next, the old object leaves: its slot may be another agent's later.
        if exact_type_is(&old, class_of(installed().map_err(misfit_error)?, NEEDS))
            && Self::attached_here(slf, &old)?.is_some()
        {
            Self::mark_detached(&old)?;
        }
        let py = slf.py();
        if slf.get().is_tainted(py)? {
            return Ok(());
        }
        if let Err(misfit) = Self::link_needs(slf, value, slot) {
            return slf.get().taint(py, misfit.0);
        }
        Self::write_row(slf, AGENT, slot, name.to_str()?, value)
    }
}

fn pyo3_misfit<E: std::fmt::Display>(error: E) -> Misfit {
    Misfit::new(error.to_string())
}

fn misfit_error(misfit: Misfit) -> PyErr {
    PyRuntimeError::new_err(misfit.0)
}

fn dict_items<'py>(dict: &Bound<'py, PyDict>) -> Vec<(Bound<'py, PyAny>, Bound<'py, PyAny>)> {
    let py = dict.py();
    // SAFETY: as in world_core::dict_values: nothing here runs Python code, and each step takes
    // new references to what it borrowed.
    unsafe {
        let mut rows = Vec::with_capacity(ffi::PyDict_Size(dict.as_ptr()).max(0) as usize);
        let mut position = 0;
        let mut key = null_mut();
        let mut value = null_mut();
        while ffi::PyDict_Next(dict.as_ptr(), &mut position, &mut key, &mut value) != 0 {
            rows.push((Bound::from_borrowed_ptr(py, key), Bound::from_borrowed_ptr(py, value)));
        }
        rows
    }
}

fn new_id_map<'py>(py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
    let kind = id_map_type();
    if kind.is_null() {
        return Err(PyRuntimeError::new_err("core/state_mirror.py is not imported"));
    }
    // SAFETY: kind is the class handed to install_id_map_type, alive for the life of the process.
    let class = unsafe { Bound::from_borrowed_ptr(py, kind.cast()) };
    class.call0()
}

#[pymethods]
impl EntityStore {
    /// `object.name = value` on an attached object, from its `__setattr__`.
    fn write(
        slf: &Bound<'_, Self>,
        object: &Bound<'_, PyAny>,
        kind: u8,
        slot: u32,
        name: &Bound<'_, PyString>,
        value: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let text = name.to_str()?;
        if let Some(field) = list_field_of(kind, text) {
            return Self::set_list(slf, kind, object, slot, field, name, value);
        }
        if kind == AGENT && text == "needs" {
            return Self::set_needs(slf, object, slot, name, value);
        }
        // Before the row, and with no borrow: a property's setter comes back through here.
        raw_setattr(object, name, value.as_ptr())?;
        Self::write_row(slf, kind, slot, text, value)
    }

    /// Something was done to an object that no row can follow, such as deleting a field.
    fn taint_now(&self, py: Python<'_>, why: String) -> PyResult<()> {
        self.taint(py, why)
    }

    /// `map[key] = value` on a bound `IdMap`: what stores it also attaches it.
    fn map_set(
        slf: &Bound<'_, Self>,
        map: &Bound<'_, PyAny>,
        kind: u8,
        key: &Bound<'_, PyAny>,
        value: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        // A key that can't be hashed fails here, with dict's own error, before any change.
        let old = raw_dict_get(map, key)?;
        if old.as_ref().is_some_and(|old| old.is(value)) {
            return Ok(());
        }
        if let Some(old) = &old {
            // Not by key: the new row takes the old key's place in the order.
            Self::detach(slf, kind, None, old)?;
        }
        Self::attach(slf, kind, key, value)?;
        raw_dict_set(map, key, value)
    }

    /// The value left a bound `IdMap`.
    fn map_removed(
        slf: &Bound<'_, Self>,
        kind: u8,
        key: &Bound<'_, PyAny>,
        value: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        Self::detach(slf, kind, Some(key), value)
    }

    /// A new `IdMap` bound to this store with the entries of value, which is what the world
    /// stores for one of its fields. `old` is the map it replaces, if any, whose entries are
    /// detached unless value holds them too, and which becomes a plain holder of what it held.
    /// Anything but a mapping is stored as it is, for validation to say what is wrong with it.
    fn assign<'py>(
        slf: &Bound<'py, Self>,
        kind: u8,
        old: &Bound<'py, PyAny>,
        value: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let this = slf.get();
        let old_map = old
            .downcast::<PyDict>()
            .ok()
            .filter(|map| map.get_type().as_ptr() == id_map_type().cast());
        let bound_here = match old_map {
            Some(map) => map.getattr(intern!(py, "_store"))?.is(slf),
            None => false,
        };
        let old_items = match old_map {
            Some(map) if bound_here => dict_items(map),
            _ => Vec::new(),
        };
        if bound_here {
            let map = old_map.expect("bound_here implies a map");
            map.setattr(intern!(py, "_store"), py.None())?;
        }
        // Keep the slots of what stays, so assigning a filtered dict costs one read per entry.
        let mut this_ids = IdMap::<u32>::default();
        let mut kept = FxHashSet::default();
        let result = match value.downcast::<PyDict>() {
            Err(_) => {
                this.taint(py, "a field of entities is not a mapping".to_owned())?;
                value.clone()
            }
            Ok(mapping) => {
                let items = dict_items(mapping);
                let map = new_id_map(py)?;
                let mut fresh = Vec::new();
                for (key, item) in &items {
                    raw_dict_set(&map, key, item)?;
                    let stays = bound_here
                        && !this.is_tainted(py)?
                        && item.get_type().is(class_of(installed().map_err(misfit_error)?, kind))
                        && key.is_exact_instance_of::<PyString>();
                    match stays.then(|| Self::attached_here(slf, item)).transpose()?.flatten() {
                        Some(slot) if kept.insert(slot) => {
                            this_ids.insert(Arc::from(key.downcast_exact::<PyString>()?.to_str()?), slot);
                        }
                        // The same object under two keys.
                        Some(_) => this.taint(py, "an entity has two places".to_owned())?,
                        None => fresh.push((key, item)),
                    }
                }
                this.rows_mut(py, |rows| {
                    if rows.tainted().is_none() {
                        rows.table_mut(kind).set_keys(this_ids);
                    }
                })?;
                // New rows go to the end of the keys; then the keys take the dict's order.
                for (key, item) in &fresh {
                    Self::attach(slf, kind, key, item)?;
                }
                if !fresh.is_empty() && !kept.is_empty() {
                    Self::reorder_keys(slf, kind, &items)?;
                }
                map.setattr(intern!(py, "_store"), slf)?;
                map.setattr(intern!(py, "_table"), kind)?;
                map
            }
        };
        for (_, item) in &old_items {
            let attached = item.hasattr(intern!(py, "_store"))?
                && Self::attached_here(slf, item)?.is_some_and(|slot| !kept.contains(&slot));
            if attached {
                let slot = Self::attached_here(slf, item)?.expect("checked above");
                Self::detach(slf, kind, None, item)?;
                this.rows_mut(py, |rows| rows.table_mut(kind).release_slot(slot))?;
            }
        }
        Ok(result)
    }

    /// The rows of a tainted store, read again from the objects of the world. True if they all
    /// fit now.
    fn refit(slf: &Bound<'_, Self>, world: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = slf.py();
        let this = slf.get();
        this.rows_mut(py, Entities::clear)?;
        for (name, kind) in FIELDS {
            let map = world.getattr(name)?;
            let Ok(map) = map.downcast::<PyDict>() else {
                this.taint(py, "a field of entities is not a mapping".to_owned())?;
                return Ok(false);
            };
            let items = dict_items(map);
            for (_, item) in &items {
                // Attached before the taint: the rows those slots named are gone.
                let class = class_of(installed().map_err(misfit_error)?, kind);
                if exact_type_is(item, class) && Self::attached_here(slf, item)?.is_some() {
                    Self::unlink_children(slf, kind, item)?;
                    Self::mark_detached(item)?;
                }
            }
            for (key, item) in &items {
                Self::attach(slf, kind, key, item)?;
            }
        }
        Ok(!this.is_tainted(py)?)
    }

    /// Whether `state`, a world's `__dict__`, holds the maps these rows follow, with as many entries
    /// as they have. Something stored past `WorldState.__setattr__` and `IdMap` shows here, unless it
    /// keeps the length, which the Python rules that read the maps then see.
    fn follows(slf: &Bound<'_, Self>, state: &Bound<'_, PyDict>) -> PyResult<bool> {
        let py = slf.py();
        for (name, kind) in FIELDS {
            let Some(map) = raw_dict_get(state, PyString::intern(py, name).as_any())? else {
                return Ok(false);
            };
            let Ok(map) = map.downcast::<PyDict>() else {
                return Ok(false);
            };
            if map.get_type().as_ptr() != id_map_type().cast()
                || !map.getattr(intern!(py, "_store"))?.is(slf)
                || map.len() != slf.get().rows_ref(py, |rows| rows.table(kind).len())?
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// The first difference between the rows and the objects of the world, or None: the check
    /// behind the tests and the soak. A tainted store has no rows to differ.
    fn verify(slf: &Bound<'_, Self>, world: &Bound<'_, PyAny>) -> PyResult<Option<String>> {
        let py = slf.py();
        if slf.get().is_tainted(py)? {
            return Ok(None);
        }
        for (name, kind) in FIELDS {
            let map = world.getattr(name)?;
            let map = map.downcast::<PyDict>()?;
            let items = dict_items(map);
            let core = slf.get().core.bind(py).try_borrow()?;
            let rows = core.world().entities.try_borrow().or_py()?;
            if rows.table(kind).len() != items.len() {
                return Ok(Some(format!(
                    "{name}: {} rows, {} objects",
                    rows.table(kind).len(),
                    items.len()
                )));
            }
            fn differs<R: PyEntityField>(
                table: &Table<R>,
                name: &str,
                items: &[(Bound<'_, PyAny>, Bound<'_, PyAny>)],
            ) -> PyResult<Option<String>> {
                for ((id, row), (key, item)) in table.iter().zip(items) {
                    if key.downcast_exact::<PyString>()?.to_str()? != &**id {
                        return Ok(Some(format!("{name}: key {id}, in the dict {key}")));
                    }
                    let current = R::from_py(item).map_err(misfit_error)?;
                    if !current.same(row) {
                        return Ok(Some(format!("{name}.{id}: the row differs from the object")));
                    }
                }
                Ok(None)
            }
            let found = match kind {
                AGENT => differs(&rows.agents, name, &items)?,
                FACILITY => differs(&rows.facilities, name, &items)?,
                STAFF => differs(&rows.staff, name, &items)?,
                REQUEST => differs(&rows.support_requests, name, &items)?,
                _ => differs(&rows.active_tasks, name, &items)?,
            };
            if found.is_some() {
                return Ok(found);
            }
        }
        Ok(None)
    }

    /// A row as a new dict named like the dataclass's fields, or None.
    fn row(&self, py: Python<'_>, field: &str, id: &str) -> PyResult<Option<Py<PyAny>>> {
        fn document<R: PyEntityField>(py: Python<'_>, table: &Table<R>, id: &str) -> PyResult<Option<Py<PyAny>>> {
            table.get(id).map(|row| row.to_py(py)).transpose()
        }
        self.rows_ref(py, |rows| match field {
            "agents" => document(py, &rows.agents, id),
            "facilities" => document(py, &rows.facilities, id),
            "staff" => document(py, &rows.staff, id),
            "support_requests" => document(py, &rows.support_requests, id),
            "active_tasks" => document(py, &rows.active_tasks, id),
            _ => Err(PyValueError::new_err("not a field of entities")),
        })?
    }

    /// `{field: [bytes of each row]}` for a WorldState document of this version: what
    /// `canonical_json` writes for the items `WorldState._canonical_data` makes of the entities.
    /// Unsupported for a tainted store and for a value canonical JSON refuses (NaN).
    fn rows_json<'py>(&self, py: Python<'py>, version: i64) -> PyResult<Bound<'py, PyDict>> {
        let core = self.core.bind(py).try_borrow()?;
        let mut rows = core.world().entities.try_borrow_mut().or_py()?;
        if rows.tainted().is_some() {
            return Err(Unsupported::new_err("the entity store is tainted"));
        }
        let mut kept = self.row_json.try_borrow_mut().or_py()?;
        if kept.version != version {
            kept.tables = Default::default();
            kept.version = version;
        }
        let rows = &mut *rows;
        let [agents, facilities, staff, requests, tasks] = &mut kept.tables;
        let documents = PyDict::new(py);
        documents.set_item("agents", table_json(py, &mut rows.agents, agents, version)?)?;
        documents.set_item("facilities", table_json(py, &mut rows.facilities, facilities, version)?)?;
        documents.set_item("staff", table_json(py, &mut rows.staff, staff, version)?)?;
        documents.set_item("support_requests", table_json(py, &mut rows.support_requests, requests, version)?)?;
        documents.set_item("active_tasks", table_json(py, &mut rows.active_tasks, tasks, version)?)?;
        Ok(documents)
    }

    /// The keys of a table in the order of its dict.
    fn keys(&self, py: Python<'_>, field: &str) -> PyResult<Vec<String>> {
        fn keys_of<R>(table: &Table<R>) -> Vec<String> {
            table.iter().map(|(id, _)| id.to_string()).collect()
        }
        self.rows_ref(py, |rows| match field {
            "agents" => Ok(keys_of(&rows.agents)),
            "facilities" => Ok(keys_of(&rows.facilities)),
            "staff" => Ok(keys_of(&rows.staff)),
            "support_requests" => Ok(keys_of(&rows.support_requests)),
            "active_tasks" => Ok(keys_of(&rows.active_tasks)),
            _ => Err(PyValueError::new_err("not a field of entities")),
        })?
    }

    #[getter]
    fn tainted(&self, py: Python<'_>) -> PyResult<Option<String>> {
        self.rows_ref(py, |rows| rows.tainted().map(str::to_owned))
    }

    /// Counters for the budget of a lane and the soak.
    fn stats<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let (agents, requests) = self.rows_ref(py, |rows| (rows.agents.len(), rows.support_requests.len()))?;
        let stats = PyDict::new(py);
        stats.set_item("forwards", self.forwards.load(Ordering::Relaxed))?;
        stats.set_item("attaches", self.attaches.load(Ordering::Relaxed))?;
        stats.set_item("detaches", self.detaches.load(Ordering::Relaxed))?;
        stats.set_item("taints", self.taints.load(Ordering::Relaxed))?;
        stats.set_item("agents", agents)?;
        stats.set_item("support_requests", requests)?;
        Ok(stats)
    }

    fn __traverse__(&self, visit: PyVisit<'_>) -> Result<(), PyTraverseError> {
        visit.call(&self.core)
    }

    // ---- The sink of a `TrackedList`, which is an ID list of an entity ----------------------

    /// `list.append(value)`.
    fn list_append(
        slf: &Bound<'_, Self>,
        list: &Bound<'_, PyAny>,
        node: u32,
        value: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        raw_list_append(list, value)?;
        Self::list_changed(slf, list, node)
    }

    /// The value a list stores for value: an ID list holds str, not nodes.
    fn adopt(&self, _node: &Bound<'_, PyAny>, _key: &Bound<'_, PyAny>, value: Py<PyAny>) -> Py<PyAny> {
        value
    }

    fn release(&self, _value: &Bound<'_, PyAny>) {}

    /// The list changed somehow. An ID list is short, so its row is read again whatever it was.
    #[pyo3(signature = (list, node, _operation, *_arguments))]
    fn forward(
        slf: &Bound<'_, Self>,
        list: &Bound<'_, PyAny>,
        node: u32,
        _operation: &Bound<'_, PyAny>,
        _arguments: &Bound<'_, PyTuple>,
    ) -> PyResult<()> {
        Self::list_changed(slf, list, node)
    }
}

impl EntityStore {
    fn list_changed(slf: &Bound<'_, Self>, list: &Bound<'_, PyAny>, node: u32) -> PyResult<()> {
        let field = (node % LIST_KINDS) as usize;
        let slot = node / LIST_KINDS;
        let (name, kind) = LIST_FIELDS[field];
        Self::write_row(slf, kind, slot, name, list)
    }

    /// Put the keys in the order of the dict, after new rows went to the end.
    fn reorder_keys(
        slf: &Bound<'_, Self>,
        kind: u8,
        items: &[(Bound<'_, PyAny>, Bound<'_, PyAny>)],
    ) -> PyResult<()> {
        let this = slf.get();
        let py = slf.py();
        if this.is_tainted(py)? {
            return Ok(());
        }
        let mut order = Vec::with_capacity(items.len());
        for (key, _) in items {
            order.push(key.downcast_exact::<PyString>()?.to_str()?);
        }
        this.rows_mut(py, |rows| rows.table_mut(kind).reorder_keys(&order))
    }

    /// Run f on the rows. f must not run Python code; kernels get Unsupported from a tainted
    /// store, and the Python rules run on the objects.
    pub(crate) fn with_rows<R>(
        slf: &Bound<'_, Self>,
        f: impl FnOnce(&mut Entities) -> PyResult<R>,
    ) -> PyResult<R> {
        let core = slf.get().core.bind(slf.py()).try_borrow()?;
        let mut rows = core.world().entities.try_borrow_mut().or_py()?;
        if rows.tainted().is_some() {
            return Err(Unsupported::new_err("the entity store is tainted"));
        }
        f(&mut rows)
    }
}

// `world.<name>`, and which table it is.
const FIELDS: [(&str, u8); 5] = [
    ("agents", AGENT),
    ("facilities", FACILITY),
    ("staff", STAFF),
    ("support_requests", REQUEST),
    ("active_tasks", TASK),
];

/// The store of a world, read now: call this again after any Python callback.
pub(crate) fn entities_of<'py>(world: &Bound<'py, PyAny>) -> PyResult<Bound<'py, EntityStore>> {
    WorldCore::entities(&core_of(world)?).map(|store| store.into_bound(world.py()))
}

/// Store a value a row already took in the attribute of its object: no `__setattr__` runs and
/// nothing is forwarded. A rule writes the row inside `with_rows`, then calls this outside it. A
/// `SupportRequestState` also has writes to report (`_write_listener`): see the lane's document.
pub(crate) fn write_field<'py>(
    world: &Bound<'py, PyAny>,
    field: &str,
    id: &str,
    name: &str,
    value: &Bound<'py, PyAny>,
) -> PyResult<()> {
    let py = world.py();
    let map = world.getattr(field)?;
    let key = PyString::new(py, id);
    let object = raw_dict_get(&map, key.as_any())?
        .ok_or_else(|| Unsupported::new_err("the world's map has no such ID"))?;
    raw_setattr(&object, &PyString::intern(py, name), value.as_ptr())
}

/// The template of a rule that writes an entity: read and write the row, then the object. Used by
/// the tests. Says Unsupported, before any change, for a tainted store or an unknown ID.
#[pyfunction]
fn add_agent_stress(world: &Bound<'_, PyAny>, id: &str, change: f64) -> PyResult<f64> {
    let py = world.py();
    let new = EntityStore::with_rows(&entities_of(world)?, |rows| {
        let row = rows
            .agents
            .get_mut(id)
            .ok_or_else(|| Unsupported::new_err("no such agent"))?;
        row.stress += change;
        Ok(row.stress)
    })?;
    write_field(world, "agents", id, "stress", new.into_pyobject(py)?.as_any())?;
    Ok(new)
}

/// A need of each agent in dict order, read from the rows only.
#[pyfunction]
fn hunger_of_agents(world: &Bound<'_, PyAny>) -> PyResult<Vec<f64>> {
    EntityStore::with_rows(&entities_of(world)?, |rows| {
        Ok(rows.agents.iter().map(|(_, row)| row.needs.hunger).collect())
    })
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<EntityStore>()?;
    m.add_function(wrap_pyfunction!(install, m)?)?;
    m.add_function(wrap_pyfunction!(add_agent_stress, m)?)?;
    m.add_function(wrap_pyfunction!(hunger_of_agents, m)?)?;
    Ok(())
}
