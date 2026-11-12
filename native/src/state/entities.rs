//! The entities of a world as Rust rows: agents with their needs, facilities, staff, support
//! requests, active tasks, zones and zone edges (docs/rust-port/lanes/p6-entity-windows.md). The
//! rows, `Table` and `Entities` are the core's (`sheltersim_core::entities`); here is what needs
//! Python.
//!
//! The rows are the truth (Issue #152, PR 6-1). The Python objects stay the dataclasses of
//! `domain/models.py`: a read is a plain attribute read of the instance, and each `__dict__` is a
//! cache equal to its row whenever Python runs. An object in a world's `IdMap` is attached: its
//! slots `_store` and `_slot` say where its row is (they stay out of `__dict__`), and its
//! `__setattr__` comes here (`write`: the object, then the row). `IdMap` reports what Python puts
//! in and takes out, and `TrackedList` what happens to the ID lists of a facility or a staff
//! member.
//!
//! Rust rules read and write the rows through `with_rows` (or the whole world through
//! `with_world`), and put in and take out rows by key. When the
//! borrow ends, before any Python runs, the objects follow (`EntityStore::flush`, rule R5 of
//! design-p5-p6.md): each field whose row and view differ is written into the object past its
//! `__setattr__`; a row the core put in gets an object made from it with no `__init__` (its
//! `profile`, `needs` and ID lists too) at the key's place in the map; a row the core took out
//! gives its object its last values, and the object leaves. Writing `obj.__dict__[name]` of an
//! attached object reaches no row, and the next flush of that field writes over it.
//!
//! The rows and their taint are the core's (`sheltersim_core::world::World::entities`), held by the
//! world's `WorldCore`; the store is a handle that holds that core, and the core holds it: a cycle
//! the collector breaks (`WorldCore.__clear__`). The store holds the attached objects by slot and
//! the bound maps (`Views`), which a flush writes; they hold the store, a cycle its
//! `__traverse__` shows.
//!
//! A value that does not fit a typed field taints the whole store, or the zones' part of it for a
//! zone or a zone edge (each part has its own taint). The Python object keeps what was written, so
//! the Python rules see it and fail with their own text; `with_rows` then says `Unsupported` until
//! `refit` reads every object again. A typed field takes only its exact
//! Python type, and an ID list only exact `str` items.
//!
//! No borrow of the rows is held across anything that may run Python code: a property setter
//! (`acute_distress`) writes back through here while `set_field` is storing.

use std::ptr::null_mut;
use std::sync::atomic::{AtomicPtr, AtomicU64, Ordering};
use std::sync::Arc;

use pyo3::exceptions::{PyKeyError, PyRuntimeError, PyValueError};
use pyo3::ffi;
use pyo3::gc::PyVisit;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyBytes, PyDict, PyFloat, PyInt, PyList, PyString, PyTuple, PyType};
use pyo3::IntoPyObjectExt;
use pyo3::PyTraverseError;
use rustc_hash::FxHashSet;
use sheltersim_core::cell::Shard;

pub(crate) use sheltersim_core::entities::{
    Activity, AgentRow, Entities, FacilityRow, Mobility, NeedRow, RequestRow, RequestStatus, Skills, StaffRow, TaskRow,
};
use sheltersim_core::entities::{
    is_zone_kind, AnyTable, Change, EdgeRow, EntityField, EntityRow, ProfileRow, RowFields, Table, ZoneRow, ACTIVITIES, AGENT, EDGE,
    FACILITY, MOBILITIES, NEEDS, REQUEST, STAFF, STATUSES, TASK, ZONE,
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
    /// `AgentProfile` and `InitialNeedProfile`: the frozen parts of an agent made from a row.
    profile: Py<PyType>,
    initial_needs: Py<PyType>,
    facility: Py<PyType>,
    staff: Py<PyType>,
    request: Py<PyType>,
    task: Py<PyType>,
    zone: Py<PyType>,
    edge: Py<PyType>,
    activity: Vec<Py<PyAny>>,
    mobility: Vec<Py<PyAny>>,
    status: Vec<Py<PyAny>>,
    /// `object.__new__`, which makes an object of a class without running its `__init__`.
    object_new: Py<PyAny>,
}

static INSTALLED: AtomicPtr<Installed> = AtomicPtr::new(null_mut());

fn installed() -> Result<&'static Installed, Misfit> {
    // SAFETY: the pointer comes from Box::leak in install and is never freed.
    unsafe { INSTALLED.load(Ordering::Acquire).as_ref() }
        .ok_or_else(|| Misfit::new("domain/models.py is not imported"))
}

/// Remember the classes and the members of the enums, in the order of the core's `ACTIVITIES` and
/// the others. Each member's `.value` must be the core's text, which is what a document holds.
/// They stay alive for the life of the process. `profile` and `initial_needs` are the classes of an
/// agent's `profile` and its `initial_needs`, for the objects made from a row.
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
    zone: Py<PyType>,
    edge: Py<PyType>,
    activity: Vec<Py<PyAny>>,
    mobility: Vec<Py<PyAny>>,
    status: Vec<Py<PyAny>>,
    profile: Py<PyType>,
    initial_needs: Py<PyType>,
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
    let object_new = py.import("builtins")?.getattr("object")?.getattr("__new__")?.unbind();
    let new = Box::leak(Box::new(Installed {
        agent, needs, profile, initial_needs, facility, staff, request, task, zone, edge, activity, mobility, status,
        object_new,
    }));
    INSTALLED.store(new, Ordering::Release);
    Ok(())
}

// ---- Typed fields ----------------------------------------------------------------------------

