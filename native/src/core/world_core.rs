//! What a world's Rust state hangs on: `WorldCore`, its `NamespaceMirror`s and the `Row` trait
//! (docs/rust-port/lanes/p4-world-core.md).
//!
//! A namespace is owned when Rust holds it in a typed row. Unported Python keeps reading the same
//! name in `world.model_state`, which is then a tree of the tracked containers of
//! `core/state_mirror.py`. A write from Python comes here with the node's number and is applied
//! to the row; a write from Rust goes to the row and then to the tree through the C API, which
//! does not run the containers' overrides. So the row and the tree hold the same values, and
//! `verify` compares their canonical JSON.
//!
//! A write that does not fit the row taints the namespace: the row is dropped, kernels get
//! `Unsupported`, and the Python rules run on the tree, which stays right whatever was written.
//!
//! The rows are the core's: `WorldCore` holds a `sheltersim_core::world::World`, whose `Slot` for
//! each owned name holds the row, its taint, whether it is detached, its generation (a stamp) and
//! its counters. A `NamespaceMirror` is a handle: the core, the slot's index, and the Python side
//! (the tree, the node table, the bytes a save was given). Rows hold no Python handles, so they
//! can be read without the GIL. A shim borrows the core shared and then the one slot it needs
//! (`Shard`); the same slot twice is a `RuntimeError`, never a panic. No borrow is held across
//! anything that may run Python code: each borrow below covers the tree side or the slot only. A
//! world can move to another core between two calls (`world.__dict__.update` in a copy-and-swap),
//! so a kernel asks `core_of(world)` again after every Python callback.

use std::marker::PhantomData;
use std::ptr::null_mut;
use std::sync::{Arc, OnceLock};

use indexmap::IndexMap;
use pyo3::exceptions::PyRuntimeError;
use pyo3::ffi;
use pyo3::gc::PyVisit;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::sync::GILOnceCell;
use pyo3::types::{PyBytes, PyDict, PyList, PyString, PyTuple};
use pyo3::PyTraverseError;
use sheltersim_core::cell::{Shard, ShardMut, BORROWED};
use sheltersim_core::stamp::Stamp;
use sheltersim_core::error::Error as CoreError;
use sheltersim_core::world::{Place, Slot, World, WorldCell};
use smallvec::SmallVec;

use super::row::{atom_to_py, Atom, PyField, PyItems, Scalar, TreeSink};
use super::tracked::{as_readable_dict, is_detached, tracked_dict_type, tracked_list_type};
use super::errors::OrPy;
use super::value::{V, VPy};
use crate::state::entities::EntityStore;
use crate::{encode, Encoder, Unsupported, MAX_DEPTH};

const NO_PARENT: u32 = u32::MAX;

pub(crate) use sheltersim_core::row::{Items, Part, Row, ValueRow};

/// A write from Python, at the node a path leads to. Values are the tree's stored values.
pub(crate) enum Write<'a, 'py> {
    Set { key: Part<'a>, value: &'a Bound<'py, PyAny> },
    Delete { key: Part<'a> },
    /// `del items[start:stop]`, of a list.
    DeleteRange { start: usize, stop: usize },
    Clear,
    Append { value: &'a Bound<'py, PyAny> },
    Extend { values: &'a Bound<'py, PyList> },
    Insert { index: usize, value: &'a Bound<'py, PyAny> },
    Reverse,
    /// The list now holds these items: a slice assignment, a sort or `*=`.
    ReplaceAll { items: &'a Bound<'py, PyList> },
}

pub(crate) use sheltersim_core::error::Misfit;

/// A Python call's error as a `Misfit` whose reason is the exception's text (`PyErr`'s
/// `Display`): `python_call().or_misfit()?`.
pub(crate) trait OrMisfit<T> {
    fn or_misfit(self) -> Result<T, Misfit>;
}

impl<T> OrMisfit<T> for PyResult<T> {
    #[inline]
    fn or_misfit(self) -> Result<T, Misfit> {
        self.map_err(|error| Misfit(error.to_string()))
    }
}

/// A namespace's state as Rust holds it (the core's `Row`), and what it does with Python. A lane
/// implements it in `state/<name>.rs` through `PyField`; `state/sample_namespace.rs` is the
/// template.
pub(crate) trait PyRow: Row {
    /// Apply a write from Python. Every part of a row is checked before the first change is made
    /// or the misfit drops the whole row, so a half-applied write can't be read.
    fn apply(&mut self, path: &[Part<'_>], write: &Write<'_, '_>) -> Result<(), Misfit>;
    /// New plain containers holding the state, in the order of the tree.
    fn to_py<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>>;
    /// `to_py(...).get(key)` without making the rest: None for a key the state does not have.
    fn part_to_py<'py>(&self, py: Python<'py>, key: &str) -> PyResult<Option<Bound<'py, PyAny>>>;
    /// `Row::part_items`, with the items made in Python.
    fn part_py_items(&self, key: &str) -> Option<&dyn PyItems>;
    /// `to_py`, with an empty list under each key of `emptied` that holds a list: the rest of a
    /// namespace whose lists a reader takes item by item (`NamespaceMirror.document_emptied`).
    fn to_py_emptied<'py>(&self, py: Python<'py>, emptied: &[&str]) -> PyResult<Bound<'py, PyAny>>;
}

impl<T: PyField> PyRow for T {
    fn apply(&mut self, path: &[Part<'_>], write: &Write<'_, '_>) -> Result<(), Misfit> {
        PyField::apply(self, path, write)
    }

    fn to_py<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        PyField::to_py(self, py)
    }

    fn part_to_py<'py>(&self, py: Python<'py>, key: &str) -> PyResult<Option<Bound<'py, PyAny>>> {
        PyField::part_to_py(self, py, key)
    }

    fn part_py_items(&self, key: &str) -> Option<&dyn PyItems> {
        PyField::part_py_items(self, key)
    }

    fn to_py_emptied<'py>(&self, py: Python<'py>, emptied: &[&str]) -> PyResult<Bound<'py, PyAny>> {
        PyField::to_py_emptied(self, py, emptied)
    }
}

/// `Items::slice_to_py` of items kept as their canonical JSON: what `json.loads` reads of each, as
/// the list's `to_py` does.
pub(crate) fn loads_items<'py, 'a>(
    py: Python<'py>,
    items: impl Iterator<Item = &'a [u8]>,
) -> PyResult<Bound<'py, PyList>> {
    let loads = py.import(intern!(py, "json"))?.getattr(intern!(py, "loads"))?;
    let list = PyList::empty(py);
    for item in items {
        list.append(loads.call1((std::str::from_utf8(item)?,))?)?;
    }
    Ok(list)
}

/// `to_py_emptied` of a value that knows nothing better: the whole, then the lists emptied.
pub(crate) fn emptied_after<'py>(whole: Bound<'py, PyAny>, emptied: &[&str]) -> PyResult<Bound<'py, PyAny>> {
    if let Ok(record) = whole.downcast::<PyDict>() {
        for key in emptied {
            if record.get_item(*key)?.is_some_and(|value| value.is_instance_of::<PyList>()) {
                record.set_item(*key, PyList::empty(whole.py()))?;
            }
        }
    }
    Ok(whole)
}

/// Reads a whole namespace, or says it does not fit.
pub(crate) type RowLoader = fn(&Bound<'_, PyAny>) -> Result<Box<dyn Row>, Misfit>;

/// What this crate does with the rows of one type. The core's slot holds a row as `dyn Row`; these
/// find its `PyRow` again by its type (`Row::as_any`).
pub(crate) struct PyRowOps {
    pub(crate) load: RowLoader,
    as_py: fn(&dyn Row) -> Option<&dyn PyRow>,
    as_py_mut: fn(&mut dyn Row) -> Option<&mut dyn PyRow>,
}

impl PyRowOps {
    fn py<'a>(&self, row: &'a dyn Row) -> PyResult<&'a dyn PyRow> {
        (self.as_py)(row).ok_or_else(|| PyRuntimeError::new_err("the slot holds a row of another type"))
    }

    fn py_mut<'a>(&self, row: &'a mut dyn Row) -> PyResult<&'a mut dyn PyRow> {
        (self.as_py_mut)(row).ok_or_else(|| PyRuntimeError::new_err("the slot holds a row of another type"))
    }
}

