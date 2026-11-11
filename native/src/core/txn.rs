//! A transaction on a Python world (lane 6-2, policy E5): the core's `Txn` with the native `Host`.
//!
//! The host's work is in `core/state_mirror.py`: validating the world as the copy through canonical
//! JSON was validated (`_txn_validate`, policy E6), making the Python containers again in canonical
//! order (`_txn_canonicalized`), giving them back on a rollback (`_txn_restored`) and dropping what
//! was kept on a commit (`_txn_committed`). Python opens one with `state_mirror.transaction(world,
//! kind)`; the world's `WorldCore` holds it while it is open, so transactions don't nest (rule R8).
//! No borrow of the core is held while the host runs (rule R4).

use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use sheltersim_core::error::Error as CoreError;
use sheltersim_core::txn::{Host, Stage, Txn, TxnKind, NESTED};

use super::errors::to_py_err;
use super::world_core::{core_of, PyWorldCell};

/// An open transaction, held by the world's `WorldCore`: the core's `Txn` and what the host kept
/// of the Python containers (`_txn_canonicalized`).
pub(crate) struct OpenTxn {
    txn: Txn,
    pub(crate) kept: Py<PyDict>,
}

/// A Python error, as the host's error.
pub(crate) struct HostError(pub(crate) PyErr);

impl From<CoreError> for HostError {
    fn from(error: CoreError) -> Self {
        HostError(to_py_err(error))
    }
}

/// The native host: the Python world and what its transaction keeps.
pub(crate) struct PyHost<'a, 'py> {
    world: &'a Bound<'py, PyAny>,
    kept: Bound<'py, PyDict>,
}

impl PyHost<'_, '_> {
    fn call(&self, name: &str, argument: Option<bool>) -> Result<(), HostError> {
        let py = self.world.py();
        let module = py.import(intern!(py, "sheltersim.core.state_mirror")).map_err(HostError)?;
        let function = module.getattr(name).map_err(HostError)?;
        match argument {
            Some(flag) => function.call1((self.world, flag)),
            None => function.call1((self.world, &self.kept)),
        }
        .map(drop)
        .map_err(HostError)
    }
}

impl Host for PyHost<'_, '_> {
    type Error = HostError;

    fn validate_candidate(&mut self, _kind: TxnKind, stage: Stage) -> Result<(), HostError> {
        self.call("_txn_validate", Some(stage == Stage::Candidate))
    }

    fn canonicalized(&mut self, _kind: TxnKind) -> Result<(), HostError> {
        self.call("_txn_canonicalized", None)
    }

    fn restored(&mut self, _kind: TxnKind) -> Result<(), HostError> {
        self.call("_txn_restored", None)
    }

    fn committed(&mut self, _kind: TxnKind) -> Result<(), HostError> {
        self.call("_txn_committed", None)
    }
}

/// The kind of a transaction by its Python name.
pub(crate) fn kind_of(name: &str) -> PyResult<TxnKind> {
    match name {
        "full" => Ok(TxnKind::Full),
        "effect" => Ok(TxnKind::Effect),
        _ => Err(PyValueError::new_err(format!("unknown kind of transaction {name:?}"))),
    }
}

/// Open a transaction on the world: `Txn::begin` with the native host. Unsupported, with nothing
/// changed, for a world whose parts are not all held by its core (`_txn_check`); `RuntimeError`
/// while another transaction is open; the validation's error, with the world as it was, when the
/// world or its canonical candidate does not pass.
pub(crate) fn begin(world: &Bound<'_, PyAny>, kind: TxnKind) -> PyResult<()> {
    let py = world.py();
    if core_of(world)?.try_borrow()?.txn.is_some() {
        return Err(PyRuntimeError::new_err(NESTED));
    }
    py.import(intern!(py, "sheltersim.core.state_mirror"))?.getattr(intern!(py, "_txn_check"))?.call1((world,))?;
    let mut host = PyHost { world, kept: PyDict::new(py) };
    let txn = Txn::begin(&PyWorldCell(world), &mut host, kind).map_err(|error| error.0)?;
    core_of(world)?.try_borrow_mut()?.txn = Some(OpenTxn { txn, kept: host.kept.unbind() });
    Ok(())
}

/// Close the world's open transaction: commit it or roll it back.
pub(crate) fn end(world: &Bound<'_, PyAny>, commit: bool) -> PyResult<()> {
    let py = world.py();
    let open = core_of(world)?.try_borrow_mut()?.txn.take();
    let open = open.ok_or_else(|| PyRuntimeError::new_err("no transaction is open on the world"))?;
    let mut host = PyHost { world, kept: open.kept.into_bound(py) };
    let cell = PyWorldCell(world);
    if commit {
        open.txn.commit(&cell, &mut host)
    } else {
        open.txn.rollback(&cell, &mut host)
    }
    .map_err(|error| error.0)
}