/// What a field of a row does with Python. `from_py` runs no Python code and takes only the exact
/// type.
pub(crate) trait PyEntityField: EntityField {
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

impl PyEntityField for bool {
    fn from_py(value: &Bound<'_, PyAny>) -> Result<Self, Misfit> {
        Ok(value.downcast_exact::<PyBool>().map_err(|_| Misfit::new("not a bool"))?.is_true())
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

        impl ViewRow for $name {
            fn cells(&self, py: Python<'_>, view: &Self, mask: u32, out: &mut Vec<Named>) {
                let mut bit = 1_u32;
                $(
                    if mask & bit != 0 {
                        // Interned once per process: no Python code runs here.
                        let name = intern!(py, stringify!($field)).clone().unbind();
                        out.push((name, ViewField::cell(&self.$field, &view.$field)));
                    }
                    bit <<= 1;
                )*
                let _ = bit;
            }

            fn view_fields<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
                let fields = PyDict::new(py);
                $(fields.set_item(intern!(py, stringify!($field)), ViewField::to_view(&self.$field, py)?)?;)*
                Ok(fields)
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
py_entity_row!(ZoneRow { id, name, capacity, display_x, display_y });
py_entity_row!(EdgeRow { id, from_zone_id, to_zone_id, travel_minutes, closed });


// ---- Views: the objects follow their rows ----------------------------------------------------

/// A value of a field, taken out of a row while the rows are borrowed and made a Python value
/// after (rule R4 of design-p5-p6.md: no Python while a row is borrowed).
pub(crate) enum Cell {
    Float(f64),
    Int(i64),
    Bool(bool),
    Str(Arc<str>),
    Null,
    Activity(Activity),
    Mobility(Mobility),
    Status(RequestStatus),
    /// An ID list, whose `TrackedList` takes the new items in place.
    Ids(Vec<Arc<str>>),
    Skills(Vec<Arc<str>>),
    /// An agent's needs: the bits of the fields to write into its `NeedState`.
    Needs(u32, NeedRow),
    /// A new `AgentProfile`.
    Profile(Box<ProfileRow>),
    Value(V),
}

/// A field's interned name and its value.
pub(crate) type Named = (Py<PyString>, Cell);

/// What a field of a row is in its object: `cell` for a flush (`view` is what the object holds
/// now), `to_view` for an object made from a row.
pub(crate) trait ViewField {
    fn cell(&self, view: &Self) -> Cell;
    fn to_view<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>>;
}

macro_rules! scalar_view {
    ($($kind:ty => |$value:ident| $cell:expr),+ $(,)?) => {
        $(impl ViewField for $kind {
            fn cell(&self, _view: &Self) -> Cell {
                let $value = self;
                $cell
            }
            fn to_view<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
                Ok(PyEntityField::to_py(self, py)?.into_bound(py))
            }
        })+
    };
}

scalar_view! {
    f64 => |value| Cell::Float(*value),
    i64 => |value| Cell::Int(*value),
    bool => |value| Cell::Bool(*value),
    Arc<str> => |value| Cell::Str(value.clone()),
    Activity => |value| Cell::Activity(*value),
    Mobility => |value| Cell::Mobility(*value),
    RequestStatus => |value| Cell::Status(*value),
    Skills => |value| Cell::Skills(value.0.clone()),
    V => |value| Cell::Value(value.clone()),
}

impl<T: ViewField> ViewField for Option<T> {
    fn cell(&self, view: &Self) -> Cell {
        match (self, view) {
            (None, _) => Cell::Null,
            (Some(value), Some(view)) => value.cell(view),
            (Some(value), None) => value.cell(value),
        }
    }
    fn to_view<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        match self {
            Some(value) => value.to_view(py),
            None => Ok(py.None().into_bound(py)),
        }
    }
}

/// An ID list: a free `TrackedList`, which the attach then links to its row.
impl ViewField for Vec<Arc<str>> {
    fn cell(&self, _view: &Self) -> Cell {
        Cell::Ids(self.clone())
    }
    fn to_view<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let list = new_tracked_list(py)?;
        // Out of any tree, as `TrackedList.__new__` leaves it; the attach links it.
        list.setattr(intern!(py, "_sink"), py.None())?;
        list.setattr(intern!(py, "_node"), py.None())?;
        for item in self {
            raw_list_append(&list, PyString::new(py, item).as_any())?;
        }
        Ok(list)
    }
}

/// An agent's `needs`, a `NeedState`.
impl ViewField for NeedRow {
    fn cell(&self, view: &Self) -> Cell {
        Cell::Needs(self.diff(view), self.clone())
    }
    fn to_view<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let installed = installed().map_err(misfit_error)?;
        new_view(py, &installed.needs, &self.view_fields(py)?, true)
    }
}

/// An agent's `profile`, a frozen `AgentProfile` whose `initial_needs` is an `InitialNeedProfile`.
impl ViewField for ProfileRow {
    fn cell(&self, _view: &Self) -> Cell {
        Cell::Profile(Box::new(self.clone()))
    }
    fn to_view<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let installed = installed().map_err(misfit_error)?;
        let fields = self.view_fields(py)?;
        let initial = new_view(py, &installed.initial_needs, &self.initial_needs.view_fields(py)?, false)?;
        fields.set_item(intern!(py, "initial_needs"), initial)?;
        new_view(py, &installed.profile, &fields, false)
    }
}

/// A row and its object: the cells of the fields of a mask, and the fields of a new object.
pub(crate) trait ViewRow: PyEntityField + RowFields + Clone {
    fn cells(&self, py: Python<'_>, view: &Self, mask: u32, out: &mut Vec<Named>);
    fn view_fields<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>>;
}

/// An object of a class made with no `__init__`, holding these fields: what Python's constructor
/// leaves after its `__post_init__` normalized them, as the row's types are those already. An
/// entity's class has the slot `_store` (None: not attached yet).
fn new_view<'py>(
    py: Python<'py>,
    class: &Py<PyType>,
    fields: &Bound<'py, PyDict>,
    entity: bool,
) -> PyResult<Bound<'py, PyAny>> {
    let installed = installed().map_err(misfit_error)?;
    let object = installed.object_new.bind(py).call1((class.bind(py),))?;
    if entity {
        raw_setattr(&object, intern!(py, "_store"), py.None().as_ptr())?;
    }
    raw_setattr(&object, intern!(py, "__dict__"), fields.as_ptr())?;
    Ok(object)
}

/// A row that a flush makes an object of.
trait MakeView {
    fn make<'py>(&self, py: Python<'py>, kind: u8) -> PyResult<Bound<'py, PyAny>>;
}

impl<R: ViewRow> MakeView for R {
    fn make<'py>(&self, py: Python<'py>, kind: u8) -> PyResult<Bound<'py, PyAny>> {
        let installed = installed().map_err(misfit_error)?;
        new_view(py, class_of(installed, kind), &self.view_fields(py)?, true)
    }
}

/// One thing a flush does, in order.
enum Step {
    /// Write these fields into the object of the slot.
    Fields { kind: u8, slot: u32, cells: Vec<Named> },
    /// The object of the slot gets the row's last values and leaves; so does its key, unless a new
    /// row took the key.
    Detach { kind: u8, slot: u32, cells: Vec<Named>, key: Arc<str>, drop_key: bool },
    /// A key whose row never had an object leaves the map, if it is there.
    DropKey { kind: u8, key: Arc<str> },
    /// Make the object of a row and store it under its key: a new key at the end, a key already
    /// there in its place. None: the row left again before the flush.
    Make { kind: u8, slot: u32, key: Arc<str>, row: Option<Box<dyn MakeView>> },
}

/// What a flush writes, taken from the rows while they are borrowed.
pub(crate) struct Flush(Vec<Step>);

/// What a fork makes its objects of (`EntityStore::fork_rows`, lane 6-6): by kind, in the order of
/// `FIELDS`, each key in its order with its slot and a copy of its row.
pub(crate) struct ForkRows(Vec<(u8, Vec<(Arc<str>, u32, Box<dyn MakeView>)>)>);

/// A copy of the row in a slot of a kind's table, to make its object from; None for a slot with
/// no row.
fn boxed_row(rows: &Entities, kind: u8, slot: u32) -> Option<Box<dyn MakeView>> {
    fn boxed<R: ViewRow + 'static>(table: &Table<R>, slot: u32) -> Option<Box<dyn MakeView>> {
        table.row_at(slot).map(|row| Box::new(row.clone()) as Box<dyn MakeView>)
    }
    match kind {
        AGENT => boxed(&rows.agents, slot),
        FACILITY => boxed(&rows.facilities, slot),
        STAFF => boxed(&rows.staff, slot),
        REQUEST => boxed(&rows.support_requests, slot),
        ZONE => boxed(&rows.zones, slot),
        EDGE => boxed(&rows.zone_edges, slot),
        _ => boxed(&rows.active_tasks, slot),
    }
}