/// A row read whole from a Python value: every `PyField` (a `state_row!` struct with its
/// `py_row!`), and `ValueRow`, which any JSON fits.
pub(crate) trait LoadRow: PyRow + Sized {
    fn load(tree: &Bound<'_, PyAny>) -> Result<Self, Misfit>;
}

impl<T: PyField> LoadRow for T {
    fn load(tree: &Bound<'_, PyAny>) -> Result<Self, Misfit> {
        T::from_py(tree)
    }
}

impl LoadRow for ValueRow {
    fn load(tree: &Bound<'_, PyAny>) -> Result<Self, Misfit> {
        Ok(ValueRow(<V as VPy>::from_py(tree).or_misfit()?))
    }
}

fn load_boxed<T: LoadRow>(tree: &Bound<'_, PyAny>) -> Result<Box<dyn Row>, Misfit> {
    Ok(Box::new(T::load(tree)?))
}

fn as_py<T: LoadRow>(row: &dyn Row) -> Option<&dyn PyRow> {
    row.as_any().downcast_ref::<T>().map(|row| row as &dyn PyRow)
}

fn as_py_mut<T: LoadRow>(row: &mut dyn Row) -> Option<&mut dyn PyRow> {
    row.as_any_mut().downcast_mut::<T>().map(|row| row as &mut dyn PyRow)
}

struct OpsOf<T>(PhantomData<T>);

impl<T: LoadRow> OpsOf<T> {
    const OPS: &'static PyRowOps = &PyRowOps { load: load_boxed::<T>, as_py: as_py::<T>, as_py_mut: as_py_mut::<T> };
}

/// A row's type as a Python object: `native.state("name").ROW`.
#[pyclass(frozen, module = "sheltersim_native")]
pub struct RowType {
    name: &'static str,
    ops: &'static PyRowOps,
}

impl RowType {
    /// The type of a namespace whose row is T: `RowType::of::<ThatRow>("name")`.
    pub(crate) fn of<T: LoadRow>(name: &'static str) -> Self {
        Self { name, ops: OpsOf::<T>::OPS }
    }
}

#[pymethods]
impl RowType {
    fn __repr__(&self) -> String {
        format!("<RowType {}>", self.name)
    }
}

fn value_at<'v>(root: &'v mut V, path: &[Part<'_>]) -> Result<&'v mut V, Misfit> {
    let mut target = root;
    for part in path {
        target = match (target, part) {
            (V::Map(map), Part::Key(key)) => map
                .get_mut(*key)
                .ok_or_else(|| Misfit::new(format!("no key {key:?}")))?,
            (V::List(items), Part::Index(index)) => items
                .get_mut(*index)
                .ok_or_else(|| Misfit::new(format!("no index {index}")))?,
            _ => return Err(Misfit::new("the path does not lead to a container")),
        };
    }
    Ok(target)
}

/// A forwarded write to a value: `ValueRow`'s, and the free-form part of a typed row's.
pub(crate) fn apply_to_value(
    root: &mut V,
    path: &[Part<'_>],
    write: &Write<'_, '_>,
) -> Result<(), Misfit> {
    let target = value_at(root, path)?;
    match (target, write) {
        (V::Map(map), Write::Set { key: Part::Key(key), value }) => {
            let value = <V as VPy>::from_py(value).or_misfit()?;
            match map.get_mut(*key) {
                Some(slot) => *slot = value,
                None => {
                    map.insert(Arc::from(*key), value);
                }
            }
        }
        (V::Map(map), Write::Delete { key: Part::Key(key) }) => {
            map.shift_remove(*key)
                .ok_or_else(|| Misfit::new(format!("no key {key:?}")))?;
        }
        (V::Map(map), Write::Clear) => map.clear(),
        (V::List(items), Write::Set { key: Part::Index(index), value }) => {
            *items
                .get_mut(*index)
                .ok_or_else(|| Misfit::new(format!("no index {index}")))? = <V as VPy>::from_py(value).or_misfit()?;
        }
        (V::List(items), Write::Delete { key: Part::Index(index) }) if *index < items.len() => {
            items.remove(*index);
        }
        (V::List(items), Write::DeleteRange { start, stop })
            if start <= stop && *stop <= items.len() =>
        {
            items.drain(*start..*stop);
        }
        (V::List(items), Write::Clear) => items.clear(),
        (V::List(items), Write::Append { value }) => items.push(<V as VPy>::from_py(value).or_misfit()?),
        (V::List(items), Write::Extend { values }) => {
            for value in values.iter() {
                items.push(<V as VPy>::from_py(&value).or_misfit()?);
            }
        }
        (V::List(items), Write::Insert { index, value }) if *index <= items.len() => {
            items.insert(*index, <V as VPy>::from_py(value).or_misfit()?);
        }
        (V::List(items), Write::Reverse) => items.reverse(),
        (V::List(items), Write::ReplaceAll { items: new_items }) => {
            *items = new_items
                .iter()
                .map(|value| <V as VPy>::from_py(&value))
                .collect::<PyResult<Vec<V>>>().or_misfit()?;
        }
        _ => return Err(Misfit::new("the write does not fit the value at its path")),
    }
    Ok(())
}

impl PyRow for ValueRow {
    fn apply(&mut self, path: &[Part<'_>], write: &Write<'_, '_>) -> Result<(), Misfit> {
        apply_to_value(&mut self.0, path, write)
    }

    fn to_py<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        VPy::to_py(&self.0, py)
    }

    fn part_to_py<'py>(&self, py: Python<'py>, key: &str) -> PyResult<Option<Bound<'py, PyAny>>> {
        match &self.0 {
            V::Map(map) => map.get(key).map(|value| VPy::to_py(value, py)).transpose(),
            _ => Ok(None),
        }
    }

    fn part_py_items(&self, key: &str) -> Option<&dyn PyItems> {
        match &self.0 {
            V::Map(map) => match map.get(key) {
                Some(V::List(items)) => Some(items as &dyn PyItems),
                _ => None,
            },
            _ => None,
        }
    }

    fn to_py_emptied<'py>(&self, py: Python<'py>, emptied: &[&str]) -> PyResult<Bound<'py, PyAny>> {
        emptied_after(VPy::to_py(&self.0, py)?, emptied)
    }
}

enum NodeKey {
    Root,
    Key(Py<PyString>),
    /// Found in the parent list by identity: indexes shift.
    ListItem,
    /// A dict key that is not an exact str. No path reaches below it, so a write there misfits.
    Other,
}

struct Node {
    parent: u32,
    key: NodeKey,
    /// The node, for a list only, whose items are searched. A dict node is reached by its keys.
    list: Option<Py<PyAny>>,
}

#[derive(Default)]
struct NodeTable {
    nodes: Vec<Option<Node>>,
    free: Vec<u32>,
    live: usize,
}

impl NodeTable {
    fn add(&mut self, node: Node) -> u32 {
        self.live += 1;
        if let Some(id) = self.free.pop() {
            self.nodes[id as usize] = Some(node);
            id
        } else {
            self.nodes.push(Some(node));
            (self.nodes.len() - 1) as u32
        }
    }

    fn remove(&mut self, id: u32) {
        if let Some(slot) = self.nodes.get_mut(id as usize) {
            if slot.take().is_some() {
                self.live -= 1;
                self.free.push(id);
            }
        }
    }

    /// The path from the root to a node, or a misfit if none can be written.
    fn path<'a>(&'a self, py: Python<'a>, id: u32) -> Result<SmallVec<[Part<'a>; 8]>, Misfit> {
        let mut path: SmallVec<[Part<'a>; 8]> = SmallVec::new();
        let mut current = id;
        loop {
            let node = self
                .nodes
                .get(current as usize)
                .and_then(Option::as_ref)
                .ok_or_else(|| Misfit::new("a node left the table"))?;
            match &node.key {
                NodeKey::Root => break,
                NodeKey::Key(key) => path.push(Part::Key(key.bind(py).to_str().or_misfit()?)),
                NodeKey::ListItem => path.push(Part::Index(self.index_in_parent(current, node)?)),
                NodeKey::Other => return Err(Misfit::new("a key is not a str")),
            }
            current = node.parent;
        }
        path.reverse();
        Ok(path)
    }

    fn index_in_parent(&self, own: u32, node: &Node) -> Result<usize, Misfit> {
        let parent = self
            .nodes
            .get(node.parent as usize)
            .and_then(Option::as_ref)
            .and_then(|parent| parent.list.as_ref())
            .ok_or_else(|| Misfit::new("a list item without its list"))?;
        // SAFETY: the table holds the list, the GIL is held and nothing here runs Python code.
        unsafe {
            let list = parent.as_ptr();
            let mut index = ffi::PyList_Size(list);
            while index > 0 {
                index -= 1;
                let item = ffi::PyList_GetItem(list, index);
                if node_number_is(item, own) {
                    return Ok(index as usize);
                }
            }
        }
        Err(Misfit::new("a list item is no longer in its list"))
    }
}

/// Whether an item of a tracked list is the node with this number.
///
/// # Safety
/// `item` is a live object, the GIL is held, and reading the slot runs no Python code.
unsafe fn node_number_is(item: *mut ffi::PyObject, id: u32) -> bool {
    if ffi::Py_TYPE(item) != tracked_dict_type() && ffi::Py_TYPE(item) != tracked_list_type() {
        return false;
    }
    let name = node_slot_name();
    let number = ffi::PyObject_GetAttr(item, name);
    if number.is_null() {
        ffi::PyErr_Clear();
        return false;
    }
    let found = ffi::PyLong_AsLong(number);
    ffi::Py_DecRef(number);
    if found == -1 && !ffi::PyErr_Occurred().is_null() {
        ffi::PyErr_Clear();
        return false;
    }
    found == id as std::os::raw::c_long
}

/// The interned name of the node slot, for code with only raw pointers. Never released.
fn node_slot_name() -> *mut ffi::PyObject {
    static NAME: OnceLock<usize> = OnceLock::new();
    // SAFETY: called with the GIL held; a text with no NUL makes a new interned str.
    *NAME.get_or_init(|| unsafe {
        ffi::PyUnicode_InternFromString(c"_node".as_ptr()) as usize
    }) as *mut ffi::PyObject
}

/// A dict of a namespace's tree, for a rule's writes: `with_row_tree` gives the root, `child` goes
/// down, and the setters `state_row!` writes store scalars here. The C API is used, so no override
/// runs and nothing is forwarded. A detached namespace has no tree, and these do nothing.
pub(crate) struct Tree<'py> {
    py: Python<'py>,
    node: Option<Bound<'py, PyAny>>,
}

impl<'py> Tree<'py> {
    pub(crate) fn py(&self) -> Python<'py> {
        self.py
    }

    pub(crate) fn child(&self, key: &Bound<'py, PyString>) -> PyResult<Tree<'py>> {
        let node = match &self.node {
            Some(node) => Some(
                raw_dict_get(node, key.as_any())?
                    .ok_or_else(|| PyRuntimeError::new_err("the tree does not have the row's key"))?,
            ),
            None => None,
        };
        Ok(Tree { py: self.py, node })
    }

    pub(crate) fn child_str(&self, key: &str) -> PyResult<Tree<'py>> {
        self.child(&PyString::new(self.py, key))
    }

    /// d.clear(), for a dict of scalars, which leaves the table of nodes as it is.
    pub(crate) fn clear(&self) -> PyResult<()> {
        if let Some(node) = &self.node {
            raw_dict_clear(node);
        }
        Ok(())
    }

    /// Put a dict's keys, which are exactly these, in this order with these key objects, unless
    /// the first is the first of them already. After a restore the order is that of the JSON.
    pub(crate) fn order_keys<const N: usize>(&self, keys: [&Bound<'py, PyString>; N]) -> PyResult<()> {
        let Some(node) = &self.node else {
            return Ok(());
        };
        if first_key_is(node, keys[0]) {
            return Ok(());
        }
        let mut values = Vec::with_capacity(N);
        for key in keys {
            values.push(
                raw_dict_get(node, key.as_any())?
                    .ok_or_else(|| PyRuntimeError::new_err("the tree does not have the row's key"))?,
            );
        }
        raw_dict_clear(node);
        for (key, value) in keys.iter().zip(&values) {
            raw_dict_set(node, key.as_any(), value)?;
        }
        Ok(())
    }

    pub(crate) fn set_str<T: Scalar>(&self, key: &str, value: &T) -> PyResult<()> {
        match &self.node {
            Some(node) => raw_dict_set(node, PyString::new(self.py, key).as_any(), &atom_to_py(self.py, value.atom())?),
            None => Ok(()),
        }
    }

    /// items.append(value) on a list of scalars, which leaves the table of nodes as it is.
    pub(crate) fn list_append<T: Scalar>(&self, value: &T) -> PyResult<()> {
        match &self.node {
            Some(node) => raw_list_append(node, &atom_to_py(self.py, value.atom())?),
            None => Ok(()),
        }
    }

    /// del items[0] on a non-empty list of scalars, which leaves the table of nodes as it is.
    pub(crate) fn list_remove_first(&self) -> PyResult<()> {
        if let Some(node) = &self.node {
            // SAFETY: node is a live list; the slice's item is an atom, released here.
            if unsafe { ffi::PyList_SetSlice(node.as_ptr(), 0, 1, null_mut()) } != 0 {
                return Err(PyErr::fetch(self.py));
            }
        }
        Ok(())
    }
}

