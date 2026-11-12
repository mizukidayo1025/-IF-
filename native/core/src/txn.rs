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
//! The `Host` is the world's outside (`host.rs`, which took in this file's trait in lane P8-0b):
//! here, the PyO3 crate's views (the Python containers, and the Python objects of the pending events
//! and of the records, which follow the canonical order and come back on a rollback) and the
//! validation of the whole world, which stays in Python in Issue #152 (policy E6). No borrow of the
//! world is held while the host runs (rule R6).

use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::Error;
pub use crate::host::Host;
use crate::world::{Snapshot, WorldCell};

/// The text of a transaction begun inside another (`Error::Runtime`, a `RuntimeError` in Python).
pub const NESTED: &str = "a transaction is open on the world already";

/// How many rules ran inside another rule's open transaction in this process (a rule's own start
/// validates the world and writes it as it is; the outer transaction commits or rolls back): the
/// rules of `rule` and the delivery delays of a supply route's interruption (`rules::handlers`).
/// For the gate's counts.
pub static NESTED_RULES: AtomicU64 = AtomicU64::new(0);
/// How many rule transactions (`rule`) opened in place, committed and rolled back in this process
/// (lane P9-1b; the PyO3 crate's `RuleTxn` counted them before).
pub static IN_PLACE: AtomicU64 = AtomicU64::new(0);
pub static COMMITTED: AtomicU64 = AtomicU64::new(0);
pub static ROLLED_BACK: AtomicU64 = AtomicU64::new(0);

/// What the rule transactions did in this process, by name: how many ran in place and inside
/// another, how many committed and rolled back.
pub fn counts() -> [(&'static str, u64); 4] {
    [
        ("in_place", IN_PLACE.load(Ordering::Relaxed)),
        ("nested", NESTED_RULES.load(Ordering::Relaxed)),
        ("committed", COMMITTED.load(Ordering::Relaxed)),
        ("rolled_back", ROLLED_BACK.load(Ordering::Relaxed)),
    ]
}

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

/// The transaction of a rule that changes the world and keeps the change only if the changed world
/// passes its checks (lane 6-3; in the core since lane P9-1b, policy E16): a rule's handler used to
/// copy the whole world through canonical JSON, change the copy, validate it and swap it in.
///
/// Where no transaction is open, the host first says whether its views of the world are all ones a
/// transaction gives back (`Host::txn_check`: its refusal passes on, nothing changed), then `begin`,
/// the body, and `commit` or `rollback` as the body says; an error of the body rolls back and passes
/// on.
///
/// Inside another open transaction (a rule a Branch's command or another rule runs), the rule
/// validates the world as a start does, with the same errors, and the body writes the world as it
/// is: the outer transaction commits or rolls it back, so the body's own rollback leaves what it
/// wrote to the outer one. The world is not put in canonical order again (the copy did that once
/// more: the order of what the outer rule wrote is the one difference, which no hash reads).
pub fn rule<C: WorldCell, H: Host, T>(
    cell: &C,
    host: &mut H,
    kind: TxnKind,
    body: impl FnOnce(&C, &mut H) -> Result<TxnEnd<T>, H::Error>,
) -> Result<T, H::Error> {
    if cell.with(|world| world.txn_open)? {
        host.validate_candidate(kind, Stage::World)?;
        host.validate_candidate(kind, Stage::Candidate)?;
        NESTED_RULES.fetch_add(1, Ordering::Relaxed);
        return match body(cell, host)? {
            TxnEnd::Commit(value) | TxnEnd::Rollback(value) => Ok(value),
        };
    }
    host.txn_check()?;
    let txn = Txn::begin(cell, host, kind)?;
    IN_PLACE.fetch_add(1, Ordering::Relaxed);
    match body(cell, host) {
        Ok(TxnEnd::Commit(value)) => {
            COMMITTED.fetch_add(1, Ordering::Relaxed);
            txn.commit(cell, host)?;
            Ok(value)
        }
        Ok(TxnEnd::Rollback(value)) => {
            ROLLED_BACK.fetch_add(1, Ordering::Relaxed);
            txn.rollback(cell, host)?;
            Ok(value)
        }
        Err(error) => {
            ROLLED_BACK.fetch_add(1, Ordering::Relaxed);
            txn.rollback(cell, host)?;
            Err(error)
        }
    }
}

/// `rule` of a body that keeps what it wrote unless it fails: an error rolls back and passes on.
pub fn rule_kept<C: WorldCell, H: Host, T>(
    cell: &C,
    host: &mut H,
    kind: TxnKind,
    body: impl FnOnce(&C, &mut H) -> Result<T, H::Error>,
) -> Result<T, H::Error> {
    rule(cell, host, kind, |cell, host| body(cell, host).map(TxnEnd::Commit))
}