fn plan_table<R: ViewRow + 'static>(py: Python<'_>, kind: u8, table: &mut Table<R>, steps: &mut Vec<Step>) {
    if !table.has_changes() {
        return;
    }
    for slot in table.take_touched() {
        if let Some((row, view)) = table.row_and_view(slot) {
            let mask = row.diff(view);
            if mask != 0 {
                let mut cells = Vec::with_capacity(mask.count_ones() as usize);
                row.cells(py, view, mask, &mut cells);
                row.copy_fields(mask, view);
                steps.push(Step::Fields { kind, slot, cells });
            }
        }
    }
    // The inserts with no object yet, by slot, and where their step is.
    let mut pending: Vec<(u32, usize)> = Vec::new();
    for change in table.take_journal() {
        match change {
            Change::Inserted { key, slot } => {
                pending.push((slot, steps.len()));
                steps.push(Step::Make { kind, slot, key, row: None });
            }
            Change::Removed { key, slot, row, view, replaced } => {
                if let Some(place) = pending.iter().position(|(pending, _)| *pending == slot) {
                    // Put in and taken out again: it never gets an object.
                    pending.remove(place);
                }
                match view {
                    Some(view) => {
                        let mut cells = Vec::new();
                        row.cells(py, &view, row.diff(&view), &mut cells);
                        steps.push(Step::Detach { kind, slot, cells, key, drop_key: !replaced });
                    }
                    None if !replaced => steps.push(Step::DropKey { kind, key }),
                    None => {}
                }
            }
        }
    }
    for (slot, index) in pending {
        table.set_view(slot);
        let made = table.row_at(slot).map(|row| Box::new(row.clone()) as Box<dyn MakeView>);
        if let Step::Make { row, .. } = &mut steps[index] {
            *row = made;
        }
    }
}

/// What the objects need to follow the rows, or None: the end of a borrow that may have written
/// them. The views of the rows take the values now, so the objects must get them (`apply_flush`).
pub(crate) fn plan_flush(py: Python<'_>, rows: &mut Entities) -> Option<Flush> {
    if !rows.has_changes() {
        return None;
    }
    let mut steps = Vec::new();
    plan_table(py, AGENT, &mut rows.agents, &mut steps);
    plan_table(py, FACILITY, &mut rows.facilities, &mut steps);
    plan_table(py, STAFF, &mut rows.staff, &mut steps);
    plan_table(py, REQUEST, &mut rows.support_requests, &mut steps);
    plan_table(py, TASK, &mut rows.active_tasks, &mut steps);
    plan_table(py, ZONE, &mut rows.zones, &mut steps);
    plan_table(py, EDGE, &mut rows.zone_edges, &mut steps);
    Some(Flush(steps))
}

/// Write the cells into an object, past its `__setattr__`: what the row holds already.
fn write_cells(object: &Bound<'_, PyAny>, cells: Vec<Named>) -> PyResult<()> {
    let py = object.py();
    for (name, cell) in cells {
        let name = name.into_bound(py);
        let value: Bound<'_, PyAny> = match cell {
            Cell::Float(value) => PyFloat::new(py, value).into_any(),
            Cell::Int(value) => value.into_pyobject(py)?.into_any(),
            Cell::Bool(value) => PyBool::new(py, value).to_owned().into_any(),
            Cell::Str(value) => PyString::new(py, &value).into_any(),
            Cell::Null => py.None().into_bound(py),
            Cell::Activity(value) => value.member(py)?,
            Cell::Mobility(value) => value.member(py)?,
            Cell::Status(value) => value.member(py)?,
            Cell::Ids(items) => {
                // In place: the list stays the node of its row.
                let list = object.getattr(&name)?;
                let items = PyList::new(py, items.iter().map(|item| &**item))?;
                // SAFETY: list is a live list (a TrackedList is one); the C call bypasses its
                // overrides, so nothing is forwarded, and takes its own references to the items.
                if !list.is_instance_of::<PyList>()
                    || unsafe { ffi::PyList_SetSlice(list.as_ptr(), 0, ffi::PY_SSIZE_T_MAX, items.as_ptr()) } != 0
                {
                    return Err(PyErr::take(py).unwrap_or_else(|| PyRuntimeError::new_err("an ID list is not a list")));
                }
                continue;
            }
            Cell::Skills(items) => PyTuple::new(py, items.iter().map(|item| &**item))?.into_any(),
            Cell::Needs(mask, needs) => {
                let target = object.getattr(&name)?;
                let mut cells = Vec::with_capacity(mask.count_ones() as usize);
                needs.cells(py, &needs, mask, &mut cells);
                write_cells(&target, cells)?;
                continue;
            }
            Cell::Profile(profile) => profile.to_view(py)?,
            Cell::Value(value) => crate::core::value::VPy::to_py(&value, py)?,
        };
        raw_setattr(object, &name, value.as_ptr())?;
    }
    Ok(())
}

/// Remove a key from a dict past its overrides, if it is there.
fn raw_dict_discard(dict: &Bound<'_, PyAny>, key: &Bound<'_, PyAny>) -> PyResult<()> {
    if raw_dict_get(dict, key)?.is_none() {
        return Ok(());
    }
    // SAFETY: dict is a live dict holding key; the C call bypasses the overrides of IdMap.
    if unsafe { ffi::PyDict_DelItem(dict.as_ptr(), key.as_ptr()) } != 0 {
        return Err(PyErr::fetch(dict.py()));
    }
    Ok(())
}

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
        ZONE => &installed.zone,
        EDGE => &installed.edge,
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
    views: Shard<Views>,
    /// What an open transaction keeps to give the objects back (`txn_begin`, lane 6-2).
    txn: Shard<Option<EntityTxn>>,
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

/// The objects of the rows by kind and slot, and the bound `IdMap` of each kind: where a flush
/// writes. An object is here while it is attached, and its row then has a view (`Table`).
#[derive(Default)]
struct Views {
    objects: [Vec<Option<Py<PyAny>>>; 8],
    maps: [Option<Py<PyAny>>; 8],
}

impl Views {
    fn set(&mut self, kind: u8, slot: u32, object: Option<Py<PyAny>>) {
        let objects = &mut self.objects[kind as usize];
        if objects.len() <= slot as usize {
            if object.is_none() {
                return;
            }
            objects.resize_with(slot as usize + 1, || None);
        }
        objects[slot as usize] = object;
    }

    fn get(&self, py: Python<'_>, kind: u8, slot: u32) -> Option<Py<PyAny>> {
        self.objects[kind as usize].get(slot as usize)?.as_ref().map(|object| object.clone_ref(py))
    }

    fn take(&mut self, kind: u8, slot: u32) -> Option<Py<PyAny>> {
        self.objects[kind as usize].get_mut(slot as usize)?.take()
    }
}

/// What a transaction keeps of the objects (lane 6-2): the bound maps and the objects by slot as
/// they were at its start, and the objects each entity held as its profile, needs and ID lists, so
/// a rollback gives back those very objects.
struct EntityTxn {
    maps: [Option<Py<PyAny>>; 8],
    objects: [Vec<Option<Py<PyAny>>>; 8],
    children: Vec<(u8, u32, Vec<(Py<PyString>, Py<PyAny>)>)>,
}

/// The attributes of an entity that hold objects of their own.
fn child_names(kind: u8) -> &'static [&'static str] {
    match kind {
        AGENT => &["profile", "needs"],
        FACILITY => &["active_agent_ids", "queue_agent_ids", "resource_active_agent_ids", "resource_queue_agent_ids"],
        STAFF => &["work_queue"],
        _ => &[],
    }
}

