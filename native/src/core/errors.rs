//! The one exit for the texts of an event's expected outcomes: `EventSuppressed` and
//! `EventApplicationError` of `events/models.py` (docs/rust-port/lanes/l8-texts-events.md).
//!
//! These texts are saved data: the dispatcher writes `str(exc)` to the event's record as its
//! `detail`, which is hashed and shown. So a kernel that raises one must say what the Python rule
//! says, byte for byte, and in the same order of checks. The classes stay Python's; a kernel raises
//! them through here.
//!
//! The errors and their texts are `sheltersim_core::error::Error`; `to_py_err` makes one into its
//! Python class, and `OrPy` does it for a core call's result: `core_call().or_py()?`.


use pyo3::exceptions::{PyKeyError, PyRuntimeError, PyTypeError, PyValueError};
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::sync::GILOnceCell;
use pyo3::types::{PyBool, PyInt, PyString, PyType};

pub(crate) use sheltersim_core::error::Outcome;
use sheltersim_core::error::{refused_text, Error};

/// The Python exception of a core error: the same class and text as the exits below give. The
/// census counted it when the core built it, so nothing is counted here.
pub(crate) fn to_py_err(error: Error) -> PyErr {
    match error {
        Error::Unsupported(reason) => crate::Unsupported::counted(reason.into_owned()),
        Error::Refused { label, reason } => {
            let text = refused_text(&label, &reason);
            Python::with_gil(|py| {
                let refusal = PyErr::new::<KernelRefusal, _>(text);
                refusal.set_cause(py, Some(crate::Unsupported::counted(reason.into_owned())));
                refusal
            })
        }
        Error::Outcome(outcome, text) => Python::with_gil(|py| marked(py, outcome, &text)),
        Error::MarkedValue(text) => Python::with_gil(|py| marked_value(py, &text)),
        Error::Value(text) => PyValueError::new_err(text),
        Error::Key(text) => PyKeyError::new_err(text),
        Error::Runtime(text) => PyRuntimeError::new_err(text),
        Error::Type(text) => PyTypeError::new_err(text),
        Error::Invariant(text) => PyErr::new::<StateInvariantError, _>(text),
        Error::Ledger(text) => PyErr::new::<ResourceLedgerError, _>(text),
        corrupt @ Error::Corrupt { .. } => PyErr::new::<StateInvariantError, _>(corrupt.to_string()),
        Error::Math(error) => crate::core::mathx::math_error(error),
    }
}

/// A core call's result as Python's: `core_call().or_py()?`.
pub(crate) trait OrPy<T> {
    fn or_py(self) -> PyResult<T>;
}

impl<T> OrPy<T> for sheltersim_core::error::Result<T> {
    #[inline]
    fn or_py(self) -> PyResult<T> {
        self.map_err(to_py_err)
    }
}

/// The attribute an outcome made here carries; its text, `str(exc)`, is the rule's alone.
const MARK: &str = "_sheltersim_native_outcome";
static CLASSES: GILOnceCell<(Py<PyType>, Py<PyType>)> = GILOnceCell::new();

