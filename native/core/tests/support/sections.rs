//! The Python-parsed sections and the floor plan of an exported world, for a `TestHost` (#225 lane
//! F2, #214): the crate's `load::Sections` (#244), which reads them as the PyO3 crate's
//! `PyHost::fill_section` and `Geo::of` do.
//!
//! `fill` gives a `TestHost` every section the host parses; it fills each with the world's value
//! when a rule asks, and panics where the PyO3 crate raises out of `fill_section`. `floor_plan` is
//! built once, from the world as exported; a test that changes the overlay builds it again
//! (`plan_of`).

use std::sync::Arc;

use serde_json::Value;
use sheltersim_core::config::{Configs, Parser, SectionId};
use sheltersim_core::error::Error;
use sheltersim_core::geometry::Geo;
use sheltersim_core::load::{Sections, OVERLAY};
use sheltersim_core::value::V;

use super::load::v;
use super::TestHost;

fn sections(entry: &Value) -> Sections {
    Sections::of_entry(entry).unwrap_or_else(|error| panic!("{error}"))
}

/// Give the host every Python-parsed section of an exported world (`entry`): it fills each with the
/// world's value when a rule asks. Each section the core parses itself is left to the core.
pub fn fill(host: &mut TestHost, entry: &Value) {
    let sections = Arc::new(sections(entry));
    for &which in SectionId::ALL {
        if which.parser() != Parser::Host {
            continue;
        }
        let sections = Arc::clone(&sections);
        host.sections.push((which, Box::new(move |configs: &Configs| {
            sections.fill(configs, which).unwrap_or_else(|error| panic!("{error}"));
        })));
    }
}

/// One section of an exported world, as the host fills it: in configs of its own.
pub fn section(entry: &Value, which: SectionId) -> Arc<Configs> {
    let configs = Arc::new(Configs::new(None));
    assert!(sections(entry).fill(&configs, which).unwrap(), "no section {}", which.name());
    configs
}

/// `Geo::of` of an exported world with `overlay` (`Sections::floor_plan`).
pub fn plan_of(entry: &Value, overlay: Option<&V>) -> Result<Arc<Geo>, Error> {
    sections(entry).floor_plan(overlay)
}

/// `plan_of` the world as exported.
pub fn floor_plan(entry: &Value) -> Result<Arc<Geo>, Error> {
    let overlay = entry["world"]["model_state"].get(OVERLAY).map(v);
    plan_of(entry, overlay.as_ref())
}
