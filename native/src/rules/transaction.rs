//! The transaction of a rule that changes the world and keeps the change only if the changed world
//! passes its checks (lane 6-3 of Issue #152, design-p5-p6.md §6).
//!
//! Such a rule used to copy the whole world through canonical JSON, change the copy, validate it
//! and swap it in (`world.__dict__.clear(); world.__dict__.update(copy.__dict__)`). It now opens a
//! transaction of the world's core (`core/txn.rs`): the world itself is put in the order the copy
//! had and validated as the copy was, the rule writes it, validates it and commits; a rule that
//! stops (an outcome, an error) rolls the world back to what it was. `RuleTxn::world` is what the
//! rule writes.
//!
//! A world whose parts the core does not all hold is refused by the transaction with nothing changed
//! (`_txn_check` of `core/state_mirror.py`: a tainted entity store, a map of entities that is not
//! the rows', a key of model_state that is not a str, an event queue or EventLog held in Python, no
//! core). For it the rule takes today's path: `RuleTxn::world` is the copy (`copy_of`,
//! `effect_copy_of`), a commit swaps it in and a rollback drops it.
//!
//! A rule called inside another rule's open transaction validates the world as a start does and
//! writes it as it is; the outer rule's commit or rollback covers it. The delivery delays of a supply
//! route's interruption do this in the core since lane P8 d2 (`rules::handlers`, through
//! `Host::validate_candidate`), and count in the same `NESTED_RULES`. Its copy used to put the world in canonical order once more, so
//! the order of what the outer rule wrote before it is the one difference (no hash reads it; the
//! next transaction or Checkpoint puts it in canonical order).
//!
//! The debug checks of `core/txn_checks.py` reach the copy too: at the copy, every
//! object of the world that the copy replaces gets a class that raises on any use, and gets its
//! own class back if the copy is dropped.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use sheltersim_core::txn::{TxnKind, NESTED_RULES};

use crate::core::txn as core_txn;
use crate::core::world_core::core_of;
use crate::Unsupported;

// ---- Counts, for the gate -------------------------------------------------------------------------

static IN_PLACE: AtomicU64 = AtomicU64::new(0);
static ON_COPY: AtomicU64 = AtomicU64::new(0);
static COMMITTED: AtomicU64 = AtomicU64::new(0);
static ROLLED_BACK: AtomicU64 = AtomicU64::new(0);
/// Nanoseconds in the start of a transaction in place, and in the copy of the other path.
static BEGIN_NS: AtomicU64 = AtomicU64::new(0);
static COPY_NS: AtomicU64 = AtomicU64::new(0);

fn add_time(counter: &AtomicU64, since: Instant) {
    counter.fetch_add(u64::try_from(since.elapsed().as_nanos()).unwrap_or(u64::MAX), Ordering::Relaxed);
}

// ---- The copy, where a transaction refuses the world ----------------------------------------------

/// `MutableWorldState.from_dict(world.to_dict())`.
fn copy_of<'py>(world: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = world.py();
    let class = py.import(intern!(py, "sheltersim.core.world"))?.getattr(intern!(py, "WorldState"))?;
    class.call_method1(intern!(py, "from_dict"), (world.call_method0(intern!(py, "to_dict"))?,))
}

/// `_effect_candidate(world)` of `events/effects.py`: the copy an effect changes, which holds the
/// world's own EventLog instead of one made again from its documents.
fn effect_copy_of<'py>(world: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = world.py();
    py.import(intern!(py, "sheltersim.events.effects"))?.getattr(intern!(py, "_effect_candidate"))?.call1((world,))
}

/// `world.__dict__.clear(); world.__dict__.update(candidate.__dict__)`, and the copy's core is the
/// world's then (`state_mirror.owned_again`, lane 6-5).
fn swap(world: &Bound<'_, PyAny>, candidate: &Bound<'_, PyAny>) -> PyResult<()> {
    let py = world.py();
    let target = world.getattr(intern!(py, "__dict__"))?;
    let source = candidate.getattr(intern!(py, "__dict__"))?;
    let old_core = target.call_method1(intern!(py, "get"), (intern!(py, "_core"),))?;
    target.call_method0(intern!(py, "clear"))?;
    target.call_method1(intern!(py, "update"), (source,))?;
    state_mirror(py)?.getattr(intern!(py, "owned_again"))?.call1((world, old_core))?;
    Ok(())
}

fn state_mirror(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    py.import(intern!(py, "sheltersim.core.state_mirror"))
}

/// Whether a transaction is open on the world's core.
fn in_transaction(world: &Bound<'_, PyAny>) -> bool {
    core_of(world).is_ok_and(|core| core.try_borrow().is_ok_and(|core| core.txn.is_some()))
}

fn txn_checks(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    py.import(intern!(py, "sheltersim.core.txn_checks"))
}

// ---- The transaction ------------------------------------------------------------------------------

enum Path<'py> {
    /// A transaction of the world's core is open.
    InPlace,
    /// Inside another rule's open transaction (`delivery_delay_on` in `interrupt_route`): the rule
    /// writes the world as it is, and the outer transaction commits or rolls back.
    Nested,
    /// The copy the rule changes, and what `txn_checks.poison_copied` poisoned (None when it is off).
    Copy { copy: Bound<'py, PyAny>, poisoned: Bound<'py, PyAny> },
}