/// The setters of the core's rows write the tree here: a field's key is interned, as `intern!`
/// makes it, and an entry's key is a new str. The row's type says the old value and the new one
/// are not containers, so the table of nodes stays right and no Python code runs: this is allowed
/// inside `with_row`.
impl<'py> TreeSink for Tree<'py> {
    type Error = PyErr;

    fn set_field(&self, key: &'static str, value: Atom<'_>) -> PyResult<()> {
        match &self.node {
            Some(node) => raw_dict_set(node, PyString::intern(self.py, key).as_any(), &atom_to_py(self.py, value)?),
            None => Ok(()),
        }
    }

    fn set_entry(&self, key: &str, value: Atom<'_>) -> PyResult<()> {
        match &self.node {
            Some(node) => raw_dict_set(node, PyString::new(self.py, key).as_any(), &atom_to_py(self.py, value)?),
            None => Ok(()),
        }
    }
}

/// The tree of one owned namespace in one world, and the sink of its tracked nodes. The row and
/// what is said of it (taint, detach, generation, counters) are the core's: the slot at `index`
/// of the `World` that `core` holds. The handle reaches its slot through a shared borrow of the
/// core and a borrow of that slot alone (design-p5-p6.md, R3).
#[pyclass(frozen, module = "sheltersim_native")]
pub struct NamespaceMirror {
    core: Py<WorldCore>,
    index: usize,
    name: Arc<str>,
    row_type: Py<RowType>,
    tree: Shard<TreeSide>,
}

/// The Python side of a namespace: its tree and what a save was given.
#[derive(Default)]
struct TreeSide {
    root: Option<Py<PyAny>>,
    table: NodeTable,
    /// The bytes objects the last `items_json` of each list gave, by its key: kept for the items
    /// whose bytes are still the same, so a save knows them by identity.
    items_given: Vec<(Box<str>, Vec<Py<PyBytes>>)>,
}

/// A new empty tracked container of the class given, without its Python `__new__`.
fn new_container<'py>(py: Python<'py>, list: bool) -> PyResult<Bound<'py, PyAny>> {
    type NewFunction = unsafe extern "C" fn(
        *mut ffi::PyTypeObject,
        *mut ffi::PyObject,
        *mut ffi::PyObject,
    ) -> *mut ffi::PyObject;
    static NEW: OnceLock<[usize; 2]> = OnceLock::new();
    let functions = NEW.get_or_init(|| {
        // SAFETY: both built-in types live forever, and tp_new is always set on them.
        unsafe {
            [
                ffi::PyType_GetSlot(std::ptr::addr_of_mut!(ffi::PyDict_Type), ffi::Py_tp_new) as usize,
                ffi::PyType_GetSlot(std::ptr::addr_of_mut!(ffi::PyList_Type), ffi::Py_tp_new) as usize,
            ]
        }
    });
    let kind = if list { tracked_list_type() } else { tracked_dict_type() };
    if kind.is_null() {
        return Err(PyRuntimeError::new_err("core/state_mirror.py is not imported"));
    }
    let arguments = PyTuple::empty(py);
    // SAFETY: the address is tp_new of dict or list, kind is a subclass of that built-in type
    // that adds only slots, and those functions run no Python code.
    unsafe {
        let new = std::mem::transmute::<usize, NewFunction>(functions[list as usize]);
        Bound::from_owned_ptr_or_err(py, new(kind, arguments.as_ptr(), null_mut()))
    }
}

/// A new empty `TrackedList`, out of any tree.
pub(crate) fn new_tracked_list(py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
    new_container(py, true)
}

fn exact_str_key<'py>(key: &Bound<'py, PyAny>) -> NodeKey {
    match key.downcast_exact::<PyString>() {
        Ok(text) => NodeKey::Key(text.clone().unbind()),
        Err(_) => NodeKey::Other,
    }
}

fn is_atom(value: &Bound<'_, PyAny>) -> bool {
    value.is_none()
        || value.is_exact_instance_of::<pyo3::types::PyFloat>()
        || value.is_exact_instance_of::<PyString>()
        || value.is_exact_instance_of::<pyo3::types::PyInt>()
        || value.is_exact_instance_of::<pyo3::types::PyBool>()
}

pub(crate) fn raw_dict_set(dict: &Bound<'_, PyAny>, key: &Bound<'_, PyAny>, value: &Bound<'_, PyAny>) -> PyResult<()> {
    // SAFETY: all are live objects; PyDict_SetItem takes its own references, and the type is a
    // dict subclass whose override this call means not to run.
    if unsafe { ffi::PyDict_SetItem(dict.as_ptr(), key.as_ptr(), value.as_ptr()) } != 0 {
        return Err(PyErr::fetch(dict.py()));
    }
    Ok(())
}

fn raw_dict_clear(dict: &Bound<'_, PyAny>) {
    // SAFETY: dict is a live dict. Its values are released here; a caller keeps any node it needs.
    unsafe { ffi::PyDict_Clear(dict.as_ptr()) }
}

/// The first key of a non-empty dict, without a new reference.
fn first_key_is(dict: &Bound<'_, PyAny>, key: &Bound<'_, PyString>) -> bool {
    // SAFETY: dict is a live dict, and the key found is only compared.
    unsafe {
        let mut position = 0;
        let mut found = null_mut();
        let mut value = null_mut();
        ffi::PyDict_Next(dict.as_ptr(), &mut position, &mut found, &mut value) != 0
            && found == key.as_ptr()
    }
}

pub(crate) fn raw_list_append(list: &Bound<'_, PyAny>, value: &Bound<'_, PyAny>) -> PyResult<()> {
    // SAFETY: as above.
    if unsafe { ffi::PyList_Append(list.as_ptr(), value.as_ptr()) } != 0 {
        return Err(PyErr::fetch(list.py()));
    }
    Ok(())
}

/// A strong reference to a dict's value, or None; a failed hash is an error.
pub(crate) fn raw_dict_get<'py>(
    dict: &Bound<'py, PyAny>,
    key: &Bound<'py, PyAny>,
) -> PyResult<Option<Bound<'py, PyAny>>> {
    let py = dict.py();
    // SAFETY: dict is a live dict; the value is borrowed until the new reference below.
    unsafe {
        let value = ffi::PyDict_GetItemWithError(dict.as_ptr(), key.as_ptr());
        if value.is_null() {
            return match PyErr::take(py) {
                Some(error) => Err(error),
                None => Ok(None),
            };
        }
        Ok(Some(Bound::from_borrowed_ptr(py, value)))
    }
}

fn dict_values<'py>(dict: &Bound<'py, PyAny>) -> Vec<(Bound<'py, PyAny>, Bound<'py, PyAny>)> {
    let py = dict.py();
    // SAFETY: dict is a live dict. Keys and values are borrowed for one step, each kept by the
    // new references made in that step, and nothing here runs Python code.
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

fn list_values<'py>(list: &Bound<'py, PyAny>) -> Vec<Bound<'py, PyAny>> {
    let py = list.py();
    // SAFETY: list is a live list, and nothing here runs Python code.
    unsafe {
        let count = ffi::PyList_Size(list.as_ptr());
        (0..count)
            .map(|index| Bound::from_borrowed_ptr(py, ffi::PyList_GetItem(list.as_ptr(), index)))
            .collect()
    }
}

/// Whether value is a tracked container, and of which kind: Some(true) for a list.
#[inline]
fn tracked_kind(value: &Bound<'_, PyAny>) -> Option<bool> {
    // SAFETY: the type pointer is only compared.
    let kind = unsafe { ffi::Py_TYPE(value.as_ptr()) };
    if kind == tracked_dict_type() {
        Some(false)
    } else if kind == tracked_list_type() {
        Some(true)
    } else {
        None
    }
}