fn class(py: Python<'_>, outcome: Outcome) -> PyResult<&Bound<'_, PyType>> {
    let classes = CLASSES.get_or_try_init(py, || -> PyResult<_> {
        let models = py.import(intern!(py, "sheltersim.events.models"))?;
        Ok((
            models.getattr(intern!(py, "EventSuppressed"))?.downcast_into::<PyType>()?.unbind(),
            models.getattr(intern!(py, "EventApplicationError"))?.downcast_into::<PyType>()?.unbind(),
        ))
    })?;
    Ok(match outcome {
        Outcome::Suppressed => classes.0.bind(py),
        Outcome::Failed => classes.1.bind(py),
    })
}

/// Whether an exception is an `EventSuppressed` or an `EventApplicationError`.
pub(crate) fn is_outcome(py: Python<'_>, error: &PyErr) -> bool {
    [Outcome::Suppressed, Outcome::Failed].into_iter().any(|outcome| class(py, outcome).is_ok_and(|class| error.is_instance(py, class)))
}

/// The exception of an outcome, marked as made here.
fn marked(py: Python<'_>, outcome: Outcome, text: &str) -> PyErr {
    let made = (|| -> PyResult<PyErr> {
        let instance = class(py, outcome)?.call1((text,))?;
        instance.setattr(intern!(py, MARK), true)?;
        Ok(PyErr::from_value(instance))
    })();
    made.unwrap_or_else(|error| error)
}

fn marked_value(py: Python<'_>, text: &str) -> PyErr {
    let made = (|| -> PyResult<PyErr> {
        let instance = py.get_type::<pyo3::exceptions::PyValueError>().call1((text,))?;
        instance.setattr(intern!(py, MARK), true)?;
        Ok(PyErr::from_value(instance))
    })();
    made.unwrap_or_else(|error| error)
}

/// The exception of an outcome.
#[track_caller]
#[inline]
pub(crate) fn error(py: Python<'_>, outcome: Outcome, text: impl Into<String>) -> PyErr {
    marked(py, outcome, &text.into())
}

/// `raise ValueError(text)` in the Python rule's words, marked as made here so that a step which turns
/// a Python call's exception into Unsupported lets it through (a check of `effect_program_v1`).
pub(crate) fn marked_value_error(py: Python<'_>, text: &str) -> PyErr {
    marked_value(py, text)
}

/// Whether this error is an outcome made here, rather than one a Python call raised: a step that
/// turns any exception of a call into Unsupported lets these through.
pub(crate) fn made_here(py: Python<'_>, error: &PyErr) -> bool {
    error.value(py).hasattr(intern!(py, MARK)).unwrap_or(false)
}

/// `raise EventSuppressed(text)`.
#[track_caller]
#[inline]
pub(crate) fn suppressed<T>(py: Python<'_>, text: impl Into<String>) -> PyResult<T> {
    Err(error(py, Outcome::Suppressed, text))
}

/// `raise EventApplicationError(text)`.
#[track_caller]
#[inline]
pub(crate) fn failed<T>(py: Python<'_>, text: impl Into<String>) -> PyResult<T> {
    Err(error(py, Outcome::Failed, text))
}

/// `repr(value)` of a Python value, for the `{x!r}` of a Python rule's f-string.
pub(crate) fn repr(value: &Bound<'_, PyAny>) -> PyResult<String> {
    Ok(value.repr()?.to_str()?.to_owned())
}

/// `repr(text)` of a str, as Python writes it.
pub(crate) fn repr_str(py: Python<'_>, text: &str) -> PyResult<String> {
    repr(PyString::new(py, text).as_any())
}

// ---- The type tests of a rule's checks ---------------------------------------------------------------
//
// A check that raises a text must say yes and no exactly where the Python rule does. Where Rust can't
// tell (a subclass, an int past 64 bits) it says Unsupported, and the Python rule runs and decides.

/// `isinstance(value, int) and not isinstance(value, bool)`: Some(value), None when it isn't one.
pub(crate) fn int_not_bool(value: &Bound<'_, PyAny>) -> PyResult<Option<i64>> {
    if value.is_instance_of::<PyBool>() || !value.is_instance_of::<PyInt>() {
        return Ok(None);
    }
    crate::core::row::exact_int(value).map(Some)
}

/// An int of any size, as a rule compares it with an i64: within i64, or past either end.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Wide {
    Int(i64),
    Above,
    Below,
}

/// `int_not_bool` that also answers for an exact int past 64 bits, which a Scenario's payload may
/// hold (a JSON int has no bound): Python compares it with the rule's i64 values, and so does this.
pub(crate) fn wide_int_not_bool(value: &Bound<'_, PyAny>) -> PyResult<Option<Wide>> {
    if value.is_instance_of::<PyBool>() || !value.is_instance_of::<PyInt>() {
        return Ok(None);
    }
    if let Ok(number) = value.downcast_exact::<PyInt>() {
        if let Ok(number) = number.extract::<i64>() {
            return Ok(Some(Wide::Int(number)));
        }
        return Ok(Some(if number.gt(0)? { Wide::Above } else { Wide::Below }));
    }
    crate::core::row::exact_int(value).map(|number| Some(Wide::Int(number)))
}

/// `type(value) is int`: Some(value), None when it isn't.
pub(crate) fn exactly_int(value: &Bound<'_, PyAny>) -> PyResult<Option<i64>> {
    if !value.is_exact_instance_of::<PyInt>() {
        return Ok(None);
    }
    crate::core::row::exact_int(value).map(Some)
}

/// `isinstance(value, str)`: Some(text), None when it isn't one. A subclass is refused.
pub(crate) fn str_of<'a>(value: &'a Bound<'_, PyAny>) -> PyResult<Option<&'a str>> {
    if !value.is_instance_of::<PyString>() {
        return Ok(None);
    }
    crate::core::row::exact_str(value).map(Some)
}

/// `Enum(value)` of a str enum whose values are `members`: Some(the value), None where the Python rule
/// catches the ValueError or TypeError. A str subclass (a member of the enum itself) is refused.
pub(crate) fn member_of<'a>(value: &'a Bound<'_, PyAny>, members: &[&str]) -> PyResult<Option<&'a str>> {
    if !value.is_instance_of::<PyString>() {
        return Ok(None);
    }
    let text = crate::core::row::exact_str(value)?;
    Ok(members.contains(&text).then_some(text))
}

// ---- social (lane L8c) -------------------------------------------------------------------------------

/// The exception of an outcome a kernel already noted where it gave the text (`noted`), raised without
/// noting it again: the record's last text, which a comparison counts, stays at that site.
pub(crate) fn raise_noted<T>(py: Python<'_>, outcome: Outcome, text: &str) -> PyResult<T> {
    Err(marked(py, outcome, text))
}

