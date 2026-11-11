//! Transactions in place (policy E5, design-p5-p6.md §6, lane 6-2).
//!
//! A rule that changes the world and keeps the change only if the changed world passes its checks
//! used to copy the whole world through canonical JSON (`WorldState.from_dict(world.to_dict())`),
//! change the copy, validate it and swap it in. A `Txn` does that on the world itself: `begin`
//! validates the world as it is, keeps a `Snapshot` (copy on write), puts the world in the order the
//! copy had (`World::canonicalize`) and validates it again as the copy was validated; the body then
//! writes the world; `commit` drops the snapshot, `rollback` gives it back. Transactions don't nest
//! (rule R8 of the design).
//!
//! The `Host` is the world's outside: the PyO3 crate's views (the Python containers, and the Python
//! objects of the pending events and of the records, which follow the canonical order and come
//! back on a rollback) and the validation of the whole world, which stays in Python in Issue #152
//! (policy E6). No borrow of the world is held while the host runs (rule R6).

use crate::error::Error;
use crate::world::{Snapshot, WorldCell};

/// The text of a transaction begun inside another (`Error::Runtime`, a `RuntimeError` in Python).
pub const NESTED: &str = "a transaction is open on the world already";

/// Which copy a transaction stands for, which says how its candidate is validated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxnKind {
    /// `_effect_candidate` of `events/effects.py`: the world's own EventLog.
    Effect,
    /// `WorldState.from_dict(world.to_dict())`, the log included.
    Full,
}

/// What a validation is of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// The world as the transaction finds it, as `to_dict` (or `_effect_candidate`) validated it
    /// first: its error passes on as it is.
    World,
    /// The world in canonical order, as `from_dict` validated the copy: its error is wrapped as
    /// from_dict wraps it (`StateInvariantError("invalid WorldState document: ...")`).
    Candidate,
}

/// How a body ends.
#[derive(Debug, PartialEq, Eq)]
pub enum TxnEnd<T> {
    Commit(T),
    Rollback(T),
}

/// The outside of the world for a transaction (policy E6). Each call runs with nothing borrowed.
pub trait Host {
    type Error: From<Error>;
    /// Validate the whole world (`WorldState.validate` in the native host).
    fn validate_candidate(&mut self, kind: TxnKind, stage: Stage) -> Result<(), Self::Error>;
    /// The world was put in canonical order: the views follow it. `before` is the world as it was
    /// (the queue's old serials, by which a host keeps its objects of the events). On an error the
    /// transaction gives the snapshot back and calls `restored`.
    fn canonicalized(&mut self, _kind: TxnKind, _before: &Snapshot) -> Result<(), Self::Error> {
        Ok(())
    }
    /// The snapshot came back: the views follow it.
    fn restored(&mut self, _kind: TxnKind) -> Result<(), Self::Error> {
        Ok(())
    }
    /// The transaction kept its writes.
    fn committed(&mut self, _kind: TxnKind) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// An open transaction: its kind and the world's snapshot from its start.
pub struct Txn {
    kind: TxnKind,
    snapshot: Snapshot,
}

impl Txn {
    pub fn kind(&self) -> TxnKind {
        self.kind
    }

    /// The world as it was at the start.
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    /// Validate the world, keep a snapshot, put the world in canonical order and validate it as the
    /// candidate. A failed validation of the world changes nothing; any later error gives the
    /// snapshot back first. `Error::Runtime(NESTED)` while another transaction is open.
    pub fn begin<C: WorldCell, H: Host>(cell: &C, host: &mut H, kind: TxnKind) -> Result<Txn, H::Error> {
        cell.with(|world| if world.txn_open { Err(Error::Runtime(NESTED.to_owned())) } else { Ok(()) })??;
        host.validate_candidate(kind, Stage::World)?;
        let snapshot = cell.with(|world| {
            if world.txn_open {
                return Err(Error::Runtime(NESTED.to_owned()));
            }
            let snapshot = world.snapshot();
            world.canonicalize();
            world.txn_open = true;
            Ok(snapshot)
        })??;
        let followed = host
            .canonicalized(kind, &snapshot)
            .and_then(|()| host.validate_candidate(kind, Stage::Candidate));
        let txn = Txn { kind, snapshot };
        match followed {
            Ok(()) => Ok(txn),
            Err(error) => {
                txn.rollback(cell, host)?;
                Err(error)
            }
        }
    }

    /// Keep what the body wrote: the snapshot goes.
    pub fn commit<C: WorldCell, H: Host>(self, cell: &C, host: &mut H) -> Result<(), H::Error> {
        cell.with(|world| world.txn_open = false)?;
        let Txn { kind, snapshot } = self;
        drop(snapshot);
        host.committed(kind)
    }

    /// Give the snapshot back: the world is what it was before `begin`, in its order then.
    pub fn rollback<C: WorldCell, H: Host>(self, cell: &C, host: &mut H) -> Result<(), H::Error> {
        let Txn { kind, snapshot } = self;
        cell.with(|world| {
            world.restore(snapshot);
            world.txn_open = false;
        })?;
        host.restored(kind)
    }

    /// `begin`, the body, and `commit` or `rollback` as the body says; an error of the body rolls
    /// back and passes on.
    pub fn run<C: WorldCell, H: Host, T>(
        cell: &C,
        host: &mut H,
        kind: TxnKind,
        body: impl FnOnce(&C, &mut H) -> Result<TxnEnd<T>, H::Error>,
    ) -> Result<T, H::Error> {
        let txn = Self::begin(cell, host, kind)?;
        match body(cell, host) {
            Ok(TxnEnd::Commit(value)) => {
                txn.commit(cell, host)?;
                Ok(value)
            }
            Ok(TxnEnd::Rollback(value)) => {
                txn.rollback(cell, host)?;
                Ok(value)
            }
            Err(error) => {
                txn.rollback(cell, host)?;
                Err(error)
            }
        }
    }
}