impl NamespaceMirror {
    #[inline]
    fn ops(&self) -> &'static PyRowOps {
        self.row_type.get().ops
    }

    /// Run f on the slot, borrowed mutably. f must not run Python code.
    #[inline]
    fn with_slot_mut<R>(&self, py: Python<'_>, f: impl FnOnce(&mut Slot) -> R) -> PyResult<R> {
        let core = self.core.bind(py).try_borrow()?;
        let mut slot = core.slot(self.index)?.try_borrow_mut().or_py()?;
        Ok(f(&mut slot))
    }

    /// Run f on the slot, borrowed shared. f must not run Python code but to make plain values.
    #[inline]
    fn with_slot<R>(&self, py: Python<'_>, f: impl FnOnce(&Slot) -> R) -> PyResult<R> {
        let core = self.core.bind(py).try_borrow()?;
        let slot = core.slot(self.index)?.try_borrow().or_py()?;
        Ok(f(&slot))
    }

    /// A new generation: something was written.
    #[inline]
    fn touch(&self, py: Python<'_>) -> PyResult<()> {
        self.with_slot_mut(py, Slot::touch)
    }

    #[inline]
    fn tree_mut(&self) -> PyResult<ShardMut<'_, TreeSide>> {
        self.tree.try_borrow_mut().or_py()
    }

    #[inline]
    fn root<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        Ok(self.tree.try_borrow().or_py()?.root.as_ref().map(|root| root.bind(py).clone()))
    }

    fn add_node(
        slf: &Bound<'_, Self>,
        node: &Bound<'_, PyAny>,
        parent: u32,
        key: NodeKey,
        list: bool,
    ) -> PyResult<()> {
        let py = slf.py();
        let handle = list.then(|| node.clone().unbind());
        let id = slf.get().tree_mut()?.table.add(Node { parent, key, list: handle });
        node.setattr(intern!(py, "_sink"), slf)?;
        node.setattr(intern!(py, "_node"), id)?;
        Ok(())
    }

    /// What the tree stores for value: an atom as it is and a container as nodes. Any other value
    /// is stored as it is; a row can't read it, so a namespace holding one is tainted.
    fn adopt_value<'py>(
        slf: &Bound<'py, Self>,
        parent: u32,
        key: NodeKey,
        value: &Bound<'py, PyAny>,
        depth: usize,
    ) -> PyResult<Bound<'py, PyAny>> {
        if is_atom(value) || depth > MAX_DEPTH {
            return Ok(value.clone());
        }
        let py = value.py();
        let tracked = tracked_kind(value);
        // SAFETY: the type pointer is only compared.
        let kind = unsafe { ffi::Py_TYPE(value.as_ptr()) };
        let is_list = tracked == Some(true) || kind == std::ptr::addr_of_mut!(ffi::PyList_Type);
        let is_dict = tracked == Some(false) || kind == std::ptr::addr_of_mut!(ffi::PyDict_Type);
        if !is_list && !is_dict {
            return Ok(value.clone());
        }
        // A free node (out of any tree) moves in as itself. A node already in a tree, of this
        // namespace or another, is copied, so a node has one place: two names of one object can
        // not outlive a JSON copy-and-swap anyway, so no code can depend on them.
        let moved = tracked.is_some() && value.getattr(intern!(py, "_sink"))?.is_none();
        let node = if moved { value.clone() } else { new_container(py, is_list)? };
        Self::add_node(slf, &node, parent, key, is_list)?;
        let id: u32 = node.getattr(intern!(py, "_node"))?.extract()?;
        if is_dict {
            for (child_key, child) in dict_values(value) {
                let stored = Self::adopt_value(slf, id, exact_str_key(&child_key), &child, depth + 1)?;
                if !moved || !stored.is(&child) {
                    raw_dict_set(&node, &child_key, &stored)?;
                }
            }
        } else {
            for (index, child) in list_values(value).into_iter().enumerate() {
                let stored = Self::adopt_value(slf, id, NodeKey::ListItem, &child, depth + 1)?;
                if !moved {
                    raw_list_append(&node, &stored)?;
                } else if !stored.is(&child) {
                    // SAFETY: index is within the list; PyList_SetItem steals the new reference.
                    if unsafe { ffi::PyList_SetItem(node.as_ptr(), index as _, stored.into_ptr()) } != 0 {
                        return Err(PyErr::fetch(py));
                    }
                }
            }
        }
        Ok(node)
    }

    /// Take value and everything under it out of the table. It stays a working container: its
    /// later writes are forwarded to `next_sink`, or to nobody if that is None, like the plain
    /// dict it would be.
    fn release_value(
        slf: &Bound<'_, Self>,
        value: &Bound<'_, PyAny>,
        next_sink: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let Some(is_list) = tracked_kind(value) else {
            return Ok(());
        };
        let py = value.py();
        if !value.getattr(intern!(py, "_sink"))?.is(slf) {
            return Ok(());
        }
        let id: u32 = value.getattr(intern!(py, "_node"))?.extract()?;
        slf.get().tree_mut()?.table.remove(id);
        value.setattr(intern!(py, "_sink"), next_sink)?;
        value.setattr(intern!(py, "_node"), py.None())?;
        if is_list {
            for child in list_values(value) {
                Self::release_value(slf, &child, next_sink)?;
            }
        } else {
            for (_, child) in dict_values(value) {
                Self::release_value(slf, &child, next_sink)?;
            }
        }
        Ok(())
    }

    fn apply(slf: &Bound<'_, Self>, id: u32, write: &Write<'_, '_>) -> PyResult<()> {
        let py = slf.py();
        let this = slf.get();
        // A debug run counts the configs written in place inside a tick (`configs::note_write`).
        super::configs::note_write(py, &this.name)?;
        let tree = this.tree.try_borrow().or_py()?;
        let ops = this.ops();
        this.with_slot_mut(py, |slot| {
            slot.counts.forwards += 1;
            slot.touch();
            let Some(row) = slot.row.as_deref_mut() else {
                return Ok(());
            };
            let row = ops.py_mut(row)?;
            let result = tree.table.path(py, id).and_then(|path| row.apply(&path, write));
            if let Err(misfit) = result {
                slot.misfit(misfit.0);
            }
            Ok(())
        })?
    }

    fn set_root(slf: &Bound<'_, Self>, value: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        let py = slf.py();
        let this = slf.get();
        let root = Self::adopt_value(slf, NO_PARENT, NodeKey::Root, value, 0)?;
        let row = (this.ops().load)(&root);
        this.with_slot_mut(py, |slot| {
            match row {
                Ok(row) => {
                    slot.row = Some(row);
                    slot.taint = None;
                }
                Err(misfit) => slot.misfit(misfit.0),
            }
            slot.detached = false;
            slot.touch();
        })?;
        let old = this.tree_mut()?.root.replace(root.clone().unbind());
        drop(old);
        Ok(root.unbind())
    }

    fn drop_root(slf: &Bound<'_, Self>) -> PyResult<()> {
        let py = slf.py();
        let this = slf.get();
        this.touch(py)?;
        let old = this.tree_mut()?.root.take();
        if let Some(old) = old {
            Self::release_value(slf, old.bind(py), py.None().bind(py))?;
        }
        Ok(())
    }

    /// Write a value into the tree from Rust, after the row took it: no override runs and nothing
    /// is forwarded. A detached namespace has no tree to write. `path` leads to a dict.
    pub(crate) fn write_through<'py>(
        slf: &Bound<'py, Self>,
        path: &[&str],
        key: &str,
        value: &Bound<'py, PyAny>,
    ) -> PyResult<()> {
        let py = slf.py();
        let Some(mut node) = slf.get().root(py)? else {
            return Ok(());
        };
        for part in path {
            let part = PyString::new(py, part);
            node = raw_dict_get(&node, part.as_any())?
                .ok_or_else(|| Unsupported::new_err("the tree has no such path"))?;
        }
        let key = PyString::new(py, key);
        Self::store(slf, &node, key.as_any(), value)
    }

    /// `del node[key]` at this path of the tree from Rust, after the row dropped it, if the tree
    /// has it: no override runs and nothing is forwarded.
    pub(crate) fn remove_through<'py>(slf: &Bound<'py, Self>, path: &[&str], key: &str) -> PyResult<()> {
        let py = slf.py();
        slf.get().touch(py)?;
        let Some(mut node) = slf.get().root(py)? else {
            return Ok(());
        };
        for part in path {
            let part = PyString::new(py, part);
            node = raw_dict_get(&node, part.as_any())?
                .ok_or_else(|| PyRuntimeError::new_err("the tree does not have the row's key"))?;
        }
        let key = PyString::new(py, key);
        let Some(old) = raw_dict_get(&node, key.as_any())? else {
            return Ok(());
        };
        // SAFETY: both are live objects, and the type is a dict subclass whose override this call
        // means not to run.
        if unsafe { ffi::PyDict_DelItem(node.as_ptr(), key.as_ptr()) } != 0 {
            return Err(PyErr::fetch(py));
        }
        Self::release_value(slf, &old, py.None().bind(py))
    }

    /// The list at this path of the tree, from the root through dicts, or None for a detached
    /// namespace. `path` leads to a list.
    fn list_at<'py>(slf: &Bound<'py, Self>, path: &[&str]) -> PyResult<Option<Bound<'py, PyAny>>> {
        let py = slf.py();
        let Some(mut node) = slf.get().root(py)? else {
            return Ok(None);
        };
        for part in path {
            let part = PyString::new(py, part);
            node = raw_dict_get(&node, part.as_any())?
                .ok_or_else(|| Unsupported::new_err("the tree has no such path"))?;
        }
        Ok(Some(node))
    }

    /// items.append(value) in the tree from Rust, after the row took it. See `write_through`.
    pub(crate) fn append_through<'py>(
        slf: &Bound<'py, Self>,
        path: &[&str],
        value: &Bound<'py, PyAny>,
    ) -> PyResult<()> {
        slf.get().touch(slf.py())?;
        let Some(list) = Self::list_at(slf, path)? else {
            return Ok(());
        };
        let id: u32 = list.getattr(intern!(slf.py(), "_node"))?.extract()?;
        let stored = Self::adopt_value(slf, id, NodeKey::ListItem, value, 0)?;
        raw_list_append(&list, &stored)
    }

    /// del items[:count] in the tree from Rust, after the row dropped them. See `write_through`.
    pub(crate) fn drop_oldest_through(slf: &Bound<'_, Self>, path: &[&str], count: usize) -> PyResult<()> {
        let py = slf.py();
        slf.get().touch(py)?;
        let Some(list) = Self::list_at(slf, path)? else {
            return Ok(());
        };
        let list_pointer = list.as_ptr();
        // SAFETY: list is a live list, the indexes are within it when the row has this many, and
        // each item gets its own reference before it leaves the list.
        let old: Vec<Bound<'_, PyAny>> = unsafe {
            let size = ffi::PyList_Size(list_pointer) as usize;
            (0..count.min(size))
                .map(|index| Bound::from_borrowed_ptr(py, ffi::PyList_GetItem(list_pointer, index as ffi::Py_ssize_t)))
                .collect()
        };
        // SAFETY: a null value deletes the slice.
        if unsafe { ffi::PyList_SetSlice(list_pointer, 0, old.len() as ffi::Py_ssize_t, null_mut()) } != 0 {
            return Err(PyErr::fetch(py));
        }
        for item in &old {
            Self::release_value(slf, item, py.None().bind(py))?;
        }
        Ok(())
    }

    /// d[key] = value in the tree, keeping the table right, without touching the row.
    fn store<'py>(
        slf: &Bound<'py, Self>,
        node: &Bound<'py, PyAny>,
        key: &Bound<'py, PyAny>,
        value: &Bound<'py, PyAny>,
    ) -> PyResult<()> {
        let py = slf.py();
        slf.get().touch(py)?;
        let old = raw_dict_get(node, key)?;
        if old.as_ref().is_some_and(|old| old.is(value)) {
            return Ok(());
        }
        let id: u32 = node.getattr(intern!(py, "_node"))?.extract()?;
        let stored = Self::adopt_value(slf, id, exact_str_key(key), value, 0)?;
        raw_dict_set(node, key, &stored)?;
        if let Some(old) = old {
            Self::release_value(slf, &old, py.None().bind(py))?;
        }
        Ok(())
    }

    /// Make `root[field]`, a dict of records of scalars, hold these entries in this order, as a new
    /// dict of new dicts would: the cheapest write of a map whose keys change at every tick. The
    /// records already in the tree carry the new values, so they make no new nodes: a record has
    /// no meaning of its own, and nothing keeps one across a tick. One whose first key is not the
    /// interned first name, as after a restore, has another order, and leaves the tree. The row is
    /// the caller's to change; values must not be containers, and `entries` must not run Python.
    pub(crate) fn refill_record_map<'py, const N: usize>(
        slf: &Bound<'py, Self>,
        field: &Bound<'py, PyString>,
        names: [&Bound<'py, PyString>; N],
        entries: impl Iterator<Item = PyResult<(Bound<'py, PyString>, [Bound<'py, PyAny>; N])>>,
    ) -> PyResult<()> {
        let py = slf.py();
        let this = slf.get();
        this.touch(py)?;
        let Some(root) = this.root(py)? else {
            return Ok(());
        };
        let map = raw_dict_get(&root, field.as_any())?
            .ok_or_else(|| PyRuntimeError::new_err("the tree does not have the row's key"))?;
        let parent: u32 = map.getattr(intern!(py, "_node"))?.extract()?;
        let mut pool = Vec::new();
        let mut leaving = Vec::new();
        for (_, record) in dict_values(&map) {
            if tracked_kind(&record) == Some(false) && first_key_is(&record, names[0]) {
                pool.push(record);
            } else {
                leaving.push(record);
            }
        }
        raw_dict_clear(&map);
        for entry in entries {
            let (key, values) = entry?;
            let record = match pool.pop() {
                Some(record) => {
                    let id: usize = record.getattr(intern!(py, "_node"))?.extract()?;
                    let mut tree = this.tree_mut()?;
                    let node = tree.table.nodes.get_mut(id).and_then(Option::as_mut);
                    node.ok_or_else(|| PyRuntimeError::new_err("a record left the table"))?.key =
                        NodeKey::Key(key.clone().unbind());
                    record
                }
                None => {
                    let record = new_container(py, false)?;
                    Self::add_node(slf, &record, parent, NodeKey::Key(key.clone().unbind()), false)?;
                    record
                }
            };
            for (name, value) in names.iter().zip(&values) {
                raw_dict_set(&record, name.as_any(), value)?;
            }
            raw_dict_set(&map, key.as_any(), &record)?;
        }
        for record in pool.into_iter().chain(leaving) {
            Self::release_value(slf, &record, py.None().bind(py))?;
        }
        Ok(())
    }

    /// Run f on the row of type T. f must not run Python code; kernels get Unsupported from a
    /// tainted or absent namespace, and the Python rules run on the tree.
    #[inline]
    pub(crate) fn with_row<T: Row, R>(
        slf: &Bound<'_, Self>,
        f: impl FnOnce(&mut T) -> PyResult<R>,
    ) -> PyResult<R> {
        let this = slf.get();
        let core = this.core.bind(slf.py()).try_borrow()?;
        let mut slot = core.slot(this.index)?.try_borrow_mut().or_py()?;
        let row = slot
            .row
            .as_deref_mut()
            .ok_or_else(|| Unsupported::new_err("the namespace has no row"))?;
        let row = row
            .as_any_mut()
            .downcast_mut::<T>()
            .ok_or_else(|| Unsupported::new_err("the namespace has another row"))?;
        f(row)
    }

    /// with_row, with the root of the tree for mirrored setters. See `TreeSink for Tree`.
    pub(crate) fn with_row_tree<'py, T: Row, R>(
        slf: &Bound<'py, Self>,
        f: impl FnOnce(&mut T, &Tree<'py>) -> PyResult<R>,
    ) -> PyResult<R> {
        let py = slf.py();
        let this = slf.get();
        let core = this.core.bind(py).try_borrow()?;
        let mut slot = core.slot(this.index)?.try_borrow_mut().or_py()?;
        // What f writes through `Tree` does not pass the counted paths.
        slot.touch();
        let tree = Tree { py, node: this.root(py)? };
        let row = slot
            .row
            .as_deref_mut()
            .ok_or_else(|| Unsupported::new_err("the namespace has no row"))?;
        let row = row
            .as_any_mut()
            .downcast_mut::<T>()
            .ok_or_else(|| Unsupported::new_err("the namespace has another row"))?;
        f(row, &tree)
    }

    /// Run f on the row as this crate's `PyRow`, for a reader that makes Python values of it.
    /// Unsupported without a row.
    pub(crate) fn with_py_row<R>(slf: &Bound<'_, Self>, f: impl FnOnce(&dyn PyRow) -> PyResult<R>) -> PyResult<R> {
        let this = slf.get();
        let ops = this.ops();
        this.with_slot(slf.py(), |slot| {
            let row = slot.row.as_deref().ok_or_else(|| Unsupported::new_err("the namespace has no row"))?;
            f(ops.py(row)?)
        })?
    }

    /// Whether value is the root of this mirror's tree.
    pub(crate) fn root_is(&self, value: &Bound<'_, PyAny>) -> bool {
        self.tree
            .try_borrow()
            .is_ok_and(|tree| tree.root.as_ref().is_some_and(|root| root.as_ptr() == value.as_ptr()))
    }

    /// Whether `held`, what `model_state` holds under this namespace, is the state the row
    /// follows: the tree, or the placeholder while the namespace is detached and the row alone
    /// holds it (`state_mirror._table_holds_row_of`).
    pub(crate) fn holds_state(&self, held: &Bound<'_, PyAny>) -> bool {
        self.root_is(held)
            || (is_detached(held)
                && self.with_slot(held.py(), |slot| slot.detached && slot.row.is_some()).unwrap_or(false))
    }

    /// `generation_of(held)`, and the generation for the placeholder of a detached namespace too.
    pub(crate) fn held_generation(&self, held: &Bound<'_, PyAny>) -> Option<u64> {
        self.held_stamp(held).map(Stamp::get)
    }

    /// `held_generation` as the stamp.
    pub(crate) fn held_stamp(&self, held: &Bound<'_, PyAny>) -> Option<Stamp> {
        let root = self.root_is(held);
        self.with_slot(held.py(), |slot| {
            (root || (slot.detached && slot.row.is_some() && is_detached(held))).then_some(slot.generation)
        })
        .ok()
        .flatten()
    }

    /// Run f on the `V` of a namespace whose row is a `ValueRow`, for a kernel that reads a
    /// detached one; Unsupported without such a row. f must not run Python code (`with_row`).
    pub(crate) fn with_value<R>(slf: &Bound<'_, Self>, f: impl FnOnce(&V) -> PyResult<R>) -> PyResult<R> {
        Self::with_row::<ValueRow, _>(slf, |row| f(&row.0))
    }
}

