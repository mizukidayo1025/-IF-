//! The configs of a world as the kernels read them, kept by the stamp of `model_configs`
//! (design-p7.md §2–§4, Issue #152, lane 7-1; policy E7 of docs/rust-port/step5-plan.md).
//!
//! `Configs` is a value derived from the `model_configs` slot: it is good while the slot has the
//! stamp it was made for. Every write to the configs gives the slot a new stamp (`Slot::touch`), so
//! the next read finds no `Configs` of that stamp and parses again. A rollback that brings an old
//! state back brings its old stamp back too, and the second entry of `Derived` then still holds it.
//! A stamp is never issued twice in a process, so a hit is never a later state.
//!
//! The PyO3 crate parses, with the Python contracts, at the first read of each section
//! (`src/core/configs.rs`). Sections join lane by lane: P7-1 has the model selection and the plan
//! of the end-of-tick check; geometry, movement, routing and the rest follow in 7-2 and 7-5.

pub mod section;
pub mod selection;

use std::sync::Arc;

use crate::stamp::Stamp;

pub use section::{Fault, FaultClass, Lazy, Opaque, Section};
pub use selection::Selection;

/// The sections parsed from one state of `model_configs`.
#[derive(Default)]
pub struct Configs {
    /// The stamp of the `model_configs` slot these were parsed from. None: no slot follows the
    /// configs (a world without a core, or a field assigned whole), so these serve one read.
    pub stamp: Option<Stamp>,
    /// `selected_models(model_configs)`.
    pub selection: Lazy<Selection>,
    /// The plan of the end-of-tick check (`core/tick_validation.py`'s `tick_plan`), a Python
    /// object of the PyO3 crate.
    pub tick_plan: Lazy<Opaque>,
}

impl Configs {
    pub fn new(stamp: Option<Stamp>) -> Self {
        Self { stamp, ..Self::default() }
    }
}

/// What a world keeps that is not its state: values derived from it, keyed by stamps. Nothing
/// canonicalizes, hashes, saves or compares it; dropping it costs a parse and changes no result.
#[derive(Default)]
pub struct Derived {
    /// The configs of the two most recent stamps, the latest first.
    configs: [Option<Arc<Configs>>; 2],
}

impl Derived {
    /// The configs of this stamp: kept, or new with nothing parsed, which drops the older entry.
    pub fn configs(&mut self, stamp: Stamp) -> Arc<Configs> {
        let held = |entry: &Option<Arc<Configs>>| entry.as_ref().is_some_and(|configs| configs.stamp == Some(stamp));
        if !held(&self.configs[0]) {
            if held(&self.configs[1]) {
                self.configs.swap(0, 1);
            } else {
                self.configs[1] = self.configs[0].take();
            }
        }
        self.configs[0].get_or_insert_with(|| Arc::new(Configs::new(Some(stamp)))).clone()
    }

    /// The stamps kept, the latest first (for tests).
    pub fn stamps(&self) -> [Option<Stamp>; 2] {
        [0, 1].map(|index| self.configs[index].as_ref().and_then(|configs| configs.stamp))
    }
}
