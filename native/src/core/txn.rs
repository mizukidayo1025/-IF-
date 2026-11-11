//! A transaction on a Python world (lane 6-2, policy E5): the core's `Txn` with the native `Host`
//! (`crate::host::PyHost`, lane P8-0b).
//!
//! The host's work is in `core/state_mirror.py`: validating the world as the copy through canonical
//! JSON was validated (`_txn_validate`, policy E6), making the Python containers again in canonical
//! order (`_txn_canonicalized`), giving them back on a rollback (`_txn_restored`) and dropping what
//! was kept on a commit (`_txn_committed`). The world's queue and log are parts of the core's
//! `World` (lane 6-5), which the snapshot gives back; here their stores' Python objects follow:
//! the objects of the pending events move to the serials of the canonical queue and come back with
//! the old ones (`QueueStore::txn_remap`, `txn_rollback`), and the records a rollback took out of
//! the log take their objects with them (`EventStore::cut_to_log`). Python opens one with
//! `state_mirror.transaction(world, kind)`; the world's `WorldCore` holds it while it is open, so
//! transactions don't nest (rule R8). No borrow of the core is held while the host runs (rule R4).

use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use sheltersim_core::txn::{Txn, TxnKind, NESTED};

use super::world_core::{core_of, PyWorldCell};
use crate::host::PyHost;

/// An open transaction, held by the world's `WorldCore`: the core's `Txn` and what the host kept
/// of the Python containers (`_txn_canonicalized`).
pub(crate) struct OpenTxn {
    txn: Txn,
    pub(crate) kept: Py<PyDict>,
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
    let mut host = PyHost::of_world(world, PyDict::new(py));
    let txn = Txn::begin(&PyWorldCell(world), &mut host, kind).map_err(|error| error.0)?;
    core_of(world)?.try_borrow_mut()?.txn = Some(OpenTxn { txn, kept: host.kept.unbind() });
    Ok(())
}

/// Close the world's open transaction: commit it or roll it back.
pub(crate) fn end(world: &Bound<'_, PyAny>, commit: bool) -> PyResult<()> {
    let py = world.py();
    let open = core_of(world)?.try_borrow_mut()?.txn.take();
    let open = open.ok_or_else(|| PyRuntimeError::new_err("no transaction is open on the world"))?;
    let mut host = PyHost::of_world(world, open.kept.into_bound(py));
    let cell = PyWorldCell(world);
    if commit {
        open.txn.commit(&cell, &mut host)
    } else {
        open.txn.rollback(&cell, &mut host)
    }
    .map_err(|error| error.0)
}