/// What `model_state` holds under an owned namespace, for a kernel that reads it.
pub(crate) enum Held<'py> {
    /// The row is what the world holds: its tree, or the row alone while the namespace is detached.
    /// The row and the tree hold the same values, so a reader of the row reads what the tree has.
    Row(Bound<'py, NamespaceMirror>),
    /// Anything else, such as a tainted tree or a value stored past the table, for the rules to
    /// read as Python holds it.
    Value(Bound<'py, PyAny>),
}

/// `world.model_state.get(name)` as a kernel reads it, building no tree: None without the key.
/// Unsupported for a `model_state` kernels don't read.
pub(crate) fn held_state<'py>(world: &Bound<'py, PyAny>, name: &str) -> PyResult<Option<Held<'py>>> {
    let py = world.py();
    let table = world.getattr(intern!(py, "model_state"))?;
    let dict = as_readable_dict(&table).ok_or_else(|| Unsupported::new_err("model_state is not a plain dict"))?;
    let Some(value) = dict.get_item(name)? else {
        return Ok(None);
    };
    if let Ok(mirror) = mirror_of(world, name) {
        let this = mirror.get();
        if this.with_slot(py, |slot| slot.row.is_some())? && this.holds_state(&value) {
            return Ok(Some(Held::Row(mirror)));
        }
    }
    Ok(Some(Held::Value(value)))
}

/// `model_state[name].get(key)` of a detached namespace, for a kernel that only reads that part: a
/// new value made from that one field of its row (`NamespaceMirror.document_part`), or None for a
/// key the row does not have. For a reader of a part of the tree, when `held` is the placeholder.
pub(crate) fn detached_part<'py>(world: &Bound<'py, PyAny>, name: &str, key: &str) -> PyResult<Option<Bound<'py, PyAny>>> {
    let mirror = mirror_of(world, name)?;
    NamespaceMirror::with_py_row(&mirror, |row| row.part_to_py(world.py(), key))
}

/// `{"staff": ...}` of a detached `staff_operations_v1`: a reader of its staff records, which are
/// few, gets them as documents, as the tree would hold them.
pub(crate) fn detached_staff_operations<'py>(world: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyAny>>> {
    let py = world.py();
    let state = PyDict::new(py);
    if let Some(staff) = detached_part(world, "staff_operations_v1", "staff")? {
        state.set_item(intern!(py, "staff"), staff)?;
    }
    Ok(Some(state.into_any()))
}

/// A write to the row at this path, as a forwarded write from Python applies it, for a rule that
/// writes through the tree when the namespace is detached and has no tree: it writes the row this
/// way instead of making the tree again. A row checks a write before it changes anything, so a
/// write that does not fit changes nothing here and is Unsupported: a detached row is all there is
/// of its namespace, and must not be dropped.
pub(crate) fn apply_to_row(mirror: &Bound<'_, NamespaceMirror>, path: &[Part<'_>], write: &Write<'_, '_>) -> PyResult<()> {
    let this = mirror.get();
    let ops = this.ops();
    this.with_slot_mut(mirror.py(), |slot| {
        let Some(row) = slot.row.as_deref_mut() else {
            return Err(Unsupported::new_err("the namespace has no row"));
        };
        ops.py_mut(row)?.apply(path, write).map_err(|misfit| Unsupported::new_err(misfit.0))?;
        slot.counts.forwards += 1;
        slot.touch();
        Ok(())
    })?
}

