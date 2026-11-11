//! The configs as the kernels read them (design-p7.md §3, Issue #152, lane 7-1; policies E7 and
//! E14 of docs/rust-port/step5-plan.md).
//!
//! The core's `Configs` (`sheltersim_core::config`) is kept in the world's `World.derived` by the
//! stamp of the `model_configs` slot. This file fills its sections with today's Python contracts,
//! called on the same tree, at the first read of a section after the stamp changed. A parse never
//! raises here: what it raised is kept as a `Fault`, and the read site turns it into its own error
//! at the line it raises it today. A refusal keeps its reason there; an error the rule relays as it
//! is comes from a second parse (`reraise`), so its class and text are Python's own.
//!
//! The stamp changes with every write to the configs, so an edit of `model_configs` in place is
//! seen at the next read. Inside a tick phase the Python scopes (`selected_models_scope`,
//! `geometry_config_scope`) saw it only at the next minute. Nothing in `src/` makes such an edit
//! (decision U1 of design-p7.md); a debug run counts any (`watch_config_writes`), and the all-path
//! gate reports a count above zero as a problem.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use pyo3::exceptions::{PyKeyError, PyTypeError, PyValueError};
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::sync::GILOnceCell;
use pyo3::types::{PyDict, PyString};
use sheltersim_core::config::{Configs, Fault, FaultClass, Section, Selection};

use super::errors::{corrupt, OrPy};
use super::row::attribute;
use super::world_core::field_stamp;

const MODEL_CONFIGS: &str = "model_configs";

/// The configs of `world.model_configs` now: those kept for its stamp, or new ones with nothing
/// parsed. Without a stamp (no core, or a field assigned whole), new ones that serve one read.
pub(crate) fn current(world: &Bound<'_, PyAny>) -> PyResult<Arc<Configs>> {
    let raw = attribute(world, intern!(world.py(), "model_configs"))?;
    current_of(world, &raw)
}

fn current_of(world: &Bound<'_, PyAny>, raw: &Bound<'_, PyAny>) -> PyResult<Arc<Configs>> {
    match field_stamp(world, MODEL_CONFIGS, raw)? {
        Some((core, stamp)) => core.try_borrow()?.world().configs(stamp).or_py(),
        None => Ok(Arc::new(Configs::new(None))),
    }
}

/// What a parse gave, as a section: a value, or what it raised.
pub(crate) fn section_of<T: ?Sized>(py: Python<'_>, parsed: PyResult<Arc<T>>) -> Section<T> {
    match parsed {
        Ok(value) => Section::Ok(value),
        Err(error) => Section::Fault(fault_of(py, &error)),
    }
}

/// The class and text of a Python error.
pub(crate) fn fault_of(py: Python<'_>, error: &PyErr) -> Fault {
    let kind = error.get_type(py);
    let class = if kind.is(&py.get_type::<PyValueError>()) {
        FaultClass::Value
    } else if kind.is(&py.get_type::<PyTypeError>()) {
        FaultClass::Type
    } else if kind.is(&py.get_type::<PyKeyError>()) {
        FaultClass::Key
    } else {
        let module = kind.module().map(|module| module.to_string()).unwrap_or_default();
        let name = kind.name().map(|name| name.to_string()).unwrap_or_default();
        if name == "ValidationError" && module.starts_with("pydantic") {
            FaultClass::Validation
        } else {
            FaultClass::Other(format!("{module}.{name}").into())
        }
    };
    Fault::new(class, error.value(py).to_string())
}

/// The error of a fault, for a read site that relays it as it is: what a second parse raises,
/// which is the same class and text, since a parse is a pure function of the configs.
pub(crate) fn reraise<T>(parse: impl FnOnce() -> PyResult<T>) -> PyErr {
    match parse() {
        Err(error) => error,
        Ok(_) => corrupt(MODEL_CONFIGS, "a config fault did not reproduce"),
    }
}

// ---- The model selection (design-p7.md §1.1, rows 4–4e) --------------------------------------------

struct Policies {
    module: Py<PyModule>,
    /// `_SELECTION_SCOPE`: set while a tick phase runs (`OfflineSimulationController.__call__`).
    scope: Py<PyAny>,
}

static POLICIES: GILOnceCell<Policies> = GILOnceCell::new();
/// `selected_models` of the module, as first seen. A read that finds another function there (a
/// test's patch) calls that one each time and keeps nothing.
static SELECTED_MODELS: GILOnceCell<Py<PyAny>> = GILOnceCell::new();

fn policies(py: Python<'_>) -> PyResult<&Policies> {
    POLICIES.get_or_try_init(py, || {
        let module = py.import("sheltersim.contracts.policies")?;
        Ok(Policies { scope: module.getattr("_SELECTION_SCOPE")?.unbind(), module: module.unbind() })
    })
}