// ---- The other errors of the Python rules (lane L8b, docs/rust-port/lanes/l8-texts-rest.md) -------
//
// Where a kernel used to say `Unsupported` so that the Python rule ran and raised `ValueError`,
// `StateInvariantError` or another class of the rules, it raises the rule's error itself through one
// of these: the same class and the same text, before it has changed anything, as a refusal must be.
//
// `corrupt` is the one exit for a state the checks of the Python rules never let through (the C
// group of docs/rust-port/step4-plan.md): its text names the namespace and the check and is not
// Python's.
//
// `kernel_refusal` is the one exit of a refusal that no Python rule stands behind (lane L10): a
// `KernelRefusal` of `core/kernels.py` that names the kernel and gives its reason.

#[cold]
#[inline(never)]
#[track_caller]
fn moved(text: &str) {
    crate::core::census::moved(text);
}

/// A Python class, imported once.
macro_rules! python_class {
    ($name:ident, $module:literal, $class:literal) => {
        pub(crate) struct $name;

        impl $name {
            #[track_caller]
            pub(crate) fn new_err(text: impl Into<String>) -> PyErr {
                let text = text.into();
                moved(&text);
                PyErr::new::<$name, _>(text)
            }
        }

        // SAFETY: the type object is the class of that name in that module, or ValueError, its
        // base, if the module can't be imported (it is part of the package, so it can).
        unsafe impl pyo3::PyTypeInfo for $name {
            const NAME: &'static str = $class;
            const MODULE: Option<&'static str> = Some($module);

            fn type_object_raw(py: Python<'_>) -> *mut pyo3::ffi::PyTypeObject {
                static CLASS: GILOnceCell<Py<PyType>> = GILOnceCell::new();
                let class = CLASS.get_or_try_init(py, || -> PyResult<Py<PyType>> {
                    Ok(py.import($module)?.getattr($class)?.downcast_into::<PyType>()?.unbind())
                });
                match class {
                    Ok(class) => class.as_ptr().cast(),
                    Err(_) => PyValueError::type_object_raw(py),
                }
            }
        }
    };
}

// Used by `invariant` and `corrupt`, which wait for the step-4 entry (docs/rust-port/lanes/l8-texts-rest.md).
#[allow(dead_code)]
mod invariant_class {
    use super::*;
    python_class!(StateInvariantError, "sheltersim.core.world", "StateInvariantError");
}
#[allow(unused_imports)]
pub(crate) use invariant_class::StateInvariantError;

// `new_err` is left for the lanes; `ledger_error` maps the core error.
#[allow(dead_code)]
mod ledger_class {
    use super::*;
    python_class!(ResourceLedgerError, "sheltersim.domain.resources", "ResourceLedgerError");
}
pub(crate) use ledger_class::ResourceLedgerError;

/// `raise ResourceLedgerError(text)` of `domain/resources.py`'s operation: a ValueError whose text
/// the ledger's handler and the water and meal service save inside their own.
#[track_caller]
pub(crate) fn ledger_error(text: impl Into<String>) -> PyErr {
    to_py_err(Error::ledger(text))
}

/// `raise ValueError(text)` of a Python rule.
#[track_caller]
pub(crate) fn value_error(text: impl Into<String>) -> PyErr {
    to_py_err(Error::value_error(text))
}

/// `raise StateInvariantError(text)` of a Python rule.
#[track_caller]
#[allow(dead_code)]
pub(crate) fn invariant(text: impl Into<String>) -> PyErr {
    to_py_err(Error::invariant(text))
}

/// The error of a Python call that the rule makes too (a contract's `model_validate`), raised as
/// it is: the same call on the same value, so the same class and text. Noted as made here.
#[track_caller]
pub(crate) fn relayed(py: Python<'_>, error: PyErr) -> PyErr {
    moved(&error.value(py).to_string());
    error
}

/// The one exit for a state the Python rules' checks never let through: a `StateInvariantError`
/// whose text names the namespace and the check. Not the Python rule's text, so it is not compared.
#[track_caller]
#[allow(dead_code)]
pub(crate) fn corrupt(namespace: &str, check: &str) -> PyErr {
    to_py_err(Error::corrupt(namespace, check))
}


#[allow(dead_code)]
mod refusal_class {
    use super::*;
    python_class!(KernelRefusal, "sheltersim.core.kernels", "KernelRefusal");
}
pub(crate) use refusal_class::KernelRefusal;

/// The one exit of a kernel's refusal, where no Python rule stands behind it: `Unsupported` becomes
/// `KernelRefusal(f"{label} refused: {reason}")` with the refusal as its cause, as `kernel_only` of
/// `core/kernels.py` raises it. `label` is `module.name` of the kernel. Any other error goes on as it is.
pub(crate) fn kernel_refusal(py: Python<'_>, label: &str, error: PyErr) -> PyErr {
    if !error.is_instance_of::<crate::Unsupported>(py) {
        return error;
    }
    let refusal = PyErr::new::<KernelRefusal, _>(refused_text(label, &error.value(py).to_string()));
    refusal.set_cause(py, Some(error));
    refusal
}