/// `model_state.get(name)` through the C API, but a detached namespace comes back as its tree,
/// which Python rebuilds from the row (`_DetachingTable.__getitem__`): for a kernel that writes
/// through the tree, or reads it at an event. Unsupported for a `model_state` kernels don't read.
pub(crate) fn attached_state<'py>(table: &Bound<'py, PyAny>, name: &str) -> PyResult<Option<Bound<'py, PyAny>>> {
    let dict = as_readable_dict(table).ok_or_else(|| Unsupported::new_err("model_state is not a plain dict"))?;
    match dict.get_item(name)? {
        // PyObject_GetItem runs the override, which builds the tree and stores it in the table.
        Some(value) if is_detached(&value) => Ok(Some(table.get_item(name)?)),
        held => Ok(held),
    }
}

/// Run f on `model_state.get(name)` as a `V`, building no tree: the row of a detached namespace,
/// whose row is a `ValueRow`, or the tree read as it is; None for no such key. For a kernel that
/// only reads the namespace. f must not run Python code (`NamespaceMirror::with_value`).
pub(crate) fn with_state_value<R>(
    world: &Bound<'_, PyAny>,
    name: &str,
    f: impl FnOnce(Option<&V>) -> PyResult<R>,
) -> PyResult<R> {
    let table = world
        .getattr(intern!(world.py(), "model_state"))
        .map_err(|_| Unsupported::new_err("an input has no such attribute"))?;
    let dict = as_readable_dict(&table).ok_or_else(|| Unsupported::new_err("model_state is not a plain dict"))?;
    match dict.get_item(name)? {
        None => f(None),
        Some(held) if is_detached(&held) => NamespaceMirror::with_value(&mirror_of(world, name)?, |value| f(Some(value))),
        Some(held) => f(Some(&<V as VPy>::from_py(&held)?)),
    }
}

/// `model_state.get(name)` for a kernel that reads it with Python's own reads at a rare event: a
/// detached namespace comes back as a new document of its row (`state_mirror.read_only_state`),
/// which nothing follows, so it must never be written. The tree otherwise.
pub(crate) fn read_only_state<'py>(world: &Bound<'py, PyAny>, name: &str) -> PyResult<Option<Bound<'py, PyAny>>> {
    let py = world.py();
    let table = world
        .getattr(intern!(py, "model_state"))
        .map_err(|_| Unsupported::new_err("an input has no such attribute"))?;
    let dict = as_readable_dict(&table).ok_or_else(|| Unsupported::new_err("model_state is not a plain dict"))?;
    match dict.get_item(name)? {
        Some(held) if is_detached(&held) => {
            let mirror = mirror_of(world, name)?;
            let this = mirror.get();
            let ops = this.ops();
            this.with_slot(py, |slot| {
                let row = slot.row.as_deref().ok_or_else(|| PyRuntimeError::new_err("a detached namespace always has a row"))?;
                Ok(Some(ops.py(row)?.to_py(py)?))
            })?
        }
        held => Ok(held),
    }
}

/// `model_state.get(name)` as a new `V`, building no tree (`with_state_value`).
pub(crate) fn state_value(world: &Bound<'_, PyAny>, name: &str) -> PyResult<Option<V>> {
    with_state_value(world, name, |value| Ok(value.cloned()))
}

fn part_of<'a>(key: &'a Bound<'_, PyAny>) -> Result<Part<'a>, Misfit> {
    let text = key
        .downcast_exact::<PyString>()
        .map_err(|_| Misfit::new("a key is not a str"))?;
    Ok(Part::Key(text.to_str().or_misfit()?))
}

#[pymethods]
impl NamespaceMirror {
    /// d[key] = value on a bound TrackedDict: what stores it also forwards it.
    fn dict_set(
        slf: &Bound<'_, Self>,
        node: &Bound<'_, PyAny>,
        id: u32,
        key: &Bound<'_, PyAny>,
        value: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let py = slf.py();
        // A key that can't be hashed fails here, with dict's own error, before any change.
        let old = raw_dict_get(node, key)?;
        if old.as_ref().is_some_and(|old| old.is(value)) {
            // Reassigning what a key holds changes nothing, and is common after an edit in place.
            return Ok(());
        }
        let stored = Self::adopt_value(slf, id, exact_str_key(key), value, 0)?;
        raw_dict_set(node, key, &stored)?;
        if let Some(old) = old {
            Self::release_value(slf, &old, py.None().bind(py))?;
        }
        match part_of(key) {
            Ok(part) => Self::apply(slf, id, &Write::Set { key: part, value: &stored }),
            Err(misfit) => slf.get().with_slot_mut(py, |slot| {
                slot.misfit(misfit.0);
                slot.touch();
            }),
        }
    }

    /// items.append(value) on a bound TrackedList.
    fn list_append(
        slf: &Bound<'_, Self>,
        node: &Bound<'_, PyAny>,
        id: u32,
        value: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let stored = Self::adopt_value(slf, id, NodeKey::ListItem, value, 0)?;
        raw_list_append(node, &stored)?;
        Self::apply(slf, id, &Write::Append { value: &stored })
    }