/// A rule's open transaction. `commit`, `rollback` or `finish` ends it; one dropped open rolls back.
pub(crate) struct RuleTxn<'py> {
    world: Bound<'py, PyAny>,
    path: Option<Path<'py>>,
}

impl<'py> RuleTxn<'py> {
    /// Open a transaction on the world, or make the copy where the transaction refuses it. The
    /// errors are the copy's: the world's validation as it is, then the candidate's, wrapped as
    /// `from_dict` wraps it; with either, the world is as it was.
    pub(crate) fn begin(world: &Bound<'py, PyAny>, kind: TxnKind) -> PyResult<Self> {
        let py = world.py();
        if in_transaction(world) {
            // The validations of a start, with the same errors; the world isn't put in canonical
            // order again (what the outer rule wrote stays in the order it wrote it).
            let validate = state_mirror(py)?.getattr(intern!(py, "_txn_validate"))?;
            validate.call1((world, false))?;
            validate.call1((world, true))?;
            NESTED_RULES.fetch_add(1, Ordering::Relaxed);
            return Ok(Self { world: world.clone(), path: Some(Path::Nested) });
        }
        let started = Instant::now();
        match core_txn::begin(world, kind) {
            Ok(()) => {
                add_time(&BEGIN_NS, started);
                IN_PLACE.fetch_add(1, Ordering::Relaxed);
                return Ok(Self { world: world.clone(), path: Some(Path::InPlace) });
            }
            Err(error) if error.is_instance_of::<Unsupported>(py) => {}
            Err(error) => return Err(error),
        }
        let started = Instant::now();
        let copy = match kind {
            TxnKind::Effect => effect_copy_of(world)?,
            TxnKind::Full => copy_of(world)?,
        };
        add_time(&COPY_NS, started);
        ON_COPY.fetch_add(1, Ordering::Relaxed);
        let poisoned = txn_checks(py)?.getattr(intern!(py, "poison_copied"))?.call1((world, &copy))?;
        Ok(Self { world: world.clone(), path: Some(Path::Copy { copy, poisoned }) })
    }

    /// The world the rule reads after the start and writes: the world itself, or the copy.
    pub(crate) fn world(&self) -> &Bound<'py, PyAny> {
        match &self.path {
            Some(Path::Copy { copy, .. }) => copy,
            _ => &self.world,
        }
    }

    /// Keep what the rule wrote: the transaction commits, or the copy takes the world's place.
    pub(crate) fn commit(mut self) -> PyResult<()> {
        COMMITTED.fetch_add(1, Ordering::Relaxed);
        match self.path.take() {
            Some(Path::InPlace) => core_txn::end(&self.world, true),
            Some(Path::Copy { copy, .. }) => swap(&self.world, &copy),
            Some(Path::Nested) | None => Ok(()),
        }
    }

    /// Give the world back as the start found it: the transaction rolls back, or the copy goes.
    pub(crate) fn rollback(mut self) -> PyResult<()> {
        ROLLED_BACK.fetch_add(1, Ordering::Relaxed);
        self.end_without_commit()
    }

    fn end_without_commit(&mut self) -> PyResult<()> {
        let py = self.world.py();
        match self.path.take() {
            Some(Path::InPlace) => core_txn::end(&self.world, false),
            Some(Path::Copy { poisoned, .. }) => {
                txn_checks(py)?.getattr(intern!(py, "unpoison"))?.call1((poisoned,))?;
                Ok(())
            }
            // What it wrote goes with the outer transaction's rollback, as the error that ends
            // this one passes on through the outer rule.
            Some(Path::Nested) | None => Ok(()),
        }
    }

    /// A step of the rule before its end: after a value the transaction stays open, after an error
    /// it rolls back and the error passes on (an error of the rollback itself in its place).
    pub(crate) fn keep<T>(self, result: PyResult<T>) -> PyResult<(Self, T)> {
        match result {
            Ok(value) => Ok((self, value)),
            Err(error) => {
                self.rollback()?;
                Err(error)
            }
        }
    }

    /// The end the rule's result says: commit after a value, roll back after an error, which then
    /// passes on (an error of the rollback itself passes on in its place).
    pub(crate) fn finish<T>(self, result: PyResult<T>) -> PyResult<T> {
        match result {
            Ok(value) => {
                self.commit()?;
                Ok(value)
            }
            Err(error) => {
                self.rollback()?;
                Err(error)
            }
        }
    }
}

impl Drop for RuleTxn<'_> {
    fn drop(&mut self) {
        if self.path.is_some() {
            ROLLED_BACK.fetch_add(1, Ordering::Relaxed);
            if let Err(error) = self.end_without_commit() {
                error.write_unraisable(self.world.py(), Some(&self.world));
            }
        }
    }
}

/// What the rule transactions did in this process: how many ran in place, on a copy and inside
/// another, how many
/// committed and rolled back, and the nanoseconds of their starts in place and of the copies.
#[pyfunction]
fn counts(py: Python<'_>) -> PyResult<Bound<'_, PyDict>> {
    let out = PyDict::new(py);
    for (name, counter) in [
        ("in_place", &IN_PLACE),
        ("copy", &ON_COPY),
        ("nested", &NESTED_RULES),
        ("committed", &COMMITTED),
        ("rolled_back", &ROLLED_BACK),
        ("begin_ns", &BEGIN_NS),
        ("copy_ns", &COPY_NS),
    ] {
        out.set_item(name, counter.load(Ordering::Relaxed))?;
    }
    Ok(out)
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(counts, m)?)?;
    Ok(())
}
