//! What the phases' steps read besides the world, through the host and the Run (lane P9-2a): what
//! the world holds (H1), the configs' sections (H2), the floor plan, the queue's rows (H6), and the
//! readers shared by several steps. Each reader reads what the step's PyO3 entry read, in its order,
//! with the same refusals.

use std::sync::Arc;

use crate::config::{Configs, Fault, Lazy, Section, SectionId};
use crate::error::{Error, Result};
use crate::geometry::Geo;
use crate::host::{Again, Holding, Host};
use crate::rules::kit::{self, Held, Kept};
use crate::world::{WorldCell, MODEL_CONFIGS};

use super::{RunCell, Tick};

/// The refusal of a section the configs do not have (`configs::kept` of the PyO3 crate).
pub const NO_SUCH_SECTION: &str = "the configs have no such section";
/// `as_readable_dict(model_state)` failed where a step checks it (`body_state_of` of psychology).
pub const NOT_READABLE: &str = "model_state is not readable";
/// `exact_dict(model_state)` failed (`exact_dict` of `core/row.rs`).
pub const NOT_PLAIN: &str = "a value is not a plain dict";

/// A section as a `Kept` read: its value, or the fault for the read site.
pub fn kept_of<T>(section: Section<T>) -> Kept<T> {
    match section {
        Section::Ok(value) => Ok(value),
        Section::Fault(fault) => Err(fault),
        Section::Absent => Err(Fault::refused(NO_SUCH_SECTION)),
    }
}

impl<C: WorldCell, H: Host, R: RunCell> Tick<'_, C, H, R> {
    /// What the world holds now (H1).
    pub fn holding(&mut self) -> std::result::Result<Arc<Holding>, H::Error> {
        kit::holding(self.cell, self.host)
    }

    /// A section the core parses from the configs (`configs::kept_section`).
    pub fn value_section<T>(
        &mut self,
        holding: &Holding,
        which: SectionId,
        get: fn(&Configs) -> &Lazy<T>,
        parse: fn(&crate::value::V) -> crate::config::read::Read<T>,
    ) -> std::result::Result<Kept<T>, H::Error> {
        Ok(kept_of(kit::section_value(self.cell, self.host, holding, which, get, parse)?))
    }

    /// A section the core parses, raised at once where it is a fault (`configs::kept_value`).
    pub fn value_now<T>(
        &mut self,
        holding: &Holding,
        which: SectionId,
        get: fn(&Configs) -> &Lazy<T>,
        parse: fn(&crate::value::V) -> crate::config::read::Read<T>,
    ) -> std::result::Result<Arc<T>, H::Error> {
        match self.value_section(holding, which, get, parse)? {
            Ok(value) => Ok(value),
            Err(fault) => Err(kit::kept::<T>(&Err(fault)).err().unwrap_or_else(|| Error::Runtime("a fault read as a value".to_owned())).into()),
        }
    }

    /// A section a Python contract parses (H2), filled by the host at its first read.
    pub fn host_section<T>(&mut self, holding: &Holding, which: SectionId, get: fn(&Configs) -> &Lazy<T>) -> std::result::Result<Kept<T>, H::Error> {
        Ok(kept_of(kit::section(self.cell, self.host, holding, which, get)?))
    }

    /// The error a read site raises for a fault of a section Python parses (`configs::raise`): a
    /// refusal its own `Unsupported`, any other what the parse raises again (policy E14).
    pub fn raise(&mut self, which: SectionId, fault: &Fault) -> H::Error {
        if fault.is_refusal() {
            return Error::unsupported(fault.text.to_string()).into();
        }
        match self.host.read_again(Again::Section(which)) {
            Err(error) => error,
            Ok(()) => Error::corrupt(MODEL_CONFIGS, "a config fault did not reproduce").into(),
        }
    }

    /// The floor plan, or what reading it raised, kept for the read site (`Plan::of`).
    pub fn plan(&mut self) -> Plan<H::Error> {
        Plan(self.host.floor_plan())
    }

    /// `namespace_is_absent(world, name)`, its refusal raised.
    pub fn is_absent(&self, holding: &Holding, name: &str) -> std::result::Result<bool, H::Error> {
        holding.is_absent(name).map_err(|reason| Error::unsupported(reason).into())
    }
}

/// The floor plan as the host read it (`Plan` of the PyO3 crate's travel glue).
pub struct Plan<E>(pub std::result::Result<Arc<Geo>, E>);

impl<E> Plan<E> {
    pub fn get(&self) -> Option<&Arc<Geo>> {
        self.0.as_ref().ok()
    }

    /// The step's result, with what reading the plan raised where the step read it
    /// (`PLAN_UNREAD`).
    pub fn finish<T>(self, result: std::result::Result<T, E>, refused: impl Fn(&E) -> Option<String>) -> std::result::Result<T, E> {
        match (result, self.0) {
            (Err(error), Err(read)) if refused(&error).as_deref() == Some(crate::rules::overlay::PLAN_UNREAD) => Err(read),
            (result, _) => result,
        }
    }
}

/// Whether a host error is the refusal with this text.
pub fn is_refusal<E: crate::host::HostFailure>(error: &E) -> Option<String> {
    (error.class() == crate::host::ErrorClass::Refused).then(|| error.text())
}

/// A namespace held as the step reads it, the refusal raised where the step raised it.
pub fn held_now(held: Held) -> Result<Held> {
    match held {
        Held::Other(reason) => Err(Error::unsupported(reason)),
        held => Ok(held),
    }
}