    /// What a container stores for value, for the mutators written in Python. key is None for a
    /// list item.
    fn adopt<'py>(
        slf: &Bound<'py, Self>,
        id: u32,
        key: &Bound<'py, PyAny>,
        value: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let key = if key.is_none() { NodeKey::ListItem } else { exact_str_key(key) };
        Self::adopt_value(slf, id, key, value, 0)
    }

    /// Value left the tree and is an ordinary container now.
    fn release(slf: &Bound<'_, Self>, value: &Bound<'_, PyAny>) -> PyResult<()> {
        Self::release_value(slf, value, slf.py().None().bind(slf.py()))
    }

    /// A write the container made itself: `set` and `del` take the key or index, `del_range`
    /// start and stop, `extend` the stored items, `insert` index and stored item. `slice_set` and
    /// `replace_all` say the list's items changed, and read them from node.
    #[pyo3(signature = (node, id, op, *arguments))]
    fn forward(
        slf: &Bound<'_, Self>,
        node: &Bound<'_, PyAny>,
        id: u32,
        op: &str,
        arguments: &Bound<'_, PyTuple>,
    ) -> PyResult<()> {
        let argument = |index: usize| arguments.get_item(index);
        match op {
            "set" | "del" => {
                let key_object = argument(0)?;
                let part = if node.is_instance_of::<PyList>() {
                    key_object
                        .extract::<usize>()
                        .map(Part::Index)
                        .map_err(|_| Misfit::new("not an index"))
                } else {
                    part_of(&key_object)
                };
                match part {
                    Ok(key) if op == "set" => {
                        Self::apply(slf, id, &Write::Set { key, value: &argument(1)? })
                    }
                    Ok(key) => Self::apply(slf, id, &Write::Delete { key }),
                    Err(misfit) => slf.get().with_slot_mut(slf.py(), |slot| {
                        slot.misfit(misfit.0);
                        slot.touch();
                    }),
                }
            }
            "del_range" => {
                let (start, stop) = (argument(0)?.extract()?, argument(1)?.extract()?);
                Self::apply(slf, id, &Write::DeleteRange { start, stop })
            }
            "clear" => Self::apply(slf, id, &Write::Clear),
            "extend" => {
                let values = argument(0)?;
                Self::apply(slf, id, &Write::Extend { values: values.downcast::<PyList>()? })
            }
            "insert" => {
                let index = argument(0)?.extract()?;
                Self::apply(slf, id, &Write::Insert { index, value: &argument(1)? })
            }
            "reverse" => Self::apply(slf, id, &Write::Reverse),
            "slice_set" | "replace_all" => {
                let items = node.downcast::<PyList>()?;
                Self::apply(slf, id, &Write::ReplaceAll { items })
            }
            other => Err(pyo3::exceptions::PyValueError::new_err(format!("unknown write {other:?}"))),
        }
    }

    /// Own value as the namespace's new tree and return that tree: at attach, and for a write of
    /// the whole namespace. A tree that does not fit is stored and the namespace is tainted.
    pub(crate) fn replace_root(slf: &Bound<'_, Self>, value: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        Self::drop_root(slf)?;
        let root = Self::set_root(slf, value)?;
        slf.get().with_slot_mut(slf.py(), |slot| slot.counts.whole_writes += 1)?;
        Ok(root)
    }

    /// The namespace left model_state.
    fn clear_root(slf: &Bound<'_, Self>) -> PyResult<()> {
        Self::drop_root(slf)?;
        let this = slf.get();
        let old_row = this.with_slot_mut(slf.py(), |slot| {
            slot.taint = None;
            slot.detached = false;
            slot.row.take()
        })?;
        drop(old_row);
        let given = std::mem::take(&mut this.tree_mut()?.items_given);
        drop(given);
        Ok(())
    }

    /// Drop the tree: the row alone holds the state, and a later Python read calls rebuild. Nodes
    /// still named somewhere get dead_sink, which raises on a write, because a write to one could
    /// reach neither the row nor a new tree. Not possible for a tainted namespace, whose only
    /// state is the tree. Returns whether it detached.
    fn detach(slf: &Bound<'_, Self>, dead_sink: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = slf.py();
        let this = slf.get();
        if !this.with_slot(py, |slot| slot.row.is_some())? {
            return Ok(false);
        }
        let root = this.tree_mut()?.root.take();
        if let Some(root) = root {
            Self::release_value(slf, root.bind(py), dead_sink)?;
        }
        this.with_slot_mut(py, |slot| slot.detached = true)?;
        Ok(true)
    }

    /// Load value into the row and keep no tree: what `replace_root` and then `detach` leave,
    /// without building the tree. For a namespace that stays detached (`state_mirror.
    /// DETACHED_NAMESPACES`). Returns false, changing nothing, for a value that does not fit the
    /// row: the caller then stores it as a tree, which taints the namespace as before.
    fn load_detached(slf: &Bound<'_, Self>, value: &Bound<'_, PyAny>, dead_sink: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = slf.py();
        let this = slf.get();
        let Ok(row) = (this.ops().load)(value) else {
            return Ok(false);
        };
        this.touch(py)?;
        let old = this.tree_mut()?.root.take();
        if let Some(old) = old {
            Self::release_value(slf, old.bind(py), dead_sink)?;
        }
        let old_row = this.with_slot_mut(py, |slot| {
            let old_row = slot.row.replace(row);
            slot.taint = None;
            slot.detached = true;
            slot.counts.whole_writes += 1;
            old_row
        })?;
        drop(old_row);
        Ok(true)
    }

    /// A new tree from the row, after a detach.
    fn rebuild(slf: &Bound<'_, Self>) -> PyResult<Py<PyAny>> {
        let py = slf.py();
        let this = slf.get();
        let ops = this.ops();
        let plain = this.with_slot(py, |slot| match slot.row.as_deref() {
            Some(row) => ops.py(row)?.to_py(py),
            None => Err(PyRuntimeError::new_err("a detached namespace always has a row")),
        })??;
        let root = Self::adopt_value(slf, NO_PARENT, NodeKey::Root, &plain, 0)?;
        let old = this.tree_mut()?.root.replace(root.clone().unbind());
        drop(old);
        this.with_slot_mut(py, |slot| {
            slot.detached = false;
            slot.counts.rebuilds += 1;
            slot.touch();
        })?;
        Ok(root.unbind())
    }

    /// The row as plain dicts and lists, which nothing follows: for a reader of a detached
    /// namespace that must not rebuild its tree (`state_mirror.read_only_state`). None without a row.
    fn document<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        let ops = self.ops();
        self.with_slot(py, |slot| slot.row.as_deref().map(|row| ops.py(row)?.to_py(py)).transpose())?
    }

    /// `document().get(key)` as a 1-tuple, or None for a key the row does not have: what a reader
    /// of one part of a detached namespace reads without making the rest
    /// (`state_mirror.read_only_part`). Unsupported without a row.
    fn document_part<'py>(&self, py: Python<'py>, key: &str) -> PyResult<Option<Bound<'py, PyTuple>>> {
        let ops = self.ops();
        self.with_slot(py, |slot| {
            let Some(row) = slot.row.as_deref() else {
                return Err(Unsupported::new_err("the namespace has no row"));
            };
            ops.py(row)?.part_to_py(py, key)?.map(|value| PyTuple::new(py, [value])).transpose()
        })?
    }

    /// The number of items of the list under key, or None for a key the row does not have or a
    /// value that is not a list: `len(document()[key])` for a reader of a detached namespace.
    /// Unsupported without a row.
    fn part_len(&self, py: Python<'_>, key: &str) -> PyResult<Option<usize>> {
        self.with_slot(py, |slot| {
            let Some(row) = slot.row.as_deref() else {
                return Err(Unsupported::new_err("the namespace has no row"));
            };
            Ok(row.part_items(key).map(|items| items.count()))
        })?
    }

    /// `document()[key][start:stop]` of the list under key, and nothing else made: new plain
    /// values, as `document` gives. start and stop are clamped to the list. Unsupported without a
    /// row or without such a list.
    fn part_slice<'py>(&self, py: Python<'py>, key: &str, start: usize, stop: usize) -> PyResult<Bound<'py, PyList>> {
        let ops = self.ops();
        self.with_slot(py, |slot| {
            let Some(row) = slot.row.as_deref() else {
                return Err(Unsupported::new_err("the namespace has no row"));
            };
            let items = ops.py(row)?.part_py_items(key).ok_or_else(|| Unsupported::new_err("the row has no such list"))?;
            let stop = stop.min(items.count());
            items.slice_to_py(py, start.min(stop), stop)
        })?
    }

    /// The canonical JSON of each item of the list under key, or None for a key the row does not
    /// have or a value that is not a list: for a save, which hashes and archives a history by its
    /// items (`core/checkpoint_cache.py`). The leading items whose bytes are those of a run of the
    /// items the last call gave come back as the same bytes objects, in that order; every other
    /// item is a new object. So, as for a list that only drops items from its front and appends
    /// new ones, the same first and last object of a run mean the same run. Unsupported without a
    /// row.
    fn items_json<'py>(&self, py: Python<'py>, key: &str) -> PyResult<Option<Bound<'py, PyList>>> {
        let core = self.core.bind(py).try_borrow()?;
        let slot = core.slot(self.index)?.try_borrow().or_py()?;
        let Some(row) = slot.row.as_deref() else {
            return Err(Unsupported::new_err("the namespace has no row"));
        };
        let Some(items) = row.part_items(key) else {
            return Ok(None);
        };
        let mut tree = self.tree_mut()?;
        let given = match tree.items_given.iter().position(|(name, _)| &**name == key) {
            Some(index) => tree.items_given.remove(index).1,
            None => Vec::new(),
        };
        let count = items.count();
        let mut scratch = Encoder { out: Vec::new() };
        // Where the first item was in the last call's list: past the items dropped since.
        let mut offset = given.len();
        if count > 0 {
            let first = items.item_json(0, &mut scratch).or_py()?;
            if let Some(found) = given.iter().position(|old| old.as_bytes(py) == first) {
                offset = found;
            }
        }
        let mut out = Vec::with_capacity(count);
        let mut same = true;
        for index in 0..count {
            let bytes = items.item_json(index, &mut scratch).or_py()?;
            let old = given.get(offset + index).filter(|old| same && old.as_bytes(py) == bytes);
            match old {
                Some(old) => out.push(old.clone_ref(py)),
                None => {
                    same = false;
                    out.push(PyBytes::new(py, bytes).unbind());
                }
            }
        }
        let list = PyList::new(py, out.iter().map(|item| item.bind(py)))?;
        tree.items_given.push((Box::from(key), out));
        Ok(Some(list))
    }

    /// `document()` with an empty list under each key of `emptied` that holds a list, and those
    /// lists not made: for a save, which reads them by `items_json`. None without a row.
    fn document_emptied<'py>(&self, py: Python<'py>, emptied: Vec<String>) -> PyResult<Option<Bound<'py, PyAny>>> {
        let keys: Vec<&str> = emptied.iter().map(String::as_str).collect();
        let ops = self.ops();
        self.with_slot(py, |slot| slot.row.as_deref().map(|row| ops.py(row)?.to_py_emptied(py, &keys)).transpose())?
    }

    /// Drop the row, as a write that does not fit does. Not for a detached namespace, whose row
    /// is all there is of it: read it from Python first, which builds its tree.
    fn taint(&self, py: Python<'_>, reason: String) -> PyResult<()> {
        let old_row = self.with_slot_mut(py, |slot| {
            if slot.detached {
                return Err(PyRuntimeError::new_err("a detached namespace has nothing but its row"));
            }
            slot.taint = Some(reason);
            Ok(slot.row.take())
        })??;
        drop(old_row);
        Ok(())
    }

    /// Why the namespace has no row, or None.
    #[getter]
    fn tainted(&self, py: Python<'_>) -> PyResult<Option<String>> {
        self.with_slot(py, |slot| slot.taint.clone())
    }

    #[getter]
    fn has_row(&self, py: Python<'_>) -> PyResult<bool> {
        self.with_slot(py, |slot| slot.row.is_some())
    }

    #[getter]
    fn is_detached(&self, py: Python<'_>) -> PyResult<bool> {
        self.with_slot(py, |slot| slot.detached)
    }

    /// The heap bytes of the row's `V` (`V::heap_bytes`), or None without a `ValueRow`: for the
    /// measurements of lane 5-3.
    #[getter]
    fn value_bytes(&self, py: Python<'_>) -> PyResult<Option<usize>> {
        self.with_slot(py, |slot| slot.row::<ValueRow>().ok().map(|row| row.0.heap_bytes()))
    }

    /// Read the tree into a new row, for a namespace whose misfit is gone. Returns whether it fits.
    fn refit(slf: &Bound<'_, Self>) -> PyResult<bool> {
        let py = slf.py();
        let this = slf.get();
        let Some(root) = this.root(py)? else {
            return Ok(false);
        };
        let row = (this.ops().load)(&root);
        let (fits, old_row) = this.with_slot_mut(py, |slot| match row {
            Ok(row) => {
                slot.taint = None;
                (true, slot.row.replace(row))
            }
            Err(misfit) => {
                slot.taint = Some(misfit.0);
                (false, None)
            }
        })?;
        drop(old_row);
        Ok(fits)
    }

    /// Whether value is the tree of this mirror. A field of the world assigned whole is not.
    fn holds(&self, value: &Bound<'_, PyAny>) -> bool {
        self.root_is(value)
    }

    /// A number that stays the same while nothing writes to the tree, if value is the tree or the
    /// placeholder of the detached namespace, else None. So a check can tell it read this very
    /// state without comparing it. Both this and `holds` miss `dict.__setitem__` on a node, which
    /// passes the tree's overrides (docs/rust-port/lanes/t-tick-validation.md). The number is a
    /// stamp (`sheltersim_core::stamp`): no other state of any namespace ever has it.
    fn generation_of(&self, value: &Bound<'_, PyAny>) -> Option<u64> {
        self.held_generation(value)
    }

    /// The canonical JSON of the row, or None without one.
    fn row_json<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyBytes>>> {
        self.with_slot(py, |slot| {
            let Some(row) = slot.row.as_deref() else {
                return Ok(None);
            };
            let mut encoder = Encoder { out: Vec::with_capacity(256) };
            row.write_json(&mut encoder).or_py()?;
            Ok(Some(PyBytes::new(py, &encoder.out)))
        })?
    }

    /// Whether the row and the tree have the same canonical JSON; true when one of them is
    /// missing, since there is nothing to differ.
    fn verify(slf: &Bound<'_, Self>) -> PyResult<bool> {
        let py = slf.py();
        let this = slf.get();
        let row_bytes = this.with_slot(py, |slot| {
            let Some(row) = slot.row.as_deref() else {
                return Ok(None);
            };
            let mut encoder = Encoder { out: Vec::with_capacity(256) };
            row.write_json(&mut encoder).or_py()?;
            Ok::<_, PyErr>(Some(encoder.out))
        })??;
        let Some(row_bytes) = row_bytes else {
            return Ok(true);
        };
        let Some(root) = this.root(py)? else {
            return Ok(true);
        };
        Ok(encode(&root)? == row_bytes)
    }

    /// Counters for the budget of a lane.
    fn stats<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let counts = self.with_slot(py, |slot| slot.counts)?;
        let live = self.tree.try_borrow().or_py()?.table.live;
        let stats = PyDict::new(py);
        stats.set_item("namespace", &*self.name)?;
        stats.set_item("forwards", counts.forwards)?;
        stats.set_item("whole_writes", counts.whole_writes)?;
        stats.set_item("rebuilds", counts.rebuilds)?;
        stats.set_item("live_nodes", live)?;
        Ok(stats)
    }

    fn __traverse__(&self, visit: PyVisit<'_>) -> Result<(), PyTraverseError> {
        visit.call(&self.core)?;
        visit.call(&self.row_type)?;
        // A tree borrowed now is being changed by Rust; what it holds is visited next time.
        if let Ok(tree) = self.tree.try_borrow() {
            visit.call(&tree.root)?;
            for node in tree.table.nodes.iter().flatten() {
                visit.call(&node.list)?;
            }
        }
        Ok(())
    }

    /// The tree's references go; the core's `__clear__` breaks the cycle with the core.
    fn __clear__(&self) {
        if let Ok(mut tree) = self.tree.try_borrow_mut() {
            let old = std::mem::take(&mut *tree);
            drop(tree);
            drop(old);
        }
    }
}