/// Whether `function` is the module's own `selected_models`, whose selection may be kept. The
/// first one seen is taken by its names, so a patch in place at the first read is not.
fn is_the_contract(function: &Bound<'_, PyAny>) -> PyResult<bool> {
    let py = function.py();
    if let Some(known) = SELECTED_MODELS.get(py) {
        return Ok(function.is(known));
    }
    let named = |name: &Bound<'_, PyString>, wanted: &str| -> PyResult<bool> {
        Ok(function.getattr(name).ok().and_then(|value| value.extract::<String>().ok()).as_deref() == Some(wanted))
    };
    let own = named(intern!(py, "__module__"), "sheltersim.contracts.policies")?
        && named(intern!(py, "__qualname__"), "selected_models")?;
    if own {
        let _ = SELECTED_MODELS.set(py, function.clone().unbind());
    }
    Ok(own)
}

/// `selected_models(world.model_configs)`, kept for the configs' stamp: the selection, or the fault
/// for the read site to replay. A fault whose class is `Refused` is a selection that is not a dict
/// of str to str; any other is what `selected_models` raised (`reraise_selection`).
pub(crate) fn selection(world: &Bound<'_, PyAny>) -> PyResult<Result<Arc<Selection>, Fault>> {
    let py = world.py();
    let raw = attribute(world, intern!(py, "model_configs"))?;
    let policies = policies(py)?;
    let function = policies.module.bind(py).getattr(intern!(py, "selected_models"))?;
    let configs = if is_the_contract(&function)? {
        current_of(world, &raw)?
    } else {
        Arc::new(Configs::new(None))
    };
    let section = configs.selection.get_or_parse(|| match function.call1((&raw,)) {
        Ok(selection) => match read_selection(&selection) {
            Some(selection) => Section::Ok(Arc::new(selection)),
            None => Section::Fault(Fault::refused("not a selection")),
        },
        Err(error) => Section::Fault(fault_of(py, &error)),
    });
    Ok(match section {
        Section::Ok(selection) => Ok(selection.clone()),
        Section::Fault(fault) => Err(fault.clone()),
        Section::Absent => Err(Fault::refused("not a selection")),
    })
}

/// What `selected_models(world.model_configs)` raises now, for a read site that relays it.
pub(crate) fn reraise_selection(world: &Bound<'_, PyAny>) -> PyErr {
    let py = world.py();
    reraise(|| {
        let raw = attribute(world, intern!(py, "model_configs"))?;
        policies(py)?.module.bind(py).getattr(intern!(py, "selected_models"))?.call1((raw,))
    })
}

/// A dict of exact str to exact str, in its order, or None.
fn read_selection(selection: &Bound<'_, PyAny>) -> Option<Selection> {
    let selection = selection.downcast_exact::<PyDict>().ok()?;
    let mut models = Vec::with_capacity(selection.len());
    for (domain, model) in selection.iter() {
        let text = |value: &Bound<'_, PyAny>| -> Option<Arc<str>> {
            Some(Arc::from(value.downcast_exact::<PyString>().ok()?.to_str().ok()?))
        };
        models.push((text(&domain)?, text(&model)?));
    }
    Some(Selection::new(models))
}

// ---- The debug count of configs written inside a tick (decision U1) ---------------------------------

static WATCHING: AtomicBool = AtomicBool::new(false);
static WRITES_IN_TICK: AtomicU64 = AtomicU64::new(0);

/// Count, from now on, the writes to a `model_configs` tree in place while a tick phase runs, and
/// start the count again from zero (off: stop counting).
#[pyfunction]
pub(crate) fn watch_config_writes(on: bool) {
    WRITES_IN_TICK.store(0, Ordering::Relaxed);
    WATCHING.store(on, Ordering::Relaxed);
}

/// The writes counted since `watch_config_writes(True)`.
#[pyfunction]
pub(crate) fn config_writes_in_tick() -> u64 {
    WRITES_IN_TICK.load(Ordering::Relaxed)
}

/// A write from Python reached the tree of `name` (`NamespaceMirror::apply`). It holds no borrow.
#[inline]
pub(crate) fn note_write(py: Python<'_>, name: &str) -> PyResult<()> {
    if !WATCHING.load(Ordering::Relaxed) || name != MODEL_CONFIGS {
        return Ok(());
    }
    note_write_in_tick(py)
}

#[cold]
fn note_write_in_tick(py: Python<'_>) -> PyResult<()> {
    if !policies(py)?.scope.bind(py).call_method0(intern!(py, "get"))?.is_none() {
        WRITES_IN_TICK.fetch_add(1, Ordering::Relaxed);
    }
    Ok(())
}
