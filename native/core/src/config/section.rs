//! A section of the configs: what one parse of `model_configs` gave (design-p7.md §2, §3).
//!
//! A parse never raises where the configs are read the first time after a change. What it raised
//! is kept as a `Fault`, and each read site turns it into the error it raises today, at the line it
//! raises it: a refusal with its own reason, or, for an error the rule relays as it is, the error
//! of a second parse (policy E14 of docs/rust-port/step5-plan.md). A parse is a pure function of the
//! ordered configs and of the contracts of the process, so the value, the error and the moment of
//! the error are the same as when every read parsed.

use std::any::Any;
use std::sync::{Arc, OnceLock};

/// What one parse of a section gave.
pub enum Section<T: ?Sized> {
    /// The configs have no such section.
    Absent,
    Ok(Arc<T>),
    Fault(Fault),
}

impl<T: ?Sized> Clone for Section<T> {
    fn clone(&self) -> Self {
        match self {
            Section::Absent => Section::Absent,
            Section::Ok(value) => Section::Ok(value.clone()),
            Section::Fault(fault) => Section::Fault(fault.clone()),
        }
    }
}

impl<T> From<Result<T, Fault>> for Section<T> {
    fn from(parsed: Result<T, Fault>) -> Self {
        match parsed {
            Ok(value) => Section::Ok(Arc::new(value)),
            Err(fault) => Section::Fault(fault),
        }
    }
}

/// What a parse raised: the class and `str(error)`, which a build without Python can report.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Fault {
    pub class: FaultClass,
    pub text: Arc<str>,
}

impl Fault {
    pub fn new(class: FaultClass, text: impl Into<Arc<str>>) -> Self {
        Self { class, text: text.into() }
    }

    /// The reader of the parsed value refused it; the text is its reason.
    pub fn refused(reason: impl Into<Arc<str>>) -> Self {
        Self::new(FaultClass::Refused, reason)
    }

    /// Whether the reader refused what the parse gave, rather than the parse raising.
    pub fn is_refusal(&self) -> bool {
        self.class == FaultClass::Refused
    }
}

/// The class of a fault.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FaultClass {
    Value,
    Type,
    Key,
    /// pydantic's `ValidationError`.
    Validation,
    /// The reader of the parsed value refused it (`Unsupported`).
    Refused,
    /// Another class, by its module and name.
    Other(Arc<str>),
}

/// A value only the PyO3 crate reads (design-p7.md §2). Nothing holds one since the plan of the
/// end-of-tick check moved here (lane P8 d4).
pub type Opaque = dyn Any + Send + Sync;

/// A section parsed at its first read and then kept: a parse runs only where the configs are read.
pub struct Lazy<T: ?Sized>(OnceLock<Section<T>>);

impl<T: ?Sized> Default for Lazy<T> {
    fn default() -> Self {
        Self(OnceLock::new())
    }
}

impl<T: ?Sized> Lazy<T> {
    /// The section, if it was parsed.
    #[inline]
    pub fn get(&self) -> Option<&Section<T>> {
        self.0.get()
    }

    /// The section, parsed by `parse` if it was not yet. No lock is held while `parse` runs, so a
    /// parse may read the configs itself; the first parse to finish is kept, and every parse of the
    /// same configs gives the same section.
    pub fn get_or_parse(&self, parse: impl FnOnce() -> Section<T>) -> &Section<T> {
        if let Some(section) = self.0.get() {
            return section;
        }
        let section = parse();
        // Err: a parse inside `parse` filled it first, with the same section.
        let _ = self.0.set(section);
        self.0.get().expect("the section was set above")
    }
}