/// A world's Rust state: `world.__dict__["_core"]`. It holds the core's `World` and the handles
/// that are its Python views: the mirror of each slot and the entity store. Each handle holds the
/// core, so they make cycles, which `__traverse__` and `__clear__` let the collector break. It
/// holds no reference to the world, so `world.__dict__.clear()/update()` moves it with the state
/// it belongs to.
#[pyclass(module = "sheltersim_native")]
pub struct WorldCore {
    world: World,
    /// The mirror of each slot, in the slots' order: `mirrors[i]` is the view of slot i.
    mirrors: IndexMap<Arc<str>, Py<NamespaceMirror>>,
    store: Option<Py<EntityStore>>,
}

impl WorldCore {
    /// The world, for a shim that holds this core shared (R3).
    #[inline]
    pub(crate) fn world(&self) -> &World {
        &self.world
    }

    #[inline]
    fn slot(&self, index: usize) -> PyResult<&Shard<Slot>> {
        self.world.slots.at(index).ok_or_else(|| PyRuntimeError::new_err("a mirror's slot is not in its core"))
    }

    fn add_slot(slf: &Bound<'_, Self>, name: &str, row_type: Py<RowType>, place: Place) -> PyResult<Py<NamespaceMirror>> {
        let py = slf.py();
        let name: Arc<str> = Arc::from(name);
        let index = slf.try_borrow_mut()?.world.slots.add(name.clone(), place).or_py()?;
        let mirror = Py::new(
            py,
            NamespaceMirror { core: slf.clone().unbind(), index, name: name.clone(), row_type, tree: Shard::default() },
        )?;
        let mut this = slf.try_borrow_mut()?;
        this.mirrors.insert(name, mirror.clone_ref(py));
        debug_assert_eq!(this.mirrors.len(), this.world.slots.len());
        Ok(mirror)
    }
}

#[pymethods]
impl WorldCore {
    #[new]
    fn new() -> Self {
        Self { world: World::default(), mirrors: IndexMap::new(), store: None }
    }

    /// The handle of the world's entity rows (state/entities.rs).
    pub(crate) fn entities(slf: &Bound<'_, Self>) -> PyResult<Py<EntityStore>> {
        let py = slf.py();
        if let Some(store) = &slf.try_borrow()?.store {
            return Ok(store.clone_ref(py));
        }
        let store = Py::new(py, EntityStore::new(slf.clone().unbind()))?;
        slf.try_borrow_mut()?.store = Some(store.clone_ref(py));
        Ok(store)
    }

    /// Say the namespace is owned, with a row of this type, and return its mirror. It has no
    /// state until `replace_root`. `field`: it is a field of the world, not of `model_state`.
    #[pyo3(signature = (name, row_type, field = false))]
    fn own(slf: &Bound<'_, Self>, name: &str, row_type: Py<RowType>, field: bool) -> PyResult<Py<NamespaceMirror>> {
        Self::add_slot(slf, name, row_type, if field { Place::Field } else { Place::Namespace })
    }

    /// `own` for a key of `model_state` that no lane owns: its row is a `ValueRow`, which any JSON
    /// fits (`Place::Value`). `state_mirror.NamespaceTable` calls it when such a key is first stored.
    fn own_value(slf: &Bound<'_, Self>, name: &str) -> PyResult<Py<NamespaceMirror>> {
        static VALUE_ROW: GILOnceCell<Py<RowType>> = GILOnceCell::new();
        let py = slf.py();
        let row_type = VALUE_ROW.get_or_try_init(py, || Py::new(py, RowType::of::<ValueRow>("value")))?.clone_ref(py);
        Self::add_slot(slf, name, row_type, Place::Value)
    }

    /// The mirror of an owned namespace, or None.
    fn mirror(&self, py: Python<'_>, name: &str) -> Option<Py<NamespaceMirror>> {
        self.mirrors.get(name).map(|mirror| mirror.clone_ref(py))
    }

    fn owned_names(&self) -> Vec<String> {
        self.world.slots.names().map(|name| name.to_string()).collect()
    }

    /// `world.<name> = value` of a scalar (`SCALAR_FIELDS`), from `WorldState.__setattr__`.
    fn set_scalar(&self, name: &str, value: &Bound<'_, PyAny>) -> PyResult<()> {
        let mut scalars = self.world.scalars.try_borrow_mut().or_py()?;
        crate::state::world_fields::set_scalar(&mut scalars, name, value)
    }

    /// The scalars as the core holds them, a misfit as None (`state/world_fields.rs`).
    fn scalars<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        crate::state::world_fields::scalars_to_py(py, &*self.world.scalars.try_borrow().or_py()?)
    }

    fn __traverse__(&self, visit: PyVisit<'_>) -> Result<(), PyTraverseError> {
        for mirror in self.mirrors.values() {
            visit.call(mirror)?;
        }
        visit.call(&self.store)?;
        Ok(())
    }

    fn __clear__(&mut self) {
        let mirrors = std::mem::take(&mut self.mirrors);
        let store = self.store.take();
        drop((mirrors, store));
    }
}

/// The core of a world, read now: call this again after any Python callback, because a
/// copy-and-swap in the callback moves the world to another core.
pub(crate) fn core_of<'py>(world: &Bound<'py, PyAny>) -> PyResult<Bound<'py, WorldCore>> {
    let core = world
        .getattr(intern!(world.py(), "_core"))
        .map_err(|_| Unsupported::new_err("the world has no core"))?;
    Ok(core.downcast::<WorldCore>()?.clone())
}

/// A world's core as the core's `WorldCell`: `with` looks up `world.__dict__["_core"]` now (rule R1
/// of design-p5-p6.md: a callback may have moved the world to another core) and holds the
/// `WorldCore` exclusively while f runs (R2). A world without a core is Unsupported; a core
/// borrowed already (a shim of this crate is reading a part of it) is `RuntimeError`, the text of a
/// part borrowed twice.
pub(crate) struct PyWorldCell<'a, 'py>(pub(crate) &'a Bound<'py, PyAny>);

impl WorldCell for PyWorldCell<'_, '_> {
    fn with<R>(&self, f: impl FnOnce(&mut World) -> R) -> sheltersim_core::error::Result<R> {
        let core = core_of(self.0).map_err(|_| CoreError::unsupported("the world has no core"))?;
        let (result, flush) = {
            let mut held = core.try_borrow_mut().map_err(|_| CoreError::Runtime(BORROWED.to_owned()))?;
            let result = f(&mut held.world);
            let flush = crate::state::entities::plan_flush(held.world.entities.get_mut());
            let store = held.store.as_ref().map(|store| store.clone_ref(core.py()));
            (result, flush.zip(store))
        };
        // The objects follow the rows f wrote (rule R5), with the world no longer borrowed.
        if let Some((flush, store)) = flush {
            EntityStore::flush(store.bind(core.py()), flush).map_err(|error| CoreError::Runtime(error.to_string()))?;
        }
        Ok(result)
    }
}

/// Run f on the whole world, borrowed exclusively (rule R2): f must not call Python, and neither may
/// the core functions it calls. A kernel gathers what Python holds before, and makes the Python
/// values of what f returns after (docs/rust-port/step5-plan.md, PR 5-4).
#[inline]
pub(crate) fn with_world<R>(world: &Bound<'_, PyAny>, f: impl FnOnce(&mut World) -> R) -> PyResult<R> {
    PyWorldCell(world).with(f).or_py()
}

/// Whether `world.<name>` is the tree of the field's slot now, which a field assigned whole is not
/// (`field_mirror_of`). True for a name that is not owned or a world without a core: the core
/// function says what is wrong then, at the point the kernel said it before.
pub(crate) fn field_held(world: &Bound<'_, PyAny>, name: &Bound<'_, PyString>) -> PyResult<bool> {
    let Ok(mirror) = mirror_of(world, name.to_str()?) else {
        return Ok(true);
    };
    Ok(mirror.get().root_is(&world.getattr(name)?))
}

/// The stamp of the owned world field `name` while it holds `held`, the mirror's tree, and the core
/// that holds the field: what `configs.rs` keeps the kernels' configs by. None: the world has no
/// core, the field is not owned, or it holds another value.
pub(crate) fn field_stamp<'py>(
    world: &Bound<'py, PyAny>,
    name: &str,
    held: &Bound<'py, PyAny>,
) -> PyResult<Option<(Bound<'py, WorldCore>, Stamp)>> {
    let Ok(core) = core_of(world) else {
        return Ok(None);
    };
    let mirror = core.try_borrow()?.mirrors.get(name).map(|mirror| mirror.clone_ref(world.py()));
    Ok(mirror.and_then(|mirror| mirror.get().held_stamp(held)).map(|stamp| (core, stamp)))
}

/// The mirror of a namespace of a world, read now, or Unsupported if it is not owned.
pub(crate) fn mirror_of<'py>(world: &Bound<'py, PyAny>, name: &str) -> PyResult<Bound<'py, NamespaceMirror>> {
    let core = core_of(world)?;
    let found = core.try_borrow()?.mirrors.get(name).map(|mirror| mirror.clone_ref(world.py()));
    found
        .map(|mirror| mirror.into_bound(world.py()))
        .ok_or_else(|| Unsupported::new_err("the namespace is not owned"))
}

/// The mirror of an owned world field such as `metric_state`, or Unsupported if it is not owned or
/// the field holds something else now. `WorldState.__setattr__` catches a whole assignment to
/// `model_state` only; a field assigned whole simply stops being Rust's, and the Python rules run.
pub(crate) fn field_mirror_of<'py>(
    world: &Bound<'py, PyAny>,
    name: &Bound<'py, PyString>,
) -> PyResult<Bound<'py, NamespaceMirror>> {
    let mirror = mirror_of(world, name.to_str()?)?;
    let current = world.getattr(name)?;
    if !mirror.get().root_is(&current) {
        return Err(Unsupported::new_err("the field holds another dict"));
    }
    Ok(mirror)
}
