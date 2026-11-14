//! The errors a kernel raises, with the texts a Python caller sees, without Python.
//!
//! Each variant stands for one Python class; the PyO3 crate maps it to that class with this text
//! (`native/src/core/errors.rs`, `to_py_err`). The texts are saved data: the dispatcher writes
//! `str(exc)` of an event's outcome to its record as `detail`, which is hashed and shown. So every
//! text is built here, in the Python rule's words.
//!
//! The builders count for `benchmarks/rule_shadow/refusal_census.py` (`census`) where the old
//! exits did, at the caller's line (`#[track_caller]`). A variant built directly is not counted.

use std::borrow::Cow;
use std::fmt;

use crate::census;
use crate::mathx::MathError;

/// Which of an event's two expected outcomes (`events/models.py`).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Outcome {
    /// `EventSuppressed`.
    Suppressed,
    /// `EventApplicationError`.
    Failed,
}

/// An error of the core and the Python class it becomes.
#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    /// `sheltersim_native.Unsupported(reason)`: the kernel leaves the value to Python.
    Unsupported(Cow<'static, str>),
    /// `KernelRefusal(f"{label} refused: {reason}")` of `core/kernels.py`, caused by
    /// `Unsupported(reason)`: a refusal no Python rule stands behind (lane L10). `label` is
    /// `module.name` of the kernel.
    Refused { label: String, reason: Cow<'static, str> },
    /// `EventSuppressed(text)` or `EventApplicationError(text)`, marked as made here.
    Outcome(Outcome, String),
    /// `ValueError(text)` marked as made here, so that a step which turns a Python call's
    /// exception into Unsupported lets it through (a check of `effect_program_v1`).
    MarkedValue(String),
    /// `ValueError(text)` of a Python rule.
    Value(String),
    /// pydantic's `ValidationError` (a `ValueError`) with its text, where a rule lets it through
    /// as it is (`contracts::pydantic::text`; the whole world's validation, #250).
    Validation(String),
    /// `KeyError(text)`.
    Key(String),
    /// `RuntimeError(text)`.
    Runtime(String),
    /// `TypeError(text)`.
    Type(String),
    /// `StateInvariantError(text)` of `core/world.py`, from a Python rule.
    Invariant(String),
    /// `ResourceLedgerError(text)` of `domain/resources.py`, the contract of the ledger.
    Ledger(String),
    /// `StateInvariantError(corrupt_text(namespace, check))`: a state the Python rules' checks never
    /// let through (the C group of docs/rust-port/step4-plan.md). Not Python's text.
    Corrupt { namespace: String, check: String },
    /// The exception CPython raised in `exp`, `expm1` or `pow` (`mathx`).
    Math(MathError),
    /// `TickExecutionError(CLOCK)` of `core/engine.py`: a tick phase moved the clock the engine
    /// owns (lane P9-0, for the core's tick driver).
    Clock,
}

/// The text of `Error::Clock`, as `TickEngine` and the tick driver raise it.
pub const CLOCK: &str = "tick phase attempted to change the engine-owned clock";

/// What the core's fallible calls return.
pub type Result<T> = std::result::Result<T, Error>;

/// The text of a kernel's refusal, as `kernel_only` of `core/kernels.py` writes it.
pub fn refused_text(label: &str, reason: &str) -> String {
    format!("{label} refused: {reason}")
}

/// The text of a corrupt state: it names the namespace and the check.
pub fn corrupt_text(namespace: &str, check: &str) -> String {
    format!("{namespace}: corrupt state: {check}")
}

impl Error {
    /// `Unsupported(reason)`, counted at the caller's line as `Unsupported::new_err` counts it.
    #[track_caller]
    #[inline]
    pub fn unsupported(reason: impl Into<Cow<'static, str>>) -> Self {
        let reason = reason.into();
        census::raised(&reason);
        Error::Unsupported(reason)
    }

    /// A refusal made a `KernelRefusal` that names the kernel; any other error goes on as it is.
    pub fn refused(label: impl Into<String>, error: Error) -> Self {
        match error {
            Error::Unsupported(reason) => Error::Refused { label: label.into(), reason },
            other => other,
        }
    }

    /// `raise EventSuppressed(text)`.
    #[inline]
    pub fn suppressed(text: impl Into<String>) -> Self {
        Error::Outcome(Outcome::Suppressed, text.into())
    }

    /// `raise EventApplicationError(text)`.
    #[inline]
    pub fn failed(text: impl Into<String>) -> Self {
        Error::Outcome(Outcome::Failed, text.into())
    }

    /// `raise ValueError(text)` of a Python rule, counted at the caller's line.
    #[track_caller]
    pub fn value_error(text: impl Into<String>) -> Self {
        let text = text.into();
        moved(&text);
        Error::Value(text)
    }

    /// `raise StateInvariantError(text)` of a Python rule, counted at the caller's line.
    #[track_caller]
    pub fn invariant(text: impl Into<String>) -> Self {
        let text = text.into();
        moved(&text);
        Error::Invariant(text)
    }

    /// `raise ResourceLedgerError(text)` of the ledger's operation, counted at the caller's line.
    #[track_caller]
    pub fn ledger(text: impl Into<String>) -> Self {
        let text = text.into();
        moved(&text);
        Error::Ledger(text)
    }

    /// The one exit for a corrupt state, counted at the caller's line.
    #[track_caller]
    pub fn corrupt(namespace: impl Into<String>, check: impl Into<String>) -> Self {
        let (namespace, check) = (namespace.into(), check.into());
        if census::is_on() {
            moved(&corrupt_text(&namespace, &check));
        }
        Error::Corrupt { namespace, check }
    }
}

#[cold]
#[inline(never)]
#[track_caller]
fn moved(text: &str) {
    census::moved(text);
}

impl From<MathError> for Error {
    fn from(error: MathError) -> Self {
        Error::Math(error)
    }
}

/// `str(exc)` of the Python exception. A `KeyError`'s `str` quotes its text; this gives it bare.
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Unsupported(reason) => f.write_str(reason),
            Error::Refused { label, reason } => f.write_str(&refused_text(label, reason)),
            Error::Outcome(_, text)
            | Error::MarkedValue(text)
            | Error::Value(text)
            | Error::Validation(text)
            | Error::Key(text)
            | Error::Runtime(text)
            | Error::Type(text)
            | Error::Invariant(text)
            | Error::Ledger(text) => f.write_str(text),
            Error::Corrupt { namespace, check } => f.write_str(&corrupt_text(namespace, check)),
            Error::Math(error) => f.write_str(error.text()),
            Error::Clock => f.write_str(CLOCK),
        }
    }
}

impl std::error::Error for Error {}

/// Why a value or a write does not fit a row: the namespace is tainted with this reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Misfit(pub String);

impl Misfit {
    pub fn new(reason: impl Into<String>) -> Self {
        Self(reason.into())
    }
}