/// The values of the restored row of a slot into its object, which holds what the transaction's
/// body left (lane 6-2): the fields that differ, into the objects it holds (needs, ID lists), or
/// every field for an object a row can't read.
fn restore_object<R: KeyedRow>(slf: &Bound<'_, EntityStore>, slot: u32, object: &Bound<'_, PyAny>) -> PyResult<()> {
    let py = slf.py();
    let row = slf.get().rows_mut(py, |rows| R::table(rows).row_at(slot).cloned())?;
    let row = row.ok_or_else(|| PyRuntimeError::new_err("a restored object has no row"))?;
    match R::from_py(object) {
        Ok(current) => {
            let mut cells = Vec::new();
            row.cells(py, &current, row.diff(&current), &mut cells);
            write_cells(object, cells)
        }
        Err(_) => {
            for (name, value) in row.view_fields(py)?.iter() {
                raw_setattr(object, name.downcast::<PyString>()?, value.as_ptr())?;
            }
            Ok(())
        }
    }
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
            views: Shard::default(),
            txn: Shard::default(),
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

    /// Whether the rows of the kind's part (the people's, or the zones') are dropped.
    fn is_tainted(&self, py: Python<'_>, kind: u8) -> PyResult<bool> {
        self.rows_ref(py, |rows| rows.tainted_for(kind).is_some())
    }

    /// Drop every row of the kind's part; the first reason stays the reason.
    fn taint_rows(&self, rows: &mut Entities, kind: u8, why: String) {
        count(&self.taints);
        rows.taint_for(kind, why);
    }

    fn taint(&self, py: Python<'_>, kind: u8, why: String) -> PyResult<()> {
        self.rows_mut(py, |rows| self.taint_rows(rows, kind, why))
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
                ZONE => rows.zones.fill_slot(slot, ZoneRow::from_py(object)?),
                EDGE => rows.zone_edges.fill_slot(slot, EdgeRow::from_py(object)?),
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
        if slf.get().is_tainted(py, kind)? {
            return Ok(());
        }
        match Self::attach_fitting(slf, kind, key, value) {
            Ok(()) => Ok(()),
            Err(misfit) => slf.get().taint(py, kind, misfit.0),
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
        this.set_object(kind, slot, Some(value.clone().unbind())).map_err(pyo3_misfit)?;
        this.rows_mut(py, |rows| {
            count(&this.attaches);
            let table = rows.table_mut(kind);
            if let Some(old) = table.bind(key, slot) {
                table.release_slot(old);
            }
        })
        .map_err(pyo3_misfit)
    }

    fn set_object(&self, kind: u8, slot: u32, object: Option<Py<PyAny>>) -> PyResult<()> {
        // The old object, if any, is alive in its map or with its caller: no finalizer runs here.
        self.views.try_borrow_mut().or_py()?.set(kind, slot, object);
        Ok(())
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
        if value.hasattr(intern!(py, "_store"))? {
            if let Some(slot) = Self::attached_here(slf, value)? {
                Self::unlink_children(slf, kind, value)?;
                Self::mark_detached(value)?;
                slf.get().forget_object(kind, slot, value)?;
                count(&slf.get().detaches);
            }
        }
        Ok(())
    }

    /// The world died (`WorldCore::owner_died`, lane 6-5): every object it held and every bound
    /// map work on as plain objects with the values they have, which are their rows' (they follow
    /// the rows at every return to Python, rule R5). The rows are not reached after this.
    pub(crate) fn detach_all(slf: &Bound<'_, Self>) -> PyResult<()> {
        let py = slf.py();
        let views = std::mem::take(&mut *slf.get().views.try_borrow_mut().or_py()?);
        for (kind, objects) in views.objects.iter().enumerate() {
            for object in objects.iter().flatten() {
                let object = object.bind(py);
                Self::unlink_children(slf, kind as u8, object)?;
                Self::mark_detached(object)?;
                count(&slf.get().detaches);
            }
        }
        for map in views.maps.iter().flatten() {
            map.bind(py).setattr(intern!(py, "_store"), py.None())?;
        }
        drop(views);
        Ok(())
    }

    /// The object left its slot: the slot no longer names it.
    fn forget_object(&self, kind: u8, slot: u32, object: &Bound<'_, PyAny>) -> PyResult<()> {
        let mut views = self.views.try_borrow_mut().or_py()?;
        let held = views.objects[kind as usize].get(slot as usize).and_then(Option::as_ref);
        if held.is_some_and(|held| held.is(object)) {
            let gone = views.take(kind, slot);
            drop(views);
            drop(gone);
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
            if rows.tainted_for(kind).is_some() {
                return;
            }
            // The object holds the value already: the row and its view take it, and the slot is
            // not touched.
            fn set_in<R>(
                table: &mut Table<R>,
                slot: u32,
                apply: impl Fn(&mut R) -> Result<(), Misfit>,
            ) -> Result<(), Misfit> {
                match table.row_and_view_mut(slot) {
                    Some((row, view)) => {
                        apply(row)?;
                        view.map_or(Ok(()), apply)
                    }
                    None => Err(Misfit::new("an attached object has no row")),
                }
            }
            let result = match kind {
                AGENT => set_in(&mut rows.agents, slot, |row| row.set(name, value)),
                NEEDS => set_in(&mut rows.agents, slot, |row| row.needs.set(name, value)),
                FACILITY => set_in(&mut rows.facilities, slot, |row| row.set(name, value)),
                STAFF => set_in(&mut rows.staff, slot, |row| row.set(name, value)),
                REQUEST => set_in(&mut rows.support_requests, slot, |row| row.set(name, value)),
                ZONE => set_in(&mut rows.zones, slot, |row| row.set(name, value)),
                EDGE => set_in(&mut rows.zone_edges, slot, |row| row.set(name, value)),
                _ => set_in(&mut rows.active_tasks, slot, |row| row.set(name, value)),
            };
            if let Err(misfit) = result {
                this.taint_rows(rows, kind, misfit.0);
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
        if slf.get().is_tainted(py, kind)? {
            return raw_setattr(object, name, value.as_ptr());
        }
        match Self::adopt_list(slf, value, list_node(slot, field)) {
            Ok(stored) => {
                raw_setattr(object, name, stored.as_ptr())?;
                Self::write_row(slf, kind, slot, name.to_str()?, &stored)
            }
            Err(misfit) => {
                raw_setattr(object, name, value.as_ptr())?;
                slf.get().taint(py, kind, misfit.0)
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
        if slf.get().is_tainted(py, AGENT)? {
            return Ok(());
        }
        if let Err(misfit) = Self::link_needs(slf, value, slot) {
            return slf.get().taint(py, AGENT, misfit.0);
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

impl EntityStore {
    /// `follows`, of these maps.
    pub(crate) fn maps_follow(slf: &Bound<'_, Self>, state: &Bound<'_, PyDict>, fields: &[(&str, u8)]) -> PyResult<bool> {
        let py = slf.py();
        for &(name, kind) in fields {
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

    /// `follows` of every map, the zones' and the edges' too: for a check that reads their rows
    /// (`rules/after_tick.rs`).
    pub(crate) fn follows_every_map(slf: &Bound<'_, Self>, state: &Bound<'_, PyDict>) -> PyResult<bool> {
        Self::maps_follow(slf, state, &FIELDS)
    }
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

    /// Something was done to an object of this kind that no row can follow, such as deleting a
    /// field.
    #[pyo3(signature = (why, kind = AGENT))]
    fn taint_now(&self, py: Python<'_>, why: String, kind: u8) -> PyResult<()> {
        self.taint(py, kind, why)
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
                this.taint(py, kind, "a field of entities is not a mapping".to_owned())?;
                this.views.try_borrow_mut().or_py()?.maps[kind as usize] = None;
                value.clone()
            }
            Ok(mapping) => {
                let items = dict_items(mapping);
                let map = new_id_map(py)?;
                let mut fresh = Vec::new();
                for (key, item) in &items {
                    raw_dict_set(&map, key, item)?;
                    let stays = bound_here
                        && !this.is_tainted(py, kind)?
                        && item.get_type().is(class_of(installed().map_err(misfit_error)?, kind))
                        && key.is_exact_instance_of::<PyString>();
                    match stays.then(|| Self::attached_here(slf, item)).transpose()?.flatten() {
                        Some(slot) if kept.insert(slot) => {
                            this_ids.insert(Arc::from(key.downcast_exact::<PyString>()?.to_str()?), slot);
                        }
                        // The same object under two keys.
                        Some(_) => this.taint(py, kind, "an entity has two places".to_owned())?,
                        None => fresh.push((key, item)),
                    }
                }
                this.rows_mut(py, |rows| {
                    if rows.tainted_for(kind).is_none() {
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
                let old = this.views.try_borrow_mut().or_py()?.maps[kind as usize].replace(map.clone().unbind());
                drop(old);
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
        let objects = std::mem::take(&mut this.views.try_borrow_mut().or_py()?.objects);
        drop(objects);
        for (name, kind) in FIELDS {
            let map = world.getattr(name)?;
            let Ok(map) = map.downcast::<PyDict>() else {
                this.taint(py, kind, "a field of entities is not a mapping".to_owned())?;
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
        Ok(!this.is_tainted(py, AGENT)? && !this.is_tainted(py, ZONE)?)
    }

    /// Whether `state`, a world's `__dict__`, holds the maps these rows follow, with as many entries
    /// as they have. Something stored past `WorldState.__setattr__` and `IdMap` shows here, unless it
    /// keeps the length, which the Python rules that read the maps then see. The people's maps only
    /// (`follows_every_map` adds the zones' and the edges').
    fn follows(slf: &Bound<'_, Self>, state: &Bound<'_, PyDict>) -> PyResult<bool> {
        Self::maps_follow(slf, state, &PEOPLE)
    }

    /// The first difference between the rows and the objects of the world, or None: the check
    /// behind the tests, the soak and the debug runs that check every tick
    /// (`state_mirror.watch_entity_rows`). Each map's keys are its table's, in the same order (the
    /// map is the store's bound map), each object is the attached object of its key's slot, and
    /// it, its view and its row hold the same values. A tainted store has no rows to differ.
    fn verify(slf: &Bound<'_, Self>, world: &Bound<'_, PyAny>) -> PyResult<Option<String>> {
        let py = slf.py();
        let this = slf.get();
        for (name, kind) in FIELDS {
            if this.is_tainted(py, kind)? {
                continue;
            }
            let map = world.getattr(name)?;
            let bound = this.views.try_borrow().or_py()?.maps[kind as usize].as_ref().is_some_and(|held| held.is(&map));
            if !bound {
                return Ok(Some(format!("{name}: the world's map is not the rows' map")));
            }
            let map = map.downcast::<PyDict>()?;
            let items = dict_items(map);
            let core = this.core.bind(py).try_borrow()?;
            let rows = core.world().entities.try_borrow().or_py()?;
            if rows.table(kind).len() != items.len() {
                return Ok(Some(format!(
                    "{name}: {} rows, {} objects",
                    rows.table(kind).len(),
                    items.len()
                )));
            }
            if rows.has_changes() {
                return Ok(Some(format!("{name}: writes the objects have not followed")));
            }
            let views = this.views.try_borrow().or_py()?;
            fn differs<R: PyEntityField>(
                slf: &Bound<'_, EntityStore>,
                views: &Views,
                kind: u8,
                table: &Table<R>,
                name: &str,
                items: &[(Bound<'_, PyAny>, Bound<'_, PyAny>)],
            ) -> PyResult<Option<String>> {
                let py = slf.py();
                for ((id, slot), (key, item)) in table.slots().zip(items) {
                    if key.downcast_exact::<PyString>()?.to_str()? != &**id {
                        return Ok(Some(format!("{name}: key {id}, in the dict {key}")));
                    }
                    let held = views.objects[kind as usize].get(slot as usize).and_then(Option::as_ref);
                    if !held.is_some_and(|held| held.is(item)) || EntityStore::attached_here(slf, item)? != Some(slot) {
                        return Ok(Some(format!("{name}.{id}: the object is not the one of its row")));
                    }
                    let (Some(row), Some(view)) = (table.row_at(slot), table.view_at(slot)) else {
                        return Ok(Some(format!("{name}.{id}: the row has no view")));
                    };
                    let current = R::from_py(item).map_err(misfit_error)?;
                    if !current.same(row) || !view.same(row) {
                        return Ok(Some(format!("{name}.{id}: the row differs from the object")));
                    }
                }
                let _ = py;
                Ok(None)
            }
            let found = match kind {
                AGENT => differs(slf, &views, kind, &rows.agents, name, &items)?,
                FACILITY => differs(slf, &views, kind, &rows.facilities, name, &items)?,
                STAFF => differs(slf, &views, kind, &rows.staff, name, &items)?,
                REQUEST => differs(slf, &views, kind, &rows.support_requests, name, &items)?,
                ZONE => differs(slf, &views, kind, &rows.zones, name, &items)?,
                EDGE => differs(slf, &views, kind, &rows.zone_edges, name, &items)?,
                _ => differs(slf, &views, kind, &rows.active_tasks, name, &items)?,
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
            "zones" => document(py, &rows.zones, id),
            "zone_edges" => document(py, &rows.zone_edges, id),
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
            "zones" => Ok(keys_of(&rows.zones)),
            "zone_edges" => Ok(keys_of(&rows.zone_edges)),
            _ => Err(PyValueError::new_err("not a field of entities")),
        })?
    }

    /// Why the rows of the people's tables were dropped, or None.
    #[getter]
    fn tainted(&self, py: Python<'_>) -> PyResult<Option<String>> {
        self.rows_ref(py, |rows| rows.tainted().map(str::to_owned))
    }

    /// Why the rows of the zones and zone edges were dropped, or None.
    #[getter]
    fn zones_tainted(&self, py: Python<'_>) -> PyResult<Option<String>> {
        self.rows_ref(py, |rows| rows.zones_tainted().map(str::to_owned))
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
        visit.call(&self.core)?;
        // A store being written by a flush is visited next time.
        if let Ok(views) = self.views.try_borrow() {
            for object in views.objects.iter().flatten().flatten() {
                visit.call(object)?;
            }
            for map in views.maps.iter().flatten() {
                visit.call(map)?;
            }
        }
        if let Ok(txn) = self.txn.try_borrow() {
            if let Some(txn) = &*txn {
                for object in txn.objects.iter().flatten().flatten().chain(txn.maps.iter().flatten()) {
                    visit.call(object)?;
                }
                for (_, _, children) in &txn.children {
                    for (_, child) in children {
                        visit.call(child)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn __clear__(&self) {
        let taken = self.views.try_borrow_mut().map(|mut views| std::mem::take(&mut *views));
        drop(taken);
        let taken = self.txn.try_borrow_mut().map(|mut txn| txn.take());
        drop(taken);
    }

    // ---- A transaction (lane 6-2, `transaction` in core/state_mirror.py) ---------------------

    /// Start a transaction, after the core put each table in the order of the entities' IDs
    /// (`World::canonicalize`): each map of the world becomes a new bound `IdMap` of the same
    /// objects in that order, as `WorldState.from_dict(world.to_dict())` makes new dicts; the old
    /// one is a plain dict now. Keeps the old maps and the objects, for `txn_rollback`.
    /// Unsupported for a tainted store, or a world whose maps are not the bound ones.
    fn txn_begin(slf: &Bound<'_, Self>, world: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = slf.py();
        let this = slf.get();
        let state = world.getattr(intern!(py, "__dict__"))?;
        let orders = this.rows_ref(py, |rows| {
            if rows.tainted().is_some() || rows.zones_tainted().is_some() {
                return None;
            }
            Some(FIELDS.map(|(_, kind)| rows.table(kind).key_slots()))
        })?;
        let orders = orders.ok_or_else(|| Unsupported::new_err("the entity store is tainted"))?;
        let (maps, objects) = {
            let views = this.views.try_borrow().or_py()?;
            (
                views.maps.each_ref().map(|map| map.as_ref().map(|map| map.clone_ref(py))),
                views.objects.each_ref().map(|objects| objects.iter().map(|object| object.as_ref().map(|object| object.clone_ref(py))).collect::<Vec<_>>()),
            )
        };
        for (name, kind) in FIELDS {
            let held = raw_dict_get(&state, PyString::intern(py, name).as_any())?;
            let bound = maps[kind as usize].as_ref().is_some_and(|map| held.is_some_and(|held| held.is(map.bind(py))));
            if !bound {
                return Err(Unsupported::new_err("a map of entities is not the rows' map"));
            }
        }
        let mut new_maps = Vec::with_capacity(FIELDS.len());
        for ((_, kind), order) in FIELDS.iter().zip(&orders) {
            let map = new_id_map(py)?;
            for (key, slot) in order {
                let object = objects[*kind as usize]
                    .get(*slot as usize)
                    .and_then(Option::as_ref)
                    .ok_or_else(|| Unsupported::new_err("a row has no object"))?;
                raw_dict_set(&map, PyString::new(py, key).as_any(), object.bind(py))?;
            }
            new_maps.push(map);
        }
        // A profile whose provenance holds its keys in another order than the row, sorted now (an
        // object holds the order Python gave it): a new profile from the row, as from_dict makes.
        let unsorted: Vec<(u32, ProfileRow)> = this.rows_ref(py, |rows| {
            let agents = &rows.agents;
            orders[0]
                .iter()
                .filter_map(|(_, slot)| {
                    let (row, view) = (agents.row_at(*slot)?, agents.view_at(*slot)?);
                    let sorted = row.profile.provenance.same_in_order(&view.profile.provenance);
                    (!sorted).then(|| (*slot, row.profile.clone()))
                })
                .collect()
        })?;
        if !unsorted.is_empty() {
            this.rows_mut(py, |rows| {
                for (slot, profile) in &unsorted {
                    if let Some((_, view)) = rows.agents.row_and_view(*slot) {
                        view.profile = profile.clone();
                    }
                }
            })?;
        }
        let mut children = Vec::new();
        for (_, kind) in FIELDS {
            let names = child_names(kind);
            if names.is_empty() {
                continue;
            }
            for (slot, object) in objects[kind as usize].iter().enumerate() {
                if let Some(object) = object {
                    let mut held = Vec::with_capacity(names.len());
                    for name in names {
                        let name = PyString::intern(py, name);
                        held.push((name.clone().unbind(), object.bind(py).getattr(&name)?.unbind()));
                    }
                    children.push((kind, slot as u32, held));
                }
            }
        }
        for (slot, profile) in unsorted {
            if let Some(object) = objects[AGENT as usize].get(slot as usize).and_then(Option::as_ref) {
                raw_setattr(object.bind(py), intern!(py, "profile"), profile.to_view(py)?.as_ptr())?;
            }
        }
        for ((name, kind), map) in FIELDS.iter().zip(&new_maps) {
            map.setattr(intern!(py, "_store"), slf)?;
            map.setattr(intern!(py, "_table"), *kind)?;
            if let Some(old) = &maps[*kind as usize] {
                old.bind(py).setattr(intern!(py, "_store"), py.None())?;
            }
            state.set_item(PyString::intern(py, name), map)?;
        }
        {
            let mut views = this.views.try_borrow_mut().or_py()?;
            for ((_, kind), map) in FIELDS.iter().zip(&new_maps) {
                views.maps[*kind as usize] = Some(map.clone().unbind());
            }
        }
        *this.txn.try_borrow_mut().or_py()? = Some(EntityTxn { maps, objects, children });
        Ok(())
    }

    /// The transaction rolled back, after the core gave its rows back (`World::restore`): the
    /// objects the body made leave, those it took out come back to their slots, every object holds
    /// its row's values again in the objects it held (profile, needs, ID lists), and the world's
    /// maps are the old ones, bound again.
    fn txn_rollback(slf: &Bound<'_, Self>, world: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = slf.py();
        let this = slf.get();
        let Some(kept) = this.txn.try_borrow_mut().or_py()?.take() else {
            return Ok(());
        };
        let (current, current_maps) = {
            let mut views = this.views.try_borrow_mut().or_py()?;
            let current = std::mem::take(&mut views.objects);
            let maps = std::mem::take(&mut views.maps);
            views.objects = kept.objects.each_ref().map(|objects| objects.iter().map(|object| object.as_ref().map(|object| object.clone_ref(py))).collect());
            views.maps = kept.maps.each_ref().map(|map| map.as_ref().map(|map| map.clone_ref(py)));
            (current, maps)
        };
        for (_, kind) in FIELDS {
            let old: FxHashSet<usize> =
                kept.objects[kind as usize].iter().flatten().map(|object| object.as_ptr() as usize).collect();
            for object in current[kind as usize].iter().flatten() {
                let object = object.bind(py);
                if !old.contains(&(object.as_ptr() as usize)) && Self::attached_here(slf, object)?.is_some() {
                    Self::unlink_children(slf, kind, object)?;
                    Self::mark_detached(object)?;
                }
            }
        }
        let mut children = kept.children.iter().peekable();
        for (_, kind) in FIELDS {
            for (slot, object) in kept.objects[kind as usize].iter().enumerate() {
                let Some(object) = object else {
                    continue;
                };
                let object = object.bind(py);
                let slot = slot as u32;
                if Self::attached_here(slf, object)?.is_some() {
                    Self::unlink_children(slf, kind, object)?;
                }
                if let Some((_, _, held)) = children.next_if(|(owner, at, _)| *owner == kind && *at == slot) {
                    for (name, child) in held {
                        if !object.getattr(name.bind(py))?.is(child.bind(py)) {
                            raw_setattr(object, name.bind(py), child.as_ptr())?;
                        }
                    }
                }
                match kind {
                    AGENT => restore_object::<AgentRow>(slf, slot, object)?,
                    FACILITY => restore_object::<FacilityRow>(slf, slot, object)?,
                    STAFF => restore_object::<StaffRow>(slf, slot, object)?,
                    REQUEST => restore_object::<RequestRow>(slf, slot, object)?,
                    ZONE => restore_object::<ZoneRow>(slf, slot, object)?,
                    EDGE => restore_object::<EdgeRow>(slf, slot, object)?,
                    _ => restore_object::<TaskRow>(slf, slot, object)?,
                }
                Self::link_children(slf, kind, object, slot).map_err(misfit_error)?;
                Self::mark_attached(slf, object, slot)?;
            }
        }
        let state = world.getattr(intern!(py, "__dict__"))?;
        for (name, kind) in FIELDS {
            if let Some(map) = &current_maps[kind as usize] {
                map.bind(py).setattr(intern!(py, "_store"), py.None())?;
            }
            if let Some(map) = &kept.maps[kind as usize] {
                let map = map.bind(py);
                map.setattr(intern!(py, "_store"), slf)?;
                map.setattr(intern!(py, "_table"), kind)?;
                state.set_item(PyString::intern(py, name), map)?;
            }
        }
        drop(current);
        Ok(())
    }

    /// The transaction kept its writes: what it kept goes.
    fn txn_commit(&self) -> PyResult<()> {
        let kept = self.txn.try_borrow_mut().or_py()?.take();
        drop(kept);
        Ok(())
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
        if this.is_tainted(py, kind)? {
            return Ok(());
        }
        let mut order = Vec::with_capacity(items.len());
        for (key, _) in items {
            order.push(key.downcast_exact::<PyString>()?.to_str()?);
        }
        this.rows_mut(py, |rows| rows.table_mut(kind).reorder_keys(&order))
    }

    // ---- A fork (lane 6-6, `WorldCore.fork`) --------------------------------------------------

    /// What a fork of the world takes of these rows (`deepcopy(world)`): for each map, its keys in
    /// their order with their slots and a copy of each row. None, with nothing changed, when the
    /// objects are not all what the rows say: a tainted store, a map of the world that is not the
    /// bound one, or one whose keys, their order or their objects are not the table's (something
    /// written past `IdMap`). `state_mirror.deepcopy_world` then copies the world as before.
    pub(crate) fn fork_rows(slf: &Bound<'_, Self>, world: &Bound<'_, PyAny>) -> PyResult<Option<ForkRows>> {
        let py = slf.py();
        let this = slf.get();
        let orders = this.rows_ref(py, |rows| {
            if rows.tainted().is_some() || rows.zones_tainted().is_some() {
                return None;
            }
            Some(FIELDS.map(|(_, kind)| rows.table(kind).key_slots()))
        })?;
        let Some(orders) = orders else {
            return Ok(None);
        };
        let state = world.getattr(intern!(py, "__dict__"))?;
        let mut held = Vec::with_capacity(FIELDS.len());
        for (name, _) in FIELDS {
            held.push(raw_dict_get(&state, PyString::intern(py, name).as_any())?);
        }
        {
            // Reads of dicts and str only below: no Python code runs while the views are borrowed.
            let views = this.views.try_borrow().or_py()?;
            for (((_, kind), order), held) in FIELDS.iter().zip(&orders).zip(&held) {
                let objects = &views.objects[*kind as usize];
                let Some(map) = &views.maps[*kind as usize] else {
                    return Ok(None);
                };
                let map = map.bind(py);
                if !held.as_ref().is_some_and(|held| held.is(map)) {
                    return Ok(None);
                }
                let map = map.downcast::<PyDict>()?;
                if map.len() != order.len() {
                    return Ok(None);
                }
                for ((key, object), (id, slot)) in dict_items(map).iter().zip(order) {
                    let same_key = key.downcast_exact::<PyString>().is_ok_and(|key| key.to_str().is_ok_and(|key| key == &**id));
                    let attached = objects
                        .get(*slot as usize)
                        .and_then(Option::as_ref)
                        .is_some_and(|attached| attached.bind(py).is(object));
                    if !same_key || !attached {
                        return Ok(None);
                    }
                }
            }
        }
        let rows = this.rows_ref(py, |rows| {
            let mut kinds = Vec::with_capacity(FIELDS.len());
            for ((_, kind), order) in FIELDS.iter().zip(&orders) {
                let mut entries = Vec::with_capacity(order.len());
                for (key, slot) in order {
                    entries.push((key.clone(), *slot, boxed_row(rows, *kind, *slot)?));
                }
                kinds.push((*kind, entries));
            }
            Some(kinds)
        })?;
        Ok(rows.map(ForkRows))
    }

    /// The objects of a fork's store (`WorldCore.fork`): the object of each row made from the row,
    /// as a flush makes the object of a row the core put in, attached to its slot, in the order of
    /// the keys; and each kind's new bound map. Returns the maps by the world's field name.
    pub(crate) fn fork_objects<'py>(slf: &Bound<'py, Self>, rows: ForkRows) -> PyResult<Bound<'py, PyDict>> {
        let py = slf.py();
        let this = slf.get();
        let maps = PyDict::new(py);
        for ((name, kind), (made_kind, entries)) in FIELDS.iter().zip(rows.0) {
            debug_assert_eq!(*kind, made_kind);
            let map = new_id_map(py)?;
            for (key, slot, row) in entries {
                let object = row.make(py, *kind)?;
                Self::link_children(slf, *kind, &object, slot).map_err(misfit_error)?;
                Self::mark_attached(slf, &object, slot)?;
                this.set_object(*kind, slot, Some(object.clone().unbind()))?;
                count(&this.attaches);
                raw_dict_set(&map, PyString::new(py, &key).as_any(), &object)?;
            }
            map.setattr(intern!(py, "_store"), slf)?;
            map.setattr(intern!(py, "_table"), *kind)?;
            let old = this.views.try_borrow_mut().or_py()?.maps[*kind as usize].replace(map.clone().unbind());
            drop(old);
            maps.set_item(PyString::intern(py, name), map)?;
        }
        Ok(maps)
    }

    /// Run f on the rows. f must not run Python code; kernels get Unsupported from a tainted
    /// store, and the Python rules run on the objects. When f is done, with nothing borrowed, the
    /// objects follow what it wrote (`flush`): a Rust rule writes the rows only. They follow when
    /// f fails too, so the objects never hold less than the rows.
    pub(crate) fn with_rows<R>(
        slf: &Bound<'_, Self>,
        f: impl FnOnce(&mut Entities) -> PyResult<R>,
    ) -> PyResult<R> {
        let (result, flush) = {
            let core = slf.get().core.bind(slf.py()).try_borrow()?;
            let mut rows = core.world().entities.try_borrow_mut().or_py()?;
            if rows.tainted().is_some() {
                return Err(Unsupported::new_err("the entity store is tainted"));
            }
            let result = f(&mut rows);
            (result, plan_flush(slf.py(), &mut rows))
        };
        if let Some(flush) = flush {
            Self::flush(slf, flush)?;
        }
        result
    }

    /// Write what a borrow of the rows did into the objects (rule R5 of design-p5-p6.md), before
    /// any Python runs: the fields each touched row and its object differ in, the objects of the
    /// rows the core put in (made from the rows, at the key's place in the map), and the last
    /// values of the rows it took out, whose objects then leave. Nothing is borrowed here.
    pub(crate) fn flush(slf: &Bound<'_, Self>, flush: Flush) -> PyResult<()> {
        let py = slf.py();
        let this = slf.get();
        for step in flush.0 {
            match step {
                Step::Fields { kind, slot, cells } => {
                    let object = this.views.try_borrow().or_py()?.get(py, kind, slot);
                    let object = object.ok_or_else(|| PyRuntimeError::new_err("a row has no object"))?;
                    write_cells(object.bind(py), cells)?;
                }
                Step::Detach { kind, slot, cells, key, drop_key } => {
                    let object = this.views.try_borrow_mut().or_py()?.take(kind, slot);
                    let object = object.ok_or_else(|| PyRuntimeError::new_err("a row has no object"))?;
                    let object = object.into_bound(py);
                    write_cells(&object, cells)?;
                    Self::unlink_children(slf, kind, &object)?;
                    Self::mark_detached(&object)?;
                    count(&this.detaches);
                    if drop_key {
                        let map = this.map_of(py, kind)?;
                        let key = PyString::new(py, &key);
                        if raw_dict_get(&map, key.as_any())?.is_some_and(|held| held.is(&object)) {
                            raw_dict_discard(&map, key.as_any())?;
                        }
                    }
                }
                Step::DropKey { kind, key } => {
                    let map = this.map_of(py, kind)?;
                    raw_dict_discard(&map, PyString::new(py, &key).as_any())?;
                }
                Step::Make { kind, slot, key, row: Some(row) } => {
                    let object = row.make(py, kind)?;
                    Self::link_children(slf, kind, &object, slot).map_err(misfit_error)?;
                    Self::mark_attached(slf, &object, slot)?;
                    this.set_object(kind, slot, Some(object.clone().unbind()))?;
                    count(&this.attaches);
                    let map = this.map_of(py, kind)?;
                    raw_dict_set(&map, PyString::new(py, &key).as_any(), &object)?;
                }
                Step::Make { row: None, .. } => {}
            }
        }
        Ok(())
    }

    /// The bound `IdMap` of a kind, which a flush writes into.
    fn map_of<'py>(&self, py: Python<'py>, kind: u8) -> PyResult<Bound<'py, PyAny>> {
        let views = self.views.try_borrow().or_py()?;
        let map = views.maps[kind as usize].as_ref().map(|map| map.clone_ref(py));
        map.map(|map| map.into_bound(py)).ok_or_else(|| PyRuntimeError::new_err("the rows have no map"))
    }
}

// `world.<name>`, and which table it is: `ENTITY_FIELDS` of core/state_mirror.py.
const FIELDS: [(&str, u8); 7] = [
    ("agents", AGENT),
    ("facilities", FACILITY),
    ("staff", STAFF),
    ("support_requests", REQUEST),
    ("active_tasks", TASK),
    ("zones", ZONE),
    ("zone_edges", EDGE),
];

// The people's maps, which the kernels read as rows (`with_rows`, `rows_json`, `follows`).
const PEOPLE: [(&str, u8); 5] =
    [FIELDS[0], FIELDS[1], FIELDS[2], FIELDS[3], FIELDS[4]];

const _: () = assert!(!is_zone_kind(PEOPLE[4].1) && is_zone_kind(FIELDS[5].1) && is_zone_kind(FIELDS[6].1));

/// The store of a world, read now: call this again after any Python callback.
/// Whether the world's entity rows follow every map of its `__dict__`, the zones' and the edges'
/// too (`EntityStore::follows_every_map`): false for a world without the store or whose `__dict__`
/// is not a dict (lane P10-1c: the zones' capacities of `capacity_metrics`).
pub(crate) fn rows_follow_maps(world: &Bound<'_, PyAny>) -> PyResult<bool> {
    let Ok(store) = entities_of(world) else {
        return Ok(false);
    };
    let state = crate::core::row::attribute(world, pyo3::intern!(world.py(), "__dict__"))?;
    match state.downcast::<PyDict>() {
        Ok(state) => EntityStore::follows_every_map(&store, state),
        Err(_) => Ok(false),
    }
}

pub(crate) fn entities_of<'py>(world: &Bound<'py, PyAny>) -> PyResult<Bound<'py, EntityStore>> {
    WorldCore::entities(&core_of(world)?).map(|store| store.into_bound(world.py()))
}

/// A row of a table that a rule can put in or take out by its key.
pub(crate) trait KeyedRow: ViewRow + 'static {
    fn table(rows: &mut Entities) -> &mut Table<Self>;
}

macro_rules! keyed_row {
    ($($row:ident: $field:ident);+ $(;)?) => {
        $(impl KeyedRow for $row {
            fn table(rows: &mut Entities) -> &mut Table<Self> {
                &mut rows.$field
            }
        })+
    };
}

keyed_row! {
    AgentRow: agents;
    FacilityRow: facilities;
    StaffRow: staff;
    RequestRow: support_requests;
    TaskRow: active_tasks;
    ZoneRow: zones;
    EdgeRow: zone_edges;
}

/// The template of a rule that writes an entity: read and write the row; the object follows when
/// the borrow ends. Used by the tests. Says Unsupported, before any change, for a tainted store or
/// an unknown ID.
#[pyfunction]
fn add_agent_stress(world: &Bound<'_, PyAny>, id: &str, change: f64) -> PyResult<f64> {
    EntityStore::with_rows(&entities_of(world)?, |rows| {
        let row = rows
            .agents
            .get_mut(id)
            .ok_or_else(|| Unsupported::new_err("no such agent"))?;
        row.stress += change;
        Ok(row.stress)
    })
}

/// A need of each agent in dict order, read from the rows only.
#[pyfunction]
fn hunger_of_agents(world: &Bound<'_, PyAny>) -> PyResult<Vec<f64>> {
    EntityStore::with_rows(&entities_of(world)?, |rows| {
        Ok(rows.agents.iter().map(|(_, row)| row.needs.hunger).collect())
    })
}

/// What a rule of the core does to one table in one borrow, for the tests of the views: each op is
/// `("insert", key, row)` (row: an object with the row's attributes, read before the borrow),
/// `("remove", key)` (KeyError for a key the table lacks) or `("write", key, name, value)` (the
/// row only, as a Rust rule writes). The objects follow when the borrow ends.
#[pyfunction]
fn apply_row_ops(world: &Bound<'_, PyAny>, field: &str, ops: Vec<Bound<'_, PyTuple>>) -> PyResult<()> {
    fn run<R: KeyedRow + Forward>(world: &Bound<'_, PyAny>, ops: &[Bound<'_, PyTuple>]) -> PyResult<()> {
        enum Op<'py, R> {
            Insert(Arc<str>, R),
            Remove(String),
            Write(String, String, Bound<'py, PyAny>),
        }
        let mut planned = Vec::with_capacity(ops.len());
        for op in ops {
            let what: String = op.get_item(0)?.extract()?;
            let key: String = op.get_item(1)?.extract()?;
            planned.push(match what.as_str() {
                "insert" => Op::Insert(Arc::from(key), R::from_py(&op.get_item(2)?).map_err(misfit_error)?),
                "remove" => Op::Remove(key),
                "write" => Op::Write(key, op.get_item(2)?.extract()?, op.get_item(3)?),
                _ => return Err(PyValueError::new_err("not an op")),
            });
        }
        EntityStore::with_rows(&entities_of(world)?, |rows| {
            for op in planned {
                let table = R::table(rows);
                match op {
                    Op::Insert(key, row) => {
                        table.insert(key, row);
                    }
                    Op::Remove(key) => {
                        if !table.remove_row(&key) {
                            return Err(PyKeyError::new_err(key));
                        }
                    }
                    Op::Write(key, name, value) => {
                        let row = table.get_mut(&key).ok_or_else(|| PyKeyError::new_err(key.clone()))?;
                        row.set(&name, &value).map_err(misfit_error)?;
                    }
                }
            }
            Ok(())
        })
    }
    match field {
        "agents" => run::<AgentRow>(world, &ops),
        "facilities" => run::<FacilityRow>(world, &ops),
        "staff" => run::<StaffRow>(world, &ops),
        "support_requests" => run::<RequestRow>(world, &ops),
        "active_tasks" => run::<TaskRow>(world, &ops),
        "zones" => run::<ZoneRow>(world, &ops),
        "zone_edges" => run::<EdgeRow>(world, &ops),
        _ => Err(PyValueError::new_err("not a field of entities")),
    }
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<EntityStore>()?;
    m.add_function(wrap_pyfunction!(install, m)?)?;
    m.add_function(wrap_pyfunction!(add_agent_stress, m)?)?;
    m.add_function(wrap_pyfunction!(hunger_of_agents, m)?)?;
    m.add_function(wrap_pyfunction!(apply_row_ops, m)?)?;
    Ok(())
}
